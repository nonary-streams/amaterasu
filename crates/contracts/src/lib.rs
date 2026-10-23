//! Version 1 control-plane messages. These types are independent of the bootstrap ABI.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    #[serde(default)]
    pub native_provide: BTreeSet<String>,
    #[serde(default)]
    pub native_use: BTreeSet<String>,
    #[serde(default)]
    pub provide: BTreeSet<String>,
    #[serde(default)]
    pub call: BTreeSet<String>,
    #[serde(default)]
    pub publish: BTreeSet<String>,
    #[serde(default)]
    pub subscribe: BTreeSet<String>,
    #[serde(default)]
    pub hook: BTreeSet<String>,
}
impl Grants {
    pub fn intersect(&self, policy: &Self) -> Self {
        fn both(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
            a.intersection(b).cloned().collect()
        }
        Self {
            native_provide: both(&self.native_provide, &policy.native_provide),
            native_use: both(&self.native_use, &policy.native_use),
            provide: both(&self.provide, &policy.provide),
            call: both(&self.call, &policy.call),
            publish: both(&self.publish, &policy.publish),
            subscribe: both(&self.subscribe, &policy.subscribe),
            hook: both(&self.hook, &policy.hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookSpec {
    pub owner: String,
    pub participants: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub principals: BTreeMap<String, Grants>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_methods: Vec<ApiMethodApproval>,
    #[serde(default)]
    pub hooks: BTreeMap<String, HookSpec>,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    /// Host-owned runtime facts are never serialized into a signed package
    /// policy. The host injects this context only after the selected package
    /// has been verified.
    #[serde(default)]
    pub host_context: RuntimeHostContext,
}

/// A wire contract can describe arbitrary namespaced methods. `Any` and
/// `Object` keep nested payloads extensible; the broker still bounds byte size.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFieldKind {
    Any,
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSchema {
    pub required: BTreeMap<String, ApiFieldKind>,
    #[serde(default)]
    pub optional: BTreeMap<String, ApiFieldKind>,
    #[serde(default)]
    pub allow_extra: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiPrivilege {
    User,
    Elevated,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiMethod {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub request: ApiSchema,
    pub response: ApiSchema,
    pub privilege: ApiPrivilege,
}

/// Installed policy, not a publisher's manifest, controls provider identity,
/// callable identities, and whether a handler is approved at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiMethodApproval {
    pub provider: String,
    pub callers: BTreeSet<String>,
    pub method: ApiMethod,
}

/// Facts supplied by the trusted host for one worker process.
///
/// `state_root` is intentionally separate from module settings: a release
/// policy may choose modules and grants, but it must not choose where a
/// user-token process persists state.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHostContext {
    pub schema: u32,
    #[serde(default)]
    pub state_root: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    NativeRegister {
        service: String,
        factory: usize,
    },
    NativeBind {
        service: String,
        interface_major: u32,
        min_minor: u32,
        min_size: u32,
    },
    NativeRelease {
        binding: u64,
    },
    GetSettings {},
    /// Returns host-owned context to the state service, and to the Linux audio
    /// module for its restart-safe cleanup journal. This prevents a module
    /// policy from impersonating a host decision through `GetSettings`.
    GetHostContext {},
    Register {
        service: String,
    },
    RegisterApi {
        method: ApiMethod,
    },
    QueryApi {
        service: String,
        method: String,
        interface_major: u32,
        min_minor: u32,
    },
    CallApi {
        service: String,
        method: String,
        interface_major: u32,
        interface_minor: u32,
        payload: Value,
        timeout_ms: u64,
        operation_id: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    Request {
        service: String,
        payload: Value,
        timeout_ms: u64,
        operation_id: String,
        #[serde(default)]
        parent: Option<u64>,
    },
    Poll {},
    Complete {
        request: u64,
        payload: Value,
    },
    CheckRequest {
        request: u64,
    },
    TakeResult {
        request: u64,
    },
    ReleaseResult {
        request: u64,
    },
    Cancel {
        request: u64,
    },
    Subscribe {
        topic: String,
        capacity: usize,
        #[serde(default)]
        coalesce: bool,
    },
    Publish {
        topic: String,
        payload: Value,
    },
    NextEvent {
        subscription: u64,
    },
    Unsubscribe {
        subscription: u64,
    },
    RegisterHook {
        hook: String,
    },
    RunHook {
        hook: String,
        payload: Value,
    },
    HookResult {
        invocation: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    NativeBound {
        binding: u64,
        address: usize,
    },
    Ok,
    Error {
        code: String,
        message: String,
    },
    Accepted {
        id: u64,
    },
    Pending,
    Result {
        payload: Value,
    },
    Request {
        id: u64,
        caller: String,
        initiator: String,
        service: String,
        payload: Value,
        remaining_ms: u64,
    },
    Event {
        publisher: String,
        topic: String,
        sequence: u64,
        payload: Value,
    },
    ResyncRequired {
        topic: String,
    },
    HookResult {
        proposals: Vec<Proposal>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub participant: String,
    pub payload: Value,
}

impl Response {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error { .. })
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b'/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn older_configurations_default_to_no_settings() {
        let configuration: RuntimeConfig =
            serde_json::from_value(json!({"principals": {}})).unwrap();
        assert!(configuration.settings.is_empty());
    }

    #[test]
    fn native_grants_default_empty_and_intersect_independently() {
        let old: Grants = serde_json::from_value(json!({"call":["frames/1"]})).unwrap();
        assert!(old.native_use.is_empty());
        assert!(old.native_provide.is_empty());
        let requested = Grants {
            native_use: ["frames/1".into()].into(),
            native_provide: ["frames/1".into()].into(),
            ..Default::default()
        };
        let policy = Grants {
            native_use: ["frames/1".into()].into(),
            call: ["frames/1".into()].into(),
            ..Default::default()
        };
        let effective = requested.intersect(&policy);
        assert_eq!(effective.native_use, requested.native_use);
        assert!(effective.native_provide.is_empty());
        assert!(effective.call.is_empty());
    }

    #[test]
    fn control_envelopes_reject_duplicate_and_unknown_fields() {
        for envelope in [
            r#"{"op":"get_settings","caller":"other"}"#,
            r#"{"op":"native_bind","service":"frames/1","interface_major":1,"min_minor":0,"min_size":32,"caller":"other"}"#,
            r#"{"op":"poll","op":"get_settings"}"#,
            r#"{"op":"cancel","request":1,"request":2}"#,
            r#"{"op":"request","service":"work/1","payload":null,"timeout_ms":1,"operation_id":"one","authority":{}}"#,
        ] {
            assert!(
                serde_json::from_str::<Command>(envelope).is_err(),
                "{envelope}"
            );
        }
        assert!(
            serde_json::from_value::<RuntimeConfig>(json!({
                "principals": {}, "settings": {}, "unknown": true,
            }))
            .is_err()
        );
    }
}
