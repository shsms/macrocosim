//! End-to-end tests for the per-microgrid gateway on a site stepped
//! by hand: request lifetimes and augmentations expire on sim time,
//! the SoC window holds a running setpoint, trips follow each
//! component's rules, and a command crosses the gateway and device
//! delays once.

use std::sync::Arc;
use std::time::Duration;

mod common;

use common::{TestServer, first_bounds};
use macrocosim::proto::common::metrics::{Bounds, Metric};
use macrocosim::proto::microgrid::microgrid_client::MicrogridClient;
use macrocosim::proto::microgrid::{
    AugmentElectricalComponentBoundsRequest, PowerType, SetElectricalComponentPowerRequest,
};

use macrocosim::sim::MicrogridSite;
use macrocosim::sim::gateway::Mode;
use macrocosim::sim::runtime::Health;
use macrocosim::sim::sim_clock::{ManualClock, NowSource, headless_base};
use macrocosim::spec::{Defaults, MicrogridSpec, build_topology};
use macrocosim::timeout_tracker::SetpointAxis;

/// A site built from `components` and `connections` (JSON arrays) on
/// a clock that moves only when the test steps it. The components get
/// no spec defaults, only their own.
struct Headless {
    site: MicrogridSite,
    clock: Arc<ManualClock>,
}

fn headless(components: &str, connections: &str) -> Headless {
    let spec: MicrogridSpec = serde_json::from_str(&format!(
        r#"{{"id": 9, "components": {components}, "connections": {connections}}}"#
    ))
    .expect("a valid spec");
    let none = Defaults {
        grid_connection_point: Default::default(),
        meter: Default::default(),
        battery: Default::default(),
        battery_inverter: Default::default(),
        solar_inverter: Default::default(),
        chp: Default::default(),
        ev_charger: Default::default(),
        steam_boiler: Default::default(),
        wind_turbine: Default::default(),
        power_transformer: Default::default(),
        breaker: Default::default(),
    };
    let built = build_topology(&spec, &none).expect("the topology builds");
    let site = MicrogridSite::new();
    let clock = Arc::new(ManualClock::new());
    site.set_now_source(NowSource::sim(headless_base(), clock.clone()));
    site.swap_topology(built, &std::collections::BTreeSet::new());
    Headless { site, clock }
}

impl Headless {
    /// Advance the clock by `dt` and tick the site once.
    fn step(&self, dt: Duration) {
        self.clock.advance(dt);
        self.site.tick_once(self.site.now(), dt);
    }

    /// Step by `dt` until `total` of sim time has passed.
    fn run(&self, total: Duration, dt: Duration) {
        let mut elapsed = Duration::ZERO;
        while elapsed < total {
            self.step(dt);
            elapsed += dt;
        }
    }

