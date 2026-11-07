//! Headless macrocosim simulator: run a Python script or boot the
//! microgrids of a topology document (`--topology`), spawn the
//! physics tick, serve the Microgrid gRPC API and the UI.
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use macrocosim::runner::python::Interpreter;
use macrocosim::{
    assets_server::AssetsServer,
    dispatch_server::DispatchServer,
    engine::{Engine, Metadata},
    proto::assets::platform_assets_server::PlatformAssetsServer as AssetsGrpcServer,
    proto::dispatch::microgrid_dispatch_service_server::MicrogridDispatchServiceServer as DispatchGrpcServer,
    runtime::{MicrogridRuntimes, RuntimeOptions, RuntimeStatus},
    sim::sim_clock::NowSource,
    ui, ui_log,
};
use simplelog::{
    ColorChoice, CombinedLogger, ConfigBuilder, LevelFilter, TermLogger, TerminalMode,
};
use tonic::transport::Server;

use tokio_stream::wrappers::TcpListenerStream;

/// Headless macrocosim microgrid simulator.
#[derive(Parser)]
struct Args {
    /// The Python script (`plant.py`) to run, in a child process that
    /// is started again whenever a `.py` file in its directory tree
    /// changes. With none, the engine boots with an empty registry;
    /// create microgrids from the Microgrids tab.
    #[arg(value_name = "SCRIPT")]
    scripts: Vec<PathBuf>,

    /// Boot the microgrids of this topology document (JSON), each
    /// built from its spec with the file as its source. Not with a
    /// script.
    #[arg(long, value_name = "PATH", conflicts_with = "scripts")]
    topology: Option<PathBuf>,

    /// The enterprise id every microgrid reports.
    #[arg(long, value_name = "ID")]
    enterprise_id: Option<u64>,

    /// Address of the PlatformAssets gRPC server (default
    /// `[::1]:9900`).
    #[arg(long, value_name = "ADDR")]
    assets_addr: Option<SocketAddr>,

    /// Address of the MicrogridDispatchService gRPC server (default
    /// `[::1]:8900`).
    #[arg(long, value_name = "ADDR")]
    dispatch_addr: Option<SocketAddr>,

    /// Request lifetime of a setpoint that carries none, in seconds
    /// (default 60).
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    request_lifetime: Option<Duration>,

    /// Lifetime of a bounds augmentation that carries none, in
    /// seconds (default 5).
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    augment_lifetime: Option<Duration>,

    /// The directory recordings write their CSV files under
    /// (`recordings/`). Defaults to the current directory.
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,

    /// The Python interpreter that runs the script and exports a
    /// microgrid as a `.py` module. Without it, `$MACROCOSIM_PYTHON`,
    /// then a `python` next to this binary, then `python3` or
    /// `python` on `PATH`.
    #[arg(long, value_name = "PATH")]
    python: Option<PathBuf>,

    /// UI HTTP port (0 = OS-chosen). Ignored under --ephemeral-ports.
    #[arg(long, default_value_t = 8801)]
    ui_port: u16,

    /// Bind the UI and every gRPC listener (per-microgrid, assets,
    /// dispatch) on an OS-chosen port, overriding config / defaults —
    /// for running parallel instances (e.g. CI) without port clashes.
    #[arg(long)]
    ephemeral_ports: bool,

    /// Once every listener is bound, write the resolved endpoints as
    /// one JSON line — to `--emit-endpoints=PATH`, or stdout if the
    /// flag is given bare. Requires `=` so it can't swallow the
    /// script positional. The machine-readable readiness signal.
    #[arg(long, value_name = "PATH", num_args = 0..=1, require_equals = true, default_missing_value = "-")]
    emit_endpoints: Option<String>,
}

/// A non-negative, finite number of seconds.
fn parse_seconds(text: &str) -> Result<Duration, String> {
    let s: f64 = text.parse().map_err(|e| format!("{e}"))?;
    Duration::try_from_secs_f64(s)
        .map_err(|_| format!("{s} is not a non-negative number of seconds"))
}

