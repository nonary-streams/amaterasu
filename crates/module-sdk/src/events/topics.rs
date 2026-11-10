//! The topics the built-in modules publish, with their payloads.
//!
//! Each event says that something changed and carries a revision. It carries
//! no state: read that from the owning service, with the grants that service
//! requires.
use super::Topic;
use serde::{Deserialize, Serialize};

/// The session coordinator committed a change. Read sessions from
/// `amaterasu.session/1`.
pub const SESSION_CHANGED: Topic<SessionChanged> = Topic::new("amaterasu.session.changed/1");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionChanged {
    /// The coordinator's revision after the change.
    pub revision: u64,
}

/// Stored configuration changed for one owner. The event names no schema and
/// carries no value. Read values from `amaterasu.configuration/1`.
pub const CONFIGURATION_CHANGED: Topic<ConfigurationChanged> =
    Topic::new("amaterasu.configuration.changed/1");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigurationChanged {
    /// The principal, or the product settings owner, whose values changed.
    pub owner: String,
    pub revision: u64,
    /// Absent for a principal's own configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ConfigurationKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationKind {
    /// Product settings, which are stored per owner rather than per caller.
    ProductSettings,
    /// A kind this SDK version does not name.
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payloads_keep_the_wire_shape_existing_subscribers_read() {
        assert_eq!(
            serde_json::to_value(SessionChanged { revision: 4 }).unwrap(),
            json!({"revision": 4})
        );
        let own = ConfigurationChanged {
            owner: "example.plugin".into(),
            revision: 2,
            kind: None,
        };
        assert_eq!(
            serde_json::to_value(&own).unwrap(),
            json!({"owner": "example.plugin", "revision": 2})
        );
        let product = ConfigurationChanged {
            kind: Some(ConfigurationKind::ProductSettings),
            ..own
        };
        assert_eq!(
            serde_json::to_value(&product).unwrap(),
            json!({"owner": "example.plugin", "revision": 2, "kind": "product_settings"})
        );
    }

    #[test]
    fn a_kind_added_later_still_decodes() {
        let event: ConfigurationChanged = serde_json::from_value(
            json!({"owner": "example.plugin", "revision": 2, "kind": "future_kind"}),
        )
        .unwrap();
        assert_eq!(event.kind, Some(ConfigurationKind::Other));
    }
}
