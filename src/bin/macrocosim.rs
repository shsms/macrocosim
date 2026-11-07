//! Headless macrocosim simulator: run a Python script, boot the
//! microgrids of a topology document (`--topology`) or of Lisp
//! scripts, spawn the physics tick, serve the Microgrid gRPC API and
//! the UI.
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use clap::Parser;
use macrocosim::{
    assets_server::AssetsServer,
    dispatch_server::DispatchServer,
    lisp::Config,
    proto::assets::platform_assets_server::PlatformAssetsServer as AssetsGrpcServer,
    proto::dispatch::microgrid_dispatch_service_server::MicrogridDispatchServiceServer as DispatchGrpcServer,
    runtime::{MicrogridRuntimes, RuntimeOptions, RuntimeStatus},
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
    /// A Python script (`plant.py`) to run, or Lisp scripts to
    /// evaluate at boot, in order. A Python script runs alone, in a
    /// child process that is started again whenever a `.py` file in
    /// its directory tree changes. With none, the engine boots bare:
    /// UI + REPL up, empty registry — load a topology on demand with
    /// `(load "…")` from the REPL or the Microgrids tab.
    scripts: Vec<PathBuf>,

    /// Boot the microgrids of this topology document (JSON), each
    /// built from its spec with the file as its source. Not with a
    /// script.
    #[arg(long, value_name = "PATH", conflicts_with = "scripts")]
    topology: Option<PathBuf>,

    /// The enterprise id every microgrid reports. Overrides
    /// `enterprise.lisp`.
    #[arg(long, value_name = "ID")]
    enterprise_id: Option<u64>,

    /// Address of the PlatformAssets gRPC server (default
    /// `[::1]:9900`). Overrides `enterprise.lisp`.
    #[arg(long, value_name = "ADDR")]
    assets_addr: Option<SocketAddr>,

    /// Address of the MicrogridDispatchService gRPC server (default
    /// `[::1]:8900`). Overrides `enterprise.lisp`.
    #[arg(long, value_name = "ADDR")]
    dispatch_addr: Option<SocketAddr>,

    /// Request lifetime of a setpoint that carries none, in seconds
    /// (default 60). Overrides `enterprise.lisp`.
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    request_lifetime: Option<Duration>,

    /// Lifetime of a bounds augmentation that carries none, in seconds
    /// (default 5). Overrides `enterprise.lisp`.
    #[arg(long, value_name = "SECONDS", value_parser = parse_seconds)]
    augment_lifetime: Option<Duration>,

    /// Anchor directory for persistent state (enterprise.lisp,
    /// snapshots/, managed microgrid files) and for relative
    /// `(load …)` paths. Defaults to the current directory.
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
    /// one JSON line — to `--emit-endpoints=PATH`, or stdout if the flag
    /// is given bare. Requires `=` so it can't swallow the `config`
    /// positional. The machine-readable readiness signal.
    #[arg(long, value_name = "PATH", num_args = 0..=1, require_equals = true, default_missing_value = "-")]
    emit_endpoints: Option<String>,
}

/// A non-negative, finite number of seconds.
fn parse_seconds(text: &str) -> Result<Duration, String> {
    let s: f64 = text.parse().map_err(|e| format!("{e}"))?;
    Duration::try_from_secs_f64(s)
        .map_err(|_| format!("{s} is not a non-negative number of seconds"))
}

/// Lay the enterprise flags over the settings `enterprise.lisp` gave.
fn apply_enterprise_flags(args: &Args, engine: &macrocosim::engine::Engine) {
    engine.update_metadata(|m| {
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
    });
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
    let lock = engine.create_lock();
    let _structural = lock.lock().await;
    let created = topology::create_doc(engine, runtimes, doc, Some(source))
        .await
        .unwrap_or_else(|e| fail(e.to_string()));
    for c in created {
        log::info!("Microgrid #{} built from {}", c.id, path.display());
    }
}

