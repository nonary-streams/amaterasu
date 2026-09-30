#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
pub use linux::{Capture, enumerate, route_restore_pending};
#[cfg(target_os = "linux")]
pub use linux::{shutdown, startup};
#[cfg(windows)]
pub use windows::{Capture, enumerate, route_restore_pending};
#[cfg(windows)]
pub use windows::{configure_steam_driver_install, steam_driver_install_status};
#[cfg(windows)]
pub use windows::{shutdown, startup};

pub fn virtual_sink_cleanup_pending() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::virtual_sink_cleanup_pending()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(
    not(windows),
    expect(
        dead_code,
        reason = "device recovery options are consumed by the Windows capture backend"
    )
)]
pub struct RecoveryConfig {
    pub endpoint: String,
    pub follow_console_default: bool,
    pub virtual_channels: Option<u32>,
    pub preferred_valid_bits: Option<u16>,
    pub capture_sink_without_routing: bool,
    pub explicit_route: bool,
    #[cfg(target_os = "linux")]
    route_lease: Option<std::sync::Arc<linux::RouteLease>>,
    #[cfg(target_os = "linux")]
    virtual_sink_lease: Option<std::sync::Arc<linux::ManagedVirtualSink>>,
    #[cfg(target_os = "linux")]
    channel_positions: Vec<i32>,
}

#[cfg(any(windows, test))]
pub(super) fn should_resolve_console_default(requested: &str, uses_virtual_sink: bool) -> bool {
    !uses_virtual_sink && (requested.is_empty() || requested == "default")
}

#[cfg(any(windows, test))]
pub(super) fn should_follow_console_default(
    requested: &str,
    uses_virtual_sink: bool,
    capture_sink_without_routing: bool,
) -> bool {
    should_resolve_console_default(requested, uses_virtual_sink) && !capture_sink_without_routing
}

#[cfg(not(any(windows, target_os = "linux")))]
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

#[cfg(not(any(windows, target_os = "linux")))]
pub use unsupported::{Capture, enumerate, route_restore_pending};

#[cfg(not(any(windows, target_os = "linux")))]
pub fn shutdown() {}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn startup() {}

#[cfg(not(windows))]
static UNSUPPORTED_STEAM_DRIVER_SETTINGS: std::sync::OnceLock<
    std::sync::Mutex<(bool, Option<u64>)>,
> = std::sync::OnceLock::new();

#[cfg(not(windows))]
pub fn configure_steam_driver_install(enabled: bool, revision: u64) -> serde_json::Value {
    let settings =
        UNSUPPORTED_STEAM_DRIVER_SETTINGS.get_or_init(|| std::sync::Mutex::new((true, None)));
    let mut settings = settings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *settings = (enabled, Some(revision));
    unsupported_steam_driver_status(settings.0, settings.1)
}

#[cfg(not(windows))]
pub fn steam_driver_install_status() -> serde_json::Value {
    let settings =
        UNSUPPORTED_STEAM_DRIVER_SETTINGS.get_or_init(|| std::sync::Mutex::new((true, None)));
    let settings = settings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsupported_steam_driver_status(settings.0, settings.1)
}

#[cfg(not(windows))]
fn unsupported_steam_driver_status(enabled: bool, revision: Option<u64>) -> serde_json::Value {
    serde_json::json!({
        "enabled": enabled,
        "revision": revision,
        "state": "unsupported_platform",
        "error_code": null,
        "native_error": null,
        "endpoint": null,
        "restored_roles": [],
        "restore_failures": [],
    })
}

#[cfg(all(test, not(windows)))]
mod steam_driver_status_tests {
    #[test]
    fn unsupported_platform_status_keeps_the_global_configuration_readable() {
        let status = super::configure_steam_driver_install(false, 17);
        assert_eq!(status["enabled"], false);
        assert_eq!(status["revision"], 17);
        assert_eq!(status["state"], "unsupported_platform");

        let status = super::steam_driver_install_status();
        assert_eq!(status["enabled"], false);
        assert_eq!(status["revision"], 17);
        assert_eq!(status["state"], "unsupported_platform");
    }
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
