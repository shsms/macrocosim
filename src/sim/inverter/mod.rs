pub mod battery_inverter;
pub mod solar_inverter;

pub use battery_inverter::BatteryInverter;
pub use solar_inverter::SolarInverter;

use crate::sim::{
    Category, MicrogridSite, Telemetry,
    bounds::VecBounds,
    component::power_state,
    meter::{per_phase_apparent_current, split_per_phase},
};

/// Telemetry shared by both inverters: P/Q with per-phase splits,
/// grid voltage/frequency, apparent currents, and the caller's own
/// envelopes. The inverters differ only in where `p` comes from
/// (measured AC output vs ramp state).
pub(crate) fn inverter_telemetry(
    id: u64,
    site: &MicrogridSite,
    p: f32,
    q: f32,
    active_power_bounds: VecBounds,
    reactive_power_bounds: (f32, f32),
) -> Telemetry {
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
        active_power_bounds: Some(active_power_bounds),
        reactive_power_bounds: Some(reactive_power_bounds),
        component_state: Some(power_state(p)),
        ..Default::default()
    }
}
