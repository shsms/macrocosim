//! Connected EVs: the catalog a plugged car is built from, and the
//! draw law — how much a car takes within what its charger offers.

use std::time::Duration;

use chrono::{DateTime, Utc};

/// Nominal phase-to-neutral voltage every current ↔ power conversion
/// in this module uses.
pub const NOMINAL_VOLTAGE_V: f32 = 230.0;
/// IEC 61851: an AC charger cannot signal less than 6 A per phase.
/// A limit that maps below it pauses the session instead.
pub const MIN_CURRENT_A: f32 = 6.0;

/// A catalog entry: the car a `plug-ev` builds when it names one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvPreset {
    pub name: &'static str,
    pub phases: u8,
    /// The car's onboard charger cap, per phase.
    pub max_current_a: f32,
    pub capacity_wh: f32,
    pub initial_soc_pct: f32,
    /// SoC above which the accepted current falls linearly …
    pub taper_start_pct: f32,
    /// … to this fraction of `max_current_a` at 100 %.
    pub taper_floor: f32,
}

pub const PRESETS: [EvPreset; 4] = [
    EvPreset {
        name: "phev",
        phases: 1,
        max_current_a: 16.0,
        capacity_wh: 13_000.0,
        initial_soc_pct: 40.0,
        taper_start_pct: 80.0,
        taper_floor: 0.25,
    },
    EvPreset {
        name: "city",
        phases: 1,
        max_current_a: 32.0,
        capacity_wh: 45_000.0,
        initial_soc_pct: 40.0,
        taper_start_pct: 80.0,
        taper_floor: 0.25,
    },
    EvPreset {
        name: "sedan",
        phases: 3,
        max_current_a: 16.0,
        capacity_wh: 77_000.0,
        initial_soc_pct: 40.0,
        taper_start_pct: 80.0,
        taper_floor: 0.20,
    },
    EvPreset {
        name: "van",
        phases: 3,
        max_current_a: 32.0,
        capacity_wh: 100_000.0,
        initial_soc_pct: 40.0,
        taper_start_pct: 85.0,
        taper_floor: 0.30,
    },
];

pub fn preset(name: &str) -> Option<&'static EvPreset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// Per-plug overrides on top of a preset; `None` keeps the preset's
/// value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvOverrides {
    pub soc_pct: Option<f32>,
    pub target_soc_pct: Option<f32>,
    pub phases: Option<u8>,
    pub max_current_a: Option<f32>,
    pub capacity_wh: Option<f32>,
    pub taper_start_pct: Option<f32>,
    pub taper_floor: Option<f32>,
}

/// The car plugged into a charger. Owned by the charger's state; not
/// a site component.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectedEv {
    pub preset: &'static str,
    pub capacity_wh: f32,
    pub soc_pct: f32,
    pub target_soc_pct: f32,
    pub phases: u8,
    pub max_current_a: f32,
    pub taper_start_pct: f32,
    pub taper_floor: f32,
    pub plugged_at: DateTime<Utc>,
    pub energy_wh: f32,
}

fn pct(label: &str, v: f32) -> Result<f32, String> {
    if v.is_finite() && (0.0..=100.0).contains(&v) {
        Ok(v)
    } else {
        Err(format!("{label} must be within 0..=100, got {v}"))
    }
}

fn positive(label: &str, v: f32) -> Result<f32, String> {
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(format!("{label} must be a positive number, got {v}"))
    }
}

impl ConnectedEv {
    pub fn new(p: &EvPreset, o: &EvOverrides, now: DateTime<Utc>) -> Result<Self, String> {
        let phases = o.phases.unwrap_or(p.phases);
        if !(1..=3).contains(&phases) {
            return Err(format!("phases must be 1, 2 or 3, got {phases}"));
        }
        let taper_floor = o.taper_floor.unwrap_or(p.taper_floor);
        if !taper_floor.is_finite() || !(0.0..=1.0).contains(&taper_floor) {
            return Err(format!(
                "taper-floor must be within 0..=1, got {taper_floor}"
            ));
        }
        Ok(Self {
            preset: p.name,
            capacity_wh: positive("capacity", o.capacity_wh.unwrap_or(p.capacity_wh))?,
            soc_pct: pct("soc", o.soc_pct.unwrap_or(p.initial_soc_pct))?,
            target_soc_pct: pct("target-soc", o.target_soc_pct.unwrap_or(100.0))?,
            phases,
            max_current_a: positive("max-current-a", o.max_current_a.unwrap_or(p.max_current_a))?,
            taper_start_pct: pct(
                "taper-start",
                o.taper_start_pct.unwrap_or(p.taper_start_pct),
            )?,
            taper_floor,
            plugged_at: now,
            energy_wh: 0.0,
        })
    }

