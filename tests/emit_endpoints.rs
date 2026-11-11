//! The endpoints file `--emit-endpoints` writes once everything is bound.

mod common;

use common::endpoints_of;

/// `--topology` boots the document's microgrids, and the assets and
/// dispatch address flags move those servers.
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
            "--dispatch-addr".as_ref(),
            "127.0.0.1:8900".as_ref(),
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
    assert!(
        v["dispatch"].as_str().unwrap().starts_with("127.0.0.1:"),
        "the flag moved the dispatch server: {v}"
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
    let topology = dir.path().join("topology.json");
    std::fs::write(
        &topology,
        r#"{"schema_version": 1, "microgrids": [{"id": 51, "name": "north", "components": []}]}"#,
    )
    .unwrap();
    let (_child, v) = endpoints_of(dir.path(), &["--topology".as_ref(), topology.as_os_str()]);
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
