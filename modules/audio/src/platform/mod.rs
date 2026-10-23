#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Capture, enumerate, route_restore_pending};
#[cfg(windows)]
pub use windows::{shutdown, startup};

#[derive(Clone, Debug)]
pub struct RecoveryConfig {
    pub endpoint: String,
    pub follow_console_default: bool,
    pub virtual_channels: Option<u32>,
    pub capture_format: Option<crate::model::PcmCaptureFormat>,
    pub preferred_valid_bits: Option<u16>,
    pub capture_sink_without_routing: bool,
    pub explicit_route: bool,
}

pub(super) fn should_resolve_console_default(requested: &str, uses_virtual_sink: bool) -> bool {
    !uses_virtual_sink && (requested.is_empty() || requested == "default")
}

pub(super) fn should_follow_console_default(
    requested: &str,
    uses_virtual_sink: bool,
    capture_sink_without_routing: bool,
) -> bool {
    should_resolve_console_default(requested, uses_virtual_sink) && !capture_sink_without_routing
}

#[cfg(test)]
mod tests {
    use super::{should_follow_console_default, should_resolve_console_default};

    #[test]
    fn default_aliases_resolve_console_only_without_a_virtual_sink() {
        assert!(should_resolve_console_default("", false));
        assert!(should_resolve_console_default("default", false));
        assert!(!should_resolve_console_default("render.explicit", false));
        assert!(!should_resolve_console_default("default", true));
    }

    #[test]
    fn capture_only_pins_the_initial_console_default_without_disabling_its_selection() {
        for requested in ["", "default"] {
            assert!(should_resolve_console_default(requested, false));
            assert!(should_follow_console_default(requested, false, false));
            assert!(!should_follow_console_default(requested, false, true));
        }

        assert!(!should_follow_console_default(
            "render.explicit",
            false,
            true
        ));
        assert!(!should_follow_console_default("default", true, true));
    }
}

#[cfg(not(windows))]
mod unsupported {
    use super::RecoveryConfig;
    use crate::model::{AudioResult, Endpoint, Format, Packet};
    pub fn enumerate() -> AudioResult<Vec<Endpoint>> {
        Ok(Vec::new())
    }

    pub fn route_restore_pending() -> bool {
        false
    }

    pub struct Capture;
    impl Capture {
        pub fn open(
            _: &str,
            _: Option<&str>,
            _: Option<u32>,
            _: Option<crate::model::PcmCaptureFormat>,
            _: bool,
            _: bool,
        ) -> AudioResult<Self> {
            Err("unsupported_platform")
        }
        pub fn reopen(_: &RecoveryConfig) -> AudioResult<Self> {
            Err("unsupported_platform")
        }
        pub fn recovery_config(&self) -> RecoveryConfig {
            unreachable!("unsupported capture never opens")
        }
        pub fn format(&self) -> Format {
            unreachable!("unsupported capture never opens")
        }
        pub fn endpoint(&self) -> &str {
            unreachable!("unsupported capture never opens")
        }
        pub fn read(&mut self) -> AudioResult<Option<Packet>> {
            Err("unsupported_platform")
        }
    }
}

#[cfg(not(windows))]
pub use unsupported::{Capture, enumerate, route_restore_pending};

#[cfg(not(windows))]
pub fn shutdown() {}

#[cfg(not(windows))]
pub fn startup() {}
