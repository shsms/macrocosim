//! Per-microgrid undo / redo over managed microgrid files.
//!
//! A step is one *generated block* — the text between the markers of
//! a managed file. Every structural edit regenerates that block
//! ([`Config::persist`]), and just before it does, the block the file
//! currently carries is pushed onto that microgrid's undo stack. Undo
//! pops the newest step, composes it with whatever the file's script
//! section says now, writes it back, and reloads just that file; the
//! block it displaced goes onto the redo stack. Redo is the mirror.
//!
//! Only the structure is versioned. The hand-written script section
//! is the author's and is carried through untouched, so an undo never
//! reverts a line a person typed.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use super::Config;
use super::microgrid_file;

/// How many undo steps one microgrid keeps. Older steps fall off the
/// back — the stack is a convenience for taking back a few clicks,
/// not a history store (that is what snapshots and git are for).
const UNDO_CAP: usize = 20;

/// One microgrid's edit history: generated blocks, newest last.
#[derive(Default)]
pub struct UndoHistory {
    /// Blocks the file used to carry, oldest first. `undo` pops the
    /// back.
    undo: VecDeque<String>,
    /// Blocks undone and not yet re-applied, newest last. Cleared by
    /// any fresh edit — a new branch of history drops the old one.
    redo: Vec<String>,
}

/// The per-microgrid histories, shared like the rest of `Config`.
pub type SharedUndo = Arc<Mutex<HashMap<u64, UndoHistory>>>;

pub fn new_undo_histories() -> SharedUndo {
    Arc::new(Mutex::new(HashMap::new()))
}

/// How deep each stack is — what `GET /api/mg/{id}/undo` reports and
/// what the mutating endpoints echo back so the UI can grey out its
/// buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndoDepths {
    pub undo: usize,
    pub redo: usize,
}

impl Config {
    /// Stack `block` — the generated block a rewrite just replaced —
    /// as microgrid `id`'s newest undo step, and drop the redo stack:
    /// a fresh edit starts a new branch of history.
    ///
    /// The invariant every rewrite of a generated block keeps: the
    /// block it replaced is what one press of Undo puts back. Call it
    /// AFTER the write succeeds, or a failed write leaves a step that
    /// undoes to a state the file never had.
    pub(super) fn push_undo_block(&self, id: u64, block: String) {
        let mut histories = self.undo.lock();
        let history = histories.entry(id).or_default();
        history.undo.push_back(block);
        while history.undo.len() > UNDO_CAP {
            history.undo.pop_front();
        }
        history.redo.clear();
    }

    /// Stack depths for microgrid `id`, both zero when it has no
    /// history yet.
    pub fn undo_depths(&self, id: u64) -> UndoDepths {
        let histories = self.undo.lock();
        match histories.get(&id) {
            Some(h) => UndoDepths {
                undo: h.undo.len(),
                redo: h.redo.len(),
            },
            None => UndoDepths { undo: 0, redo: 0 },
        }
    }

    /// Step microgrid `id` one structural edit back. Errors when the
    /// microgrid is unmanaged, has no file, or has nothing to undo.
    pub fn undo(&self, id: u64) -> Result<UndoDepths, String> {
        self.step(id, Direction::Undo)
    }

    /// Step microgrid `id` one structural edit forward again.
    pub fn redo(&self, id: u64) -> Result<UndoDepths, String> {
        self.step(id, Direction::Redo)
    }

