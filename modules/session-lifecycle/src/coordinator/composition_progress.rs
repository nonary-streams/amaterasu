//! Durable sequencing state for composition, not an authorization decision.
//!
//! The coordinator must validate every frozen selection and native observation,
//! and persist each update with the authority journal before dispatch. Version 2
//! remains disabled until the scheduler and owner verification are complete.
use amaterasu_extension_api::{ResourceControlResponse, ResourceFinalizeRequest};
use serde::{Deserialize, Serialize};

use super::composition::{CompositionSelectionSnapshot, StoredOutput};
use crate::prepare_driver::PrepareState;
use crate::provider_dispatch::DispatchRecord;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompositionProgress {
    /// Frozen coordinator-selected inputs and installed approvals before planning.
    pub inputs: Vec<StoredOutput>,
    pub selections: Vec<CompositionSelectionSnapshot>,
    /// Every control request must have its own durable permit and terminal proof.
    pub controls: Vec<super::control_dispatch::ControlDispatchState>,
    /// Distinct fresh owner plan and rights, never a settled prepare admission.
    pub finalize: Option<FinalizeProgress>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinalizeProgress {
    /// The request and admitted plan must be derived from fresh owner planning.
    pub owner_plan: PrepareState,
    pub request: Option<ResourceFinalizeRequest>,
    pub dispatch: Option<DispatchRecord>,
    pub pending_receipt: Option<String>,
    pub status_polls: u32,
    pub result: Option<ResourceControlResponse>,
}
