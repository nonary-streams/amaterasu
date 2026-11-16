//! Durable driver setup state machine, independent of installer implementation.
//! Call from a worker. Persist before every machine-changing operation.
use super::{composition::PluginSource, digest, validate_requirement};
use crate::PackageResult;
use amaterasu_module_sdk::drivers::{
    Action, DriverRequirement, DriverState, HelperReply, HelperRequest, SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Checking,
    AwaitingApproval,
    WaitingForIdle,
    Installing,
    RebootRequired,
    ManualActionRequired,
    ReadyToActivate,
    Activating,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Driver {
    pub requirement: DriverRequirement,
    pub required_by: Vec<String>,
    /// Component identities for details navigation, derived from signed manifests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_components: Vec<String>,
    pub state: DriverState,
    pub message: String,
    pub installed_version: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub repository: String,
    pub manifest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<Preparation>,
    pub id: String,
    pub revision: u64,
    pub installation_revision: String,
    pub sources: Vec<PluginSource>,
    pub runtime_identity: String,
    pub phase: Phase,
    pub approved_revision: Option<String>,
    pub drivers: Vec<Driver>,
    pub error: Option<String>,
    /// Set durably before calling Install. An interrupted call requires Reconcile.
    pub in_flight: Option<String>,
}

/// Implementations must independently validate current publisher approval,
/// exact prepared bytes and the finite machine-operation approval.
pub trait DriverBackend {
    fn request(
        &mut self,
        operation: &Operation,
        request: &HelperRequest,
    ) -> PackageResult<HelperReply>;
}

/// Stored operations live below their own private lock directory; the host can
/// continue holding its package root lock. All read/modify/write steps use CAS.
pub struct OperationStore {
    directory: std::path::PathBuf,
}

impl OperationStore {
    pub fn new(directory: std::path::PathBuf) -> PackageResult<Self> {
        if !directory.is_absolute()
            || directory
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("plugin_invalid_operation_directory".into());
        }
        crate::create_private_directory(&directory)?;
        super::validate_directory(&directory)?;
        Ok(Self { directory })
    }

    /// Atomic replacement makes unlocked status observation safe; mutations still use CAS.
    pub fn observe(&self) -> PackageResult<Option<Operation>> {
        self.read_locked()
    }

    pub fn read(&self) -> PackageResult<Option<Operation>> {
        let _lock = crate::RootLock::acquire(&self.directory)?;
        self.read_locked()
    }

    pub fn create(&self, operation: &Operation) -> PackageResult<()> {
        let _lock = crate::RootLock::acquire(&self.directory)?;
        if let Some(previous) = self.read_locked()?
            && !(matches!(
                previous.phase,
                Phase::Ready | Phase::Cancelled | Phase::Failed
            ) && previous.in_flight.is_none())
        {
            return Err("plugin_setup_in_progress".into());
        }
        self.write_locked(operation)
    }

    /// A successful write never
    /// overwrites another browser's decision or a worker's newer receipt.
    pub fn save(&self, expected: (&str, u64), operation: &Operation) -> PackageResult<()> {
        let _lock = crate::RootLock::acquire(&self.directory)?;
        let current = self.read_locked()?.ok_or("plugin_operation_missing")?;
        if current.id != expected.0
            || current.revision != expected.1
            || operation.id != current.id
            || operation.revision <= current.revision
        {
            return Err("plugin_revision_conflict".into());
        }
        self.write_locked(operation)
    }

    fn read_locked(&self) -> PackageResult<Option<Operation>> {
        let path = self.directory.join("operation.json");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            _ => return Err("plugin_unsafe_operation_record".into()),
        }
        let raw = crate::read_bounded(&path, 256 * 1024)?;
        let operation: Operation = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        operation.validate()?;
        Ok(Some(operation))
    }

    fn write_locked(&self, operation: &Operation) -> PackageResult<()> {
        operation.validate()?;
        if serde_json::to_vec(operation)
            .map_err(|e| e.to_string())?
            .len()
            > 256 * 1024
        {
            return Err("plugin_operation_too_large".into());
        }
        crate::write_json_atomically(&self.directory.join("operation.json"), operation)
    }
}

