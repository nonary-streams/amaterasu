//! Separately provisioned machine service. Only pinned host processes may submit
//! finite, signed installation reviews; browser values never select executable paths.
use amaterasu_module_sdk::drivers::HelperRequest;
#[cfg(any(windows, target_os = "linux"))]
use amaterasu_module_sdk::drivers::{Action, HelperReply};
#[cfg(any(windows, target_os = "linux"))]
use amaterasu_package::plugins::installed;
use amaterasu_package::{PackageResult, plugins::operation::Operation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(any(windows, target_os = "linux"))]
use serde_json::json;
#[cfg(any(windows, target_os = "linux"))]
use std::{
    collections::BTreeMap,
    sync::{Mutex, atomic::Ordering},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub repository_directory: Option<PathBuf>,
    pub root: PathBuf,
    pub endpoint: PathBuf,
    pub approvals: PathBuf,
    pub host_sha256: String,
    pub runtime_identity: String,
    pub publishers: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Approve {
        operation: Operation,
    },
    Execute {
        operation: String,
        request: HelperRequest,
    },
    Stage {
        operation: Operation,
    },
}
#[cfg(any(windows, target_os = "linux"))]
struct Job {
    result: Option<PackageResult<HelperReply>>,
}
#[cfg(any(windows, target_os = "linux"))]
type Jobs = Arc<Mutex<BTreeMap<String, Job>>>;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(any(windows, target_os = "linux"))]
struct Receipt {
    request: HelperRequest,
    result: Option<PackageResult<HelperReply>>,
}
#[cfg(any(windows, target_os = "linux"))]
fn receipt_path(policy: &Policy, request: &HelperRequest) -> PackageResult<PathBuf> {
    let approval = approval_path(policy, &request.operation)?;
    Ok(approval.with_file_name(format!(
        "{}-{}.receipt.json",
        request.operation,
        amaterasu_package::sha256(request.requirement.id.as_bytes())
    )))
}
#[cfg(any(windows, target_os = "linux"))]
fn current_revision(policy: &Policy, operation: &Operation) -> PackageResult<()> {
    let selection: amaterasu_package::ActivationState =
        amaterasu_package::read_json(&policy.root.join("selection.json"))?;
    if installed::revision(
        &policy.root,
        selection.active.as_deref().ok_or("plugin_base_missing")?,
    )? != operation.installation_revision
    {
        return Err("plugin_revision_conflict".into());
    }
    Ok(())
}
#[cfg(any(windows, target_os = "linux"))]
fn verify(policy: &Policy, operation: &Operation, elevated: bool) -> PackageResult<()> {
    import_sources(policy, operation)?;
    operation.validate()?;
    current_revision(policy, operation)?;
    if operation.runtime_identity != policy.runtime_identity {
        return Err("driver_runtime_identity_changed".into());
    }
    let bootstrap = amaterasu_package::read_json(&policy.root.join("bootstrap.json"))?;
    let store =
        amaterasu_package::repository::RepositoryStore::new(policy.root.join("repositories"))?;
    store.with_active_keys(|_| {
        let plugins =
            installed::verify_sources_locked(&policy.root, &operation.sources, &bootstrap)?;
        if elevated
            && operation
                .drivers
                .iter()
                .filter(|driver| driver.state != amaterasu_module_sdk::drivers::DriverState::Ready)
                .any(|driver| {
                    !plugins.iter().any(|plugin| {
                        policy.publishers.contains(&plugin.source.publisher)
                            && plugin.manifest.drivers.contains(&driver.requirement)
                    })
                })
        {
            return Err("driver_publisher_not_approved".into());
        }
        let requirements = plugins
            .iter()
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .drivers
                    .iter()
                    .map(|driver| (plugin.manifest.id.clone(), driver.clone()))
            })
            .collect();
        let mut derived = Operation::prepare(
            operation.installation_revision.clone(),
            operation.sources.clone(),
            operation.runtime_identity.clone(),
            requirements,
        )?;
        derived.id = operation.id.clone();
        if derived.approval_digest() != operation.approval_digest() {
            return Err("driver_requirements_changed".into());
        }
        Ok(())
    })
}
#[cfg(any(windows, target_os = "linux", test))]
fn approval_path(policy: &Policy, id: &str) -> PackageResult<PathBuf> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
    {
        return Err("driver_invalid_operation".into());
    }
    Ok(policy.approvals.join(format!("{id}.json")))
}
#[cfg(any(windows, target_os = "linux"))]
fn prune_receipts(policy: &Policy, current: &str) -> PackageResult<()> {
    let mut records = Vec::new();
    for entry in std::fs::read_dir(&policy.approvals).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if id == current {
            continue;
        }
        if let Ok(operation) = amaterasu_package::read_json::<Operation>(&entry.path())
            && operation.id == id
        {
            records.push((
                entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .map_err(|e| e.to_string())?,
                id.to_owned(),
                entry.path(),
            ));
        }
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.0));
    for (_, id, path) in records.into_iter().skip(31) {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(&policy.approvals).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            if entry.file_type().map_err(|e| e.to_string())?.is_file()
                && name.to_str().is_some_and(|name| {
                    name.starts_with(&format!("{id}-")) && name.ends_with(".receipt.json")
                })
            {
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
#[cfg(any(windows, target_os = "linux"))]
fn handle(policy: &Policy, request: Request, jobs: &Jobs) -> PackageResult<Value> {
    match request {
        Request::Approve { operation } => {
            verify(policy, &operation, true)?;
            if operation.approved_revision.as_deref() != Some(operation.approval_digest()?.as_str())
            {
                return Err("driver_approval_missing".into());
            }
            amaterasu_package::write_json_atomically(
                &approval_path(policy, &operation.id)?,
                &operation,
            )?;
            jobs.lock()
                .map_err(|_| "driver_jobs_unavailable")?
                .retain(|key, job| {
                    !key.starts_with(&format!("{}:", operation.id))
                        || !matches!(job.result, Some(Err(_)))
                });
            prune_receipts(policy, &operation.id)?;
            Ok(json!({"ok":true}))
        }
        Request::Stage { operation } => stage_plugin(policy, operation),
        Request::Execute { operation, request } => {
            if !matches!(request.action, Action::Install | Action::Reconcile) {
                return Err("driver_invalid_machine_action".into());
            }
            let approved: Operation =
                amaterasu_package::read_json(&approval_path(policy, &operation)?)?;
            if request.operation != approved.id
                || request.runtime_identity != approved.runtime_identity
                || !approved
                    .drivers
                    .iter()
                    .any(|driver| driver.requirement == request.requirement)
            {
                return Err("driver_stale_approval".into());
            }
            let key = format!("{}:{}", operation, request.requirement.id);
            let mut state = jobs.lock().map_err(|_| "driver_jobs_unavailable")?;
            if let Some(job) = state.get(&key) {
                if job.result.is_none() {
                    return serde_json::to_value(super::helper::reply(
                        &request,
                        amaterasu_module_sdk::drivers::DriverState::Installing,
                        "Driver installation is running",
                    ))
                    .map_err(|e| e.to_string());
                }
                if request.action == Action::Reconcile {
                    return serde_json::to_value(job.result.clone().unwrap()?)
                        .map_err(|e| e.to_string());
                }
                state.remove(&key);
            } else if request.action == Action::Reconcile {
                let path = receipt_path(policy, &request)?;
                if path.exists() {
                    let receipt: Receipt = amaterasu_package::read_json(&path)?;
                    if receipt.request.requirement != request.requirement
                        || receipt.request.runtime_identity != request.runtime_identity
                    {
                        return Err("driver_stale_receipt".into());
                    }
                    if receipt.request.action == Action::Install
                        && let Some(Ok(reply)) = receipt.result
                    {
                        return serde_json::to_value(reply).map_err(|e| e.to_string());
                    }
                }
            }
            if state.len() >= 32 {
                let retired = state
                    .iter()
                    .find(|(_, value)| value.result.is_some())
                    .map(|(key, _)| key.clone());
                if let Some(key) = retired {
                    state.remove(&key);
                } else {
                    return Err("driver_capacity_exhausted".into());
                }
            }
            verify(policy, &approved, true)?;
            let bootstrap = amaterasu_package::read_json(&policy.root.join("bootstrap.json"))?;
            let store = amaterasu_package::repository::RepositoryStore::new(
                policy.root.join("repositories"),
            )?;
            let source = store.with_active_keys(|_| {
                let plugins =
                    installed::verify_sources_locked(&policy.root, &approved.sources, &bootstrap)?;
                plugins
                    .into_iter()
                    .find(|plugin| {
                        policy.publishers.contains(&plugin.source.publisher)
                            && plugin.manifest.drivers.contains(&request.requirement)
                    })
                    .map(|plugin| plugin.directory)
                    .ok_or("driver_requirement_not_signed".into())
            })?;
            let receipt = receipt_path(policy, &request)?;
            amaterasu_package::write_json_atomically(
                &receipt,
                &Receipt {
                    request: request.clone(),
                    result: None,
                },
            )?;
            state.insert(key.clone(), Job { result: None });
            let jobs = jobs.clone();
            let initial = super::helper::reply(
                &request,
                amaterasu_module_sdk::drivers::DriverState::Installing,
                "Driver installation is running",
            );
            let root = policy.root.clone();
            let staging = policy.approvals.clone();
            let live_approvals = policy.repository_directory.clone();
            let reviewed_policy = policy.clone();
            std::thread::spawn(move || {
                let reply = (|| {
                    let bootstrap = amaterasu_package::read_json(&root.join("bootstrap.json"))?;
                    let store = amaterasu_package::repository::RepositoryStore::new(
                        root.join("repositories"),
                    )?;
                    store.with_active_keys(|_| {
                        installed::verify_sources_locked(&root, &approved.sources, &bootstrap)?;
                        current_revision(&reviewed_policy, &approved)?;
                        if let Some(directory) = &live_approvals {
                            let live = amaterasu_package::repository::RepositoryStore::new(
                                directory.clone(),
                            )?;
                            live.with_active_keys(|_| {
                                installed::verify_sources_from_store(
                                    &live,
                                    &approved.sources,
                                    &bootstrap,
                                )?;
                                super::helper::run_protected(&source, &request, &staging)
                            })
                        } else {
                            super::helper::run_protected(&source, &request, &staging)
                        }
                    })
                })();
                let persisted = amaterasu_package::write_json_atomically(
                    &receipt,
                    &Receipt {
                        request: request.clone(),
                        result: Some(reply.clone()),
                    },
                );
                let reply = match persisted {
                    Ok(()) => reply,
                    Err(error) => Err(format!("driver_receipt_failed:{error}")),
                };
                if let Ok(mut state) = jobs.lock() {
                    state.insert(
                        key,
                        Job {
                            result: Some(reply),
                        },
                    );
                }
            });
            serde_json::to_value(initial).map_err(|e| e.to_string())
        }
    }
}
#[cfg(target_os = "linux")]
fn stage_plugin(_policy: &Policy, _operation: Operation) -> PackageResult<Value> {
    Err("plugin_stage_requires_runtime_account".into())
}
#[cfg(windows)]
fn stage_plugin(policy: &Policy, operation: Operation) -> PackageResult<Value> {
    verify(policy, &operation, false)?;
    if operation.phase != amaterasu_package::plugins::operation::Phase::ReadyToActivate
        || operation
            .drivers
            .iter()
            .any(|driver| driver.state != amaterasu_module_sdk::drivers::DriverState::Ready)
    {
        return Err("driver_not_ready".into());
    }
    if operation.approved_revision.is_some() {
        let approved: Operation =
            amaterasu_package::read_json(&approval_path(policy, &operation.id)?)?;
        if approved.approval_digest() != operation.approval_digest() {
            return Err("driver_stale_approval".into());
        }
    }
    let selection: amaterasu_package::ActivationState =
        amaterasu_package::read_json(&policy.root.join("selection.json"))?;
    if let Some(directory) = &policy.repository_directory {
        installed::approval_store(&policy.root, directory.clone())?;
    }
    installed::stage(
        &policy.root,
        selection.active.as_deref().ok_or("plugin_base_missing")?,
        &operation,
    )?;
    Ok(json!({"ok":true}))
}
#[cfg(target_os = "linux")]
pub fn serve(policy: Policy, stop: Arc<AtomicBool>) -> PackageResult<()> {
    use std::{
        io::{Read, Write},
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };
    if unsafe { libc::geteuid() } != 0 {
        return Err("driver_service_requires_root".into());
    }
    amaterasu_package::create_machine_private_directory(&policy.approvals)?;
    super::security::machine_private(&policy.approvals)?;
    let parent = policy.endpoint.parent().ok_or("driver_invalid_endpoint")?;
    super::security::real_path(parent)?;
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = std::fs::metadata(parent).map_err(|e| e.to_string())?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("driver_endpoint_parent_unprotected".into());
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&policy.endpoint) {
            if metadata.uid() != 0 || !metadata.file_type().is_socket() {
                return Err("driver_endpoint_exists".into());
            }
            match std::os::unix::net::UnixStream::connect(&policy.endpoint) {
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(&policy.endpoint).map_err(|e| e.to_string())?
                }
                _ => return Err("driver_endpoint_in_use".into()),
            }
        }
    }
    let listener = UnixListener::bind(&policy.endpoint).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&policy.endpoint, std::fs::Permissions::from_mode(0o666))
        .map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let jobs = Arc::new(Mutex::new(BTreeMap::new()));
    while !stop.load(Ordering::Acquire) {
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        let result = (|| {
            use std::os::fd::AsRawFd;
            let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
            let mut length = std::mem::size_of_val(&peer) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut peer as *mut libc::ucred).cast(),
                    &mut length,
                )
            } != 0
                || peer.uid.to_string() != policy.runtime_identity
            {
                return Err("driver_peer_denied".into());
            }
            let command =
                std::fs::read(format!("/proc/{}/cmdline", peer.pid)).map_err(|e| e.to_string())?;
            let args: Vec<_> = command
                .split(|byte| *byte == 0)
                .filter_map(|part| std::str::from_utf8(part).ok())
                .collect();
            if args.get(1) != Some(&"service-user") || args.get(2).copied() != policy.root.to_str()
            {
                return Err("driver_host_instance_denied".into());
            }
            let image =
                std::fs::read(format!("/proc/{}/exe", peer.pid)).map_err(|e| e.to_string())?;
            if amaterasu_package::sha256(&image) != policy.host_sha256 {
                return Err("driver_host_changed".into());
            }
            let mut bytes = Vec::new();
            (&mut stream)
                .take(256 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 256 * 1024 {
                return Err("driver_request_too_large".into());
            }
            handle(
                &policy,
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                &jobs,
            )
        })();
        let reply = result.unwrap_or_else(|code: String| json!({"ok":false,"code":code}));
        let _ = stream.write_all(&serde_json::to_vec(&reply).map_err(|e| e.to_string())?);
    }
    let _ = std::fs::remove_file(&policy.endpoint);
    Ok(())
}
#[cfg(target_os = "linux")]
pub fn client(endpoint: &Path, request: &Request) -> PackageResult<Value> {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
    };
    let mut stream = UnixStream::connect(endpoint).map_err(|_| "driver_service_not_provisioned")?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    {
        use std::os::fd::AsRawFd;
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&peer) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut peer as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
            || peer.uid != 0
        {
            return Err("driver_service_identity_invalid".into());
        }
    }
    stream
        .write_all(&serde_json::to_vec(request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    stream
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if reply["ok"] == false {
        return Err(reply["code"]
            .as_str()
            .unwrap_or("driver_service_failed")
            .into());
    }
    Ok(reply)
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn client(_endpoint: &Path, _request: &Request) -> PackageResult<Value> {
    Err("driver_platform_unsupported".into())
}
#[cfg(not(any(windows, target_os = "linux")))]
pub fn serve(_policy: Policy, _stop: Arc<AtomicBool>) -> PackageResult<()> {
    Err("driver_platform_unsupported".into())
}
#[cfg(windows)]
pub fn client(_endpoint: &Path, request: &Request) -> PackageResult<Value> {
    let mut pipe =
        amaterasu_input_companion::InstallationPipe::connect().map_err(|e| format!("{e:?}"))?;
    pipe.write(&serde_json::to_vec(request).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{e:?}"))?;
    let bytes = pipe.read()?.ok_or("driver_service_closed")?;
    let reply: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if reply["ok"] == false {
        return Err(reply["code"]
            .as_str()
            .unwrap_or("driver_service_failed")
            .into());
    }
    Ok(reply)
}
#[cfg(windows)]
pub fn serve(policy: Policy, stop: Arc<AtomicBool>) -> PackageResult<()> {
    use amaterasu_input_companion::InstallationPipe;
    amaterasu_package::create_machine_private_directory(&policy.approvals)?;
    super::security::machine_private(&policy.approvals)?;
    let jobs = Arc::new(Mutex::new(BTreeMap::new()));
    while !stop.load(Ordering::Acquire) {
        let mut pipe = InstallationPipe::listen(true)?;
        while !stop.load(Ordering::Acquire) && !pipe.accept(&stop)? {}
        if stop.load(Ordering::Acquire) {
            break;
        }
        let result = (|| {
            let bytes = pipe.read()?.ok_or("driver_request_missing")?;
            let peer = pipe.peer()?;
            if (!policy.runtime_identity.is_empty() && peer.principal != policy.runtime_identity)
                || !authorized_workers()
                    .lock()
                    .map_err(|_| "driver_identity_unavailable")?
                    .get(&peer.process_id)
                    .is_some_and(|time| *time == peer.creation_time)
            {
                return Err("driver_peer_denied".into());
            }
            // This check uses the kernel pipe PID after token impersonation.
            let image = process_image(peer.process_id)?;
            if amaterasu_package::sha256(&std::fs::read(image).map_err(|e| e.to_string())?)
                != policy.host_sha256
            {
                return Err("driver_host_changed".into());
            }
            let request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let request_policy = policy_for_request(&policy, &peer.principal, &request)?;
            handle(&request_policy, request, &jobs)
        })();
        let reply = result.unwrap_or_else(|code: String| json!({"ok":false,"code":code}));
        let _ = pipe.write(&serde_json::to_vec(&reply).map_err(|e| e.to_string())?);
    }
    Ok(())
}
/// The existing SCM supervisor can stage ordinary, verified plugin compositions
/// without granting machine-installation authority. The principal comes only
/// from its authenticated, supervisor-owned worker, never from the request.
#[cfg(any(windows, test))]
fn policy_for_request(
    policy: &Policy,
    principal: &str,
    request: &Request,
) -> PackageResult<Policy> {
    let mut reviewed = policy.clone();
    if policy.runtime_identity.is_empty() {
        if !matches!(request, Request::Stage { .. }) {
            return Err("driver_service_not_provisioned".into());
        }
        reviewed.repository_directory = Some(
            policy
                .root
                .join("user-state")
                .join(principal)
                .join("repositories"),
        );
        reviewed.runtime_identity = principal.to_owned();
    } else if policy.runtime_identity != principal {
        return Err("driver_peer_denied".into());
    }
    Ok(reviewed)
}
#[cfg(windows)]
fn process_image(pid: u32) -> PackageResult<PathBuf> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
    };
    // SAFETY: PID is supplied by the authenticated local pipe kernel endpoint;
    // the query handle and UTF-16 output allocation live until the call returns.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err("driver_peer_unavailable".into());
        }
        let mut buffer = vec![0u16; 32768];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        if result == 0 {
            return Err("driver_peer_image_unavailable".into());
        }
        Ok(PathBuf::from(
            String::from_utf16(&buffer[..length as usize]).map_err(|e| e.to_string())?,
        ))
    }
}

