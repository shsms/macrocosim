//! The microgrid collection: list them, create them from a topology
//! document, import one from a site export (both in memory only, with
//! a stored spec), and the Lisp side until it goes: load one from a
//! file on disk (`/api/load`, plus `/api/load-as` for a second copy
//! under a free id), and adopt a hand-written file so macrocosim may
//! rewrite its structure. Load-as waits for the new microgrid's
//! runtime and reports it.

use axum::{extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::engine::{Engine, topology};
use crate::lisp::Config;
use crate::runtime::{MicrogridRuntimes, RuntimeView};
use crate::spec::TopologyDoc;
use crate::ui::api::{ApiError, Json, Mg};

/// One `/api/microgrids` entry: the registry's view of the microgrid
/// plus its runtime, `null` when none was started.
#[derive(Serialize)]
pub(in crate::ui) struct MicrogridListEntry {
    #[serde(flatten)]
    view: crate::sim::microgrids::MicrogridView,
    runtime: Option<RuntimeView>,
}

pub(in crate::ui) async fn microgrids_list(
    State(engine): State<Engine>,
    axum::Extension(runtimes): axum::Extension<MicrogridRuntimes>,
) -> Json<Vec<MicrogridListEntry>> {
    Json(
        crate::sim::microgrids::snapshot(&engine.microgrids())
            .into_iter()
            .map(|view| {
                let runtime = runtimes.status(view.id);
                MicrogridListEntry { view, runtime }
            })
            .collect(),
    )
}

/// Start `id`'s runtime and report it. Of this call and the
/// listener's, the one that runs second finds the runtime running and
/// only reads the status, or tries again after a failure. A start
/// failure is reported, not returned: the microgrid exists either
/// way.
pub(in crate::ui) async fn start_and_report(
    runtimes: &MicrogridRuntimes,
    id: u64,
) -> Option<RuntimeView> {
    if let Err(e) = runtimes.start(id).await {
        log::error!("Microgrid #{id}: {e}");
    }
    runtimes.status(id)
}

/// POST /api/microgrids — create the microgrids of a topology
/// document, in memory only. A microgrid without an id or a
/// `grpc_port` takes the lowest free one; every component needs an id
/// (422 without). Each microgrid's runtime is started and reported in
/// the 201, `null` when none was started: the microgrid exists either
/// way. 409 when a microgrid id, a component id or a port is taken;
/// nothing is created then.
pub(in crate::ui) async fn microgrids_create(
    State(engine): State<Engine>,
    axum::Extension(runtimes): axum::Extension<MicrogridRuntimes>,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let doc = TopologyDoc::from_value(body)
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    if doc.microgrids.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the document has no microgrids",
        ));
    }
    let create_lock = engine.create_lock();
    let _structural = create_lock.lock().await;
    let created = topology::create_doc(&engine, &runtimes, doc, None).await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "microgrids": created })),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct ImportMicrogridBody {
    name: String,
    /// Microgrid id to claim, the one the import dialog asks for.
    /// Omit to take the lowest free one, as every import did before
    /// the dialog existed — the field is additive.
    #[serde(default)]
    id: Option<u64>,
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
    /// The new microgrid's runtime; `null` when none was started.
    runtime: Option<RuntimeView>,
}