    /// The current the car's BMS accepts at its present SoC: full up
    /// to the taper start, then a straight line down to the floor at
    /// 100 %.
    pub fn accepted_current_a(&self) -> f32 {
        if self.soc_pct <= self.taper_start_pct {
            return self.max_current_a;
        }
        let span = (100.0 - self.taper_start_pct).max(f32::EPSILON);
        let frac = ((self.soc_pct - self.taper_start_pct) / span).clamp(0.0, 1.0);
        let scale = 1.0 - frac * (1.0 - self.taper_floor);
        self.max_current_a * scale
    }

    pub fn done(&self) -> bool {
        self.soc_pct >= self.target_soc_pct
    }

    /// Power the car draws when the charger offers `offered_a` per
    /// phase on `charger_phases` phases: the lesser of the offer and
    /// what the car's BMS accepts at this SoC (itself capped by
    /// `max_current_a`), on the phases both sides have.
    pub fn draw_w(&self, offered_a: f32, charger_phases: u8) -> f32 {
        if self.done() {
            return 0.0;
        }
        let amps = offered_a.min(self.accepted_current_a());
        let phases = self.phases.min(charger_phases).max(1) as f32;
        amps.max(0.0) * NOMINAL_VOLTAGE_V * phases
    }

    /// One rectangular step: energy in, SoC up (clamped at 100). The
    /// SoC step is the shared `decay::integrate_soc_pct` every other
    /// pack in the simulator uses, so a car and a battery move their
    /// SoC by exactly the same arithmetic.
    pub fn integrate(&mut self, power_w: f32, dt: Duration) {
        self.energy_wh += power_w * dt.as_secs_f32() / 3600.0;
        self.soc_pct =
            crate::sim::decay::integrate_soc_pct(self.soc_pct, power_w, dt, self.capacity_wh);
    }
}

/// The per-phase current a charger with `charger_phases` phases
/// signals for a power limit of `limit_w`, or `None` when that would
/// be under the 6 A floor (the charger pauses).
pub fn offered_current_a(limit_w: f32, charger_phases: u8) -> Option<f32> {
    let amps = limit_w / (NOMINAL_VOLTAGE_V * charger_phases.max(1) as f32);
    (amps.is_finite() && amps >= MIN_CURRENT_A).then_some(amps)
}

/// What the charger is doing with its car this tick; `ev-info` and
/// the inspector read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvDrawState {
    Charging,
    Paused,
    Done,
    Tripped,
}

impl EvDrawState {
    pub fn as_str(self) -> &'static str {
        match self {
            EvDrawState::Charging => "charging",
            EvDrawState::Paused => "paused",
            EvDrawState::Done => "done",
            EvDrawState::Tripped => "tripped",
        }
    }
}

/// The simulator's private view of a plugged car.
#[derive(Clone, Debug)]
pub struct EvInfo {
    pub ev: ConnectedEv,
    pub state: EvDrawState,
}

