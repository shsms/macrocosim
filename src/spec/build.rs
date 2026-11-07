//! Building a microgrid's components from its spec, without touching
//! any site: a failed build changes nothing.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use super::{
    BatteryInverterSpec, BatterySpec, ComponentSpec, Defaults, EvChargerSpec,
    GridConnectionPointSpec, MarkerSpec, MeterSpec, MicrogridSpec, Modes, SolarInverterSpec,
    SpecError, SteamBoilerSpec, fill, order,
};
use crate::proto::common::metrics::Bounds;
use crate::sim::{
    Battery, BatteryInverter, Category, EvCharger, Grid, Marker, Meter, OperationalMode,
    ReactiveSource, SimulatedComponent, SolarInverter, SteamBoiler, SteamBoilerConfig,
    battery::BatteryConfig,
    dynamic_scalar::DynamicScalar,
    ev_charger::EvChargerConfig,
    inverter::{battery_inverter::BatteryInverterConfig, solar_inverter::SolarInverterConfig},
    reactive::ReactiveCapability,
    runtime::{CommandMode, ComponentRuntime, Health, TelemetryMode},
};

/// The physics tick of a microgrid whose spec sets none.
const DEFAULT_PHYSICS_TICK_MS: u64 = 100;

/// What a site applies to a component after it installs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InitialState {
    /// The runtime knobs its spec's modes give it
    /// ([`runtime_from_modes`]).
    pub runtime: ComponentRuntime,
    pub operational_mode: Option<OperationalMode>,
    pub name: Option<String>,
}

/// A microgrid's components, built and not yet installed in a site.
pub struct BuiltTopology {
    /// Children before parents: the order to register (and tick)
    /// them.
    pub components: Vec<Arc<dyn SimulatedComponent>>,
    pub connections: Vec<(u64, u64)>,
    /// Each component's spec, with the defaults filled in, by id.
    pub filled: BTreeMap<u64, ComponentSpec>,
    pub initial: BTreeMap<u64, InitialState>,
    pub physics_tick_ms: u64,
}

impl fmt::Debug for BuiltTopology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<u64> = self.components.iter().map(|c| c.id()).collect();
        f.debug_struct("BuiltTopology")
            .field("components", &ids)
            .field("connections", &self.connections)
            .field("physics_tick_ms", &self.physics_tick_ms)
            .finish_non_exhaustive()
    }
}

/// Fill `spec` from `defaults`, check it, and build every component
/// children first.
///
/// Checked: a given `grpc_port` is not 0, every component has an id,
/// no id appears twice, each
/// connection names two components and appears once, the connections
/// have no cycle, each component's values and the fields that go in
/// pairs follow the rules of the Lisp `make-*` functions, and each
/// component's modes agree ([`runtime_from_modes`]).
pub fn build_topology(
    spec: &MicrogridSpec,
    defaults: &Defaults,
) -> Result<BuiltTopology, SpecError> {
    if spec.grpc_port == Some(0) {
        return Err(SpecError::new("grpc_port must be 1..=65535"));
    }
    let MicrogridSpec {
        components: filled_components,
        connections,
        physics_tick_s,
        ..
    } = fill(spec, defaults);
    let mut by_id: BTreeMap<u64, ComponentSpec> = BTreeMap::new();
    let mut ids = Vec::with_capacity(filled_components.len());
    for (index, c) in filled_components.into_iter().enumerate() {
        let Some(id) = c.id() else {
            return Err(SpecError::new(format!(
                "components[{index}] ({}) has no id",
                c.type_name()
            )));
        };
        if id == 0 {
            return Err(SpecError::new(format!(
                "components[{index}] ({}) has id 0; ids are positive",
                c.type_name()
            )));
        }
        if by_id.insert(id, c).is_some() {
            return Err(SpecError::new(format!("component id {id} appears twice")));
        }
        ids.push(id);
    }

    let mut edges = BTreeSet::new();
    for &(parent, child) in &connections {
        for end in [parent, child] {
            if !by_id.contains_key(&end) {
                return Err(SpecError::new(format!(
                    "connection {parent} → {child} names component {end}, which is not in \
                     components"
                )));
            }
        }
        if parent == child {
            return Err(SpecError::new(format!(
                "connection {parent} → {child} is a self-edge"
            )));
        }
        if !edges.insert((parent, child)) {
            return Err(SpecError::new(format!(
                "connection {parent} → {child} appears twice"
            )));
        }
    }
    if let Some((parent, child)) = order::cycle_edge(&connections) {
        return Err(SpecError::new(format!(
            "the connections contain a cycle (reachable through {parent} → {child})"
        )));
    }

    let physics_tick_ms = match physics_tick_s {
        None => DEFAULT_PHYSICS_TICK_MS,
        // Whole milliseconds, at least one, as `set-physics-tick-s`.
        Some(s) => (secs("physics_tick_s", s)
            .map_err(SpecError::new)?
            .as_millis() as u64)
            .max(1),
    };

    let mut components = Vec::with_capacity(ids.len());
    let mut initial = BTreeMap::new();
    for id in order::children_first(ids, &connections) {
        let c = &by_id[&id];
        let fail = |e: String| SpecError::new(format!("component {id} ({}): {e}", c.type_name()));
        let runtime = runtime_from_modes(id, c.modes()).map_err(fail)?;
        components.push(build_component(id, c).map_err(fail)?);
        initial.insert(
            id,
            InitialState {
                runtime,
                operational_mode: c.modes().operational_mode,
                name: c.name().map(str::to_string),
            },
        );
    }
    Ok(BuiltTopology {
        components,
        connections,
        filled: by_id,
        initial,
        physics_tick_ms,
    })
}

