//! The per-type defaults the engine fills into a spec before it builds
//! it: the values of `sim/defaults.lisp`. They sit on top of each
//! component's own `*Config::default()`, so a field with no entry here
//! keeps the component's default.
//!
//! Some fields have no entry on purpose, because leaving them unset
//! selects a behaviour of their own: a meter's `power_w` (it reads
//! the sum of its children) and reactive source, a solar inverter's
//! `sunlight_pct` (it follows the weather), `weather_lag_s` (derived
//! from the id) and `array_peak_w` (the size of `rated_lower_w`), and
//! a steam boiler's `initial_bar` (its `target_bar`).

use serde::Serialize;

use super::{
    BatteryInverterSpec, BatterySpec, ComponentSpec, EvChargerSpec, GridConnectionPointSpec,
    MarkerSpec, MeterSpec, MicrogridSpec, SolarInverterSpec, SteamBoilerSpec,
};
use crate::sim::runtime::Health;

/// One partial spec per component type: the fields it fills.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Defaults {
    pub grid_connection_point: GridConnectionPointSpec,
    pub meter: MeterSpec,
    pub battery: BatterySpec,
    pub battery_inverter: BatteryInverterSpec,
    pub solar_inverter: SolarInverterSpec,
    pub chp: MarkerSpec,
    pub ev_charger: EvChargerSpec,
    pub steam_boiler: SteamBoilerSpec,
    pub wind_turbine: MarkerSpec,
    pub power_transformer: MarkerSpec,
    pub breaker: MarkerSpec,
}

impl Default for Defaults {
    /// The table of `sim/defaults.lisp`.
    fn default() -> Self {
        // The types without physics share one entry, as the Lisp
        // `marker-defaults` plist does.
        let marker = MarkerSpec {
            stream_jitter_pct: Some(0.0),
            ..Default::default()
        };
        Self {
            grid_connection_point: GridConnectionPointSpec {
                rated_fuse_current_a: Some(100),
                stream_jitter_pct: Some(1.0),
                ..Default::default()
            },
            meter: MeterSpec {
                interval_s: Some(0.2),
                stream_jitter_pct: Some(4.0),
                ..Default::default()
            },
            battery: BatterySpec {
                soc_protect_margin_pct: Some(10.0),
                stream_jitter_pct: Some(8.0),
                health: Some(Health::Ok),
                ..Default::default()
            },
            battery_inverter: BatteryInverterSpec {
                command_delay_s: Some(1.5),
                ramp_rate_w_per_s: Some(5000.0),
                stream_jitter_pct: Some(8.0),
                // 0 turns the PF limit off.
                reactive_pf_limit: Some(0.0),
                reactive_apparent_va: Some(32000.0),
                ..Default::default()
            },
            solar_inverter: SolarInverterSpec {
                ramp_rate_w_per_s: Some(2000.0),
                stream_jitter_pct: Some(5.0),
                ..Default::default()
            },
            chp: marker.clone(),
            ev_charger: EvChargerSpec {
                command_delay_s: Some(0.5),
                ramp_rate_w_per_s: Some(3000.0),
                stream_jitter_pct: Some(10.0),
                ..Default::default()
            },
            steam_boiler: SteamBoilerSpec {
                command_delay_s: Some(0.5),
                ramp_rate_w_per_s: Some(50000.0),
                stream_jitter_pct: Some(10.0),
                ..Default::default()
            },
            wind_turbine: marker.clone(),
            power_transformer: marker.clone(),
            breaker: marker,
        }
    }
}

impl Defaults {
    /// `spec` with every field it leaves unset taken from this table.
    pub fn fill_component(&self, spec: &ComponentSpec) -> ComponentSpec {
        let mut out = spec.clone();
        match &mut out {
            ComponentSpec::GridConnectionPoint(s) => s.fill_from(&self.grid_connection_point),
            ComponentSpec::Meter(s) => s.fill_from(&self.meter),
            ComponentSpec::Battery(s) => s.fill_from(&self.battery),
            ComponentSpec::BatteryInverter(s) => s.fill_from(&self.battery_inverter),
            ComponentSpec::SolarInverter(s) => s.fill_from(&self.solar_inverter),
            ComponentSpec::Chp(s) => s.fill_from(&self.chp),
            ComponentSpec::EvCharger(s) => s.fill_from(&self.ev_charger),
            ComponentSpec::SteamBoiler(s) => s.fill_from(&self.steam_boiler),
            ComponentSpec::WindTurbine(s) => s.fill_from(&self.wind_turbine),
            ComponentSpec::PowerTransformer(s) => s.fill_from(&self.power_transformer),
            ComponentSpec::Breaker(s) => s.fill_from(&self.breaker),
        }
        out
    }
}

