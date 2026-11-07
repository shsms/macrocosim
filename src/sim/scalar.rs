//! A scalar component input the physics tick reads while an HTTP
//! write may change it, so the `f32` lives in an atomic as its bits.
//! A boiler's steam demand is one `Scalar` that a write sets; a
//! meter's power and a solar inverter's driven sunlight hold one
//! behind a lock that a write replaces whole.

use std::sync::atomic::{AtomicU32, Ordering};

/// One `f32` that the physics tick reads and a knob write replaces.
pub struct Scalar(AtomicU32);

/// A clone holds the value as it is now, in an atomic of its own.
impl Clone for Scalar {
    fn clone(&self) -> Self {
        Self::new(self.get())
    }
}

impl Scalar {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }

    /// The current value. Acquire pairs with `set`'s Release, so the
    /// physics tick sees the latest write.
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Acquire))
    }

    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_replaces_the_value_and_a_clone_keeps_its_own() {
        let s = Scalar::new(1.5);
        let copy = s.clone();
        s.set(-2.0);
        assert_eq!(s.get(), -2.0);
        assert_eq!(copy.get(), 1.5);
    }
}
