use chrono::DateTime;
use chrono::Utc;

/// A source of wall-clock time.
///
/// Injected wherever the crate reads "now" so tests can pin it. Covers reading
/// time only — sleeping still goes through `tokio::time::sleep`.
pub trait Clock {
    /// The current instant, in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock pinned to a fixed instant.
///
/// Public rather than `#[cfg(test)]` because the integration tests under
/// `tests/` link this crate externally and cannot see test-only items.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn fixed_clock_returns_its_instant() {
        let at = dt("2026-07-20T09:31:52Z");
        let c = FixedClock(at);
        assert_eq!(c.now(), at);
        // Stable across reads — this is the property tests depend on.
        assert_eq!(c.now(), at);
    }

    #[test]
    fn system_clock_returns_a_plausible_now() {
        // Not asserting an exact instant; only that it is a real wall clock
        // somewhere after this code was written.
        let c = SystemClock;
        assert!(c.now() > dt("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn clock_is_usable_behind_a_trait_object() {
        let at = dt("2026-07-20T09:31:52Z");
        let c: &dyn Clock = &FixedClock(at);
        assert_eq!(c.now(), at);
    }
}
