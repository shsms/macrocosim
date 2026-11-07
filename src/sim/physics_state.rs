//! The physics state a component carries across a rebuild: what a
//! freshly built component would otherwise start over from its spec.
//! A rebuild reads it from the old component with
//! [`SimulatedComponent::physics_state`] and writes it into the new
//! one with [`SimulatedComponent::restore_physics_state`].
//!
//! [`SimulatedComponent::physics_state`]:
//! crate::sim::SimulatedComponent::physics_state
//! [`SimulatedComponent::restore_physics_state`]:
//! crate::sim::SimulatedComponent::restore_physics_state

use crate::sim::ev_presets::ConnectedEv;

/// One variant per component type with physics state of its own.
#[derive(Clone, Debug, PartialEq)]
pub enum PhysicsState {
    /// A battery's state of charge, in percent.
    Battery { soc_pct: f32 },
    /// The car plugged into an EV charger, with its own state of
    /// charge; `None` for an empty charger.
    Ev { connected: Option<ConnectedEv> },
    /// A steam boiler's pressure, in bar.
    Boiler { pressure_bar: f32 },
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::PhysicsState;
    use crate::sim::{
        Battery, EvCharger, SimulatedComponent, SteamBoiler, SteamBoilerConfig,
        battery::BatteryConfig, ev_charger::EvChargerConfig, ev_presets::test_car,
    };

    const SECOND: Duration = Duration::from_secs(1);

    /// Read the state from `old`, restore it onto `fresh` and read it
    /// back.
    fn carried(old: &dyn SimulatedComponent, fresh: &dyn SimulatedComponent) -> PhysicsState {
        let state = old.physics_state().expect("a stateful component has state");
        fresh.restore_physics_state(state.clone());
        let back = fresh.physics_state().expect("still stateful");
        assert_eq!(back, state);
        back
    }

    #[test]
    fn a_battery_carries_its_soc() {
        let old = Battery::new(1, SECOND, BatteryConfig::default());
        old.dc_storage().unwrap().set_soc_pct(37.5);
        let fresh = Battery::new(1, SECOND, BatteryConfig::default());
        assert_eq!(
            carried(&old, &fresh),
            PhysicsState::Battery { soc_pct: 37.5 }
        );
        assert_eq!(fresh.dc_storage().unwrap().soc_pct(), 37.5);
    }

    #[test]
    fn an_ev_charger_carries_its_car_and_its_soc() {
        let old = EvCharger::new(2, SECOND, EvChargerConfig::default());
        let car = test_car("sedan", Some(42.0));
        old.ev_port().unwrap().plug_ev(car.clone()).unwrap();
        let fresh = EvCharger::new(2, SECOND, EvChargerConfig::default());
        assert_eq!(
            carried(&old, &fresh),
            PhysicsState::Ev {
                connected: Some(car.clone())
            }
        );
        let info = fresh
            .ev_port()
            .unwrap()
            .ev_info()
            .expect("a car came along");
        assert_eq!(info.ev, car);
        assert_eq!(info.ev.soc_pct, 42.0);
    }

    #[test]
    fn an_empty_charger_carries_no_car_over_a_plugged_one() {
        let old = EvCharger::new(2, SECOND, EvChargerConfig::default());
        let fresh = EvCharger::new(2, SECOND, EvChargerConfig::default());
        fresh
            .ev_port()
            .unwrap()
            .plug_ev(test_car("van", None))
            .unwrap();
        carried(&old, &fresh);
        assert!(fresh.ev_port().unwrap().ev_info().is_none());
    }

    #[test]
    fn a_steam_boiler_carries_its_pressure() {
        let old = SteamBoiler::new(3, SECOND, SteamBoilerConfig::default());
        old.steam_drive().unwrap().set_pressure_bar(9.25);
        let fresh = SteamBoiler::new(3, SECOND, SteamBoilerConfig::default());
        assert_eq!(
            carried(&old, &fresh),
            PhysicsState::Boiler { pressure_bar: 9.25 }
        );
    }

    /// A state for another type leaves the component as it was.
    #[test]
    fn a_state_of_another_type_is_ignored() {
        let bat = Battery::new(1, SECOND, BatteryConfig::default());
        bat.restore_physics_state(PhysicsState::Boiler { pressure_bar: 9.0 });
        assert_eq!(
            bat.physics_state(),
            Some(PhysicsState::Battery { soc_pct: 50.0 })
        );
        let meter = crate::sim::Meter::new(4, SECOND, None, None, 0.0, false);
        assert_eq!(meter.physics_state(), None);
        meter.restore_physics_state(PhysicsState::Battery { soc_pct: 10.0 });
    }
}