    /// Command `value` on `id`'s `axis` for `lifetime_s` of sim time,
    /// refused outside the envelope as over gRPC.
    fn set(&self, id: u64, axis: SetpointAxis, value: f32, lifetime_s: u64) -> Result<(), String> {
        self.site
            .gateway()
            .set_power(
                id,
                self.site.run_generation(),
                axis,
                value,
                Duration::from_secs(lifetime_s),
                Mode::Reject,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The active power `id` puts out, as a parent meter sums it.
    fn active(&self, id: u64) -> f32 {
        self.site.get(id).unwrap().aggregate_power_w(&self.site)
    }

    /// The reactive power `id` puts out.
    fn reactive(&self, id: u64) -> f32 {
        self.site
            .get(id)
            .unwrap()
            .aggregate_reactive_var(&self.site)
    }

    /// The upper edge of `id`'s active-power bounds.
    fn bound_upper(&self, id: u64) -> f32 {
        self.site
            .bounds_of(id, SetpointAxis::Active)
            .unwrap()
            .0
            .last()
            .unwrap()
            .upper
            .unwrap()
    }

    fn soc(&self, id: u64) -> f32 {
        let c = self.site.get(id).unwrap();
        c.telemetry(&self.site).soc_pct.unwrap()
    }
}

const TICK: Duration = Duration::from_millis(100);

const INVERTER_AND_BATTERY: &str = r#"[
    {"type": "battery_inverter", "id": 3, "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
    {"type": "battery", "id": 4, "rated_lower_w": -5000.0, "rated_upper_w": 5000.0}
]"#;

/// A setpoint and an augmentation both lapse after their lifetime of
/// sim time, though almost no wall time passes.
#[test]
fn headless_setpoint_and_augmentation_expire_on_sim_time() {
    let h = headless(INVERTER_AND_BATTERY, "[[3, 4]]");
    h.set(3, SetpointAxis::Active, 2000.0, 10).unwrap();
    h.run(Duration::from_secs(5), TICK);
    assert!((h.active(3) - 2000.0).abs() < 1.0);
    h.run(Duration::from_secs(6), TICK);
    assert!(h.active(3).abs() < 1.0, "expired on sim time");

    h.site
        .gateway()
        .augment(
            3,
            h.site.run_generation(),
            SetpointAxis::Active,
            macrocosim::sim::bounds::VecBounds(vec![Bounds {
                lower: Some(-1000.0),
                upper: Some(1000.0),
            }]),
            Duration::from_secs(10),
        )
        .unwrap();
    assert!((h.bound_upper(3) - 1000.0).abs() < 1e-3);
    h.run(Duration::from_secs(5), TICK);
    assert!((h.bound_upper(3) - 1000.0).abs() < 1e-3, "live at 5 s");
    h.run(Duration::from_secs(6), TICK);
    assert!(
        (h.bound_upper(3) - 5000.0).abs() < 1e-3,
        "expired on sim time"
    );
}

const SMALL_PACK: &str = r#"[
    {"type": "battery_inverter", "id": 3, "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
    {"type": "battery", "id": 4, "capacity_wh": 1000.0, "initial_soc_pct": 80.0,
     "soc_upper_pct": 90.0, "soc_protect_margin_pct": 5.0,
     "rated_lower_w": -5000.0, "rated_upper_w": 5000.0}
]"#;

/// Charging toward `soc_upper_pct`, the inverter's output tapers
/// inside the margin and stops at the window, within one device
/// delay; the command stands throughout.
#[test]
fn the_soc_window_tapers_and_holds_a_running_setpoint() {
    let h = headless(SMALL_PACK, "[[3, 4]]");
    h.set(3, SetpointAxis::Active, 3600.0, 900).unwrap();
    // 3.6 kW into 1 kWh moves the SoC 0.1 % per second.
    for _ in 0..1_000 {
        if h.soc(4) >= 87.0 {
            break;
        }
        h.step(TICK);
    }
    let p = h.active(3);
    assert!(
        p > 0.0 && p < 3_500.0,
        "tapering inside the margin, got {p}"
    );

    h.run(Duration::from_secs(120), TICK);
    let s = h.soc(4);
    assert!(
        s <= 90.03,
        "the window holds within one device delay, got {s}"
    );
    assert!(s >= 89.9, "charged up to the window, got {s}");
    assert!(h.active(3).abs() < 1.0);
    assert!(
        h.site
            .gateway()
            .remaining_lifetime(3, SetpointAxis::Active)
            .is_some()
    );
}

/// Driving the hardware directly, with no gateway in the path, the
/// same pack charges all the way to 100 % and stops there.
#[test]
fn the_hardware_alone_charges_to_full() {
    let h = headless(SMALL_PACK, "[[3, 4]]");
    let site = &h.site;
    let inv = site.get(3).unwrap();
    let ctl = inv.controllable().unwrap();
    let mut now = site.now();
    for _ in 0..3_000 {
        now += chrono::Duration::milliseconds(100);
        ctl.set_command(SetpointAxis::Active, 3_600.0);
        for c in site.components().iter() {
            c.tick(site, now, TICK);
        }
    }
    assert!((h.soc(4) - 100.0).abs() < 1e-3);
    assert!(site.get(4).unwrap().aggregate_power_w(site).abs() < 1.0);
    assert!(
        inv.aggregate_power_w(site).abs() < 1.0,
        "the inverter's accept ratio reaches 0 at the hard limit"
    );
}

/// Trips: the battery inverter loses its command; the PV inverter
/// keeps its curtailment and loses Q; the charger with
/// `resume_on_recovery` keeps its command; nothing replays.
#[test]
fn trips_follow_each_components_rules() {
    let h = headless(
        r#"[
            {"type": "battery_inverter", "id": 3},
            {"type": "battery", "id": 4},
            {"type": "solar_inverter", "id": 5, "sunlight_pct": 100.0,
             "rated_lower_w": -10000.0, "rated_upper_w": 0.0,
             "reactive_pf_limit": 0, "reactive_apparent_va": 10000.0,
             "reactive_command_delay_s": 0},
            {"type": "ev_charger", "id": 6, "command_delay_s": 0, "resume_on_recovery": true}
        ]"#,
        "[[3, 4]]",
    );
    for (id, value) in [(3, 3000.0), (5, -4000.0), (6, 11000.0)] {
        h.set(id, SetpointAxis::Active, value, 600).unwrap();
    }
    h.run(Duration::from_secs(1), TICK);
    h.set(5, SetpointAxis::Reactive, 2000.0, 600).unwrap();
    h.run(Duration::from_secs(5), TICK);
    assert!((h.active(3) - 3000.0).abs() < 1.0);
    assert!((h.active(5) + 4000.0).abs() < 1.0);
    assert!((h.reactive(5) - 2000.0).abs() < 1.0);

    for id in [3, 5, 6] {
        h.site.set_health(id, Health::Error).unwrap();
    }
    h.run(Duration::from_secs(1), TICK);
    assert!(h.active(3).abs() < 1.0);
    assert!(h.active(5).abs() < 1.0);
    assert!(h.reactive(5).abs() < 1.0);
    {
        let gw = h.site.gateway();
        assert_eq!(gw.remaining_lifetime(3, SetpointAxis::Active), None);
        assert!(gw.remaining_lifetime(5, SetpointAxis::Active).is_some());
        assert_eq!(gw.remaining_lifetime(5, SetpointAxis::Reactive), None);
        assert!(gw.remaining_lifetime(6, SetpointAxis::Active).is_some());
    }

    for id in [3, 5, 6] {
        h.site.set_health(id, Health::Ok).unwrap();
    }
    h.run(Duration::from_secs(2), TICK);
    assert!(h.active(3).abs() < 1.0, "no replay, awaits re-dispatch");
    assert!(
        (h.active(5) + 4000.0).abs() < 1.0,
        "the curtailment resumes"
    );
    assert!(h.reactive(5).abs() < 1.0, "Q awaits re-dispatch");
}

/// A command reaches the output after the gateway delay plus the
/// device delay, at any physics tick: with 300 ms + 200 ms it shows
/// on the 6th 100 ms tick and on the 4th 250 ms tick.
#[test]
fn a_command_crosses_the_gateway_and_device_delays_at_any_tick() {
    for (tick_ms, quiet) in [(100_u64, 5_usize), (250, 3)] {
        let h = headless(
            r#"[
                {"type": "battery_inverter", "id": 3, "command_delay_s": 0.3, "device_delay_s": 0.2},
                {"type": "battery", "id": 4}
            ]"#,
            "[[3, 4]]",
        );
        let dt = Duration::from_millis(tick_ms);
        h.set(3, SetpointAxis::Active, 3000.0, 600).unwrap();
        for _ in 0..quiet {
            h.step(dt);
        }
        assert!(h.active(3).abs() < 1.0, "{tick_ms} ms ticks: too early");
        h.step(dt);
        assert!(
            (h.active(3) - 3000.0).abs() < 1.0,
            "{tick_ms} ms ticks: on time"
        );
    }
}

