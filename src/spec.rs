//! The topology document: one JSON document that describes one or
//! more microgrids, as a flat list of components and the connections
//! between them. These types own its format; the JSON Schema in
//! [`schema`] is derived from them.
//!
//! A field is named after what it sets, with a unit suffix wherever
//! the value has a unit (`_w`, `_var`, `_va`, `_wh`, `_pct`, `_s`,
//! `_a`, `_bar`, `_kg_per_s`). A field left out takes the value of
//! the [`Defaults`] table, and without an entry there the component's
//! own default. Every struct refuses a field it does not know.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::sim::{
    Category, OperationalMode,
    ev_charger::EvIdle,
    runtime::{CommandMode, Health, TelemetryMode},
};

pub mod build;
pub mod defaults;
pub mod order;
pub mod schema;

pub use build::{BuiltTopology, InitialState, build_topology, runtime_from_modes};
pub use defaults::{Defaults, fill};

/// The `schema_version` this engine reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Why a topology document or a microgrid spec was refused. The
/// message names the field or the component id at fault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecError(String);

impl SpecError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpecError {}

/// One or more microgrids.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopologyDoc {
    /// The version of this format; this engine reads 1.
    pub schema_version: u32,
    pub microgrids: Vec<MicrogridSpec>,
}

impl TopologyDoc {
    /// Parse a topology document. The `schema_version` is checked
    /// before anything else, so a document of another version is
    /// refused for its version and not for a field this one lacks.
    pub fn from_json(text: &str) -> Result<Self, SpecError> {
        let value: serde_json::Value = serde_json::from_str(text)
            .map_err(|e| SpecError::new(format!("the topology document is not JSON: {e}")))?;
        Self::from_value(value)
    }

    /// [`Self::from_json`] for a document already parsed as JSON.
    pub fn from_value(value: serde_json::Value) -> Result<Self, SpecError> {
        match value.get("schema_version") {
            None => {
                return Err(SpecError::new(format!(
                    "schema_version is missing; this engine reads schema_version {SCHEMA_VERSION}"
                )));
            }
            Some(v) if v.as_u64() != Some(u64::from(SCHEMA_VERSION)) => {
                return Err(SpecError::new(format!(
                    "schema_version {v} is not supported; this engine reads schema_version \
                     {SCHEMA_VERSION}"
                )));
            }
            Some(_) => {}
        }
        serde_json::from_value(value).map_err(|e| SpecError::new(e.to_string()))
    }
}

/// One microgrid: its own settings, its components and the parent →
/// child connections between them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MicrogridSpec {
    /// The microgrid id; chosen by the engine when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The transmission system operator the microgrid reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tso: Option<String>,
    /// The port of the microgrid's Microgrid API server; chosen by
    /// the engine when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc_port: Option<u16>,
    /// Time between physics steps, in seconds; 0.1 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physics_tick_s: Option<f64>,
    #[serde(default)]
    pub components: Vec<ComponentSpec>,
    /// `[parent, child]` pairs of component ids. A component under
    /// two parents appears in two connections.
    #[serde(default)]
    pub connections: Vec<(u64, u64)>,
}

/// The starting values of a component's modes. The operational mode
/// is configuration; the other three are fault knobs it constrains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    pub operational_mode: Option<OperationalMode>,
    pub health: Option<Health>,
    pub telemetry_mode: Option<TelemetryMode>,
    pub command_mode: Option<CommandMode>,
}

