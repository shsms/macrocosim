//! `Config::eval` and the persistence it triggers.
//!
//! A successful eval that moved a microgrid's structure regenerates
//! that microgrid's managed file from live state
//! (`Config::persist`); one that touched enterprise-wide state — a
//! `*-defaults` plist or a metadata setter — regenerates
//! `enterprise.lisp` (`Config::persist_enterprise`). Runtime pokes
//! (`set-meter-power`, health flips, scenario steps) change no file:
//! the script section is where a hand-written poke belongs.
//!
//! The `persisted_overrides*` / `overrides_text*` functions below
//! serve the legacy overrides journal dialog. Nothing writes a
//! journal any more; they are read/prune paths over files an older
//! switchyard wrote, and go away with their routes.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

use super::microgrid_file;
use super::{Config, DEFAULT_CATEGORIES};

/// One top-level form found in the per-microgrid override file. The
/// `idx` is the form's 0-based position; stable until the next
/// `remove_persisted_overrides` rewrites the file. `source` is the
/// form rendered via tulisp's `Display` impl — round-trips through
/// eval but doesn't preserve the original spelling (comments
/// stripped, whitespace normalized).
#[derive(Debug, Clone, Serialize)]
pub struct PersistedOverride {
    pub idx: usize,
    pub source: String,
}

impl Config {
    /// Evaluate `src` in the interpreter and, on success, save
    /// whatever it changed. `eval_string` returns the final form's
    /// value; we stringify it via `Display` and return the result
    /// formatted via `Display`. Errors are formatted with full trace
    /// context the same way the reload path's logger formats them.
    ///
    /// Synchronous — acquires the interpreter write lock for the
    /// duration of the eval. Callers in async contexts must wrap in
    /// `tokio::task::spawn_blocking` to keep the executor free.
    ///
    /// Errored evals save nothing — a half-applied topology change
    /// shouldn't be written out as the microgrid's new structure.
    /// Either way the MicrogridSite version bumps so UI subscribers
    /// refetch.
    pub fn eval(&self, src: &str) -> Result<String, String> {
        let mut ctx = self.ctx.borrow_mut();
        self.eval_locked(&mut ctx, src)
    }

    /// Per-microgrid scoped eval — `/api/mg/{id}/eval`'s backend.
    /// Scope-set, eval, save, and the version bump all happen under
    /// one interpreter-lock acquisition, so two concurrent scoped
    /// evals can't cross microgrids (the scope pointer is ambient
    /// global state every scoped defun reads).
    pub fn eval_in_mg(&self, mg_id: u64, src: &str) -> Result<String, String> {
        self.scoped(mg_id, |cfg, ctx| cfg.eval_locked(ctx, src))
    }

    /// Run `f` with the interpreter locked and `current_microgrid`
    /// flipped to `mg_id` (restored on exit, panic included). The
    /// one sanctioned way to do a scoped operation from Rust: the
    /// pointer only ever flips while the interpreter lock is held,
    /// so no in-flight eval can observe a foreign scope.
    pub fn scoped<R>(
        &self,
        mg_id: u64,
        f: impl FnOnce(&Self, &mut tulisp::TulispContext) -> R,
    ) -> R {
        let mut ctx = self.ctx.borrow_mut();
        crate::sim::microgrids::with_microgrid(&self.current_microgrid, mg_id, || f(self, &mut ctx))
    }

