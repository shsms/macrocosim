//! Tracks the deadline by which the most recent set-power request for
//! each (component, power axis) pair expires. Mirrors microsim's
//! TimeoutTracker, extended per-axis: active and reactive setpoints
//! carry independent request lifetimes, so a short-lived Q command
//! must not clear a long-lived P command when it expires (and vice
//! versa).
//!
//! Implementation is a `HashMap<(u64, SetpointAxis), Instant>` swept
//! by `reset_expired_with` once per timeout-loop tick (100 ms cadence).
//! Sweep is O(N) over active entries; with typical scales (tens of
//! components, occasional setpoint churn) that's a non-issue. For
//! large microgrids with thousands of active timeouts the natural
//! upgrade is a `BinaryHeap<(Instant, …)>` so the sweep pops only
//! the earliest-due entry. Defer that until the scan shows up in a
//! profile.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

/// Which setpoint a request lifetime governs. Active and reactive
/// commands time out independently; expiry resets only its own axis
/// (`SimulatedComponent::reset_setpoint_axis`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SetpointAxis {
    Active,
    Reactive,
}

impl SetpointAxis {
    /// The axis's unit label, as carried into setpoint error
    /// messages (`MicrogridSite::gate_setpoint`).
    pub fn unit(self) -> &'static str {
        match self {
            SetpointAxis::Active => "W",
            SetpointAxis::Reactive => "VAr",
        }
    }
}

#[derive(Clone, Default)]
pub struct TimeoutTracker {
    inner: Arc<Mutex<HashMap<(u64, SetpointAxis), Instant>>>,
}

