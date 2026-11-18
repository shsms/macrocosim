//! Convenience CLI for poking at a running macrocosim server.
//!
//! --microgrid-id selects the microgrid for the gRPC commands,
//! `dispatch`, `pool` and `dashboard`; the default is the lowest id
//! `GET /api/microgrids` reports.
//!
//! gRPC commands connect to the microgrid's server as the UI server
//! lists it. --addr names a gRPC server directly; with
//! --microgrid-id the lookup still runs and the two must agree.
//!
//! gRPC commands (the microgrid's own server, or --addr):
//!   macroctl info
//!   macroctl list
//!   macroctl list --category battery
//!   macroctl tree
//!   macroctl stream 1001
//!   macroctl stream 1001 --samples 5 --json
//!   macroctl set-power 1001 8000
//!   macroctl set-power 1001 -5000 --lifetime-s 30   # negative → discharge
//!   macroctl set-reactive-power 1001 3000 --lifetime-s 30
//!   macroctl augment-bounds 1001 --lower-w -15000 --upper-w 15000 --lifetime-s 60
//!   macroctl augment-reactive-bounds 1001 --lower-var -3000 --upper-var 3000 --lifetime-s 60
//!
//! Assets API command (no simulator; the API's URL, key and secret from --url /
//! --auth-key / --sign-secret or $ASSETS_API_URL / $ASSETS_API_AUTH_KEY /
//! $ASSETS_API_SIGN_SECRET):
//!   macroctl from-assets 42 -o site_42.py

use std::collections::{BTreeMap, BTreeSet};

use clap::{Parser, Subcommand, ValueEnum};
use tonic::transport::Channel;

use macrocosim::assets_client::sign::Credentials;
use macrocosim::proto::common::metrics::Metric;
use macrocosim::proto::common::microgrid::electrical_components::{
    ElectricalComponent, ElectricalComponentCategory,
    electrical_component_category_specific_info::Kind,
};
use macrocosim::proto::dispatch::microgrid_dispatch_service_client::MicrogridDispatchServiceClient;
use macrocosim::proto::dispatch::{
    CreateMicrogridDispatchRequest, DeleteMicrogridDispatchRequest, DispatchData,
    GetMicrogridDispatchRequest, ListMicrogridDispatchesRequest, UpdateMicrogridDispatchRequest,
    update_microgrid_dispatch_request::DispatchUpdate,
};
use macrocosim::proto::microgrid::microgrid_client::MicrogridClient;
use macrocosim::proto::microgrid::{
    AugmentElectricalComponentBoundsRequest, ListElectricalComponentConnectionsRequest,
    ListElectricalComponentsRequest, PowerType, ReceiveElectricalComponentTelemetryStreamRequest,
    SetElectricalComponentPowerRequest,
};
use macrocosim::python::Interpreter;
use macrocosim::sim::dispatch::{json_to_struct, parse_target, struct_to_json, target_to_string};
use macrocosim::spec::{SCHEMA_VERSION, TopologyDoc};

#[derive(Parser, Debug)]
#[command(
    name = "macroctl",
    about = "Macrocosim microgrid client",
    version,
    propagate_version = true
)]
struct Cli {
    /// gRPC endpoint of the microgrid's server, named directly. With
    /// `--microgrid-id` the lookup through the UI server still runs
    /// and the two must agree.
    #[arg(long, global = true)]
    addr: Option<String>,

    /// HTTP endpoint of the simulator's UI server. Used by the HTTP
    /// subcommands and to look up a microgrid's gRPC server.
    #[arg(long, default_value = "http://127.0.0.1:8801", global = true)]
    ui_addr: String,

    /// gRPC endpoint of the (enterprise-wide) MicrogridDispatchService.
    /// Used by the `dispatch` subcommand; separate from `--addr` since
    /// the dispatch service binds its own shared port.
    #[arg(long, default_value = "http://[::1]:8900", global = true)]
    dispatch_addr: String,

    /// Emit JSON instead of human-friendly output where applicable.
    #[arg(long, global = true)]
    json: bool,

    /// Microgrid for the gRPC commands, `dispatch`, `pool` and
    /// `dashboard`. Default: the lowest id `GET /api/microgrids`
    /// reports.
    #[arg(long, global = true)]
    microgrid_id: Option<u64>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Show microgrid metadata.
    Info,

    /// List electrical components.
    List {
        /// Filter by category.
        #[arg(long)]
        category: Option<Category>,
        /// Filter by component ID (repeatable).
        #[arg(long = "component-id")]
        ids: Vec<u64>,
    },

    /// List electrical-component connections.
    Connections {
        /// Filter by source ID (repeatable).
        #[arg(long = "from")]
        from: Vec<u64>,
        /// Filter by destination ID (repeatable).
        #[arg(long = "to")]
        to: Vec<u64>,
    },

    /// Pretty-print the topology as a tree.
    Tree,

    /// Stream telemetry samples for a component.
    Stream {
        /// Component ID.
        id: u64,
        /// Stop after N samples (default: stream forever, Ctrl+C to exit).
        #[arg(long)]
        samples: Option<usize>,
        /// Subscribe only to specific metrics. Repeatable. Names are
        /// case-insensitive and match the proto enum either with or
        /// without the `METRIC_` prefix (e.g. `dc_power` or
        /// `METRIC_DC_POWER`). Omitting the flag streams every metric.
        #[arg(long = "metric")]
        metrics: Vec<String>,
    },

    /// Set the active power set-point on a component.
    SetPower {
        /// Component ID.
        id: u64,
        /// Active power in watts. Negative = discharge.
        #[arg(allow_hyphen_values = true)]
        power_w: f32,
        /// Request lifetime in whole seconds (10..=900).
        #[arg(long = "lifetime-s")]
        lifetime_s: Option<u64>,
    },

    /// Set the reactive power set-point on a component.
    SetReactivePower {
        /// Component ID.
        id: u64,
        /// Reactive power in VAr.
        #[arg(allow_hyphen_values = true)]
        power_var: f32,
        /// Request lifetime in whole seconds (10..=900).
        #[arg(long = "lifetime-s")]
        lifetime_s: Option<u64>,
    },

    /// Augment a component's active-power bounds for a limited time.
    AugmentBounds {
        /// Component ID.
        id: u64,
        /// New lower bound in watts.
        #[arg(long = "lower-w", allow_hyphen_values = true)]
        lower_w: f32,
        /// New upper bound in watts.
        #[arg(long = "upper-w", allow_hyphen_values = true)]
        upper_w: f32,
        /// Request lifetime in whole seconds (10..=900).
        #[arg(long = "lifetime-s", default_value_t = 60)]
        lifetime_s: u64,
    },

    /// Augment a component's reactive-power bounds for a limited
    /// time. The server validates the bounds against the live Q
    /// envelope at the component's current P.
    AugmentReactiveBounds {
        /// Component ID.
        id: u64,
        /// New lower bound in VAr.
        #[arg(long = "lower-var", allow_hyphen_values = true)]
        lower_var: f32,
        /// New upper bound in VAr.
        #[arg(long = "upper-var", allow_hyphen_values = true)]
        upper_var: f32,
        /// Request lifetime in whole seconds (10..=900).
        #[arg(long = "lifetime-s", default_value_t = 60)]
        lifetime_s: u64,
    },

    /// Create / list / pause / resume / delete dispatches via the
    /// MicrogridDispatchService gRPC API (default --dispatch-addr
    /// http://[::1]:8900).
    #[command(subcommand)]
    Dispatch(DispatchCmd),