/// The runtime knobs a freshly registered component ends up with once
/// `modes` are applied in the site's order (operational mode, health,
/// telemetry mode, command mode), or why that order refuses them.
///
/// The operational mode sets the knobs it allows: no telemetry
/// silences the stream, no control errors the command channel. Health
/// `error` errors the command channel, and a command mode given with
/// it is skipped; health `ok` reopens the channel if the operational
/// mode allows it. A `normal` telemetry or command mode is refused
/// when the operational mode does not allow it.
pub fn runtime_from_modes(id: u64, modes: Modes) -> Result<ComponentRuntime, String> {
    let mode = modes.operational_mode.unwrap_or_default();
    let mut runtime = ComponentRuntime::default();
    if modes.operational_mode.is_some() {
        runtime.telemetry = if mode.provides_telemetry() {
            TelemetryMode::Normal
        } else {
            TelemetryMode::Silent
        };
        runtime.command = if mode.accepts_control() {
            CommandMode::Normal
        } else {
            CommandMode::Error
        };
    }
    if let Some(health) = modes.health {
        runtime.health = health;
        match health {
            Health::Error => runtime.command = CommandMode::Error,
            Health::Ok => {
                runtime.command = if mode.accepts_control() {
                    CommandMode::Normal
                } else {
                    CommandMode::Error
                };
            }
            Health::Standby => {}
        }
    }
    if let Some(telemetry) = modes.telemetry_mode {
        if telemetry == TelemetryMode::Normal && !mode.provides_telemetry() {
            return Err(format!(
                "component {id} has operational mode {mode}, which streams no \
                 telemetry; cannot set telemetry-mode to normal"
            ));
        }
        runtime.telemetry = telemetry;
    }
    // Health `error` keeps the command channel shut: a command mode
    // given with it is not applied.
    if let Some(command) = modes.command_mode
        && modes.health != Some(Health::Error)
    {
        if command == CommandMode::Normal && !mode.accepts_control() {
            return Err(format!(
                "component {id} has operational mode {mode}, which accepts no \
                 commands; cannot set command-mode to normal"
            ));
        }
        runtime.command = command;
    }
    Ok(runtime)
}

fn build_component(id: u64, spec: &ComponentSpec) -> Result<Arc<dyn SimulatedComponent>, String> {
    Ok(match spec {
        ComponentSpec::GridConnectionPoint(s) => Arc::new(grid(id, s)),
        ComponentSpec::Meter(s) => Arc::new(meter(id, s)?),
        ComponentSpec::Battery(s) => Arc::new(battery(id, s)?),
        ComponentSpec::BatteryInverter(s) => Arc::new(battery_inverter(id, s)?),
        ComponentSpec::SolarInverter(s) => Arc::new(solar_inverter(id, s)?),
        ComponentSpec::EvCharger(s) => Arc::new(ev_charger(id, s)?),
        ComponentSpec::SteamBoiler(s) => Arc::new(steam_boiler(id, s)?),
        ComponentSpec::Chp(s) => Arc::new(marker(id, Category::Chp, s)),
        ComponentSpec::WindTurbine(s) => Arc::new(marker(id, Category::WindTurbine, s)),
        ComponentSpec::PowerTransformer(s) => Arc::new(marker(id, Category::PowerTransformer, s)),
        ComponentSpec::Breaker(s) => Arc::new(marker(id, Category::Breaker, s)),
    })
}

