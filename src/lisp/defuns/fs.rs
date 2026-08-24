//! Filesystem helpers exposed to Lisp: `(file-exists-p)`, which a
//! script guards an optional `(load …)` with — "load my companion
//! file if it is there".

use std::path::{Path, PathBuf};

use tulisp::TulispContext;

pub(super) fn register(ctx: &mut TulispContext, load_dir: PathBuf) {
    // Path resolution mirrors tulisp's `(load PATH)`: relative paths
    // are joined onto the state dir, absolutes pass through. Sharing
    // the base path is the point — a script that guards a `(load …)`
    // with `(file-exists-p …)` must have both calls looking at the
    // same file regardless of the process CWD.
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