impl Operation {
    /// Bounded management representation. Sequence/revision values remain strings
    /// so browsers never round a u64 and approve a different operation revision.
    pub fn management_view(&self) -> PackageResult<serde_json::Value> {
        self.validate()?;
        let drivers = driver_management_view(&self.drivers, &self.sources);
        Ok(
            serde_json::json!({"id":self.id,"revision":self.revision.to_string(),"installation_revision":self.installation_revision,
            "phase":self.phase,"approval":self.approval_digest()?,"drivers":drivers,"error":self.error,
            "cancellable":self.in_flight.is_none() && !matches!(self.phase,Phase::Installing|Phase::Activating|Phase::Ready|Phase::Cancelled)}),
        )
    }

    pub fn validate(&self) -> PackageResult<()> {
        if self.schema != 1
            || self.revision == 0
            || !super::digest(&self.installation_revision)
            || self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || self.runtime_identity.is_empty()
            || self.runtime_identity.len() > 256
            || self.runtime_identity.chars().any(char::is_control)
            || self.drivers.len() > 32
            || self.sources.len() > 32
            || self.error.as_ref().is_some_and(|value| value.len() > 4096)
        {
            return Err("plugin_invalid_operation".into());
        }
        if let Some(preparation) = &self.preparation
            && (!amaterasu_contracts::valid_name(&preparation.repository)
                || !digest(&preparation.manifest)
                || !self.sources.is_empty()
                || !self.drivers.is_empty()
                || self.approved_revision.is_some()
                || !matches!(
                    self.phase,
                    Phase::Checking | Phase::Failed | Phase::Cancelled
                ))
        {
            return Err("plugin_invalid_preparation".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for driver in &self.drivers {
            validate_requirement(&driver.requirement)?;
            if !ids.insert(&driver.requirement.id)
                || driver.required_by.is_empty()
                || driver.required_by.len() > 128
                || driver.required_components.len() > 1024
                || driver
                    .required_components
                    .iter()
                    .any(|value| !amaterasu_contracts::valid_name(value))
                || driver
                    .required_by
                    .iter()
                    .any(|value| !amaterasu_contracts::valid_name(value))
                || driver.message.len() > 4096
            {
                return Err("driver_invalid_operation".into());
            }
        }
        if self.in_flight.as_ref().is_some_and(|id| !ids.contains(id))
            || self
                .approved_revision
                .as_ref()
                .is_some_and(|approval| self.approval_digest().as_ref() != Ok(approval))
        {
            return Err("driver_stale_approval".into());
        }
        Ok(())
    }

    pub fn prepare(
        installation_revision: String,
        sources: Vec<PluginSource>,
        runtime_identity: String,
        requirements: Vec<(String, DriverRequirement)>,
    ) -> PackageResult<Self> {
        if !digest(&installation_revision)
            || sources.len() > 32
            || runtime_identity.is_empty()
            || runtime_identity.len() > 256
            || runtime_identity.chars().any(char::is_control)
        {
            return Err("plugin_invalid_preparation".into());
        }
        let mut drivers = BTreeMap::<String, Driver>::new();
        for (plugin, requirement) in requirements {
            validate_requirement(&requirement)?;
            if let Some(existing) = drivers.get_mut(&requirement.id) {
                // A driver may be shared only when the exact installer, payload and
                // compatibility contract agree. A provider may package a broader contract.
                let old = &existing.requirement;
                if old.feature != requirement.feature
                    || old.target != requirement.target
                    || old.compatibility != requirement.compatibility
                    || old.offered_version != requirement.offered_version
                    || old.installer != requirement.installer
                    || old.payloads != requirement.payloads
                {
                    return Err(format!("driver_requirement_conflict:{}", requirement.id));
                }
                if !existing.required_by.contains(&plugin) {
                    existing.required_by.push(plugin);
                }
            } else {
                drivers.insert(
                    requirement.id.clone(),
                    Driver {
                        requirement,
                        required_by: vec![plugin],
                        required_components: Vec::new(),
                        state: DriverState::Checking,
                        message: String::new(),
                        installed_version: None,
                    },
                );
            }
        }
        if drivers.len() > 32 {
            return Err("driver_too_many_requirements".into());
        }
        Ok(Self {
            schema: 1,
            preparation: None,
            id: crate::random_identifier()?,
            revision: 1,
            installation_revision,
            sources,
            runtime_identity,
            phase: Phase::Checking,
            approved_revision: None,
            drivers: drivers.into_values().collect(),
            error: None,
            in_flight: None,
        })
    }

    /// Approval binds all immutable inputs. Runtime status never broadens it.
    pub fn approval_digest(&self) -> PackageResult<String> {
        let requirements: Vec<_> = self
            .drivers
            .iter()
            .map(|driver| (&driver.requirement, &driver.required_by))
            .collect();
        let bytes = serde_json::to_vec(&(
            self.schema,
            &self.id,
            &self.installation_revision,
            &self.sources,
            &self.runtime_identity,
            requirements,
        ))
        .map_err(|e| e.to_string())?;
        Ok(crate::sha256(&bytes))
    }

    pub fn approve(
        &mut self,
        revision: u64,
        installation_revision: &str,
        approval: &str,
    ) -> PackageResult<()> {
        self.require_current(revision, installation_revision)?;
        if self.phase != Phase::AwaitingApproval || self.approval_digest()? != approval {
            return Err("driver_stale_approval".into());
        }
        self.approved_revision = Some(approval.into());
        self.phase = Phase::WaitingForIdle;
        self.bump_revision()?;
        Ok(())
    }

    pub fn cancel(&mut self, revision: u64, installation_revision: &str) -> PackageResult<()> {
        self.require_current(revision, installation_revision)?;
        if self.in_flight.is_some()
            || matches!(
                self.phase,
                Phase::Installing | Phase::Activating | Phase::Ready
            )
        {
            return Err("driver_not_cancellable".into());
        }
        self.phase = Phase::Cancelled;
        self.approved_revision = None;
        self.bump_revision()?;
        Ok(())
    }

    pub fn require_current(&self, revision: u64, installation_revision: &str) -> PackageResult<()> {
        if self.revision != revision || self.installation_revision != installation_revision {
            return Err("plugin_revision_conflict".into());
        }
        Ok(())
    }

    /// A failed or reboot-pending operation is explicitly rechecked, never assumed ready.
    pub fn recheck(&mut self, revision: u64, installation_revision: &str) -> PackageResult<()> {
        self.require_current(revision, installation_revision)?;
        if matches!(
            self.phase,
            Phase::Installing | Phase::Activating | Phase::Ready | Phase::Cancelled
        ) {
            return Err("plugin_operation_in_progress".into());
        }
        self.phase = Phase::Checking;
        self.error = None;
        self.bump_revision()?;
        Ok(())
    }

    pub fn advance(
        &mut self,
        idle: bool,
        backend: &mut impl DriverBackend,
        persist: &mut impl FnMut(&Self) -> PackageResult<()>,
    ) -> PackageResult<()> {
        self.validate()?;
        if self.preparation.is_some() {
            return Err("plugin_preparation_incomplete".into());
        }
        if !matches!(
            self.phase,
            Phase::Checking | Phase::WaitingForIdle | Phase::Installing
        ) {
            return Ok(());
        }
        if !idle {
            if self.phase != Phase::WaitingForIdle {
                self.phase = Phase::WaitingForIdle;
                self.revision = self
                    .revision
                    .checked_add(1)
                    .ok_or("plugin_revision_exhausted")?;
                self.persist(persist)?;
            }
            return Ok(());
        }
        let approved = self
            .approved_revision
            .as_ref()
            .is_some_and(|value| self.approval_digest().as_ref() == Ok(value));
        for index in 0..self.drivers.len() {
            let requirement = self.drivers[index].requirement.clone();
            if self
                .in_flight
                .as_ref()
                .is_some_and(|driver| driver != &requirement.id)
            {
                continue;
            }
            let interrupted = self.in_flight.as_deref() == Some(&requirement.id);
            let action = if interrupted {
                Action::Reconcile
            } else {
                Action::Detect
            };
            let reply = self.call(backend, action, requirement.clone())?;
            self.observe(index, reply);
            if self.drivers[index].state == DriverState::Ready {
                let reply = self.call(backend, Action::Verify, requirement.clone())?;
                self.observe(index, reply);
            }
            if interrupted && self.drivers[index].state == DriverState::Installing {
                self.phase = Phase::Installing;
                self.persist(persist)?;
                return Ok(());
            }
            if interrupted {
                self.in_flight = None;
                self.persist(persist)?;
            }
            if matches!(
                self.drivers[index].state,
                DriverState::Missing | DriverState::Incompatible | DriverState::AccessDenied
            ) && approved
            {
                self.phase = Phase::Installing;
                self.in_flight = Some(requirement.id.clone());
                self.bump_revision()?;
                self.persist(persist)?;
                let reply = self.call(backend, Action::Install, requirement.clone())?;
                self.observe(index, reply);
                if self.drivers[index].state == DriverState::Installing {
                    self.phase = Phase::Installing;
                    self.persist(persist)?;
                    return Ok(());
                }
                // A successful installer exit is never proof of runtime usability.
                if self.drivers[index].state == DriverState::Ready {
                    let reply = self.call(backend, Action::Verify, requirement)?;
                    self.observe(index, reply);
                }
                self.in_flight = None;
                self.bump_revision()?;
                self.persist(persist)?;
            }
        }
        if self.drivers.len() > 1
            && self
                .drivers
                .iter()
                .all(|driver| driver.state == DriverState::Ready)
        {
            for index in 0..self.drivers.len() {
                let reply = self.call(
                    backend,
                    Action::Verify,
                    self.drivers[index].requirement.clone(),
                )?;
                self.observe(index, reply);
            }
        }
        self.phase = if self
            .drivers
            .iter()
            .all(|driver| driver.state == DriverState::Ready)
        {
            Phase::ReadyToActivate
        } else if self.drivers.iter().any(|driver| {
            matches!(
                driver.state,
                DriverState::Failed
                    | DriverState::Unsupported
                    | DriverState::Installing
                    | DriverState::Checking
            )
        }) {
            Phase::Failed
        } else if self
            .drivers
            .iter()
            .any(|driver| driver.state == DriverState::RebootRequired)
        {
            Phase::RebootRequired
        } else if self
            .drivers
            .iter()
            .any(|driver| driver.state == DriverState::ManualActionRequired)
        {
            Phase::ManualActionRequired
        } else if approved {
            Phase::Failed
        } else {
            Phase::AwaitingApproval
        };
        self.bump_revision()?;
        self.persist(persist)
    }

    fn bump_revision(&mut self) -> PackageResult<()> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or("plugin_revision_exhausted")?;
        Ok(())
    }

    fn persist(&mut self, save: &mut impl FnMut(&Self) -> PackageResult<()>) -> PackageResult<()> {
        self.bump_revision()?;
        save(self)
    }

    fn call(
        &self,
        backend: &mut impl DriverBackend,
        action: Action,
        requirement: DriverRequirement,
    ) -> PackageResult<HelperReply> {
        let request = HelperRequest {
            schema: SCHEMA,
            operation: self.id.clone(),
            action,
            requirement,
            runtime_identity: self.runtime_identity.clone(),
        };
        let reply = backend.request(self, &request)?;
        reply.validate_for(&request)?;
        Ok(reply)
    }

    fn observe(&mut self, index: usize, reply: HelperReply) {
        let driver = &mut self.drivers[index];
        driver.state = reply.state;
        driver.message = reply.message;
        driver.installed_version = reply.installed_version;
    }
}

/// Last verified observations. This does not run helpers or claim a live OS probe.
pub fn driver_management_view(
    drivers: &[Driver],
    sources: &[PluginSource],
) -> Vec<serde_json::Value> {
    drivers
        .iter()
        .map(|driver| {
            let publisher = sources
                .iter()
                .find(|source| driver.required_by.contains(&source.plugin))
                .map(|source| source.publisher.as_str())
                .unwrap_or("Unrecorded publisher");
            let offered = driver.requirement.offered_version.as_ref();
            let update = offered
                .map(|version| version.compare(driver.installed_version.as_deref()))
                .unwrap_or(amaterasu_module_sdk::drivers::DriverUpdate::Unknown);
            serde_json::json!({
                "id": driver.requirement.id,
                "name": driver.requirement.name,
                "publisher": publisher,
                "reason": driver.requirement.reason,
                "required_by": driver.required_by,
                "required_components": driver.required_components,
                "state": driver.state,
                "message": driver.message,
                "may_require_reboot": driver.requirement.may_require_reboot,
                "installed_version": driver.installed_version,
                "offered_version": offered.map(|version| &version.version),
                "update": update,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::drivers::Artifact;
    use std::collections::VecDeque;

    fn requirement() -> DriverRequirement {
        DriverRequirement {
            schema: 1,
            id: "example.driver".into(),
            name: "Example driver".into(),
            target: crate::current_target(),
            feature: "display".into(),
            reason: "Creates virtual displays".into(),
            compatibility: "display.v1".into(),
            may_require_reboot: true,
            offered_version: None,
            installer: Artifact {
                file: "installer.bin".into(),
                sha256: "a".repeat(64),
                size: 1,
            },
            payloads: vec![],
        }
    }

    fn operation() -> Operation {
        Operation::prepare(
            "b".repeat(64),
            vec![],
            "1000".into(),
            vec![("example.plugin".into(), requirement())],
        )
        .unwrap()
    }

    struct Installer {
        replies: VecDeque<(Action, DriverState)>,
        calls: Vec<Action>,
    }
    impl DriverBackend for Installer {
        fn request(
            &mut self,
            _operation: &Operation,
            request: &HelperRequest,
        ) -> PackageResult<HelperReply> {
            let (action, state) = self.replies.pop_front().expect("unexpected installer call");
            assert_eq!(action, request.action);
            self.calls.push(action);
            Ok(HelperReply {
                schema: 1,
                operation: request.operation.clone(),
                driver: request.requirement.id.clone(),
                state,
                installed_version: None,
                message: String::new(),
                cancellable: false,
            })
        }
    }
    fn installer(replies: &[(Action, DriverState)]) -> Installer {
        Installer {
            replies: replies.iter().copied().collect(),
            calls: vec![],
        }
    }

    #[test]
    fn signed_version_metadata_binds_approval_and_reports_only_known_ordering() {
        use amaterasu_module_sdk::drivers::{OfferedVersion, VersionScheme};
        let mut operation = operation();
        let previous = operation.approval_digest().unwrap();
        operation.drivers[0].requirement.offered_version = Some(OfferedVersion {
            version: "1.10".into(),
            scheme: VersionScheme::Numeric,
        });
        assert_ne!(previous, operation.approval_digest().unwrap());
        operation.drivers[0].installed_version = Some("1.9".into());
        let view = operation.management_view().unwrap();
        assert_eq!(view["drivers"][0]["update"], "available");
        assert_eq!(view["drivers"][0]["offered_version"], "1.10");
        operation.drivers[0].installed_version = Some("vendor-build".into());
        assert_eq!(
            operation.management_view().unwrap()["drivers"][0]["update"],
            "unknown"
        );
        let mut different = operation.drivers[0].requirement.clone();
        different.offered_version.as_mut().unwrap().version = "1.11".into();
        assert!(
            Operation::prepare(
                "b".repeat(64),
                vec![],
                "1000".into(),
                vec![
                    (
                        "first.plugin".into(),
                        operation.drivers[0].requirement.clone()
                    ),
                    ("second.plugin".into(), different),
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_requirements_keep_their_serialized_identity() {
        let value = serde_json::to_value(requirement()).unwrap();
        assert!(value.get("offered_version").is_none());
        let restored: DriverRequirement = serde_json::from_value(value).unwrap();
        assert_eq!(restored, requirement());
    }

    #[test]
    fn only_successful_activation_commits_driver_inventory() {
        use super::super::installed;
        let root = std::env::temp_dir().join(format!(
            "amaterasu-driver-inventory-{}",
            crate::random_identifier().unwrap()
        ));
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        let mut selected = installed::Selection::default();
        let mut op = operation();
        op.drivers[0].installed_version = Some("1.9".into());
        selected.pending = Some(op.clone());
        crate::write_json_atomically(&root.join("plugins/selection.json"), &selected).unwrap();
        installed::finish_trial(&root, true).unwrap();
        assert_eq!(
            installed::read(&root).unwrap().active_drivers[0]
                .installed_version
                .as_deref(),
            Some("1.9")
        );
        let mut selected = installed::read(&root).unwrap();
        op.drivers[0].installed_version = Some("2.0".into());
        selected.pending = Some(op);
        crate::write_json_atomically(&root.join("plugins/selection.json"), &selected).unwrap();
        installed::finish_trial(&root, false).unwrap();
        assert_eq!(
            installed::read(&root).unwrap().active_drivers[0]
                .installed_version
                .as_deref(),
            Some("1.9")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_driver_requires_exact_approval_and_verified_installation() {
        let mut operation = operation();
        let mut backend = installer(&[(Action::Detect, DriverState::Missing)]);
        operation
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::AwaitingApproval);
        let approval = operation.approval_digest().unwrap();
        assert!(
            operation
                .approve(operation.revision, "changed", &approval)
                .is_err()
        );
        operation
            .approve(operation.revision, &"b".repeat(64), &approval)
            .unwrap();
        let mut backend = installer(&[
            (Action::Detect, DriverState::Missing),
            (Action::Install, DriverState::Ready),
            (Action::Verify, DriverState::Ready),
        ]);
        let mut snapshots = vec![];
        operation
            .advance(true, &mut backend, &mut |record| {
                snapshots.push(record.in_flight.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(operation.phase, Phase::ReadyToActivate);
        assert!(snapshots.contains(&Some("example.driver".into())));
    }

    #[test]
    fn active_stream_never_calls_an_installer() {
        let mut operation = operation();
        let mut backend = installer(&[]);
        operation
            .advance(false, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::WaitingForIdle);
        assert!(backend.calls.is_empty());
    }

    #[test]
    fn successful_install_with_failed_runtime_access_never_becomes_ready() {
        let mut operation = operation();
        operation.phase = Phase::AwaitingApproval;
        operation
            .approve(
                operation.revision,
                &"b".repeat(64),
                &operation.approval_digest().unwrap(),
            )
            .unwrap();
        let mut backend = installer(&[
            (Action::Detect, DriverState::Missing),
            (Action::Install, DriverState::Ready),
            (Action::Verify, DriverState::AccessDenied),
        ]);
        operation
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::Failed);
    }

    #[test]
    fn interrupted_install_reconciles_before_any_retry() {
        let mut operation = operation();
        operation.in_flight = Some("example.driver".into());
        let mut backend = installer(&[(Action::Reconcile, DriverState::RebootRequired)]);
        operation
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::RebootRequired);
        assert!(operation.in_flight.is_none());
    }

    #[test]
    fn already_present_driver_is_verified_without_approval() {
        let mut operation = operation();
        let mut backend = installer(&[
            (Action::Detect, DriverState::Ready),
            (Action::Verify, DriverState::Ready),
        ]);
        operation
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::ReadyToActivate);
        assert!(operation.approved_revision.is_none());
    }

    #[test]
    fn shared_drivers_are_deduplicated_and_conflicts_rejected() {
        let requirements = vec![("a".into(), requirement()), ("b".into(), requirement())];
        let operation =
            Operation::prepare("b".repeat(64), vec![], "1000".into(), requirements).unwrap();
        assert_eq!(operation.drivers.len(), 1);
        assert_eq!(operation.drivers[0].required_by, vec!["a", "b"]);
        let mut conflicting = requirement();
        conflicting.compatibility = "display.v2".into();
        assert!(
            Operation::prepare(
                "b".repeat(64),
                vec![],
                "1000".into(),
                vec![("a".into(), requirement()), ("b".into(), conflicting)]
            )
            .is_err()
        );
    }

    #[test]
    fn persisted_operation_survives_restart_and_stale_writes_fail() {
        let directory = std::env::temp_dir().join(format!(
            "amaterasu-driver-store-{}",
            crate::random_identifier().unwrap()
        ));
        let store = OperationStore::new(directory.clone()).unwrap();
        let mut operation = operation();
        store.create(&operation).unwrap();
        let original = operation.revision;
        operation.phase = Phase::AwaitingApproval;
        operation.bump_revision().unwrap();
        store.save((&operation.id, original), &operation).unwrap();
        let loaded = OperationStore::new(directory.clone())
            .unwrap()
            .read()
            .unwrap()
            .unwrap();
        assert_eq!(loaded.phase, Phase::AwaitingApproval);
        assert_eq!(
            loaded.approval_digest().unwrap(),
            operation.approval_digest().unwrap()
        );
        assert!(store.save((&operation.id, original), &operation).is_err());
        assert!(store.create(&operation).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn changed_payload_invalidates_an_existing_approval() {
        let mut operation = operation();
        operation.phase = Phase::AwaitingApproval;
        operation
            .approve(
                operation.revision,
                &"b".repeat(64),
                &operation.approval_digest().unwrap(),
            )
            .unwrap();
        operation.drivers[0].requirement.installer.sha256 = "c".repeat(64);
        let mut backend = installer(&[]);
        assert!(
            operation
                .advance(true, &mut backend, &mut |_| Ok(()))
                .is_err()
        );
        assert!(backend.calls.is_empty());
    }

    #[test]
    fn unchanged_coordinator_accepts_an_independent_installer_implementation() {
        struct Replacement;
        impl DriverBackend for Replacement {
            fn request(
                &mut self,
                _operation: &Operation,
                request: &HelperRequest,
            ) -> PackageResult<HelperReply> {
                assert!(matches!(request.action, Action::Detect | Action::Verify));
                Ok(HelperReply {
                    schema: SCHEMA,
                    operation: request.operation.clone(),
                    driver: request.requirement.id.clone(),
                    state: DriverState::Ready,
                    installed_version: Some("replacement-1".into()),
                    message: "Supplied by another implementation".into(),
                    cancellable: true,
                })
            }
        }
        let mut operation = operation();
        operation
            .advance(true, &mut Replacement, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::ReadyToActivate);
        assert_eq!(
            operation.drivers[0].installed_version.as_deref(),
            Some("replacement-1")
        );
    }
    #[test]
    fn async_install_retains_intent_until_reconciled_and_verified() {
        let mut operation = operation();
        operation.phase = Phase::AwaitingApproval;
        operation
            .approve(
                operation.revision,
                &"b".repeat(64),
                &operation.approval_digest().unwrap(),
            )
            .unwrap();
        let mut backend = installer(&[
            (Action::Detect, DriverState::Missing),
            (Action::Install, DriverState::Installing),
        ]);
        operation
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(operation.phase, Phase::Installing);
        assert!(operation.in_flight.is_some());
        assert!(
            operation
                .cancel(operation.revision, &"b".repeat(64))
                .is_err()
        );
        let mut recovered: Operation =
            serde_json::from_slice(&serde_json::to_vec(&operation).unwrap()).unwrap();
        let mut backend = installer(&[(Action::Reconcile, DriverState::Installing)]);
        recovered
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(recovered.phase, Phase::Installing);
        assert!(recovered.in_flight.is_some());
        let mut backend = installer(&[
            (Action::Reconcile, DriverState::Ready),
            (Action::Verify, DriverState::Ready),
        ]);
        recovered
            .advance(true, &mut backend, &mut |_| Ok(()))
            .unwrap();
        assert_eq!(recovered.phase, Phase::ReadyToActivate);
        assert!(recovered.in_flight.is_none());
    }
    #[test]
    fn unfinished_package_preparation_cannot_turn_into_an_empty_activation() {
        let mut operation =
            Operation::prepare("b".repeat(64), vec![], "1000".into(), vec![]).unwrap();
        operation.preparation = Some(Preparation {
            repository: "example".into(),
            manifest: "a".repeat(64),
        });
        let mut backend = installer(&[]);
        assert!(
            operation
                .advance(true, &mut backend, &mut |_| Ok(()))
                .is_err()
        );
        assert_eq!(operation.phase, Phase::Checking);
    }
    #[test]
    fn interrupted_second_driver_does_not_probe_another_driver_mid_install() {
        let mut op = operation();
        let mut other = op.drivers[0].clone();
        other.requirement.id = "example.second".into();
        other.state = DriverState::Installing;
        op.drivers[0].state = DriverState::Ready;
        op.drivers.push(other);
        op.in_flight = Some("example.second".into());
        op.phase = Phase::Installing;
        let mut backend = installer(&[(Action::Reconcile, DriverState::Installing)]);
        op.advance(true, &mut backend, &mut |_| Ok(())).unwrap();
        assert_eq!(backend.calls, vec![Action::Reconcile]);
        assert_eq!(op.phase, Phase::Installing);
    }
}
