//! Shared test fixtures for the lisp/ subtree. Every child module's
//! `#[cfg(test)] mod tests` block builds its `Config` instances
//! through `config_with`, which seeds a unique temp dir + auto-
//! wraps the test body in a `(make-microgrid …)` form so callers
//! don't have to repeat the boilerplate.

use std::sync::atomic::{AtomicU64, Ordering};

use super::Config;

static UNIQ: AtomicU64 = AtomicU64::new(0);

/// Build a Config from a tiny config.lisp body in a unique temp
/// dir; returns the Config + the dir so tests can mess with the
/// per-microgrid override path.
pub(super) fn config_with(body: &str) -> (Config, std::path::PathBuf) {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "switchyard-cfg-{}-{}",
        std::process::id(),
        UNIQ.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.lisp");
    let wrapped = wrap_test_body(body);
    std::fs::write(&path, wrapped).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let cfg = rt
        .block_on(async { Config::new(path.to_str().unwrap()) })
        .expect("config eval");
    // Drop the runtime — Config keeps its own handles to whatever
    // tulisp-async spawned during init.
    std::mem::forget(rt);
    (cfg, dir)
}

/// Auto-wrap a test body in `(make-microgrid …)` if the body doesn't
/// already register one — every config must do so post-migration, but
/// most tests don't care about the wrapper and just want their forms
/// evaluated in a microgrid scope. Tests that exercise make-microgrid
/// itself, or that care about the microgrid's id, supply their own
/// form and the wrapper is skipped. Everything else gets the fixed
/// default id 2200.
pub(super) fn wrap_test_body(body: &str) -> String {
    if body.contains("make-microgrid") {
        return body.to_string();
    }
    let inner = if body.trim().is_empty() {
        "nil".to_string()
    } else {
        body.to_string()
    };
    format!("(make-microgrid :id 2200 :grpc-port 8800 :topology (lambda () {inner}))")
}

/// Counter for tests that need their own unique temp dir without
/// going through `config_with`.
pub(super) fn next_unique() -> u64 {
    UNIQ.fetch_add(1, Ordering::Relaxed)
}
