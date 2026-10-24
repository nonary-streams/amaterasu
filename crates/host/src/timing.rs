//! A short generic polling interval keeps asynchronous module stages responsive.
//! Product libraries still own their own frame, input and resource deadlines.
pub(crate) const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);
