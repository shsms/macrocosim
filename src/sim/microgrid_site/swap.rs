//! Rebuild with carry: replace a site's components with a freshly
//! built set while its physics task, history sampler and servers keep
//! running. What a matching component keeps, and what every component
//! takes from its spec, is listed on
//! [`MicrogridSite::swap_topology`].

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, atomic::Ordering};

use super::MicrogridSite;
use crate::sim::PhysicsState;
use crate::spec::BuiltTopology;

/// The components a rebuild carries: each id whose new spec equals
/// its old one, with the physics state read from the old component
/// (`None` for a component that has none).
pub type CarryPlan = BTreeMap<u64, Option<PhysicsState>>;

impl MicrogridSite {
    /// Install `built` in place of this site's components, carrying the
    /// state of those in `carry`.
    ///
    /// Each matching component in `carry` gets its old physics state
    /// back, and keeps its history, setpoint log and energy total;
    /// every other id starts fresh, and a removed id's are dropped.
    /// Every component then takes its health, modes and name from its
    /// spec, and the site its physics tick. Commands, request
    /// lifetimes and augmentations are cleared, telemetry streams end,
    /// and a running scenario journal is stopped. Weather, grid state
    /// and sample lag stay as they are.
    ///
    /// The component list, the id index and the connections change
    /// under one set of write guards, so a physics tick sees the
    /// whole old set or the whole new one. The run generation moves,
    /// so the gateway refuses a command looked up before the swap.
    pub fn swap_topology(&self, built: BuiltTopology, carry: &CarryPlan) {
        let BuiltTopology {
            components,
            connections,
            initial,
            physics_tick_ms,
            ..
        } = built;
        for c in &components {
            if let Some(Some(state)) = carry.get(&c.id()) {
                c.restore_physics_state(state.clone());
            }
        }
        // The runtime knobs, operational modes and names the specs
        // set, ready to install with the components.
        let mut runtime = HashMap::new();
        let mut modes = HashMap::new();
        let mut names = HashMap::new();
        for c in &components {
            let id = c.id();
            let init = initial.get(&id).cloned().unwrap_or_default();
            runtime.insert(id, init.runtime);
            if let Some(mode) = init.operational_mode {
                modes.insert(id, mode);
            }
            if let Some(name) = init.name {
                names.insert(id, name);
            }
        }
        let by_id: HashMap<u64, _> = components.iter().map(|c| (c.id(), c.clone())).collect();
        let keep = |id: &u64| carry.contains_key(id);

        self.inner.run_generation.fetch_add(1, Ordering::Relaxed);
        // Each stream task holds its component from subscribe time;
        // ending them makes a client that subscribes again reach the
        // new set.
        self.cancel_all_streams();
        if self.scenario_is_running() {
            self.scenario_stop(self.now(), None);
        }
        self.inner.scenario_knob_baseline.write().clear();
        // Before the components are visible: a command that finds a
        // new component finds its axes too.
        self.inner.gateway.reinstall(&components);
        {
            let mut list = self.inner.components.write();
            let mut index = self.inner.by_id.write();
            let mut edges = self.inner.connections.write();
            let mut rt = self.inner.runtime.write();
            let mut op = self.inner.operational_modes.write();
            *list = Arc::new(components);
            *index = by_id;
            *edges = connections;
            *rt = runtime;
            *op = modes;
            *self.inner.name_overrides.write() = names;
        }
        self.inner.histories.write().retain(|id, _| keep(id));
        self.inner.component_energy.write().retain(|id, _| keep(id));
        self.inner.setpoint_logs.write().retain(|id, _| keep(id));
        self.inner
            .scenario
            .write()
            .energy_baseline_wh
            .retain(|id, _| keep(id));
        self.set_physics_tick_ms(physics_tick_ms);
        self.bump_structural();
        self.bump_version();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::sim::runtime::Health;
    use crate::spec::{Defaults, TopologyDoc, build_topology};

    fn built(json: &str) -> BuiltTopology {
        let doc = format!(r#"{{"schema_version": 1, "microgrids": [{json}]}}"#);
        let spec = TopologyDoc::from_json(&doc).unwrap().microgrids.remove(0);
        build_topology(&spec, &Defaults::default()).unwrap()
    }

    const TWO_BATTERIES: &str = r#"{"components": [
        {"type": "grid_connection_point", "id": 1},
        {"type": "battery_inverter", "id": 2},
        {"type": "battery", "id": 3, "initial_soc_pct": 50},
        {"type": "battery_inverter", "id": 4},
        {"type": "battery", "id": 5, "initial_soc_pct": 50}
    ], "connections": [[1, 2], [2, 3], [1, 4], [4, 5]]}"#;