/// With no gateway in the path, an empty pack discharges to 0 % and
/// the inverter's output stops there.
#[test]
fn the_hardware_alone_stops_discharging_at_empty() {
    let h = headless(
        &SMALL_PACK.replace(r#""initial_soc_pct": 80.0"#, r#""initial_soc_pct": 20.0"#),
        "[[3, 4]]",
    );
    let site = &h.site;
    let inv = site.get(3).unwrap();
    let ctl = inv.controllable().unwrap();
    let mut now = site.now();
    for _ in 0..3_000 {
        now += chrono::Duration::milliseconds(100);
        ctl.set_command(SetpointAxis::Active, -3_600.0);
        for c in site.components().iter() {
            c.tick(site, now, TICK);
        }
    }
    assert!(h.soc(4).abs() < 1e-3);
    assert!(site.get(4).unwrap().aggregate_power_w(site).abs() < 1.0);
    assert!(inv.aggregate_power_w(site).abs() < 1.0);
}

/// PV output follows the sun: a drop in sunlight cuts the output
/// within one device delay, a return climbs at the gateway ramp rate.
#[test]
fn pv_output_follows_sunlight_cuts_at_once_and_climbs_at_the_ramp() {
    let h = headless(
        r#"[{"type": "solar_inverter", "id": 5, "sunlight_pct": 100.0,
             "rated_lower_w": -10000.0, "rated_upper_w": 0.0,
             "array_peak_w": 10000.0, "ramp_rate_w_per_s": 1000.0}]"#,
        "[]",
    );
    let sun = |pct: f32| {
        h.site
            .get(5)
            .unwrap()
            .sunlight_drive()
            .unwrap()
            .set_sunlight_pct(pct)
    };
    h.run(Duration::from_secs(30), TICK);
    let full = h.active(5);
    assert!(
        (full + 10_000.0).abs() < 1.0,
        "steady in full sun, got {full}"
    );

    sun(50.0);
    h.run(Duration::from_millis(300), TICK);
    let cut = h.active(5);
    assert!(
        (cut + 5_000.0).abs() < 1.0,
        "cut at once, not ramped, got {cut}"
    );

    sun(100.0);
    h.run(Duration::from_millis(1_000), TICK);
    let mid = h.active(5);
    assert!(mid < -5_000.0 && mid > -10_000.0, "climbing, got {mid}");
    assert!(
        (mid + 6_000.0).abs() < 300.0,
        "about 1000 W/s after 1 s, got {mid}"
    );
    h.run(Duration::from_secs(10), TICK);
    assert!((h.active(5) + 10_000.0).abs() < 1.0);
}

