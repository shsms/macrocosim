/// `Display` for mode enums, spelt as serde spells them (`over_bound`):
/// the one name the topology document, the HTTP routes and the knob
/// events share.
macro_rules! display_as_serde {
    ($($ty:ty),+ $(,)?) => {$(
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match serde_json::to_value(self) {
                    Ok(serde_json::Value::String(name)) => f.write_str(&name),
                    _ => unreachable!("a mode enum serializes as a string"),
                }
            }
        }
    )+};
}

pub mod battery;
pub mod bounds;
pub mod component;
pub mod decay;
pub mod device_axis;
pub mod dispatch;
pub mod energy;
pub mod ev_charger;
pub mod ev_presets;
pub mod events;
pub mod frequency;
pub mod gateway;
pub mod gateway_axis;
pub mod graph_adapter;
pub mod grid;
pub mod history;
pub mod inverter;
pub mod marker;
pub mod meter;
pub mod microgrid_site;
pub mod microgrids;
pub mod physics_state;
pub mod ramp;
pub mod reactive;
pub(crate) mod recording;
pub(crate) mod recording_csv;
pub mod runtime;
pub(crate) mod scalar;
pub mod setpoints;
pub mod sim_clock;
pub mod site_import;
pub mod steam_boiler;
pub mod weather;

pub use battery::Battery;
pub use component::{
    AugmentError, Category, ComponentHandle, Controllable, DcStorage, EvPort, MeterDrive,
    OperationalMode, ReactiveLimits, SimulatedComponent, SteamDrive, SunlightDrive, Telemetry,
};
pub use energy::EnergyAccum;
pub use ev_charger::EvCharger;
pub use grid::Grid;
pub use inverter::{BatteryInverter, SolarInverter};
pub use marker::Marker;
pub use meter::{Meter, ReactiveSource};
pub use microgrid_site::MicrogridSite;
pub use physics_state::PhysicsState;
pub use steam_boiler::{SteamBoiler, SteamBoilerConfig};