/// Split the positional arguments into the one Python script and the
/// Lisp scripts, refusing a second `.py` or a `.py` beside anything
/// else.
fn split_scripts(paths: &[PathBuf]) -> Result<(Option<PathBuf>, Vec<PathBuf>), String> {
    let (py, lisp): (Vec<&PathBuf>, Vec<&PathBuf>) = paths
        .iter()
        .partition(|p| p.extension().is_some_and(|e| e == "py"));
    let shown = |ps: &[&PathBuf]| {
        ps.iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match py.as_slice() {
        [] => Ok((None, lisp.into_iter().cloned().collect())),
        [script] if lisp.is_empty() => Ok((Some((*script).clone()), Vec::new())),
        [_] => Err(format!(
            "a Python script runs alone; it cannot be combined with {}",
            shown(&lisp)
        )),
        _ => Err(format!("one Python script per engine; got {}", shown(&py))),
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
    macrocosim::tokio_runtime::build()
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

    let (python_script, lisp_scripts) = split_scripts(&args.scripts).unwrap_or_else(|e| {
        log::error!("{e}");
        std::process::exit(1);
    });
    let python_script = python_script.map(|p| {
        p.canonicalize().unwrap_or_else(|e| {
            log::error!("Script {}: {e}", p.display());
            std::process::exit(1);
        })
    });
    let scripts: Vec<String> = lisp_scripts
        .iter()
        .map(|p| {
            p.to_str().map(str::to_owned).unwrap_or_else(|| {
                log::error!("Script path is not valid UTF-8: {}", p.display());
                std::process::exit(1);
            })
        })
        .collect();
    if let Some(script) = &python_script {
        log::info!("Running script {}", script.display());
    } else if scripts.is_empty() {
        log::info!("No boot scripts given — starting a bare engine");
    } else {
        log::info!("Evaluating boot script(s): {}", scripts.join(", "));
    }
    let config = Config::new_with(&scripts, args.state_dir.clone()).unwrap_or_else(|e| {
        log::error!("Failed to eval boot scripts:\n{e}");
        std::process::exit(1);
    });
    let engine = config.engine().clone();
    apply_enterprise_flags(&args, &engine);
    if let Some(python) = &args.python {
        engine.set_python(python.clone());
    }
    // A script needs a usable interpreter; without one, say how to
    // get one before anything binds.
    let python = match &python_script {
        Some(_) => Some(engine.python().await.unwrap_or_else(|e| {
            log::error!("{e}");
            std::process::exit(1);
        })),
        None => None,
    };

    // Every microgrid runtime, boot-time and later, starts through
    // MicrogridRuntimes. The listener subscribes before the file
    // watcher starts, so no reload can register a microgrid unseen.
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
    let _listener = runtimes.spawn_registration_listener().await;
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

    // Watch the config file in the background so saves trigger reload.
    tokio::spawn(config.clone().watch());

    // Bind the UI, assets and dispatch listeners up front so
    // ephemeral (:0) ports resolve to real ones before the endpoints
    // are emitted. Hosts stay loopback (UI 127.0.0.1, gRPC [::1]); a
    // routable --*-bind + the hardening it gates is a follow-up
    // (todo §D3).
    let eph = args.ephemeral_ports;
    let ui_port = if eph { 0 } else { args.ui_port };
    let (ui_listener, ui_addr) =
        bind_or_exit(SocketAddr::from((Ipv4Addr::LOCALHOST, ui_port)), "UI").await;
    let ui_config = config.clone();

    // Assets + dispatch: single enterprise-wide sockets (defaults
    // [::1]:9900 / [::1]:8900, lisp-overridable). --ephemeral-ports
    // zeroes the port for an OS-chosen one.
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
    // status instead. (The lisp refresh loop lives inside Config and
    // stays fire-and-forget for now.)
    let mut tasks: tokio::task::JoinSet<&'static str> = tokio::task::JoinSet::new();
    log::info!("Macrocosim UI listening on http://{ui_addr}");
    let ui_runtimes = runtimes.clone();
    let ui_runner = supervisor.clone();
    tasks.spawn(async move {
        if let Err(e) =
            ui::serve_with_listener(ui_listener, ui_config, ui_runtimes, ui_runner).await
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

    /// A bare `macrocosim some.lisp` invocation keeps the documented
    /// defaults: UI on 8801, no state dir, the script positional.
    /// CI always boots with --ephemeral-ports and --state-dir, so
    /// nothing else exercises these defaults.
    #[test]
    fn bare_invocation_keeps_the_documented_defaults() {
        let a = Args::parse_from(["macrocosim", "some.lisp"]);
        assert_eq!(a.ui_port, 8801);
        assert!(a.state_dir.is_none());
        assert!(!a.ephemeral_ports);
        assert_eq!(a.scripts, vec![std::path::PathBuf::from("some.lisp")]);
        assert!(a.topology.is_none() && a.enterprise_id.is_none());
    }

    #[test]
    fn a_topology_takes_no_script() {
        let a = Args::parse_from(["macrocosim", "--topology", "t.json"]);
        assert_eq!(a.topology, Some(PathBuf::from("t.json")));
        assert!(Args::try_parse_from(["macrocosim", "--topology", "t.json", "x.lisp"]).is_err());
    }

    #[test]
    fn one_python_script_runs_alone() {
        let paths = |ps: &[&str]| ps.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            split_scripts(&paths(&["plant.py"])).unwrap(),
            (Some(PathBuf::from("plant.py")), vec![])
        );
        assert_eq!(
            split_scripts(&paths(&["a.lisp", "b.lisp"])).unwrap(),
            (None, paths(&["a.lisp", "b.lisp"]))
        );
        assert_eq!(
            split_scripts(&paths(&["a.py", "b.py"])).unwrap_err(),
            "one Python script per engine; got a.py, b.py"
        );
        assert_eq!(
            split_scripts(&paths(&["a.py", "b.lisp"])).unwrap_err(),
            "a Python script runs alone; it cannot be combined with b.lisp"
        );
        assert!(Args::try_parse_from(["macrocosim", "--topology", "t.json", "a.py"]).is_err());
        let a = Args::parse_from(["macrocosim", "--python", "/opt/py/bin/python", "a.py"]);
        assert_eq!(a.python, Some(PathBuf::from("/opt/py/bin/python")));
    }

    #[test]
    fn the_enterprise_flags_parse() {
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
    }
}