    /// The shared body of undo and redo: pop a block off one stack,
    /// write it into the file, reload that file, and push the block
    /// it displaced onto the other stack.
    ///
    /// Everything after the first line runs under the interpreter
    /// lock, because a step is a read-modify-write of the file: it
    /// composes the popped block with the script section the file
    /// carries RIGHT NOW. A `/api/mg/{id}/eval` persisting in between
    /// would otherwise be overwritten with no trace, and the redo
    /// stack would hold a block that no longer follows from the
    /// file. Taking the interpreter lock first is also the order
    /// `persist` takes (interpreter, then history), so the two can
    /// never deadlock against each other.
    fn step(&self, id: u64, dir: Direction) -> Result<UndoDepths, String> {
        let path = self.managed_source(id)?;
        let mut ctx = self.ctx.borrow_mut();
        let Some(block) = self.pop(id, dir) else {
            return Err(format!("microgrid {id} has nothing to {dir}"));
        };
        match self.apply_block_locked(&mut ctx, &path, &block) {
            Ok(displaced) => {
                self.push(id, dir.opposite(), displaced);
                Ok(self.undo_depths(id))
            }
            // The write never happened, so the step is still ahead of
            // us: put it back where it came from.
            Err(StepFailure::NotWritten(e)) => {
                self.push(id, dir, block);
                Err(e)
            }
            // The file DOES carry the popped block now — only
            // re-evaluating it failed (typically the script section
            // driving a component the older block doesn't declare).
            // So the step was taken, and the block it displaced goes
            // on the opposite stack: the newer structure stays one
            // press of Redo away. Pushing the popped block back
            // instead would drop `displaced` on the floor, leaving
            // the file and the history disagreeing and every further
            // press repeating the same failure.
            Err(StepFailure::ReloadFailed { error, displaced }) => {
                self.push(id, dir.opposite(), displaced);
                Err(error)
            }
        }
    }

    /// Write `block` into `path` as its generated block, keeping the
    /// file's script section, and reload just that file. Returns the
    /// block that was displaced, for the opposite stack.
    ///
    /// Takes the interpreter guard its caller already holds — see
    /// [`Config::step`] for why the whole read-modify-write sits
    /// inside it.
    fn apply_block_locked(
        &self,
        ctx: &mut tulisp::TulispContext,
        path: &Path,
        block: &str,
    ) -> Result<String, StepFailure> {
        // Everything down to the write leaves the file as it was.
        let unwritten = StepFailure::NotWritten;
        let text = std::fs::read_to_string(path)
            .map_err(|e| unwritten(format!("cannot read {}: {e}", path.display())))?;
        let parsed = microgrid_file::parse(&text)
            .map_err(|e| unwritten(format!("{}: {e}", path.display())))?;
        let displaced = parsed.generated.ok_or_else(|| {
            unwritten(format!(
                "{} carries no switchyard-generated block",
                path.display()
            ))
        })?;
        let composed = microgrid_file::compose(block, &parsed.script);
        microgrid_file::write_atomic(path, &composed)
            .map_err(|e| unwritten(format!("write {}: {e}", path.display())))?;
        // Our own write — the watcher must not bounce it back as an
        // edit and reload the file a second time.
        self.record_self_write(path, &composed);
        // Per file, never the whole world: undoing one microgrid's
        // edit must not rebuild every other microgrid.
        //
        // The file already carries `block`, so a failure from here on
        // is a step that HAPPENED: hand `displaced` back with the
        // error so the caller stacks it instead of un-popping.
        match self.reload_file_locked(ctx, path) {
            Ok(_) => Ok(displaced),
            Err(error) => Err(StepFailure::ReloadFailed { error, displaced }),
        }
    }

    /// The managed file backing microgrid `id`. Undo rewrites that
    /// file, so a microgrid without one (unmanaged, or typed into the
    /// REPL) has no undo at all.
    fn managed_source(&self, id: u64) -> Result<PathBuf, String> {
        let registry = self.microgrids.lock();
        let entry = registry
            .get(&id)
            .ok_or_else(|| format!("microgrid {id} not registered"))?;
        if !entry.managed {
            return Err(format!(
                "microgrid {id} is not managed by switchyard; adopt it first"
            ));
        }
        entry
            .source
            .clone()
            .ok_or_else(|| format!("microgrid {id} has no file to undo against"))
    }

    fn pop(&self, id: u64, dir: Direction) -> Option<String> {
        let mut histories = self.undo.lock();
        let history = histories.get_mut(&id)?;
        match dir {
            Direction::Undo => history.undo.pop_back(),
            Direction::Redo => history.redo.pop(),
        }
    }

    fn push(&self, id: u64, dir: Direction, block: String) {
        let mut histories = self.undo.lock();
        let history = histories.entry(id).or_default();
        match dir {
            Direction::Undo => {
                history.undo.push_back(block);
                while history.undo.len() > UNDO_CAP {
                    history.undo.pop_front();
                }
            }
            Direction::Redo => history.redo.push(block),
        }
    }
}