/// A supervisor grants this only to the exact process it created, before READY.
#[cfg(windows)]
fn authorized_workers() -> &'static Mutex<BTreeMap<u32, u64>> {
    static WORKERS: std::sync::OnceLock<Mutex<BTreeMap<u32, u64>>> = std::sync::OnceLock::new();
    WORKERS.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[cfg(windows)]
pub fn authorize_worker(pid: u32, creation_time: u64) -> PackageResult<()> {
    let mut workers = authorized_workers()
        .lock()
        .map_err(|_| "driver_identity_unavailable")?;
    if workers.len() >= 128 {
        workers.clear();
    }
    workers.insert(pid, creation_time);
    Ok(())
}

pub fn read_policy(path: &Path) -> PackageResult<Policy> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("driver_unsafe_policy".into());
    }
    super::security::machine_private(path)?;
    let policy: Policy = amaterasu_package::read_json(path)?;
    super::security::real_path(&policy.root)?;
    #[cfg(windows)]
    super::security::windows_parent(&policy.root)?;
    if !policy.root.is_absolute()
        || !policy.approvals.is_absolute()
        || !policy.endpoint.is_absolute()
        || !amaterasu_package::plugins::digest(&policy.host_sha256)
        || policy.publishers.is_empty()
        || policy
            .publishers
            .iter()
            .any(|publisher| !amaterasu_package::plugins::digest(publisher))
    {
        return Err("driver_invalid_policy".into());
    }
    Ok(policy)
}

