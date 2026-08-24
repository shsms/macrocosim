//! `/api/microgrids` — list every registered microgrid + the
//! create endpoint that allocates a fresh id + port and notifies
//! the binary's registered-microgrid listener to boot the runtime.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::lisp::Config;

pub(in crate::ui) async fn microgrids_list(
    State(config): State<Config>,
) -> Json<Vec<crate::sim::microgrids::MicrogridView>> {
    Json(crate::sim::microgrids::snapshot(&config.microgrids()))
}

#[derive(Deserialize)]
pub(in crate::ui) struct CreateMicrogridBody {
    name: String,
    #[serde(default)]
    tso: Option<String>,
}

#[derive(Serialize)]
pub(in crate::ui) struct CreateMicrogridResp {
    id: u64,
    name: String,
    grpc_port: u16,
    tso: Option<String>,
}

/// POST /api/microgrids/create — auto-allocates id + grpc_port,
/// inserts a fresh entry in the registry, and broadcasts a
/// registered-microgrid notification. The binary's listener (see
/// `bin/switchyard.rs`) reacts by booting the runtime — physics +
/// history + Microgrid gRPC server + loopback client — so there is
/// exactly one spawn path shared with runtime `(make-microgrid …)`
/// evals, and no path can double-boot a runtime.
///
/// Empty-name requests are rejected. The new microgrid's site is
/// constructed with the shared enterprise id allocator so its
/// auto-allocated component ids stay globally unique.
pub(in crate::ui) async fn microgrids_create(
    State(config): State<Config>,
    Json(body): Json<CreateMicrogridBody>,
) -> Result<Json<CreateMicrogridResp>, (StatusCode, String)> {
    let created = create_core(&config, &body.name, body.tso.as_deref())?;
    // Notify enterprise-wide subscribers: the binary's listener boots
    // the runtime (physics + history + gRPC server + loopback), and
    // the WS event pump starts forwarding topology_changed / sample
    // events to live UI sessions. The registry insert + stub write
    // both happen before this, so the listener's lookup finds the
    // entry. Test fixtures run no listener — the entry simply gets
    // no runtime, same as the old no-op spawner.
    config.notify_microgrid_registered(created.id);
    Ok(Json(created))
}

/// The shared create path: allocates id + port, inserts the registry
/// entry, and writes the per-mg config stub. Does NOT notify the
/// runtime spawner — the caller does, after any extra persistence of
/// its own (the import writes the overrides file in between).
fn create_core(
    config: &Config,
    name: &str,
    tso: Option<&str>,
) -> Result<CreateMicrogridResp, (StatusCode, String)> {
    use crate::sim::microgrids::{
        MicrogridDef, MicrogridEntry, next_free_id_in, next_free_port_in,
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name must be non-empty".into()));
    }
    let registry = config.microgrids();
    let site = crate::sim::MicrogridSite::with_id_allocator(config.enterprise_id_allocator());
    // Allocate id + port AND insert the entry under one lock so
    // concurrent creates can't pick the same port (the earlier
    // shape probed both before locking; two simultaneous calls
    // could land on the same grpc_port and the second tonic
    // listener would fail to bind silently inside its tokio task).
    let (id, grpc_port, def) = {
        let mut r = registry.lock();
        let id = next_free_id_in(&r);
        let grpc_port = next_free_port_in(&r);
        let def = MicrogridDef {
            id,
            name: name.clone(),
            grpc_port,
            tso: tso.map(str::to_string),
        };
        r.insert(
            id,
            MicrogridEntry {
                def: def.clone(),
                site: site.clone(),
                // Backfilled to the stub path right after the stub is
                // written below — the entry has to exist first (the
                // allocate + insert is one critical section), and the
                // path has to exist before it can be canonicalized.
                source: None,
                managed: false,
                unsaved: false,
            },
        );
        (id, grpc_port, def)
    };
    // Persist the per-mg config stub BEFORE spawning the runtime.
    // If the write fails the next boot would orphan the live tasks
    // (gRPC server, loopback, physics, history sampler) since the
    // stub is what re-creates the microgrid at load-time. Rolling
    // back the registry insert + bailing out keeps the failure
    // mode clean: nothing started, nothing leaked.
    let stub = match write_microgrid_stub(config, id, &name, grpc_port, tso) {
        Ok(path) => path,
        Err(e) => {
            registry.lock().remove(&id);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, e));
        }
    };
    // The stub is this microgrid's source file: reload replays it,
    // and its `(make-microgrid …)` form must be recognised as this
    // entry's OWN declaration rather than a stranger claiming a
    // registered id.
    if let Some(entry) = registry.lock().get_mut(&id) {
        entry.source = Some(stub);
    }
    Ok(CreateMicrogridResp {
        id,
        name: def.name,
        grpc_port,
        tso: def.tso,
    })
}