/// Declare a component spec struct: the fields every component has,
/// then `$field`s of its own, each optional.
macro_rules! component_spec {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$fmeta:meta])* $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            /// The component id, unique across the engine. Every
            /// component the engine builds has one.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub id: Option<u64>,
            /// Display name.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub name: Option<String>,
            $(
                $(#[$fmeta])*
                #[serde(default, skip_serializing_if = "Option::is_none")]
                pub $field: Option<$ty>,
            )*
            /// Random spread of each telemetry interval, in percent.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub stream_jitter_pct: Option<f64>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub operational_mode: Option<OperationalMode>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub health: Option<Health>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub telemetry_mode: Option<TelemetryMode>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub command_mode: Option<CommandMode>,
        }

        impl $name {
            /// Take each field still unset here from `defaults`. The
            /// id never comes from a default.
            fn fill_from(&mut self, defaults: &Self) {
                fn take<T: Clone>(field: &mut Option<T>, default: &Option<T>) {
                    if field.is_none() {
                        field.clone_from(default);
                    }
                }
                take(&mut self.name, &defaults.name);
                $( take(&mut self.$field, &defaults.$field); )*
                take(&mut self.stream_jitter_pct, &defaults.stream_jitter_pct);
                take(&mut self.operational_mode, &defaults.operational_mode);
                take(&mut self.health, &defaults.health);
                take(&mut self.telemetry_mode, &defaults.telemetry_mode);
                take(&mut self.command_mode, &defaults.command_mode);
            }

            /// Clear the fields a rebuild applies from the spec to
            /// every component, carried or not: the name, the health
            /// and the operational, telemetry and command modes.
            fn clear_applied(&mut self) {
                self.name = None;
                self.operational_mode = None;
                self.health = None;
                self.telemetry_mode = None;
                self.command_mode = None;
            }

            fn modes(&self) -> Modes {
                Modes {
                    operational_mode: self.operational_mode,
                    health: self.health,
                    telemetry_mode: self.telemetry_mode,
                    command_mode: self.command_mode,
                }
            }
        }
    };
}

component_spec! {
    /// Where the microgrid meets the grid.
    GridConnectionPointSpec {
        /// Fuse rating, in whole amperes.
        rated_fuse_current_a: u32,
        /// Lower end of the rated band; left out, that side is
        /// unbounded.
        rated_lower_w: f64,
        /// Upper end of the rated band; left out, that side is
        /// unbounded.
        rated_upper_w: f64,
    }
}

component_spec! {
    /// A meter. A meter without `power_w` reads the sum of its
    /// children.
    MeterSpec {
        /// Telemetry interval, in seconds.
        interval_s: f64,
        /// Active power.
        power_w: f64,
        /// Reactive power; not with `power_factor`.
        reactive_power_var: f64,
        /// Power factor in (0, 1] that sets the reactive power from
        /// the meter's own active power; not with
        /// `reactive_power_var`.
        power_factor: f64,
        /// A leading reactive power, of the opposite sign of the
        /// active power; needs `power_factor`.
        leading: bool,
        /// Left out of the Microgrid API's component lists; its
        /// parent still counts its power.
        hidden: bool,
    }
}

component_spec! {
    /// A battery, under one or more battery inverters.
    BatterySpec {
        interval_s: f64,
        /// Energy it holds when full.
        capacity_wh: f64,
        /// State of charge at the start.
        initial_soc_pct: f64,
        /// Lower end of the SoC window.
        soc_lower_pct: f64,
        /// Upper end of the SoC window.
        soc_upper_pct: f64,
        /// DC voltage.
        voltage_v: f64,
        rated_lower_w: f64,
        rated_upper_w: f64,
        /// Room inside each SoC limit where the bounds taper to 0.
        soc_protect_margin_pct: f64,
    }
}

component_spec! {
    /// A battery inverter; its children are batteries.
    BatteryInverterSpec {
        interval_s: f64,
        rated_lower_w: f64,
        rated_upper_w: f64,
        /// Delay before an active power command starts.
        command_delay_s: f64,
        /// How fast the active power moves.
        ramp_rate_w_per_s: f64,
        /// Time a command takes to reach the output, on both axes.
        device_delay_s: f64,
        /// `k` in |Q| <= k * |P|; 0 or less turns it off.
        reactive_pf_limit: f64,
        /// Limit on P^2 + Q^2 <= VA^2; 0 or less turns it off.
        reactive_apparent_va: f64,
        /// Delay before a reactive power command starts.
        reactive_command_delay_s: f64,
        /// How fast the reactive power moves.
        reactive_ramp_rate_var_per_s: f64,
    }
}

