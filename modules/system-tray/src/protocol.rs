use crate::settings::{ActivityUpdate, TraySettings, UpdateNotice};
use serde::Deserialize;

pub const SERVICE: &str = "amaterasu.system-tray/1";
pub const MANAGEMENT_CALLER: &str = "amaterasu.management";
pub const UPDATER_CALLER: &str = "amaterasu.updater";

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    SettingsGet {},
    SettingsUpdate {
        settings: TraySettings,
        expected_revision: u64,
    },
    EventNext {},
    NotifyUpdate {
        version: String,
        release_url: String,
    },
    ActivityUpdate {
        update: ActivityUpdate,
    },
    NotifyPairing {},
    NotifyVigemMissing {},
}

impl Operation {
    pub fn authorized_for(&self, caller: &str) -> bool {
        match self {
            Self::SettingsGet {} | Self::SettingsUpdate { .. } | Self::EventNext {} => {
                caller == MANAGEMENT_CALLER
            }
            Self::NotifyUpdate { .. } => caller == UPDATER_CALLER,
            Self::ActivityUpdate { .. } | Self::NotifyVigemMissing {} => {
                caller == "amaterasu.stream-session"
            }
            Self::NotifyPairing {} => caller == "amaterasu.pairing",
        }
    }

    pub fn notification(self) -> Result<UpdateNotice, &'static str> {
        match self {
            Self::NotifyUpdate {
                version,
                release_url,
            } => UpdateNotice::parse(version, release_url),
            _ => Err("not_a_notification"),
        }
    }

    pub fn activity(self) -> Result<ActivityUpdate, &'static str> {
        match self {
            Self::ActivityUpdate { update } => {
                update.validate()?;
                Ok(update)
            }
            _ => Err("not_an_activity_update"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn operations_have_strict_shapes_and_explicit_callers() {
        let get: Operation = serde_json::from_value(json!({"action":"settings_get"})).unwrap();
        assert!(get.authorized_for(MANAGEMENT_CALLER));
        assert!(!get.authorized_for(UPDATER_CALLER));

        let update: Operation = serde_json::from_value(json!({
            "action":"settings_update",
            "settings":{"system_tray":false},
            "expected_revision":8
        }))
        .unwrap();
        assert!(update.authorized_for(MANAGEMENT_CALLER));
        assert!(!update.authorized_for(UPDATER_CALLER));

        let notify: Operation = serde_json::from_value(json!({
            "action":"notify_update",
            "version":"v2.0.0",
            "release_url":"https://github.com/Nonary/amaterasu/releases/tag/v2.0.0"
        }))
        .unwrap();
        assert!(notify.authorized_for(UPDATER_CALLER));
        assert!(!notify.authorized_for(MANAGEMENT_CALLER));

        let activity: Operation = serde_json::from_value(json!({
            "action":"activity_update",
            "update":{"state":"playing","app_name":"Game"}
        }))
        .unwrap();
        assert!(activity.authorized_for("amaterasu.stream-session"));
        assert!(!activity.authorized_for("amaterasu.management"));
        assert_eq!(activity.activity().unwrap().app_name, "Game");

        let event_next: Operation = serde_json::from_value(json!({"action":"event_next"})).unwrap();
        assert!(event_next.authorized_for(MANAGEMENT_CALLER));

        let pairing: Operation =
            serde_json::from_value(json!({"action":"notify_pairing"})).unwrap();
        assert!(pairing.authorized_for("amaterasu.pairing"));

        for malformed in [
            json!({"action":"settings_get","unexpected":true}),
            json!({"action":"settings_update","settings":{"system_tray":true}}),
            json!({"action":"notify_update","version":"v2.0.0"}),
        ] {
            assert!(serde_json::from_value::<Operation>(malformed).is_err());
        }

        let invalid_activity: Operation = serde_json::from_value(json!({
            "action":"activity_update",
            "update":{"state":"playing","app_name":""}
        }))
        .unwrap();
        assert_eq!(invalid_activity.activity().unwrap_err(), "invalid_app_name");
    }
}
