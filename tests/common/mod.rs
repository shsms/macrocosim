//! Integration-test harness: spawn a `Config`-driven macrocosim
//! server in-process on OS-assigned ports, expose its gRPC + UI
//! addresses, and tear everything down on Drop.
//!
//! Each test gets its own temp dir for `config.lisp` and a fresh
//! `Config`, so parallel tests can't stomp each other's state.
//!
//! The fixture is in-process rather than out-of-process because:
//! - cargo runs each `tests/<file>.rs` as its own binary already,
//!   so OS-level isolation is overkill.
//! - In-process tests can poke at the engine's sites directly when
//!   the black-box gRPC / HTTP surface isn't enough.
//! - LOG_TAP and other process-level globals stay un-initialised in
//!   tests, so the `/api/logs` endpoint just returns empty —
//!   acceptable for a fixture.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use macrocosim::proto::common::metrics::{Bounds, Metric};
use macrocosim::proto::microgrid::{
    ReceiveElectricalComponentTelemetryStreamRequest, microgrid_client::MicrogridClient,
};
use macrocosim::runtime::MicrogridRuntimes;
use macrocosim::spec::TopologyDoc;
use macrocosim::{
    assets_server::AssetsServer, engine::Engine, lisp::Config,
    proto::assets::platform_assets_server::PlatformAssetsServer as AssetsGrpcServer,
    sim::MicrogridSite, ui,
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

static UNIQ: AtomicU64 = AtomicU64::new(0);

/// A live macrocosim instance: gRPC + UI on OS-assigned localhost
/// ports, plus the underlying [`Engine`] for direct world inspection
/// and the [`Config`] for driving it through Lisp.
/// `Drop` aborts the registration listener and the UI / assets server
/// tasks in `handles`; per-microgrid servers live in the runtimes and
/// are never aborted. The temp dir cleans up via the held `TempDir`
/// handle.
///
/// Each integration-test binary picks the fields it needs; the
/// `#[allow(dead_code)]` keeps the unused-warning quiet for tests
/// that only touch one surface.
#[allow(dead_code)]
pub struct TestServer {
    pub grpc_url: String,
    pub ui_url: String,
    pub assets_url: String,
    pub engine: Engine,
    pub config: Config,
    /// Id of the default microgrid, the one `grpc_url` serves.
    pub mg_id: u64,
    pub runtimes: MicrogridRuntimes,
    handles: Vec<JoinHandle<()>>,
    _tempdir: TempDir,
}

impl TestServer {
    /// Bring up a server backed by the supplied `config.lisp` body.
    /// Caller is on a tokio runtime (provided by `#[tokio::test]`).
    pub async fn start(config_body: &str) -> Self {
        let tempdir = TempDir::with_prefix(format!(
            "macrocosim-it-{}-",
            UNIQ.fetch_add(1, Ordering::Relaxed),
        ))
        .expect("create temp dir");
        let path = tempdir.path().join("config.lisp");
        let wrapped = wrap_body(config_body);
        std::fs::write(&path, wrapped).expect("write config");

        let config = Config::new(path.to_str().unwrap()).expect("config eval");
        let engine = config.engine().clone();
        // Same startup path as the binary: the runtimes start every
        // registered microgrid, and the listener starts the ones a
        // test registers later. Ephemeral ports keep parallel tests
        // from colliding.
        let runtimes = macrocosim::runtime::MicrogridRuntimes::new(
            engine.clone(),
            macrocosim::runtime::RuntimeOptions {
                ephemeral_ports: true,
                bind_host: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            },
        )
        .expect("runtimes");
        let mut handles = Vec::new();
        handles.push(runtimes.spawn_registration_listener().await);

        // Single-microgrid tests: `mg_id` and the gRPC address are
        // those of the default registry entry (the one auto-seeded by
        // Config::new, or the id an explicit form in `config_body`
        // chose).
        let default_mg_id = {
            let reg = engine.microgrids();
            let r = reg.lock();
            r.keys().copied().next().expect("default microgrid entry")
        };
        let grpc_addr = runtimes
            .status(default_mg_id)
            .and_then(|v| v.grpc_addr)
            .expect("the default microgrid started");

        // Bind the UI and assets servers to OS-assigned ports;
        // local_addr() reads back the chosen port before the listener
        // goes to its server.
        let ui_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ui port");
        let ui_addr = ui_listener.local_addr().expect("ui addr");
        let ui_config = config.clone();
        let ui_runtimes = runtimes.clone();
        handles.push(tokio::spawn(async move {
            let _ = ui::serve_with_listener(ui_listener, ui_config, ui_runtimes).await;
        }));

        // PlatformAssets has its own port, as in the binary.
        let assets_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind assets port");
        let assets_addr = assets_listener.local_addr().expect("assets addr");
        let assets_server = AssetsServer::new(engine.clone());
        handles.push(tokio::spawn(async move {
            let _ = Server::builder()
                .add_service(AssetsGrpcServer::new(assets_server))
                .serve_with_incoming(TcpListenerStream::new(assets_listener))
                .await;
        }));

        // Wait until the UI server actually answers a request
        // instead of sleeping a fixed 50 ms — on a loaded machine
        // the accept loops can take longer than any fixed delay,
        // and this poll returns as soon as they are up. The gRPC
        // listener needs no probe of its own: its socket is bound
        // above, so connects queue until tonic starts accepting.
        let probe = reqwest::Client::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match probe
                .get(format!("http://{ui_addr}/api/microgrids"))
                .send()
                .await
            {
                Ok(_) => break,
                Err(e) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "UI server at {ui_addr} not ready after 10s: {e}"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }

        Self {
            grpc_url: format!("http://{grpc_addr}"),
            ui_url: format!("http://{ui_addr}"),
            assets_url: format!("http://{assets_addr}"),
            engine,
            config,
            mg_id: default_mg_id,
            runtimes,
            handles,
            _tempdir: tempdir,
        }
    }

    /// [`Self::start`] with the Lisp starter, then every microgrid of
    /// `doc` built from its spec through `engine::topology::create`,
    /// with `source` as its file. `mg_id` and `grpc_url` are those of
    /// the first spec microgrid.
    #[allow(dead_code)]
    pub async fn start_spec_from(doc: TopologyDoc, source: Option<PathBuf>) -> Self {
        let mut server = Self::start("").await;
        let lock = server.engine.create_lock();
        let _structural = lock.lock().await;
        let mut first = None;
        for spec in doc.microgrids {
            let created = macrocosim::engine::topology::create(
                &server.engine,
                &server.runtimes,
                spec,
                source.clone(),
            )
            .await
            .expect("create a microgrid from its spec");
            first.get_or_insert(created);
        }
        let first = first.expect("the document has a microgrid");
        server.mg_id = first.id;
        let addr = first
            .runtime
            .and_then(|r| r.grpc_addr)
            .expect("the spec microgrid started");
        server.grpc_url = format!("http://{addr}");
        server
    }

    /// [`Self::start_spec_from`] with no source file: microgrids made
    /// as the UI makes them.
    #[allow(dead_code)]
    pub async fn start_spec(doc: TopologyDoc) -> Self {
        Self::start_spec_from(doc, None).await
    }

    /// The temp dir this server runs in.
    #[allow(dead_code)]
    pub fn dir(&self) -> &std::path::Path {
        self._tempdir.path()
    }

    /// The site of this server's microgrid, read from the registry.
    #[allow(dead_code)]
    pub fn site(&self) -> MicrogridSite {
        self.engine
            .microgrids()
            .lock()
            .get(&self.mg_id)
            .expect("the default microgrid is registered")
            .site
            .clone()
    }

    /// URL of a route under this server's microgrid.
    #[allow(dead_code)]
    pub fn mg_url(&self, suffix: &str) -> String {
        format!("{}/api/mg/{}/{suffix}", self.ui_url, self.mg_id)
    }

    /// Path of the config.lisp file backing this server. Tests
    /// that exercise the watcher (hot-reload) overwrite this file
    /// to trigger a reload.
    #[allow(dead_code)]
    pub fn config_path(&self) -> PathBuf {
        self._tempdir.path().join("config.lisp")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

/// POST a Lisp body to `/api/eval` and fail the test unless the
/// server answers 200 with a `value`. The shared spelling of "drive
/// the sim the way the dashboard's REPL does" every integration test
/// that needs a live eval uses.
#[allow(dead_code)]
pub async fn eval_or_panic(client: &reqwest::Client, s: &TestServer, body: &str) {
    let r = client
        .post(format!("{}/api/eval", s.ui_url))
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status();
    let json: serde_json::Value = r.json().await.unwrap();
    assert!(
        status.is_success() && json["value"].is_string(),
        "eval {body} failed: {status} {json}",
    );
}

/// `GET /api/mg/{mg}/spec`: the status and the body.
#[allow(dead_code)]
pub async fn get_spec(
    client: &reqwest::Client,
    s: &TestServer,
    mg: u64,
) -> (reqwest::StatusCode, serde_json::Value) {
    let r = client
        .get(format!("{}/api/mg/{mg}/spec", s.ui_url))
        .send()
        .await
        .unwrap();
    (r.status(), r.json().await.unwrap())
}

/// `PUT /api/mg/{mg}/spec` with `spec`: the status and the body.
#[allow(dead_code)]
pub async fn put_spec(
    client: &reqwest::Client,
    s: &TestServer,
    mg: u64,
    spec: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let r = client
        .put(format!("{}/api/mg/{mg}/spec", s.ui_url))
        .json(spec)
        .send()
        .await
        .unwrap();
    (r.status(), r.json().await.unwrap())
}

/// `POST /api/mg/{mg}/component/{id}/drive` with `body`; panics
/// unless it succeeds.
#[allow(dead_code)]
pub async fn drive(
    client: &reqwest::Client,
    s: &TestServer,
    mg: u64,
    id: u64,
    body: serde_json::Value,
) {
    post_ok(client, s, mg, id, "drive", body).await;
}

/// `POST /api/mg/{mg}/component/{id}/status` with `body`; panics
/// unless it succeeds.
#[allow(dead_code)]
pub async fn status(
    client: &reqwest::Client,
    s: &TestServer,
    mg: u64,
    id: u64,
    body: serde_json::Value,
) {
    post_ok(client, s, mg, id, "status", body).await;
}

async fn post_ok(
    client: &reqwest::Client,
    s: &TestServer,
    mg: u64,
    id: u64,
    route: &str,
    body: serde_json::Value,
) {
    let r = client
        .post(format!("{}/api/mg/{mg}/component/{id}/{route}", s.ui_url))
        .json(&body)
        .send()
        .await
        .unwrap();
    let code = r.status();
    assert!(
        code.is_success(),
        "{route} {id} {body}: {code} {}",
        r.text().await.unwrap_or_default()
    );
}

/// Subscribe to `id` and return the bounds of the first `metric`
/// sample that carries any. Panics if none arrives within 5 s.
#[allow(dead_code)]
pub async fn first_bounds(
    c: &mut MicrogridClient<tonic::transport::Channel>,
    id: u64,
    metric: Metric,
) -> Vec<Bounds> {
    let mut stream = c
        .receive_electrical_component_telemetry_stream(
            ReceiveElectricalComponentTelemetryStreamRequest {
                electrical_component_id: id,
                filter: None,
            },
        )
        .await
        .expect("subscribe")
        .into_inner();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(msg)) = stream.message().await {
            let sample = msg
                .telemetry
                .as_ref()
                .and_then(|t| t.metric_samples.iter().find(|s| s.metric == metric as i32));
            if let Some(s) = sample
                && !s.bounds.is_empty()
            {
                return Some(s.bounds.clone());
            }
        }
        None
    })
    .await
    .expect("telemetry stream timed out")
    .expect("a sample carrying bounds")
}

