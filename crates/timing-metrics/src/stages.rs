//! Thread-owned storage substage timing. Fixed identities and bounded storage.
use super::*;
use std::{cell::RefCell, collections::BTreeMap};

thread_local! {
    static STAGES: RefCell<BTreeMap<&'static str, LatencyMetric>> = const { RefCell::new(BTreeMap::new()) };
}
#[derive(Serialize)]
pub struct StageObservation {
    pub stage: &'static str,
    pub timing_us: LatencyMetric,
}
pub fn measure_stage<T>(stage: &'static str, operation: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let result = operation();
    STAGES.with(|stages| {
        let mut stages = stages.borrow_mut();
        let key = if stages.len() < 31 || stages.contains_key(stage) {
            stage
        } else {
            "other"
        };
        stages.entry(key).or_default().record(elapsed_us(started));
    });
    result
}
pub fn take_stages() -> Vec<StageObservation> {
    STAGES.with(|stages| {
        std::mem::take(&mut *stages.borrow_mut())
            .into_iter()
            .map(|(stage, timing_us)| StageObservation { stage, timing_us })
            .collect()
    })
}