#[cfg(any(windows, target_os = "linux"))]
fn import_sources(policy: &Policy, operation: &Operation) -> PackageResult<()> {
    let Some(directory) = &policy.repository_directory else {
        return Ok(());
    };
    let input = amaterasu_package::repository::RepositoryStore::new(directory.clone())?;
    let output =
        amaterasu_package::repository::RepositoryStore::new(policy.root.join("repositories"))?;
    let bootstrap: amaterasu_package::BootstrapConfig =
        amaterasu_package::read_json(&policy.root.join("bootstrap.json"))?;
    for source in &operation.sources {
        let approved = input.approved(&source.repository)?;
        if approved.fingerprint != source.publisher {
            return Err("driver_publisher_not_approved".into());
        }
        match output.approved(&source.repository) {
            Ok(existing) if existing.fingerprint == source.publisher => {}
            Ok(_) => return Err("plugin_publisher_changed".into()),
            Err(_) => output.approve(approved.clone())?,
        }
        let (raw, signature) = input.export_catalog(&source.repository)?;
        output.accept_catalog(
            &source.repository,
            raw.as_bytes(),
            &signature,
            &amaterasu_package::current_target(),
            amaterasu_package::unix_timestamp(),
        )?;
        let catalog = output.catalog(
            &source.repository,
            &amaterasu_package::current_target(),
            amaterasu_package::unix_timestamp(),
        )?;
        let candidate = catalog
            .candidates()
            .iter()
            .find(|candidate| candidate.manifest_sha256 == source.manifest_digest)
            .ok_or("plugin_not_offered")?;
        let from = input
            .existing_native_candidate_cache_path(&source.repository, &source.manifest_digest)?;
        amaterasu_package::repository::verify_candidate(
            &approved,
            &catalog,
            candidate,
            &from,
            &bootstrap,
            amaterasu_package::unix_timestamp(),
        )?;
        let to = output.native_candidate_cache_path(&source.repository, &source.manifest_digest)?;
        if !to.exists() {
            let stage = to
                .parent()
                .ok_or("driver_cache_parent_missing")?
                .join(format!(
                    ".stage-{}",
                    amaterasu_package::random_identifier()?
                ));
            amaterasu_package::create_machine_private_directory(&stage)?;
            let copied = (|| {
                let manifest: amaterasu_package::plugins::PluginManifest =
                    amaterasu_package::read_json(&from.join("manifest.json"))?;
                let files: Vec<_> = manifest
                    .artifacts()?
                    .into_iter()
                    .map(|artifact| artifact.file)
                    .chain(["manifest.json".into(), "manifest.sig".into()])
                    .collect();
                for file in &files {
                    std::fs::copy(from.join(file), stage.join(file)).map_err(|e| e.to_string())?;
                }
                amaterasu_package::repository::verify_candidate(
                    &approved,
                    &catalog,
                    candidate,
                    &stage,
                    &bootstrap,
                    amaterasu_package::unix_timestamp(),
                )?;
                #[cfg(windows)]
                {
                    super::security::runtime_read(&stage, &policy.runtime_identity)?;
                    for file in &files {
                        super::security::runtime_read(&stage.join(file), &policy.runtime_identity)?;
                    }
                }
                std::fs::rename(&stage, &to).map_err(|e| e.to_string())?;
                amaterasu_package::sync_directory(to.parent().ok_or("driver_cache_parent_missing")?)
            })();
            if copied.is_err() {
                let _ = std::fs::remove_dir_all(stage);
            }
            copied?;
        }
        amaterasu_package::repository::verify_candidate(
            &approved,
            &catalog,
            candidate,
            &to,
            &bootstrap,
            amaterasu_package::unix_timestamp(),
        )?;
    }
    Ok(())
}
#[cfg(windows)]
pub struct Service {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<PackageResult<()>>>,
}
#[cfg(windows)]
impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(windows)]
pub fn start_optional(root: &Path) -> PackageResult<Option<Service>> {
    let path = root.join("plugin-installation-policy.json");
    super::security::real_path(root)?;
    super::security::windows_parent(root)?;
    let configured = if path.exists() {
        match read_policy(&path) {
            Ok(policy) => Some(policy),
            Err(error) => {
                eprintln!("Driver installation policy is unavailable: {error}");
                None
            }
        }
    } else {
        None
    };
    let policy = configured.unwrap_or(Policy {
        repository_directory: None,
        root: root.to_owned(),
        endpoint: PathBuf::from(r"\\.\pipe\AmaterasuPluginInstaller-v1"),
        approvals: root.join("composition-approvals"),
        host_sha256: amaterasu_package::sha256(
            &std::fs::read(std::env::current_exe().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        ),
        runtime_identity: String::new(),
        publishers: Vec::new(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let thread = std::thread::spawn(move || {
        let result = serve(policy, worker_stop);
        if let Err(error) = &result {
            eprintln!("Plugin composition service stopped: {error}");
        }
        result
    });
    Ok(Some(Service {
        stop,
        thread: Some(thread),
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approval_names_cannot_select_paths() {
        let policy = Policy {
            repository_directory: None,
            root: "/fixture".into(),
            endpoint: "/fixture/socket".into(),
            approvals: "/fixture/approvals".into(),
            host_sha256: "a".repeat(64),
            runtime_identity: "1000".into(),
            publishers: vec!["b".repeat(64)],
        };
        for id in ["../escape", "/etc/passwd", "a/b", "", "a\\b"] {
            assert!(approval_path(&policy, id).is_err());
        }
        assert_eq!(
            approval_path(&policy, "setup-1").unwrap(),
            PathBuf::from("/fixture/approvals/setup-1.json")
        );
    }
    #[test]
    fn composition_staging_does_not_enable_privileged_driver_approval() {
        let operation = Operation::prepare("a".repeat(64), vec![], "user".into(), vec![]).unwrap();
        let mut policy = Policy {
            repository_directory: None,
            root: std::env::temp_dir().join("unused-composition-policy-fixture"),
            endpoint: "unused".into(),
            approvals: "unused".into(),
            host_sha256: "b".repeat(64),
            runtime_identity: String::new(),
            publishers: vec![],
        };
        assert_eq!(
            policy_for_request(
                &policy,
                "user",
                &Request::Approve {
                    operation: operation.clone()
                }
            )
            .err()
            .as_deref(),
            Some("driver_service_not_provisioned")
        );
        let request = Request::Stage { operation };
        let staged = policy_for_request(&policy, "user", &request).unwrap();
        assert_eq!(staged.runtime_identity, "user");
        assert_eq!(
            staged.repository_directory,
            Some(policy.root.join("user-state/user/repositories"))
        );
        assert!(staged.publishers.is_empty());
        policy.runtime_identity = "pinned-user".into();
        assert_eq!(
            policy_for_request(&policy, "user", &request)
                .err()
                .as_deref(),
            Some("driver_peer_denied")
        );
    }
}
