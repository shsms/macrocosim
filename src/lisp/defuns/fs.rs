//! Filesystem helpers exposed to Lisp: `(file-exists-p)`, used by
//! the override-file loader's `(load-overrides)` guard.

use std::path::{Path, PathBuf};

use tulisp::TulispContext;

pub(super) fn register(ctx: &mut TulispContext, load_dir: PathBuf) {
    // Path resolution mirrors tulisp's `(load PATH)`: relative paths
    // are joined onto the config file's load dir, absolutes pass
    // through. `load-overrides` gates `(load <override-file>)` with
    // a `(file-exists-p …)` check; same base path keeps both calls
    // looking at the same file regardless of the process CWD.
    ctx.defun("file-exists-p", move |path: String| -> bool {
        let p = Path::new(&path);
        let resolved = if p.is_absolute() {
            p.to_path_buf()
        } else {
            load_dir.join(p)
        };
        resolved.exists()
    });
}
