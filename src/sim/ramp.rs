//! Two orthogonal control-loop primitives shared by inverters, EV
//! chargers, anything else that does not respond instantly to a
//! set-point command.
//!
//! A real inverter takes some time to acknowledge a SCADA command (the
//! `CommandDelay`) and then ramps power toward the target at a slew
//! rate (the `Ramp`) — exceeding the slew rate would damage capacitors,
//! breakers, or the battery itself. Tests for both live next to the
//! implementations.

use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

/// Holds a pending set-point that becomes "armed" only after `delay`
/// has elapsed since `set` was called.
///
/// Models a device that takes `delay` to execute each command: every
/// command arms at its own set-time-plus-delay, and while one command
/// is still executing only the newest later arrival is kept (a
/// one-deep waiting slot). A controller that re-sends faster than
/// `delay` therefore trails by about one delay but always makes
/// progress — a fresh command must never restart the executing
/// command's clock, or a fast re-send cadence would starve the device
/// forever.
#[derive(Debug)]
pub struct CommandDelay {
    state: Mutex<State>,
    delay: Duration,
}

#[derive(Debug, Clone)]
struct State {
    /// The command being executed; arms once its due time passes.
    executing: Option<(DateTime<Utc>, f32)>,
    /// The newest command that arrived while another was executing.
    /// It keeps its own arrival time, so it arms at arrival + delay —
    /// commands pipeline; they are not serialized one per delay.
    waiting: Option<(DateTime<Utc>, f32)>,
    armed: Option<f32>,
}

impl State {
    /// Arm every command whose execution has finished by `now`. Runs
    /// at most twice: the executing command, then a waiting one that
    /// is also already past its own due time.
    fn promote(&mut self, now: DateTime<Utc>, delay: chrono::Duration) {
        while let Some((set_at, v)) = self.executing {
            if now < set_at + delay {
                return;
            }
            self.armed = Some(v);
            self.executing = self.waiting.take();
        }
    }
}

impl CommandDelay {
    pub fn new(delay: Duration) -> Self {
        Self {
            state: Mutex::new(State {
                executing: None,
                waiting: None,
                armed: None,
            }),
            delay,
        }
    }

    pub fn delay(&self) -> Duration {
        self.delay
    }

    pub fn set_target(&self, now: DateTime<Utc>, value: f32) {
        let mut s = self.state.lock();
        if self.delay.is_zero() {
            s.armed = Some(value);
            s.executing = None;
            s.waiting = None;
            return;
        }
        s.promote(now, self.delay_chrono());
        if s.executing.is_none() {
            s.executing = Some((now, value));
        } else {
            s.waiting = Some((now, value));
        }
    }

    /// Promote finished commands, then return the currently armed
    /// value (None until the first command finishes executing).
    pub fn poll(&self, now: DateTime<Utc>) -> Option<f32> {
        let mut s = self.state.lock();
        s.promote(now, self.delay_chrono());
        s.armed
    }

    pub fn reset(&self) {
        let mut s = self.state.lock();
        s.armed = None;
        s.executing = None;
        s.waiting = None;
    }

    fn delay_chrono(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.delay).unwrap_or(chrono::Duration::zero())
    }

    /// Inspect the armed value without advancing the delay clock.
    pub fn armed(&self) -> Option<f32> {
        self.state.lock().armed
    }
}

/// Slew-rate-limited tracker: `actual` moves toward `target` at most
/// `rate_w_per_s` per second.
///
/// Use `rate = f32::INFINITY` to make the tracker pass-through (the
/// behaviour of microsim's inverters today).
#[derive(Debug)]
pub struct Ramp {
    state: Mutex<RampState>,
    rate_w_per_s: f32,
}

#[derive(Debug, Clone)]
struct RampState {
    actual: f32,
    target: f32,
}

impl Ramp {
    pub fn new(rate_w_per_s: f32, initial: f32) -> Self {
        Self {
            state: Mutex::new(RampState {
                actual: initial,
                target: initial,
            }),
            rate_w_per_s,
        }
    }

    pub fn rate(&self) -> f32 {
        self.rate_w_per_s
    }

    pub fn set_target(&self, target: f32) {
        // NaN propagating through the slew math poisons `actual`
        // permanently; reject it at the door. ±∞ is left through —
        // a target of f32::INFINITY combined with a finite rate still
        // gives a well-defined per-tick step.
        if target.is_nan() {
            log::warn!("Ramp::set_target ignored NaN");
            return;
        }
        self.state.lock().target = target;
    }

    pub fn snap_to(&self, value: f32) {
        // Same hazard as `set_target`: a NaN here poisons `actual`
        // permanently, since every later slew step propagates it.
        if value.is_nan() {
            log::warn!("Ramp::snap_to ignored NaN");
            return;
        }
        let mut s = self.state.lock();
        s.target = value;
        s.actual = value;
    }

