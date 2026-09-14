//! The injected clock (ADR-0011, PLAN M0.2).
//!
//! TTL renders as a local countdown and the degraded Viewer header shows a read
//! age, so a rendered frame is a function of *when* it was drawn. Golden-frame
//! tests need it to be a function of state alone, so the core never calls
//! `Instant::now()` — time arrives through here.

/// Wall-clock time source. The shell supplies a real one; tests supply a fixed
/// one.
///
/// `Send + Sync` so one clock can be shared with every task the shell spawns:
/// a message's timestamp comes from here, never from an inline
/// `SystemTime::now()` (review H4).
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    ///
    /// Epoch time, not a monotonic reading from an arbitrary origin: the keys
    /// pane's TTL countdown stores epoch seconds, and a stream entry's age is
    /// measured against the epoch milliseconds Redis embeds in its ID. A clock
    /// with any other origin makes both silently wrong. This used to say
    /// "monotonic, never wall time", the opposite of what every caller needed
    /// (review H4).
    fn now_epoch_ms(&self) -> u64;
}

/// A clock frozen at a fixed instant, for tests and golden frames.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_epoch_ms(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_does_not_advance() {
        let c = FixedClock(1_000);
        assert_eq!(c.now_epoch_ms(), c.now_epoch_ms());
    }
}