    /// `eval` body against an already-held interpreter guard. The
    /// save pass and the version bump stay inside the locked section
    /// because both resolve the ambient scope pointer — after the
    /// lock is released a concurrent scoped call may flip it.
    ///
    /// An eval consisting only of `(load "file")` forms is a load,
    /// not an edit, so it is routed through [`Config::load_file`]
    /// instead of a bare `eval_string`: the microgrids the file
    /// registers are attributed to it, and its path joins the
    /// reload-replay list.
    ///
    /// A load MIXED with other top-level forms still runs through the
    /// plain eval path (the other forms need it); the loaded files
    /// are recorded for reload just the same.
    fn eval_locked(&self, ctx: &mut tulisp::TulispContext, src: &str) -> Result<String, String> {
        let (loads, has_other_forms) = top_level_load_paths(src);
        // A pure-load eval IS a load: route it through the loader so
        // the file becomes the ambient source of whatever microgrids
        // it registers, and joins the reload-replay list. Every such
        // load is recorded — the file is the artifact, and a file the
        // operator asked for is part of the world whether or not it
        // happened to move the topology.
        if !loads.is_empty() && !has_other_forms {
            for path in &loads {
                // `load_file_locked` already bumped exactly the sites
                // the load created or rebuilt; the ambient site the
                // other paths bump is not necessarily one of them.
                self.load_file_locked(ctx, path)?;
            }
            // `(load …)` is a statement, not an expression; mirror
            // elisp and report success rather than a file's last form.
            return Ok("t".to_string());
        }
        let before = self.structural_versions();
        let result = match ctx.eval_string(src) {
            Ok(v) => Ok(v.to_string()),
            Err(e) => Err(e.format(ctx)),
        };
        if result.is_ok() {
            for path in loads {
                let resolved = if path.is_absolute() {
                    path
                } else {
                    self.state_dir.join(path)
                };
                // Canonicalized so a relative and an absolute
                // spelling of the same file dedup to one replay
                // entry (tulisp canonicalizes its load path the
                // same way).
                match resolved.canonicalize() {
                    Ok(canonical) => self.record_loaded_file(canonical),
                    Err(_) => {
                        // (load) resolved it some other way or the
                        // file moved mid-eval; reload just won't
                        // replay it.
                        log::warn!(
                            "eval loaded {} but the path does not resolve under {}; \
                             it will not survive a reload",
                            resolved.display(),
                            self.state_dir.display()
                        );
                    }
                }
            }
            self.persist_changed(ctx, src, &before);
        }
        // Bump the version on the microgrid the eval actually
        // mutated (the one current_microgrid points at, or — if no
        // scope was set — the router's fallback) so the WS event
        // pump fires TopologyChanged on the right bus. Without this
        // the bootstrap site's version moved, but UI sessions only
        // listen to per-mg buses.
        self.router.site().bump_version();
        result
    }

    /// Read-only eval — same machinery as `eval` but the result is
    /// NOT appended to the override file and the site version does
    /// NOT bump. For UI introspection (e.g. "what's the current
    /// value of battery-defaults?") that shouldn't surface as a
    /// persisted edit.
    pub fn eval_silent(&self, src: &str) -> Result<String, String> {
        let mut ctx = self.ctx.borrow_mut();
        match ctx.eval_string(src) {
            Ok(v) => Ok(v.to_string()),
            Err(e) => Err(e.format(&ctx)),
        }
    }

    /// Every registered microgrid's structural version, snapshotted
    /// before an eval so the save pass can tell which microgrids the
    /// eval actually moved. Pokes (power, health, modes) don't touch
    /// the structural version, so they cost no file write.
    fn structural_versions(&self) -> Vec<(u64, u64)> {
        self.microgrids
            .lock()
            .iter()
            .map(|(id, e)| (*id, e.site.structural_version()))
            .collect()
    }

    /// Save what an eval changed: every microgrid whose structure
    /// moved since `before`, plus `enterprise.lisp` when the source
    /// carries an enterprise-wide edit.
    ///
    /// Suppressed while a file is loading — a load must not rewrite
    /// the file it is reading (the file's own forms are exactly what
    /// moved the structure, so every load would otherwise rewrite
    /// itself, and a reload would rewrite everything).
    fn persist_changed(&self, ctx: &mut tulisp::TulispContext, src: &str, before: &[(u64, u64)]) {
        if self.loading.lock().is_some() {
            return;
        }
        let changed: Vec<(u64, bool, bool)> = {
            let reg = self.microgrids.lock();
            reg.iter()
                .filter(|(id, e)| {
                    let was = before.iter().find(|(bid, _)| bid == *id).map(|(_, v)| *v);
                    // A microgrid registered by this eval has no
                    // `before` entry — that counts as changed.
                    was != Some(e.site.structural_version())
                })
                .map(|(id, e)| (*id, e.managed, e.source.is_some()))
                .collect()
        };
        for (id, managed, has_source) in changed {
            if managed && has_source {
                // `persist` logs + banners its own failures.
                let _ = self.persist(id);
                continue;
            }
            if !has_source {
                log::warn!(
                    "microgrid {id} changed but is not backed by a file; \
                     the edit will not survive a restart"
                );
            }
            // Live state the file on disk doesn't carry: an unmanaged
            // file is the author's to edit (Adopt makes it managed),
            // and a REPL microgrid has no file at all.
            self.set_unsaved(id, true);
        }
        if (contains_defaults_setq(src) || enterprise_setter_in(src))
            && let Err(e) = self.persist_enterprise_locked(ctx)
        {
            let path = self.enterprise_path();
            log::error!("failed to save {}: {e}", path.display());
            self.router.site().broadcast_config_error(format!(
                "enterprise settings applied but could not be saved to {}: {e} — \
                 the edit will not survive a restart",
                path.display()
            ));
        }
    }