/// A number of seconds as a `Duration`: negative, NaN, infinite and
/// overflowing values are refused, naming `field`.
fn secs(field: &str, s: f64) -> Result<Duration, String> {
    Duration::try_from_secs_f64(s)
        .map_err(|_| format!("{field} must be a non-negative number of seconds, got {s}"))
}

/// `field` as a `Duration`, `default` when unset.
fn secs_or(field: &str, s: Option<f64>, default: Duration) -> Result<Duration, String> {
    s.map_or(Ok(default), |s| secs(field, s))
}

fn interval(s: Option<f64>) -> Result<Duration, String> {
    secs_or("interval_s", s, Duration::from_secs(1))
}

/// A ramp rate must be a non-negative, non-NaN number: a negative
/// rate walks away from the target without bound and NaN never
/// arrives.
fn ramp_rate(field: &str, v: f64) -> Result<f32, String> {
    if v.is_nan() || v < 0.0 {
        return Err(format!("{field} must be a non-negative rate, got {v}"));
    }
    Ok(v as f32)
}

fn jitter(v: Option<f64>) -> f32 {
    v.unwrap_or(0.0) as f32
}

/// The reactive fields both inverters take, applied over the config's
/// own values. A limit of 0 or less turns that limit off; an unset
/// one keeps the config's.
struct Reactive {
    pf_limit: Option<f64>,
    apparent_va: Option<f64>,
    command_delay_s: Option<f64>,
    ramp_rate_var_per_s: Option<f64>,
}

impl Reactive {
    fn apply(
        self,
        caps: &mut ReactiveCapability,
        command_delay: &mut Duration,
        ramp: &mut f32,
    ) -> Result<(), String> {
        let merge = |input: Option<f64>, fallback: Option<f32>| match input {
            Some(v) if v > 0.0 => Some(v as f32),
            Some(_) => None,
            None => fallback,
        };
        caps.pf_limit = merge(self.pf_limit, caps.pf_limit);
        caps.apparent_va = merge(self.apparent_va, caps.apparent_va);
        *command_delay = secs_or(
            "reactive_command_delay_s",
            self.command_delay_s,
            *command_delay,
        )?;
        if let Some(v) = self.ramp_rate_var_per_s {
            *ramp = ramp_rate("reactive_ramp_rate_var_per_s", v)?;
        }
        Ok(())
    }
}

fn grid(id: u64, s: &GridConnectionPointSpec) -> Grid {
    Grid::new(
        id,
        s.rated_fuse_current_a.unwrap_or(0),
        Bounds {
            lower: s.rated_lower_w.map(|v| v as f32),
            upper: s.rated_upper_w.map(|v| v as f32),
        },
        jitter(s.stream_jitter_pct),
    )
}

fn meter(id: u64, s: &MeterSpec) -> Result<Meter, String> {
    if s.reactive_power_var.is_some() && s.power_factor.is_some() {
        return Err("reactive_power_var and power_factor cannot both be set".to_string());
    }
    if s.leading.is_some() && s.power_factor.is_none() {
        return Err("leading needs power_factor".to_string());
    }
    let reactive = match (s.power_factor, s.reactive_power_var) {
        (Some(pf), _) => {
            if !(pf > 0.0 && pf <= 1.0) {
                return Err(format!("power_factor must be in (0, 1], got {pf}"));
            }
            Some(ReactiveSource::PowerFactor {
                pf: pf as f32,
                leading: s.leading.unwrap_or(false),
            })
        }
        (None, Some(var)) => Some(ReactiveSource::Var(DynamicScalar::constant(var as f32))),
        (None, None) => None,
    };
    Ok(Meter::new(
        id,
        interval(s.interval_s)?,
        s.power_w.map(|w| DynamicScalar::constant(w as f32)),
        reactive,
        jitter(s.stream_jitter_pct),
        s.hidden.unwrap_or(false),
    ))
}