/// A minimal WebSocket client for `/ws/events`: the handshake, then
/// server text frames (never masked).
#[allow(dead_code)]
pub struct WsClient {
    stream: tokio::net::TcpStream,
}

#[allow(dead_code)]
impl WsClient {
    pub async fn connect(ui_url: &str) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let host = ui_url.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        let request = format!(
            "GET /ws/events HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
        }
        let head = String::from_utf8_lossy(&head);
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        Self { stream }
    }

    /// The next text frame, parsed as JSON.
    pub async fn next_json(&mut self) -> serde_json::Value {
        use tokio::io::AsyncReadExt;
        loop {
            let b0 = self.stream.read_u8().await.unwrap();
            let len = match self.stream.read_u8().await.unwrap() & 0x7f {
                126 => u64::from(self.stream.read_u16().await.unwrap()),
                127 => self.stream.read_u64().await.unwrap(),
                n => u64::from(n),
            };
            let mut payload = vec![0; len as usize];
            self.stream.read_exact(&mut payload).await.unwrap();
            if b0 & 0x0f == 1 {
                return serde_json::from_slice(&payload).unwrap();
            }
        }
    }

    /// The first event `pred` accepts within `within`; panics naming
    /// `what` when none comes.
    pub async fn wait_for(
        &mut self,
        within: std::time::Duration,
        what: &str,
        mut pred: impl FnMut(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        tokio::time::timeout(within, async {
            loop {
                let event = self.next_json().await;
                if pred(&event) {
                    return event;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {what} within {within:?}"))
    }
}

/// Wrap a test body in `(make-microgrid …)` if the body doesn't
/// already register one. Tests that care about the microgrid's id
/// supply their own `(make-microgrid …)` form; everything else gets
/// the fixed default id 2200.
fn wrap_body(body: &str) -> String {
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

/// The interpreter of `python/.venv`, where `uv sync` in `python/`
/// installs the package. `None`, with a printed note, when it is
/// missing; a panic instead under `MACROCOSIM_REQUIRE_PYTHON=1`,
/// which CI sets.
#[allow(dead_code)]
pub fn venv_python() -> Option<PathBuf> {
    let path = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/python/.venv/bin/python"
    ));
    if path.exists() {
        return Some(path);
    }
    let note = format!("{} is missing; run `uv sync` in python/", path.display());
    assert!(
        std::env::var_os("MACROCOSIM_REQUIRE_PYTHON").is_none_or(|v| v != "1"),
        "{note}"
    );
    eprintln!("skipped: {note}");
    None
}

/// `doc`'s components sorted by id and its connections sorted, so two
/// renderings of one microgrid compare equal.
#[allow(dead_code)]
pub fn normalized(mut microgrid: serde_json::Value) -> serde_json::Value {
    if let Some(components) = microgrid["components"].as_array_mut() {
        components.sort_by_key(|c| c["id"].as_u64());
    }
    if let Some(connections) = microgrid["connections"].as_array_mut() {
        connections.sort_by_key(|c| (c[0].as_u64(), c[1].as_u64()));
    }
    microgrid
}
