//! Serialized participant outputs and owner-mediated control contracts.
//!
//! Payloads and identifiers are descriptive data, never authority. They must not
//! contain native handles; owners verify coordinator admission and permits before
//! executing a control request.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CaptureBinding, LifecycleTarget, MethodRef, ResourceAccess, ResourceRef};

/// Identifies an approved, versioned schema and its exact content digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRef {
    pub id: String,
    pub major: u32,
    pub minor: u32,
    pub digest: String,
}

/// A producer's serialized observation, bound to its lifecycle and resource.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOutput {
    pub output_id: String,
    pub capability: String,
    pub schema: SchemaRef,
    pub producer: String,
    pub producer_service: String,
    pub origin: LifecycleTarget,
    pub resource: ResourceRef,
    pub data: Value,
}

/// A proposed owner-mediated operation; admission and a permit are required.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceControlProposal {
    pub proposal_id: String,
    pub output_id: String,
    pub owner_provider: String,
    pub owner_service: String,
    pub target: LifecycleTarget,
    pub resource: ResourceRef,
    pub method: MethodRef,
    pub argument_schema: SchemaRef,
    pub arguments: Value,
    pub access: ResourceAccess,
    pub mutation_domains: Vec<String>,
}

/// A control result confirmed by the coordinator for its intended consumer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedControl {
    pub proposal_id: String,
    pub output: ResourceOutput,
    pub candidate: Option<CaptureBinding>,
}

/// Coordinator request to the resource owner, with a separately verified permit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceControlRequest {
    pub version: u32,
    pub transition_id: String,
    pub operation_id: String,
    pub provider_sequence: u64,
    pub target: LifecycleTarget,
    pub proposal: ResourceControlProposal,
    pub admission_id: String,
    pub permit_id: String,
    pub remaining_ms: u64,
}

/// Owner's result for an admitted control operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceControlResponse {
    Applied {
        output: Box<ResourceOutput>,
        candidate: Option<Box<CaptureBinding>>,
    },
    Pending {
        receipt_id: String,
    },
    Rejected {
        reason: String,
    },
    RecoveryRequired {
        reason: String,
    },
}
