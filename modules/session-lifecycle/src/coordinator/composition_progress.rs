//! Durable sequencing state for composition, not an authorization decision.
//!
//! The coordinator must validate every frozen selection and native observation,
//! and persist each update with the authority journal before dispatch. Version 2
//! remains disabled until the scheduler and owner verification are complete.
use serde::{Deserialize, Serialize};

use super::composition::{CompositionSelectionSnapshot, StoredOutput};
use crate::prepare_driver::PrepareState;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompositionProgress {
    /// Frozen coordinator-selected inputs and installed approvals before planning.
    pub inputs: Vec<StoredOutput>,
    pub selections: Vec<CompositionSelectionSnapshot>,
    /// Every control request must have its own durable permit and terminal proof.
    pub controls: Vec<ControlProgress>,
    /// Distinct fresh owner plan and rights, never a settled prepare admission.
    pub finalize: Option<FinalizeProgress>,
}

/// The provisional control result is insufficient until the owner confirms
/// settlement of this exact durable tuple. Neither field is authority alone.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlProgress {
    pub dispatch: super::control_dispatch::ControlDispatchState,
    pub settlement: Option<super::settlement_dispatch::SettlementState>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FinalizeProgress {
    /// A separately selected owner-only snapshot is valid with no controls.
    pub selection: super::finalize_selection::FinalizeSelectionSnapshot,
    /// The request and admitted plan must be derived from fresh owner planning.
    pub owner_plan: PrepareState,
    pub dispatch: Option<super::finalize_dispatch::FinalizeDispatchState>,
}
