//! The script runner: `macrocosim plant.py` runs the script in one
//! child Python process (`python -P -m macrocosim._runner`), which
//! sends the script's microgrids to the engine in a hello and then
//! drives them over HTTP, as a test does. The engine is the parent:
//! the [`ScriptSupervisor`] starts the child, starts a new one when a
//! `.py` file in the script's tree changes, and puts the new child's
//! microgrids in place.
//!
//! `python` finds and checks the interpreter, which the export of a
//! microgrid as a `.py` module uses too.

pub mod child;
pub mod hello;
pub mod python;
pub mod script_watch;
pub mod supervisor;

pub use supervisor::{ScriptStatus, ScriptSupervisor};