component_spec! {
    /// A solar inverter with its array.
    SolarInverterSpec {
        interval_s: f64,
        /// Sunlight held at this value; without it the array follows
        /// the site's weather.
        sunlight_pct: f64,
        rated_lower_w: f64,
        rated_upper_w: f64,
        /// Peak DC output of the panels, positive; the size of
        /// `rated_lower_w` when left out.
        array_peak_w: f64,
        /// How far behind the site's weather the array reads, 0 to
        /// 3600 s; a stable 0 to 60 s derived from the id when left
        /// out.
        weather_lag_s: f64,
        /// Random spread of each weather reading, 0 to under 100.
        weather_jitter_pct: f64,
        command_delay_s: f64,
        ramp_rate_w_per_s: f64,
        device_delay_s: f64,
        reactive_pf_limit: f64,
        reactive_apparent_va: f64,
        reactive_command_delay_s: f64,
        reactive_ramp_rate_var_per_s: f64,
    }
}

component_spec! {
    /// An EV charger. It starts with no car plugged in.
    EvChargerSpec {
        interval_s: f64,
        rated_lower_w: f64,
        rated_upper_w: f64,
        command_delay_s: f64,
        /// How fast the offered limit moves.
        ramp_rate_w_per_s: f64,
        device_delay_s: f64,
        /// The phases it is wired on: 1 or 3.
        phases: u8,
        /// What it offers with no command standing.
        idle: EvIdle,
        /// Keep the command through a health fault.
        resume_on_recovery: bool,
    }
}

component_spec! {
    /// A hybrid gas and electric steam boiler.
    SteamBoilerSpec {
        interval_s: f64,
        /// Steam demand.
        demand_kg_per_s: f64,
        rated_lower_w: f64,
        rated_upper_w: f64,
        /// Pressure the gas burner holds; above 0.
        target_bar: f64,
        /// Highest pressure; at least `target_bar`.
        max_bar: f64,
        /// Pressure at the start; `target_bar` when left out.
        initial_bar: f64,
        /// Energy to raise the pressure by 1 bar.
        capacity_wh_per_bar: f64,
        /// Energy to make 1 kg of steam.
        wh_per_kg: f64,
        command_delay_s: f64,
        /// How fast the electric power moves.
        ramp_rate_w_per_s: f64,
        device_delay_s: f64,
    }
}

component_spec! {
    /// A CHP, wind turbine, power transformer or breaker. It has no
    /// physics of its own: it completes the topology and sets the
    /// kind of the meters next to it.
    MarkerSpec {}
}

/// One component, tagged by its `type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComponentSpec {
    GridConnectionPoint(GridConnectionPointSpec),
    Meter(MeterSpec),
    Battery(BatterySpec),
    BatteryInverter(BatteryInverterSpec),
    SolarInverter(SolarInverterSpec),
    Chp(MarkerSpec),
    EvCharger(EvChargerSpec),
    SteamBoiler(SteamBoilerSpec),
    WindTurbine(MarkerSpec),
    PowerTransformer(MarkerSpec),
    Breaker(MarkerSpec),
}

/// Run `$body` with `$s` bound to the struct inside any variant.
macro_rules! each_variant {
    ($spec:expr, $s:ident => $body:expr) => {
        match $spec {
            ComponentSpec::GridConnectionPoint($s) => $body,
            ComponentSpec::Meter($s) => $body,
            ComponentSpec::Battery($s) => $body,
            ComponentSpec::BatteryInverter($s) => $body,
            ComponentSpec::SolarInverter($s) => $body,
            ComponentSpec::Chp($s) => $body,
            ComponentSpec::EvCharger($s) => $body,
            ComponentSpec::SteamBoiler($s) => $body,
            ComponentSpec::WindTurbine($s) => $body,
            ComponentSpec::PowerTransformer($s) => $body,
            ComponentSpec::Breaker($s) => $body,
        }
    };
}

