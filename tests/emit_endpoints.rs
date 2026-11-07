//! The endpoints file `--emit-endpoints` writes once everything is bound.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run the binary with `args`, wait for its endpoints file, and read
/// it. The child is killed when the returned guard drops.
fn endpoints_of(
    dir: &std::path::Path,
    args: &[&std::ffi::OsStr],
) -> (KillOnDrop, serde_json::Value) {
    let endpoints = dir.join("endpoints.json");
    let child = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_macrocosim"))
            .args(["--ephemeral-ports", "--state-dir"])
            .arg(dir)
            .arg(format!("--emit-endpoints={}", endpoints.display()))
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !endpoints.exists() {
        assert!(Instant::now() < deadline, "no endpoints file after 20 s");
        std::thread::sleep(Duration::from_millis(50));
    }
    let v = serde_json::from_str(&std::fs::read_to_string(&endpoints).unwrap()).unwrap();
    (child, v)
}

/// `--topology` boots the document's microgrids with no Lisp, and the
/// enterprise flags reach the servers they configure.
#[test]
fn a_topology_file_boots_its_microgrids() {
    let dir = tempfile::TempDir::with_prefix("mc-endpoints-").unwrap();
    let topology = dir.path().join("topology.json");
    std::fs::write(
        &topology,
        r#"{"schema_version": 1, "microgrids": [{"id": 61, "name": "south", "components": [
            {"type": "grid_connection_point", "id": 1},
            {"type": "meter", "id": 2, "power_w": 500.0}
        ], "connections": [[1, 2]]}]}"#,
    )
    .unwrap();
    let (_child, v) = endpoints_of(
        dir.path(),
        &[
            "--topology".as_ref(),
            topology.as_os_str(),
            "--assets-addr".as_ref(),
            "127.0.0.1:9900".as_ref(),
        ],
    );
    let mg0 = &v["microgrids"][0];
    assert_eq!(mg0["id"], 61, "{v}");
    assert_eq!(mg0["name"], "south", "{v}");
    assert!(mg0["grpc_addr"].as_str().is_some(), "{v}");
    assert!(
        v["assets"].as_str().unwrap().starts_with("127.0.0.1:"),
        "the flag moved the assets server: {v}"
    );
    assert_eq!(
        v["version"],
        macrocosim::version::pep440(env!("CARGO_PKG_VERSION")),
        "{v}"
    );
}

#[test]
fn each_microgrid_lists_its_grpc_addr() {
    let dir = tempfile::TempDir::with_prefix("mc-endpoints-").unwrap();
    let mg = dir.path().join("mg.lisp");
    std::fs::write(
        &mg,
        "(make-microgrid :id 51 :name \"north\" :topology (lambda () nil))",
    )
    .unwrap();
    let (_child, v) = endpoints_of(dir.path(), &[mg.as_os_str()]);
    let mg0 = &v["microgrids"][0];
    assert_eq!(mg0["id"], 51, "{v}");
    assert_eq!(mg0["name"], "north", "{v}");
    let addr = mg0["grpc_addr"].as_str().expect("grpc_addr is a string");
    assert!(addr.contains(':'), "{addr}");
    assert!(mg0.get("grpc").is_none(), "{v}");
    assert_eq!(
        v["version"],
        macrocosim::version::pep440(env!("CARGO_PKG_VERSION")),
        "{v}"
    );
}
