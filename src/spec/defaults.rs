//! The per-type defaults the engine fills into a spec before it
//! builds it, served on `GET /api/defaults`. They sit on top of each
//! component's own `*Config::default()`, so a field with no entry
//! here keeps the component's default.
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
    /// The defaults table.
    fn default() -> Self {
        // The types without physics share one entry.
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
    use super::*;

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
