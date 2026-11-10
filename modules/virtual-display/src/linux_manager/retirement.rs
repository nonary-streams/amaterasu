//! Paces the provider's own attempts to retire an output it parked.
//!
//! A parked output stays lit as the desktop's screen while no monitor can take
//! over. No caller owns it any more, so nothing asks for it again before the
//! next stream or a provider restart. The provider therefore looks for a
//! monitor itself. Each poll reads connector status from sysfs only; a real
//! attempt, which reaches the broker and the compositor, runs when a display
//! outside the virtual pool is attached, immediately when that set changes and
//! otherwise at growing intervals.
use super::connectors::ConnectedDisplays;
use std::time::{Duration, Instant};

/// How often connector status is read while an output is parked.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// A monitor that was only slow to start is usually ready this soon.
const FIRST_ATTEMPT_AFTER: Duration = Duration::from_secs(1);
/// Waits after the first, second, ... failed attempt; the last one repeats.
const ATTEMPT_DELAYS: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];
/// An unplugged parked output only awaits its saved layout. That layout is not
/// applied to the desktop again after this many attempts: the user may have
/// arranged the desktop since, and a restart or the next stream still restores.
pub(super) const UNPLUGGED_ATTEMPTS: u32 = 4;

pub(super) struct Retirement {
    attempts: u32,
    next_attempt: Instant,
    /// Displays outside the virtual pool seen at the last poll.
    other_displays: Option<Vec<String>>,
    last_outcome: Option<String>,
}

impl Retirement {
    pub fn begin(now: Instant) -> Self {
        Self {
            attempts: 0,
            next_attempt: now + FIRST_ATTEMPT_AFTER,
            other_displays: None,
            last_outcome: None,
        }
    }

    /// How long the worker may sleep before the next poll. An attempt that is
    /// overdue but not yet possible is polled for at the ordinary interval.
    pub fn wait(&self, now: Instant) -> Duration {
        let until_attempt = self.next_attempt.saturating_duration_since(now);
        if until_attempt.is_zero() {
            POLL_INTERVAL
        } else {
            until_attempt.min(POLL_INTERVAL)
        }
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Whether an attempt should run now. `displays` is `None` when connector
    /// status cannot be read; attempts then follow the delays alone.
    pub fn due(&mut self, now: Instant, displays: Option<&ConnectedDisplays>) -> bool {
        let Some(displays) = displays else {
            self.other_displays = None;
            return now >= self.next_attempt;
        };
        let changed = self.other_displays.as_ref() != Some(&displays.other);
        if changed {
            self.other_displays = Some(displays.other.clone());
        }
        if displays.other.is_empty() {
            // Nothing is attached that could show the desktop instead.
            return false;
        }
        if changed && self.attempts > 0 {
            // A monitor was plugged in or switched on: try without waiting.
            self.attempts = 0;
            return true;
        }
        now >= self.next_attempt
    }

    pub fn attempted(&mut self, now: Instant) {
        let delay = ATTEMPT_DELAYS[(self.attempts as usize).min(ATTEMPT_DELAYS.len() - 1)];
        self.attempts = self.attempts.saturating_add(1);
        self.next_attempt = now + delay;
    }

    /// True when `outcome` differs from the previous attempt's, so a repeating
    /// failure is written to the journal once.
    pub fn outcome_changed(&mut self, outcome: &str) -> bool {
        if self.last_outcome.as_deref() == Some(outcome) {
            return false;
        }
        self.last_outcome = Some(outcome.to_owned());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn displays(other: &[&str]) -> ConnectedDisplays {
        ConnectedDisplays {
            virtual_outputs: vec!["Virtual-9".into()],
            other: other.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn first_attempt_follows_shortly_and_failures_back_off() {
        let start = Instant::now();
        let mut retirement = Retirement::begin(start);
        let monitor = displays(&["card1-HDMI-A-1"]);
        assert!(!retirement.due(start, Some(&monitor)));
        assert_eq!(retirement.wait(start), FIRST_ATTEMPT_AFTER);
        let mut now = start + FIRST_ATTEMPT_AFTER;
        let mut waits = Vec::new();
        for _ in 0..7 {
            assert!(retirement.due(now, Some(&monitor)));
            retirement.attempted(now);
            let mut waited = Duration::ZERO;
            while !retirement.due(now, Some(&monitor)) {
                let step = retirement.wait(now);
                assert!(step <= POLL_INTERVAL);
                waited += step;
                now += step;
            }
            waits.push(waited.as_secs());
        }
        assert_eq!(waits, [2, 5, 10, 30, 60, 60, 60]);
    }

    #[test]
    fn absent_monitor_costs_no_attempt_and_its_return_is_tried_at_once() {
        let start = Instant::now();
        let mut retirement = Retirement::begin(start);
        let monitor = displays(&["card1-HDMI-A-1"]);
        let later = start + Duration::from_secs(3600);
        assert!(!retirement.due(later, Some(&displays(&[]))));
        assert_eq!(retirement.attempts(), 0);
        assert_eq!(retirement.wait(later), POLL_INTERVAL);
        // Switched on after an hour: the first poll that sees it attempts.
        assert!(retirement.due(later, Some(&monitor)));
        retirement.attempted(later);
        retirement.attempted(later);
        retirement.attempted(later);
        assert!(!retirement.due(later, Some(&monitor)));
        // Off and on again resets the growing delay.
        assert!(!retirement.due(later, Some(&displays(&[]))));
        assert!(retirement.due(later, Some(&monitor)));
        assert_eq!(retirement.attempts(), 0);
    }

    #[test]
    fn unreadable_connector_status_falls_back_to_the_delays() {
        let start = Instant::now();
        let mut retirement = Retirement::begin(start);
        assert!(!retirement.due(start, None));
        assert!(retirement.due(start + FIRST_ATTEMPT_AFTER, None));
        retirement.attempted(start + FIRST_ATTEMPT_AFTER);
        assert!(!retirement.due(start + FIRST_ATTEMPT_AFTER, None));
    }

    #[test]
    fn repeated_outcome_is_reported_once() {
        let mut retirement = Retirement::begin(Instant::now());
        assert!(retirement.outcome_changed("no_survivor"));
        assert!(!retirement.outcome_changed("no_survivor"));
        assert!(retirement.outcome_changed("restore_failed"));
    }
}
