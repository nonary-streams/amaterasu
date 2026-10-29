//! Metadata persisted at the package and host boundaries.
use amaterasu_contracts::{Grants, RuntimeConfig};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    /// Version of the host-to-module context contract required by this set.
    /// Missing in legacy manifests, which production hosts reject rather than
    /// letting a module select a process state path from policy settings.
    #[serde(default)]
    pub host_context_schema: u32,
    pub project: String,
    pub sequence: u64,
    pub expires_unix: u64,
    pub target: String,
    pub components: Vec<ComponentManifest>,
    /// Requests only; machine-protected approval is required separately.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub privileged_requests: Vec<crate::PrivilegedRequest>,
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
    /// Development repository sets retain their explicit signer provenance.
    #[serde(default)]
    pub repository_sets: BTreeMap<String, RepositorySet>,
    /// Highest signed native release accepted from each pinned publisher key.
    #[serde(default)]
    pub repository_floors: BTreeMap<String, NativeReleaseFloor>,
    /// Sets successfully promoted after a disposable worker health trial.
    #[serde(default)]
    pub healthy_sets: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositorySet {
    pub repository_id: String,
    pub key_fingerprint: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReleaseFloor {
    pub sequence: u64,
    pub digest: String,
}
pub struct VerifiedPackage {
    pub manifest: ReleaseManifest,
    pub digest: String,
    pub signed: bool,
    /// SHA-256 of the actual verified Ed25519 key bytes, never a display key ID.
    pub signer_fingerprint: Option<String>,
    pub privileged_requests: Vec<crate::VerifiedPrivilegedRequest>,
    pub runtime_config: RuntimeConfig,
}
