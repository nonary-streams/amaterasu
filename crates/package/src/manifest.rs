//! Metadata persisted at the package and host boundaries.
use amaterasu_contracts::{Grants, RuntimeConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentManifest {
    pub id: String,
    pub version: String,
    pub file: String,
    pub sha256: String,
    pub size: u64,
    pub abi_major: u32,
    pub runtime: bool,
    #[serde(default)]
    pub requires: BTreeMap<String, String>,
    pub requested: Grants,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: u32,
    pub project: String,
    pub sequence: u64,
    pub expires_unix: u64,
    pub target: String,
    pub components: Vec<ComponentManifest>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseSignature {
    pub key_id: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapConfig {
    /// Development host only; no claim of installed service/admin ACL enforcement yet.
    pub development: bool,
    #[serde(default)]
    pub allow_unsigned: bool,
    pub trusted_keys: BTreeMap<String, String>,
    pub policy: RuntimeConfig,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationState {
    pub active: Option<String>,
    pub previous: Option<String>,
    pub pending: Option<String>,
    #[serde(default)]
    pub trial: Option<String>,
    pub highest_sequence: u64,
}
pub struct VerifiedPackage {
    pub manifest: ReleaseManifest,
    pub digest: String,
    pub signed: bool,
    pub runtime_config: RuntimeConfig,
}
