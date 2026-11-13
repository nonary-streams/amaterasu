//! Automatic updates: the administrator's saved choice of how far an update
//! proceeds unattended, and the schedule of the checks that start one.
//!
//! Nothing here performs a step or stores the choice. The service asks which
//! step is due on every host tick, so that answer only compares values
//! already in memory. The choice is saved with the module's other settings.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The action name `routes::resolve` gives a request that chooses the mode.
pub(crate) const ACTION: &str = "package_update_automatic";

/// How far an update proceeds without an administrator asking. A request,
/// the settings document and the status reply all name a mode in snake case.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AutomaticMode {
    /// Nothing happens until the console asks.
    #[default]
    Off,
    /// Checks on a schedule and downloads the update a check finds.
    Download,
    /// As `Download`, then asks the host to activate the downloaded set.
    Install,
}

impl AutomaticMode {
    /// Takes a reference because serde decides with it whether a settings
    /// document carries the mode.
    pub(crate) fn is_off(&self) -> bool {
        *self == Self::Off
    }

    /// The mode a console request chooses. The request is its action and one
    /// known mode; anything else chooses nothing.
    pub(crate) fn requested(payload: &Value) -> Option<Self> {
        let request = payload.as_object()?;
        if request.len() != 2 || request.get("action")?.as_str()? != ACTION {
            return None;
        }
        Self::deserialize(request.get("mode")?).ok()
    }
}

#[cfg(target_os = "linux")]
pub(super) use schedule::{Automatic, Progress, Step};
#[cfg(all(test, target_os = "linux"))]
pub(super) use schedule::{CHECK_INTERVAL, FIRST_CHECK_DELAY};

#[cfg(target_os = "linux")]
mod schedule {
    use super::AutomaticMode;
    use serde_json::{Value, json};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// The first check waits until the host has finished starting.
    pub(crate) const FIRST_CHECK_DELAY: Duration = Duration::from_secs(2 * 60);
    /// A check is due this long after the last one finished.
    pub(crate) const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