/// `spec` with each component's unset fields taken from `defaults`.
pub fn fill(spec: &MicrogridSpec, defaults: &Defaults) -> MicrogridSpec {
    MicrogridSpec {
        components: spec
            .components
            .iter()
            .map(|c| defaults.fill_component(c))
            .collect(),
        id: spec.id,
        name: spec.name.clone(),
        tso: spec.tso.clone(),
        grpc_port: spec.grpc_port,
        physics_tick_s: spec.physics_tick_s,
        connections: spec.connections.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A flat plist as Lisp prints it, `(:a 1 :b ok)`, as key → value.
    fn parse_plist(printed: &str) -> BTreeMap<String, String> {
        let inner = printed
            .trim()
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or_else(|| panic!("not a list: {printed}"));
        let tokens: Vec<&str> = inner.split_whitespace().collect();
        assert!(tokens.len().is_multiple_of(2), "not a plist: {printed}");
        tokens
            .chunks(2)
            .map(|kv| (kv[0].to_string(), kv[1].to_string()))
            .collect()
    }

    /// One entry of the Rust table as Lisp keys and values: `_` becomes
    /// `-` in keys and in enum values, numbers stay numbers.
    fn as_plist(entry: &impl Serialize) -> BTreeMap<String, serde_json::Value> {
        let serde_json::Value::Object(map) = serde_json::to_value(entry).unwrap() else {
            unreachable!()
        };
        map.into_iter()
            .map(|(k, v)| {
                let v = match v {
                    serde_json::Value::String(s) => serde_json::Value::String(s.replace('_', "-")),
                    v => v,
                };
                (format!(":{}", k.replace('_', "-")), v)
            })
            .collect()
    }

    fn same(lisp: &BTreeMap<String, String>, rust: &BTreeMap<String, serde_json::Value>) -> bool {
        lisp.len() == rust.len()
            && lisp.iter().all(|(k, v)| match rust.get(k) {
                Some(serde_json::Value::Number(n)) => v.parse::<f64>().ok() == n.as_f64(),
                Some(serde_json::Value::String(s)) => v == s,
                _ => false,
            })
    }

    /// The table equals `sim/defaults.lisp` as a running interpreter
    /// reads it.
    #[tokio::test]
    async fn the_table_equals_the_lisp_defaults() {
        let dir = crate::test_dir::TestDir::new("spec-defaults");
        let file = dir.join("mg.lisp");
        std::fs::write(&file, "(make-microgrid :id 1 :topology (lambda ()))").unwrap();
        let (config, _clock) = crate::lisp::Config::new_headless(file.to_str().unwrap()).unwrap();
        let d = Defaults::default();
        let table: [(&str, BTreeMap<String, serde_json::Value>); 11] = [
            ("grid", as_plist(&d.grid_connection_point)),
            ("meter", as_plist(&d.meter)),
            ("battery", as_plist(&d.battery)),
            ("battery-inverter", as_plist(&d.battery_inverter)),
            ("solar-inverter", as_plist(&d.solar_inverter)),
            ("marker", as_plist(&d.chp)),
            ("ev-charger", as_plist(&d.ev_charger)),
            ("steam-boiler", as_plist(&d.steam_boiler)),
            ("marker", as_plist(&d.wind_turbine)),
            ("marker", as_plist(&d.power_transformer)),
            ("marker", as_plist(&d.breaker)),
        ];
        for (category, rust) in table {
            let printed = config
                .defaults_silent(&format!("{category}-defaults"))
                .unwrap();
            let lisp = parse_plist(&printed);
            assert!(
                same(&lisp, &rust),
                "{category}: Lisp {lisp:?} against Rust {rust:?}"
            );
        }
    }

    /// Every Lisp `*-defaults` plist has a row in the table above.
    #[test]
    fn every_lisp_defaults_category_is_compared() {
        for category in crate::lisp::DEFAULT_CATEGORIES {
            assert!(
                [
                    "grid",
                    "meter",
                    "battery",
                    "battery-inverter",
                    "solar-inverter",
                    "ev-charger",
                    "steam-boiler",
                    "marker"
                ]
                .contains(category),
                "{category}-defaults has no entry in the_table_equals_the_lisp_defaults"
            );
        }
    }

    #[test]
    fn fill_takes_unset_fields_and_keeps_set_ones() {
        let spec = MicrogridSpec {
            components: vec![ComponentSpec::BatteryInverter(BatteryInverterSpec {
                id: Some(3),
                command_delay_s: Some(0.25),
                ..Default::default()
            })],
            ..Default::default()
        };
        let filled = fill(&spec, &Defaults::default());
        let ComponentSpec::BatteryInverter(inv) = &filled.components[0] else {
            unreachable!()
        };
        assert_eq!(inv.id, Some(3));
        assert_eq!(inv.command_delay_s, Some(0.25), "a set field wins");
        assert_eq!(inv.ramp_rate_w_per_s, Some(5000.0));
        assert_eq!(inv.reactive_apparent_va, Some(32000.0));
        assert_eq!(inv.device_delay_s, None, "no entry, so the Rust default");
    }

    #[test]
    fn fields_unset_on_purpose_stay_unset() {
        let spec = MicrogridSpec {
            components: vec![
                ComponentSpec::Meter(MeterSpec {
                    id: Some(1),
                    ..Default::default()
                }),
                ComponentSpec::SolarInverter(SolarInverterSpec {
                    id: Some(2),
                    ..Default::default()
                }),
                ComponentSpec::SteamBoiler(SteamBoilerSpec {
                    id: Some(3),
                    ..Default::default()
                }),
            ],
            ..Default::default()
        };
        let filled = fill(&spec, &Defaults::default());
        let [
            ComponentSpec::Meter(meter),
            ComponentSpec::SolarInverter(pv),
            ComponentSpec::SteamBoiler(boiler),
        ] = filled.components.as_slice()
        else {
            unreachable!()
        };
        assert_eq!(meter.interval_s, Some(0.2), "the meter was filled");
        assert_eq!(meter.power_w, None);
        assert_eq!(meter.reactive_power_var, None);
        assert_eq!(meter.power_factor, None);
        assert_eq!(
            pv.ramp_rate_w_per_s,
            Some(2000.0),
            "the inverter was filled"
        );
        assert_eq!(pv.sunlight_pct, None);
        assert_eq!(pv.weather_lag_s, None);
        assert_eq!(pv.array_peak_w, None);
        assert_eq!(boiler.command_delay_s, Some(0.5), "the boiler was filled");
        assert_eq!(boiler.initial_bar, None);
    }
}