#[derive(Deserialize)]
pub(in crate::ui) struct ImportMicrogridBody {
    name: String,
    #[serde(default)]
    tso: Option<String>,
    /// The site export's components.json, verbatim.
    components: crate::sim::site_import::ComponentsFile,
    /// The site export's connections.json, verbatim (optional).
    #[serde(default)]
    connections: Option<crate::sim::site_import::ConnectionsFile>,
}

#[derive(Serialize)]
pub(in crate::ui) struct ImportMicrogridResp {
    id: u64,
    name: String,
    grpc_port: u16,
    tso: Option<String>,
    components: usize,
    connections: usize,
}

/// POST /api/microgrids/import — creates a REAL microgrid from a
/// site export: same allocate + stub + boot path as create, then the
/// export's components rendered as one `(progn (make-* …) …
/// (connect …) …)` form evaluated against the new microgrid — the
/// same path a UI edit takes, so the persistence gate appends the
/// form to the microgrid's overrides file and the stub's
/// `(load-overrides)` replays it at every later boot. Capacity, SoC
/// bounds, rated power bounds and the grid's rated fuse current all
/// survive into the simulation.
///
/// Imported component ids are kept verbatim. Component ids are
/// enterprise-unique in switchyard, so an id that any registered
/// microgrid already carries fails the whole import atomically —
/// nothing is created. The enterprise id allocator jumps past the
/// import's highest id so later auto-assigned ids can't collide.
pub(in crate::ui) async fn microgrids_import(
    State(config): State<Config>,
    Json(body): Json<ImportMicrogridBody>,
) -> Result<Json<ImportMicrogridResp>, (StatusCode, String)> {
    let import = crate::sim::site_import::parse(body.components, body.connections)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // One import at a time, from here to the eval that registers
    // the components. The collision check below is authoritative
    // only while no other import can add components between the
    // scan and this import's own eval — the make-* re-check at
    // replay is per site and cannot see a cross-site collision.
    // Parsing stays outside the lock; it touches no shared state.
    let import_lock = config.import_lock();
    let _serialized = import_lock.lock().await;
    // Collision check against every registered site, plus the
    // bootstrap site legacy single-site configs run on.
    // `import.components` comes from a dedup-validated
    // parse, so the collected list is already sorted and unique.
    {
        // Resolve the bootstrap site BEFORE taking the registry
        // lock: config.site() takes that same (non-reentrant) lock
        // internally, so the other order deadlocks.
        let bootstrap = config.site();
        // Snapshot the site handles under the lock, then scan with
        // the lock RELEASED: a big export means thousands of per-id
        // lookups, and holding the registry mutex through them
        // would stall every other registry user (create, listing,
        // the WS pump, the typed control endpoints).
        let sites: Vec<crate::sim::MicrogridSite> = {
            let registry = config.microgrids();
            let r = registry.lock();
            std::iter::once(bootstrap)
                .chain(r.values().map(|e| e.site.clone()))
                .collect()
        };
        let taken: Vec<u64> = import
            .components
            .iter()
            .map(|c| c.id)
            .filter(|id| sites.iter().any(|s| s.get(*id).is_some()))
            .collect();
        if !taken.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "component ids already exist in other microgrids: {taken:?} \
                     (component ids are enterprise-unique)"
                ),
            ));
        }
    }
    let created = create_core(&config, &body.name, body.tso.as_deref())?;
    // Move the shared allocator past the imported ids before any
    // component is built, so nothing auto-allocates into that range.
    // Saturating: an export carrying id u64::MAX must not overflow
    // the bump (the allocator then sits at the ceiling, which only
    // affects auto-assigned ids, not the explicit imported ones).
    config.enterprise_id_allocator().fetch_max(
        import.max_id().saturating_add(1),
        std::sync::atomic::Ordering::SeqCst,
    );
    config.notify_microgrid_registered(created.id);
    // Populate the (empty) new microgrid through the per-mg eval
    // path. eval_in_mg holds the interpreter lock across scope-set +
    // eval + overrides append, and the progn is one form, so the
    // whole topology lands atomically — and persists for later
    // boots. spawn_blocking because tulisp's lock is std-sync.
    let forms = import.forms();
    let cfg = config.clone();
    let id = created.id;
    let evaled = super::blocking(move || cfg.eval_in_mg(id, &forms)).await?;
    if let Err(e) = evaled {
        // The runtime is already booted, so this cannot roll back
        // cleanly; the parse + collision checks above make this a
        // should-not-happen. Name the leftover so the user can act.
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "import failed while building components: {e} \
                 (microgrid {id} was created but is incomplete)"
            ),
        ));
    }
    Ok(Json(ImportMicrogridResp {
        id: created.id,
        name: created.name,
        grpc_port: created.grpc_port,
        tso: created.tso,
        components: import.components.len(),
        connections: import.connections.len(),
    }))
}

