//! A short generic polling interval keeps asynchronous module stages responsive.
//! Product libraries still own their own frame, input and resource deadlines.
pub(crate) const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);

/// Charge module work to the polling interval. Sleeping for a whole interval
/// after every pass makes completion delivery wait for work plus the interval.
pub(crate) fn remaining_tick_interval(elapsed: std::time::Duration) -> std::time::Duration {
    TICK_INTERVAL.saturating_sub(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn module_work_consumes_the_polling_budget() {
        assert_eq!(remaining_tick_interval(Duration::ZERO), TICK_INTERVAL);
        assert_eq!(
            remaining_tick_interval(Duration::from_micros(1_500)),
            Duration::from_micros(500)
        );
    }

    #[test]
    fn an_overdue_pass_does_not_add_another_completion_delay() {
        assert_eq!(remaining_tick_interval(TICK_INTERVAL), Duration::ZERO);
        assert_eq!(
            remaining_tick_interval(Duration::from_millis(20)),
            Duration::ZERO
        );
    }
}