    /// Regenerate microgrid `id`'s managed file from its live state:
    /// the generated block is rewritten, the hand-written script
    /// section is copied through byte for byte. A no-op for a
    /// microgrid that is unmanaged or has no file.
    ///
    /// On failure the in-memory edit stands, the microgrid is flagged
    /// *unsaved*, and the UI gets a config-error banner — the write
    /// is retried by the next structural edit.
    pub fn persist(&self, id: u64) -> std::io::Result<()> {
        let Some((def, site, path)) = ({
            let reg = self.microgrids.lock();
            reg.get(&id).filter(|e| e.managed).and_then(|e| {
                let path = e.source.clone()?;
                Some((e.def.clone(), e.site.clone(), path))
            })
        }) else {
            return Ok(());
        };
        let block = microgrid_file::render_block(&def, &site);
        match self.write_two_section(&path, &block, microgrid_file::FRESH_SCRIPT_HEADER) {
            Ok(()) => {
                self.set_unsaved(id, false);
                Ok(())
            }
            Err(e) => {
                log::error!("failed to save microgrid {id} to {}: {e}", path.display());
                site.broadcast_config_error(format!(
                    "microgrid {id} could not be saved to {}: {e} — the edit is \
                     live but will not survive a restart",
                    path.display()
                ));
                self.set_unsaved(id, true);
                Err(e)
            }
        }
    }

    /// Regenerate `enterprise.lisp` from live enterprise-wide state.
    pub fn persist_enterprise(&self) -> std::io::Result<()> {
        let mut ctx = self.ctx.borrow_mut();
        self.persist_enterprise_locked(&mut ctx)
    }

    /// [`persist_enterprise`](Self::persist_enterprise) against an
    /// already-held interpreter guard — reading the `*-defaults`
    /// plists needs the interpreter the caller already holds.
    pub(super) fn persist_enterprise_locked(
        &self,
        ctx: &mut tulisp::TulispContext,
    ) -> std::io::Result<()> {
        let block = self.render_enterprise_block(ctx);
        self.write_two_section(
            &self.enterprise_path(),
            &block,
            microgrid_file::FRESH_ENTERPRISE_SCRIPT_HEADER,
        )
    }

    /// The generated block of `enterprise.lisp`: the enterprise
    /// metadata setters in a fixed order, then every bound
    /// `*-defaults` plist. Fixed order so two saves of the same state
    /// produce the same bytes (the watcher's self-write check and
    /// `git diff` both care).
    fn render_enterprise_block(&self, ctx: &mut tulisp::TulispContext) -> String {
        use crate::lisp::escape_lisp_string as esc;
        use std::fmt::Write as _;

        let md = self.metadata();
        let mut out = String::new();
        writeln!(out, "(set-enterprise-id {})", md.enterprise_id).unwrap();
        writeln!(out, "(set-timezone \"{}\")", esc(self.tz_name())).unwrap();
        writeln!(
            out,
            "(set-default-request-lifetime-ms {})",
            md.default_request_lifetime.as_millis()
        )
        .unwrap();
        writeln!(
            out,
            "(set-assets-socket-addr \"{}\")",
            esc(&md.assets_socket_addr)
        )
        .unwrap();
        writeln!(
            out,
            "(set-dispatch-socket-addr \"{}\")",
            esc(&md.dispatch_socket_addr)
        )
        .unwrap();
        for cat in DEFAULT_CATEGORIES {
            let var = format!("{cat}-defaults");
            // The guard keeps an unbound (or makunbound) category
            // from erroring the whole render.
            let Ok(value) = ctx.eval_string(&format!("(and (boundp '{var}) {var})")) else {
                continue;
            };
            if value.null() {
                continue;
            }
            let text = value.to_string();
            // format_with_width returns the source unchanged on
            // failure; either way the text re-reads as the same
            // value. Continuation lines are indented to sit under
            // the quote.
            let pretty = tulisp_fmt::format_with_width(&text, 72).unwrap_or_else(|_| text.clone());
            let body = pretty.trim_end().replace('\n', "\n       ");
            writeln!(out, "(setq {var}\n      '{body})").unwrap();
        }
        out
    }

