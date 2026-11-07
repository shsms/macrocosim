//! `GET /api/mg/{mg}/spec.py`: a microgrid exported as a Python
//! module by `python -m macrocosim.export`.

mod common;

use std::path::{Path, PathBuf};

use common::{TestServer, normalized, venv_python};
use macrocosim::spec::TopologyDoc;
use reqwest::StatusCode;

/// One microgrid with a component of every category, a component
/// under two parents, and a field of each kind of value.
const EVERY_CATEGORY: &str = r#"{"schema_version": 1, "microgrids": [{
    "id": 41, "name": "it's \"north\"", "tso": "TN", "physics_tick_s": 0.05,
    "components": [
        {"type": "grid_connection_point", "id": 1, "rated_fuse_current_a": 32,
         "rated_upper_w": 30000.0},
        {"type": "meter", "id": 2, "power_w": 5000.0, "power_factor": 0.9,
         "leading": true, "interval_s": 1.0, "name": "load"},
        {"type": "battery_inverter", "id": 3, "operational_mode": "control_only"},
        {"type": "battery_inverter", "id": 4},
        {"type": "battery", "id": 5, "capacity_wh": 10000.0, "initial_soc_pct": 42.5,
         "health": "standby"},
        {"type": "solar_inverter", "id": 6, "sunlight_pct": 80.0},
        {"type": "chp", "id": 7},
        {"type": "ev_charger", "id": 8, "phases": 1, "idle": "full"},
        {"type": "steam_boiler", "id": 9, "target_bar": 10.0, "demand_kg_per_s": 0.5},
        {"type": "wind_turbine", "id": 10},
        {"type": "power_transformer", "id": 11},
        {"type": "meter", "id": 12, "telemetry_mode": "silent"},
        {"type": "breaker", "id": 13},
        {"type": "meter", "id": 14}
    ],
    "connections": [[1, 2], [1, 3], [1, 4], [3, 5], [4, 5], [1, 6], [1, 7], [1, 8],
                    [1, 9], [1, 10], [1, 11], [11, 12], [1, 13], [13, 14]]
}]}"#;

/// Import the exported module at `path` with the venv's Python and
/// render its `MICROGRIDS` back into topology JSON.
fn import_exported(python: &Path, path: &Path) -> serde_json::Value {
    const PROGRAM: &str = "import importlib.util, json, sys\n\
        from macrocosim._render import render\n\
        spec = importlib.util.spec_from_file_location('exported', sys.argv[1])\n\
        module = importlib.util.module_from_spec(spec)\n\
        spec.loader.exec_module(module)\n\
        print(json.dumps([render(m)['microgrids'][0] for m in module.MICROGRIDS]))\n";
    let out = std::process::Command::new(python)
        .arg("-c")
        .arg(PROGRAM)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the export does not import: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

async fn server_with(python: PathBuf) -> TestServer {
    let doc = TopologyDoc::from_json(EVERY_CATEGORY).unwrap();
    let s = TestServer::start_spec(doc).await;
    s.python.set_override(python);
    s
}

#[tokio::test]
async fn an_exported_microgrid_imports_and_gives_its_spec_back() {
    let Some(python) = venv_python() else { return };
    let s = server_with(python.clone()).await;
    let client = reqwest::Client::new();
    let r = client.get(s.mg_url("spec.py")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "text/x-python; charset=utf-8");
    let text = r.text().await.unwrap();
    assert!(text.contains("MICROGRIDS = ["), "{text}");
    assert!(text.contains(r#"name='it\'s "north"'"#), "{text}");
    let path = s.dir().join("exported.py");
    std::fs::write(&path, &text).unwrap();
    let rendered = import_exported(&python, &path);
    let (_, stored) = common::get_spec(&client, &s, s.mg_id).await;
    assert_eq!(
        normalized(rendered[0].clone()),
        normalized(stored),
        "{text}"
    );
}

#[tokio::test]
async fn no_interpreter_answers_503_with_the_install_message() {
    let s = server_with(PathBuf::from("/nonexistent/python")).await;
    let r = reqwest::get(s.mg_url("spec.py")).await.unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = r.json().await.unwrap();
    let error = body["error"].as_str().unwrap();
    assert!(
        error.starts_with("macrocosim needs Python 3.11 or later with the macrocosim package"),
        "{error}"
    );
    assert!(error.contains("Found: /nonexistent/python ("), "{error}");
}

#[tokio::test]
async fn exports_run_one_at_a_time() {
    let Some(python) = venv_python() else { return };
    let s = server_with(python).await;
    let client = reqwest::Client::new();
    // Each export starts Python, which takes far longer than sending
    // the next request, so the later ones find the first running.
    let requests = (0..4).map(|_| client.get(s.mg_url("spec.py")).send());
    let statuses: Vec<StatusCode> = futures::future::join_all(requests)
        .await
        .into_iter()
        .map(|r| r.unwrap().status())
        .collect();
    assert!(statuses.contains(&StatusCode::OK), "{statuses:?}");
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
}
