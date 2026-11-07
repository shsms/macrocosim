//! The Python starter site matches the Lisp one: two binaries, one on
//! `examples/starter-site.lisp` through Lisp and one on
//! `examples/starter_site.py` through the script runner, serve the
//! same topology for microgrid 2200. Goes with the Lisp file.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");

/// A binary in its own state directory, killed on drop.
struct Engine {
    child: Child,
    ui: String,
    _dir: tempfile::TempDir,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Engine {
    /// Copy `example` into a new state directory and start the binary
    /// on the copy with `args`, once its endpoints file is written.
    fn start(example: &str, args: &[&std::ffi::OsStr]) -> Self {
        let dir = tempfile::TempDir::with_prefix("mc-parity-").unwrap();
        let script = dir.path().join(example);
        std::fs::copy(Path::new(EXAMPLES).join(example), &script).unwrap();
        let endpoints = dir.path().join("endpoints.json");
        let log = std::fs::File::create(dir.path().join("engine.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_macrocosim"))
            .args(["--ephemeral-ports", "--state-dir"])
            .arg(dir.path())
            .arg(format!("--emit-endpoints={}", endpoints.display()))
            .args(args)
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !endpoints.exists() {
            if Instant::now() > deadline {
                let log = std::fs::read_to_string(dir.path().join("engine.log"));
                panic!("{example}: no endpoints file after 60 s:\n{log:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&endpoints).unwrap()).unwrap();
        Self {
            child,
            ui: format!("http://{}", v["ui"].as_str().unwrap()),
            _dir: dir,
        }
    }

    async fn topology(&self) -> Value {
        let r = reqwest::get(format!("{}/api/mg/2200/topology", self.ui))
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}: {}", self.ui, r.status());
        r.json().await.unwrap()
    }
}

#[tokio::test]
async fn the_python_starter_site_has_the_lisp_topology() {
    let Some(python) = common::venv_python() else {
        return;
    };
    let lisp = Engine::start("starter-site.lisp", &[]);
    let script = Engine::start(
        "starter_site.py",
        &["--python".as_ref(), python.as_os_str()],
    );
    let (from_lisp, from_python) = (lisp.topology().await, script.topology().await);
    assert!(
        from_lisp["components"].as_array().unwrap().len() == 12,
        "{from_lisp:#}"
    );
    assert_eq!(from_python, from_lisp);
}