    /// Read the loopback BatteryPool / PvPool aggregates. Output
    /// matches the `Sample<Q>` shape frequenz-microgrid's pool
    /// streams yield, so the output is paste-into-a-bug-report
    /// compatible with what a downstream control app sees.
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },

    /// Terminal-resident pulse bar — one line per second showing
    /// component health, the grid / PV / battery readouts, and the
    /// loopback Microgrid status. Useful in a tmux pane next to
    /// the editor. Polls the UI HTTP surface; gRPC isn't touched.
    Dashboard {
        /// Stream forever instead of printing a single snapshot
        /// and exiting.
        #[arg(long)]
        tail: bool,
        /// Polling cadence in seconds (only with --tail); at least 0.1.
        #[arg(long = "interval-s", value_parser = parse_secs, default_value = "1")]
        interval: std::time::Duration,
    },

    /// Fetch a microgrid from an Assets API and write a Python module that
    /// declares it, as the UI's "Save as .py" does. With --json, write its
    /// topology document instead. Talks to no simulator.
    ///
    /// The module keeps the API's microgrid and component ids. Components and
    /// connections outside their operational lifetime are left out. So are
    /// components the simulator has no model for; each of their parents is
    /// connected to each of their children. A microgrid with more than one grid
    /// connection point is refused, and a component not under the grid
    /// connection point is left out.
    ///
    /// A hybrid inverter, and an inverter of no type over a battery, are left
    /// out with the components only such inverters lead to. The module
    /// (not --json) carries them commented out under the first of the
    /// inverter's parents that is left and can have components under it; one
    /// with no such parent is not written. stderr names every component left
    /// out, and why.
    FromAssets {
        /// The microgrid's id in the Assets API. The global --microgrid-id is
        /// refused here.
        #[arg(value_name = "MICROGRID_ID")]
        id: u64,
        /// The Assets API, as `grpc://host[:port][?ssl=false]`.
        #[arg(long, env = "ASSETS_API_URL", hide_env_values = true)]
        url: String,
        /// The API key.
        #[arg(long, env = "ASSETS_API_AUTH_KEY", hide_env_values = true)]
        auth_key: String,
        /// The key's signing secret. A flag shows to every user in `ps`; the
        /// environment variable shows only to you and root.
        #[arg(long, env = "ASSETS_API_SIGN_SECRET", hide_env_values = true)]
        sign_secret: String,
        /// Write the module to this file instead of stdout.
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
        /// The Python to write the module with: 3.11 or later, with the
        /// macrocosim package of this binary's version. Default:
        /// $MACROCOSIM_PYTHON, a python next to this binary, then python3 or
        /// python on PATH. Not used with --json.
        #[arg(long)]
        python: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum DispatchCmd {
    /// List a microgrid's dispatches (newest created first).
    List,
    /// Show a single dispatch.
    Get {
        /// Dispatch id.
        dispatch_id: u64,
    },
    /// Create a dispatch. TARGET is a comma-separated list of either
    /// component categories (battery, grid, meter, inverter,
    /// ev_charger, chp, steam_boiler) or numeric component ids
    /// (e.g. "1,2,3").
    Create {
        /// Dispatch type the downstream actor keys on.
        #[arg(name = "type")]
        type_: String,
        /// Target components.
        target: String,
        /// Duration in seconds; omit for an indefinite dispatch.
        #[arg(long = "duration-s")]
        duration_s: Option<u32>,
        /// JSON payload object, e.g. '{"target_power_w": 5000}'.
        #[arg(long)]
        payload: Option<String>,
        /// Create the dispatch inactive (default: active).
        #[arg(long)]
        inactive: bool,
        /// Mark the dispatch as a dry run.
        #[arg(long)]
        dry_run: bool,
        /// RFC3339 start time; omit to start immediately (now).
        #[arg(long)]
        start: Option<String>,
    },
    /// Pause a dispatch (set it inactive).
    Pause {
        /// Dispatch id.
        dispatch_id: u64,
    },
    /// Resume a paused dispatch (set it active).
    Resume {
        /// Dispatch id.
        dispatch_id: u64,
    },
    /// Delete a dispatch.
    Delete {
        /// Dispatch id.
        dispatch_id: u64,
    },
}

#[derive(Subcommand, Debug)]
enum PoolCmd {
    /// Battery pool — power + active-power envelope.
    Battery {
        /// Stream forever; otherwise prints one snapshot and exits.
        #[arg(long)]
        stream: bool,
    },
    /// PV pool — current aggregate active power. (frequenz-microgrid
    /// 0.4.1 has no PvPool envelope, so this is just `pv_power`.)
    Pv {
        #[arg(long)]
        stream: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Category {
    Grid,
    Meter,
    Inverter,
    Battery,
    EvCharger,
    Chp,
    SteamBoiler,
}

impl Category {
    fn to_proto(self) -> ElectricalComponentCategory {
        match self {
            Self::Grid => ElectricalComponentCategory::GridConnectionPoint,
            Self::Meter => ElectricalComponentCategory::Meter,
            Self::Inverter => ElectricalComponentCategory::Inverter,
            Self::Battery => ElectricalComponentCategory::Battery,
            Self::EvCharger => ElectricalComponentCategory::EvCharger,
            Self::Chp => ElectricalComponentCategory::Chp,
            Self::SteamBoiler => ElectricalComponentCategory::SteamBoiler,
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.cmd {
        // HTTP-backed commands dispatch directly: they use the HTTP
        // client (`dispatch` only to look up the microgrid id, then
        // talks to the dispatch service) and need no gRPC channel to
        // the Microgrid service, so a readout never waits on a gRPC
        // connect.
        Cmd::Dispatch(d) => {
            let http = reqwest::Client::new();
            // Only the id lookup needs the UI server; an explicit
            // --microgrid-id skips it.
            let mg = resolve_microgrid_id(&http, &cli.ui_addr, cli.microgrid_id)
                .await
                .map_err(|e| format!("{e}; pass --microgrid-id"))?;
            run_dispatch(d, &cli.dispatch_addr, mg, cli.json).await
        }
        Cmd::Dashboard { tail, interval } => {
            run_dashboard(&cli.ui_addr, cli.microgrid_id, tail, interval).await
        }
        Cmd::Pool { cmd } => run_pool(cmd, &cli.ui_addr, cli.microgrid_id, cli.json).await,
        Cmd::FromAssets {
            id,
            url,
            auth_key,
            sign_secret,
            output,
            python,
        } => {
            if cli.microgrid_id.is_some() {
                return Err("from-assets takes the Assets API's microgrid id as its \
                            argument, not --microgrid-id"
                    .into());
            }
            let creds = Credentials::new(auth_key, sign_secret)?;
            run_from_assets(&url, creds, id, output, python, cli.json).await
        }
        // Everything else talks the Microgrid gRPC API: connect
        // once, then dispatch.
        cmd => {
            let http = reqwest::Client::new();
            let endpoint =
                grpc_endpoint(&http, &cli.ui_addr, cli.addr.as_deref(), cli.microgrid_id).await?;
            let mut client = MicrogridClient::connect(endpoint).await?;
            if let Some(req) = set_power_request(&cmd) {
                return cmd_set_power(&mut client, req).await;
            }
            if let Some(req) = augment_request(&cmd) {
                return cmd_augment(&mut client, req).await;
            }
            match cmd {
                Cmd::Info => cmd_info(&mut client, cli.json).await,
                Cmd::List { category, ids } => cmd_list(&mut client, category, ids, cli.json).await,
                Cmd::Connections { from, to } => {
                    cmd_connections(&mut client, from, to, cli.json).await
                }
                Cmd::Tree => cmd_tree(&mut client).await,
                Cmd::Stream {
                    id,
                    samples,
                    metrics,
                } => cmd_stream(&mut client, id, samples, metrics, cli.json).await,
                _ => unreachable!("HTTP-backed and write commands are dispatched above"),
            }
        }
    }
}

/// `macroctl from-assets`: fetch microgrid `id`, note what was left out on
/// stderr, and write its module (or with `json`, its topology document) to
/// `output` or stdout.
async fn run_from_assets(
    url: &str,
    creds: Credentials,
    id: u64,
    output: Option<std::path::PathBuf>,
    python: Option<std::path::PathBuf>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let interpreter = Interpreter::default();
    if let Some(path) = python {
        interpreter.set_override(path);
    }
    let resolve = async {
        if json {
            Ok(None)
        } else {
            interpreter.resolve().await.map(Some)
        }
    };
    // The interpreter is checked during the fetch, so a missing one fails at
    // once.
    let fetch = macrocosim::assets_client::fetch(url, creds, id);
    let (fetched, python) = tokio::try_join!(fetch, resolve)?;
    for note in &fetched.left_out {
        eprintln!("note: {note}");
    }
    let text = match python {
        None => {
            let doc = TopologyDoc {
                schema_version: SCHEMA_VERSION,
                microgrids: vec![fetched.spec],
            };
            serde_json::to_string_pretty(&doc)? + "\n"
        }
        Some(python) => {
            let (input, notes) = fetched.module_input();
            let text = macrocosim::python::write_module(&python, &input)
                .await
                .map_err(|e| format!("the export failed: {e}"))?;
            for note in notes {
                eprintln!("note: {note}");
            }
            text
        }
    };
    match output {
        Some(path) => std::fs::write(&path, text)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?,
        None => print!("{text}"),
    }
    Ok(())
}

/// A clap value parser for a non-negative, finite number of seconds.
fn parse_secs(s: &str) -> Result<std::time::Duration, String> {
    let secs: f64 = s.parse().map_err(|e| format!("{e}"))?;
    std::time::Duration::try_from_secs_f64(secs).map_err(|e| format!("{e}"))
}

fn str_err(e: String) -> Box<dyn std::error::Error> {
    e.into()
}

fn rfc3339_to_ts(s: &str) -> Result<prost_types::Timestamp, Box<dyn std::error::Error>> {
    let dt = chrono::DateTime::parse_from_rfc3339(s)?.with_timezone(&chrono::Utc);
    Ok(macrocosim::proto_conv::datetime_to_ts(dt))
}

/// One-line dispatch summary for `dispatch list`.
fn dispatch_summary(d: &macrocosim::proto::dispatch::Dispatch) -> String {
    let id = d.metadata.as_ref().map(|m| m.dispatch_id).unwrap_or(0);
    let data = d.data.as_ref();
    let ty = data.map(|x| x.r#type.as_str()).unwrap_or("?");
    let active = data.map(|x| x.is_active).unwrap_or(false);
    let dry = if data.map(|x| x.is_dry_run).unwrap_or(false) {
        " dry-run"
    } else {
        ""
    };
    let status = if active { "active" } else { "inactive" };
    let dur = data
        .and_then(|x| x.duration)
        .map(|s| format!("{s}s"))
        .unwrap_or_else(|| "indefinite".to_string());
    let target = target_to_string(data.and_then(|x| x.target.as_ref()));
    format!("#{id}  {ty}  [{status}{dry}]  dur={dur}  target={target}")
}

/// Multi-line dispatch detail for `dispatch get` / `create`.
fn print_dispatch_detail(d: &macrocosim::proto::dispatch::Dispatch) {
    println!("{}", dispatch_summary(d));
    if let Some(data) = d.data.as_ref() {
        if let Some(st) = data.start_time.as_ref() {
            println!("  start   = {}", format_ts(st));
        }
        if let Some(p) = data.payload.as_ref() {
            println!("  payload = {}", struct_to_json(p));
        }
    }
    if let Some(end) = d.metadata.as_ref().and_then(|m| m.end_time.as_ref()) {
        println!("  end     = {}", format_ts(end));
    }
}

/// Pause (`active=false`) / resume (`active=true`) a dispatch via an
/// `is_active` field-mask update.
async fn set_active_dispatch(
    client: &mut MicrogridDispatchServiceClient<Channel>,
    microgrid_id: u64,
    dispatch_id: u64,
    active: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = client
        .update_microgrid_dispatch(UpdateMicrogridDispatchRequest {
            microgrid_id,
            dispatch_id,
            update_mask: Some(prost_types::FieldMask {
                paths: vec!["is_active".to_string()],
            }),
            update: Some(DispatchUpdate {
                is_active: Some(active),
                ..Default::default()
            }),
        })
        .await?
        .into_inner();
    if json {
        println!("{resp:#?}");
    } else {
        let verb = if active { "resumed" } else { "paused" };
        println!("{verb} dispatch {dispatch_id} (microgrid {microgrid_id})");
    }
    Ok(())
}

async fn run_dispatch(
    cmd: DispatchCmd,
    addr: &str,
    microgrid_id: u64,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = MicrogridDispatchServiceClient::connect(addr.to_string()).await?;
    match cmd {
        DispatchCmd::List => {
            let resp = client
                .list_microgrid_dispatches(ListMicrogridDispatchesRequest {
                    microgrid_id,
                    filter: None,
                    sort_options: None,
                    pagination_params: None,
                })
                .await?
                .into_inner();
            if json {
                println!("{resp:#?}");
                return Ok(());
            }
            if resp.dispatches.is_empty() {
                println!("(no dispatches for microgrid {microgrid_id})");
            }
            for d in &resp.dispatches {
                println!("{}", dispatch_summary(d));
            }
            Ok(())
        }
        DispatchCmd::Get { dispatch_id } => {
            let resp = client
                .get_microgrid_dispatch(GetMicrogridDispatchRequest {
                    microgrid_id,
                    dispatch_id,
                })
                .await?
                .into_inner();
            if json {
                println!("{resp:#?}");
                return Ok(());
            }
            match resp.dispatch.as_ref() {
                Some(d) => print_dispatch_detail(d),
                None => println!("(dispatch {dispatch_id} not found)"),
            }
            Ok(())
        }
        DispatchCmd::Create {
            type_,
            target,
            duration_s,
            payload,
            inactive,
            dry_run,
            start,
        } => {
            let target = parse_target(&target).map_err(str_err)?;
            let payload = match payload {
                Some(s) => {
                    let value: serde_json::Value = serde_json::from_str(&s)?;
                    Some(json_to_struct(&value).map_err(str_err)?)
                }
                None => None,
            };
            let start_immediately = start.is_none();
            let start_time = match start.as_deref() {
                Some(s) => Some(rfc3339_to_ts(s)?),
                None => None,
            };
            let data = DispatchData {
                r#type: type_,
                start_time,
                duration: duration_s,
                target: Some(target),
                is_active: !inactive,
                is_dry_run: dry_run,
                payload,
                recurrence: None,
            };
            let resp = client
                .create_microgrid_dispatch(CreateMicrogridDispatchRequest {
                    microgrid_id,
                    dispatch_data: Some(data),
                    // None (not Some(false)) when a start time was
                    // given — absent is the proto's "not requested".
                    start_immediately: start_immediately.then_some(true),
                })
                .await?
                .into_inner();
            if json {
                println!("{resp:#?}");
                return Ok(());
            }
            match resp.dispatch.as_ref() {
                Some(d) => {
                    println!("created:");
                    print_dispatch_detail(d);
                }
                None => println!("(no dispatch returned)"),
            }
            Ok(())
        }
        DispatchCmd::Pause { dispatch_id } => {
            set_active_dispatch(&mut client, microgrid_id, dispatch_id, false, json).await
        }
        DispatchCmd::Resume { dispatch_id } => {
            set_active_dispatch(&mut client, microgrid_id, dispatch_id, true, json).await
        }
        DispatchCmd::Delete { dispatch_id } => {
            let resp = client
                .delete_microgrid_dispatch(DeleteMicrogridDispatchRequest {
                    microgrid_id,
                    dispatch_id,
                })
                .await?
                .into_inner();
            if json {
                println!("{resp:#?}");
            } else {
                println!(
                    "deleted dispatch {} (microgrid {})",
                    resp.dispatch_id, resp.microgrid_id
                );
            }
            Ok(())
        }
    }
}

async fn run_pool(
    cmd: PoolCmd,
    ui_addr: &str,
    microgrid_id: Option<u64>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    let mg = resolve_microgrid_id(&http, ui_addr, microgrid_id).await?;
    let (kind, stream) = match cmd {
        PoolCmd::Battery { stream } => ("battery", stream),
        PoolCmd::Pv { stream } => ("pv", stream),
    };
    loop {
        // Same transient-error tolerance as dashboard --tail.
        match build_pool_line(&http, ui_addr, mg, kind, json).await {
            Ok(line) => println!("{line}"),
            Err(e) if stream => eprintln!("macroctl: pool fetch failed ({e}); retrying"),
            Err(e) => return Err(e),
        }
        if !stream {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// `host:port` of an endpoint, with any scheme stripped.
fn endpoint_authority(s: &str) -> &str {
    let s = s.trim();
    s.split_once("://")
        .map_or(s, |(_, rest)| rest)
        .trim_end_matches('/')
}

/// Whether two endpoints name the same `host:port`, whatever their
/// schemes. Compares the text; no name lookup.
fn same_endpoint(a: &str, b: &str) -> bool {
    endpoint_authority(a) == endpoint_authority(b)
}

/// The microgrid to use from an `/api/microgrids` reply, as its id
/// and its entry: `explicit` when the listing has it, otherwise the
/// lowest id. The error names an `explicit` id the listing lacks, or
/// says the listing is empty.
fn choose_mg(
    list: &serde_json::Value,
    explicit: Option<u64>,
) -> Result<(u64, &serde_json::Value), String> {
    match explicit {
        Some(id) => listed(list)
            .find(|&(m, _)| m == id)
            .ok_or_else(|| format!("microgrid {id} not registered")),
        None => listed(list)
            .min_by_key(|&(m, _)| m)
            .ok_or_else(|| "no microgrids registered".to_string()),
    }
}

/// The microgrid [`choose_mg`] picks from an `/api/microgrids` reply,
/// with its running gRPC server's address. The error names the
/// microgrid when it is not registered, has no runtime, is not
/// running, or has no address, and carries the runtime's `error`
/// text when it reports one.
fn pick_grpc_addr(list: &serde_json::Value, mg: Option<u64>) -> Result<(u64, String), String> {
    let (id, entry) = choose_mg(list, mg)?;
    let runtime = entry.get("runtime").filter(|r| !r.is_null());
    let running = runtime
        .and_then(|r| r.get("status")?.as_str())
        .is_some_and(|s| s == "running");
    let addr = runtime.and_then(|r| r.get("grpc_addr")?.as_str());
    if running && let Some(addr) = addr {
        return Ok((id, addr.to_string()));
    }
    let msg = format!("microgrid {id} has no running gRPC server");
    match runtime.and_then(|r| r.get("error")?.as_str()) {
        Some(err) => Err(format!("{msg}: {err}")),
        None => Err(msg),
    }
}

/// The gRPC URL to connect to: `--addr` as given when it is the only
/// choice made; otherwise the selected microgrid's server from
/// `/api/microgrids`, checked against `--addr` when both are given.
async fn grpc_endpoint(
    http: &reqwest::Client,
    ui_addr: &str,
    addr: Option<&str>,
    mg: Option<u64>,
) -> Result<String, Box<dyn std::error::Error>> {
    if let (Some(addr), None) = (addr, mg) {
        return Ok(addr.to_string());
    }
    let list = fetch_microgrids(http, ui_addr)
        .await
        .map_err(|e| format!("{e}; pass --addr to connect directly"))?;
    let (id, found) = pick_grpc_addr(&list, mg)?;
    if let Some(addr) = addr
        && !same_endpoint(addr, &found)
    {
        return Err(format!("--addr {addr} is not microgrid {id}'s server ({found})").into());
    }
    Ok(format!("http://{}", endpoint_authority(&found)))
}

/// Resolve the microgrid id macroctl should hit for per-mg HTTP
/// endpoints. Honours an explicit `--microgrid-id N` if supplied,
/// otherwise queries `/api/microgrids` and returns the lowest
/// registered id.
async fn resolve_microgrid_id(
    http: &reqwest::Client,
    ui_addr: &str,
    explicit: Option<u64>,
) -> Result<u64, Box<dyn std::error::Error>> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    let list = fetch_microgrids(http, ui_addr).await?;
    Ok(choose_mg(&list, None)?.0)
}

/// The UI server's `/api/microgrids` listing. A failed connect names
/// the UI server; a failed response is an error as [`checked`]
/// reports it.
async fn fetch_microgrids(
    http: &reqwest::Client,
    ui_addr: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let resp = http
        .get(format!("{ui_addr}/api/microgrids"))
        .send()
        .await
        .map_err(|e| {
            format!("cannot reach the UI server at {ui_addr} to find a microgrid ({e})")
        })?;
    Ok(checked(resp).await?.json().await?)
}

/// The entries in an `/api/microgrids` reply that carry an id, each
/// with that id; none for a reply that is not a list.
fn listed(list: &serde_json::Value) -> impl Iterator<Item = (u64, &serde_json::Value)> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m.get("id")?.as_u64()?, m)))
}

async fn build_pool_line(
    http: &reqwest::Client,
    ui_addr: &str,
    mg_id: u64,
    kind: &str,
    json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let latest = get_json(http, format!("{ui_addr}/api/mg/{mg_id}/metrics/latest")).await?;
    let stream_for = |s: &str| latest.get(s).cloned();
    if json {
        let mut out = serde_json::Map::new();
        match kind {
            "battery" => {
                if let Some(v) = stream_for("battery_pool_power") {
                    out.insert("power".into(), v);
                }
                if let Some(v) = stream_for("battery_pool_bounds_lower") {
                    out.insert("bounds_lower".into(), v);
                }
                if let Some(v) = stream_for("battery_pool_bounds_upper") {
                    out.insert("bounds_upper".into(), v);
                }
            }
            "pv" => {
                if let Some(v) = stream_for("pv_power") {
                    out.insert("power".into(), v);
                }
            }
            _ => {}
        }
        return Ok(serde_json::to_string(&serde_json::Value::Object(out))?);
    }
    let now = chrono::Local::now().format("%H:%M:%S");
    let read = |s: &str| latest_value(&latest, s);
    let fmt = |w: Option<f64>| match w {
        None => "—".to_owned(),
        Some(v) if v.abs() >= 1e6 => format!("{:+.2} MW", v / 1e6),
        Some(v) if v.abs() >= 1e3 => format!("{:+.2} kW", v / 1e3),
        Some(v) => format!("{:+.1} W", v),
    };
    match kind {
        "battery" => Ok(format!(
            "{now}  power={}  envelope=[{} → {}]",
            fmt(read("battery_pool_power")),
            fmt(read("battery_pool_bounds_lower")),
            fmt(read("battery_pool_bounds_upper")),
        )),
        "pv" => Ok(format!("{now}  power={}", fmt(read("pv_power")))),
        _ => Ok(String::new()),
    }
}

/// Polls /api/mg/{mg}/topology + /api/mg/{mg}/metrics/latest at
/// `interval` (at least 0.1 s) and prints a one-line pulse summary per
/// tick.
/// With `tail=false` (single snapshot mode) the loop runs once and
/// exits, matching `macroctl dashboard` without a flag.
async fn run_dashboard(
    ui_addr: &str,
    microgrid_id: Option<u64>,
    tail: bool,
    interval: std::time::Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let http = reqwest::Client::new();
    let mg = resolve_microgrid_id(&http, ui_addr, microgrid_id).await?;
    let dt = interval.max(std::time::Duration::from_millis(100));
    loop {
        // In --tail mode a transient HTTP error (server mid-reload,
        // loopback rebuilding) must not abort an overnight watch —
        // warn and try again next tick. One-shot mode still errors.
        match build_dashboard_line(&http, ui_addr, mg).await {
            Ok(line) => println!("{line}"),
            Err(e) if tail => eprintln!("macroctl: dashboard fetch failed ({e}); retrying"),
            Err(e) => return Err(e),
        }
        if !tail {
            return Ok(());
        }
        tokio::time::sleep(dt).await;
    }
}

async fn build_dashboard_line(
    http: &reqwest::Client,
    ui_addr: &str,
    mg_id: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let topo = get_json(http, format!("{ui_addr}/api/mg/{mg_id}/topology")).await?;
    let latest = get_json(http, format!("{ui_addr}/api/mg/{mg_id}/metrics/latest")).await?;
    let now = chrono::Local::now().format("%H:%M:%S");
    let components = topo
        .get("components")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ok = 0u32;
    let mut err = 0u32;
    let mut bat_socs: Vec<f64> = Vec::new();
    for c in &components {
        match c.get("health").and_then(|h| h.as_str()) {
            Some("ok") => ok += 1,
            Some(_) => err += 1,
            None => {}
        }
        if c.get("category").and_then(|v| v.as_str()) == Some("battery") {
            // SoC isn't on the topology route; the histogram
            // column lands when J5's pool stream gets folded in.
            // Keep the field for forward compatibility.
            if let Some(soc) = c.get("soc").and_then(|v| v.as_f64()) {
                bat_socs.push(soc);
            }
        }
    }
    let total = components.len();
    let pick = |stream| latest_value(&latest, stream);
    let grid = pick("grid_power");
    let pv = pick("pv_power");
    let bat = pick("battery_pool_power");
    let loopback = latest.as_object().map(|m| !m.is_empty()).unwrap_or(false);
    let fmt = |watts: Option<f64>| match watts {
        None => "—".to_owned(),
        Some(v) if v.abs() >= 1e6 => format!("{:.2}MW", v / 1e6),
        Some(v) if v.abs() >= 1e3 => format!("{:.1}kW", v / 1e3),
        Some(v) => format!("{:.0}W", v),
    };
    let bat_summary = if bat_socs.is_empty() {
        String::new()
    } else {
        let min = bat_socs.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = bat_socs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        format!(" soc=[{:.0}-{:.0}%]", min, max)
    };
    Ok(format!(
        "{now}  comp={total} ok={ok} err={err}  grid={} pv={} bat={}{bat_summary}  loopback={}",
        fmt(grid),
        fmt(pv),
        fmt(bat),
        if loopback { "connected" } else { "off" },
    ))
}

/// The JSON body of a GET of `url`; a failed response is an error as
/// [`checked`] reports it.
async fn get_json(
    http: &reqwest::Client,
    url: impl reqwest::IntoUrl,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    send_json(http.get(url)).await
}

/// The JSON body of the response to `req`; a failed response is an
/// error as [`checked`] reports it.
async fn send_json(
    req: reqwest::RequestBuilder,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(checked(req.send().await?).await?.json().await?)
}

/// `resp` when it succeeded; otherwise an error carrying the
/// server's `error` text, or the body or status when there is none.
async fn checked(resp: reqwest::Response) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    Err(error_line(status, &text).into())
}

/// The report for a failed response: the status, then its JSON
/// `error` field or else its trimmed body; the status alone when the
/// body is empty.
fn error_line(status: reqwest::StatusCode, text: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| text.trim().to_owned());
    if msg.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {msg}")
    }
}

/// `latest[stream].value` as f64 from an `/api/.../metrics/latest`
/// map — shared by the pool and dashboard line builders.
fn latest_value(latest: &serde_json::Value, stream: &str) -> Option<f64> {
    latest
        .get(stream)
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_f64())
}

