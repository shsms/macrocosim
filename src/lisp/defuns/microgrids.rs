//! `(make-microgrid)` + the `current-microgrid-id` / `microgrid-name`
//! accessors. Each `(make-microgrid …)` form mints a fresh
//! `MicrogridSite`, inserts a registry entry, flips the
//! `CurrentMicrogrid` pointer, and funcalls the `:topology` lambda
//! so nested make-* forms register into the new site.

use std::sync::Arc;

use tokio::sync::broadcast;
use tulisp::{TulispContext, TulispObject};

use crate::sim::MicrogridSite;

tulisp::AsPlist! {
    pub struct MakeMicrogridArgs {
        name: Option<String> {= None},
        id: Option<i64> {= None},
        grpc_port<":grpc-port">: Option<i64> {= None},
        /// Optional TSO zone label (informational; see
        /// `crate::sim::microgrids::MicrogridDef::tso`).
        tso: Option<String> {= None},
        /// Zero-arg lambda whose body builds the microgrid's
        /// topology — typically a single nested
        /// `(make-grid-connection-point …)` call. The lambda
        /// form is required (not a plain expression) because the
        /// body must evaluate *after* make-microgrid has set the
        /// current-microgrid pointer, so the nested make-* calls
        /// register into the new site instead of the previously-
        /// active one. Optional so a config can register an empty
        /// microgrid that the UI fills in component-by-component.
        topology: Option<crate::lisp::value::LispValue> {= None},
    }
}

