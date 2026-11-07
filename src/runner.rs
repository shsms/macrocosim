//! The script runner: `macrocosim plant.py` runs the script in a
//! child Python process that sends its microgrids to the engine and
//! then drives them over HTTP. `python` finds and checks the
//! interpreter, which the export of a microgrid as a `.py` module
//! uses too.

pub mod python;