async fn cmd_info(
    client: &mut MicrogridClient<Channel>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = client.get_microgrid(()).await?.into_inner();
    if json {
        println!("{:#?}", resp);
        return Ok(());
    }
    if let Some(mg) = resp.microgrid {
        println!("microgrid_id   = {}", mg.id);
        println!("enterprise_id  = {}", mg.enterprise_id);
        println!("name           = {}", mg.name);
        println!("status         = {}", mg.status);
        if let Some(t) = mg.create_timestamp {
            println!("created_at     = {}", format_ts(&t));
        }
    } else {
        println!("(no microgrid)");
    }
    Ok(())
}

async fn cmd_list(
    client: &mut MicrogridClient<Channel>,
    category: Option<Category>,
    ids: Vec<u64>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let req = ListElectricalComponentsRequest {
        electrical_component_categories: category
            .map(|c| vec![c.to_proto() as i32])
            .unwrap_or_default(),
        electrical_component_ids: ids,
    };
    let resp = client.list_electrical_components(req).await?.into_inner();
    if json {
        println!("{:#?}", resp.electrical_components);
        return Ok(());
    }
    println!("{}", list_header());
    for c in &resp.electrical_components {
        let (lo, hi) = active_bounds(c);
        println!(
            "{:>5}  {:<24}  {:<10}  {:<12}  {:>14}  {:>14}",
            c.id,
            c.name,
            short_category(c.category),
            short_subtype(c),
            fmt_opt(lo),
            fmt_opt(hi),
        );
    }
    Ok(())
}