/// Register `(make-microgrid …)`. Each call creates a fresh
/// `MicrogridSite`, inserts a registry entry for it, sets the
/// `CurrentMicrogrid` pointer, funcalls the `:topology` lambda
/// (whose body's make-* calls then register into the new site
/// via the router's per-call dispatch), and finally restores the
/// previous pointer.
pub(in crate::lisp) fn register(
    ctx: &mut TulispContext,
    registry: crate::sim::microgrids::SharedMicrogrids,
    current: crate::sim::microgrids::CurrentMicrogrid,
    id_allocator: Arc<std::sync::atomic::AtomicU64>,
    registered_tx: Arc<broadcast::Sender<u64>>,
    grid_frequency: crate::sim::frequency::SharedFrequency,
    loading: crate::sim::microgrids::LoadingSlot,
) {
    // `(current-source-file)` — the file whose load is in flight, or
    // nil outside a load (a REPL eval). Scripts use it to resolve
    // sibling data files, and to tell "I was loaded" from "I was
    // typed".
    {
        let slot = loading.clone();
        ctx.defun(
            "current-source-file",
            move || -> Result<TulispObject, tulisp::Error> {
                Ok(match slot.lock().as_ref() {
                    Some(f) => TulispObject::from(f.path.display().to_string()),
                    None => TulispObject::nil(),
                })
            },
        );
    }
    // Read-only accessors scripts use to dispatch on the active
    // microgrid. Outside a per-mg
    // context (e.g. boot before any (make-microgrid) form, or a
    // legacy /api/eval call without an mg scope) they fall back
    // to the first registry entry so single-microgrid configs
    // keep returning sensible values.
    {
        let cur = current.clone();
        let reg = registry.clone();
        ctx.defun(
            "current-microgrid-id",
            move || -> Result<i64, tulisp::Error> {
                if let Some(id) = *cur.read() {
                    return Ok(id as i64);
                }
                let r = reg.lock();
                Ok(r.keys().next().copied().unwrap_or(0) as i64)
            },
        );
    }
    {
        let cur = current.clone();
        let reg = registry.clone();
        ctx.defun(
            "microgrid-name",
            move || -> Result<String, tulisp::Error> {
                let id_opt = *cur.read();
                let r = reg.lock();
                let entry = id_opt
                    .and_then(|id| r.get(&id))
                    .or_else(|| r.values().next());
                Ok(entry.map(|e| e.def.name.clone()).unwrap_or_default())
            },
        );
    }
    use crate::sim::microgrids::{
        DEFAULT_MICROGRID_NAME, MicrogridDef, MicrogridEntry, next_free_id_in, next_free_port_in,
        with_microgrid,
    };
    ctx.defun(
        "make-microgrid",
        move |ctx: &mut TulispContext,
              args: tulisp::Plist<MakeMicrogridArgs>|
              -> Result<i64, tulisp::Error> {
            let a = args.into_inner();
            // `as u16` would silently wrap ports > 65535 onto a
            // different port — validate the range up front.
            if let Some(p) = a.grpc_port
                && !(1..=65535).contains(&p)
            {
                return Err(tulisp::Error::invalid_argument(format!(
                    "make-microgrid: :grpc-port {p} outside 1..=65535"
                )));
            }
            let name = a
                .name
                .clone()
                .unwrap_or_else(|| DEFAULT_MICROGRID_NAME.to_string());
            // Which file (if any) is being loaded right now. A
            // microgrid belongs to that file; one declared from the
            // REPL belongs to nothing.
            let loading_file = loading.lock().clone();
            // Re-registering an id that's already in the registry is
            // only legal from the file that owns it — the reload path
            // re-evaluating its own `(make-microgrid …)` form. That
            // REUSES the existing entry's site, reset in place: the
            // boot-spawned physics + history tasks, the per-port gRPC
            // server, and the loopback client all hold that site
            // handle, and minting a fresh one would orphan every
            // runtime (the old site would keep ticking and serving
            // gRPC while the registry acts on a site that never
            // ticks). Name / tso update live; the gRPC port is pinned
            // by the listening server, so a changed :grpc-port is
            // kept as-is with a warning.
            //
            // Any OTHER claimant — a second file, or a REPL form — is
            // a hard error naming the owner, because silently merging
            // two unrelated definitions onto one site is how a
            // "loaded" microgrid ends up being neither file's.
            //
            // One registry lock for the id probe, the port probe, the
            // reuse check, AND the insert — separate acquisitions let
            // a concurrent /api/microgrids/create hand out the same
            // id or port between our probe and our insert.
            let (id, reused) = {
                let mut reg = registry.lock();
                let id = match a.id {
                    Some(v) if v > 0 => v as u64,
                    // Reject negative ids instead of silently
                    // auto-allocating: a typo like :id -2200 would
                    // otherwise boot under an auto id with the wrong
                    // overrides file and gRPC port, no diagnostic.
                    // (:id 0 stays the documented auto sentinel.)
                    Some(v) if v < 0 => {
                        return Err(tulisp::Error::invalid_argument(format!(
                            "make-microgrid: :id {v} must not be negative"
                        )));
                    }
                    _ => next_free_id_in(&reg),
                };
                let existing = reg
                    .get(&id)
                    .map(|e| (e.def.grpc_port, e.site.clone(), e.source.clone()));
                let (grpc_port, site, reused) = match existing {
                    Some((bound_port, site, source)) => {
                        // Same file re-declaring its own microgrid?
                        // Both sides carry canonicalized paths, so a
                        // relative and an absolute spelling of one
                        // file compare equal.
                        let same_file = match (&loading_file, &source) {
                            (Some(l), Some(s)) => l.path == *s,
                            _ => false,
                        };
                        if !same_file {
                            let owner = match &source {
                                Some(p) => format!("from {}", p.display()),
                                None => "from the REPL".to_string(),
                            };
                            return Err(tulisp::Error::invalid_argument(format!(
                                "microgrid {id} is already loaded ({owner})"
                            )));
                        }
                        if let Some(p) = a.grpc_port
                            && p as u16 != bound_port
                        {
                            log::warn!(
                                "make-microgrid #{id}: :grpc-port {p} ignored — the running \
                                 gRPC server is bound to :{bound_port} (restart to move it)"
                            );
                        }
                        site.reset();
                        (bound_port, site, true)
                    }
                    None => {
                        let grpc_port = match a.grpc_port {
                            Some(p) => {
                                let p = p as u16;
                                // A collision registers cleanly here but
                                // the binary's spawner then fails the
                                // bind and SKIPS this microgrid's gRPC
                                // server with only a log line — a
                                // registry entry that looks healthy and
                                // serves nothing. Reject up front, under
                                // the same lock as the insert.
                                if let Some((other, _)) =
                                    reg.iter().find(|(_, e)| e.def.grpc_port == p)
                                {
                                    return Err(tulisp::Error::invalid_argument(format!(
                                        "make-microgrid #{id}: :grpc-port {p} is already \
                                         bound by microgrid {other}"
                                    )));
                                }
                                p
                            }
                            None => next_free_port_in(&reg),
                        };
                        // Fresh site per microgrid that shares the
                        // enterprise's id allocator with the bootstrap site
                        // + every other microgrid — component ids stay
                        // globally unique across the registry without
                        // per-site coordination.
                        let site = MicrogridSite::with_id_allocator(id_allocator.clone());
                        // Same grid frequency source as every other site,
                        // so their `frequency_hz` reads all return the same
                        // OU value (one AC grid → one frequency).
                        site.set_grid_frequency(grid_frequency.clone());
                        (grpc_port, site, false)
                    }
                };
                let def = MicrogridDef {
                    id,
                    name,
                    grpc_port,
                    tso: a.tso.clone(),
                };
                reg.insert(
                    id,
                    MicrogridEntry {
                        def,
                        site: site.clone(),
                        source: loading_file.as_ref().map(|l| l.path.clone()),
                        managed: loading_file.as_ref().is_some_and(|l| l.managed),
                        // A file just (re)declared the microgrid, so
                        // disk and memory agree by construction.
                        unsaved: false,
                    },
                );
                (id, reused)
            };
            // Funcall the :topology lambda (if any) with the
            // current-microgrid pointer flipped to this new entry, so
            // the nested make-* calls register into the new site.
            // This runs BEFORE the registered notification: a failing
            // topology must not leave a zombie microgrid announced to
            // subscribers — a fresh entry is removed again on error
            // (a reused one stays; its reset site is the same state a
            // failed reload leaves).
            if let Some(topology) = a.topology {
                let lambda = topology.into_inner();
                if !lambda.null() {
                    let nil = TulispObject::nil();
                    if let Err(e) = with_microgrid(&current, id, || ctx.funcall(&lambda, &nil)) {
                        if !reused {
                            registry.lock().remove(&id);
                        }
                        return Err(e);
                    }
                }
            }
            // Notify enterprise-wide subscribers (the WS event pump
            // and the binary's runtime spawner) that a new microgrid
            // landed. Reused entries skip this — their forwarders and
            // runtimes already exist. send() returns Err when there
            // are no live receivers — fine to ignore; it just means
            // no UI session is open.
            if !reused {
                let _ = registered_tx.send(id);
            }
            Ok(id as i64)
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::config_with;

    /// A second FILE claiming an id another file already loaded is a
    /// hard error, and the message names the owning file so the
    /// operator knows which one to edit.
    #[test]
    fn loading_a_second_file_with_a_taken_id_errors() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let other = dir.join("other.lisp");
        std::fs::write(
            &other,
            "(make-microgrid :id 9 :grpc-port 8801 :topology (lambda () nil))",
        )
        .unwrap();
        let err = cfg.load_file(&other).unwrap_err();
        assert!(err.contains("microgrid 9 is already loaded"), "{err}");
        assert!(
            err.contains("config.lisp"),
            "error names the owning file: {err}"
        );
    }

    /// Re-loading the SAME file keeps the reuse-in-place semantics the
    /// live runtimes depend on: same site, reset and rebuilt.
    #[test]
    fn reloading_the_same_file_reuses_the_entry_in_place() {
        let (cfg, dir) = config_with(
            "(make-microgrid :id 9 :grpc-port 8800 :topology \
             (lambda () (%make-grid-connection-point :id 1)))",
        );
        let live = cfg.microgrids().lock().get(&9).unwrap().site.clone();
        let path = dir.join("config.lisp");
        cfg.load_file(&path).expect("same-file reload allowed");
        assert!(live.get(1).is_some(), "same site, rebuilt in place");
    }

    /// The REPL has no loading file, so re-declaring a loaded id from
    /// an eval collides too — and says the id came from a file.
    #[test]
    fn repl_make_microgrid_with_taken_id_errors() {
        let (cfg, _dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let err = cfg
            .eval("(make-microgrid :id 9 :grpc-port 8801 :topology (lambda () nil))")
            .unwrap_err();
        assert!(err.contains("already loaded"), "{err}");
    }

    /// Component ids are enterprise-unique: a second microgrid reusing
    /// a component id fails its load, leaving nothing registered.
    #[test]
    fn component_id_collisions_across_microgrids_fail_the_load() {
        let (cfg, dir) = config_with(
            "(make-microgrid :id 9 :grpc-port 8800 :topology \
             (lambda () (%make-meter :id 42)))",
        );
        let other = dir.join("other.lisp");
        std::fs::write(
            &other,
            "(make-microgrid :id 10 :grpc-port 8801 :topology \
             (lambda () (%make-meter :id 42)))",
        )
        .unwrap();
        let err = cfg.load_file(&other).unwrap_err();
        assert!(err.contains("42"), "{err}");
        assert!(
            !cfg.microgrids().lock().contains_key(&10),
            "nothing registered"
        );
    }

    /// `load_as` copies a managed file under a free id, rewriting the
    /// id inside the generated block; unmanaged files are refused.
    #[test]
    fn load_as_copies_and_rewrites_the_id() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let src = dir.join("managed.lisp");
        std::fs::write(
            &src,
            crate::lisp::microgrid_file::compose(
                "(make-microgrid :id 9 :name \"m\" :grpc-port 8890\n  :topology\n  (lambda ()\n    nil))",
                "",
            ),
        )
        .unwrap();
        let id = cfg.load_as(&src, 11).expect("load as free id");
        assert_eq!(id, 11);
        assert!(cfg.microgrids().lock().contains_key(&11));
        assert!(dir.join("microgrids/11.lisp").exists());
        // Unmanaged files are refused.
        let raw = dir.join("raw.lisp");
        std::fs::write(&raw, "(make-microgrid :id 9 :topology (lambda () nil))").unwrap();
        assert!(cfg.load_as(&raw, 12).is_err());
    }

    /// A loaded microgrid remembers the file it came from; a REPL one
    /// does not.
    #[test]
    fn entries_record_their_source_file() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        cfg.eval("(make-microgrid :id 10 :grpc-port 8801 :topology (lambda () nil))")
            .unwrap();
        let reg = cfg.microgrids();
        let r = reg.lock();
        let nine = r.get(&9).unwrap();
        assert_eq!(
            nine.source.as_deref(),
            Some(dir.join("config.lisp").canonicalize().unwrap().as_path()),
        );
        assert!(!nine.managed, "a plain script is unmanaged");
        assert!(
            r.get(&10).unwrap().source.is_none(),
            "REPL entries have no source"
        );
    }

    /// `(current-source-file)` names the file being loaded, and is nil
    /// outside a load.
    #[test]
    fn current_source_file_reports_the_loading_file() {
        let (cfg, dir) = config_with(
            "(make-microgrid :id 9 :grpc-port 8800 :topology \
             (lambda () (setq seen (current-source-file))))",
        );
        let seen = cfg.eval_silent("seen").unwrap();
        assert!(
            seen.contains("config.lisp"),
            "topology should see its own file: {seen}"
        );
        let _ = dir;
        assert_eq!(cfg.eval_silent("(current-source-file)").unwrap(), "nil");
    }

    /// `config_with` auto-wraps a body lacking `(make-microgrid …)`
    /// into a single-entry registration. The id is sourced from any
    /// inline `(set-microgrid-id N)` (a leftover from the pre-
    /// migration test fixture shape), keeping the body's intended
    /// microgrid id stable.
    #[test]
    fn auto_wrapper_registers_single_microgrid_from_set_microgrid_id() {
        let (cfg, _dir) = config_with("(set-microgrid-id 4242)");
        let reg = cfg.microgrids();
        let r = reg.lock();
        assert_eq!(r.len(), 1);
        let e = r.get(&4242).expect("auto-wrapped under set-microgrid-id");
        assert_eq!(e.def.name, "default");
        assert_eq!(e.def.grpc_port, 8800);
    }

    /// `(make-microgrid …)` builds a *new* site for the entry and
    /// funcalls the :topology lambda with the current-microgrid
    /// pointer set to the new id. Nested make-* calls register
    /// into that fresh site, not the bootstrap or any prior
    /// microgrid's site.
    #[test]
    fn make_microgrid_registers_entry_and_topology() {
        let (cfg, _dir) = config_with("(set-microgrid-id 0)");
        cfg.eval(
            r#"
            (make-microgrid
              :name "south yard"
              :id 7777
              :grpc-port 8810
              :tso "TN"
              :topology
              (lambda ()
                (%make-grid-connection-point :id 1)))
            "#,
        )
        .unwrap();
        let reg = cfg.microgrids();
        let r = reg.lock();
        let e = r.get(&7777).expect("registered");
        assert_eq!(e.def.name, "south yard");
        assert_eq!(e.def.grpc_port, 8810);
        assert_eq!(e.def.tso.as_deref(), Some("TN"));
        // The :topology lambda ran with current-microgrid pinned
        // to the new id, so the grid component lives on the new
        // microgrid's own site — NOT on the bootstrap site.
        assert!(
            e.site.get(1).is_some(),
            "grid-connection-point id=1 should be on the new site",
        );
    }

    /// Re-running `(make-microgrid …)` for an id that's already
    /// registered must reuse the existing entry's site (reset in
    /// place), not mint a fresh one — the boot-spawned runtimes and
    /// the per-port gRPC server all hold the original handle, and a
    /// fresh site would orphan them (frozen physics, stale gRPC).
    ///
    /// Re-declaring an id is only legal from the file that owns it
    /// (see `loading_a_second_file_with_a_taken_id_errors`), so the
    /// re-registration here goes through a re-load of that file —
    /// which is the reload path's real shape anyway.
    #[test]
    fn make_microgrid_reuses_the_existing_site_on_reregistration() {
        let (cfg, dir) = config_with(
            r#"
            (make-microgrid
              :name "yard" :id 7000 :grpc-port 8810
              :topology (lambda () (%make-grid-connection-point :id 1)))
            "#,
        );
        // The handle a boot-spawned runtime would hold.
        let live_site = cfg.microgrids().lock().get(&7000).unwrap().site.clone();
        assert!(live_site.get(1).is_some());

        // Re-register the same id with a different topology.
        let path = dir.join("config.lisp");
        std::fs::write(
            &path,
            r#"
            (make-microgrid
              :name "yard v2" :id 7000 :grpc-port 8810
              :topology (lambda () (%make-grid-connection-point :id 2)))
            "#,
        )
        .unwrap();
        cfg.load_file(&path).unwrap();

        // The pre-rerun handle sees the new topology: same site,
        // reset and rebuilt in place.
        assert!(live_site.get(1).is_none(), "old component is gone");
        assert!(live_site.get(2).is_some(), "new component on the SAME site");
        let entry = cfg.microgrids().lock().get(&7000).cloned().unwrap();
        assert_eq!(entry.def.name, "yard v2");
        assert!(entry.site.get(2).is_some());
    }

    /// Auto-allocated component ids stay globally unique across
    /// microgrids: each `(make-meter)` consumes the next entry on
    /// the enterprise-wide allocator, regardless of which site
    /// receives the component.
    #[test]
    fn auto_ids_are_globally_unique_across_microgrids() {
        let (cfg, _dir) = config_with("(set-microgrid-id 0)");
        let ids: String = cfg
            .eval(
                r#"
                (let (a b c)
                  (make-microgrid :name "alpha" :id 3200
                                  :topology (lambda ()
                                              (setq a (component-id (%make-meter)))))
                  (make-microgrid :name "beta"  :id 3201
                                  :topology (lambda ()
                                              (setq b (component-id (%make-meter)))))
                  (make-microgrid :name "gamma" :id 3202
                                  :topology (lambda ()
                                              (setq c (component-id (%make-meter)))))
                  (format "%d/%d/%d" a b c))
                "#,
            )
            .unwrap()
            .trim_matches('"')
            .to_string();
        let parts: Vec<u64> = ids.split('/').map(|s| s.parse().unwrap()).collect();
        assert_eq!(parts.len(), 3);
        // Distinct values, all >= FIRST_AUTO_ID.
        assert_ne!(parts[0], parts[1]);
        assert_ne!(parts[1], parts[2]);
        assert_ne!(parts[0], parts[2]);
        for p in &parts {
            assert!(*p >= crate::sim::component::FIRST_AUTO_ID);
        }
    }

    /// Two microgrids end up with isolated sites — adding a grid
    /// to one doesn't leak into the other.
    #[test]
    fn two_microgrids_have_isolated_sites() {
        let (cfg, _dir) = config_with("(set-microgrid-id 0)");
        cfg.eval(
            r#"
            (make-microgrid :name "alpha" :id 1001
                            :topology (lambda ()
                                        (%make-grid-connection-point :id 1)))
            (make-microgrid :name "beta"  :id 1002
                            :topology (lambda ()
                                        (%make-grid-connection-point :id 2)))
            "#,
        )
        .unwrap();
        let reg = cfg.microgrids();
        let r = reg.lock();
        let a = r.get(&1001).unwrap();
        let b = r.get(&1002).unwrap();
        // Each microgrid sees its own grid component.
        assert!(a.site.get(1).is_some(), "alpha owns id=1");
        assert!(b.site.get(2).is_some(), "beta owns id=2");
        // Neither sees the other's.
        assert!(a.site.get(2).is_none(), "alpha doesn't see beta's id=2");
        assert!(b.site.get(1).is_none(), "beta doesn't see alpha's id=1");
    }

    /// When :id / :grpc-port are omitted, make-microgrid hands out
    /// the next free values starting at the registry's known
    /// floors.
    #[test]
    fn make_microgrid_auto_allocates_id_and_port() {
        let (cfg, _dir) = config_with("(set-microgrid-id 0)");
        let first: i64 = cfg
            .eval("(make-microgrid :name \"alpha\")")
            .unwrap()
            .parse()
            .unwrap();
        let second: i64 = cfg
            .eval("(make-microgrid :name \"beta\")")
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            second > first,
            "auto-allocated ids must be strictly increasing"
        );
        let r = cfg.microgrids();
        let g = r.lock();
        let a = g.get(&(first as u64)).unwrap();
        let b = g.get(&(second as u64)).unwrap();
        assert_ne!(a.def.grpc_port, b.def.grpc_port);
    }
}
