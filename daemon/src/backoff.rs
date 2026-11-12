//! Reconnect delays for the control channel: exponential backoff with jitter.
//!
//! The delay doubles from [`BASE`] up to [`CAP`] with each failed attempt, and
//! each wait is drawn uniformly from the upper half of that window ("equal
//! jitter"), so daemons that lost the control plane together do not all
//! reconnect at the same instant, yet none retries sooner than half the step.
//! A session that stayed up for [`STABLE_AFTER`] resets the backoff.

use std::time::Duration;

/// First retry window.
pub(crate) const BASE: Duration = Duration::from_secs(1);
/// Largest retry window.
pub(crate) const CAP: Duration = Duration::from_secs(60);
/// A session lasting this long counts as healthy and resets the backoff, so a
/// control plane that accepts and then drops the connection at once (a rejected
/// credential, a crash loop) is still backed off.
pub(crate) const STABLE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub(crate) struct Backoff {
    attempt: u32,
}

impl Backoff {
    /// The wait before the next attempt; advances the backoff.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = delay_for(self.attempt, random_unit());
        self.attempt = self.attempt.saturating_add(1);
        delay
    }

    /// Forget earlier failures after a healthy session.
    pub(crate) fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// The window for `attempt` (0-based), then a point in its upper half chosen
/// by `unit` in `[0, 1]`.
pub(crate) fn delay_for(attempt: u32, unit: f64) -> Duration {
    let window = BASE
        .checked_mul(1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX))
        .unwrap_or(CAP)
        .min(CAP);
    let half = window / 2;
    half + half.mul_f64(unit.clamp(0.0, 1.0))
}

/// A uniform value in `[0, 1)` from the OS generator behind `uuid` v4, which
/// the daemon already depends on.
fn random_unit() -> f64 {
    let bits = uuid::Uuid::new_v4().as_u128() as u64 >> 11;
    bits as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_doubles_up_to_the_cap() {
        let upper: Vec<u64> = (0..9).map(|n| delay_for(n, 1.0).as_secs()).collect();
        assert_eq!(upper, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(delay_for(u32::MAX, 1.0), CAP);
    }

    #[test]
    fn jitter_stays_in_the_upper_half() {
        assert_eq!(delay_for(3, 0.0), Duration::from_secs(4));
        assert_eq!(delay_for(3, 0.5), Duration::from_secs(6));
        assert_eq!(delay_for(10, 0.0), Duration::from_secs(30));
        assert_eq!(delay_for(10, 7.0), CAP);
        for _ in 0..1000 {
            let unit = random_unit();
            assert!((0.0..1.0).contains(&unit));
        }
    }

    #[test]
    fn reset_starts_over() {
        let mut backoff = Backoff::default();
        for _ in 0..5 {
            backoff.next_delay();
        }
        assert!(backoff.next_delay() >= Duration::from_secs(16));
        backoff.reset();
        assert!(backoff.next_delay() <= BASE);
    }
}