/// POST /api/microgrids/import — create a microgrid from a site
/// export, in memory only, as `POST /api/microgrids` does: the export
/// becomes a spec that keeps its component ids, and capacity, SoC
/// bounds, rated power bounds and the grid's rated fuse current all
/// survive into the simulation.
///
/// Component ids are unique across the engine, so an id another
/// microgrid holds refuses the whole import with a 409 and nothing is
/// created. The MICROGRID's own id is a separate matter: `id` claims
/// one (the import dialog asks for it, pre-filled with the lowest
/// free one), and omitting it takes the lowest free one; a taken one
/// is a 409 too.
pub(in crate::ui) async fn microgrids_import(
    State(engine): State<Engine>,
    axum::Extension(runtimes): axum::Extension<MicrogridRuntimes>,
    Json(body): Json<ImportMicrogridBody>,
) -> Result<Json<ImportMicrogridResp>, ApiError> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name must be non-empty"));
    }
    let import = crate::sim::site_import::parse(body.components, body.connections)
        .map_err(ApiError::bad_request)?;
    let mut spec = import.spec().map_err(ApiError::bad_request)?;
    spec.id = body.id;
    spec.name = Some(name.clone());
    spec.tso = body.tso;
    let create_lock = engine.create_lock();
    let _structural = create_lock.lock().await;
    let created = topology::create(&engine, &runtimes, spec, None).await?;
    let tso = engine
        .microgrids()
        .lock()
        .get(&created.id)
        .and_then(|e| e.def.tso.clone());
    Ok(Json(ImportMicrogridResp {
        id: created.id,
        name,
        grpc_port: created.grpc_port,
        tso,
        components: import.components.len(),
        connections: import.connections.len(),
        runtime: created.runtime,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct LoadBody {
    /// Path of the file to load. Relative paths resolve against the
    /// state dir, the same anchor `(load …)` uses.
    path: String,
}

/// POST /api/load — evaluate a microgrid file and register whatever
/// microgrids it declares.
///
/// `loaded` lists the microgrids the file BACKS once it has run, not
/// just the ones it newly minted: re-loading a live file reuses its
/// entries in place, and reporting `[]` for that would read as "the
/// file did nothing". An genuinely empty list means the file ran and
/// registered nothing at all — legal (a driver-only script) but
/// worth saying out loud, since the load picker's job is to put
/// microgrids on screen.
///
/// The interesting failure is a collision: the file declares an id
/// some other file already loaded. That gets a 409 carrying the id,
/// whether the file is a managed one (only those can be re-idded
/// mechanically), and a free id to offer — so the UI can turn the
/// refusal into a "load it as N instead?" button that posts to
/// `/api/load-as`.
pub(in crate::ui) async fn load_file(
    State(config): State<Config>,
    Json(body): Json<LoadBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cfg = config.clone();
    let path = std::path::PathBuf::from(&body.path);
    let loaded = super::blocking(move || cfg.load_file(&path)).await?;
    match loaded {
        Ok(_) => {
            let ids = config.microgrids_backed_by(std::path::Path::new(&body.path));
            if ids.is_empty() {
                log::warn!(
                    "load {}: the file ran but registered no microgrid",
                    body.path
                );
            }
            Ok(Json(serde_json::json!({ "loaded": ids })))
        }
        Err(crate::lisp::LoadError::Collision { id }) => {
            let suggested = crate::sim::microgrids::next_free_id(&config.microgrids());
            Err(collision_response(&config, &body.path, id, suggested))
        }
        Err(e) => Err(ApiError::bad_request(e.to_string())),
    }
}

/// The 409 body for a collision. `managed` decides which offer the
/// UI makes: a managed file can be re-idded by `/api/load-as`, a
/// hand-written one has to be edited by a person.
fn collision_response(
    config: &Config,
    path: &str,
    collision_id: u64,
    suggested_id: u64,
) -> ApiError {
    let resolved = config.resolve_in_state_dir(std::path::Path::new(path));
    let managed = std::fs::read_to_string(&resolved)
        .ok()
        .and_then(|text| crate::lisp::microgrid_file::parse(&text).ok())
        .is_some_and(|parsed| parsed.generated.is_some());
    ApiError::conflict(format!("microgrid {collision_id} is already loaded"))
        .with("collision_id", collision_id)
        .with("managed", managed)
        .with("suggested_id", suggested_id)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::ui) struct LoadAsBody {
    path: String,
    /// Id the copy is registered under.
    id: u64,
}

/// POST /api/load-as — copy a managed microgrid file under a free id
/// and load the copy, so one file can back two live microgrids. The
/// answer to the collision 409 above.
///
/// Three outcomes, not two. A clean load is `200 {id, runtime}`. A
/// load that left nothing behind (the copy was cleaned up) is the 409
/// the caller can retry under a different id. In between sits the
/// COMMITTED PARTIAL: the copy's generated block registered and its
/// script section then failed, so the microgrid is live and its file
/// is kept. That one is `200 {id, runtime, warning}` — reporting it
/// as a 409 would read as the collision code and invite a retry that
/// then hits "target exists", or mints a second copy of a microgrid
/// that already loaded. Both 200s carry the copy's runtime, `null`
/// when none was started.
pub(in crate::ui) async fn load_file_as(
    State(config): State<Config>,
    axum::Extension(runtimes): axum::Extension<MicrogridRuntimes>,
    Json(body): Json<LoadAsBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let path = std::path::PathBuf::from(&body.path);
    let id = body.id;
    match super::blocking_under_create_lock(&config, move |cfg| cfg.load_as(&path, id)).await? {
        Ok(id) => Ok(Json(serde_json::json!({
            "id": id,
            "runtime": start_and_report(&runtimes, id).await,
        }))),
        // `load_as` typed this for us — it is the only thing that
        // knows whether the copy got as far as registering, so the
        // handler never has to ask the registry and never has to
        // guess which of several refusals it is looking at.
        Err(e) => match &e {
            crate::lisp::LoadAsError::CommittedPartial { id, .. } => Ok(Json(serde_json::json!({
                "id": id,
                "runtime": start_and_report(&runtimes, *id).await,
                "warning": e.to_string(),
            }))),
            crate::lisp::LoadAsError::Other(_) => Err(ApiError::conflict(e.to_string())),
        },
    }
}

/// POST /api/mg/{mg}/adopt — take a hand-written microgrid file
/// over, so macrocosim may rewrite its structure from then on.
///
/// The live structure is written as a generated block at the top of
/// the file and the original `(make-microgrid …)` form is commented
/// out below it, where it stays readable as a record of what the file
/// used to say. Everything else in the file — comments, `every`
/// blocks, defuns — is carried through untouched and keeps running.
///
/// A microgrid with no file at all (declared from the REPL) gets a
/// fresh `microgrids/{id}.lisp` instead.
pub(in crate::ui) async fn adopt(
    State(config): State<Config>,
    mg: Mg,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mg_id = mg.id;
    let warnings = super::blocking(move || adopt_body(&config, mg_id)).await??;
    Ok(Json(serde_json::json!({ "warnings": warnings })))
}

/// [`adopt`]'s body: all blocking (file read + write) work.
/// Returns the warnings the caller should show.
fn adopt_body(config: &Config, mg_id: u64) -> Result<Vec<String>, ApiError> {
    use crate::lisp::microgrid_file as file;

    let registry = config.microgrids();
    let (def, site, source, managed) = {
        let r = registry.lock();
        let e = r
            .get(&mg_id)
            .ok_or_else(|| ApiError::not_registered(mg_id))?;
        if e.spec.is_some() {
            return Err(ApiError::conflict(
                crate::sim::microgrids::spec_built_refusal(mg_id),
            ));
        }
        (e.def.clone(), e.site.clone(), e.source.clone(), e.managed)
    };
    if managed {
        return Err(ApiError::conflict(format!(
            "microgrid {mg_id} is already managed"
        )));
    }
    // Live state the generated block cannot write down — a
    // lambda-bound input, or a value poked in at runtime that was
    // never a constructor argument. Reported, not refused: the
    // structure still round-trips, but these inputs come back at
    // their constructed values and have to be set again from the
    // script section.
    let warnings: Vec<String> = site
        .components()
        .iter()
        .filter(|c| c.has_unrenderable_source())
        .map(|c| {
            format!(
                "component {} ({}) carries an input value the generated block cannot \
                 write down — set it again from the script section",
                c.id(),
                c.make_fn()
            )
        })
        .collect();

    let block = file::render_block(&def, &site);
    let (path, text) = match source {
        Some(path) => {
            // One file, one microgrid: adopting rewrites the whole
            // file from ONE microgrid's live state, so a file that
            // declares several would lose the others.
            let sharers = registry
                .lock()
                .values()
                .filter(|e| e.source.as_deref() == Some(path.as_path()))
                .count();
            if sharers > 1 {
                return Err(ApiError::conflict(format!(
                    "{} declares {sharers} microgrids; split the file first, one \
                         microgrid per file",
                    path.display()
                )));
            }
            let original = std::fs::read_to_string(&path)
                .map_err(|e| ApiError::internal(format!("cannot read {}: {e}", path.display())))?;
            let script = comment_out_make_microgrid(&original, mg_id)
                .map_err(|e| ApiError::conflict(format!("{}: {e}", path.display())))?;
            (path, file::compose(&block, &script))
        }
        // Nothing on disk backs this microgrid yet — give it the same
        // file the create endpoint would have written.
        None => {
            let path = config.microgrids_dir().join(format!("{mg_id}.lisp"));
            if path.exists() {
                return Err(ApiError::conflict(format!(
                    "{} already exists; refusing to clobber",
                    path.display()
                )));
            }
            (path, file::compose(&block, file::FRESH_SCRIPT_HEADER))
        }
    };
    file::write_atomic(&path, &text)
        .map_err(|e| ApiError::internal(format!("write {}: {e}", path.display())))?;
    // Our own write: the watcher must not read it back as a human
    // edit and reload the file underneath us.
    config.record_self_write(&path, &text);
    let canonical = path.canonicalize().unwrap_or(path);
    config.note_source_file(&canonical);
    if let Some(entry) = registry.lock().get_mut(&mg_id) {
        entry.source = Some(canonical);
        entry.managed = true;
        // The file now carries exactly what is live.
        entry.unsaved = false;
    }
    Ok(warnings)
}

/// Comment out the top-level `(make-microgrid …)` form for `mg_id` in
/// `text`, leaving every other line as it was. The generated block
/// composed above the result replaces what the form used to do, so
/// leaving it live would declare the microgrid twice.
///
/// Errors when the form cannot be found at top level — a file that
/// builds its microgrid some other way (inside a `let`, from a
/// helper defun) is not something adopt can mechanically supersede.
fn comment_out_make_microgrid(text: &str, mg_id: u64) -> Result<String, String> {
    use tulisp_fmt::cst::CstNode;

    let cst = tulisp_fmt::parse(text).map_err(|e| format!("failed to parse: {e:?}"))?;
    type ParsedId = Option<Result<u64, std::num::ParseIntError>>;
    let forms: Vec<(std::ops::Range<usize>, ParsedId)> = cst
        .nodes
        .iter()
        .filter_map(|n| {
            let CstNode::List { children, .. } = n else {
                return None;
            };
            let mut atoms = children.iter().filter_map(|c| match c {
                CstNode::Atom { text, .. } => Some(text.as_str()),
                _ => None,
            });
            if atoms.next() != Some("make-microgrid") {
                return None;
            }
            // Whatever the form's `:id` says, if it says anything.
            let declared = atoms
                .clone()
                .skip_while(|a| *a != ":id")
                .nth(1)
                .map(|v| v.parse::<u64>());
            Some((n.span(), declared))
        })
        .collect();
    let span = match forms.iter().find(|(_, id)| *id == Some(Ok(mg_id))) {
        Some((span, _)) => span.clone(),
        // One form with no explicit `:id` got an auto-allocated one,
        // and with a single form in the file that is ours.
        None if forms.len() == 1 && forms[0].1.is_none() => forms[0].0.clone(),
        None => {
            return Err(format!(
                "no top-level (make-microgrid …) form for microgrid {mg_id} found"
            ));
        }
    };
    // Start from the beginning of the form's line so a form indented
    // under a comment column still comments out cleanly.
    let start = text[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let commented: String = text[start..span.end]
        .lines()
        .map(|l| format!(";; {l}\n"))
        .collect();
    Ok(format!(
        "{};; superseded by the generated block above:\n{}{}",
        &text[..start],
        commented,
        &text[span.end..]
    ))
}