fn list_header() -> String {
    format!(
        "{:>5}  {:<24}  {:<10}  {:<12}  {:>14}  {:>14}",
        "id", "name", "category", "subtype", "rated_lower_w", "rated_upper_w"
    )
}

async fn cmd_connections(
    client: &mut MicrogridClient<Channel>,
    from: Vec<u64>,
    to: Vec<u64>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let req = ListElectricalComponentConnectionsRequest {
        source_electrical_component_ids: from,
        destination_electrical_component_ids: to,
    };
    let resp = client
        .list_electrical_component_connections(req)
        .await?
        .into_inner();
    if json {
        println!("{:#?}", resp.electrical_component_connections);
        return Ok(());
    }
    for c in &resp.electrical_component_connections {
        println!(
            "{} -> {}",
            c.source_electrical_component_id, c.destination_electrical_component_id
        );
    }
    Ok(())
}

async fn cmd_tree(client: &mut MicrogridClient<Channel>) -> Result<(), Box<dyn std::error::Error>> {
    let comps = client
        .list_electrical_components(ListElectricalComponentsRequest::default())
        .await?
        .into_inner()
        .electrical_components;
    let conns = client
        .list_electrical_component_connections(ListElectricalComponentConnectionsRequest::default())
        .await?
        .into_inner()
        .electrical_component_connections;

    let by_id: BTreeMap<u64, &ElectricalComponent> = comps.iter().map(|c| (c.id, c)).collect();
    let mut children: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    let mut has_parent: BTreeSet<u64> = BTreeSet::new();
    for c in &conns {
        children
            .entry(c.source_electrical_component_id)
            .or_default()
            .push(c.destination_electrical_component_id);
        has_parent.insert(c.destination_electrical_component_id);
    }

    let roots: Vec<u64> = by_id
        .keys()
        .copied()
        .filter(|id| !has_parent.contains(id))
        .collect();
    for (i, r) in roots.iter().enumerate() {
        print_tree(*r, &by_id, &children, "", i == roots.len() - 1, true);
    }
    Ok(())
}