fn battery(id: u64, s: &BatterySpec) -> Result<Battery, String> {
    let mut cfg = BatteryConfig::default();
    for (field, value) in [
        (&mut cfg.capacity_wh, s.capacity_wh),
        (&mut cfg.initial_soc_pct, s.initial_soc_pct),
        (&mut cfg.soc_lower_pct, s.soc_lower_pct),
        (&mut cfg.soc_upper_pct, s.soc_upper_pct),
        (&mut cfg.voltage_v, s.voltage_v),
        (&mut cfg.rated_lower_w, s.rated_lower_w),
        (&mut cfg.rated_upper_w, s.rated_upper_w),
        (&mut cfg.soc_protect_margin_pct, s.soc_protect_margin_pct),
        (&mut cfg.stream_jitter_pct, s.stream_jitter_pct),
    ] {
        if let Some(v) = value {
            *field = v as f32;
        }
    }
    Ok(Battery::new(id, interval(s.interval_s)?, cfg))
}

fn battery_inverter(id: u64, s: &BatteryInverterSpec) -> Result<BatteryInverter, String> {
    let mut cfg = BatteryInverterConfig::default();
    if let Some(v) = s.rated_lower_w {
        cfg.rated_lower_w = v as f32;
    }
    if let Some(v) = s.rated_upper_w {
        cfg.rated_upper_w = v as f32;
    }
    cfg.command_delay = secs_or("command_delay_s", s.command_delay_s, cfg.command_delay)?;
    if let Some(v) = s.ramp_rate_w_per_s {
        cfg.ramp_rate_w_per_s = ramp_rate("ramp_rate_w_per_s", v)?;
    }
    cfg.device_delay = secs_or("device_delay_s", s.device_delay_s, cfg.device_delay)?;
    cfg.stream_jitter_pct = jitter(s.stream_jitter_pct);
    Reactive {
        pf_limit: s.reactive_pf_limit,
        apparent_va: s.reactive_apparent_va,
        command_delay_s: s.reactive_command_delay_s,
        ramp_rate_var_per_s: s.reactive_ramp_rate_var_per_s,
    }
    .apply(
        &mut cfg.reactive,
        &mut cfg.reactive_command_delay,
        &mut cfg.reactive_ramp_rate_var_per_s,
    )?;
    Ok(BatteryInverter::new(id, interval(s.interval_s)?, cfg))
}

fn solar_inverter(id: u64, s: &SolarInverterSpec) -> Result<SolarInverter, String> {
    let mut cfg = SolarInverterConfig {
        // Unset, the array follows the site's weather.
        sunlight_pct: s.sunlight_pct.map(|v| v as f32),
        ..Default::default()
    };
    if let Some(v) = s.weather_lag_s {
        cfg.weather_lag = Some(
            crate::sim::weather::validate::weather_lag_s(v)
                .map_err(|e| format!("weather_lag_s {e}"))?,
        );
    }
    if let Some(v) = s.weather_jitter_pct {
        if !(v.is_finite() && (0.0..100.0).contains(&v)) {
            return Err(format!(
                "weather_jitter_pct must be within [0, 100), got {v}"
            ));
        }
        cfg.weather_jitter_pct = v as f32;
    }
    if let Some(v) = s.rated_lower_w {
        cfg.rated_lower_w = v as f32;
    }
    if let Some(v) = s.rated_upper_w {
        cfg.rated_upper_w = v as f32;
    }
    cfg.array_peak_w = match s.array_peak_w {
        Some(v) if !(v.is_finite() && v > 0.0) => {
            return Err(format!("array_peak_w must be a positive number, got {v}"));
        }
        Some(v) => v as f32,
        // A matched array.
        None => cfg.rated_lower_w.abs(),
    };
    cfg.command_delay = secs_or("command_delay_s", s.command_delay_s, cfg.command_delay)?;
    if let Some(v) = s.ramp_rate_w_per_s {
        cfg.ramp_rate_w_per_s = ramp_rate("ramp_rate_w_per_s", v)?;
    }
    cfg.device_delay = secs_or("device_delay_s", s.device_delay_s, cfg.device_delay)?;
    cfg.stream_jitter_pct = jitter(s.stream_jitter_pct);
    Reactive {
        pf_limit: s.reactive_pf_limit,
        apparent_va: s.reactive_apparent_va,
        command_delay_s: s.reactive_command_delay_s,
        ramp_rate_var_per_s: s.reactive_ramp_rate_var_per_s,
    }
    .apply(
        &mut cfg.reactive,
        &mut cfg.reactive_command_delay,
        &mut cfg.reactive_ramp_rate_var_per_s,
    )?;
    Ok(SolarInverter::new(id, interval(s.interval_s)?, cfg))
}