impl TimeoutTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Time left before the (id, axis) setpoint expires. `None` when
    /// the pair isn't tracked or the deadline already passed (the
    /// sweep will remove it shortly).
    pub fn remaining(&self, id: u64, axis: SetpointAxis) -> Option<Duration> {
        self.inner
            .lock()
            .get(&(id, axis))
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
    }

    /// Actuate and arm a deadline as one atomic step under the
    /// deadline-map lock: `f` (the setpoint actuation) runs while the
    /// lock is held, and the deadline is inserted only if `f` succeeds,
    /// before the lock is released. This closes the race where a
    /// renewal lands between "apply the setpoint" and "arm its
    /// deadline" (previously ~12 lines apart with no lock spanning
    /// them) and got wiped by a reset armed for the stale deadline. A
    /// failed actuation arms nothing — there's no remove-single API,
    /// so arming ahead of a failed actuation would let a later sweep
    /// reset an unrelated, still-valid previous command on this axis.
    ///
    /// Lock order (see module docs / `MicrogridSite::actuate_and_arm`):
    /// tracker → components-map → axis mutexes. `f` is expected to be
    /// a bare component actuation (already-resolved `Arc<dyn
    /// SimulatedComponent>`) that only touches its own axis mutexes,
    /// never the components map or another instance of this tracker —
    /// so holding the tracker lock across it can't invert the order.
    pub fn actuate_and_arm<E>(
        &self,
        id: u64,
        axis: SetpointAxis,
        lifetime: Duration,
        f: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        let mut guard = self.inner.lock();
        f()?;
        guard.insert((id, axis), Instant::now() + lifetime);
        Ok(())
    }

    /// Drain and reset expired deadlines as one atomic step: the
    /// lock is taken once, expired keys are collected and removed,
    /// and `f` is invoked per expired key, all before the lock is
    /// released. This closes the race where the sweep drains expired
    /// keys, a renewal lands, and only then does the (now stale)
    /// reset run and wipe it — the previous two-step
    /// `remove_expired` + per-id reset had no lock spanning the gap.
    ///
    /// Lock order: tracker → components-map → axis mutexes. Callers
    /// pass an `f` that looks up the component (components-map lock)
    /// and resets its axis (axis mutex) — both acquired *after* this
    /// tracker's lock, matching the order, never independently before
    /// it. Nothing else in this codebase acquires the tracker lock
    /// while already holding the components map or an axis mutex
    /// (verified by grep over all `timeout_tracker` call sites), so
    /// this can't deadlock against another path taking those locks
    /// first.
    pub fn reset_expired_with(&self, mut f: impl FnMut(u64, SetpointAxis)) {
        let now = Instant::now();
        let mut guard = self.inner.lock();
        guard.retain(|&(id, axis), deadline| {
            if *deadline <= now {
                f(id, axis);
                false
            } else {
                true
            }
        });
    }

    /// Drop every armed deadline. Called from `MicrogridSite::reset()`
    /// alongside the registry and journal clears: a deadline armed
    /// before a hot reload otherwise outlives the run that armed it and
    /// fires against whatever component the rebuilt config registers
    /// under the same id, resetting a command the new run accepted.
    /// Nothing is actuated — the components those deadlines named are
    /// gone.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }

    /// Drop both of one component's deadlines (active AND reactive).
    /// Called when `id` leaves the registry, and again when an id is
    /// (re-)registered — a removal that raced an in-flight setpoint can
    /// leave an entry behind, and a re-created component must not
    /// inherit the previous occupant's expiry.
    pub fn remove_component(&self, id: u64) {
        let mut guard = self.inner.lock();
        guard.remove(&(id, SetpointAxis::Active));
        guard.remove(&(id, SetpointAxis::Reactive));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two axes hold independent deadlines for the same
    /// component: an elapsed reactive lifetime drains alone, leaving
    /// the active deadline armed.
    #[test]
    fn axes_expire_independently() {
        let t = TimeoutTracker::new();
        t.actuate_and_arm(7, SetpointAxis::Active, Duration::from_secs(3600), || {
            Ok::<(), ()>(())
        })
        .unwrap();
        t.actuate_and_arm(7, SetpointAxis::Reactive, Duration::ZERO, || {
            Ok::<(), ()>(())
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let mut reset = Vec::new();
        t.reset_expired_with(|id, axis| reset.push((id, axis)));
        assert_eq!(reset, vec![(7, SetpointAxis::Reactive)]);
        // The active deadline is untouched and still pending.
        let mut reset = Vec::new();
        t.reset_expired_with(|id, axis| reset.push((id, axis)));
        assert_eq!(reset, Vec::new());
    }

    /// Latest-set-wins is per axis: re-arming the active deadline
    /// doesn't disturb the reactive one.
    #[test]
    fn rearming_one_axis_keeps_the_other() {
        let t = TimeoutTracker::new();
        t.actuate_and_arm(7, SetpointAxis::Reactive, Duration::ZERO, || {
            Ok::<(), ()>(())
        })
        .unwrap();
        t.actuate_and_arm(7, SetpointAxis::Active, Duration::ZERO, || Ok::<(), ()>(()))
            .unwrap();
        t.actuate_and_arm(7, SetpointAxis::Active, Duration::from_secs(3600), || {
            Ok::<(), ()>(())
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let mut reset = Vec::new();
        t.reset_expired_with(|id, axis| reset.push((id, axis)));
        assert_eq!(reset, vec![(7, SetpointAxis::Reactive)]);
    }

    /// `remaining` reports the time left for a tracked, still-live
    /// deadline, and None for an untracked axis or id.
    #[test]
    fn remaining_reports_time_left_and_none_when_absent() {
        let t = TimeoutTracker::new();
        t.actuate_and_arm(1, SetpointAxis::Active, Duration::from_secs(60), || {
            Ok::<(), ()>(())
        })
        .unwrap();
        let left = t.remaining(1, SetpointAxis::Active).expect("tracked");
        assert!(left <= Duration::from_secs(60) && left > Duration::from_secs(58));
        assert_eq!(t.remaining(1, SetpointAxis::Reactive), None);
        assert_eq!(t.remaining(2, SetpointAxis::Active), None);
    }

    /// A tracked pair whose deadline has already passed reports None,
    /// same as an untracked one — the sweep just hasn't caught up yet.
    #[test]
    fn remaining_is_none_once_the_deadline_has_passed() {
        let t = TimeoutTracker::new();
        t.actuate_and_arm(3, SetpointAxis::Active, Duration::ZERO, || Ok::<(), ()>(()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(t.remaining(3, SetpointAxis::Active), None);
    }

    /// A renewal that lands while the sweep is mid-drain must not be
    /// wiped. This is a genuine cross-thread pin, not just a
    /// single-threaded ordering check: from inside the
    /// `reset_expired_with` callback (i.e. while the tracker's lock
    /// is held) we spawn a thread that calls `actuate_and_arm` on the
    /// same key, and assert it has NOT completed after a generous
    /// sleep. A collect-then-release-then-callback implementation
    /// (the old `remove_expired` + per-id reset shape) would let that
    /// thread race straight through and this assertion would fail;
    /// the real lock-holding implementation blocks the renewal until
    /// `reset_expired_with` returns. We then join the thread and
    /// confirm the renewal landed (freshly armed, not expired).
    #[test]
    fn rearmed_key_survives_the_expiry_sweep() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let t = TimeoutTracker::new();
        t.actuate_and_arm(7, SetpointAxis::Active, Duration::ZERO, || Ok::<(), ()>(()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(1));

        let completed = Arc::new(AtomicBool::new(false));
        let mut renewal: Option<std::thread::JoinHandle<()>> = None;
        let mut reset = Vec::new();
        t.reset_expired_with(|id, axis| {
            reset.push((id, axis));
            let t2 = t.clone();
            let completed2 = completed.clone();
            let h = std::thread::spawn(move || {
                t2.actuate_and_arm(id, axis, Duration::from_secs(900), || Ok::<(), ()>(()))
                    .unwrap();
                completed2.store(true, Ordering::SeqCst);
            });
            // Generous sleep so this pins real blocking rather than
            // getting lucky on a fast machine: if the renewal thread
            // can complete while we're still inside this callback,
            // the tracker isn't holding its lock across the sweep.
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                !completed.load(Ordering::SeqCst),
                "renewal must block on the tracker lock while the sweep callback is running"
            );
            renewal = Some(h);
        });
        // The lock is released now (reset_expired_with returned) — the
        // renewal thread can proceed.
        renewal.take().unwrap().join().unwrap();
        assert!(completed.load(Ordering::SeqCst));
        assert_eq!(reset, vec![(7, SetpointAxis::Active)]);
        // The renewal landed after the sweep released the lock, so
        // it's freshly armed — must not read back as expired.
        assert!(t.remaining(7, SetpointAxis::Active).is_some());
    }

    /// `clear` drops every armed deadline, so a later sweep resets
    /// nothing — the reset/hot-reload case, where the components those
    /// deadlines named no longer exist.
    #[test]
    fn clear_drops_every_deadline() {
        let t = TimeoutTracker::new();
        t.actuate_and_arm(7, SetpointAxis::Active, Duration::ZERO, || Ok::<(), ()>(()))
            .unwrap();
        t.actuate_and_arm(8, SetpointAxis::Reactive, Duration::ZERO, || {
            Ok::<(), ()>(())
        })
        .unwrap();
        t.actuate_and_arm(9, SetpointAxis::Active, Duration::from_secs(3600), || {
            Ok::<(), ()>(())
        })
        .unwrap();

        t.clear();

        std::thread::sleep(Duration::from_millis(2));
        let mut reset = Vec::new();
        t.reset_expired_with(|id, axis| reset.push((id, axis)));
        assert_eq!(
            reset,
            Vec::new(),
            "a cleared tracker must have nothing left to expire"
        );
        // The still-live one is gone too — clear is unconditional.
        assert_eq!(t.remaining(9, SetpointAxis::Active), None);
    }

    /// `remove_component` drops BOTH of one id's axes and leaves every
    /// other component's deadlines alone.
    #[test]
    fn remove_component_drops_both_axes_of_that_id_only() {
        let t = TimeoutTracker::new();
        for (id, axis) in [
            (7, SetpointAxis::Active),
            (7, SetpointAxis::Reactive),
            (8, SetpointAxis::Active),
        ] {
            t.actuate_and_arm(id, axis, Duration::from_secs(3600), || Ok::<(), ()>(()))
                .unwrap();
        }

        t.remove_component(7);

        assert_eq!(t.remaining(7, SetpointAxis::Active), None);
        assert_eq!(t.remaining(7, SetpointAxis::Reactive), None);
        assert!(
            t.remaining(8, SetpointAxis::Active).is_some(),
            "another component's deadline must survive"
        );
        // Removing an untracked id is a no-op, not a panic.
        t.remove_component(999);
        assert!(t.remaining(8, SetpointAxis::Active).is_some());
    }

    /// A failed actuation must not arm a deadline (otherwise the sweep
    /// later resets the PREVIOUS, still-valid command).
    #[test]
    fn failed_actuation_arms_nothing() {
        let t = TimeoutTracker::new();
        let r = t.actuate_and_arm(
            7,
            SetpointAxis::Active,
            Duration::from_secs(900),
            || Err(()),
        );
        assert!(r.is_err());
        let mut reset = Vec::new();
        // Nothing armed → nothing expires even at t+∞ (use a zero-lifetime
        // probe on another key to prove the sweep itself works).
        t.actuate_and_arm(8, SetpointAxis::Active, Duration::ZERO, || Ok::<(), ()>(()))
            .unwrap();
        t.reset_expired_with(|id, axis| reset.push((id, axis)));
        assert_eq!(reset, vec![(8, SetpointAxis::Active)]);
    }
}