fn print_tree(
    id: u64,
    by_id: &BTreeMap<u64, &ElectricalComponent>,
    children: &BTreeMap<u64, Vec<u64>>,
    prefix: &str,
    is_last: bool,
    is_root: bool,
) {
    let connector = if is_root {
        ""
    } else if is_last {
        "└── "
    } else {
        "├── "
    };
    let label = match by_id.get(&id) {
        Some(c) => format!("[{}] {} ({})", c.id, c.name, short_category(c.category)),
        None => format!("[{id}] <unknown>"),
    };
    println!("{prefix}{connector}{label}");
    if let Some(kids) = children.get(&id) {
        let new_prefix = if is_root {
            String::new()
        } else if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };
        for (i, k) in kids.iter().enumerate() {
            print_tree(*k, by_id, children, &new_prefix, i == kids.len() - 1, false);
        }
    }
}

async fn cmd_stream(
    client: &mut MicrogridClient<Channel>,
    id: u64,
    samples: Option<usize>,
    metric_names: Vec<String>,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let filter = if metric_names.is_empty() {
        None
    } else {
        let mut metrics = Vec::with_capacity(metric_names.len());
        for name in &metric_names {
            metrics.push(parse_metric_name(name)? as i32);
        }
        Some(
            macrocosim::proto::microgrid::receive_electrical_component_telemetry_stream_request::ComponentTelemetryStreamFilter {
                metrics,
            },
        )
    };
    let req = ReceiveElectricalComponentTelemetryStreamRequest {
        electrical_component_id: id,
        filter,
    };
    let mut stream = client
        .receive_electrical_component_telemetry_stream(req)
        .await?
        .into_inner();

    if !json {
        println!("{}", stream_header());
    }

    let mut got = 0usize;
    while samples != Some(got)
        && let Some(msg) = stream.message().await?
    {
        let Some(t) = msg.telemetry else { continue };
        if json {
            println!("{:#?}", t);
        } else {
            for s in &t.metric_samples {
                let metric = Metric::try_from(s.metric)
                    .map(short_metric)
                    .unwrap_or_else(|_| format!("METRIC_{}", s.metric));
                let value = s
                    .value
                    .as_ref()
                    .and_then(|v| v.metric_value_variant.as_ref())
                    .and_then(|v| match v {
                        macrocosim::proto::common::metrics::metric_value_variant::MetricValueVariant::SimpleMetric(sv) => Some(sv.value),
                        _ => None,
                    });
                let bounds = if s.bounds.is_empty() {
                    String::new()
                } else {
                    s.bounds
                        .iter()
                        .map(|b| format!("[{}, {}]", fmt_opt(b.lower), fmt_opt(b.upper)))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let ts = s
                    .sample_time
                    .as_ref()
                    .map(format_ts)
                    .unwrap_or_else(|| "-".into());
                let unit = Metric::try_from(s.metric).map_or("", metric_unit);
                println!("{}", stream_row(&ts, &metric, value, unit, &bounds));
            }
        }
        got += 1;
    }
    Ok(())
}

fn stream_header() -> String {
    format!(
        "{:<24}  {:<26}  {:>14}  {:<5}  {:<22}",
        "time", "metric", "value", "unit", "bounds"
    )
}

fn stream_row(ts: &str, metric: &str, value: Option<f32>, unit: &str, bounds: &str) -> String {
    let value = match value {
        Some(v) => format!("{v:>14.2}"),
        None => format!("{:>14}", "-"),
    };
    format!("{ts:<24}  {metric:<26}  {value}  {unit:<5}  {bounds}")
}

/// The unit a metric's samples and bounds are in. Electrical metrics
/// (power, energy, voltage, current, frequency, battery capacity), SoC
/// and humidity have one. A ratio (power factor, harmonic distortion),
/// temperature, dew point, wind, air pressure, irradiance and an
/// unspecified metric print none.
fn metric_unit(m: Metric) -> &'static str {
    let name = m.as_str_name();
    let no_unit = [
        "POWER_FACTOR",
        "HARMONIC_DISTORTION",
        "TEMPERATURE",
        "DEW_POINT",
        "WIND_",
        "AIR_PRESSURE",
        "IRRADIANCE",
    ];
    if no_unit.iter().any(|family| name.contains(family)) {
        ""
    } else if name.contains("_ENERGY_") {
        if name.contains("REACTIVE") {
            "VArh"
        } else if name.contains("APPARENT") {
            "VAh"
        } else {
            "Wh"
        }
    } else if name.contains("POWER_REACTIVE") {
        "VAr"
    } else if name.contains("POWER_APPARENT") {
        "VA"
    } else if name.contains("POWER") {
        "W"
    } else if name.contains("BATTERY_CAPACITY") {
        "Wh"
    } else if name.contains("VOLTAGE") {
        "V"
    } else if name.contains("CURRENT") {
        "A"
    } else if name.contains("FREQUENCY") {
        "Hz"
    } else if name.contains("SOC_PCT") || name.contains("HUMIDITY") {
        "%"
    } else {
        ""
    }
}