fn ev_charger(id: u64, s: &EvChargerSpec) -> Result<EvCharger, String> {
    let mut cfg = EvChargerConfig::default();
    if let Some(v) = s.rated_lower_w {
        cfg.rated_lower_w = v as f32;
    }
    if let Some(v) = s.rated_upper_w {
        cfg.rated_upper_w = v as f32;
    }
    cfg.command_delay = secs_or("command_delay_s", s.command_delay_s, cfg.command_delay)?;
    if let Some(v) = s.ramp_rate_w_per_s {
        cfg.ramp_rate_w_per_s = ramp_rate("ramp_rate_w_per_s", v)?;
    }
    cfg.device_delay = secs_or("device_delay_s", s.device_delay_s, cfg.device_delay)?;
    cfg.stream_jitter_pct = jitter(s.stream_jitter_pct);
    cfg.resume_on_recovery = s.resume_on_recovery.unwrap_or(false);
    if let Some(v) = s.phases {
        if v != 1 && v != 3 {
            return Err(format!("phases must be 1 or 3, got {v}"));
        }
        cfg.phases = v;
    }
    if let Some(v) = s.idle {
        cfg.idle = v;
    }
    Ok(EvCharger::new(id, interval(s.interval_s)?, cfg))
}

fn steam_boiler(id: u64, s: &SteamBoilerSpec) -> Result<SteamBoiler, String> {
    let mut cfg = SteamBoilerConfig::default();
    for (field, value) in [
        (&mut cfg.rated_lower_w, s.rated_lower_w),
        (&mut cfg.rated_upper_w, s.rated_upper_w),
        (&mut cfg.target_bar, s.target_bar),
        (&mut cfg.max_bar, s.max_bar),
        (&mut cfg.capacity_wh_per_bar, s.capacity_wh_per_bar),
        (&mut cfg.wh_per_kg, s.wh_per_kg),
        (&mut cfg.demand_kg_per_s, s.demand_kg_per_s),
    ] {
        if let Some(v) = value {
            *field = v as f32;
        }
    }
    cfg.initial_bar = s.initial_bar.map(|v| v as f32);
    cfg.command_delay = secs_or("command_delay_s", s.command_delay_s, cfg.command_delay)?;
    if let Some(v) = s.ramp_rate_w_per_s {
        cfg.ramp_rate_w_per_s = ramp_rate("ramp_rate_w_per_s", v)?;
    }
    cfg.device_delay = secs_or("device_delay_s", s.device_delay_s, cfg.device_delay)?;
    cfg.stream_jitter_pct = jitter(s.stream_jitter_pct);
    if !(cfg.max_bar >= cfg.target_bar && cfg.target_bar > 0.0) {
        return Err(format!(
            "max_bar must be at least target_bar, and target_bar above 0; got max_bar {} and \
             target_bar {}",
            cfg.max_bar, cfg.target_bar
        ));
    }
    Ok(SteamBoiler::new(id, interval(s.interval_s)?, cfg))
}