const Q_INVERTER: &str = r#"[
    {"type": "battery_inverter", "id": 3, "reactive_pf_limit": 0, "reactive_apparent_va": 10000.0},
    {"type": "battery", "id": 4}
]"#;

/// Set inverter `id`'s apparent-power limit, as the `drive` route's
/// `reactive_apparent_va` does.
fn apparent_va(h: &Headless, id: u64, va: f32) {
    h.site
        .get(id)
        .unwrap()
        .reactive_limits()
        .unwrap()
        .set_reactive_apparent_va(Some(va));
}

/// Narrowing the apparent-power limit at runtime shrinks the live Q
/// envelope and turns a Q request that used to fit into a rejection.
#[test]
fn changing_the_reactive_limit_at_runtime_moves_the_q_envelope() {
    let h = headless(Q_INVERTER, "[[3, 4]]");
    h.run(Duration::from_secs(1), TICK);
    h.set(3, SetpointAxis::Reactive, 8000.0, 600).unwrap();
    let before = h.site.bounds_of(3, SetpointAxis::Reactive).unwrap();

    apparent_va(&h, 3, 5000.0);
    h.run(Duration::from_secs(1), TICK);
    let after = h.site.bounds_of(3, SetpointAxis::Reactive).unwrap();
    assert_ne!(before.to_string(), after.to_string(), "envelope moved");
    let err = h
        .set(3, SetpointAxis::Reactive, 8000.0, 600)
        .expect_err("8 kVAr no longer fits");
    assert!(
        err.contains("8000 VAr out of bounds"),
        "wrong rejection, envelope {after}: {err}"
    );
    h.set(3, SetpointAxis::Reactive, 4000.0, 600).unwrap();
}

/// A reactive command already standing is pulled in when the apparent
/// power limit narrows under it, and resumes when the limit returns.
#[test]
fn a_standing_q_command_follows_a_narrowing_apparent_power_limit() {
    let h = headless(Q_INVERTER, "[[3, 4]]");
    h.run(Duration::from_secs(1), TICK);
    h.set(3, SetpointAxis::Reactive, 8000.0, 600).unwrap();
    // 100 ms delay, then 2000 VAr/s: 8 kVAr takes about 4 s.
    h.run(Duration::from_secs(6), TICK);
    assert!((h.reactive(3) - 8_000.0).abs() < 1.0);

    apparent_va(&h, 3, 5000.0);
    h.run(Duration::from_secs(2), TICK);
    let q = h.reactive(3);
    assert!(
        q <= 5_001.0 && q > 4_000.0,
        "pulled in, not cleared, got {q}"
    );
    assert!(
        h.site
            .gateway()
            .remaining_lifetime(3, SetpointAxis::Reactive)
            .is_some()
    );

    apparent_va(&h, 3, 10000.0);
    h.run(Duration::from_secs(4), TICK);
    let q = h.reactive(3);
    assert!((q - 8_000.0).abs() < 1.0, "back to the command, got {q}");
}

const AGREEMENT_TOPOLOGY: &str = r#"{
  "components": [
    {"type": "grid_connection_point", "id": 1},
    {"type": "meter", "id": 2},
    {"type": "battery_inverter", "id": 4, "rated_lower_w": -10000.0, "rated_upper_w": 10000.0},
    {"type": "battery", "id": 3, "initial_soc_pct": 87.5, "soc_upper_pct": 90.0,
     "soc_protect_margin_pct": 5.0, "rated_lower_w": -10000.0, "rated_upper_w": 10000.0},
    {"type": "steam_boiler", "id": 6, "demand_kg_per_s": 0.027777778}
  ],
  "connections": [[1, 2], [2, 4], [4, 3], [2, 6]]
}"#;