    /// Write a two-section file: `block` between the markers, the
    /// existing file's script section (or `fresh_script` when the
    /// file is new) after them. Atomic, and the written bytes are
    /// recorded so the watcher can recognise our own save.
    ///
    /// Refuses a file that lost its markers — composing over it would
    /// bury the whole hand-written text under a generated block.
    fn write_two_section(
        &self,
        path: &Path,
        block: &str,
        fresh_script: &str,
    ) -> std::io::Result<()> {
        let script = match fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => fresh_script.to_string(),
            Ok(text) => {
                let parsed = microgrid_file::parse(&text).map_err(std::io::Error::other)?;
                match parsed.generated {
                    Some(_) => parsed.script,
                    None => {
                        return Err(std::io::Error::other(format!(
                            "{} carries no switchyard-generated block",
                            path.display()
                        )));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fresh_script.to_string(),
            Err(e) => return Err(e),
        };
        let text = microgrid_file::compose(block, &script);
        microgrid_file::write_atomic(path, &text)?;
        self.record_self_write(path, &text);
        Ok(())
    }

    /// Flag (or clear) "live edits this microgrid's file doesn't
    /// carry" — the UI renders it as an unsaved marker on the
    /// microgrid card.
    fn set_unsaved(&self, id: u64, unsaved: bool) {
        if let Some(entry) = self.microgrids.lock().get_mut(&id) {
            entry.unsaved = unsaved;
        }
    }

    /// One entry per top-level form in the per-microgrid override
    /// file (`config.<microgrid-id>.overrides.lisp`). Returns an
    /// empty vec if the file is missing or malformed — load-overrides
    /// will surface a parse error on the next reload, so we don't
    /// bother propagating it here.
    ///
    /// Parsed with tulisp-fmt's Cst parser rather than the
    /// interpreter: this runs on every overrides-pill refresh, and
    /// doing the file read + parse under the interpreter lock stalled
    /// concurrent evals (and the lisp refresh loop) on disk I/O. As a
    /// bonus the Cst keeps the user's original spelling, so the
    /// dialog shows the form as written instead of Display-normalized.
    pub fn persisted_overrides(&self) -> Vec<PersistedOverride> {
        let Some(path) = self.overrides_path() else {
            return Vec::new();
        };
        Self::persisted_overrides_from(&path)
    }

    /// [`persisted_overrides`](Self::persisted_overrides) for an
    /// explicit microgrid id. Lock-free like the ambient variant: the
    /// per-mg file path is a pure function of the id, so no scope
    /// pointer (and no interpreter lock) is involved.
    pub fn persisted_overrides_for(&self, mg_id: u64) -> Vec<PersistedOverride> {
        Self::persisted_overrides_from(&self.overrides_path_for(mg_id))
    }

    /// [`persisted_overrides`](Self::persisted_overrides) against an
    /// already-resolved path, so callers that must resolve the
    /// ambient scope exactly once (under the interpreter lock) can
    /// reuse their resolution.
    fn persisted_overrides_from(path: &Path) -> Vec<PersistedOverride> {
        use tulisp_fmt::cst::CstNode;
        let Ok(text) = fs::read_to_string(path) else {
            return Vec::new();
        };
        let Ok(cst) = tulisp_fmt::parse(&text) else {
            return Vec::new();
        };
        cst.nodes
            .iter()
            // Top-level expression forms only — trivia (comments,
            // blank lines) doesn't count toward the idx the delete
            // endpoints address forms by.
            .filter(|n| !matches!(n, CstNode::Comment { .. } | CstNode::LineBreak { .. }))
            .enumerate()
            .map(|(idx, n)| PersistedOverride {
                idx,
                source: text[n.span()].trim().to_string(),
            })
            .collect()
    }

    /// Drop a set of persisted-override entries (by their
    /// file-position idx) and re-derive MicrogridSite state. Atomic: the
    /// override file is rewritten without those forms (temp +
    /// rename, with a `tulisp-fmt` pretty-print pass over the
    /// surviving forms), then `reload()` re-runs config.lisp +
    /// `load-overrides` on the new file so the deleted forms'
    /// effects vanish via the MicrogridSite reset inside reload.
    ///
    /// Returns the count of forms actually dropped — out-of-range
    /// indices are silently ignored. An IO error during rewrite
    /// leaves the site state untouched (the file was renamed
    /// atomically only on success).
    ///
    /// Bulk shape so the UI's checkbox-toolbar can prune N entries
    /// in one round trip with one reload, instead of N round trips
    /// with N reloads.
    pub fn remove_persisted_overrides(&self, indices: &[usize]) -> std::io::Result<usize> {
        // One interpreter lock across resolve → read → rewrite →
        // reload, resolving the path exactly once. overrides_path()
        // follows the ambient microgrid scope, whose contract
        // requires this lock (see SiteRouter::with_microgrid) — an
        // unlocked call racing a scoped /api/mg/{id}/eval could
        // resolve one microgrid's file for the read and ANOTHER's
        // for the rename, silently overwriting that file's
        // persisted edits.
        let mut ctx = self.ctx.borrow_mut();
        self.remove_persisted_overrides_locked(&mut ctx, indices)
    }

    /// [`remove_persisted_overrides`](Self::remove_persisted_overrides)
    /// against an explicit microgrid id — the per-mg delete routes
    /// resolve their target through the scoped-eval machinery instead
    /// of whatever the ambient pointer happens to hold.
    pub fn remove_persisted_overrides_for(
        &self,
        mg_id: u64,
        indices: &[usize],
    ) -> std::io::Result<usize> {
        self.scoped(mg_id, |cfg, ctx| {
            cfg.remove_persisted_overrides_locked(ctx, indices)
        })
    }

    fn remove_persisted_overrides_locked(
        &self,
        ctx: &mut tulisp::TulispContext,
        indices: &[usize],
    ) -> std::io::Result<usize> {
        let Some(path) = self.overrides_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no resolvable microgrid scope; can't rewrite overrides",
            ));
        };
        let drop: HashSet<usize> = indices.iter().copied().collect();
        let entries = Self::persisted_overrides_from(&path);
        let kept: Vec<String> = entries
            .iter()
            .filter(|o| !drop.contains(&o.idx))
            .map(|o| o.source.clone())
            .collect();
        let dropped = entries.len() - kept.len();
        if dropped == 0 {
            return Ok(0);
        }
        let tmp = path.with_extension("lisp.tmp");
        {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            writeln!(file, ";; ── {} ──", Utc::now().to_rfc3339())?;
            writeln!(file)?;
            // Hand each surviving form to tulisp-fmt so the file
            // stays readable. format_with_width returns the same
            // source on failure; we fall back to the raw text
            // rather than dropping a form. Blank line between
            // forms keeps multi-line `let*` paste shapes visually
            // separable.
            for src in &kept {
                let fmt =
                    tulisp_fmt::format_with_width(src, 80).unwrap_or_else(|_| format!("{src}\n"));
                file.write_all(fmt.as_bytes())?;
                writeln!(file)?;
            }
            file.flush()?;
        }
        fs::rename(&tmp, &path)?;
        // A reload error after a successful rewrite leaves the file
        // on disk and the site reset to empty — the next save
        // (or a manual `reload`) is the recovery path. Surface the
        // error as IO so the HTTP handler can return 5xx; the
        // user's already lost the broken forms either way.
        if let Err(msg) = self.reload_locked(ctx) {
            return Err(std::io::Error::other(format!(
                "reload after rewrite failed: {msg}"
            )));
        }
        Ok(dropped)
    }

    /// Read the raw text of the active microgrid's overrides file.
    /// Empty string when the file doesn't exist yet (no edits have
    /// been persisted) or the scope can't resolve. Used by the
    /// canvas-undo handler to snapshot state before each mutation.
    pub fn overrides_text(&self) -> std::io::Result<String> {
        let Some(path) = self.overrides_path() else {
            return Ok(String::new());
        };
        match fs::read_to_string(&path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    /// Replace the overrides file with `content` and reload. The
    /// canvas-undo handler restores a snapshot of the file taken
    /// before a mutation; redo replays the snapshot taken after.
    /// Atomic rewrite (temp + rename) so an interruption mid-write
    /// can't corrupt the file.
    pub fn replace_overrides_text(&self, content: &str) -> std::io::Result<()> {
        let mut ctx = self.ctx.borrow_mut();
        self.replace_overrides_text_locked(&mut ctx, content)
    }

    /// `replace_overrides_text` body against an already-held
    /// interpreter guard — the scoped per-mg HTTP handler holds the
    /// lock across the scope flip, and the reload at the tail must
    /// reuse it rather than re-borrow.
    pub(crate) fn replace_overrides_text_locked(
        &self,
        ctx: &mut tulisp::TulispContext,
        content: &str,
    ) -> std::io::Result<()> {
        let Some(path) = self.overrides_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no resolvable microgrid scope; can't rewrite overrides",
            ));
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("lisp.tmp");
        {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.flush()?;
        }
        fs::rename(&tmp, &path)?;
        if let Err(msg) = self.reload_locked(ctx) {
            return Err(std::io::Error::other(format!(
                "reload after override-text replace failed: {msg}"
            )));
        }
        Ok(())
    }

    /// Resolve the per-microgrid overrides file path. Keyed off the
    /// active microgrid id (set by /api/mg/{id}/eval and the
    /// scenarios per-mg replay), falling back to the first registry
    /// entry when nothing's selected.
    ///
    /// Returns `None` when neither source resolves — current is
    /// `None` AND the registry is empty. The boot path can't reach
    /// that case (`Config::new` rejects an empty registry), but
    /// guarding against it here keeps a future `(reset-microgrid)`-
    /// then-eval flow from writing to a meaningless
    /// `config.0.overrides.lisp`.
    pub(super) fn overrides_path(&self) -> Option<PathBuf> {
        let mg_id = self
            .current_microgrid
            .read()
            .or_else(|| self.microgrids.lock().keys().next().copied())?;
        Some(self.overrides_path_for(mg_id))
    }

    /// [`overrides_path`](Self::overrides_path) for an explicit
    /// microgrid id — a pure function of the id, no ambient scope.
    pub(super) fn overrides_path_for(&self, mg_id: u64) -> PathBuf {
        self.state_dir
            .join("microgrids")
            .join(format!("config.{mg_id}.overrides.lisp"))
    }
}

