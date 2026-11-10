//! Applies a saved desktop layout and waits for the compositor to report it.
//!
//! A physical output that was just enabled is not immediately readable in its
//! final state: the modeset, the output order and the published properties
//! settle over the following moments. One readback taken straight after the
//! command therefore reports a difference that is no longer there shortly
//! afterwards. Like Vibeshine, poll the readback for a bounded time and apply
//! the remaining difference again before concluding that restoration failed.
use super::layout::Layout;
use serde_json::Value;
use std::time::Duration;

/// Pause between two readbacks of the desktop.
pub(super) const READBACK_INTERVAL: Duration = Duration::from_millis(50);
/// A difference that outlives this window is applied again.
const REAPPLY_AFTER: Duration = Duration::from_millis(750);
/// The layout is applied at most this many times in one restoration.
const MAX_APPLICATIONS: u32 = 3;
/// Restoration fails when the layout still differs this long after it began.
const SETTLE_DEADLINE: Duration = Duration::from_secs(3);

/// The desktop operations a restoration needs, with its own clock so the
/// pacing can be exercised without a compositor.
pub(super) trait Desktop {
    fn query(&mut self) -> Result<Value, String>;
    fn apply(&mut self, arguments: &[String]) -> Result<(), String>;
    /// Time since this restoration began.
    fn elapsed(&self) -> Duration;
    fn pause(&mut self, duration: Duration);
}

/// How a restoration that reached the saved layout got there.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Settled {
    pub applications: u32,
    pub waited: Duration,
    /// Readbacks that still differed after the layout had been applied, and
    /// the last such difference. Recorded so a slow output can be identified.
    pub differing_readbacks: u32,
    pub last_difference: Option<String>,
}