/// A car off the catalog, optionally at a given SoC: the plug every
/// test that needs a charger with something in it starts from.
#[cfg(test)]
pub fn test_car(name: &str, soc: Option<f32>) -> ConnectedEv {
    ConnectedEv::new(
        preset(name).unwrap_or_else(|| panic!("no preset named {name}")),
        &EvOverrides {
            soc_pct: soc,
            ..Default::default()
        },
        Utc::now(),
    )
    .expect("test car")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::time::Duration;

    #[test]
    fn catalog_has_the_four_presets() {
        for (name, phases, amps) in [
            ("phev", 1, 16.0),
            ("city", 1, 32.0),
            ("sedan", 3, 16.0),
            ("van", 3, 32.0),
        ] {
            let p = preset(name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!((p.phases, p.max_current_a), (phases, amps), "{name}");
        }
        assert!(preset("unicorn").is_none());
    }

    #[test]
    fn overrides_apply_and_are_validated() {
        let p = preset("sedan").unwrap();
        let ev = ConnectedEv::new(
            p,
            &EvOverrides {
                soc_pct: Some(30.0),
                phases: Some(2),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!((ev.soc_pct, ev.phases, ev.preset), (30.0, 2, "sedan"));
        for bad in [
            EvOverrides {
                phases: Some(0),
                ..Default::default()
            },
            EvOverrides {
                phases: Some(4),
                ..Default::default()
            },
            EvOverrides {
                soc_pct: Some(101.0),
                ..Default::default()
            },
            EvOverrides {
                target_soc_pct: Some(-1.0),
                ..Default::default()
            },
            EvOverrides {
                max_current_a: Some(0.0),
                ..Default::default()
            },
            EvOverrides {
                capacity_wh: Some(f32::NAN),
                ..Default::default()
            },
        ] {
            assert!(ConnectedEv::new(p, &bad, Utc::now()).is_err(), "{bad:?}");
        }
    }

    /// 22 kW on three phases is 32 A; 7.4 kW on one phase is 32 A;
    /// below 6 A the charger offers nothing.
    #[test]
    fn offered_current_floors_at_six_amps() {
        assert!((offered_current_a(22_080.0, 3).unwrap() - 32.0).abs() < 0.01);
        assert!((offered_current_a(7_360.0, 1).unwrap() - 32.0).abs() < 0.01);
        assert!((offered_current_a(4_140.0, 3).unwrap() - 6.0).abs() < 0.01);
        assert_eq!(offered_current_a(4_100.0, 3), None);
        assert_eq!(offered_current_a(0.0, 3), None);
    }

    /// The car takes the least of what is offered, its own cap, and
    /// its taper, on its own phase count.
    #[test]
    fn draw_is_the_minimum_of_offer_cap_and_taper_on_the_cars_phases() {
        let sedan = test_car("sedan", None); // 3 ph, 16 A
        assert!(
            (sedan.draw_w(32.0, 3) - 3.0 * 230.0 * 16.0).abs() < 0.5,
            "capped by the car"
        );
        assert!(
            (sedan.draw_w(10.0, 3) - 3.0 * 230.0 * 10.0).abs() < 0.5,
            "capped by the offer"
        );
        let city = test_car("city", None);
        assert!(
            (city.draw_w(32.0, 3) - 230.0 * 32.0).abs() < 0.5,
            "one phase only"
        );
        let two = ConnectedEv::new(
            preset("sedan").unwrap(),
            &EvOverrides {
                phases: Some(2),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert!(
            (two.draw_w(32.0, 3) - 2.0 * 230.0 * 16.0).abs() < 0.5,
            "two phases"
        );
        assert!(
            (sedan.draw_w(32.0, 1) - 230.0 * 16.0).abs() < 0.5,
            "a 3-phase car on a 1-phase charger"
        );
    }

    #[test]
    fn taper_falls_linearly_to_the_floor() {
        let mut ev = test_car("sedan", None); // taper 80 % → floor 0.20 at 100 %
        ev.soc_pct = 50.0;
        assert_eq!(ev.accepted_current_a(), 16.0);
        ev.soc_pct = 80.0;
        assert_eq!(ev.accepted_current_a(), 16.0);
        ev.soc_pct = 90.0;
        assert!(
            (ev.accepted_current_a() - 16.0 * 0.6).abs() < 0.01,
            "halfway down"
        );
        ev.soc_pct = 100.0;
        assert!(
            (ev.accepted_current_a() - 16.0 * 0.2).abs() < 0.01,
            "the floor"
        );
    }

    #[test]
    fn done_at_target_and_integration_moves_soc_and_energy() {
        let mut ev = ConnectedEv::new(
            preset("sedan").unwrap(),
            &EvOverrides {
                soc_pct: Some(50.0),
                target_soc_pct: Some(50.5),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert!(!ev.done());
        // 11 040 W for 60 s = 184 Wh = 0.24 % of 77 kWh.
        ev.integrate(11_040.0, Duration::from_secs(60));
        assert!((ev.energy_wh - 184.0).abs() < 0.1);
        assert!(ev.soc_pct > 50.2 && ev.soc_pct < 50.3, "got {}", ev.soc_pct);
        ev.integrate(11_040.0, Duration::from_secs(120));
        assert!(ev.done(), "past target at {}", ev.soc_pct);
        assert_eq!(ev.draw_w(32.0, 3), 0.0, "a done car takes nothing");
    }
}
