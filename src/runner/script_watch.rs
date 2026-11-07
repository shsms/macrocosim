//! What the supervisor watches: every `.py` file in the script's
//! directory tree, skipping the directories no script edit lives in
//! and the engine's own output.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::watch::{Mode, Watch};

/// Saves closer together than this are one change.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// Directories never watched, besides hidden ones.
const SKIPPED: &[&str] = &[
    "__pycache__",
    "site-packages",
    ".venv",
    "venv",
    "target",
    "node_modules",
];

/// Whether a directory below the script's is watched.
pub fn watched_dir(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    !name.starts_with('.') && !SKIPPED.contains(&name)
}

/// Whether a path is a `.py` file, the only kind a reload follows.
pub fn is_python(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "py")
}

/// The directories of the engine's state dir that a watch of `dir`
/// skips: its `recordings/`, where the CSV files of a recording are
/// written as it runs, and the state dir itself when it lies below
/// `dir`.
pub fn state_dirs(dir: &Path, state_dir: &Path) -> Vec<PathBuf> {
    let state_dir = state_dir
        .canonicalize()
        .unwrap_or_else(|_| state_dir.to_path_buf());
    let mut skipped = vec![state_dir.join("recordings")];
    if state_dir != dir && state_dir.starts_with(dir) {
        skipped.push(state_dir);
    }
    skipped
}

/// The watch on `dir`, the script's directory, skipping `skipped`.
pub fn script_tree(dir: PathBuf, skipped: Vec<PathBuf>) -> Watch {
    Watch {
        mode: Mode::Tree {
            root: dir,
            relevant: Box::new(is_python),
        },
        filter: Box::new(move |d| watched_dir(d) && !skipped.iter().any(|s| s == d)),
        debounce: DEBOUNCE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_and_tool_directories_are_skipped() {
        for skipped in [
            ".git",
            ".venv",
            "venv",
            "__pycache__",
            "site-packages",
            "target",
            "node_modules",
        ] {
            assert!(!watched_dir(&Path::new("/s").join(skipped)), "{skipped}");
        }
        assert!(watched_dir(Path::new("/s/helpers")));
        assert!(is_python(Path::new("/s/plant.py")));
        assert!(!is_python(Path::new("/s/plant.pyc")));
        assert!(!is_python(Path::new("/s/notes.txt")));
    }

    #[test]
    fn the_recordings_and_a_state_dir_below_the_script_are_skipped() {
        let s = Path::new("/s");
        assert_eq!(
            state_dirs(s, Path::new("/s")),
            [PathBuf::from("/s/recordings")]
        );
        assert_eq!(
            state_dirs(s, Path::new("/s/state")),
            [
                PathBuf::from("/s/state/recordings"),
                PathBuf::from("/s/state")
            ]
        );
        assert_eq!(
            state_dirs(s, Path::new("/elsewhere")),
            [PathBuf::from("/elsewhere/recordings")]
        );
    }
}
