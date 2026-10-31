//! Serialized extension contracts. References select resources; they never confer authority.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
pub const EXTENSION_REGISTRY_SERVICE: &str = "amaterasu.extension-registry/1";
pub const EXTENSION_ACTIONS_SERVICE: &str = "amaterasu.extension-actions/1";
pub const CONTRACT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRef {
    pub provider: String,
    pub resource_id: String,
    pub incarnation: String,
    pub revision: u64,
    pub execution_domain: String,
    pub mutation_domain: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBinding {
    pub capture_provider: String,
    pub selector_schema: String,
    pub selector: Value,
    pub resource: ResourceRef,
    pub mode: VideoMode,
    pub plan_revision: u64,
    #[serde(default)]
    pub fallback: CaptureFallback,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFallback {
    #[default]
    Forbidden,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LifecycleTarget {
    Session {
        host_epoch: String,
        session_id: String,
        generation: u64,
    },
    Stream {
        host_epoch: String,
        session_id: String,
        stream_id: String,
        generation: u64,
    },
    Validation {
        host_epoch: String,
        validation_id: String,
        generation: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionTarget {
    Session {
        host_epoch: String,
        session_id: String,
        expected_generation: u64,
    },
    Stream {
        host_epoch: String,
        session_id: String,
        stream_id: String,
        expected_generation: u64,
    },
    Host {
        host_epoch: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInvoke {
    pub epoch: String,
    pub sequence: u64,
    pub operation_id: String,
    pub action_id: String,
    pub target: ActionTarget,
    pub arguments: Value,
    pub expected_settings_revision: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Queued,
    Running,
    Succeeded,
    Rejected,
    Cancelled,
    Failed,
    RecoveryRequired,
    ReceiptExpired,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReceipt {
    pub receipt_id: String,
    pub operation_id: String,
    pub status: ActionStatus,
    pub result: Option<Value>,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    Host,
    Application,
    PairedClient,
    Session,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPolicy {
    NextSession,
    NextStream,
    LiveAction,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    ReadOnly,
    SessionMutation,
    HostMutation,
}
