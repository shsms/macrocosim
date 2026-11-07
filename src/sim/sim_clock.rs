//! The source of "now" for a site's clock.
//!
//! A live engine reads the wall clock. The physics tests instead read
//! a fixed base time plus the elapsed time of a [`ManualClock`] they
//! advance by hand, so request lifetimes, recordings and physics move
//! together and the tests run faster than real time.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

/// A clock that moves only when [`Self::advance`] is called.
#[derive(Debug, Default)]
pub struct ManualClock {
    elapsed: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        *self.elapsed.lock() += by;
    }

    /// The time advanced since the clock was made.
    pub fn elapsed(&self) -> Duration {
        *self.elapsed.lock()
    }
}

/// A clonable handle yielding the current simulation time as a
/// `DateTime<Utc>`. Cheap to clone (one `Arc`).
#[derive(Clone)]
pub struct NowSource(Arc<Inner>);

enum Inner {
    /// `now()` is `Utc::now()`.
    Wall,
    /// `now()` is `base + clock.elapsed()`.
    Sim {
        base: DateTime<Utc>,
        clock: Arc<ManualClock>,
    },
}

impl NowSource {
    /// Wall-clock source — the live-server default.
    pub fn wall() -> Self {
        Self(Arc::new(Inner::Wall))
    }

    /// Simulated source tied to `clock`: `now()` advances exactly as the
    /// host advances `clock`, off a fixed `base`.
    pub fn sim(base: DateTime<Utc>, clock: Arc<ManualClock>) -> Self {
        Self(Arc::new(Inner::Sim { base, clock }))
    }

    /// Whether `now()` reads the wall clock, as a live engine's does.
    pub fn is_wall(&self) -> bool {
        matches!(*self.0, Inner::Wall)
    }

    pub fn now(&self) -> DateTime<Utc> {
        match &*self.0 {
            Inner::Wall => Utc::now(),
            Inner::Sim { base, clock } => {
                *base + chrono::Duration::from_std(clock.elapsed()).unwrap_or_default()
            }
        }
    }
}

impl Default for NowSource {
    fn default() -> Self {
        Self::wall()
    }
}

/// A fixed base instant for a clock the tests advance by hand, so
/// absolute timestamps are reproducible across runs.
pub fn headless_base() -> DateTime<Utc> {
    DateTime::from_timestamp(1_577_836_800, 0).expect("2020-01-01T00:00:00Z is valid")
}

/// Parse an absolute wall time `"HH:MM"` (24-hour) into the offset
/// from midnight: the weather's sunrise and sunset. `None` on a
/// malformed value or out-of-range hour/minute.
pub fn parse_time_of_day(s: &str) -> Option<Duration> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u64 = h.trim().parse().ok()?;
    let m: u64 = m.trim().parse().ok()?;
    if h >= 24 || m >= 60 {
        return None;
    }
    Some(Duration::from_secs(h * 3600 + m * 60))
}

#[cfg(test)]
mod tests {
    use super::{ManualClock, NowSource, headless_base, parse_time_of_day};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn a_sim_source_moves_with_its_manual_clock() {
        let clock = Arc::new(ManualClock::new());
        let now = NowSource::sim(headless_base(), clock.clone());
        assert!(!now.is_wall());
        assert_eq!(now.now(), headless_base());
        clock.advance(Duration::from_millis(1500));
        clock.advance(Duration::from_secs(2));
        assert_eq!(clock.elapsed(), Duration::from_millis(3500));
        assert_eq!(
            now.now(),
            headless_base() + chrono::Duration::milliseconds(3500)
        );
    }

    #[test]
    fn parse_time_of_day_hhmm() {
        assert_eq!(parse_time_of_day("00:00"), Some(Duration::ZERO));
        assert_eq!(parse_time_of_day("14:30"), Some(Duration::from_secs(52200)));
        assert_eq!(parse_time_of_day("24:00"), None);
        assert_eq!(parse_time_of_day("12:60"), None);
        assert_eq!(parse_time_of_day("noon"), None);
    }
}
