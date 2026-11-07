pub mod battery_inverter;
pub mod solar_inverter;

pub use battery_inverter::BatteryInverter;
pub use solar_inverter::SolarInverter;

use std::time::Duration;

use crate::sim::{
    Category, MicrogridSite, Telemetry,
    component::power_state,
    meter::{per_phase_apparent_current, split_per_phase},
};

/// The device delay a component gets when its spec gives none.
pub(crate) const DEFAULT_DEVICE_DELAY: Duration = Duration::from_millis(100);

/// Telemetry shared by both inverters: P/Q with per-phase splits,
/// grid voltage/frequency and apparent currents. The bounds come from
/// the gateway (`MicrogridSite::telemetry_of`). The inverters differ
/// only in where `p` comes from (measured AC output vs device
/// output).
pub(crate) fn inverter_telemetry(id: u64, site: &MicrogridSite, p: f32, q: f32) -> Telemetry {
    let grid = site.grid_state();
    let pp = split_per_phase(p, grid.voltage_per_phase);
    let qpp = split_per_phase(q, grid.voltage_per_phase);
    Telemetry {
        id,
        category: Some(Category::Inverter),
        active_power_w: Some(p),
        reactive_power_var: Some(q),
        per_phase_active_w: Some(pp),
        per_phase_reactive_var: Some(qpp),
        per_phase_voltage_v: Some(grid.voltage_per_phase),
        per_phase_current_a: Some(per_phase_apparent_current(pp, qpp, grid.voltage_per_phase)),
        frequency_hz: Some(grid.frequency_hz),
        component_state: Some(power_state(p)),
        ..Default::default()
    }
}