/// The enterprise settings: the defaults, with the enterprise flags
/// laid over them.
fn metadata_of(args: &Args) -> Metadata {
    let mut m = Metadata::default();
    if let Some(id) = args.enterprise_id {
        m.enterprise_id = id;
    }
    if let Some(addr) = args.assets_addr {
        m.assets_socket_addr = addr.to_string();
    }
    if let Some(addr) = args.dispatch_addr {
        m.dispatch_socket_addr = addr.to_string();
    }
    if let Some(d) = args.request_lifetime {
        m.default_request_lifetime = d;
    }
    if let Some(d) = args.augment_lifetime {
        m.default_augment_lifetime = d;
    }
    m
}

/// The Lisp files of macrocosim 0.1 a run found, which this version
/// refuses.
#[derive(Debug, PartialEq)]
struct LispInputs {
    /// The `.lisp` arguments.
    args: Vec<PathBuf>,
    /// The state directory's `enterprise.lisp` and
    /// `microgrids/*.lisp`.
    in_state_dir: Vec<PathBuf>,
}

impl LispInputs {
    fn is_empty(&self) -> bool {
        self.args.is_empty() && self.in_state_dir.is_empty()
    }

    /// The error the binary exits with when Lisp files were found. It
    /// points at `--state-dir` only when the state directory holds
    /// some of them.
    fn refusal(&self) -> String {
        let found = self
            .args
            .iter()
            .chain(&self.in_state_dir)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let mut message = format!(
            "macrocosim 0.2 no longer reads Lisp; support ended with 0.1.\n\
             Found: {found}. Port these files to a Python script (see docs/python-scripts.md)"
        );
        if !self.in_state_dir.is_empty() {
            message.push_str(",\nor pass --state-dir to start from another directory");
        }
        message.push('.');
        message
    }
}

/// The Lisp files of macrocosim 0.1 a run was given or finds: every
/// `.lisp` argument, and the state directory's `enterprise.lisp` and
/// `microgrids/*.lisp`.
fn lisp_inputs(scripts: &[PathBuf], state_dir: &Path) -> LispInputs {
    let is_lisp = |p: &Path| p.extension().is_some_and(|e| e == "lisp");
    let args = scripts.iter().filter(|p| is_lisp(p)).cloned().collect();
    let mut in_state_dir = Vec::new();
    let enterprise = state_dir.join("enterprise.lisp");
    if enterprise.exists() {
        in_state_dir.push(enterprise);
    }
    let mut managed: Vec<PathBuf> = std::fs::read_dir(state_dir.join("microgrids"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_lisp(p))
        .collect();
    managed.sort();
    in_state_dir.extend(managed);
    LispInputs { args, in_state_dir }
}

/// Build every microgrid of the topology document at `path`, with the
/// file as its source, or log why not and exit.
async fn boot_topology(
    path: &std::path::Path,
    engine: &macrocosim::engine::Engine,
    runtimes: &MicrogridRuntimes,
) {
    use macrocosim::engine::topology;
    let fail = |e: String| -> ! {
        log::error!("--topology {}: {e}", path.display());
        std::process::exit(1);
    };
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(e.to_string()));
    let doc =
        macrocosim::spec::TopologyDoc::from_json(&text).unwrap_or_else(|e| fail(e.to_string()));
    let source = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let structural = engine.structural().await;
    let created = topology::create_doc(&structural, runtimes, doc, Some(source))
        .await
        .unwrap_or_else(|e| fail(e.to_string()));
    let ids: Vec<u64> = created.iter().map(|c| c.id).collect();
    log::info!("--topology {}: microgrids {ids:?} built", path.display());
}