    pub fn actual(&self) -> f32 {
        self.state.lock().actual
    }

    pub fn target(&self) -> f32 {
        self.state.lock().target
    }

    /// Advance `actual` by the most it is allowed to move in `dt`.
    pub fn advance(&self, dt: Duration) -> f32 {
        let mut s = self.state.lock();
        if !self.rate_w_per_s.is_finite() {
            s.actual = s.target;
            return s.actual;
        }
        let max_step = self.rate_w_per_s * dt.as_secs_f32();
        let diff = s.target - s.actual;
        if diff.abs() <= max_step {
            s.actual = s.target;
        } else {
            s.actual += diff.signum() * max_step;
        }
        s.actual
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_delay_zero_arms_immediately() {
        let cd = CommandDelay::new(Duration::ZERO);
        cd.set_target(Utc::now(), 5000.0);
        assert_eq!(cd.armed(), Some(5000.0));
    }

    #[test]
    fn command_delay_blocks_until_due() {
        let t0 = Utc::now();
        let cd = CommandDelay::new(Duration::from_secs(2));
        cd.set_target(t0, 5000.0);
        assert_eq!(cd.poll(t0 + chrono::Duration::seconds(1)), None);
        assert_eq!(cd.poll(t0 + chrono::Duration::seconds(2)), Some(5000.0));
    }

    /// A controller re-sending faster than the delay must not starve
    /// the device: the executing command keeps its own due time, so
    /// commands keep arming even under a continuous fast stream.
    #[test]
    fn command_delay_survives_fast_resend_cadence() {
        let t0 = Utc::now();
        let cd = CommandDelay::new(Duration::from_millis(1500));
        // One command every 500 ms, values ramping 1000, 2000, …
        for i in 0..10 {
            cd.set_target(
                t0 + chrono::Duration::milliseconds(500 * i),
                (i + 1) as f32 * 1000.0,
            );
        }
        // At t0+4.5s the stream has been running for 9 commands; the
        // device must have armed several of them by now, not none.
        let armed = cd.poll(t0 + chrono::Duration::milliseconds(4500));
        assert!(armed.is_some(), "fast re-sends starved the device");
        // And it keeps progressing: the newest command eventually arms.
        let settled = cd.poll(t0 + chrono::Duration::seconds(60));
        assert_eq!(settled, Some(10_000.0));
    }

    /// While one command executes, only the newest waiting command
    /// survives — intermediate values are superseded, not queued.
    #[test]
    fn command_delay_newest_waiting_command_wins() {
        let t0 = Utc::now();
        let cd = CommandDelay::new(Duration::from_secs(2));
        cd.set_target(t0, 1000.0);
        cd.set_target(t0 + chrono::Duration::milliseconds(200), 2000.0);
        cd.set_target(t0 + chrono::Duration::milliseconds(400), 3000.0);
        // First command arms at its own due time, t0+2s. The waiting
        // slot holds only the newest (3000), which arms at t0+2.4s —
        // its own set time plus the delay. 2000 never arms.
        assert_eq!(cd.poll(t0 + chrono::Duration::seconds(2)), Some(1000.0));
        assert_eq!(
            cd.poll(t0 + chrono::Duration::milliseconds(2300)),
            Some(1000.0)
        );
        assert_eq!(
            cd.poll(t0 + chrono::Duration::milliseconds(2400)),
            Some(3000.0)
        );
    }

    #[test]
    fn ramp_step_limit() {
        let r = Ramp::new(1000.0, 0.0);
        r.set_target(5000.0);
        assert_eq!(r.advance(Duration::from_secs(1)), 1000.0);
        assert_eq!(r.advance(Duration::from_secs(1)), 2000.0);
        // Big jump → step caps it
        assert_eq!(r.advance(Duration::from_secs(2)), 4000.0);
    }

    #[test]
    fn ramp_pass_through_when_infinite() {
        let r = Ramp::new(f32::INFINITY, 0.0);
        r.set_target(5000.0);
        assert_eq!(r.advance(Duration::from_millis(1)), 5000.0);
    }

    #[test]
    fn ramp_ignores_nan_target() {
        let r = Ramp::new(1000.0, 0.0);
        r.set_target(5000.0);
        r.advance(Duration::from_secs(1)); // → 1000
        r.set_target(f32::NAN); // no-op, target stays at 5000
        let v = r.advance(Duration::from_secs(1));
        assert!(
            v.is_finite() && (v - 2000.0).abs() < 1e-3,
            "expected 2000, got {v}"
        );
    }
}
