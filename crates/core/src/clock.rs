//! The injected clock (ADR-0011, PLAN M0.2).
//!
//! TTL renders as a local countdown and the degraded Viewer header shows a read
//! age, so a rendered frame is a function of *when* it was drawn. Golden-frame
//! tests need it to be a function of state alone, so the core never calls
//! `Instant::now()` — time arrives through here.

/// Monotonic time source. The shell supplies a real one; tests supply a fixed one.
pub trait Clock {
    /// Milliseconds since an arbitrary fixed origin. Monotonic, never wall time.
    fn now_ms(&self) -> u64;
}

/// A clock frozen at a fixed instant, for tests and golden frames.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_does_not_advance() {
        let c = FixedClock(1_000);
        assert_eq!(c.now_ms(), c.now_ms());
    }
}