impl ComponentSpec {
    /// Whether a Python script can write components under this one: whether
    /// the Python client's spec of this type takes successors. The topology
    /// document itself puts no such limit on connections.
    pub fn takes_successors(&self) -> bool {
        match self {
            ComponentSpec::GridConnectionPoint(_)
            | ComponentSpec::Meter(_)
            | ComponentSpec::BatteryInverter(_)
            | ComponentSpec::PowerTransformer(_)
            | ComponentSpec::Breaker(_) => true,
            ComponentSpec::Battery(_)
            | ComponentSpec::SolarInverter(_)
            | ComponentSpec::Chp(_)
            | ComponentSpec::EvCharger(_)
            | ComponentSpec::SteamBoiler(_)
            | ComponentSpec::WindTurbine(_) => false,
        }
    }

    pub fn id(&self) -> Option<u64> {
        each_variant!(self, s => s.id)
    }

    pub fn name(&self) -> Option<&str> {
        each_variant!(self, s => s.name.as_deref())
    }

    pub fn modes(&self) -> Modes {
        each_variant!(self, s => s.modes())
    }

    /// A copy of this spec without its name, health and modes, which
    /// every rebuild applies anyway. A rebuild carries a component's
    /// state when the carry keys of its old and new defaults-filled
    /// specs are equal.
    pub fn carry_key(&self) -> ComponentSpec {
        let mut key = self.clone();
        each_variant!(&mut key, s => s.clear_applied());
        key
    }

