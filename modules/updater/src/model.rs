use serde::{Deserialize, Serialize};

pub const DEFAULT_UPDATE_CHECK_INTERVAL_SECONDS: u32 = 86_400;
pub const MAX_UPDATE_CHECK_INTERVAL_SECONDS: u32 = i32::MAX as u32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub notify_pre_releases: bool,
    pub update_check_interval: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notify_pre_releases: false,
            update_check_interval: DEFAULT_UPDATE_CHECK_INTERVAL_SECONDS,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.update_check_interval > MAX_UPDATE_CHECK_INTERVAL_SECONDS {
            return Err("update_check_interval must be between 0 and 2147483647 seconds");
        }
        Ok(())
    }
}