fn marker(id: u64, category: Category, s: &MarkerSpec) -> Marker {
    Marker::new(id, category, jitter(s.stream_jitter_pct))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::sim::OperationalMode;
    use crate::spec::TopologyDoc;

    /// The position of each id in `components`, for order checks.
    fn positions(built: &BuiltTopology) -> HashMap<u64, usize> {
        built
            .components
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id(), i))
            .collect()
    }

    fn spec(json: &str) -> MicrogridSpec {
        let doc = format!(r#"{{"schema_version": 1, "microgrids": [{json}]}}"#);
        TopologyDoc::from_json(&doc).unwrap().microgrids.remove(0)
    }

    fn build(json: &str) -> Result<BuiltTopology, String> {
        build_topology(&spec(json), &Defaults::default()).map_err(|e| e.to_string())
    }

    fn refused(json: &str) -> String {
        build(json).unwrap_err()
    }

    #[test]
    fn every_type_builds_from_a_minimal_spec() {
        let built = build(
            r#"{"components": [
                {"type": "grid_connection_point", "id": 1},
                {"type": "meter", "id": 2},
                {"type": "battery_inverter", "id": 3},
                {"type": "battery", "id": 4},
                {"type": "solar_inverter", "id": 5},
                {"type": "ev_charger", "id": 6},
                {"type": "steam_boiler", "id": 7},
                {"type": "chp", "id": 8},
                {"type": "wind_turbine", "id": 9},
                {"type": "power_transformer", "id": 10},
                {"type": "breaker", "id": 11}
            ],
            "connections": [[1, 2], [2, 3], [3, 4], [2, 5], [2, 6], [2, 7], [2, 8],
                            [1, 10], [10, 11], [11, 9]]}"#,
        )
        .unwrap();
        let kinds: BTreeMap<u64, (Category, Option<&str>)> = built
            .components
            .iter()
            .map(|c| (c.id(), (c.category(), c.subtype())))
            .collect();
        let expected = [
            (1, Category::Grid, None),
            (2, Category::Meter, None),
            (3, Category::Inverter, Some("battery")),
            (4, Category::Battery, None),
            (5, Category::Inverter, Some("solar")),
            (6, Category::EvCharger, None),
            (7, Category::SteamBoiler, None),
            (8, Category::Chp, None),
            (9, Category::WindTurbine, None),
            (10, Category::PowerTransformer, None),
            (11, Category::Breaker, None),
        ];
        assert_eq!(kinds.len(), expected.len());
        for (id, category, subtype) in expected {
            let (got, sub) = kinds[&id];
            assert_eq!(got, category, "component {id}");
            if subtype.is_some() {
                assert_eq!(sub, subtype, "component {id}");
            }
        }
        let at = positions(&built);
        for (parent, child) in &built.connections {
            assert!(at[child] < at[parent], "{child} before {parent}");
        }
        assert_eq!(built.physics_tick_ms, 100);
        assert_eq!(built.filled.len(), 11);
        let ComponentSpec::BatteryInverter(inv) = &built.filled[&3] else {
            unreachable!()
        };
        assert_eq!(
            inv.command_delay_s,
            Some(1.5),
            "the defaults were filled in"
        );
        assert_eq!(built.initial[&4].runtime.health, Health::Ok);
    }

    #[test]
    fn spec_values_reach_the_component() {
        let built = build(
            r#"{"physics_tick_s": 0.25, "components": [
                {"type": "grid_connection_point", "id": 1, "rated_fuse_current_a": 63},
                {"type": "meter", "id": 2, "power_w": 2000.0, "name": "load"},
                {"type": "battery", "id": 4, "initial_soc_pct": 30.0},
                {"type": "steam_boiler", "id": 7, "initial_bar": 9.5}
            ], "connections": [[1, 2]]}"#,
        )
        .unwrap();
        assert_eq!(built.physics_tick_ms, 250);
        let get = |id: u64| {
            built
                .components
                .iter()
                .find(|c| c.id() == id)
                .unwrap()
                .clone()
        };
        assert_eq!(get(1).rated_fuse_current(), Some(63));
        let power = get(2).meter_drive().unwrap().meter_power_reading().unwrap();
        assert_eq!(power.value, 2000.0);
        assert_eq!(built.initial[&2].name.as_deref(), Some("load"));
        assert_eq!(get(4).dc_storage().unwrap().soc_pct(), 30.0);
        assert_eq!(
            get(7).physics_state(),
            Some(crate::sim::PhysicsState::Boiler { pressure_bar: 9.5 })
        );
    }

    /// One given side bounds the grid on that side; the other stays
    /// open, as in Lisp.
    #[test]
    fn a_one_sided_grid_band_bounds_that_side_only() {
        let rated = |side: &str| {
            let built = build(&format!(
                r#"{{"components": [
                    {{"type": "grid_connection_point", "id": 1, {side}}}
                ]}}"#
            ))
            .unwrap();
            built.components[0].rated_active_bounds()
        };
        assert_eq!(
            rated(r#""rated_upper_w": 40000.0"#),
            Some(Bounds {
                lower: None,
                upper: Some(40_000.0)
            })
        );
        assert_eq!(
            rated(r#""rated_lower_w": -25000.0"#),
            Some(Bounds {
                lower: Some(-25_000.0),
                upper: None
            })
        );
    }

    #[test]
    fn a_diamond_builds_the_shared_battery_once_before_both_inverters() {
        let built = build(
            r#"{"components": [
                {"type": "grid_connection_point", "id": 1},
                {"type": "battery_inverter", "id": 2},
                {"type": "battery_inverter", "id": 3},
                {"type": "battery", "id": 4}
            ], "connections": [[1, 2], [1, 3], [2, 4], [3, 4]]}"#,
        )
        .unwrap();
        let ids: Vec<u64> = built.components.iter().map(|c| c.id()).collect();
        assert_eq!(ids.iter().filter(|&&id| id == 4).count(), 1, "{ids:?}");
        let at = positions(&built);
        assert!(at[&4] < at[&2] && at[&4] < at[&3], "{ids:?}");
    }

    #[test]
    fn a_missing_or_repeated_id_is_refused() {
        let msg = refused(r#"{"components": [{"type": "meter", "id": 1}, {"type": "meter"}]}"#);
        assert_eq!(msg, "components[1] (meter) has no id");
        let msg = refused(r#"{"components": [{"type": "meter", "id": 0}]}"#);
        assert!(msg.contains("components[0] (meter) has id 0"), "{msg}");
        let msg = refused(
            r#"{"components": [{"type": "meter", "id": 3}, {"type": "battery", "id": 3}]}"#,
        );
        assert_eq!(msg, "component id 3 appears twice");
    }

    #[test]
    fn a_bad_connection_is_refused_naming_it() {
        let two = r#"{"type": "meter", "id": 1}, {"type": "meter", "id": 2}"#;
        let msg = refused(&format!(
            r#"{{"components": [{two}], "connections": [[1, 9]]}}"#
        ));
        assert_eq!(
            msg,
            "connection 1 → 9 names component 9, which is not in components"
        );
        let msg = refused(&format!(
            r#"{{"components": [{two}], "connections": [[1, 1]]}}"#
        ));
        assert_eq!(msg, "connection 1 → 1 is a self-edge");
        let msg = refused(&format!(
            r#"{{"components": [{two}], "connections": [[1, 2], [1, 2]]}}"#
        ));
        assert_eq!(msg, "connection 1 → 2 appears twice");
    }

    #[test]
    fn a_cycle_is_refused() {
        let msg = refused(
            r#"{"components": [
                {"type": "meter", "id": 1}, {"type": "meter", "id": 2},
                {"type": "meter", "id": 3}
            ], "connections": [[1, 2], [2, 3], [3, 2]]}"#,
        );
        assert!(msg.starts_with("the connections contain a cycle"), "{msg}");
        assert!(msg.contains("2 → 3") || msg.contains("3 → 2"), "{msg}");
    }

    #[test]
    fn values_the_lisp_make_functions_refuse_are_refused() {
        for (component, expected) in [
            (
                r#"{"type": "meter", "id": 5, "reactive_power_var": 1.0, "power_factor": 0.9}"#,
                "component 5 (meter): reactive_power_var and power_factor cannot both be set",
            ),
            (
                r#"{"type": "meter", "id": 5, "leading": true}"#,
                "component 5 (meter): leading needs power_factor",
            ),
            (
                r#"{"type": "meter", "id": 5, "power_factor": 0.0}"#,
                "component 5 (meter): power_factor must be in (0, 1], got 0",
            ),
            (
                r#"{"type": "meter", "id": 5, "interval_s": -1}"#,
                "component 5 (meter): interval_s must be a non-negative number of seconds, \
                 got -1",
            ),
            (
                r#"{"type": "battery_inverter", "id": 5, "ramp_rate_w_per_s": -1}"#,
                "component 5 (battery_inverter): ramp_rate_w_per_s must be a non-negative \
                 rate, got -1",
            ),
            (
                r#"{"type": "solar_inverter", "id": 5, "array_peak_w": 0}"#,
                "component 5 (solar_inverter): array_peak_w must be a positive number, got 0",
            ),
            (
                r#"{"type": "solar_inverter", "id": 5, "weather_jitter_pct": 100}"#,
                "component 5 (solar_inverter): weather_jitter_pct must be within [0, 100), \
                 got 100",
            ),
            (
                r#"{"type": "ev_charger", "id": 5, "phases": 2}"#,
                "component 5 (ev_charger): phases must be 1 or 3, got 2",
            ),
            (
                r#"{"type": "steam_boiler", "id": 5, "target_bar": 8, "max_bar": 7}"#,
                "component 5 (steam_boiler): max_bar must be at least target_bar, and \
                 target_bar above 0; got max_bar 7 and target_bar 8",
            ),
        ] {
            let msg = refused(&format!(r#"{{"components": [{component}]}}"#));
            assert_eq!(msg, expected);
        }
        let msg = refused(r#"{"physics_tick_s": -0.1}"#);
        assert!(msg.starts_with("physics_tick_s must be"), "{msg}");
    }

    #[test]
    fn a_tiny_physics_tick_is_one_millisecond() {
        assert_eq!(
            build(r#"{"physics_tick_s": 0.0001}"#)
                .unwrap()
                .physics_tick_ms,
            1
        );
    }

    #[test]
    fn modes_the_operational_mode_forbids_are_refused() {
        let msg = refused(
            r#"{"components": [{"type": "meter", "id": 5,
                "operational_mode": "inactive", "telemetry_mode": "normal"}]}"#,
        );
        assert_eq!(
            msg,
            "component 5 (meter): component 5 has operational mode inactive, which streams no \
             telemetry; cannot set telemetry-mode to normal"
        );
    }

    /// `runtime_from_modes` agrees with the site's setters, applied in the
    /// order the Lisp make functions apply them, on every combination.
    #[test]
    fn runtime_from_modes_agrees_with_the_site_on_every_combination() {
        use crate::sim::MicrogridSite;
        fn all<T: Copy>(values: &[T]) -> Vec<Option<T>> {
            std::iter::once(None)
                .chain(values.iter().copied().map(Some))
                .collect()
        }
        let operational = all(&[
            OperationalMode::Unspecified,
            OperationalMode::Inactive,
            OperationalMode::TelemetryOnly,
            OperationalMode::ControlOnly,
            OperationalMode::ControlAndTelemetry,
        ]);
        let health = all(&[Health::Ok, Health::Error, Health::Standby]);
        let telemetry = all(&[
            TelemetryMode::Normal,
            TelemetryMode::Silent,
            TelemetryMode::Closed,
            TelemetryMode::ErrorEmpty,
            TelemetryMode::NotFound,
        ]);
        let command = all(&[
            CommandMode::Normal,
            CommandMode::Timeout,
            CommandMode::Error,
            CommandMode::OverBound,
        ]);
        let mut cases = 0;
        let mut refusals = 0;
        for &o in &operational {
            for &h in &health {
                for &t in &telemetry {
                    for &c in &command {
                        let modes = Modes {
                            operational_mode: o,
                            health: h,
                            telemetry_mode: t,
                            command_mode: c,
                        };
                        let site = MicrogridSite::new();
                        site.register(Marker::new(1, Category::Chp, 0.0));
                        let applied = (|| {
                            if let Some(o) = o {
                                site.set_operational_mode(1, o)?;
                            }
                            if let Some(h) = h {
                                site.set_health(1, h)?;
                            }
                            if let Some(t) = t {
                                site.set_telemetry_mode(1, t)?;
                            }
                            if let Some(c) = c
                                && h != Some(Health::Error)
                            {
                                site.set_command_mode(1, c)?;
                            }
                            Ok::<_, String>(site.runtime_of(1))
                        })();
                        let checked = runtime_from_modes(1, modes);
                        assert_eq!(checked, applied, "{modes:?}");
                        cases += 1;
                        refusals += usize::from(checked.is_err());
                    }
                }
            }
        }
        assert_eq!(cases, 6 * 4 * 6 * 5);
        assert!(refusals > 0);
    }
}
