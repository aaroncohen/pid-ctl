//! Deadline-based `loop` scheduling — see `pid-ctl_plan.md` (Scheduling).

use std::time::{Duration, Instant};

/// Returns the next sleep target after waking for `tick_deadline`.
///
/// Advances by `interval` from the scheduled boundary (`tick_deadline + interval`), not from `now`,
/// so overrun does not accumulate drift. If `now` is past one or more future boundaries, the
/// deadline steps forward by whole `interval` steps until strictly after `now` (no catch-up burst
/// of missed ticks).
#[must_use]
pub fn next_deadline_after_tick(
    tick_deadline: Instant,
    interval: Duration,
    now: Instant,
) -> Instant {
    let mut next = tick_deadline + interval;
    while next <= now {
        next += interval;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_time_next_is_tick_plus_interval() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(100);
        let tick_deadline = t0 + interval;
        let now = tick_deadline;
        let next = next_deadline_after_tick(tick_deadline, interval, now);
        assert_eq!(next, tick_deadline + interval);
    }

    #[test]
    fn overrun_does_not_anchor_to_now() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(100);
        let tick_deadline = t0 + interval;
        // 150 ms late: would be t0+250 with buggy now+interval; expect grid at t0+300.
        let now = tick_deadline + Duration::from_millis(150);
        let next = next_deadline_after_tick(tick_deadline, interval, now);
        assert_eq!(next, t0 + Duration::from_millis(300));
    }

    #[test]
    fn large_slip_skips_missed_boundaries_without_burst() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(100);
        let tick_deadline = t0 + interval;
        let now = t0 + Duration::from_millis(350);
        let next = next_deadline_after_tick(tick_deadline, interval, now);
        assert_eq!(next, t0 + Duration::from_millis(400));
    }
}