    /// The `type` tag, as the JSON spells it.
    pub fn type_name(&self) -> &'static str {
        match self {
            ComponentSpec::GridConnectionPoint(_) => "grid_connection_point",
            ComponentSpec::Meter(_) => "meter",
            ComponentSpec::Battery(_) => "battery",
            ComponentSpec::BatteryInverter(_) => "battery_inverter",
            ComponentSpec::SolarInverter(_) => "solar_inverter",
            ComponentSpec::Chp(_) => "chp",
            ComponentSpec::EvCharger(_) => "ev_charger",
            ComponentSpec::SteamBoiler(_) => "steam_boiler",
            ComponentSpec::WindTurbine(_) => "wind_turbine",
            ComponentSpec::PowerTransformer(_) => "power_transformer",
            ComponentSpec::Breaker(_) => "breaker",
        }
    }

    /// The category of the component this spec builds.
    pub fn category(&self) -> Category {
        match self {
            ComponentSpec::GridConnectionPoint(_) => Category::Grid,
            ComponentSpec::Meter(_) => Category::Meter,
            ComponentSpec::Battery(_) => Category::Battery,
            ComponentSpec::BatteryInverter(_) | ComponentSpec::SolarInverter(_) => {
                Category::Inverter
            }
            ComponentSpec::Chp(_) => Category::Chp,
            ComponentSpec::EvCharger(_) => Category::EvCharger,
            ComponentSpec::SteamBoiler(_) => Category::SteamBoiler,
            ComponentSpec::WindTurbine(_) => Category::WindTurbine,
            ComponentSpec::PowerTransformer(_) => Category::PowerTransformer,
            ComponentSpec::Breaker(_) => Category::Breaker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A number written back the way `GET /spec` writes it reads as
    /// the same `f64`, so a spec sent back unchanged is equal and
    /// carries.
    #[test]
    fn a_number_round_trips_exactly() {
        let text = r#"{"schema_version": 1, "microgrids": [{"components": [
            {"type": "battery", "id": 3, "capacity_wh": 9.4484577729767469e-2}]}]}"#;
        let doc = TopologyDoc::from_json(text).unwrap();
        let again = TopologyDoc::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(doc, again);
    }

    fn err(text: &str) -> String {
        TopologyDoc::from_json(text).unwrap_err().to_string()
    }

    #[test]
    fn the_design_example_reads_and_writes_back() {
        let text = r#"{
          "schema_version": 1,
          "microgrids": [
            {
              "id": 7, "name": "plant", "grpc_port": null,
              "components": [
                {"type": "grid_connection_point", "id": 1, "rated_fuse_current_a": 63},
                {"type": "meter", "id": 2, "power_w": 2000.0},
                {"type": "battery_inverter", "id": 3,
                 "rated_lower_w": -5000.0, "rated_upper_w": 5000.0},
                {"type": "battery", "id": 4, "capacity_wh": 100000.0,
                 "initial_soc_pct": 50.0}
              ],
              "connections": [[1, 2], [1, 3], [3, 4]]
            }
          ]
        }"#;
        let doc = TopologyDoc::from_json(text).unwrap();
        let mg = &doc.microgrids[0];
        assert_eq!(mg.id, Some(7));
        assert_eq!(mg.grpc_port, None);
        assert_eq!(mg.connections, vec![(1, 2), (1, 3), (3, 4)]);
        let ComponentSpec::GridConnectionPoint(gcp) = &mg.components[0] else {
            panic!("{:?}", mg.components[0]);
        };
        assert_eq!(gcp.rated_fuse_current_a, Some(63));
        assert_eq!(mg.components[3].type_name(), "battery");
        let again = TopologyDoc::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(again, doc);
    }

    #[test]
    fn modes_read_in_snake_case() {
        let doc = TopologyDoc::from_json(
            r#"{"schema_version": 1, "microgrids": [{"components": [
                {"type": "meter", "id": 1, "operational_mode": "telemetry_only",
                 "health": "standby", "telemetry_mode": "error_empty",
                 "command_mode": "over_bound"},
                {"type": "ev_charger", "id": 2, "idle": "full"}
            ]}]}"#,
        )
        .unwrap();
        let modes = doc.microgrids[0].components[0].modes();
        assert_eq!(
            modes,
            Modes {
                operational_mode: Some(OperationalMode::TelemetryOnly),
                health: Some(Health::Standby),
                telemetry_mode: Some(TelemetryMode::ErrorEmpty),
                command_mode: Some(CommandMode::OverBound),
            }
        );
        let ComponentSpec::EvCharger(ev) = &doc.microgrids[0].components[1] else {
            unreachable!()
        };
        assert_eq!(ev.idle, Some(EvIdle::Full));
    }

    #[test]
    fn an_unknown_field_is_refused_by_name() {
        let msg = err(r#"{"schema_version": 1, "microgrids": [{"components": [
                {"type": "battery", "id": 1, "capacity": 5}]}]}"#);
        assert!(msg.contains("unknown field `capacity`"), "{msg}");
        let msg = err(r#"{"schema_version": 1, "microgrids": [{"ports": 1}]}"#);
        assert!(msg.contains("unknown field `ports`"), "{msg}");
        let msg = err(r#"{"schema_version": 1, "microgrids": [], "extra": true}"#);
        assert!(msg.contains("unknown field `extra`"), "{msg}");
    }

    #[test]
    fn an_unknown_type_is_refused_by_name() {
        let msg = err(r#"{"schema_version": 1, "microgrids": [{"components": [
                {"type": "flux_capacitor", "id": 1}]}]}"#);
        assert!(msg.contains("unknown variant `flux_capacitor`"), "{msg}");
    }

    #[test]
    fn a_wrong_or_missing_schema_version_is_refused() {
        let msg = err(r#"{"schema_version": 2, "microgrids": [], "new_field": 1}"#);
        assert!(
            msg.contains("schema_version 2 is not supported"),
            "the version is named before the unknown field: {msg}"
        );
        let msg = err(r#"{"schema_version": "1", "microgrids": []}"#);
        assert!(
            msg.contains("schema_version \"1\" is not supported"),
            "{msg}"
        );
        let msg = err(r#"{"microgrids": []}"#);
        assert!(msg.contains("schema_version is missing"), "{msg}");
    }

    #[test]
    fn unset_fields_are_left_out_of_the_json() {
        let spec = ComponentSpec::Battery(BatterySpec {
            id: Some(4),
            capacity_wh: Some(1000.0),
            ..Default::default()
        });
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            serde_json::json!({"type": "battery", "id": 4, "capacity_wh": 1000.0})
        );
    }
}
