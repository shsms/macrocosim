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
    /// Push the generated block `path` currently holds onto
    /// microgrid `id`'s undo stack and drop the redo stack. Called by
    /// [`Config::persist`] immediately before it overwrites the file.
    ///
    /// A file that is missing, unreadable or carries no generated
    /// block contributes no step — there is nothing to go back to.
    pub(super) fn push_undo_step(&self, id: u64, path: &Path) {
        let Some(block) = read_block(path) else {
            return;
        };
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
    /// The history lock is taken twice, in short bursts, rather than
    /// held across the write + reload: `persist` pushes a step while
    /// holding the interpreter lock, and the reload below takes that
    /// same interpreter lock — holding the history lock across it
    /// would invert the order and deadlock. A pop that then fails to
    /// write puts its block straight back.
    fn step(&self, id: u64, dir: Direction) -> Result<UndoDepths, String> {
        let path = self.managed_source(id)?;
        let Some(block) = self.pop(id, dir) else {
            return Err(format!("microgrid {id} has nothing to {dir}"));
        };
        match self.apply_block(&path, &block) {
            Ok(displaced) => {
                self.push(id, dir.opposite(), displaced);
                Ok(self.undo_depths(id))
            }
            Err(e) => {
                self.push(id, dir, block);
                Err(e)
            }
        }
    }

    /// Write `block` into `path` as its generated block, keeping the
    /// file's script section, and reload just that file. Returns the
    /// block that was displaced, for the opposite stack.
    fn apply_block(&self, path: &Path, block: &str) -> Result<String, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let parsed =
            microgrid_file::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let displaced = parsed
            .generated
            .ok_or_else(|| format!("{} carries no switchyard-generated block", path.display()))?;
        let composed = microgrid_file::compose(block, &parsed.script);
        microgrid_file::write_atomic(path, &composed)
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        // Our own write — the watcher must not bounce it back as an
        // edit and reload the file a second time.
        self.record_self_write(path, &composed);
        // Per file, never the whole world: undoing one microgrid's
        // edit must not rebuild every other microgrid.
        self.reload_file(path)?;
        Ok(displaced)
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
/// file is missing, unreadable, or not a managed file.
fn read_block(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    microgrid_file::parse(&text).ok()?.generated
}