/// Write `microgrids/config.<id>.lisp` for a runtime-created entry.
/// The stub carries a `(make-microgrid …)` form pinned to this id /
/// port / tso, plus an empty `:topology` lambda that just
/// `(load-overrides)`s — the UI populates the topology over time by
/// appending to the per-mg overrides file next to this stub. Errors
/// out instead of clobbering an existing file (concurrent creates
/// shouldn't fight over the same path, but the registry already
/// dedups by id so this is just paranoia).
fn write_microgrid_stub(
    config: &Config,
    id: u64,
    name: &str,
    grpc_port: u16,
    tso: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let dir = config.microgrids_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join(format!("config.{id}.lisp"));
    if path.exists() {
        return Err(format!(
            "stub file {} already exists; refusing to clobber",
            path.display()
        ));
    }
    // The TSO is one of the four short codes ("TN" / "AM" / "HZ" /
    // "BW") or unset, so the same escaping rule covers name and TSO.
    use crate::lisp::escape_lisp_string as esc;
    let tso_form = match tso {
        Some(t) if !t.is_empty() => format!(" :tso \"{}\"", esc(t)),
        _ => String::new(),
    };
    let content = format!(
        ";; Runtime-created microgrid (id {id}). Edit by hand or via\n\
         ;; the UI — UI edits land in config.{id}.overrides.lisp next\n\
         ;; to this file.\n\
         \n\
         (make-microgrid\n\
        \x20:id {id}\n\
        \x20:name \"{name_esc}\"\n\
        \x20:grpc-port {grpc_port}{tso_form}\n\
        \x20:topology\n\
        \x20(lambda ()\n\
        \x20  (load-overrides)))\n",
        name_esc = esc(name),
    );
    std::fs::write(&path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    // Reload replays the loaded-file list; without this the created
    // microgrid would come back EMPTY from the next reload (its stub
    // is only read at load time, and nothing scans the stub dir).
    // Canonicalized to match how the loader spells it.
    let path = path.canonicalize().unwrap_or(path);
    config.record_loaded_file(path.clone());
    Ok(path)
}
