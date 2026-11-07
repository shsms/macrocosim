//! A per-microgrid undo / redo stack, generic over what one step
//! stores: a Lisp microgrid keeps generated blocks, a microgrid with a
//! stored spec keeps specs.

use std::collections::VecDeque;

/// How many undo steps one microgrid keeps. Older steps fall off the
/// back: the stack is a convenience for taking back a few edits, not
/// a history store.
pub const UNDO_CAP: usize = 20;

/// One microgrid's edit history, newest last.
#[derive(Clone, Debug)]
pub struct UndoHistory<T> {
    /// What the microgrid used to be, oldest first. `undo` pops the
    /// back.
    undo: VecDeque<T>,
    /// Steps undone and not yet re-applied, newest last. Cleared by
    /// any fresh edit: a new branch of history drops the old one.
    redo: Vec<T>,
}

impl<T> Default for UndoHistory<T> {
    fn default() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
        }
    }
}

/// Which stack a step comes off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Undo,
    Redo,
}

impl Direction {
    pub fn opposite(self) -> Self {
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

/// How deep each stack is: what `GET /api/mg/{id}/undo` reports, so
/// the UI can grey out its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UndoDepths {
    pub undo: usize,
    pub redo: usize,
}

impl<T> UndoHistory<T> {
    /// Stack `step`, what a fresh edit just replaced, as the newest
    /// undo step, and drop the redo stack.
    pub fn push_edit(&mut self, step: T) {
        self.push(Direction::Undo, step);
        self.redo.clear();
    }

    /// Take the newest step off the `dir` stack.
    pub fn pop(&mut self, dir: Direction) -> Option<T> {
        match dir {
            Direction::Undo => self.undo.pop_back(),
            Direction::Redo => self.redo.pop(),
        }
    }

    /// Put `step` on the `dir` stack, keeping the undo stack within
    /// [`UNDO_CAP`].
    pub fn push(&mut self, dir: Direction, step: T) {
        match dir {
            Direction::Undo => {
                self.undo.push_back(step);
                while self.undo.len() > UNDO_CAP {
                    self.undo.pop_front();
                }
            }
            Direction::Redo => self.redo.push(step),
        }
    }

    pub fn depths(&self) -> UndoDepths {
        UndoDepths {
            undo: self.undo.len(),
            redo: self.redo.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_edit_drops_the_redo_stack() {
        let mut h = UndoHistory::default();
        h.push_edit(1);
        h.push_edit(2);
        assert_eq!(h.pop(Direction::Undo), Some(2));
        h.push(Direction::Redo, 3);
        assert_eq!(h.depths(), UndoDepths { undo: 1, redo: 1 });
        h.push_edit(4);
        assert_eq!(h.depths(), UndoDepths { undo: 2, redo: 0 });
        assert_eq!(h.pop(Direction::Redo), None);
    }

    #[test]
    fn the_undo_stack_keeps_the_newest_steps() {
        let mut h = UndoHistory::default();
        for step in 0..UNDO_CAP + 5 {
            h.push_edit(step);
        }
        assert_eq!(h.depths().undo, UNDO_CAP);
        assert_eq!(h.pop(Direction::Undo), Some(UNDO_CAP + 4));
        let mut oldest = None;
        while let Some(step) = h.pop(Direction::Undo) {
            oldest = Some(step);
        }
        assert_eq!(oldest, Some(5));
    }
}