/// Top-level `(load "…")` targets in `src`, plus whether any OTHER
/// top-level expression form rides along. Only string-literal
/// arguments count — a computed path `(load (concat …))` can't be
/// resolved statically and simply isn't recorded for reload.
fn top_level_load_paths(src: &str) -> (Vec<PathBuf>, bool) {
    use tulisp_fmt::cst::CstNode;
    let Ok(cst) = tulisp_fmt::parse(src) else {
        return (Vec::new(), false);
    };
    let mut loads = Vec::new();
    let mut other = false;
    for n in &cst.nodes {
        match n {
            CstNode::Comment { .. } | CstNode::LineBreak { .. } => {}
            CstNode::List { children, .. } => {
                let mut atoms = children.iter().filter_map(|c| match c {
                    CstNode::Atom { text, .. } => Some(text.as_str()),
                    _ => None,
                });
                let is_load = atoms.next() == Some("load");
                let arg = atoms.next();
                match (is_load, arg) {
                    (true, Some(s)) if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 => {
                        // Lisp string literal → path. Escapes other
                        // than \\ and \" don't appear in file paths
                        // in practice; unescape just those two.
                        let unescaped = s[1..s.len() - 1]
                            .replace("\\\\", "\\")
                            .replace("\\\"", "\"");
                        loads.push(PathBuf::from(unescaped));
                    }
                    (true, _) => other = true,
                    (false, _) => other = true,
                }
            }
            _ => other = true,
        }
    }
    (loads, other)
}

