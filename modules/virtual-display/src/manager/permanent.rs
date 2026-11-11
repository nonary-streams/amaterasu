//! One bounded application of trusted permanent-display startup policy.
use crate::backend::Driver;
use crate::model::Settings;

pub(super) struct RuntimeState {
    pub supported: bool,
    pub state: &'static str,
    pub error: Option<&'static str>,
    pub observed_count: Option<u32>,
}

pub(super) fn synchronize(settings: &Settings, driver: &mut impl Driver) -> RuntimeState {
    let mut result = RuntimeState {
        supported: driver.permanent_count_supported() && driver.supported(),
        state: "unsupported",
        error: None,
        observed_count: None,
    };
    if !result.supported {
        return result;
    }
    if !settings.enabled {
        result.state = "disabled";
        return result;
    }
    if !settings.dd_use_sunshine_virtual_display_driver {
        result.state = "unsupported_driver";
        return result;
    }
    let Some(requested) = settings.dd_virtual_display_permanent_count else {
        result.state = "not_configured";
        return result;
    };
    result.state = "failed";
    match driver.query_permanent_count() {
        Ok(count) => result.observed_count = Some(count),
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    }
    if result.observed_count == Some(u32::from(requested)) {
        result.state = "applied";
        return result;
    }
    let mutation = driver.set_permanent_count(requested);
    // An acknowledgement can be lost after the driver changes the count. One
    // independent readback settles the runtime state without replaying effects.
    match driver.query_permanent_count() {
        Ok(count) => {
            result.observed_count = Some(count);
            if count == u32::from(requested) {
                result.state = "applied";
                result.error = mutation.err();
            } else {
                result.error = Some(mutation.err().unwrap_or("permanent_count_mismatch"));
            }
        }
        Err(error) => {
            result.state = "unverified";
            result.observed_count = None;
            result.error = Some(error);
        }
    }
    result
}
