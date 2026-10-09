//! `log.*` printers + math + RNG helpers used by ported microsim
//! configs.

use std::sync::Mutex;

use tulisp::TulispContext;

/// Process-global seedable generator backing `(random)`. `None` (the
/// default) means draw from the OS thread RNG; once `(set-random-seed
/// N)` installs a [`SplitMix64`], every `(random)` — including the
/// stochastic scenario helpers (`random-outage`, `random-uniform`) — is
/// reproducible, so a scenario run can be replayed bit-for-bit (e.g. in
/// CI). One sim per process, so a single global seed is the right grain.
static SEEDED_RNG: Mutex<Option<SplitMix64>> = Mutex::new(None);

/// SplitMix64 — a tiny, fast, well-distributed seedable PRNG. Rolled by
/// hand so seeding doesn't depend on a particular `rand` feature being
/// enabled; the unseeded path still uses `rand::thread_rng()`.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// One `(random LIMIT)` draw, from the seeded generator when one is
/// installed and the OS RNG otherwise. `LIMIT` absent → a full-range
/// i64; present → `[0, LIMIT)` with `LIMIT<=0` clamped to 1 (so
/// `(random (length '()))` yields 0 rather than panicking).
fn random_draw(limit: Option<i64>) -> i64 {
    use rand::Rng;
    let mut guard = SEEDED_RNG.lock().expect("rng mutex");
    match (guard.as_mut(), limit) {
        (Some(rng), Some(l)) => (rng.next_u64() % l.max(1) as u64) as i64,
        (Some(rng), None) => rng.next_u64() as i64,
        (None, Some(l)) => rand::thread_rng().gen_range(0..l.max(1)),
        (None, None) => rand::thread_rng().r#gen(),
    }
}

pub(super) fn register(ctx: &mut TulispContext) {
    for (name, level) in [
        ("log.info", log::Level::Info),
        ("log.warn", log::Level::Warn),
        ("log.error", log::Level::Error),
        ("log.debug", log::Level::Debug),
        ("log.trace", log::Level::Trace),
    ] {
        let doc = format!(
            "Write MSG to the log at the {} level.",
            level.as_str().to_lowercase()
        );
        ctx.defun((name, ["msg"], doc), move |msg: String| {
            log::log!(level, "{msg}")
        });
    }
    // Math + RNG helpers used by ported microsim configs.
    ctx.defun(
        (
            "ceiling",
            ["n"],
            "Return the smallest integer not less than N.",
        ),
        |n: f64| n.ceil() as i64,
    )
    .defun(
        (
            "floor",
            ["n"],
            "Return the largest integer not greater than N.",
        ),
        |n: f64| n.floor() as i64,
    )
    .defun(
        ("sin", ["n"], "Return the sine of N, an angle in radians."),
        |n: f64| n.sin(),
    )
    .defun(
        ("cos", ["n"], "Return the cosine of N, an angle in radians."),
        |n: f64| n.cos(),
    )
    .defun(
        (
            "random",
            ["limit"],
            "Return a random integer, from 0 to LIMIT - 1.\n\n\
             A LIMIT of 0 or less counts as 1. Without LIMIT, the integer \
             can be any integer. After set-random-seed, the integers are \
             the same in every run.",
        ),
        |limit: Option<i64>| random_draw(limit),
    )
    .defun(
        (
            "set-random-seed",
            ["seed"],
            "Seed random with SEED, so it returns the same integers in every run.\n\n\
             The seed holds for the whole process, every microgrid included. \
             Return t.",
        ),
        |seed: i64| {
            *SEEDED_RNG.lock().expect("rng mutex") = Some(SplitMix64(seed as u64));
            true
        },
    )
    .defun(
        (
            "clear-random-seed",
            "Make random draw from the system's random numbers again.\n\n\
             This undoes set-random-seed. Return t.",
        ),
        || {
            *SEEDED_RNG.lock().expect("rng mutex") = None;
            true
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::config_with;
    use super::SplitMix64;

    /// macrocosim's `random` replaces the one tulisp adds, which refuses a
    /// LIMIT of 0 and takes t: macrocosim's counts 0 as 1 and takes only an
    /// integer. The test sets no seed, since the seed is shared by every test
    /// in the process.
    #[test]
    fn random_is_macrocosims() {
        let (cfg, _dir) = config_with("nil");
        assert_eq!(cfg.eval("(random 0)").unwrap(), "0");
        assert!(cfg.eval("(random t)").is_err());
    }

    /// Same seed → same stream; different seeds diverge. This is the
    /// property the scenario-reproducibility story rests on.
    #[test]
    fn splitmix64_is_deterministic_per_seed() {
        let draws = |seed: u64| {
            let mut r = SplitMix64(seed);
            [r.next_u64(), r.next_u64(), r.next_u64()]
        };
        assert_eq!(draws(42), draws(42));
        assert_ne!(draws(42), draws(43));
    }
}