/// The one Python script among the positional arguments, if any;
/// refuses a second script and a file that is not a `.py`.
fn script_arg(paths: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    match paths {
        [] => Ok(None),
        [script] if script.extension().is_some_and(|e| e == "py") => Ok(Some(script.clone())),
        [other] => Err(format!(
            "a script is a Python file (.py); got {}",
            other.display()
        )),
        _ => Err(format!(
            "one Python script per engine; got {}",
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Bind a TCP listener for `label`, or log and exit the process on
/// failure. Returns the listener and its resolved local address — the
/// OS-chosen port when `addr`'s port is 0 (`--ephemeral-ports`).
async fn bind_or_exit(addr: SocketAddr, label: &str) -> (tokio::net::TcpListener, SocketAddr) {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            log::error!("{label} bind {addr} failed: {e}");
            std::process::exit(1);
        });
    let resolved = listener.local_addr().unwrap();
    (listener, resolved)
}

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed building the Runtime")
        .block_on(serve());
}

async fn serve() {
    let args = Args::parse();

    // Suppress per-tick "channel closed" spam from frequenz-microgrid
    // 0.4.1's ComponentTelemetryTracker. When a `BatteryPool` drops
    // (which happens on every topology rebuild) the tracker tasks it
    // spawned keep ticking on a timer and log at error level when
    // they fail to send into the closed mpsc — see
    // /vagrant/upstream-tracker-leak.md. The trackers are otherwise
    // harmless (orphaned, no measurable CPU), but the log spam scales
    // linearly with rebuilds. Drop the noisy module here — same list
    // applied to both the terminal logger and the UI tap so the SPA's
    // log panel + /api/logs backfill stay clean too.
    let ignore_targets: &[&str] =
        &["frequenz_microgrid::microgrid::telemetry_tracker::component_telemetry_tracker"];

    // Combined logger: terminal output (existing UX) + a tap that
    // captures records into a ring buffer + broadcasts them on a
    // tokio channel. The UI server reads both: /api/logs returns the
    // ring for backfill on page load, /ws/events forwards the live
    // stream so the SPA's log panel updates in real time.
    let log_tap = ui_log::LogTap::new(
        500,
        LevelFilter::Info,
        ignore_targets.iter().map(|s| (*s).to_owned()).collect(),
    );
    ui_log::LOG_TAP
        .set(log_tap.clone())
        .unwrap_or_else(|_| panic!("LOG_TAP already initialised"));
    let mut log_cfg = ConfigBuilder::new();
    for t in ignore_targets {
        log_cfg.add_filter_ignore_str(t);
    }
    let log_config = log_cfg.build();
    CombinedLogger::init(vec![
        TermLogger::new(
            LevelFilter::Info,
            log_config,
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ),
        Box::new(log_tap),
    ])
    .unwrap();

    // 0.1's Lisp inputs, given or left in the state directory, stop
    // the run before anything binds.
    let state_dir = args.state_dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let found = lisp_inputs(&args.scripts, &state_dir);
    if !found.is_empty() {
        log::error!("{}", found.refusal());
        std::process::exit(1);
    }
    let python_script = script_arg(&args.scripts).unwrap_or_else(|e| {
        log::error!("{e}");
        std::process::exit(1);
    });
    let python_script = python_script.map(|p| {
        p.canonicalize().unwrap_or_else(|e| {
            log::error!("Script {}: {e}", p.display());
            std::process::exit(1);
        })
    });
    match &python_script {
        Some(script) => log::info!("Running script {}", script.display()),
        None if args.topology.is_none() => {
            log::info!("No script given — starting with no microgrids")
        }
        None => {}
    }
    if let Err(e) = std::fs::create_dir_all(&state_dir) {
        log::error!("state dir {} cannot be created: {e}", state_dir.display());
        std::process::exit(1);
    }
    let engine = Engine::new(state_dir, metadata_of(&args), NowSource::wall());
    // The interpreter the script and the `.py` export run on.
    let interpreter = Arc::new(Interpreter::default());
    if let Some(python) = &args.python {
        interpreter.set_override(python.clone());
    }
    // A script needs a usable interpreter; without one, say how to
    // get one before anything binds.
    let python = match &python_script {
        Some(_) => Some(interpreter.resolve().await.unwrap_or_else(|e| {
            log::error!("{e}");
            std::process::exit(1);
        })),
        None => None,
    };

    // Every microgrid runtime, boot-time and later, starts through
    // MicrogridRuntimes: each structural create starts its own.
    let runtimes = MicrogridRuntimes::new(
        engine.clone(),
        RuntimeOptions {
            ephemeral_ports: args.ephemeral_ports,
            bind_host: Ipv6Addr::LOCALHOST.into(),
        },
    )
    .unwrap_or_else(|e| {
        log::error!("{e}");
        std::process::exit(1);
    });
    if let Some(path) = &args.topology {
        boot_topology(path, &engine, &runtimes).await;
    }
    let boot_ids: Vec<u64> = engine.microgrids().lock().keys().copied().collect();
    log::info!("Enterprise carries {} microgrid(s)", boot_ids.len());
    // `--emit-endpoints` promises every boot listener is up, so a
    // boot microgrid that failed to start exits the process.
    for id in &boot_ids {
        if let Some(v) = runtimes.status(*id)
            && v.status == RuntimeStatus::Failed
        {
            log::error!("Microgrid #{id} failed to start; exiting");
            std::process::exit(1);
        }
    }

    // Bind the UI, assets and dispatch listeners up front so
    // ephemeral (:0) ports resolve to real ones before the endpoints
    // are emitted. Hosts stay loopback (UI 127.0.0.1, gRPC [::1]); a
    // routable --*-bind + the hardening it gates is a follow-up
    // (todo §D3).
    let eph = args.ephemeral_ports;
    let ui_port = if eph { 0 } else { args.ui_port };
    let (ui_listener, ui_addr) =
        bind_or_exit(SocketAddr::from((Ipv4Addr::LOCALHOST, ui_port)), "UI").await;

    // Assets + dispatch: single enterprise-wide sockets (defaults
    // [::1]:9900 / [::1]:8900, or --assets-addr / --dispatch-addr).
    // --ephemeral-ports zeroes the port for an OS-chosen one.
    let mut assets_addr: SocketAddr = engine.assets_socket_addr().parse().unwrap_or_else(|e| {
        log::error!("invalid assets socket addr: {e}");
        std::process::exit(1);
    });
    if eph {
        assets_addr.set_port(0);
    }
    let (assets_listener, assets_addr) = bind_or_exit(assets_addr, "PlatformAssets").await;
    let mut dispatch_addr: SocketAddr = engine.dispatch_socket_addr().parse().unwrap_or_else(|e| {
        log::error!("invalid dispatch socket addr: {e}");
        std::process::exit(1);
    });
    if eph {
        dispatch_addr.set_port(0);
    }
    let (dispatch_listener, dispatch_addr) = bind_or_exit(dispatch_addr, "MicrogridDispatch").await;

    // The script's supervisor exists before the UI serves, which
    // answers its child's hello; the child starts once it does.
    let supervisor = python_script.zip(python).map(|(script, python)| {
        macrocosim::runner::ScriptSupervisor::new(
            engine.clone(),
            runtimes.clone(),
            script,
            python,
            ui_addr,
        )
    });

    // Critical long-running tasks (UI, PlatformAssets and dispatch
    // servers) go into one JoinSet: any of them exiting means the
    // process is limping with a dead surface, so main notices the
    // FIRST exit and shuts the whole binary down instead of serving
    // degraded. A microgrid's gRPC server reports through its runtime
    // status instead.
    let mut tasks: tokio::task::JoinSet<&'static str> = tokio::task::JoinSet::new();
    log::info!("Macrocosim UI listening on http://{ui_addr}");
    let ui_engine = engine.clone();
    let ui_runtimes = runtimes.clone();
    let ui_runner = supervisor.clone();
    let ui_python = interpreter.clone();
    tasks.spawn(async move {
        if let Err(e) =
            ui::serve_with_listener(ui_listener, ui_engine, ui_runtimes, ui_python, ui_runner).await
        {
            log::error!("UI server exited: {e}");
        }
        "UI server"
    });

    // PlatformAssets — its own listener, reachable regardless of which
    // microgrid the client picks.
    log::info!("PlatformAssets gRPC listening on {assets_addr}");
    let engine_for_assets = engine.clone();
    tasks.spawn(async move {
        if let Err(e) = Server::builder()
            .add_service(AssetsGrpcServer::new(AssetsServer::new(engine_for_assets)))
            .serve_with_incoming(TcpListenerStream::new(assets_listener))
            .await
        {
            log::error!("PlatformAssets gRPC server exited: {e}");
        }
        "PlatformAssets gRPC server"
    });
    // The single (enterprise-wide) MicrogridDispatchService — its own
    // listener, one service fronting every microgrid (keyed by the
    // microgrid_id carried in each request).
    log::info!("MicrogridDispatch gRPC listening on {dispatch_addr}");
    let dispatch_store = engine.dispatches();
    let dispatch_registry = engine.microgrids();
    tasks.spawn(async move {
        if let Err(e) = Server::builder()
            .add_service(DispatchGrpcServer::new(DispatchServer::new(
                dispatch_store,
                dispatch_registry,
            )))
            .serve_with_incoming(TcpListenerStream::new(dispatch_listener))
            .await
        {
            log::error!("MicrogridDispatch gRPC server exited: {e}");
        }
        "MicrogridDispatch gRPC server"
    });
    // With a script, Ctrl-C stops its child (stdin closed, then
    // killed after a grace period, and its process group ended)
    // before the process exits. The listener is registered here,
    // before the first child starts, so a Ctrl-C while it loads stops
    // it the same way; on Unix the child leads its own process group,
    // which the terminal's SIGINT does not reach, and elsewhere it
    // ignores SIGINT.
    let mut script = supervisor
        .as_ref()
        .map(|supervisor| (supervisor, ctrl_c_listener()));
    // Run the script once the UI serves: its child's hello needs it.
    if let Some((supervisor, _)) = &script {
        supervisor.start().await;
    }

    // Emit the resolved endpoints once everything is bound — the
    // machine-readable readiness signal. Boot-time microgrids only;
    // runtime-created ones (POST /api/microgrids) aren't listed. With
    // a script, once its first child's hello was answered or it
    // failed to load: the microgrids registered then.
    if let Some(target) = &args.emit_endpoints {
        let boot_ids = match script.as_mut() {
            Some((supervisor, ctrl_c)) => {
                tokio::select! {
                    () = supervisor.first_load() => {}
                    () = ctrl_c => stop_script_and_exit(supervisor).await,
                }
                engine.microgrids().lock().keys().copied().collect()
            }
            None => boot_ids.clone(),
        };
        let json = serde_json::json!({
            "ui": ui_addr.to_string(),
            "microgrids": boot_ids
                .iter()
                .filter_map(|id| {
                    let name = engine.microgrids().lock().get(id)?.def.name.clone();
                    let addr = runtimes.status(*id)?.serving_addr()?;
                    Some(serde_json::json!({ "id": id, "name": name, "grpc_addr": addr }))
                })
                .collect::<Vec<_>>(),
            "assets": assets_addr.to_string(),
            "dispatch": dispatch_addr.to_string(),
            "version": *macrocosim::version::VERSION,
        })
        .to_string();
        if target == "-" {
            println!("{json}");
        } else {
            // This file is the readiness signal a harness polls for,
            // so a failed write must kill the process (like a failed
            // bind) — not leave a healthy-looking server the poller
            // can never detect. Write + rename so the file appears
            // only once its content is complete.
            let tmp = format!("{target}.tmp");
            if let Err(e) = std::fs::write(&tmp, format!("{json}\n"))
                .and_then(|()| std::fs::rename(&tmp, target))
            {
                log::error!("emit-endpoints write {target}: {e}");
                std::process::exit(1);
            }
        }
    }

    // First exit wins: a critical surface died (its own error was
    // already logged), so stop the whole process rather than limping
    // on with the remaining listeners.
    let interrupted = async {
        match script.as_mut() {
            Some((supervisor, ctrl_c)) => {
                ctrl_c.await;
                *supervisor
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        res = tasks.join_next() => {
            match res {
                Some(Ok(label)) => log::error!("{label} exited; shutting down"),
                Some(Err(e)) => log::error!("critical task panicked: {e}; shutting down"),
                None => log::error!("no server task is running; shutting down"),
            }
            if let Some(supervisor) = &supervisor {
                supervisor.shutdown().await;
            }
            std::process::exit(1);
        }
        supervisor = interrupted => stop_script_and_exit(supervisor).await,
    }
}

/// A future that resolves on the first Ctrl-C. The signal handler is
/// registered before this returns. A failed registration is logged
/// and the future never resolves, so the process keeps running.
fn ctrl_c_listener() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    #[cfg(unix)]
    let listener = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
    #[cfg(not(unix))]
    let listener = tokio::signal::windows::ctrl_c();
    let listener = listener
        .inspect_err(|e| log::error!("cannot listen for Ctrl-C: {e}"))
        .ok();
    Box::pin(async move {
        if let Some(mut listener) = listener
            && listener.recv().await.is_some()
        {
            return;
        }
        std::future::pending().await
    })
}

/// Stop the script after a Ctrl-C, then exit.
async fn stop_script_and_exit(supervisor: &macrocosim::runner::ScriptSupervisor) -> ! {
    log::info!("Interrupted; stopping the script");
    supervisor.shutdown().await;
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare `macrocosim plant.py` invocation keeps the documented
    /// defaults: UI on 8801, no state dir, the script positional.
    /// CI always boots with --ephemeral-ports and --state-dir, so
    /// nothing else exercises these defaults.
    #[test]
    fn bare_invocation_keeps_the_documented_defaults() {
        let a = Args::parse_from(["macrocosim", "plant.py"]);
        assert_eq!(a.ui_port, 8801);
        assert!(a.state_dir.is_none());
        assert!(!a.ephemeral_ports);
        assert_eq!(a.scripts, vec![std::path::PathBuf::from("plant.py")]);
        assert!(a.topology.is_none() && a.enterprise_id.is_none());
    }

    #[test]
    fn a_topology_takes_no_script() {
        let a = Args::parse_from(["macrocosim", "--topology", "t.json"]);
        assert_eq!(a.topology, Some(PathBuf::from("t.json")));
        assert!(Args::try_parse_from(["macrocosim", "--topology", "t.json", "x.py"]).is_err());
    }

    #[test]
    fn one_python_script_runs_alone() {
        let paths = |ps: &[&str]| ps.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(script_arg(&[]).unwrap(), None);
        assert_eq!(
            script_arg(&paths(&["plant.py"])).unwrap(),
            Some(PathBuf::from("plant.py"))
        );
        assert_eq!(
            script_arg(&paths(&["a.py", "b.py"])).unwrap_err(),
            "one Python script per engine; got a.py, b.py"
        );
        assert_eq!(
            script_arg(&paths(&["topology.json"])).unwrap_err(),
            "a script is a Python file (.py); got topology.json"
        );
        assert!(Args::try_parse_from(["macrocosim", "--topology", "t.json", "a.py"]).is_err());
        let a = Args::parse_from(["macrocosim", "--python", "/opt/py/bin/python", "a.py"]);
        assert_eq!(a.python, Some(PathBuf::from("/opt/py/bin/python")));
    }

    #[test]
    fn the_enterprise_flags_reach_the_settings() {
        let a = Args::parse_from([
            "macrocosim",
            "--enterprise-id",
            "42",
            "--assets-addr",
            "[::1]:9911",
            "--dispatch-addr",
            "127.0.0.1:8911",
            "--request-lifetime",
            "30",
            "--augment-lifetime",
            "2.5",
        ]);
        assert_eq!(a.enterprise_id, Some(42));
        assert_eq!(a.assets_addr, Some("[::1]:9911".parse().unwrap()));
        assert_eq!(a.dispatch_addr, Some("127.0.0.1:8911".parse().unwrap()));
        assert_eq!(a.request_lifetime, Some(Duration::from_secs(30)));
        assert_eq!(a.augment_lifetime, Some(Duration::from_millis(2500)));
        assert!(Args::try_parse_from(["macrocosim", "--request-lifetime", "-1"]).is_err());

        let m = metadata_of(&a);
        assert_eq!(m.enterprise_id, 42);
        assert_eq!(m.assets_socket_addr, "[::1]:9911");
        assert_eq!(m.dispatch_socket_addr, "127.0.0.1:8911");
        assert_eq!(m.default_request_lifetime, Duration::from_secs(30));
        assert_eq!(m.default_augment_lifetime, Duration::from_millis(2500));
    }

    /// With no enterprise flags, the settings are the defaults.
    #[test]
    fn no_enterprise_flags_keep_the_defaults() {
        let m = metadata_of(&Args::parse_from(["macrocosim"]));
        let d = Metadata::default();
        assert_eq!(m.enterprise_id, d.enterprise_id);
        assert_eq!(m.assets_socket_addr, d.assets_socket_addr);
        assert_eq!(m.dispatch_socket_addr, d.dispatch_socket_addr);
        assert_eq!(m.default_request_lifetime, d.default_request_lifetime);
        assert_eq!(m.default_augment_lifetime, d.default_augment_lifetime);
    }

    /// A `.lisp` argument and a state directory's `enterprise.lisp`
    /// and `microgrids/*.lisp` are found, other files are not.
    #[test]
    fn lisp_inputs_finds_the_lisp_arguments_and_state() {
        let dir = tempfile::TempDir::with_prefix("mc-old-inputs-").unwrap();
        let state = dir.path();
        assert!(lisp_inputs(&[PathBuf::from("plant.py")], state).is_empty());
        std::fs::create_dir(state.join("microgrids")).unwrap();
        std::fs::write(state.join("microgrids/README"), "").unwrap();
        assert!(lisp_inputs(&[], state).is_empty());
        let args = [PathBuf::from("site.lisp"), PathBuf::from("a.py")];
        assert_eq!(
            lisp_inputs(&args, state).refusal(),
            "macrocosim 0.2 no longer reads Lisp; support ended with 0.1.\n\
             Found: site.lisp. Port these files to a Python script (see docs/python-scripts.md)."
        );
        std::fs::write(state.join("microgrids/2.lisp"), "").unwrap();
        std::fs::write(state.join("microgrids/1.lisp"), "").unwrap();
        std::fs::write(state.join("enterprise.lisp"), "").unwrap();
        let found = lisp_inputs(&args, state);
        assert_eq!(
            found,
            LispInputs {
                args: vec![PathBuf::from("site.lisp")],
                in_state_dir: vec![
                    state.join("enterprise.lisp"),
                    state.join("microgrids/1.lisp"),
                    state.join("microgrids/2.lisp"),
                ],
            }
        );
        assert!(
            found.refusal().ends_with(
                "(see docs/python-scripts.md),\nor pass --state-dir to start from another directory."
            ),
            "a state-dir file names the flag"
        );
    }

    /// An argument in the state directory is still an argument: the
    /// refusal does not point at `--state-dir` for it.
    #[test]
    fn a_lisp_argument_in_the_state_dir_does_not_name_the_flag() {
        let dir = tempfile::TempDir::with_prefix("mc-old-inputs-").unwrap();
        let found = lisp_inputs(&[dir.path().join("site.lisp")], dir.path());
        assert!(
            !found.refusal().contains("--state-dir"),
            "{}",
            found.refusal()
        );
    }
}
