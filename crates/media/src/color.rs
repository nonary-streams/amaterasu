use serde::{Deserialize, Serialize};

/// SDR Y'CbCr interpretation shared by stream configuration and encoders.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[repr(u32)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    Bt601Limited = 0,
    Bt601Full = 1,
    #[default]
    Bt709Limited = 2,
    Bt709Full = 3,
}

impl ColorMode {
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Bt709Limited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names_and_default_are_stable() {
        assert_eq!(ColorMode::default(), ColorMode::Bt709Limited);
        assert_eq!(
            serde_json::to_string(&ColorMode::Bt601Limited).unwrap(),
            "\"bt601_limited\""
        );
        assert_eq!(
            serde_json::to_string(&ColorMode::Bt601Full).unwrap(),
            "\"bt601_full\""
        );
        assert_eq!(
            serde_json::to_string(&ColorMode::Bt709Limited).unwrap(),
            "\"bt709_limited\""
        );
        assert_eq!(
            serde_json::to_string(&ColorMode::Bt709Full).unwrap(),
            "\"bt709_full\""
        );
        assert_eq!(
            serde_json::from_str::<ColorMode>("\"bt709_full\"").unwrap(),
            ColorMode::Bt709Full
        );
    }
}