/// Returns once the desktop reports every still-connected saved output in its
/// saved state. The error names the first output and property that differ.
pub(super) fn restore_layout(
    desktop: &mut impl Desktop,
    layout: &Layout,
) -> Result<Settled, String> {
    let mut applications = 0;
    let mut applied_at = Duration::ZERO;
    let mut differing_readbacks = 0;
    let mut last_difference = None;
    loop {
        let current = desktop.query()?;
        let Some(mut difference) = layout.difference(&current) else {
            return Ok(Settled {
                applications,
                waited: desktop.elapsed(),
                differing_readbacks,
                last_difference,
            });
        };
        if applications > 0 {
            differing_readbacks += 1;
            last_difference = Some(difference.clone());
        }
        let now = desktop.elapsed();
        let reapply_due = applications == 0 || now.saturating_sub(applied_at) >= REAPPLY_AFTER;
        if reapply_due && applications < MAX_APPLICATIONS {
            // The mode list of an output that is still starting can lack the
            // saved mode for a moment, and a busy compositor can reject one
            // request. Both are retried within the same deadline.
            let applied = layout
                .arguments(&current)
                .and_then(|arguments| desktop.apply(&arguments));
            if let Err(error) = applied {
                difference = error;
            }
            applications += 1;
            applied_at = desktop.elapsed();
        }
        if desktop.elapsed() >= SETTLE_DEADLINE {
            return Err(format!(
                "existing desktop restoration readback differs: {difference}"
            ));
        }
        desktop.pause(READBACK_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn physical(enabled: bool, priority: u64) -> Value {
        json!({"name":"HDMI-A-1","connected":true,"enabled":enabled,"currentModeId":"2",
            "modes":[{"id":"2","size":{"width":3840,"height":2160},"refreshRate":119.88}],
            "pos":{"x":0,"y":0},"scale":2.0,"rotation":1,"hdr":false,"wcg":false,"priority":priority})
    }
    fn desktop_with(output: Value) -> Value {
        json!({"outputs":[output,
            {"name":"Virtual-9","connected":true,"enabled":true,"currentModeId":"1",
             "modes":[{"id":"1","size":{"width":3840,"height":2160},"refreshRate":60.0}],
             "pos":{"x":0,"y":0},"scale":1.0,"rotation":1,"hdr":false,"wcg":false,"priority":1}]})
    }
    fn saved_layout() -> Layout {
        Layout::capture(&json!({"outputs":[physical(true, 1)]}), "Virtual-9").unwrap()
    }

    /// Reports `settling` until `settles_after` applications are `settle_delay`
    /// old, then the saved desktop.
    struct ScriptedDesktop {
        settling: Value,
        settled: Value,
        settles_after: u32,
        settle_delay: Duration,
        applications: Vec<(Duration, Vec<String>)>,
        rejected_applications: u32,
        clock: Duration,
        queries: u32,
    }
    impl ScriptedDesktop {
        fn new(settling: Value, settles_after: u32, settle_delay: Duration) -> Self {
            Self {
                settling,
                settled: desktop_with(physical(true, 1)),
                settles_after,
                settle_delay,
                applications: Vec::new(),
                rejected_applications: 0,
                clock: Duration::ZERO,
                queries: 0,
            }
        }
    }
    impl Desktop for ScriptedDesktop {
        fn query(&mut self) -> Result<Value, String> {
            self.queries += 1;
            let settled = self
                .applications
                .get(self.settles_after.saturating_sub(1) as usize)
                .is_some_and(|(applied, _)| {
                    self.clock.saturating_sub(*applied) >= self.settle_delay
                });
            Ok(if settled && self.settles_after > 0 {
                self.settled.clone()
            } else {
                self.settling.clone()
            })
        }
        fn apply(&mut self, arguments: &[String]) -> Result<(), String> {
            if self.rejected_applications > 0 {
                self.rejected_applications -= 1;
                return Err("KScreen command failed".into());
            }
            self.applications.push((self.clock, arguments.to_vec()));
            Ok(())
        }
        fn elapsed(&self) -> Duration {
            self.clock
        }
        fn pause(&mut self, duration: Duration) {
            self.clock += duration;
        }
    }

    #[test]
    fn matching_desktop_is_left_untouched() {
        let mut desktop = ScriptedDesktop::new(desktop_with(physical(true, 1)), 0, Duration::ZERO);
        let settled = restore_layout(&mut desktop, &saved_layout()).unwrap();
        assert_eq!(settled.applications, 0);
        assert_eq!(settled.differing_readbacks, 0);
        assert!(desktop.applications.is_empty());
        assert_eq!(desktop.queries, 1);
    }

    #[test]
    fn output_that_reports_its_state_late_is_awaited_without_a_second_command() {
        // The installed host read the desktop once, straight after the command,
        // and treated this still-settling report as a failed restoration.
        let mut desktop = ScriptedDesktop::new(
            desktop_with(physical(false, 0)),
            1,
            Duration::from_millis(300),
        );
        let settled = restore_layout(&mut desktop, &saved_layout()).unwrap();
        assert_eq!(settled.applications, 1);
        assert_eq!(desktop.applications.len(), 1);
        assert!(
            desktop.applications[0]
                .1
                .contains(&"output.HDMI-A-1.enable".to_owned())
        );
        assert!(settled.waited >= Duration::from_millis(300));
        assert!(settled.waited < REAPPLY_AFTER);
        assert_eq!(settled.differing_readbacks, 5);
        assert!(
            settled
                .last_difference
                .unwrap()
                .contains("enabled expected true observed false")
        );
    }

    #[test]
    fn difference_that_persists_is_applied_again() {
        // Enabled but ranked behind the stream output: only a second command
        // that sees the output enabled moves it back to the front.
        let mut desktop = ScriptedDesktop::new(desktop_with(physical(true, 2)), 2, Duration::ZERO);
        let settled = restore_layout(&mut desktop, &saved_layout()).unwrap();
        assert_eq!(settled.applications, 2);
        assert!(desktop.applications[1].0 >= REAPPLY_AFTER);
        assert!(
            desktop.applications[1]
                .1
                .contains(&"output.HDMI-A-1.priority.1".to_owned())
        );
    }

    #[test]
    fn rejected_command_is_retried_within_the_deadline() {
        let mut desktop = ScriptedDesktop::new(desktop_with(physical(false, 0)), 1, Duration::ZERO);
        desktop.rejected_applications = 1;
        let settled = restore_layout(&mut desktop, &saved_layout()).unwrap();
        assert_eq!(settled.applications, 2);
        assert_eq!(desktop.applications.len(), 1);
    }

    #[test]
    fn lasting_difference_fails_with_the_property_that_differs() {
        let mut desktop =
            ScriptedDesktop::new(desktop_with(physical(true, 2)), u32::MAX, Duration::ZERO);
        let error = restore_layout(&mut desktop, &saved_layout()).unwrap_err();
        assert!(
            error.contains("HDMI-A-1: priority expected 1 observed 2"),
            "{error}"
        );
        assert_eq!(desktop.applications.len(), MAX_APPLICATIONS as usize);
        assert!(desktop.clock >= SETTLE_DEADLINE);
        assert!(desktop.clock < SETTLE_DEADLINE + Duration::from_millis(200));
    }

    #[test]
    fn unreadable_desktop_fails_immediately() {
        struct Unreadable;
        impl Desktop for Unreadable {
            fn query(&mut self) -> Result<Value, String> {
                Err("invalid KScreen JSON".into())
            }
            fn apply(&mut self, _: &[String]) -> Result<(), String> {
                unreachable!("nothing can be applied to an unreadable desktop")
            }
            fn elapsed(&self) -> Duration {
                Duration::ZERO
            }
            fn pause(&mut self, _: Duration) {}
        }
        assert_eq!(
            restore_layout(&mut Unreadable, &saved_layout()).unwrap_err(),
            "invalid KScreen JSON"
        );
    }
}
