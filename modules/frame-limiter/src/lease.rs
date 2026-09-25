//! Caller-owned RTSS leases and compare-before-restore lifecycle.
use crate::{
    FrameLimiterProvider, Platform, RtssFrameLimitType,
    service::{
        ApplyRequest, Operation, OperationResponse, Presence, ProviderAvailability, ResponseState,
        RestoreRequest, StatusRequest, build_response, evaluate_apply, handle_operation,
        handle_payload, parse_operation,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNAL_FORMAT: u32 = 1;
const JOURNAL_LIMIT_BYTES: u64 = 1024 * 1024;
static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProfileValues {
    pub frame_limit: i32,
    pub frame_limit_denominator: i32,
    pub sync_limiter: i32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderSnapshot {
    pub profile: ProfileValues,
    pub profile_exists: bool,
    pub limiter_disabled: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeasePhase {
    Pending,
    #[default]
    Active,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct LeaseRecord {
    pub lease_id: String,
    pub owner: String,
    pub session_id: String,
    pub request_fingerprint: String,
    pub rtss_install_path: String,
    pub executable_path: String,
    pub profile: String,
    pub before: ProviderSnapshot,
    pub applied: ProviderSnapshot,
    #[serde(default)]
    pub phase: LeasePhase,
}

struct LeaseApplyRequest<'a> {
    owner: &'a str,
    session_id: &'a str,
    executable_path: &'a str,
    rtss_install_path: &'a str,
    profile: &'a str,
    request_fingerprint: &'a str,
    desired_profile: ProfileValues,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedLeaseStatus {
    pub lease_id: String,
    pub session_id: String,
    pub executable_path: String,
    pub state: &'static str,
    pub diagnostic: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProviderError {
    Unavailable(&'static str),
    ReadFailed(&'static str),
    WriteFailed(&'static str),
    VerifyFailed(&'static str),
}

impl ProviderError {
    pub(crate) fn diagnostic(&self) -> &'static str {
        match self {
            Self::Unavailable(code)
            | Self::ReadFailed(code)
            | Self::WriteFailed(code)
            | Self::VerifyFailed(code) => code,
        }
    }
}

/// Provider operations are split by profile values and the global limiter bit
/// so the lease manager can preserve unrelated global RTSS flags.
pub(crate) trait FrameLimitProvider {
    fn select_installation(&mut self, path: &str) -> Result<String, ProviderError>;
    fn profile_exists(&mut self, profile: &str) -> Result<bool, ProviderError>;
    fn read_profile(&mut self, profile: &str) -> Result<ProfileValues, ProviderError>;
    fn write_profile_verified(
        &mut self,
        profile: &str,
        values: ProfileValues,
    ) -> Result<(), ProviderError>;
    fn delete_profile_verified(
        &mut self,
        profile: &str,
        expected: ProfileValues,
    ) -> Result<(), ProviderError>;
    fn limiter_disabled(&mut self) -> Result<bool, ProviderError>;
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format_version: u32,
    leases: Vec<LeaseRecord>,
    #[serde(default)]
    completed: Vec<CompletedLease>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            format_version: JOURNAL_FORMAT,
            leases: Vec::new(),
            completed: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct CompletedLease {
    lease_id: String,
    owner: String,
    session_id: String,
}

pub(crate) struct LeaseManager<P> {
    provider: P,
    leases: BTreeMap<String, LeaseRecord>,
    completed: BTreeMap<String, CompletedLease>,
    journal_path: Option<PathBuf>,
}

impl<P: FrameLimitProvider> LeaseManager<P> {
    #[cfg(test)]
    pub(crate) fn in_memory(provider: P) -> Self {
        Self {
            provider,
            leases: BTreeMap::new(),
            completed: BTreeMap::new(),
            journal_path: None,
        }
    }

    pub(crate) fn open(provider: P, path: PathBuf) -> Result<Self, &'static str> {
        let journal = load_journal(&path)?;
        let leases = journal
            .leases
            .into_iter()
            .map(|lease| (lease.lease_id.clone(), lease))
            .collect();
        let completed = journal
            .completed
            .into_iter()
            .map(|lease| (lease.lease_id.clone(), lease))
            .collect();
        Ok(Self {
            provider,
            leases,
            completed,
            journal_path: Some(path),
        })
    }

    fn apply(&mut self, request: LeaseApplyRequest<'_>) -> ApplyOutcome {
        let LeaseApplyRequest {
            owner,
            session_id,
            executable_path,
            rtss_install_path,
            profile,
            request_fingerprint,
            desired_profile,
        } = request;
        if let Some(existing) = self
            .leases
            .values()
            .find(|lease| lease.owner == owner && lease.session_id == session_id)
            .cloned()
        {
            if existing.executable_path == executable_path
                && existing.profile == profile
                && existing.request_fingerprint == request_fingerprint
                && existing.applied.profile == desired_profile
                && existing.applied.profile_exists
            {
                return match self.current_state(&existing) {
                    Ok(true) => ApplyOutcome::AlreadyApplied(existing.clone()),
                    Ok(false) => ApplyOutcome::Stale(existing.lease_id.clone()),
                    Err(error) => ApplyOutcome::Unavailable(error.diagnostic()),
                };
            }
            return ApplyOutcome::Conflict("session_already_has_a_different_lease");
        }

        if !self.leases.is_empty() {
            return ApplyOutcome::Conflict("rtss_limiter_is_owned_by_another_session");
        }

        let normalized_install_path = match self.provider.select_installation(rtss_install_path) {
            Ok(path) => path,
            Err(error) => return ApplyOutcome::Unavailable(error.diagnostic()),
        };

        let before_profile = match self.provider.read_profile(profile) {
            Ok(values) => values,
            Err(error) => return ApplyOutcome::Unavailable(error.diagnostic()),
        };
        let before_disabled = match self.provider.limiter_disabled() {
            Ok(disabled) => disabled,
            Err(error) => return ApplyOutcome::Unavailable(error.diagnostic()),
        };
        let before_exists = match self.provider.profile_exists(profile) {
            Ok(exists) => exists,
            Err(error) => return ApplyOutcome::Unavailable(error.diagnostic()),
        };
        let before = ProviderSnapshot {
            profile: before_profile,
            profile_exists: before_exists,
            limiter_disabled: before_disabled,
        };
        if before_disabled {
            return ApplyOutcome::Unavailable("rtss_limiter_disabled_globally");
        }
        let applied = ProviderSnapshot {
            profile: desired_profile,
            profile_exists: true,
            limiter_disabled: before_disabled,
        };

        let mut record = LeaseRecord {
            lease_id: next_lease_id(),
            owner: owner.to_owned(),
            session_id: session_id.to_owned(),
            request_fingerprint: request_fingerprint.to_owned(),
            rtss_install_path: normalized_install_path,
            executable_path: executable_path.to_owned(),
            profile: profile.to_owned(),
            before,
            applied,
            phase: LeasePhase::Pending,
        };
        self.leases.insert(record.lease_id.clone(), record.clone());
        if let Err(code) = self.persist() {
            self.leases.remove(&record.lease_id);
            return ApplyOutcome::Unavailable(code);
        }

        if let Err(error) = self
            .provider
            .write_profile_verified(profile, desired_profile)
        {
            return self.rollback_pending_apply(record, error.diagnostic());
        }

        match self.current_state(&record) {
            Ok(true) => {
                record.phase = LeasePhase::Active;
                self.leases.insert(record.lease_id.clone(), record.clone());
                if self.persist().is_err() {
                    // The durable pending intent remains sufficient for startup
                    // recovery, and the effect was read back exactly.
                    record.phase = LeasePhase::Pending;
                    self.leases.insert(record.lease_id.clone(), record.clone());
                }
                ApplyOutcome::Applied(record)
            }
            Ok(false) => self.rollback_after_unverified_apply(record),
            Err(error) => {
                let code = error.diagnostic();
                self.rollback_after_unverified_apply_with_error(record, code)
            }
        }
    }

    pub(crate) fn status(
        &mut self,
        owner: &str,
        session_id: Option<&str>,
    ) -> Vec<OwnedLeaseStatus> {
        let owned: Vec<LeaseRecord> = self
            .leases
            .values()
            .filter(|lease| {
                lease.owner == owner && session_id.is_none_or(|session| lease.session_id == session)
            })
            .cloned()
            .collect();
        let mut results = Vec::with_capacity(owned.len());
        for lease in owned {
            let (state, diagnostic) = match self.current_state(&lease) {
                Ok(true) => ("applied", None),
                Ok(false) => ("stale_lease", Some("provider_state_changed")),
                Err(error) => ("stale_lease", Some(error.diagnostic())),
            };
            results.push(OwnedLeaseStatus {
                lease_id: lease.lease_id,
                session_id: lease.session_id,
                executable_path: lease.executable_path,
                state,
                diagnostic,
            });
        }
        results
    }

    pub(crate) fn restore(
        &mut self,
        owner: &str,
        session_id: Option<&str>,
        lease_id: Option<&str>,
    ) -> RestoreOutcome {
        let targets: Vec<String> = self
            .leases
            .values()
            .filter(|lease| {
                lease.owner == owner
                    && match (session_id, lease_id) {
                        (Some(session), None) => lease.session_id == session,
                        (None, Some(id)) => lease.lease_id == id,
                        _ => false,
                    }
            })
            .map(|lease| lease.lease_id.clone())
            .collect();
        if targets.is_empty() {
            let completed: Vec<String> = self
                .completed
                .values()
                .filter(|lease| {
                    lease.owner == owner
                        && match (session_id, lease_id) {
                            (Some(session), None) => lease.session_id == session,
                            (None, Some(id)) => lease.lease_id == id,
                            _ => false,
                        }
                })
                .map(|lease| lease.lease_id.clone())
                .collect();
            if !completed.is_empty() {
                return RestoreOutcome::Restored(completed);
            }
            return RestoreOutcome::NotFound;
        }

        let mut restored = Vec::new();
        for id in targets {
            let Some(lease) = self.leases.get(&id).cloned() else {
                continue;
            };
            match self.restore_one(&lease) {
                Ok(()) => {
                    self.leases.remove(&id);
                    self.completed.insert(
                        id.clone(),
                        CompletedLease {
                            lease_id: lease.lease_id.clone(),
                            owner: lease.owner.clone(),
                            session_id: lease.session_id.clone(),
                        },
                    );
                    restored.push(id);
                }
                Err(RestoreFailure::Stale) => return RestoreOutcome::Stale(lease.lease_id),
                Err(RestoreFailure::Provider(code)) => {
                    return RestoreOutcome::Failed {
                        lease_id: lease.lease_id,
                        code,
                    };
                }
            }
        }
        if let Err(code) = self.persist() {
            return RestoreOutcome::Failed {
                lease_id: restored.first().cloned().unwrap_or_default(),
                code,
            };
        }
        RestoreOutcome::Restored(restored)
    }

    #[cfg(test)]
    pub(crate) fn records(&self) -> Vec<LeaseRecord> {
        self.leases.values().cloned().collect()
    }

    /// Reconciles the durable journal with RTSS before accepting new requests.
    /// Pending intents at their applied state become active. Intents still at
    /// their before-state are finalized; any third state stays stale and is
    /// never overwritten during recovery.
    pub(crate) fn recover(&mut self) -> Result<(), &'static str> {
        let leases: Vec<LeaseRecord> = self.leases.values().cloned().collect();
        let mut changed = false;
        for lease in leases {
            let current = self
                .provider
                .select_installation(&lease.rtss_install_path)
                .and_then(|_| self.provider.read_profile(&lease.profile))
                .and_then(|profile| {
                    let exists = self.provider.profile_exists(&lease.profile)?;
                    let disabled = self.provider.limiter_disabled()?;
                    Ok(ProviderSnapshot {
                        profile,
                        profile_exists: exists,
                        limiter_disabled: disabled,
                    })
                });
            let Ok(current) = current else {
                continue;
            };
            if current == lease.before {
                self.leases.remove(&lease.lease_id);
                self.completed.insert(
                    lease.lease_id.clone(),
                    CompletedLease {
                        lease_id: lease.lease_id.clone(),
                        owner: lease.owner,
                        session_id: lease.session_id,
                    },
                );
                changed = true;
            } else if current == lease.applied && lease.phase == LeasePhase::Pending {
                let mut active = lease;
                active.phase = LeasePhase::Active;
                self.leases.insert(active.lease_id.clone(), active);
                changed = true;
            }
        }
        if changed {
            self.persist()?;
        }
        Ok(())
    }

    fn current_state(&mut self, lease: &LeaseRecord) -> Result<bool, ProviderError> {
        self.provider
            .select_installation(&lease.rtss_install_path)?;
        let profile = self.provider.read_profile(&lease.profile)?;
        let profile_exists = self.provider.profile_exists(&lease.profile)?;
        let disabled = self.provider.limiter_disabled()?;
        Ok(profile == lease.applied.profile
            && profile_exists == lease.applied.profile_exists
            && disabled == lease.applied.limiter_disabled)
    }

    fn restore_one(&mut self, lease: &LeaseRecord) -> Result<(), RestoreFailure> {
        self.provider
            .select_installation(&lease.rtss_install_path)
            .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?;
        let current = self
            .provider
            .read_profile(&lease.profile)
            .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?;
        let profile_exists = self
            .provider
            .profile_exists(&lease.profile)
            .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?;
        let disabled = self
            .provider
            .limiter_disabled()
            .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?;
        let current_profile = ProviderSnapshot {
            profile: current,
            profile_exists,
            limiter_disabled: disabled,
        };
        let profile_at_applied = current_profile.profile == lease.applied.profile
            && current_profile.profile_exists == lease.applied.profile_exists;
        let profile_at_before = current_profile.profile == lease.before.profile
            && current_profile.profile_exists == lease.before.profile_exists;
        let flag_at_applied = disabled == lease.applied.limiter_disabled;
        let flag_at_before = disabled == lease.before.limiter_disabled;
        if !profile_at_applied && !profile_at_before {
            return Err(RestoreFailure::Stale);
        }
        if !flag_at_applied && !flag_at_before {
            return Err(RestoreFailure::Stale);
        }

        if profile_at_applied {
            if lease.before.profile_exists {
                self.provider
                    .write_profile_verified(&lease.profile, lease.before.profile)
                    .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?;
            } else {
                self.provider
                    .delete_profile_verified(&lease.profile, lease.applied.profile)
                    .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?;
            }
        }
        if !self
            .exactly_at_before(lease)
            .map_err(|error| RestoreFailure::Provider(error.diagnostic()))?
        {
            return Err(RestoreFailure::Provider("restore_verification_failed"));
        }
        Ok(())
    }

    fn exactly_at_before(&mut self, lease: &LeaseRecord) -> Result<bool, ProviderError> {
        self.provider
            .select_installation(&lease.rtss_install_path)?;
        let profile = self.provider.read_profile(&lease.profile)?;
        let profile_exists = self.provider.profile_exists(&lease.profile)?;
        let disabled = self.provider.limiter_disabled()?;
        Ok(profile == lease.before.profile
            && profile_exists == lease.before.profile_exists
            && disabled == lease.before.limiter_disabled)
    }

    fn rollback_unleased(
        &mut self,
        profile: &str,
        before: ProviderSnapshot,
    ) -> Result<(), ProviderError> {
        if before.profile_exists {
            self.provider
                .write_profile_verified(profile, before.profile)?;
        } else if self.provider.profile_exists(profile)? {
            let current_profile = self.provider.read_profile(profile)?;
            self.provider
                .delete_profile_verified(profile, current_profile)?;
        }
        let profile_after = self.provider.read_profile(profile)?;
        let profile_exists_after = self.provider.profile_exists(profile)?;
        let disabled_after = self.provider.limiter_disabled()?;
        if profile_after != before.profile
            || profile_exists_after != before.profile_exists
            || disabled_after != before.limiter_disabled
        {
            return Err(ProviderError::VerifyFailed("rollback_verification_failed"));
        }
        Ok(())
    }

    fn rollback_after_unverified_apply(&mut self, lease: LeaseRecord) -> ApplyOutcome {
        self.rollback_after_unverified_apply_with_error(lease, "provider_readback_mismatch")
    }

    fn rollback_after_unverified_apply_with_error(
        &mut self,
        lease: LeaseRecord,
        code: &'static str,
    ) -> ApplyOutcome {
        let result = self.rollback_unleased(&lease.profile, lease.before);
        match result {
            Err(error) => ApplyOutcome::RestoreFailed {
                code,
                rollback_code: error.diagnostic(),
            },
            Ok(()) => {
                self.leases.remove(&lease.lease_id);
                if let Err(journal_code) = self.persist() {
                    self.leases.insert(lease.lease_id.clone(), lease);
                    ApplyOutcome::RestoreFailed {
                        code,
                        rollback_code: journal_code,
                    }
                } else {
                    ApplyOutcome::Unavailable(code)
                }
            }
        }
    }

    fn rollback_pending_apply(&mut self, lease: LeaseRecord, code: &'static str) -> ApplyOutcome {
        match self.rollback_unleased(&lease.profile, lease.before) {
            Err(error) => ApplyOutcome::RestoreFailed {
                code,
                rollback_code: error.diagnostic(),
            },
            Ok(()) => {
                self.leases.remove(&lease.lease_id);
                if let Err(journal_code) = self.persist() {
                    self.leases.insert(lease.lease_id.clone(), lease);
                    ApplyOutcome::RestoreFailed {
                        code,
                        rollback_code: journal_code,
                    }
                } else {
                    ApplyOutcome::Unavailable(code)
                }
            }
        }
    }

    fn persist(&self) -> Result<(), &'static str> {
        let Some(path) = &self.journal_path else {
            return Ok(());
        };
        save_journal(
            path,
            self.leases.values().cloned().collect(),
            self.completed.values().cloned().collect(),
        )
    }
}

/// Applies the `amaterasu.frame-limiter/1` operation against a real provider
/// while preserving the strict v1 parsing and pure policy response plan.
pub(crate) fn handle_runtime_operation<P: FrameLimitProvider>(
    caller: &str,
    payload: serde_json::Value,
    manager: &mut LeaseManager<P>,
) -> serde_json::Value {
    if caller.trim().is_empty() || caller.len() > 128 || caller.chars().any(char::is_control) {
        let valid_probe = Operation::Status(StatusRequest {
            version: crate::CONTRACT_VERSION,
            session_id: Presence::Missing,
        });
        return handle_operation(caller, valid_probe);
    }

    let operation_json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(_) => return handle_payload(caller, payload),
    };
    let operation = match parse_operation(&operation_json) {
        Ok(operation) => operation,
        Err(_) => return handle_payload(caller, payload),
    };
    let mut response = match build_response(&operation) {
        Ok(response) => response,
        Err(_) => return handle_operation(caller, operation),
    };

    match operation {
        Operation::Apply(request) => handle_apply(caller, request, manager, &mut response),
        Operation::Status(request) => handle_status(caller, request, manager, &mut response),
        Operation::Restore(request) => handle_restore(caller, request, manager, &mut response),
    }
    serde_json::to_value(response).unwrap_or_else(|_| {
        handle_payload(
            caller,
            serde_json::json!({"action":"status", "version":crate::CONTRACT_VERSION}),
        )
    })
}

fn handle_apply<P: FrameLimitProvider>(
    caller: &str,
    request: ApplyRequest,
    manager: &mut LeaseManager<P>,
    response: &mut OperationResponse,
) {
    let settings = match request.settings.to_model(current_platform()) {
        Ok(settings) => settings,
        Err(_) => {
            set_unavailable(response, "invalid_settings");
            return;
        }
    };
    let decision = match evaluate_apply(&request) {
        Ok(decision) => decision,
        Err(_) => {
            set_unavailable(response, "policy_evaluation_failed");
            return;
        }
    };
    if !matches!(
        settings.provider,
        FrameLimiterProvider::Auto | FrameLimiterProvider::Rtss
    ) {
        set_unavailable(response, "selected_provider_not_implemented");
        return;
    }
    if current_platform() != Platform::Windows {
        set_unavailable(response, "rtss_supported_on_windows_only");
        return;
    }
    if settings.disable_vsync {
        set_unavailable(response, "rtss_cannot_apply_disable_vsync");
        return;
    }
    if !request.settings.rtss_install_path.trim().is_empty()
        && !Path::new(&request.settings.rtss_install_path).is_absolute()
    {
        set_unavailable(response, "rtss_install_path_not_absolute");
        return;
    }
    let Some(plan) = response.plan.as_ref() else {
        set_unavailable(response, "policy_plan_unavailable");
        return;
    };
    match plan.should_consider_apply {
        Some(false) => {
            response.state = ResponseState::NotNeeded;
            response.provider_availability = ProviderAvailability::Unknown;
            response
                .diagnostics
                .push("policy_does_not_require_a_limiter".into());
            return;
        }
        None => {
            set_unavailable(response, "policy_context_incomplete");
            return;
        }
        Some(true) => (),
    }
    let Some(limit_millihz) = plan.effective_limit_millihz else {
        set_unavailable(response, "effective_limit_unknown");
        return;
    };
    if limit_millihz == 0 {
        response.state = ResponseState::NotNeeded;
        response.provider_availability = ProviderAvailability::Unknown;
        response
            .diagnostics
            .push("follow_stream_has_no_resolved_rate".into());
        return;
    }

    let profile = match profile_name(&request.executable_path) {
        Some(profile) => profile,
        None => {
            set_unavailable(response, "rtss_profile_name_unsupported");
            return;
        }
    };
    let (frame_limit, denominator) = exact_rtss_ratio(limit_millihz);
    let desired_profile = ProfileValues {
        frame_limit,
        frame_limit_denominator: denominator,
        sync_limiter: sync_limiter_value(decision.rtss_sync_limiter.value),
    };
    let fingerprint = match serde_json::to_string(&request) {
        Ok(fingerprint) => fingerprint,
        Err(_) => {
            set_unavailable(response, "request_fingerprint_failed");
            return;
        }
    };
    match manager.apply(LeaseApplyRequest {
        owner: caller,
        session_id: &request.session_id,
        executable_path: &request.executable_path,
        rtss_install_path: &request.settings.rtss_install_path,
        profile: &profile,
        request_fingerprint: &fingerprint,
        desired_profile,
    }) {
        ApplyOutcome::Applied(lease) => {
            response.state = ResponseState::Applied;
            response.applied = true;
            response.lease = Some(lease.lease_id);
            response.provider_availability = ProviderAvailability::Available;
            response.diagnostics.clear();
        }
        ApplyOutcome::AlreadyApplied(lease) => {
            response.state = ResponseState::AlreadyApplied;
            response.applied = true;
            response.lease = Some(lease.lease_id);
            response.provider_availability = ProviderAvailability::Available;
            response.diagnostics.clear();
        }
        ApplyOutcome::Conflict(code) => {
            response.state = ResponseState::LeaseConflict;
            response.provider_availability = ProviderAvailability::Available;
            response.diagnostics.push(code.into());
        }
        ApplyOutcome::Stale(_) => {
            response.state = ResponseState::StaleLease;
            response.provider_availability = ProviderAvailability::Unavailable;
            response
                .diagnostics
                .push("existing_session_lease_is_stale".into());
        }
        ApplyOutcome::Unavailable(code) => set_unavailable(response, code),
        ApplyOutcome::RestoreFailed {
            code,
            rollback_code,
        } => {
            response.state = ResponseState::RestoreFailed;
            response.provider_availability = ProviderAvailability::Unavailable;
            response.diagnostics.push(code.into());
            response.diagnostics.push(rollback_code.into());
            let statuses = manager.status(caller, Some(&request.session_id));
            response.applied = statuses.iter().any(|status| status.state == "applied");
            response.leases = statuses.into_iter().map(to_wire_lease_status).collect();
            if response.applied {
                response.lease = response.leases.first().map(|lease| lease.lease.clone());
            }
        }
    }
    if let Some(plan) = response.plan.as_mut() {
        plan.provider_availability = response.provider_availability;
    }
}

fn handle_status<P: FrameLimitProvider>(
    caller: &str,
    request: StatusRequest,
    manager: &mut LeaseManager<P>,
    response: &mut OperationResponse,
) {
    let session_id = request.session_id.as_ref().map(String::as_str);
    let statuses = manager.status(caller, session_id);
    if statuses.is_empty() {
        response.state = if session_id.is_some() {
            ResponseState::NotFound
        } else {
            ResponseState::NotNeeded
        };
        response.provider_availability = ProviderAvailability::Unknown;
        response.diagnostics.push("no_owned_rtss_lease".into());
        return;
    }
    response.applied = statuses.iter().any(|status| status.state == "applied");
    response.state = if statuses.iter().any(|status| status.state == "stale_lease") {
        ResponseState::StaleLease
    } else {
        ResponseState::Applied
    };
    response.provider_availability = if response.applied {
        ProviderAvailability::Available
    } else {
        ProviderAvailability::Unavailable
    };
    response.leases = statuses.into_iter().map(to_wire_lease_status).collect();
    response.diagnostics = response
        .leases
        .iter()
        .flat_map(|lease| lease.diagnostics.iter().cloned())
        .collect();
}

fn handle_restore<P: FrameLimitProvider>(
    caller: &str,
    request: RestoreRequest,
    manager: &mut LeaseManager<P>,
    response: &mut OperationResponse,
) {
    let (session_id, lease_id) = match (&request.session_id, &request.lease) {
        (Presence::Value(session), Presence::Missing | Presence::Null) => {
            (Some(session.as_str()), None)
        }
        (Presence::Missing | Presence::Null, Presence::Value(lease)) => {
            (None, Some(lease.as_str()))
        }
        _ => {
            set_unavailable(response, "invalid_restore_target");
            return;
        }
    };
    match manager.restore(caller, session_id, lease_id) {
        RestoreOutcome::Restored(_) => {
            response.state = ResponseState::Restored;
            response.provider_availability = ProviderAvailability::Available;
        }
        RestoreOutcome::NotFound => {
            response.state = ResponseState::NotFound;
            response.provider_availability = ProviderAvailability::Unknown;
            response.diagnostics.push("owned_lease_not_found".into());
        }
        RestoreOutcome::Stale(_) => {
            response.state = ResponseState::StaleLease;
            response.provider_availability = ProviderAvailability::Available;
            response
                .diagnostics
                .push("provider_state_changed_before_restore".into());
        }
        RestoreOutcome::Failed { lease_id: _, code } => {
            response.state = ResponseState::RestoreFailed;
            response.provider_availability = ProviderAvailability::Unavailable;
            response.diagnostics.push(code.into());
            response.leases = manager
                .status(caller, session_id)
                .into_iter()
                .map(to_wire_lease_status)
                .collect();
        }
    }
}

fn to_wire_lease_status(lease: OwnedLeaseStatus) -> crate::LeaseStatus {
    let stale = lease.state == "stale_lease";
    crate::LeaseStatus {
        lease: lease.lease_id,
        session_id: lease.session_id,
        executable_path: lease.executable_path,
        provider: "rtss".into(),
        state: if stale {
            ResponseState::StaleLease
        } else {
            ResponseState::Applied
        },
        applied: !stale,
        diagnostics: lease.diagnostic.into_iter().map(str::to_owned).collect(),
    }
}

fn set_unavailable(response: &mut OperationResponse, diagnostic: &str) {
    response.state = ResponseState::RuntimeUnavailable;
    response.applied = false;
    response.provider_availability = ProviderAvailability::Unavailable;
    response.diagnostics.push(diagnostic.into());
    if let Some(plan) = response.plan.as_mut() {
        plan.provider_availability = ProviderAvailability::Unavailable;
    }
}

fn current_platform() -> Platform {
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(target_os = "linux")]
    {
        Platform::Linux
    }
    #[cfg(target_os = "macos")]
    {
        Platform::Macos
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Platform::Macos
    }
}

fn profile_name(executable_path: &str) -> Option<String> {
    let name = executable_path.rsplit(['\\', '/']).next()?;
    if name.is_empty()
        || !name.ends_with(".exe")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }
    Some(name.to_owned())
}

fn exact_rtss_ratio(millihz: u32) -> (i32, i32) {
    let divisor = gcd(millihz, 1000);
    ((millihz / divisor) as i32, (1000 / divisor) as i32)
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn sync_limiter_value(value: RtssFrameLimitType) -> i32 {
    match value {
        RtssFrameLimitType::Async => 0,
        RtssFrameLimitType::FrontEdgeSync => 1,
        RtssFrameLimitType::BackEdgeSync => 2,
        RtssFrameLimitType::NvidiaReflex => 3,
    }
}

enum RestoreFailure {
    Stale,
    Provider(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    Applied(LeaseRecord),
    AlreadyApplied(LeaseRecord),
    Conflict(&'static str),
    Stale(String),
    Unavailable(&'static str),
    RestoreFailed {
        code: &'static str,
        rollback_code: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RestoreOutcome {
    Restored(Vec<String>),
    NotFound,
    Stale(String),
    Failed {
        lease_id: String,
        code: &'static str,
    },
}

fn next_lease_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let sequence = NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp:032x}-{sequence:016x}")
}

fn load_journal(path: &Path) -> Result<Journal, &'static str> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Journal::default());
        }
        Err(_) => return Err("lease_journal_unavailable"),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("lease_journal_invalid_path");
        }
        Ok(_) => (),
    }
    let metadata = fs::metadata(path).map_err(|_| "lease_journal_unavailable")?;
    if metadata.len() > JOURNAL_LIMIT_BYTES {
        return Err("lease_journal_too_large");
    }
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| "lease_journal_unavailable")?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(JOURNAL_LIMIT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "lease_journal_unavailable")?;
    if bytes.len() as u64 > JOURNAL_LIMIT_BYTES {
        return Err("lease_journal_too_large");
    }
    let journal: Journal = serde_json::from_slice(&bytes).map_err(|_| "lease_journal_corrupt")?;
    if journal.format_version != JOURNAL_FORMAT {
        return Err("lease_journal_version_unsupported");
    }
    let mut identifiers = BTreeMap::new();
    for lease in &journal.leases {
        if lease.lease_id.is_empty()
            || lease.owner.is_empty()
            || lease.session_id.is_empty()
            || lease.profile.is_empty()
            || identifiers.insert(lease.lease_id.as_str(), ()).is_some()
        {
            return Err("lease_journal_corrupt");
        }
    }
    Ok(journal)
}

fn save_journal(
    path: &Path,
    leases: Vec<LeaseRecord>,
    completed: Vec<CompletedLease>,
) -> Result<(), &'static str> {
    let parent = path.parent().ok_or("lease_journal_invalid_path")?;
    fs::create_dir_all(parent).map_err(|_| "lease_journal_unavailable")?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
        _ => return Err("lease_journal_invalid_path"),
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        _ => return Err("lease_journal_invalid_path"),
    }
    let journal = Journal {
        format_version: JOURNAL_FORMAT,
        leases,
        completed: completed.into_iter().rev().take(256).collect(),
    };
    let bytes = serde_json::to_vec(&journal).map_err(|_| "lease_journal_unavailable")?;
    if bytes.len() as u64 > JOURNAL_LIMIT_BYTES {
        return Err("lease_journal_too_large");
    }
    let temp_path = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|_| "lease_journal_unavailable")?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|_| "lease_journal_unavailable")?;
        file.sync_all().map_err(|_| "lease_journal_unavailable")?;
        fs::rename(&temp_path, path).map_err(|_| "lease_journal_unavailable")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const START: ProfileValues = ProfileValues {
        frame_limit: 60,
        frame_limit_denominator: 1,
        sync_limiter: 0,
    };
    const TARGET: ProfileValues = ProfileValues {
        frame_limit: 59_500,
        frame_limit_denominator: 1_000,
        sync_limiter: 2,
    };

    #[derive(Clone)]
    struct FakeProvider {
        profiles: BTreeMap<String, ProfileValues>,
        templates: BTreeMap<String, ProfileValues>,
        existent_profiles: BTreeMap<String, bool>,
        limiter_disabled: bool,
        fail_writes: bool,
        mismatch_writeback: bool,
        journal_path: Option<PathBuf>,
        durable_pending_seen_before_write: bool,
    }

    impl FakeProvider {
        fn new() -> Self {
            Self {
                profiles: [("game.exe".into(), START)].into(),
                templates: [("game.exe".into(), START)].into(),
                existent_profiles: [("game.exe".into(), true)].into(),
                limiter_disabled: false,
                fail_writes: false,
                mismatch_writeback: false,
                journal_path: None,
                durable_pending_seen_before_write: false,
            }
        }
    }

    impl FrameLimitProvider for FakeProvider {
        fn select_installation(&mut self, path: &str) -> Result<String, ProviderError> {
            Ok(path.to_owned())
        }

        fn profile_exists(&mut self, profile: &str) -> Result<bool, ProviderError> {
            Ok(self
                .existent_profiles
                .get(profile)
                .copied()
                .unwrap_or(false))
        }

        fn read_profile(&mut self, profile: &str) -> Result<ProfileValues, ProviderError> {
            self.profiles
                .get(profile)
                .copied()
                .ok_or(ProviderError::ReadFailed("profile_not_found"))
        }

        fn write_profile_verified(
            &mut self,
            profile: &str,
            values: ProfileValues,
        ) -> Result<(), ProviderError> {
            if self.fail_writes {
                return Err(ProviderError::WriteFailed("fake_write_failure"));
            }
            if let Some(path) = &self.journal_path {
                let journal = load_journal(path).expect(
                    "the write-ahead lease intent must be durable before provider mutation",
                );
                self.durable_pending_seen_before_write = journal.leases.iter().any(|lease| {
                    lease.phase == LeasePhase::Pending
                        && lease.profile == profile
                        && lease.before.profile == START
                        && lease.applied.profile == values
                });
                assert!(self.durable_pending_seen_before_write);
            }
            self.profiles.insert(
                profile.to_owned(),
                if self.mismatch_writeback {
                    ProfileValues {
                        frame_limit: values.frame_limit - 1,
                        ..values
                    }
                } else {
                    values
                },
            );
            self.existent_profiles.insert(profile.to_owned(), true);
            Ok(())
        }

        fn delete_profile_verified(
            &mut self,
            profile: &str,
            expected: ProfileValues,
        ) -> Result<(), ProviderError> {
            if self.read_profile(profile)? != expected {
                return Err(ProviderError::VerifyFailed("profile_changed_before_delete"));
            }
            if let Some(template) = self.templates.get(profile).copied() {
                self.profiles.insert(profile.to_owned(), template);
            } else {
                self.profiles.remove(profile);
            }
            self.existent_profiles.insert(profile.to_owned(), false);
            Ok(())
        }

        fn limiter_disabled(&mut self) -> Result<bool, ProviderError> {
            Ok(self.limiter_disabled)
        }
    }

    fn apply(manager: &mut LeaseManager<FakeProvider>, owner: &str, session: &str) -> ApplyOutcome {
        manager.apply(LeaseApplyRequest {
            owner,
            session_id: session,
            executable_path: r"C:\Games\game.exe",
            rtss_install_path: r"C:\RTSS",
            profile: "game.exe",
            request_fingerprint: "same-request",
            desired_profile: TARGET,
        })
    }

    #[test]
    fn apply_reads_back_exact_values_and_duplicate_retry_returns_same_lease() {
        let mut manager = LeaseManager::in_memory(FakeProvider::new());
        let ApplyOutcome::Applied(lease) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("first apply must create a verified lease");
        };
        assert_eq!(lease.before.profile, START);
        assert_eq!(lease.applied.profile, TARGET);
        assert!(!lease.before.limiter_disabled);
        assert!(!lease.applied.limiter_disabled);
        assert_eq!(
            apply(&mut manager, "caller.a", "session-1"),
            ApplyOutcome::AlreadyApplied(lease)
        );
    }

    #[test]
    fn different_owner_cannot_observe_or_restore_another_callers_lease() {
        let mut manager = LeaseManager::in_memory(FakeProvider::new());
        let ApplyOutcome::Applied(lease) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("apply must succeed");
        };
        assert!(manager.status("caller.b", None).is_empty());
        assert_eq!(
            manager.restore("caller.b", None, Some(&lease.lease_id)),
            RestoreOutcome::NotFound
        );
        assert_eq!(
            manager.status("caller.a", Some("session-1"))[0].state,
            "applied"
        );
    }

    #[test]
    fn restart_reloads_owned_lease_and_restores_exact_before_state() {
        let provider = FakeProvider::new();
        let temp_directory = std::env::temp_dir().join(format!(
            "amaterasu-frame-limiter-{}-{}",
            std::process::id(),
            NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let journal_path = temp_directory.join("leases.json");
        let mut first = LeaseManager::open(provider, journal_path.clone()).unwrap();
        let ApplyOutcome::Applied(lease) = apply(&mut first, "caller.a", "session-1") else {
            panic!("apply must succeed");
        };
        let provider = first.provider.clone();
        drop(first);
        let mut restarted = LeaseManager::open(provider, journal_path.clone()).unwrap();
        assert_eq!(restarted.status("caller.a", None)[0].state, "applied");
        assert_eq!(
            restarted.restore("caller.a", Some("session-1"), None),
            RestoreOutcome::Restored(vec![lease.lease_id.clone()])
        );
        assert_eq!(restarted.provider.profiles["game.exe"], START);
        assert!(!restarted.provider.limiter_disabled);
        assert_eq!(
            restarted.restore("caller.a", None, Some(&lease.lease_id)),
            RestoreOutcome::Restored(vec![lease.lease_id.clone()])
        );
        drop(restarted);
        fs::remove_dir_all(temp_directory).unwrap();
    }

    #[test]
    fn durable_pending_intent_precedes_provider_mutation() {
        let temp_directory = std::env::temp_dir().join(format!(
            "amaterasu-frame-limiter-write-ahead-{}-{}",
            std::process::id(),
            NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let journal_path = temp_directory.join("leases.json");
        let mut provider = FakeProvider::new();
        provider.journal_path = Some(journal_path.clone());
        let mut manager = LeaseManager::open(provider, journal_path.clone()).unwrap();
        assert!(matches!(
            apply(&mut manager, "caller.a", "session-1"),
            ApplyOutcome::Applied(_)
        ));
        assert!(manager.provider.durable_pending_seen_before_write);
        drop(manager);
        fs::remove_dir_all(temp_directory).unwrap();
    }

    #[test]
    fn restart_promotes_pending_intent_after_crash_between_write_and_activation() {
        let temp_directory = std::env::temp_dir().join(format!(
            "amaterasu-frame-limiter-pending-recovery-{}-{}",
            std::process::id(),
            NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let journal_path = temp_directory.join("leases.json");
        let mut first = LeaseManager::open(FakeProvider::new(), journal_path.clone()).unwrap();
        let ApplyOutcome::Applied(active_lease) = apply(&mut first, "caller.a", "session-1") else {
            panic!("apply must establish the fake provider state");
        };

        // Model a process crash after the provider write/readback, before the
        // journal could promote the durable intent to Active.
        let mut pending_lease = active_lease.clone();
        pending_lease.phase = LeasePhase::Pending;
        save_journal(&journal_path, vec![pending_lease], Vec::new()).unwrap();
        let provider = first.provider.clone();
        drop(first);

        let mut restarted = LeaseManager::open(provider, journal_path.clone()).unwrap();
        assert_eq!(restarted.records()[0].phase, LeasePhase::Pending);
        restarted.recover().unwrap();
        assert_eq!(restarted.records()[0].phase, LeasePhase::Active);
        assert_eq!(restarted.status("caller.a", None)[0].state, "applied");
        assert_eq!(
            restarted.restore("caller.a", Some("session-1"), None),
            RestoreOutcome::Restored(vec![active_lease.lease_id])
        );
        drop(restarted);
        fs::remove_dir_all(temp_directory).unwrap();
    }

    #[test]
    fn restore_failure_retains_the_lease_for_retry() {
        let mut manager = LeaseManager::in_memory(FakeProvider::new());
        let ApplyOutcome::Applied(lease) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("apply must succeed");
        };
        manager.provider.fail_writes = true;
        assert_eq!(
            manager.restore("caller.a", None, Some(&lease.lease_id)),
            RestoreOutcome::Failed {
                lease_id: lease.lease_id.clone(),
                code: "fake_write_failure",
            }
        );
        assert_eq!(manager.records().len(), 1);
        manager.provider.fail_writes = false;
        assert_eq!(
            manager.restore("caller.a", None, Some(&lease.lease_id)),
            RestoreOutcome::Restored(vec![lease.lease_id.clone()])
        );
        assert_eq!(
            manager.restore("caller.a", None, Some(&lease.lease_id)),
            RestoreOutcome::Restored(vec![lease.lease_id])
        );
        assert!(manager.records().is_empty());
    }

    #[test]
    fn a_different_owner_cannot_acquire_the_global_rtss_lease() {
        let mut manager = LeaseManager::in_memory(FakeProvider::new());
        let ApplyOutcome::Applied(_) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("first apply must succeed");
        };
        assert_eq!(
            apply(&mut manager, "caller.b", "session-2"),
            ApplyOutcome::Conflict("rtss_limiter_is_owned_by_another_session")
        );
    }

    #[test]
    fn same_owner_session_rejects_changed_full_request_fingerprint() {
        let mut manager = LeaseManager::in_memory(FakeProvider::new());
        let ApplyOutcome::Applied(_) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("first apply must succeed");
        };
        assert_eq!(
            manager.apply(LeaseApplyRequest {
                owner: "caller.a",
                session_id: "session-1",
                executable_path: r"C:\Games\game.exe",
                rtss_install_path: r"C:\RTSS",
                profile: "game.exe",
                request_fingerprint: "different-context-or-settings",
                desired_profile: TARGET,
            }),
            ApplyOutcome::Conflict("session_already_has_a_different_lease")
        );
    }

    #[test]
    fn unverified_settings_are_rolled_back_without_issuing_a_lease() {
        let mut provider = FakeProvider::new();
        provider.mismatch_writeback = true;
        let mut manager = LeaseManager::in_memory(provider);
        assert!(matches!(
            apply(&mut manager, "caller.a", "session-1"),
            ApplyOutcome::RestoreFailed { .. }
        ));
        assert_eq!(manager.records().len(), 1);
        assert_eq!(manager.records()[0].phase, LeasePhase::Pending);
    }

    #[test]
    fn globally_disabled_limiter_is_not_enabled_as_a_side_effect() {
        let mut provider = FakeProvider::new();
        provider.limiter_disabled = true;
        let mut manager = LeaseManager::in_memory(provider);
        assert_eq!(
            apply(&mut manager, "caller.a", "session-1"),
            ApplyOutcome::Unavailable("rtss_limiter_disabled_globally")
        );
        assert!(manager.provider.limiter_disabled);
        assert!(manager.records().is_empty());
    }

    #[test]
    fn restart_reports_external_provider_changes_as_stale_without_overwriting_them() {
        let temp_directory = std::env::temp_dir().join(format!(
            "amaterasu-frame-limiter-stale-{}-{}",
            std::process::id(),
            NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let journal_path = temp_directory.join("leases.json");
        let mut manager = LeaseManager::open(FakeProvider::new(), journal_path.clone()).unwrap();
        let ApplyOutcome::Applied(lease) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("apply must succeed");
        };
        manager
            .provider
            .profiles
            .get_mut("game.exe")
            .unwrap()
            .frame_limit = 72;
        let provider = manager.provider.clone();
        drop(manager);

        let mut restarted = LeaseManager::open(provider, journal_path.clone()).unwrap();
        let status = restarted.status("caller.a", None);
        assert_eq!(status[0].state, "stale_lease");
        assert_eq!(status[0].diagnostic, Some("provider_state_changed"));
        assert_eq!(
            restarted.restore("caller.a", Some("session-1"), None),
            RestoreOutcome::Stale(lease.lease_id)
        );
        assert_eq!(restarted.provider.profiles["game.exe"].frame_limit, 72);
        drop(restarted);
        fs::remove_dir_all(temp_directory).unwrap();
    }

    #[test]
    fn absent_profile_is_created_for_apply_and_removed_on_exact_restore() {
        let mut provider = FakeProvider::new();
        provider.existent_profiles.insert("game.exe".into(), false);
        let mut manager = LeaseManager::in_memory(provider);
        let ApplyOutcome::Applied(lease) = apply(&mut manager, "caller.a", "session-1") else {
            panic!("readable template values permit a reversible lease");
        };
        assert!(manager.provider.existent_profiles["game.exe"]);
        assert_eq!(
            manager.restore("caller.a", None, Some(&lease.lease_id)),
            RestoreOutcome::Restored(vec![lease.lease_id])
        );
        assert!(!manager.provider.existent_profiles["game.exe"]);
        assert_eq!(manager.provider.profiles["game.exe"], START);
    }

    #[cfg(windows)]
    #[test]
    fn unsupported_vsync_request_is_declined_before_any_provider_mutation() {
        let mut manager = LeaseManager::in_memory(FakeProvider::new());
        let before_profiles = manager.provider.profiles.clone();
        let response = handle_runtime_operation(
            "caller.a",
            serde_json::json!({
                "action":"apply",
                "version":1,
                "session_id":"1001",
                "executable_path":r"C:\Games\game.exe",
                "settings":{
                    "frame_limiter_enable":true,
                    "frame_limiter_provider":"rtss",
                    "frame_limiter_fps_limit_millihz":60000,
                    "frame_limiter_disable_vsync":true,
                    "frame_limiter_auto_virtual_framegen":"enabled",
                    "rtss_frame_limit_type":"async",
                    "rtss_install_path":r"C:\Program Files (x86)\RivaTuner Statistics Server",
                    "rtss_allow_virtual_display_override":false,
                    "mangohud_preset":"custom",
                    "mangohud_always_show_graph":false,
                    "mangohud_limiter_method":"late"
                }
            }),
            &mut manager,
        );
        assert_eq!(response["state"], "runtime_unavailable");
        assert_eq!(
            response["diagnostics"][0],
            "rtss_cannot_apply_disable_vsync"
        );
        assert_eq!(manager.provider.profiles, before_profiles);
        assert!(manager.records().is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn apply_rollback_failure_returns_the_retained_session_cleanup_lease() {
        let mut provider = FakeProvider::new();
        provider.fail_writes = true;
        let mut manager = LeaseManager::in_memory(provider);
        let response = handle_runtime_operation(
            "caller.a",
            serde_json::json!({
                "action":"apply",
                "version":1,
                "session_id":"1001",
                "executable_path":r"C:\Games\game.exe",
                "settings":{
                    "frame_limiter_enable":true,
                    "frame_limiter_provider":"rtss",
                    "frame_limiter_fps_limit_millihz":59500,
                    "frame_limiter_disable_vsync":false,
                    "frame_limiter_auto_virtual_framegen":"enabled",
                    "rtss_frame_limit_type":"async",
                    "rtss_install_path":r"C:\Program Files (x86)\RivaTuner Statistics Server",
                    "rtss_allow_virtual_display_override":false,
                    "mangohud_preset":"custom",
                    "mangohud_always_show_graph":false,
                    "mangohud_limiter_method":"late"
                }
            }),
            &mut manager,
        );
        assert_eq!(response["state"], "restore_failed");
        assert_eq!(response["applied"], false);
        assert_eq!(response["leases"].as_array().unwrap().len(), 1);
        assert_eq!(response["leases"][0]["state"], "stale_lease");
        assert!(!response["leases"][0]["lease"].as_str().unwrap().is_empty());
        assert_eq!(response["leases"][0]["session_id"], "1001");
    }
}
