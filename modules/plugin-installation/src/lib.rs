//! Replaceable installation coordinator. All storage/network work runs off tick.
pub mod adapters;
pub mod broker;
#[cfg(target_os = "linux")]
pub mod builtin_drivers;
pub mod helper;
mod security;
mod transport;
use amaterasu_module_sdk::network::Network;
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient,
    drivers::{INSTALLATION_SERVICE, installation_methods},
};
use amaterasu_package::{
    BootstrapConfig, ReleaseManifest,
    plugins::{
        installed,
        operation::{Operation, OperationStore, Phase},
        preparation::{InstallationContext, prepare},
    },
    read_json,
    repository::RepositoryStore,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

struct Installer {
    runtime: RuntimeClient,
    root: PathBuf,
    repositories: PathBuf,
    state: OperationStore,
    network: Network,
    broker: PathBuf,
    job: Option<JoinHandle<ModuleResult<()>>>,
    cancelled: Arc<AtomicBool>,
    stopping: bool,
    activation_requested: bool,
    next_work: std::time::Instant,
    #[cfg(target_os = "linux")]
    builtin_drivers: builtin_drivers::Inventory,
}
impl Installer {
    fn base(&self) -> ModuleResult<String> {
        let Response::Result { payload } =
            self.runtime.send(Command::GetRunningPackageDigest {})?
        else {
            return Err("plugin_host_context_unavailable".into());
        };
        payload["digest"]
            .as_str()
            .map(str::to_owned)
            .ok_or("plugin_host_context_unavailable".into())
    }
    fn revision(&self) -> ModuleResult<String> {
        installed::revision(&self.root, &self.base()?)
    }
    fn status(&self) -> ModuleResult<Value> {
        if !cfg!(any(windows, target_os = "linux")) {
            return Ok(json!({"ok":true,"supported":false}));
        }
        let mut operation = self.state.observe()?;
        if let Some(op) = &mut operation {
            let selected = installed::read(&self.root)?;
            if op.phase == Phase::Activating && selected.pending.is_none() && self.job.is_none() {
                let previous = op.revision;
                op.phase = if selected.active == op.sources && selected.error.is_none() {
                    Phase::Ready
                } else {
                    Phase::Failed
                };
                op.error = selected.error;
                op.revision += 1;
                self.state.save((&op.id, previous), op)?;
            }
        }
        let mut view = operation
            .as_ref()
            .map(Operation::management_view)
            .transpose()?;
        let revision = self.revision()?;
        // A committed operation records the revision it was reviewed against;
        // report the new installation revision only after completion.
        if let Some(view) = &mut view
            && operation
                .as_ref()
                .is_some_and(|op| matches!(op.phase, Phase::Ready | Phase::Failed))
        {
            view["installation_revision"] = revision.clone().into();
        }
        let selected = installed::read(&self.root)?;
        let drivers = amaterasu_package::plugins::operation::driver_management_view(
            &selected.active_drivers,
            &selected.active,
        );
        #[cfg(target_os = "linux")]
        let builtin_drivers = self.builtin_drivers.observe();
        #[cfg(not(target_os = "linux"))]
        let builtin_drivers: Vec<Value> = Vec::new();
        Ok(json!({
            "ok": true,
            "supported": true,
            "installation_revision": revision,
            "builtin_drivers": builtin_drivers,
            "drivers": drivers,
            "operation": view,
        }))
    }
    fn receive(&mut self, action: &str, payload: &Value) -> ModuleResult<Value> {
        if action == "status" {
            return self.status();
        }
        if !cfg!(any(windows, target_os = "linux")) {
            return Err("plugin_platform_unsupported".into());
        }
        if self.stopping {
            return Err("plugin_shutting_down".into());
        }
        if self.job.is_some() {
            if action == "cancel" {
                let mut operation = self.state.observe()?.ok_or("plugin_operation_missing")?;
                if payload["operation"] != operation.id
                    || request_revision(payload)? != operation.revision
                    || payload["installation_revision"] != operation.installation_revision
                {
                    return Err("plugin_revision_conflict".into());
                }
                let expected = operation.revision;
                operation.cancel(expected, &operation.installation_revision.clone())?;
                self.state.save((&operation.id, expected), &operation)?;
                self.cancelled.store(true, Ordering::Release);
                let _ = self.runtime.send(Command::HostPluginSetup { hold: false });
                return Ok(json!({"ok":true}));
            }
            return Err("plugin_setup_in_progress".into());
        }
        let revision = self.revision()?;
        if payload["installation_revision"] != revision {
            return Err("plugin_revision_conflict".into());
        }
        if action == "prepare" {
            let repository = payload["repository"]
                .as_str()
                .ok_or("invalid_request")?
                .to_owned();
            let digest = payload["manifest"]
                .as_str()
                .ok_or("invalid_request")?
                .to_owned();
            let mut placeholder = Operation::prepare(
                revision,
                Vec::new(),
                helper::runtime_identity()?,
                Vec::new(),
            )?;
            placeholder.preparation = Some(amaterasu_package::plugins::operation::Preparation {
                repository,
                manifest: digest,
            });
            self.state.create(&placeholder)?;
            self.start_preparation(placeholder)?;
            return Ok(json!({"ok":true}));
        }
        let mut operation = self.state.observe()?.ok_or("plugin_operation_missing")?;
        let expected = operation.revision;
        if payload["operation"] != operation.id {
            return Err("plugin_operation_missing".into());
        }
        let supplied = request_revision(payload)?;
        operation.require_current(supplied, &revision)?;
        match action {
            "approve" => {
                operation.approve(
                    supplied,
                    &revision,
                    payload["approval"].as_str().ok_or("invalid_request")?,
                )?;
                self.state.save((&operation.id, expected), &operation)?;
                self.approval_job(operation);
                return self.status();
            }
            "cancel" => {
                operation.cancel(supplied, &revision)?;
                let _ = self.runtime.send(Command::HostPluginSetup { hold: false });
            }
            "retry" => {
                operation.recheck(supplied, &revision)?;
                if operation.approved_revision.is_some() {
                    self.state.save((&operation.id, expected), &operation)?;
                    self.approval_job(operation);
                    return self.status();
                }
            }
            "activate" => {
                if operation.phase != Phase::ReadyToActivate {
                    return Err("plugin_drivers_not_ready".into());
                }
                if !self.barrier_ready()? {
                    return Err("plugin_waiting_for_idle".into());
                }
                operation.phase = Phase::Activating;
                operation.revision += 1;
                self.state.save((&operation.id, expected), &operation)?;
                #[cfg(windows)]
                {
                    let broker = self.broker.clone();
                    self.job = Some(thread::spawn(move || {
                        operation.phase = Phase::ReadyToActivate;
                        broker::client(&broker, &broker::Request::Stage { operation }).map(|_| ())
                    }));
                }
                #[cfg(not(windows))]
                {
                    let root = self.root.clone();
                    let base = self.base()?;
                    self.job = Some(thread::spawn(move || {
                        operation.phase = Phase::ReadyToActivate;
                        installed::stage(&root, &base, &operation)
                    }));
                }
                return self.status();
            }
            _ => return Err("invalid_request".into()),
        }
        self.state.save((&operation.id, expected), &operation)?;
        self.status()
    }
    fn approval_job(&mut self, operation: Operation) {
        let broker = self.broker.clone();
        self.job = Some(thread::spawn(move || {
            broker::client(&broker, &broker::Request::Approve { operation }).map(|_| ())
        }));
    }
    fn start_preparation(&mut self, placeholder: Operation) -> ModuleResult<()> {
        let preparation = placeholder
            .preparation
            .as_ref()
            .ok_or("plugin_preparation_missing")?;
        let repository = preparation.repository.clone();
        let digest = preparation.manifest.clone();
        let revision = placeholder.installation_revision.clone();
        if revision != self.revision()? {
            return Err("plugin_revision_conflict".into());
        }
        let root = self.root.clone();
        let repositories = self.repositories.clone();
        let network = self.network.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancelled = cancelled.clone();
        let state = self
            .runtime
            .runtime_state_root()?
            .join("plugin-installation");
        let base = self.base()?;
        self.job = Some(thread::spawn(move || {
            let store = RepositoryStore::new(repositories)?;
            let approved = store.approved(&repository)?;
            let transport =
                transport::Transport::new(&approved.location, network, cancelled.clone());
            if store
                .catalog(
                    &repository,
                    &amaterasu_package::current_target(),
                    amaterasu_package::unix_timestamp(),
                )
                .is_err()
            {
                amaterasu_package::repository::refresh_catalog(
                    &store,
                    &repository,
                    &transport,
                    &amaterasu_package::current_target(),
                    amaterasu_package::unix_timestamp(),
                )?;
            }
            let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
            let manifest: ReleaseManifest = read_json(
                &amaterasu_package::package_directory(&root, &base)?.join("manifest.json"),
            )?;
            let selected = installed::read(&root)?;
            let context = InstallationContext {
                bootstrap: &bootstrap,
                base_digest: &base,
                components: &manifest.components,
                installation_revision: &revision,
                runtime_identity: &helper::runtime_identity()?,
                installed: &selected.active,
            };
            let mut operation = prepare(
                &context,
                &store,
                &repository,
                &digest,
                &transport,
                &cancelled,
                amaterasu_package::unix_timestamp(),
            )?;
            operation.id = placeholder.id;
            operation.revision = placeholder.revision + 1;
            OperationStore::new(state)?.save((&operation.id, placeholder.revision), &operation)
        }));
        Ok(())
    }
    fn barrier_ready(&self) -> ModuleResult<bool> {
        match self.runtime.send(Command::HostPluginSetup { hold: true })? {
            Response::Result { payload } => Ok(payload["ready"] == true),
            Response::Error { code, .. } => Err(code),
            _ => Err("plugin_barrier_unavailable".into()),
        }
    }
}
impl ManagedModule for Installer {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings: Value = runtime.settings().map_err(|e| e.to_string())?;
        let Response::Result { payload } = runtime.send(Command::GetPackageRoot {})? else {
            return Err("plugin_root_unavailable".into());
        };
        let root = PathBuf::from(
            payload["package_root"]
                .as_str()
                .ok_or("plugin_root_unavailable")?,
        );
        let state = OperationStore::new(runtime.runtime_state_root()?.join("plugin-installation"))?;
        if let Some(mut operation) = state.observe()?
            && (matches!(
                operation.phase,
                Phase::RebootRequired | Phase::ManualActionRequired
            ) || operation.phase == Phase::Failed && operation.in_flight.is_some())
        {
            let previous = operation.revision;
            operation.recheck(previous, &operation.installation_revision.clone())?;
            state.save((&operation.id, previous), &operation)?;
        }
        let network = Network::from_settings(&runtime, &settings)?;
        for method in installation_methods() {
            runtime.require_success(Command::RegisterApi { method })?;
        }
        let broker = PathBuf::from(settings["broker_endpoint"].as_str().unwrap_or(
            if cfg!(windows) {
                r"\\.\pipe\AmaterasuPluginInstaller-v1"
            } else {
                "/run/amaterasu-plugin-installer/control.sock"
            },
        ));
        Ok(Self {
            runtime,
            repositories: settings["repository_directory"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("repositories")),
            root,
            state,
            network,
            broker,
            job: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            stopping: false,
            activation_requested: false,
            next_work: std::time::Instant::now(),
            #[cfg(target_os = "linux")]
            builtin_drivers: builtin_drivers::Inventory::from_settings(&settings),
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if self.job.as_ref().is_some_and(JoinHandle::is_finished)
            && let Err(error) = self
                .job
                .take()
                .unwrap()
                .join()
                .map_err(|_| "plugin_worker_panicked")?
            && let Some(mut operation) = self.state.observe()?
        {
            let previous = operation.revision;
            operation.error = Some(bounded_error(error));
            operation.phase = if self.cancelled.load(Ordering::Acquire) {
                Phase::Cancelled
            } else {
                Phase::Failed
            };
            operation.revision += 1;
            self.state.save((&operation.id, previous), &operation)?;
        }
        if self.job.is_none()
            && !self.stopping
            && let Some(operation) = self.state.observe()?
            && operation.phase == Phase::Checking
            && operation.preparation.is_some()
        {
            self.start_preparation(operation)?;
        }
        if self.job.is_none()
            && let Some(operation) = self.state.observe()?
            && operation.phase == Phase::Activating
            && installed::read(&self.root)?.pending.is_none()
        {
            self.status()?;
        }
        if self.job.is_none()
            && !self.stopping
            && !self.activation_requested
            && let Some(operation) = self.state.observe()?
            && operation.phase == Phase::Activating
            && installed::read(&self.root)?.pending.is_some()
            && self.barrier_ready()?
        {
            self.runtime
                .request_host_lifecycle(amaterasu_module_sdk::HostLifecycleAction::Restart)?;
            self.activation_requested = true;
        }
        if self.job.is_none() && !self.stopping && std::time::Instant::now() >= self.next_work {
            self.next_work = std::time::Instant::now() + std::time::Duration::from_millis(500);
            if let Some(operation) = self.state.observe()?
                && operation.phase == Phase::ReadyToActivate
                && self.barrier_ready()?
            {
                let request = json!({"operation":operation.id,"revision":operation.revision.to_string(),"installation_revision":operation.installation_revision});
                if let Err(error) = self.receive("activate", &request) {
                    let mut operation = operation;
                    let previous = operation.revision;
                    operation.phase = Phase::Failed;
                    operation.error = Some(bounded_error(error));
                    operation.revision += 1;
                    self.state.save((&operation.id, previous), &operation)?;
                }
            }
            if self.state.observe()?.as_ref().is_some_and(|operation| {
                operation.in_flight.is_none()
                    && matches!(
                        operation.phase,
                        Phase::AwaitingApproval
                            | Phase::RebootRequired
                            | Phase::ManualActionRequired
                            | Phase::Failed
                            | Phase::Cancelled
                            | Phase::Ready
                    )
            }) {
                let _ = self.runtime.send(Command::HostPluginSetup { hold: false });
            }
            if let Some(operation) = self.state.observe()?
                && matches!(
                    operation.phase,
                    Phase::Checking | Phase::WaitingForIdle | Phase::Installing
                )
                && operation.preparation.is_none()
                && self.barrier_ready()?
            {
                let root = self.root.clone();
                let repositories = self.repositories.clone();
                let broker = self.broker.clone();
                let cancelled = self.cancelled.clone();
                let directory = self
                    .runtime
                    .runtime_state_root()?
                    .join("plugin-installation");
                self.job = Some(thread::spawn(move || {
                    let store = OperationStore::new(directory)?;
                    let mut operation = operation;
                    let mut revision = operation.revision;
                    let mut backend = helper::Backend {
                        root,
                        broker,
                        repositories,
                        cancelled,
                    };
                    operation.advance(true, &mut backend, &mut |op| {
                        store.save((&op.id, revision), op)?;
                        revision = op.revision;
                        Ok(())
                    })
                }));
            }
        }
        if let Response::Request {
            id,
            caller,
            service,
            payload,
            ..
        } = self.runtime.send(Command::Poll {})?
        {
            let result = if caller != "amaterasu.management" || service != INSTALLATION_SERVICE {
                Err("permission_denied".into())
            } else {
                amaterasu_module_sdk::operation_api::decode(payload, &installation_methods())
                    .and_then(|payload| {
                        let action = payload["action"].as_str().ok_or("invalid_request")?;
                        self.receive(action, &payload)
                    })
            };
            let payload = result.unwrap_or_else(|code: String| json!({"ok":false,"code":code}));
            self.runtime.require_success(Command::Complete {
                request: id,
                payload,
            })?;
        }
        Ok(())
    }
    fn prepare_shutdown(&mut self, _: u64) -> ModuleResult<()> {
        self.stopping = true;
        self.cancelled.store(true, Ordering::Release);
        Ok(())
    }
    fn quiesce(&mut self) -> ModuleResult<bool> {
        if self.job.as_ref().is_some_and(|job| !job.is_finished()) {
            return Ok(false);
        }
        if let Some(job) = self.job.take() {
            let _ = job.join().map_err(|_| "plugin_worker_panicked")?;
        }
        Ok(true)
    }
}
amaterasu_module_sdk::export_module!(
    amaterasu_module_sdk::service_worker::ServiceWorker<Installer>
);

fn bounded_error(mut message: String) -> String {
    message.retain(|character| !character.is_control());
    if message.len() > 4096 {
        let mut end = 4096;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

fn request_revision(payload: &Value) -> ModuleResult<u64> {
    payload["revision"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or("invalid_request".into())
}