/// The setpoint request `set-power` (active) or `set-reactive-power`
/// (reactive) sends; `None` for any other command.
fn set_power_request(cmd: &Cmd) -> Option<SetElectricalComponentPowerRequest> {
    let (id, power, power_type, lifetime_s) = match *cmd {
        Cmd::SetPower {
            id,
            power_w,
            lifetime_s,
        } => (id, power_w, PowerType::Active, lifetime_s),
        Cmd::SetReactivePower {
            id,
            power_var,
            lifetime_s,
        } => (id, power_var, PowerType::Reactive, lifetime_s),
        _ => return None,
    };
    Some(SetElectricalComponentPowerRequest {
        electrical_component_id: id,
        power,
        power_type: power_type as i32,
        request_lifetime: lifetime_s,
    })
}

/// The bounds request `augment-bounds` (active) or
/// `augment-reactive-bounds` (reactive) sends; `None` for any other
/// command.
fn augment_request(cmd: &Cmd) -> Option<AugmentElectricalComponentBoundsRequest> {
    use macrocosim::proto::common::metrics::Bounds;
    let (id, target_metric, lower, upper, lifetime_s) = match *cmd {
        Cmd::AugmentBounds {
            id,
            lower_w,
            upper_w,
            lifetime_s,
        } => (id, Metric::AcPowerActive, lower_w, upper_w, lifetime_s),
        Cmd::AugmentReactiveBounds {
            id,
            lower_var,
            upper_var,
            lifetime_s,
        } => (
            id,
            Metric::AcPowerReactive,
            lower_var,
            upper_var,
            lifetime_s,
        ),
        _ => return None,
    };
    Some(AugmentElectricalComponentBoundsRequest {
        electrical_component_id: id,
        target_metric: target_metric as i32,
        bounds: vec![Bounds {
            lower: Some(lower),
            upper: Some(upper),
        }],
        request_lifetime: Some(lifetime_s),
    })
}