    fn soc(site: &MicrogridSite, id: u64) -> f32 {
        site.get(id).unwrap().dc_storage().unwrap().soc_pct()
    }

    #[test]
    fn a_matching_component_keeps_its_state_and_history() {
        let site = MicrogridSite::new();
        site.swap_topology(built(TWO_BATTERIES), &CarryPlan::default());
        for id in [3, 5] {
            site.set_soc_pct(site.get(id).unwrap().as_ref(), 80.0)
                .unwrap();
        }
        site.tick_n(3, Duration::from_millis(100));
        site.record_history_snapshot(site.now());
        assert!(!site.history_metrics(2).is_empty());
        assert!(!site.history_metrics(4).is_empty());
        use crate::sim::setpoints::{SetpointEvent, SetpointKind, SetpointOutcome};
        let at = chrono::Utc::now();
        for id in [2, 4] {
            site.log_setpoint(
                id,
                SetpointEvent {
                    ts: at,
                    kind: SetpointKind::ActivePower,
                    value: 1000.0,
                    ttl_s: Some(60),
                    outcome: SetpointOutcome::Accepted {
                        effective_value: Some(1000.0),
                    },
                },
            );
        }
        let since = at - chrono::Duration::seconds(1);
        let generation = site.run_generation();

        let mut carry = CarryPlan::default();
        for id in [1, 2, 3] {
            carry.insert(id, site.get(id).unwrap().physics_state());
        }
        site.swap_topology(built(TWO_BATTERIES), &carry);

        assert_eq!(soc(&site, 3), 80.0, "carried");
        assert_eq!(soc(&site, 5), 50.0, "from the spec");
        assert!(!site.history_metrics(2).is_empty(), "kept");
        assert!(site.history_metrics(4).is_empty(), "dropped");
        assert_eq!(site.setpoints_window(2, since).len(), 1, "setpoints kept");
        assert!(
            site.setpoints_window(4, since).is_empty(),
            "setpoints dropped"
        );
        assert!(site.run_generation() > generation);
        assert_eq!(site.component_count(), 5);
        assert_eq!(site.connections().len(), 4);
    }

    #[test]
    fn modes_names_and_the_tick_come_from_the_spec() {
        let site = MicrogridSite::new();
        site.swap_topology(built(TWO_BATTERIES), &CarryPlan::default());
        site.set_health(3, Health::Error).unwrap();
        site.rename(2, "old".into());
        site.swap_topology(
            built(
                r#"{"physics_tick_s": 0.2, "components": [
                {"type": "battery", "id": 3, "health": "standby", "name": "pack"}
            ]}"#,
            ),
            &CarryPlan::default(),
        );
        assert_eq!(site.runtime_of(3).health, Health::Standby);
        assert_eq!(site.display_name(3).as_deref(), Some("pack"));
        assert_eq!(site.name_override(2), None);
        assert!(site.get(2).is_none());
        assert_eq!(site.physics_tick(), Duration::from_millis(200));
    }

    #[test]
    fn weather_and_grid_state_stay() {
        let site = MicrogridSite::new();
        site.set_weather(Some(crate::sim::weather::Weather::new(
            crate::sim::weather::WeatherConfig::default(),
        )));
        let mut grid = site.grid_state();
        grid.voltage_per_phase = (231.0, 232.0, 233.0);
        site.set_grid_state(grid.clone());
        site.set_sample_lag_ms(250);
        site.swap_topology(built(TWO_BATTERIES), &CarryPlan::default());
        assert!(site.with_weather(|_| ()).is_some());
        assert_eq!(site.grid_state().voltage_per_phase, grid.voltage_per_phase);
        assert_eq!(site.sample_lag_ms(), 250);
    }

    #[test]
    fn only_a_reset_moves_the_energy_epoch() {
        let site = MicrogridSite::new();
        let epoch = site.energy_epoch();
        site.swap_topology(built(TWO_BATTERIES), &CarryPlan::default());
        assert_eq!(site.energy_epoch(), epoch);
        site.reset();
        assert!(site.energy_epoch() > epoch);
    }
}
