//! Inputs that belong to one kind of component (meter, solar
//! inverter, steam boiler, EV charger), set by the HTTP knob routes
//! (`drive`, `ev`). The reactive caps the inverters share are
//! `ReactiveLimits`.

use crate::sim::{
    component::ReactiveReading,
    ev_presets::{ConnectedEv, EvInfo},
};

/// A meter's driven sources: the active and reactive power it
/// publishes in place of measuring its children.
pub trait MeterDrive: Send + Sync {
    /// Override the active-power value the meter publishes.
    fn set_active_power_override(&self, p: f32);

    /// Drop the meter's active-power override, returning it to
    /// measuring its children's aggregate — the way back from
    /// [`Self::set_active_power_override`].
    fn clear_active_power_source(&self);

    /// Override the reactive-power value the meter publishes. The Q
    /// twin of [`Self::set_active_power_override`].
    fn set_reactive_power_override(&self, vars: f32);

    /// Replace the meter's reactive-power source with a power-factor
    /// derivation that tracks the meter's own live active power.
    ///
    /// Does NOT validate `pf`: every caller must enforce
    /// `pf ∈ (0.0, 1.0]` itself before calling (the HTTP drive op
    /// and the spec build do), because an out-of-range factor lands
    /// silently otherwise.
    fn set_power_factor(&self, pf: f32, leading: bool);

    /// Drop the meter's reactive-power override — whichever of
    /// `Var` / `PowerFactor` is set — returning it to summing its
    /// children's Q. The Q twin of
    /// [`Self::clear_active_power_source`].
    fn clear_reactive_power_source(&self);

    /// The meter's active-power override (W). `None` while the meter
    /// is measuring its children.
    fn meter_power_reading(&self) -> Option<f32>;

    /// The meter's reactive-power source knob — either a direct VAr
    /// value (mirrors [`Self::meter_power_reading`]'s shape) or a
    /// power-factor derivation from the meter's own live P. `None`
    /// while the meter is summing its children.
    fn meter_reactive_reading(&self) -> Option<ReactiveReading>;
}

/// The solar inverter's sunlight knob: the sunlight percentage (100
/// is full sun) that caps its output. One built without
/// `sunlight_pct` follows the site's weather until something drives
/// it.
pub trait SunlightDrive: Send + Sync {
    /// Drive the sunlight percentage with a constant. Displaces the
    /// weather until [`Self::clear_sunlight_source`].
    fn set_sunlight_pct(&self, pct: f32);

    /// Drop the driven percentage and go back to following the
    /// site's weather: the way back from [`Self::set_sunlight_pct`].
    fn clear_sunlight_source(&self);

    /// The live percentage, driven or read off the weather.
    fn sunlight_reading(&self) -> f32;
}

/// The steam boiler's driven inputs: its steam demand and its
/// pressure state.
pub trait SteamDrive: Send + Sync {
    /// Drive the steam demand (kg/s).
    fn set_steam_demand_kg_per_s(&self, kg_per_s: f32);

    /// Overwrite the pressure state (bar).
    fn set_pressure_bar(&self, bar: f32);

    /// The live demand (kg/s).
    fn demand_reading(&self) -> f32;

    /// The live pressure (bar), for the inspector knob.
    fn pressure_reading(&self) -> f32;

    /// The thermostat target (bar), for chart annotation.
    fn pressure_target_bar(&self) -> f32;
}

/// An EV charger's car port. The car is runtime state, plugged and
/// unplugged through `POST` / `DELETE .../component/{id}/ev`; it is
/// never part of the charger's spec.
pub trait EvPort: Send + Sync {
    /// Plug `ev` in. Errors when a car is already plugged in.
    fn plug_ev(&self, ev: ConnectedEv) -> Result<(), String>;

    /// Unplug the connected car. `false` when there was none.
    fn unplug_ev(&self) -> bool;

    /// The connected car and what the charger is doing with it, or
    /// `None` for an empty charger.
    fn ev_info(&self) -> Option<EvInfo>;

    /// Teleport the plugged car's state of charge to `pct` (clamped
    /// to 0..=100; a non-finite value is ignored). `false` when no
    /// car is plugged in, so the caller can say so.
    fn set_ev_soc_pct(&self, pct: f32) -> bool;
}