/// Why applying one step failed, split by whether the file was
/// already rewritten. The two need opposite recoveries, and getting
/// it wrong loses a block: an un-pop after a successful write drops
/// the displaced block, and stacking a displaced block that was
/// never displaced invents history.
enum StepFailure {
    /// The file still carries what it carried before, so the step is
    /// still ahead of us and belongs back on the stack it came off.
    NotWritten(String),
    /// The file now carries the popped block; only re-evaluating it
    /// failed. The step counts as taken, and `displaced` — the block
    /// the write replaced — belongs on the opposite stack.
    ReloadFailed { error: String, displaced: String },
}

/// Which stack a step comes off.
#[derive(Clone, Copy)]
enum Direction {
    Undo,
    Redo,
}

impl Direction {
    fn opposite(self) -> Self {
        match self {
            Direction::Undo => Direction::Redo,
            Direction::Redo => Direction::Undo,
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Undo => f.write_str("undo"),
            Direction::Redo => f.write_str("redo"),
        }
    }
}

/// The generated block of the file at `path`, or `None` when the
/// file is missing, unreadable, or not a managed file. What a caller
/// about to overwrite that block reads first, so it can stack the
/// block it is replacing.
pub(super) fn read_generated_block(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    microgrid_file::parse(&text).ok()?.generated
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::test_support::config_with;
    use crate::lisp::microgrid_file;

    /// Load a managed microgrid file with an empty topology, the way
    /// the create endpoint writes one, and return its path.
    fn managed(cfg: &crate::lisp::Config, dir: &std::path::Path, id: u64, port: u16) -> PathBuf {
        let def = crate::sim::microgrids::MicrogridDef {
            id,
            name: format!("m{id}"),
            grpc_port: port,
            tso: None,
        };
        let path = dir.join(format!("microgrids/{id}.lisp"));
        microgrid_file::write_atomic(
            &path,
            &microgrid_file::compose(
                &microgrid_file::render_empty_block(&def),
                microgrid_file::FRESH_SCRIPT_HEADER,
            ),
        )
        .unwrap();
        cfg.load_file(&path).unwrap();
        path
    }

    /// The generated block of `path` and the block microgrid `id`'s
    /// live state renders to. A quiescent managed microgrid has these
    /// equal: every path that writes the file writes what is live,
    /// and every path that rewrites the block reloads from it.
    fn file_and_live(
        cfg: &crate::lisp::Config,
        path: &std::path::Path,
        id: u64,
    ) -> (String, String) {
        let on_disk = super::read_generated_block(path).expect("managed file");
        let reg = cfg.microgrids();
        let r = reg.lock();
        let e = r.get(&id).unwrap();
        (on_disk, microgrid_file::render_block(&e.def, &e.site))
    }

    /// An undo step is a read-modify-write of the microgrid's file,
    /// so the WHOLE of it — read, compose, write, reload — runs
    /// under the interpreter lock, the same lock a structural eval
    /// holds while it persists. Proven by the file: while a thread
    /// holds that lock, undo must not have touched it yet.
    ///
    /// Taking the interpreter lock first and the history lock inside
    /// it is also the only order either path uses, which is what
    /// keeps undo and persist from deadlocking against each other.
    #[test]
    fn undo_does_not_touch_the_file_until_it_holds_the_interpreter() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let path = managed(&cfg, &dir, 25, 8895);
        cfg.eval_in_mg(25, "(%make-meter :id 400)").unwrap();
        let before = super::read_generated_block(&path).unwrap();

        let interpreter = cfg.interpreter();
        let guard = interpreter.borrow_mut();
        std::thread::scope(|s| {
            let cfg2 = cfg.clone();
            let stepper = s.spawn(move || cfg2.undo(25).expect("undo once the lock is free"));
            // Long enough that an undo doing its read-modify-write
            // outside the lock would have finished the write.
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert_eq!(
                super::read_generated_block(&path).unwrap(),
                before,
                "undo must not rewrite the file while an eval holds the interpreter"
            );
            drop(guard);
            stepper.join().unwrap();
        });
        assert_ne!(
            super::read_generated_block(&path).unwrap(),
            before,
            "the step lands once the lock is free"
        );
        let reg = cfg.microgrids();
        assert!(reg.lock()[&25].site.get(400).is_none(), "the step landed");
    }

    /// Undo and structural evals hammering one microgrid must leave
    /// the file carrying exactly what is live — and must not deadlock
    /// (a history-then-interpreter lock order would hang here).
    #[test]
    fn undo_and_eval_racing_leave_file_and_state_agreeing() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let path = managed(&cfg, &dir, 23, 8893);
        cfg.eval_in_mg(23, "(%make-grid-connection-point :id 1)")
            .unwrap();

        std::thread::scope(|s| {
            let a = cfg.clone();
            let b = cfg.clone();
            s.spawn(move || {
                for i in 0..30 {
                    a.eval_in_mg(23, &format!("(rename-component 1 \"a{i}\")"))
                        .unwrap();
                }
            });
            s.spawn(move || {
                for _ in 0..30 {
                    // Either direction may legitimately have nothing
                    // left to walk; only the file's consistency is
                    // under test.
                    let _ = b.undo(23);
                    let _ = b.redo(23);
                }
            });
        });

        let (on_disk, live) = file_and_live(&cfg, &path, 23);
        assert_eq!(on_disk.trim(), live.trim(), "file must carry live state");
    }

    /// An undo whose RELOAD fails still rewrote the file, so the step
    /// happened: the block it displaced must land on the redo stack,
    /// or the newer structure is gone for good — the file carries the
    /// older block, the history says the newer one is still current,
    /// and every further press of Undo replays the same failure.
    ///
    /// Reachable whenever the hand-written script section drives a
    /// component only the newer block declares, which is the ordinary
    /// shape of a managed file: add a meter, then drive it.
    #[test]
    fn a_failed_reload_leaves_the_displaced_block_on_the_redo_stack() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let path = managed(&cfg, &dir, 26, 8896);
        cfg.eval_in_mg(26, "(%make-meter :id 400)").unwrap();
        // A script section that only works while meter 400 exists.
        // Undoing back past the meter therefore fails its reload.
        let text = std::fs::read_to_string(&path).unwrap();
        let block = microgrid_file::parse(&text).unwrap().generated.unwrap();
        let composed = microgrid_file::compose(&block, "(set-meter-power 400 1000.0)\n");
        microgrid_file::write_atomic(&path, &composed).unwrap();
        cfg.record_self_write(&path, &composed);

        let err = cfg
            .undo(26)
            .expect_err("the script section fails to reload");
        assert!(err.contains("400"), "the reload error is reported: {err}");
        assert_eq!(
            cfg.undo_depths(26).redo,
            1,
            "the displaced block is still recoverable"
        );

        cfg.redo(26).expect("redo puts the newer block back");
        let reg = cfg.microgrids();
        let r = reg.lock();
        assert!(
            r[&26].site.get(400).is_some(),
            "and the world it describes works again"
        );
    }

    /// Restoring a snapshot rewrites the generated block, so it
    /// stacks the block it replaced like any other structural edit —
    /// Undo walks back OUT of a restore instead of stepping over it
    /// to some older state.
    #[test]
    fn a_snapshot_restore_is_undoable() {
        let (cfg, dir) =
            config_with("(make-microgrid :id 9 :grpc-port 8800 :topology (lambda () nil))");
        let path = managed(&cfg, &dir, 24, 8894);
        cfg.eval_in_mg(24, "(%make-meter :id 300)").unwrap();
        cfg.save_snapshot_for(24, "before").unwrap();
        cfg.eval_in_mg(24, "(%make-meter :id 301)").unwrap();
        let depth_before = cfg.undo_depths(24).undo;

        cfg.load_snapshot_for(24, "before", None).unwrap();
        {
            let reg = cfg.microgrids();
            let r = reg.lock();
            assert!(r[&24].site.get(301).is_none(), "restored to the snapshot");
        }
        assert_eq!(
            cfg.undo_depths(24).undo,
            depth_before + 1,
            "the restore stacked the block it replaced"
        );

        cfg.undo(24).expect("undo the restore");
        let reg = cfg.microgrids();
        let r = reg.lock();
        assert!(
            r[&24].site.get(301).is_some(),
            "undo walks back out of the restore"
        );
        drop(r);
        let (on_disk, live) = file_and_live(&cfg, &path, 24);
        assert_eq!(on_disk.trim(), live.trim());
    }
}
