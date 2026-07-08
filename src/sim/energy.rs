//! Shared trapezoidal power→energy integrator.
//!
//! One running total plus the cursor (last power sample) needed to
//! advance the trapezoid across the next interval. Both the per-component
//! `EnergyWh` history metric (see `history.rs`) and the aggregate
//! `*_energy` loopback streams (see `ui/loopback.rs`) integrate signed
//! active power (W) into cumulative energy (Wh) this way, so the quadrature
//! lives here once rather than being copied at each site.
//!
//! Sign travels through untouched: import/consumption-positive power yields
//! a positive running total, export/production-negative a negative one, so
//! the total is *net* energy across the bus.
//!
//! (The battery SoC integral in `battery.rs` is a *related* P·dt integral
//! but a different quadrature — rectangular over one fixed physics tick,
//! not trapezoidal over a variable sample gap — so it does not share this
//! accumulator.)

/// Running trapezoidal integral of a signed power stream (W) into
/// cumulative energy (Wh). Advance once per sample; the first sample only
/// seeds the cursor.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnergyAccum {
    /// Cumulative net energy in Wh.
    pub total_wh: f64,
    /// Last power sample (W), or `None` before the first one / after a
    /// cursor reset.
    last_w: Option<f32>,
    /// Timestamp (epoch ms) of `last_w`; meaningless while `last_w` is
    /// `None`.
    last_ts_ms: i64,
}

impl EnergyAccum {
    /// Integrate the trapezoid between the previous sample and this one
    /// into the running total, then record this sample as the new cursor.
    /// The first sample after construction or a [`reset_cursor`] only seeds
    /// the cursor — no interval to integrate yet.
    ///
    /// [`reset_cursor`]: EnergyAccum::reset_cursor
    pub fn advance(&mut self, power_w: f32, ts_ms: i64) {
        if let Some(last) = self.last_w {
            // Ignore a non-monotonic timestamp (a backward wall-clock step
            // from NTP / suspend-resume, or a duplicate): integrating a
            // negative interval would corrupt the running total, and
            // rewinding the cursor would over-count the next real interval.
            // Mirrors the physics loop clamping its own `dt` to zero on a
            // backward step. Wait for time to advance past the last sample.
            if ts_ms <= self.last_ts_ms {
                return;
            }
            let dt_h = (ts_ms - self.last_ts_ms) as f64 / 3_600_000.0;
            self.total_wh += (last as f64 + power_w as f64) / 2.0 * dt_h;
        }
        self.last_w = Some(power_w);
        self.last_ts_ms = ts_ms;
    }

    /// Drop the power cursor but keep the running total, so the next
    /// [`advance`] re-seeds instead of integrating a trapezoid across the
    /// gap. Used on a loopback rebuild: the forwarders are down for an
    /// unknown span, so bridging that gap with the pre-rebuild power would
    /// count energy that never actually flowed.
    ///
    /// [`advance`]: EnergyAccum::advance
    pub fn reset_cursor(&mut self) {
        self.last_w = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_only_seeds() {
        let mut e = EnergyAccum::default();
        e.advance(3600.0, 0);
        assert_eq!(e.total_wh, 0.0);
    }

    #[test]
    fn constant_power_integrates_rectangle() {
        let mut e = EnergyAccum::default();
        e.advance(3600.0, 0);
        e.advance(3600.0, 3_600_000); // +1 h at 3600 W → 3600 Wh
        assert!((e.total_wh - 3600.0).abs() < 1e-6, "{}", e.total_wh);
    }

    #[test]
    fn ramp_down_integrates_trapezoid() {
        let mut e = EnergyAccum::default();
        e.advance(3600.0, 0);
        e.advance(3600.0, 3_600_000);
        e.advance(0.0, 5_400_000); // +0.5 h ramp 3600→0 → (3600+0)/2·0.5 = 900 Wh
        assert!((e.total_wh - 4500.0).abs() < 1e-6, "{}", e.total_wh);
    }

    #[test]
    fn negative_power_yields_negative_energy() {
        let mut e = EnergyAccum::default();
        e.advance(-2000.0, 0);
        e.advance(-2000.0, 1_800_000); // -2000 W for 0.5 h → -1000 Wh
        assert!((e.total_wh + 1000.0).abs() < 1e-6, "{}", e.total_wh);
    }

    #[test]
    fn non_monotonic_timestamp_is_ignored() {
        let mut e = EnergyAccum::default();
        e.advance(2000.0, 3_600_000); // seed at t=1 h
        e.advance(2000.0, 7_200_000); // +1 h at 2000 W → 2000 Wh
        let total = e.total_wh;
        // Clock steps back: must neither subtract energy nor rewind the
        // cursor, so a later forward sample integrates only the real gap.
        e.advance(2000.0, 5_400_000);
        assert!((e.total_wh - total).abs() < 1e-6, "{}", e.total_wh);
        // Time advances 1 h past the last *good* sample (t=2 h → 3 h).
        e.advance(2000.0, 10_800_000);
        assert!(
            (e.total_wh - (total + 2000.0)).abs() < 1e-6,
            "{}",
            e.total_wh
        );
    }

    #[test]
    fn reset_cursor_keeps_total_but_skips_the_gap() {
        let mut e = EnergyAccum::default();
        e.advance(1000.0, 0);
        e.advance(1000.0, 3_600_000); // +1000 Wh
        let total = e.total_wh;
        // A rebuild-style gap: the forwarders were down for an hour.
        e.reset_cursor();
        // The first post-reset sample re-seeds, so the dead hour adds
        // nothing even though its timestamp is an hour later.
        e.advance(1000.0, 7_200_000);
        assert!((e.total_wh - total).abs() < 1e-6, "{}", e.total_wh);
        // Integration resumes cleanly from the new cursor.
        e.advance(1000.0, 10_800_000); // +1 h at 1000 W
        assert!(
            (e.total_wh - (total + 1000.0)).abs() < 1e-6,
            "{}",
            e.total_wh
        );
    }
}