/// Enterprise-wide setters: a call to one of these changes state
/// that lives in `enterprise.lisp`, not in any microgrid file.
const ENTERPRISE_SETTERS: &[&str] = &[
    "set-enterprise-id",
    "set-timezone",
    "set-default-request-lifetime-ms",
    "set-assets-socket-addr",
    "set-dispatch-socket-addr",
];

/// Does `src` call one of the [`ENTERPRISE_SETTERS`] at top level?
/// None of them touches a microgrid's structure, so the structural
/// check alone would never save them.
fn enterprise_setter_in(src: &str) -> bool {
    use tulisp_fmt::cst::CstNode;
    let Ok(cst) = tulisp_fmt::parse(src) else {
        return false;
    };
    cst.nodes.iter().any(|n| {
        let CstNode::List { children, .. } = n else {
            return false;
        };
        children
            .iter()
            .find_map(|c| match c {
                CstNode::Atom { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .is_some_and(|head| ENTERPRISE_SETTERS.contains(&head))
    })
}

/// Does `src` contain a top-level defaults edit — `(setq <sym>-defaults …)`?
/// The Defaults panel saves through eval; those shape FUTURE `make-*`
/// calls rather than the live graph, so the structural check alone
/// would drop them — they belong in `enterprise.lisp`.
fn contains_defaults_setq(src: &str) -> bool {
    use tulisp_fmt::cst::CstNode;
    let Ok(cst) = tulisp_fmt::parse(src) else {
        return false;
    };
    cst.nodes.iter().any(|n| {
        let CstNode::List { children, .. } = n else {
            return false;
        };
        let mut atoms = children.iter().filter_map(|c| match c {
            CstNode::Atom { text, .. } => Some(text.as_str()),
            _ => None,
        });
        atoms.next() == Some("setq") && atoms.next().is_some_and(|s| s.ends_with("-defaults"))
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::config_with;

    /// A `(load "file")` eval records the file for reload replay
    /// instead of journaling the form: the file itself is the
    /// persistent artifact.
    #[test]
    fn load_evals_are_recorded_not_journaled() {
        let (cfg, dir) = config_with("nil");
        let extra = dir.join("extra-mg.lisp");
        std::fs::write(
            &extra,
            "(make-microgrid :id 55 :grpc-port 8855 :topology (lambda () nil))",
        )
        .unwrap();
        cfg.eval("(load \"extra-mg.lisp\")").unwrap();
        assert!(cfg.microgrids().lock().contains_key(&55));
        // No overrides journal picked up the (load …) form.
        let mg_dir = dir.join("microgrids");
        if let Ok(entries) = std::fs::read_dir(&mg_dir) {
            for e in entries.flatten() {
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                assert!(
                    !text.contains("extra-mg"),
                    "(load …) form must not be journaled; found in {}",
                    e.path().display()
                );
            }
        }
        // The recorded file replays on reload.
        cfg.reload().expect("reload");
        assert!(cfg.microgrids().lock().contains_key(&55));
    }

    /// A pure-load eval goes through the loader, so the file joins
    /// the reload-replay list whether or not it moved the topology.
    /// A load is not an edit to be gated: the operator asked for that
    /// FILE, and a file that is part of the world stays part of it
    /// across a reload. (A purely imperative script's forms simply
    /// run again on reload — same as re-typing them.)
    #[test]
    fn pure_loads_are_recorded_whatever_they_registered() {
        let (cfg, dir) = config_with("nil");
        let script = dir.join("poke.lisp");
        std::fs::write(&script, "(setq some-transient-var 42)").unwrap();
        cfg.eval("(load \"poke.lisp\")").unwrap();
        assert_eq!(cfg.eval_silent("some-transient-var").unwrap(), "42");
        assert!(
            cfg.loaded_files
                .lock()
                .iter()
                .any(|p| p.ends_with("poke.lisp")),
            "a loaded file joins the reload-replay list"
        );
    }

    /// A `(make-microgrid …)` typed into the REPL has no backing
    /// file, so journaling it into the ambient microgrid's overrides
    /// would poison every later reload: the owning file's
    /// `(load-overrides)` would replay the form under that file's
    /// name, the source-less entry would refuse the foreign claim,
    /// and the reload would abort with every site already reset —
    /// leaving the world empty. The unbacked microgrid is transient;
    /// the FILE-backed world must survive.
    #[test]
    fn repl_created_microgrid_does_not_break_reload() {
        // A config shaped like the real ones: its topology replays
        // its own overrides journal. That replay is what turns a
        // journaled REPL microgrid into a reload-breaking form.
        let (cfg, _dir) = config_with(
            "(make-microgrid :id 9 :grpc-port 8800
               :topology (lambda ()
                           (%make-grid-connection-point :id 1)
                           (load-overrides)))",
        );
        cfg.eval(
            "(make-microgrid :id 10 :grpc-port 8899 :topology (lambda () (%make-meter :id 2)))",
        )
        .unwrap();
        // The boot script's world comes back intact.
        cfg.reload()
            .expect("reload must survive a REPL-created microgrid");
        let reg = cfg.microgrids();
        let r = reg.lock();
        assert!(
            r.get(&9).unwrap().site.get(1).is_some(),
            "the file-backed microgrid's topology must be rebuilt",
        );
    }

    /// A structural eval rewrites the managed file it belongs to,
    /// keeping the hand-written script section; a transient poke
    /// rewrites nothing, and an unmanaged microgrid is never written
    /// to at all.
    #[test]
    fn structural_evals_regenerate_the_managed_file() {
        let (cfg, dir) = config_with(
            "(make-microgrid :id 9 :grpc-port 8800 :topology \
                                  (lambda () (%make-grid-connection-point :id 1)))",
        );
        // config.lisp is unmanaged → a structural eval flags, but writes nothing.
        cfg.eval("(rename-component 1 \"a\")").unwrap();
        assert!(!dir.join("microgrids").exists());

        // A managed microgrid: created like the UI does it.
        let def = crate::sim::microgrids::MicrogridDef {
            id: 20,
            name: "m".into(),
            grpc_port: 8890,
            tso: None,
        };
        let path = dir.join("microgrids/20.lisp");
        crate::lisp::microgrid_file::write_atomic(
            &path,
            &crate::lisp::microgrid_file::compose(
                &crate::lisp::microgrid_file::render_empty_block(&def),
                crate::lisp::microgrid_file::FRESH_SCRIPT_HEADER,
            ),
        )
        .unwrap();
        cfg.load_file(&path).unwrap();
        cfg.eval_in_mg(20, "(%make-meter :id 100 :power 500.0)")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("(%make-meter :id 100"), "{text}");
        assert!(text.contains(":power 500.0"), "{text}");
        assert!(
            text.contains("Anything below is yours"),
            "script section preserved"
        );
        // A poke does not rewrite the file.
        let before = std::fs::read_to_string(&path).unwrap();
        cfg.eval_in_mg(20, "(set-meter-power 100 4321.0)").unwrap();
        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }

    /// Enterprise-wide state — the `*-defaults` plists and the
    /// metadata setters — regenerates `enterprise.lisp`, never a
    /// microgrid file.
    #[test]
    fn defaults_edits_regenerate_enterprise_lisp() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        cfg.eval("(setq battery-defaults '(:capacity 1000.0))")
            .unwrap();
        let text = std::fs::read_to_string(dir.join("enterprise.lisp")).unwrap();
        assert!(text.contains("battery-defaults"), "{text}");
        assert!(text.contains(":capacity 1000.0"), "{text}");
        cfg.eval("(set-enterprise-id 77)").unwrap();
        let text = std::fs::read_to_string(dir.join("enterprise.lisp")).unwrap();
        assert!(text.contains("(set-enterprise-id 77)"), "{text}");
    }

    /// Concurrent per-mg evals must not cross microgrids: the scope
    /// pointer only flips under the interpreter lock, so each eval's
    /// mutations AND the file it regenerates belong to its own
    /// microgrid. Pre-fix, scope-set happened before the lock and the
    /// persistence after release — two racing `/api/mg/{id}/eval`
    /// calls could write into each other's files.
    #[test]
    fn concurrent_scoped_evals_do_not_cross_microgrids() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let managed = |id: u64, port: u16, component: u64| {
            let def = crate::sim::microgrids::MicrogridDef {
                id,
                name: format!("m{id}"),
                grpc_port: port,
                tso: None,
            };
            let path = dir.join(format!("microgrids/{id}.lisp"));
            let block = crate::lisp::microgrid_file::render_empty_block(&def).replace(
                "    nil",
                &format!("    (%make-grid-connection-point :id {component})"),
            );
            crate::lisp::microgrid_file::write_atomic(
                &path,
                &crate::lisp::microgrid_file::compose(
                    &block,
                    crate::lisp::microgrid_file::FRESH_SCRIPT_HEADER,
                ),
            )
            .unwrap();
            cfg.load_file(&path).unwrap();
            path
        };
        let nine_path = managed(21, 8891, 1);
        let ten_path = managed(22, 8892, 2);

        std::thread::scope(|s| {
            let a = cfg.clone();
            let b = cfg.clone();
            s.spawn(move || {
                for i in 0..40 {
                    a.eval_in_mg(21, &format!("(rename-component 1 \"a{i}\")"))
                        .unwrap();
                }
            });
            s.spawn(move || {
                for i in 0..40 {
                    b.eval_in_mg(22, &format!("(rename-component 2 \"b{i}\")"))
                        .unwrap();
                }
            });
        });

        let nine = std::fs::read_to_string(&nine_path).unwrap();
        let ten = std::fs::read_to_string(&ten_path).unwrap();
        assert!(
            !nine.contains(":id 2 ") && nine.contains("\"a39\""),
            "mg 21's file must hold only mg 21's state:\n{nine}"
        );
        assert!(
            !ten.contains(":id 1 ") && ten.contains("\"b39\""),
            "mg 22's file must hold only mg 22's state:\n{ten}"
        );
        // The mutations landed on the right sites too.
        let reg = cfg.microgrids();
        let r = reg.lock();
        assert_eq!(r[&21].site.display_name(1).as_deref(), Some("a39"));
        assert_eq!(r[&22].site.display_name(2).as_deref(), Some("b39"));
    }
}