async fn cmd_set_power(
    client: &mut MicrogridClient<Channel>,
    req: SetElectricalComponentPowerRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = client
        .set_electrical_component_power(req)
        .await?
        .into_inner();
    while let Some(msg) = stream.message().await? {
        let name =
            macrocosim::proto::microgrid::SetElectricalComponentPowerRequestStatus::try_from(
                msg.status,
            )
            .map(|s| {
                s.as_str_name()
                    .trim_start_matches("SET_ELECTRICAL_COMPONENT_POWER_REQUEST_STATUS_")
                    .to_string()
            })
            .unwrap_or_else(|_| msg.status.to_string());
        println!("status: {name}");
        if let Some(t) = msg.valid_until_time {
            println!("valid_until: {}", format_ts(&t));
        }
    }
    Ok(())
}

async fn cmd_augment(
    client: &mut MicrogridClient<Channel>,
    req: AugmentElectricalComponentBoundsRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = client
        .augment_electrical_component_bounds(req)
        .await?
        .into_inner();
    if let Some(t) = resp.valid_until_time {
        println!("valid_until: {}", format_ts(&t));
    } else {
        println!("(no expiry returned)");
    }
    Ok(())
}

// ---------- formatting helpers --------------------------------------------

fn fmt_opt(v: Option<f32>) -> String {
    v.map(|x| format!("{x}")).unwrap_or_else(|| "*".into())
}

fn format_ts(t: &prost_types::Timestamp) -> String {
    use chrono::{DateTime, Utc};
    let secs = t.seconds;
    let nanos = t.nanos.max(0) as u32;
    DateTime::<Utc>::from_timestamp(secs, nanos)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| format!("{secs}.{nanos:09}"))
}

fn short_category(cat: i32) -> &'static str {
    match ElectricalComponentCategory::try_from(cat) {
        Ok(ElectricalComponentCategory::GridConnectionPoint) => "grid",
        Ok(ElectricalComponentCategory::Meter) => "meter",
        Ok(ElectricalComponentCategory::Inverter) => "inverter",
        Ok(ElectricalComponentCategory::Battery) => "battery",
        Ok(ElectricalComponentCategory::EvCharger) => "ev",
        Ok(ElectricalComponentCategory::Chp) => "chp",
        Ok(ElectricalComponentCategory::SteamBoiler) => "boiler",
        _ => "?",
    }
}

