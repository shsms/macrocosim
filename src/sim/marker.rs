//! Marker components: CHP, wind turbine, power transformer, breaker.
//! They carry no physics of their own — they exist so the topology is
//! complete and so the formula engine can classify the meters around
//! them (a meter feeding a wind turbine is a wind meter). Power is
//! set directly on the neighboring meter (its `power_w`). Whatever
//! flows through a marker is its children's, so a meter above a
//! transformer or breaker still sums the branch below it.

use std::{fmt, time::Duration};

use chrono::{DateTime, Utc};

use crate::sim::{Category, MicrogridSite, SimulatedComponent, Telemetry};

pub struct Marker {
    id: u64,
    name: String,
    category: Category,
    stream_jitter_pct: f32,
}

impl Marker {
    pub fn new(id: u64, category: Category, stream_jitter_pct: f32) -> Self {
        let prefix = match category {
            Category::Chp => "chp",
            Category::WindTurbine => "wind",
            Category::PowerTransformer => "transformer",
            Category::Breaker => "breaker",
            // The constructor is only reached from the spec build's
            // marker arm; anything else is a programming error.
            other => unreachable!("Marker::new with non-marker category {other:?}"),
        };
        Self {
            id,
            name: format!("{prefix}-{id}"),
            category,
            stream_jitter_pct,
        }
    }
}

impl fmt::Display for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl SimulatedComponent for Marker {
    fn id(&self) -> u64 {
        self.id
    }
    fn category(&self) -> Category {
        self.category
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn stream_interval(&self) -> Duration {
        Duration::from_secs(1)
    }
    fn stream_jitter_pct(&self) -> f32 {
        self.stream_jitter_pct
    }
    fn tick(&self, _w: &MicrogridSite, _n: DateTime<Utc>, _d: Duration) {}
    fn telemetry(&self, _w: &MicrogridSite) -> Telemetry {
        Telemetry {
            id: self.id,
            category: Some(self.category),
            ..Default::default()
        }
    }

    fn aggregate_power_w(&self, site: &MicrogridSite) -> f32 {
        site.sum_children(self.id, |c| c.aggregate_power_w(site))
    }

    fn aggregate_reactive_var(&self, site: &MicrogridSite) -> f32 {
        site.sum_children(self.id, |c| c.aggregate_reactive_var(site))
    }
}