    /// A step the automatic flow starts on its own.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum Step {
        Check,
        Download,
        Activate,
    }

    /// How far the update has come, as far as the automatic flow continues it.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum Progress {
        /// Nothing to continue: no update is known, or the last step ended
        /// where only a check or the administrator moves it on.
        Settled,
        /// A check found an update that is not downloaded.
        Available,
        /// The update is verified in the candidate cache.
        Ready,
    }

    /// The chosen mode and when the next scheduled check is due.
    pub(crate) struct Automatic {
        mode: AutomaticMode,
        next_check: Instant,
        /// `next_check` on the wall clock, for the status reply.
        next_check_unix_ms: u64,
        /// A step failed or the administrator cancelled. Nothing continues
        /// on its own until a check or a download starts again.
        held: bool,
        /// The check that is running was started by the schedule.
        scheduled_check: bool,
    }

    impl Automatic {
        /// The schedule of a module that started at `now`.
        pub(crate) fn starting(now: Instant) -> Self {
            let mut automatic = Self {
                mode: AutomaticMode::Off,
                next_check: now,
                next_check_unix_ms: 0,
                held: false,
                scheduled_check: false,
            };
            automatic.schedule_check(now, FIRST_CHECK_DELAY);
            automatic
        }

        /// Follows the saved choice. A new choice applies to the update as
        /// it stands, whatever stopped the flow before.
        pub(crate) fn select(&mut self, mode: AutomaticMode) {
            if self.mode != mode {
                self.mode = mode;
                self.held = false;
            }
        }

        /// The one step to start now, if any. Called on every host tick: it
        /// reads no clock, allocates nothing and touches no file or socket.
        pub(crate) fn next_step(&self, now: Instant, progress: Progress) -> Option<Step> {
            if self.mode.is_off() {
                return None;
            }
            if !self.held {
                match progress {
                    Progress::Available => return Some(Step::Download),
                    Progress::Ready if self.mode == AutomaticMode::Install => {
                        return Some(Step::Activate);
                    }
                    Progress::Ready | Progress::Settled => {}
                }
            }
            (now >= self.next_check).then_some(Step::Check)
        }

        /// The schedule is about to start a check. The next one is planned
        /// first, so a check that cannot start is not tried again at once.
        pub(crate) fn scheduled_check_starting(&mut self, now: Instant) {
            self.schedule_check(now, CHECK_INTERVAL);
        }

        /// The check the schedule asked for is running.
        pub(crate) fn scheduled_check_started(&mut self) {
            self.scheduled_check = true;
        }

        /// A check or a download began, so the flow may continue from it.
        pub(crate) fn work_started(&mut self) {
            self.held = false;
        }

        /// A check ended, however it ended and whoever asked for it. Returns
        /// whether the schedule had started it.
        pub(crate) fn check_finished(&mut self, now: Instant) -> bool {
            self.schedule_check(now, CHECK_INTERVAL);
            std::mem::take(&mut self.scheduled_check)
        }

        /// Stops the flow where it is until work starts again.
        pub(crate) fn hold(&mut self) {
            self.held = true;
        }

        /// The `automatic` object of a status reply. `checks` is false when
        /// the host has no source repository to check.
        pub(crate) fn describe(&self, checks: bool) -> Value {
            let scheduled = checks && !self.mode.is_off();
            json!({
                "mode": self.mode,
                "next_check_unix_ms": scheduled.then_some(self.next_check_unix_ms),
            })
        }

        fn schedule_check(&mut self, now: Instant, delay: Duration) {
            self.next_check = now + delay;
            let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
            self.next_check_unix_ms = unix_milliseconds().saturating_add(delay_ms);
        }
    }

    fn unix_milliseconds() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const SECOND: Duration = Duration::from_secs(1);

        fn choosing(mode: AutomaticMode, started: Instant) -> Automatic {
            let mut automatic = Automatic::starting(started);
            automatic.select(mode);
            automatic
        }

        #[test]
        fn nothing_is_due_while_automatic_updates_are_off() {
            let started = Instant::now();
            let automatic = Automatic::starting(started);
            let later = started + CHECK_INTERVAL * 4;
            for progress in [Progress::Settled, Progress::Available, Progress::Ready] {
                assert_eq!(automatic.next_step(later, progress), None);
            }
            assert_eq!(
                automatic.describe(true),
                json!({"mode": "off", "next_check_unix_ms": null})
            );
        }

        #[test]
        fn the_first_check_waits_for_the_host_and_later_ones_for_the_interval() {
            let started = Instant::now();
            let mut automatic = choosing(AutomaticMode::Download, started);
            let first = started + FIRST_CHECK_DELAY;
            assert_eq!(automatic.next_step(first - SECOND, Progress::Settled), None);
            assert_eq!(
                automatic.next_step(first, Progress::Settled),
                Some(Step::Check)
            );

            // Planned again before it starts, and again when it finishes.
            automatic.scheduled_check_starting(first);
            assert_eq!(automatic.next_step(first + SECOND, Progress::Settled), None);
            automatic.scheduled_check_started();
            let finished = first + SECOND * 30;
            assert!(automatic.check_finished(finished));
            let next = finished + CHECK_INTERVAL;
            assert_eq!(automatic.next_step(next - SECOND, Progress::Settled), None);
            assert_eq!(
                automatic.next_step(next, Progress::Settled),
                Some(Step::Check)
            );

            // A check the administrator asked for moves the schedule too.
            let manual = next - CHECK_INTERVAL / 2;
            assert!(!automatic.check_finished(manual));
            assert_eq!(automatic.next_step(next, Progress::Settled), None);
            assert_eq!(
                automatic.next_step(manual + CHECK_INTERVAL, Progress::Settled),
                Some(Step::Check)
            );
        }

        #[test]
        fn each_mode_continues_an_update_only_as_far_as_it_says() {
            let started = Instant::now();
            let download = choosing(AutomaticMode::Download, started);
            assert_eq!(
                download.next_step(started, Progress::Available),
                Some(Step::Download)
            );
            assert_eq!(download.next_step(started, Progress::Ready), None);

            let install = choosing(AutomaticMode::Install, started);
            assert_eq!(
                install.next_step(started, Progress::Available),
                Some(Step::Download)
            );
            assert_eq!(
                install.next_step(started, Progress::Ready),
                Some(Step::Activate)
            );
            assert_eq!(install.next_step(started, Progress::Settled), None);
        }

        #[test]
        fn a_held_flow_only_checks_until_work_starts_again() {
            let started = Instant::now();
            let mut automatic = choosing(AutomaticMode::Install, started);
            automatic.hold();
            for progress in [Progress::Available, Progress::Ready] {
                assert_eq!(automatic.next_step(started, progress), None);
                assert_eq!(
                    automatic.next_step(started + FIRST_CHECK_DELAY, progress),
                    Some(Step::Check)
                );
            }
            automatic.work_started();
            assert_eq!(
                automatic.next_step(started, Progress::Ready),
                Some(Step::Activate)
            );

            // Choosing the same mode again changes nothing; another one
            // applies to the update as it stands.
            automatic.hold();
            automatic.select(AutomaticMode::Install);
            assert_eq!(automatic.next_step(started, Progress::Available), None);
            automatic.select(AutomaticMode::Download);
            assert_eq!(
                automatic.next_step(started, Progress::Available),
                Some(Step::Download)
            );
        }

        #[test]
        fn the_status_names_the_mode_and_the_next_check() {
            let before = unix_milliseconds();
            let automatic = choosing(AutomaticMode::Install, Instant::now());
            let described = automatic.describe(true);
            assert_eq!(described["mode"], "install");
            let next = described["next_check_unix_ms"].as_u64().unwrap();
            let delay = FIRST_CHECK_DELAY.as_millis() as u64;
            assert!((before + delay..=unix_milliseconds() + delay).contains(&next));
            // A host with nothing to check has no check to announce.
            assert_eq!(
                automatic.describe(false),
                json!({"mode": "install", "next_check_unix_ms": null})
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_chooses_one_known_mode_and_nothing_else() {
        for (name, mode) in [
            ("off", AutomaticMode::Off),
            ("download", AutomaticMode::Download),
            ("install", AutomaticMode::Install),
        ] {
            assert_eq!(
                AutomaticMode::requested(&json!({"action": ACTION, "mode": name})),
                Some(mode)
            );
            assert_eq!(serde_json::to_value(mode).unwrap(), name);
        }
        for refused in [
            json!({"action": ACTION}),
            json!({"action": ACTION, "mode": "Install"}),
            json!({"action": ACTION, "mode": "always"}),
            json!({"action": ACTION, "mode": ""}),
            json!({"action": ACTION, "mode": null}),
            json!({"action": ACTION, "mode": true}),
            json!({"action": ACTION, "mode": ["install"]}),
            json!({"action": ACTION, "mode": "install", "force": true}),
            json!({"action": ACTION, "automatic": "install"}),
            json!({"action": "package_update_activate", "mode": "install"}),
            json!({"mode": "install"}),
            json!(["install"]),
            json!("install"),
        ] {
            assert_eq!(AutomaticMode::requested(&refused), None, "{refused}");
        }
    }
}