/// gRPC telemetry, `site.bounds_of`, the history ring and the
/// recording's bounds CSV report the same upper edge for a throttled
/// battery, an augmented inverter and a boiler.
#[tokio::test(flavor = "multi_thread")]
async fn bounds_reads_agree_across_every_consumer() {
    let s = TestServer::start_mg(AGREEMENT_TOPOLOGY).await;
    let mut c = MicrogridClient::connect(s.grpc_url.clone()).await.unwrap();
    c.augment_electrical_component_bounds(AugmentElectricalComponentBoundsRequest {
        electrical_component_id: 4,
        target_metric: Metric::AcPowerActive as i32,
        bounds: vec![Bounds {
            lower: Some(-3000.0),
            upper: Some(3000.0),
        }],
        request_lifetime: Some(600),
    })
    .await
    .expect("augment ok");

    let http = reqwest::Client::new();
    let started = http
        .post(s.mg_url("recording/start"))
        .json(&serde_json::json!({"name": "bounds", "csv": true}))
        .send()
        .await
        .unwrap();
    assert!(started.status().is_success(), "{}", started.status());
    let site = s.site();
    let now = chrono::Utc::now();
    site.record_history_snapshot(now);
    let stopped = http.post(s.mg_url("recording/stop")).send().await.unwrap();
    assert!(stopped.status().is_success(), "{}", stopped.status());
    let listing: serde_json::Value = http
        .get(s.mg_url("recording/csv"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let csv_dir = std::path::PathBuf::from(listing["dir"].as_str().expect("a CSV dir"));

    for (id, metric) in [
        (3, Metric::DcPower),
        (4, Metric::AcPowerActive),
        (6, Metric::AcPowerActive),
    ] {
        let gateway = site
            .bounds_of(id, SetpointAxis::Active)
            .unwrap()
            .0
            .last()
            .unwrap()
            .upper
            .unwrap();
        let grpc = first_bounds(&mut c, id, metric)
            .await
            .last()
            .and_then(|b| b.upper)
            .expect("an upper edge");
        let history = site
            .history_window(
                id,
                macrocosim::sim::history::Metric::ActivePowerUpperBoundW,
                now - chrono::Duration::seconds(1),
            )
            .unwrap()
            .last()
            .unwrap()
            .value;
        let csv = std::fs::read_to_string(csv_dir.join(format!("{id}-bounds.csv"))).unwrap();
        let csv: f32 = csv
            .lines()
            .last()
            .unwrap()
            .split(',')
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        for (what, v) in [("grpc", grpc), ("history", history), ("csv", csv)] {
            assert!(
                (v - gateway).abs() < 1e-2,
                "component {id}: {what} {v} vs gateway {gateway}"
            );
        }
    }
    let battery = site.bounds_of(3, SetpointAxis::Active).unwrap().0[0]
        .upper
        .unwrap();
    assert!(
        battery > 0.0 && battery < 10_000.0,
        "the battery is throttled: {battery}"
    );
    assert_eq!(
        site.bounds_of(4, SetpointAxis::Active).unwrap().0[0].upper,
        Some(3000.0)
    );
    let boiler = site.bounds_of(6, SetpointAxis::Active).unwrap().0[0]
        .upper
        .unwrap();
    assert!(
        (boiler - 62_700.0).abs() < 1.0,
        "the boiler advertises its need: {boiler}"
    );
}

/// `valid_until` on a SetPower response is the gateway's deadline.
#[tokio::test(flavor = "multi_thread")]
async fn valid_until_is_the_gateways_deadline() {
    let s = TestServer::start_mg(&format!(
        r#"{{"components": {INVERTER_AND_BATTERY}, "connections": [[3, 4]]}}"#
    ))
    .await;
    let mut c = MicrogridClient::connect(s.grpc_url.clone()).await.unwrap();
    let mut stream = c
        .set_electrical_component_power(SetElectricalComponentPowerRequest {
            electrical_component_id: 3,
            power: 1000.0,
            power_type: PowerType::Active as i32,
            request_lifetime: Some(30),
        })
        .await
        .expect("set-power ok")
        .into_inner();
    let ts = stream
        .message()
        .await
        .unwrap()
        .unwrap()
        .valid_until_time
        .expect("valid_until");
    let valid_until = chrono::DateTime::from_timestamp(ts.seconds, ts.nanos as u32).unwrap();
    let left = s
        .site()
        .gateway()
        .remaining_lifetime(3, SetpointAxis::Active)
        .expect("a live lifetime");
    let gateway_deadline = chrono::Utc::now() + chrono::Duration::from_std(left).unwrap();
    assert!(
        (valid_until - gateway_deadline).num_milliseconds().abs() < 1000,
        "{valid_until} vs {gateway_deadline}"
    );
}