fn short_subtype(c: &ElectricalComponent) -> String {
    let Some(info) = c.category_specific_info.as_ref() else {
        return String::new();
    };
    let Some(kind) = info.kind.as_ref() else {
        return String::new();
    };
    match kind {
        Kind::Inverter(i) => format!("{:?}", i.r#type()),
        Kind::Battery(b) => format!("{:?}", b.r#type()),
        Kind::EvCharger(e) => format!("{:?}", e.r#type()),
        Kind::GridConnectionPoint(g) => format!("fuse={} A", g.rated_fuse_current),
        _ => String::new(),
    }
}

fn active_bounds(c: &ElectricalComponent) -> (Option<f32>, Option<f32>) {
    for b in &c.metric_config_bounds {
        let metric = Metric::try_from(b.metric).ok();
        if matches!(metric, Some(Metric::AcPowerActive) | Some(Metric::DcPower))
            && let Some(cb) = &b.config_bounds
        {
            return (cb.lower, cb.upper);
        }
    }
    (None, None)
}

fn short_metric(m: Metric) -> String {
    m.as_str_name().trim_start_matches("METRIC_").to_string()
}

/// Resolve a CLI string to the proto `Metric` enum. Accepts both
/// `METRIC_AC_POWER_ACTIVE` and the lowercase `ac_power_active` /
/// `ac-power-active` shorthands; the lookup is case-insensitive and
/// tolerant of `-` vs `_` so users don't have to fight tab-completion.
fn parse_metric_name(s: &str) -> Result<Metric, Box<dyn std::error::Error>> {
    let normalized = s.replace('-', "_").to_ascii_uppercase();
    let with_prefix = if normalized.starts_with("METRIC_") {
        normalized
    } else {
        format!("METRIC_{normalized}")
    };
    Metric::from_str_name(&with_prefix).ok_or_else(|| format!("unknown metric '{s}'").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_line_prefers_the_error_field() {
        use reqwest::StatusCode;
        assert_eq!(
            error_line(
                StatusCode::NOT_FOUND,
                r#"{"error":"microgrid 9 not registered"}"#
            ),
            "404 Not Found: microgrid 9 not registered"
        );
        assert_eq!(error_line(StatusCode::BAD_GATEWAY, ""), "502 Bad Gateway");
        assert_eq!(
            error_line(StatusCode::BAD_GATEWAY, "  \n"),
            "502 Bad Gateway"
        );
        assert_eq!(
            error_line(StatusCode::BAD_GATEWAY, "oops\n"),
            "502 Bad Gateway: oops"
        );
    }

    #[test]
    fn same_endpoint_ignores_the_scheme() {
        assert!(same_endpoint("http://[::1]:8800", "[::1]:8800"));
        assert!(same_endpoint(
            "grpc://127.0.0.1:8800",
            "http://127.0.0.1:8800"
        ));
        assert!(!same_endpoint("http://[::1]:8800", "[::1]:8802"));
    }

    #[test]
    fn choose_mg_takes_a_listed_id_else_the_lowest() {
        let list = serde_json::json!([{"id": 7}, {"id": 3}]);
        assert_eq!(choose_mg(&list, Some(7)), Ok((7, &list[0])));
        assert_eq!(choose_mg(&list, None), Ok((3, &list[1])));
        assert_eq!(
            choose_mg(&list, Some(9)).unwrap_err(),
            "microgrid 9 not registered"
        );
        assert_eq!(
            choose_mg(&serde_json::json!([]), None).unwrap_err(),
            "no microgrids registered"
        );
    }

    #[test]
    fn pick_grpc_addr_defaults_to_the_lowest_running_microgrid() {
        let list = serde_json::json!([
            {"id": 7, "runtime": {"status": "running", "grpc_addr": "[::1]:8807", "error": null}},
            {"id": 3, "runtime": {"status": "running", "grpc_addr": "[::1]:8803", "error": null}},
        ]);
        assert_eq!(
            pick_grpc_addr(&list, None).unwrap(),
            (3, "[::1]:8803".to_string())
        );
        assert_eq!(
            pick_grpc_addr(&list, Some(7)).unwrap(),
            (7, "[::1]:8807".to_string())
        );
    }

    #[test]
    fn pick_grpc_addr_refuses_a_microgrid_without_a_running_server() {
        let list = serde_json::json!([
            {"id": 3, "runtime": null},
            {"id": 4, "runtime": {"status": "failed", "grpc_addr": null, "error": "port taken"}},
        ]);
        assert!(
            pick_grpc_addr(&list, Some(3))
                .unwrap_err()
                .contains("microgrid 3")
        );
        assert_eq!(
            pick_grpc_addr(&list, Some(3)).unwrap_err(),
            "microgrid 3 has no running gRPC server"
        );
        assert_eq!(
            pick_grpc_addr(&list, Some(4)).unwrap_err(),
            "microgrid 4 has no running gRPC server: port taken"
        );
        assert!(
            pick_grpc_addr(&list, Some(9))
                .unwrap_err()
                .contains("microgrid 9")
        );
    }

    #[tokio::test]
    async fn resolve_microgrid_id_names_an_unreachable_ui_server() {
        // Nothing listens on port 1: the request fails to connect.
        let err = resolve_microgrid_id(&test_client(), "http://127.0.0.1:1", None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("cannot reach the UI server at http://127.0.0.1:1"),
            "{err}"
        );
    }

    /// The requests a [`canned_ui_server`] got, each as `METHOD
    /// path`.
    type Requests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A UI server stand-in on 127.0.0.1 that answers each of `n`
    /// connections with `body` as a JSON 200, then stops. Returns its
    /// base URL and the requests it got.
    fn canned_ui_server(body: &'static str, n: usize) -> (String, Requests) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Requests::default();
        let seen = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(n) {
                let mut stream = stream.unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let got = stream.read(&mut buf).unwrap();
                    if got == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..got]);
                }
                let head = String::from_utf8_lossy(&req);
                let line: Vec<&str> = head.split_whitespace().take(2).collect();
                seen.lock().unwrap().push(line.join(" "));
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
        });
        (url, requests)
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn grpc_endpoint_takes_a_lone_addr_without_asking_the_ui() {
        // Nothing listens on port 1: any request would fail.
        let got = grpc_endpoint(
            &test_client(),
            "http://127.0.0.1:1",
            Some("http://[::1]:8800"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(got, "http://[::1]:8800");
    }

    #[tokio::test]
    async fn grpc_endpoint_checks_addr_against_the_microgrid() {
        let (ui, _) = canned_ui_server(
            r#"[{"id": 3, "runtime": {"status": "running", "grpc_addr": "[::1]:8803", "error": null}}]"#,
            2,
        );
        let http = test_client();
        let err = grpc_endpoint(&http, &ui, Some("http://[::1]:9999"), Some(3))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "--addr http://[::1]:9999 is not microgrid 3's server ([::1]:8803)"
        );
        let got = grpc_endpoint(&http, &ui, Some("grpc://[::1]:8803"), Some(3))
            .await
            .unwrap();
        assert_eq!(got, "http://[::1]:8803");
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("macroctl").chain(args.iter().copied()))
    }

    #[test]
    fn power_flags_carry_their_unit() {
        let cli = parse(&["set-power", "5", "-300", "--lifetime-s", "30"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::SetPower {
                id: 5,
                power_w: -300.0,
                lifetime_s: Some(30)
            }
        ));
        let cli = parse(&["set-reactive-power", "5", "300"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::SetReactivePower {
                id: 5,
                power_var: 300.0,
                lifetime_s: None
            }
        ));
        assert!(parse(&["set-power", "5", "300", "--reactive"]).is_err());
        assert!(parse(&["set-power", "5", "300", "--lifetime", "30"]).is_err());
    }

    #[test]
    fn bounds_flags_carry_their_unit() {
        let cli = parse(&["augment-bounds", "5", "--lower-w", "-10", "--upper-w", "20"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::AugmentBounds {
                id: 5,
                lower_w: -10.0,
                upper_w: 20.0,
                lifetime_s: 60
            }
        ));
        let cli = parse(&[
            "augment-reactive-bounds",
            "5",
            "--lower-var",
            "-1",
            "--upper-var",
            "2",
            "--lifetime-s",
            "15",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::AugmentReactiveBounds {
                id: 5,
                lower_var: -1.0,
                upper_var: 2.0,
                lifetime_s: 15
            }
        ));
        assert!(parse(&["augment-bounds", "5", "--lower", "1", "--upper", "2"]).is_err());
        assert!(
            parse(&[
                "augment-bounds",
                "5",
                "--lower-w",
                "1",
                "--upper-w",
                "2",
                "--reactive"
            ])
            .is_err()
        );
    }

    /// The reactive subcommands send the reactive axis and the active
    /// ones the active axis.
    #[test]
    fn write_commands_send_their_own_axis() {
        let set = |args: &[&str]| set_power_request(&parse(args).unwrap().cmd).unwrap();
        let req = set(&["set-power", "5", "-300", "--lifetime-s", "30"]);
        assert_eq!(req.electrical_component_id, 5);
        assert_eq!(req.power, -300.0);
        assert_eq!(req.power_type, PowerType::Active as i32);
        assert_eq!(req.request_lifetime, Some(30));
        let req = set(&["set-reactive-power", "5", "300"]);
        assert_eq!(req.power, 300.0);
        assert_eq!(req.power_type, PowerType::Reactive as i32);
        assert_eq!(req.request_lifetime, None);

        let augment = |args: &[&str]| augment_request(&parse(args).unwrap().cmd).unwrap();
        let req = augment(&["augment-bounds", "5", "--lower-w", "-10", "--upper-w", "20"]);
        assert_eq!(req.target_metric, Metric::AcPowerActive as i32);
        assert_eq!(req.bounds[0].lower, Some(-10.0));
        assert_eq!(req.bounds[0].upper, Some(20.0));
        assert_eq!(req.request_lifetime, Some(60));
        let req = augment(&[
            "augment-reactive-bounds",
            "5",
            "--lower-var",
            "-1",
            "--upper-var",
            "2",
        ]);
        assert_eq!(req.target_metric, Metric::AcPowerReactive as i32);

        let info = parse(&["info"]).unwrap().cmd;
        assert!(set_power_request(&info).is_none());
        assert!(augment_request(&info).is_none());
    }

    #[test]
    fn other_flags_carry_their_unit_or_name_their_id() {
        assert!(parse(&["dashboard", "--tail", "--interval-s", "2.5"]).is_ok());
        assert!(parse(&["dashboard", "--interval", "2"]).is_err());
        assert!(parse(&["dashboard", "--tail", "--interval-s", "inf"]).is_err());
        assert!(parse(&["dashboard", "--tail", "--interval-s", "1e300"]).is_err());
        assert_eq!(
            parse(&["dashboard", "--tail", "--interval-s=-1"])
                .unwrap_err()
                .kind(),
            clap::error::ErrorKind::ValueValidation
        );
        assert!(parse(&["dispatch", "create", "t", "battery", "--duration-s", "60"]).is_ok());
        assert!(parse(&["dispatch", "create", "t", "battery", "--duration", "60"]).is_err());
        assert!(parse(&["snapshot", "list"]).is_err(), "snapshots are gone");
        assert!(parse(&["scenario", "list"]).is_err(), "scenarios are gone");
        let cli = parse(&["list", "--component-id", "4", "--component-id", "5"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::List { ids, .. } if ids == [4, 5]));
        assert!(parse(&["list", "--id", "4"]).is_err());
    }

    #[test]
    fn stream_rows_name_each_metrics_unit() {
        let header = stream_header();
        assert!(header.contains("unit"), "{header}");
        assert_eq!(metric_unit(Metric::AcPowerActive), "W");
        assert_eq!(metric_unit(Metric::AcPowerReactive), "VAr");
        assert_eq!(metric_unit(Metric::AcPowerApparent), "VA");
        assert_eq!(metric_unit(Metric::AcVoltage), "V");
        assert_eq!(metric_unit(Metric::AcCurrent), "A");
        assert_eq!(metric_unit(Metric::AcFrequency), "Hz");
        assert_eq!(metric_unit(Metric::BatterySocPct), "%");
        assert_eq!(metric_unit(Metric::AcEnergyActive), "Wh");
        assert_eq!(metric_unit(Metric::InverterTemperature), "");
        let row = stream_row("t", "AC_POWER_ACTIVE", Some(12.5), "W", "");
        assert_eq!(
            row,
            format!(
                "{:<24}  {:<26}  {:>14}  {:<5}  ",
                "t", "AC_POWER_ACTIVE", "12.50", "W"
            )
        );
    }

    #[test]
    fn list_labels_its_units() {
        assert!(list_header().contains("rated_lower_w"));
        assert!(list_header().contains("rated_upper_w"));
    }
}
