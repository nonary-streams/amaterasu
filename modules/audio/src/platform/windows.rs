//! All COM references and WASAPI buffers stay on one dedicated apartment thread.
use crate::{
    model::{AudioResult, Endpoint, Format, Packet, PcmCaptureFormat},
    packet::{Timeline, copy_samples},
    platform::RecoveryConfig,
    route_policy::{
        AcquirePlan, DefaultEndpoints, DefaultRole, ReleasePlan, RoleAssignment, RoutePolicy,
        assignments_reached_target,
    },
    virtual_sink::{
        ChannelLayout, VirtualFormatCandidate, VirtualFormatPolicy, VirtualSampleFormat,
        channel_layout, preferred_format_candidates, select_endpoint, select_policy_virtual_sink,
    },
};
#[cfg(test)]
use amaterasu_media::PCM_I16;
use amaterasu_media::{AUDIO_DISCONTINUITY, AUDIO_SILENT, AUDIO_TIMESTAMP_ESTIMATED, PCM_F32};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    os::windows::ffi::OsStrExt,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::{
            PKEY_Device_DeviceDesc, PKEY_Device_DriverDesc, PKEY_Device_DriverProvider,
            PKEY_Device_FriendlyName, PKEY_DeviceInterface_FriendlyName,
        },
        Media::{
            Audio::*, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM,
            Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        },
        System::Com::{StructuredStorage::PropVariantToStringAlloc, *},
    },
    core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, PWSTR},
};

const POLICY_CONFIG_CLASS: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const POLICY_CONFIG_INTERFACE: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
const HRESULT_NOT_FOUND: HRESULT = HRESULT(0x80070490u32 as i32);
const DEFAULT_ENDPOINT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const BUFFER_ERROR_RECOVERY_DELAY: Duration = Duration::from_secs(1);
const ROUTE_RESTORE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const STEAM_DRIVER_DESCRIPTION: &str = "Steam Streaming Speakers";
const STEAM_DRIVER_PROVIDER: &str = "Valve Corporation";
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;

static ROUTE_POLICY: OnceLock<Mutex<RoutePolicy>> = OnceLock::new();
static VIRTUAL_FORMAT_POLICY: OnceLock<Mutex<VirtualFormatPolicy>> = OnceLock::new();
static ROUTE_RESTORE_WORKER: OnceLock<Mutex<Option<thread::JoinHandle<()>>>> = OnceLock::new();
static ROUTE_RESTORE_SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static ROUTE_RESTORE_LIFECYCLE: Mutex<RouteRestoreLifecycle> =
    Mutex::new(RouteRestoreLifecycle { active_modules: 0 });
static STEAM_DRIVER_INSTALL: OnceLock<Mutex<SteamDriverInstallManager>> = OnceLock::new();

#[derive(Clone)]
struct SteamDriverInstallStatus {
    enabled: bool,
    revision: Option<u64>,
    state: &'static str,
    error_code: Option<&'static str>,
    native_error: Option<u32>,
    endpoint: Option<String>,
    restored_roles: Vec<&'static str>,
    restore_failures: Vec<Value>,
}

impl Default for SteamDriverInstallStatus {
    fn default() -> Self {
        Self {
            enabled: true,
            revision: None,
            state: "awaiting_configuration",
            error_code: None,
            native_error: None,
            endpoint: None,
            restored_roles: Vec::new(),
            restore_failures: Vec::new(),
        }
    }
}

impl SteamDriverInstallStatus {
    fn payload(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "revision": self.revision,
            "state": self.state,
            "error_code": self.error_code,
            "native_error": self.native_error,
            "endpoint": self.endpoint,
            "restored_roles": self.restored_roles,
            "restore_failures": self.restore_failures,
        })
    }
}

#[derive(Default)]
struct SteamDriverInstallManager {
    status: SteamDriverInstallStatus,
    attempted: bool,
    worker: Option<thread::JoinHandle<()>>,
}

struct RouteRestoreLifecycle {
    active_modules: usize,
}

impl RouteRestoreLifecycle {
    fn start(&mut self) -> bool {
        let first = self.active_modules == 0;
        self.active_modules += 1;
        first
    }

    fn stop(&mut self) -> bool {
        debug_assert!(self.active_modules > 0);
        self.active_modules -= 1;
        self.active_modules == 0
    }
}

// IPolicyConfig is an undocumented Windows COM interface. The ten method
// slots after IUnknown precede SetDefaultEndpoint in the pinned interface ABI.
// Placeholder function pointers preserve the slot layout without calling those
// methods. The pointer is queried from the COM class and released by PolicyConfig.
#[repr(C)]
struct PolicyConfigVtable {
    base__: IUnknown_Vtbl,
    preceding_set_device_format: [unsafe extern "system" fn() -> HRESULT; 3],
    set_device_format: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        *mut WAVEFORMATEX,
        *mut WAVEFORMATEX,
    ) -> HRESULT,
    intervening_methods: [unsafe extern "system" fn() -> HRESULT; 6],
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
}

struct PolicyConfig(*mut c_void);

impl PolicyConfig {
    fn new() -> AudioResult<Self> {
        // SAFETY: the COM apartment is initialized on this worker thread. The
        // returned IUnknown owns one reference; QueryInterface transfers a
        // second reference for PolicyConfig, which this wrapper releases.
        unsafe {
            let unknown: IUnknown = CoCreateInstance::<_, IUnknown>(
                &POLICY_CONFIG_CLASS,
                None::<&IUnknown>,
                CLSCTX_ALL,
            )
            .map_err(|_| "audio_route_unavailable")?;
            let unknown_vtable = *(unknown.as_raw() as *mut *const IUnknown_Vtbl);
            let mut interface = std::ptr::null_mut();
            ((*unknown_vtable).QueryInterface)(
                unknown.as_raw(),
                &POLICY_CONFIG_INTERFACE,
                &mut interface,
            )
            .ok()
            .map_err(|_| "audio_route_unavailable")?;
            if interface.is_null() {
                return Err("audio_route_unavailable");
            }
            Ok(Self(interface))
        }
    }

    fn set_default(&self, endpoint: &str, role: DefaultRole) -> AudioResult<()> {
        let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let role = match role {
            DefaultRole::Console => eConsole,
            DefaultRole::Multimedia => eMultimedia,
            DefaultRole::Communications => eCommunications,
        };
        // SAFETY: this vtable slot is SetDefaultEndpoint at IPolicyConfig slot
        // 13 (three IUnknown slots plus ten preceding interface methods). The
        // UTF-16 endpoint remains alive for the synchronous COM call.
        unsafe {
            let vtable = &**(self.0 as *mut *const PolicyConfigVtable);
            (vtable.set_default_endpoint)(self.0, PCWSTR(wide.as_ptr()), role)
                .ok()
                .map_err(|_| "audio_route_change_failed")
        }
    }

    fn set_device_format(
        &self,
        endpoint: &str,
        format: &mut WAVEFORMATEXTENSIBLE,
    ) -> AudioResult<()> {
        let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let mut default_format = WAVEFORMATEXTENSIBLE::default();
        // SAFETY: the vtable slot is SetDeviceFormat, the fourth IPolicyConfig
        // method after IUnknown. The UTF-16 ID and both extensible format
        // structures stay live for the synchronous COM call; the zeroed
        // default-format buffer has the full extension size expected by the
        // ABI. This undocumented call is isolated here and used only for an
        // explicitly selected virtual sink.
        unsafe {
            let vtable = &**(self.0 as *mut *const PolicyConfigVtable);
            (vtable.set_device_format)(
                self.0,
                PCWSTR(wide.as_ptr()),
                (&mut format.Format) as *mut WAVEFORMATEX,
                &mut default_format.Format,
            )
            .ok()
            .map_err(|_| "virtual_sink_format_unavailable")
        }
    }
}

impl Drop for PolicyConfig {
    fn drop(&mut self) {
        // SAFETY: QueryInterface returned one owned COM reference. Its Release
        // method is the first IUnknown vtable slot, and this wrapper drops on
        // the apartment thread that created it.
        unsafe {
            let vtable = &**(self.0 as *mut *const PolicyConfigVtable);
            (vtable.base__.Release)(self.0);
        }
    }
}

struct RouteLease {
    endpoint: String,
    active: bool,
}

impl RouteLease {
    fn unchanged(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_owned(),
            active: false,
        }
    }

    fn acquire(
        enumerator: &IMMDeviceEnumerator,
        endpoint: &str,
        capture_sink_without_routing: bool,
        virtual_sink: bool,
        explicit_route: bool,
    ) -> AudioResult<Self> {
        if capture_sink_without_routing || endpoint == "default" {
            return Ok(Self::unchanged(endpoint));
        }

        let mutex = ROUTE_POLICY.get_or_init(|| Mutex::new(RoutePolicy::new()));
        let mut policy = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original = if matches!(restore_pending(enumerator, &mut policy), Ok(true)) {
            default_endpoints(enumerator)?
        } else {
            let current = default_endpoints(enumerator).ok();
            if explicit_route
                && let Some(current) = current.as_ref()
                && let Some(assignments) = policy.resume_absent_restore(endpoint, current)
            {
                let result = assign_route(enumerator, &mut policy, endpoint, current, &assignments);
                if matches!(result, Err("audio_route_restore_failed")) {
                    drop(policy);
                    schedule_route_restore_retry_if_pending();
                }
                return result;
            }
            if !explicit_route
                || !current.as_ref().is_some_and(|current| {
                    policy.abandon_absent_restore_for_new_route(endpoint, current)
                })
            {
                drop(policy);
                schedule_route_restore_retry_if_pending();
                return Err("audio_route_restore_failed");
            }
            current.expect("successful handoff requires a route snapshot")
        };
        match policy.acquire_with_tracking(endpoint, &original, virtual_sink)? {
            AcquirePlan::Unchanged => Ok(Self::unchanged(endpoint)),
            AcquirePlan::Tracked | AcquirePlan::Shared => Ok(Self {
                endpoint: endpoint.to_owned(),
                active: true,
            }),
            AcquirePlan::Assign(assignments) => {
                let result =
                    assign_route(enumerator, &mut policy, endpoint, &original, &assignments);
                if matches!(result, Err("audio_route_restore_failed")) {
                    drop(policy);
                    schedule_route_restore_retry_if_pending();
                }
                result
            }
        }
    }

    fn refresh_virtual_sink(&self, enumerator: &IMMDeviceEnumerator) -> AudioResult<()> {
        if !self.active {
            return Ok(());
        }
        let mutex = ROUTE_POLICY.get().ok_or("audio_route_unavailable")?;
        let mut policy = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = default_endpoints(enumerator)?;
        let mut endpoint_policy = None;
        refresh_virtual_route(
            &mut policy,
            &self.endpoint,
            &current,
            |role| role_endpoint(enumerator, role),
            |role| {
                if endpoint_policy.is_none() {
                    endpoint_policy = Some(PolicyConfig::new()?);
                }
                endpoint_policy
                    .as_ref()
                    .expect("policy was created before assignment")
                    .set_default(&self.endpoint, role)
            },
        )
    }
}

fn refresh_virtual_route(
    policy: &mut RoutePolicy,
    endpoint: &str,
    current: &DefaultEndpoints,
    mut read_role: impl FnMut(DefaultRole) -> AudioResult<Option<String>>,
    mut assign_role: impl FnMut(DefaultRole) -> AudioResult<()>,
) -> AudioResult<()> {
    for assignment in policy.refresh_assignments(endpoint, current)? {
        let observed = read_role(assignment.role)?;
        if observed.as_deref() == Some(endpoint) {
            continue;
        }
        policy.attempt_refresh_assignment(
            endpoint,
            assignment.role,
            observed.as_deref(),
            || assign_role(assignment.role),
        )?;
        if read_role(assignment.role)?.as_deref() != Some(endpoint) {
            return Err("audio_route_change_failed");
        }
    }
    Ok(())
}

fn assign_route(
    enumerator: &IMMDeviceEnumerator,
    policy: &mut RoutePolicy,
    endpoint: &str,
    observed: &DefaultEndpoints,
    assignments: &[RoleAssignment],
) -> AudioResult<RouteLease> {
    if assign_defaults(enumerator, policy, endpoint, observed, assignments).is_err() {
        policy.abort_acquire(endpoint)?;
        if !matches!(restore_pending(enumerator, policy), Ok(true)) {
            return Err("audio_route_restore_failed");
        }
        return Err("audio_route_change_failed");
    }
    Ok(RouteLease {
        endpoint: endpoint.to_owned(),
        active: true,
    })
}

impl Drop for RouteLease {
    fn drop(&mut self) {
        if self.active {
            release_route(&self.endpoint);
        }
    }
}

fn default_endpoints(enumerator: &IMMDeviceEnumerator) -> AudioResult<DefaultEndpoints> {
    let mut endpoints: DefaultEndpoints = std::array::from_fn(|_| None);
    for role in DefaultRole::ALL {
        endpoints[role.index()] = role_endpoint(enumerator, role)?;
    }
    Ok(endpoints)
}

fn role_endpoint(
    enumerator: &IMMDeviceEnumerator,
    role: DefaultRole,
) -> AudioResult<Option<String>> {
    let role = match role {
        DefaultRole::Console => eConsole,
        DefaultRole::Multimedia => eMultimedia,
        DefaultRole::Communications => eCommunications,
    };
    // SAFETY: this is a read-only query in the caller's live COM apartment.
    unsafe {
        match enumerator.GetDefaultAudioEndpoint(eRender, role) {
            Ok(device) => identifier(&device).map(Some),
            Err(error) if error.code() == HRESULT_NOT_FOUND => Ok(None),
            Err(_) => Err("audio_route_snapshot_failed"),
        }
    }
}

fn assign_defaults(
    enumerator: &IMMDeviceEnumerator,
    route_policy: &mut RoutePolicy,
    endpoint: &str,
    original: &DefaultEndpoints,
    assignments: &[RoleAssignment],
) -> AudioResult<()> {
    let policy = PolicyConfig::new()?;
    for assignment in assignments {
        let current = role_endpoint(enumerator, assignment.role)?;
        if current.as_deref() == Some(endpoint) {
            continue;
        }
        if current != original[assignment.role.index()] {
            // Another actor changed this role after our snapshot. Preserve it.
            continue;
        }
        route_policy.attempt_assignment(endpoint, assignment.role, || {
            policy.set_default(endpoint, assignment.role)
        })?;
    }

    let current = default_endpoints(enumerator)?;
    if !assignments_reached_target(assignments, &current) {
        return Err("audio_route_change_failed");
    }
    Ok(())
}

fn restore_pending(
    enumerator: &IMMDeviceEnumerator,
    policy: &mut RoutePolicy,
) -> AudioResult<bool> {
    let Some(endpoint) = policy.endpoint_with_pending_restore().map(str::to_owned) else {
        return Ok(true);
    };
    let current = default_endpoints(enumerator)?;
    let assignments = policy
        .pending_restore_assignments(&current)
        .ok_or("audio_route_unavailable")?;
    if !assignments.is_empty() {
        let endpoint_policy = PolicyConfig::new()?;
        for assignment in assignments {
            if role_endpoint(enumerator, assignment.role)?.as_deref() == Some(endpoint.as_str()) {
                // Keep trying the other roles after a failure. The final query
                // decides whether any role still needs restoration.
                let _ = endpoint_policy.set_default(&assignment.endpoint, assignment.role);
            }
        }
    }

    let current = default_endpoints(enumerator)?;
    let restored = policy.pending_restore_complete(&current);
    policy.complete_restore(restored);
    Ok(restored)
}

fn release_route(endpoint: &str) {
    let mutex = ROUTE_POLICY.get_or_init(|| Mutex::new(RoutePolicy::new()));
    let mut policy = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Ok(enumerator) = enumerator() else {
        let _ = policy.release_without_snapshot(endpoint);
        drop(policy);
        schedule_route_restore_retry_if_pending();
        return;
    };
    let current = match default_endpoints(&enumerator) {
        Ok(current) => current,
        Err(_) => {
            let _ = policy.release_without_snapshot(endpoint);
            drop(policy);
            schedule_route_restore_retry_if_pending();
            return;
        }
    };
    match policy.release(endpoint, &current) {
        Ok(ReleasePlan::Shared) => {}
        Ok(ReleasePlan::Restore(_)) => {
            let _ = restore_pending(&enumerator, &mut policy);
        }
        Err(_) => {}
    }
    drop(policy);
    schedule_route_restore_retry_if_pending();
}

fn schedule_route_restore_retry_if_pending() {
    if !route_restore_pending() || ROUTE_RESTORE_SHUTTING_DOWN.load(Ordering::Acquire) {
        return;
    }

    let worker_state = ROUTE_RESTORE_WORKER.get_or_init(|| Mutex::new(None));
    let mut worker = worker_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if ROUTE_RESTORE_SHUTTING_DOWN.load(Ordering::Acquire) {
        return;
    }

    if worker.as_ref().is_some_and(thread::JoinHandle::is_finished) {
        let finished_worker = worker.take().expect("finished retry worker is present");
        drop(worker);
        let _ = finished_worker.join();
        schedule_route_restore_retry_if_pending();
        return;
    }

    if let Some(worker) = worker.as_ref() {
        worker.thread().unpark();
        return;
    }

    if let Ok(worker_handle) = thread::Builder::new()
        .name("audio-route-restore".to_owned())
        .spawn(route_restore_retry_worker)
    {
        worker_handle.thread().unpark();
        *worker = Some(worker_handle);
    }
}

fn route_restore_retry_worker() {
    loop {
        if ROUTE_RESTORE_SHUTTING_DOWN.load(Ordering::Acquire) {
            return;
        }

        if retry_pending_route_restore_once() {
            if ROUTE_RESTORE_SHUTTING_DOWN.load(Ordering::Acquire) {
                return;
            }
            thread::park_timeout(ROUTE_RESTORE_RETRY_INTERVAL);
        } else {
            if ROUTE_RESTORE_SHUTTING_DOWN.load(Ordering::Acquire) {
                return;
            }
            thread::park();
        }
    }
}

fn stop_and_join_route_restore_worker(worker: thread::JoinHandle<()>) {
    worker.thread().unpark();
    let _ = worker.join();
}

fn stop_worker_then_restore_once(
    worker: Option<thread::JoinHandle<()>>,
    has_pending_restore: impl FnOnce() -> bool,
    restore_once: impl FnOnce(),
) {
    if let Some(worker) = worker {
        // Finish any in-flight COM operation before the final restore attempt.
        stop_and_join_route_restore_worker(worker);
    }
    if has_pending_restore() {
        restore_once();
    }
}

fn start_route_restore_lifecycle(
    lifecycle: &Mutex<RouteRestoreLifecycle>,
    shutting_down: &AtomicBool,
    has_pending_restore: impl FnOnce() -> bool,
    schedule_retry: impl FnOnce(),
) {
    let mut lifecycle = lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if lifecycle.start() {
        // The final prior shutdown joined its retry worker while holding this
        // lifecycle lock, so no worker from that generation can outlive reset.
        shutting_down.store(false, Ordering::Release);
        if has_pending_restore() {
            schedule_retry();
        }
    }
}

fn stop_route_restore_lifecycle(
    lifecycle: &Mutex<RouteRestoreLifecycle>,
    shutting_down: &AtomicBool,
    stop_worker_and_restore: impl FnOnce(),
) {
    let mut lifecycle = lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !lifecycle.stop() {
        return;
    }
    shutting_down.store(true, Ordering::Release);
    stop_worker_and_restore();
}

pub fn startup() {
    reset_steam_driver_install_state();
    start_route_restore_lifecycle(
        &ROUTE_RESTORE_LIFECYCLE,
        &ROUTE_RESTORE_SHUTTING_DOWN,
        route_restore_pending,
        schedule_route_restore_retry_if_pending,
    );
}

pub fn shutdown() {
    shutdown_steam_driver_install();
    stop_route_restore_lifecycle(
        &ROUTE_RESTORE_LIFECYCLE,
        &ROUTE_RESTORE_SHUTTING_DOWN,
        || {
            let worker = ROUTE_RESTORE_WORKER.get().and_then(|worker_state| {
                worker_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
            });

            // Do not hold either policy state or worker state while joining. Shutdown
            // needs a final attempt because the retry worker exits when its stop flag
            // is set, potentially before it next examines pending route restoration.
            stop_worker_then_restore_once(worker, route_restore_pending, || {
                // Shutdown may run on a caller's incompatible COM apartment.
                // Open a fresh STA for the final, bounded restoration attempt.
                if let Ok(final_worker) = thread::Builder::new()
                    .name("audio-route-final-restore".to_owned())
                    .spawn(retry_pending_route_restore_once)
                {
                    let _ = final_worker.join();
                }
            });
        },
    );
}

pub fn configure_steam_driver_install(enabled: bool, revision: u64) -> Value {
    let manager =
        STEAM_DRIVER_INSTALL.get_or_init(|| Mutex::new(SteamDriverInstallManager::default()));
    let mut completed_worker = None;
    let payload = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if manager
            .worker
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            completed_worker = manager.worker.take();
        }

        let was_enabled = manager.status.enabled;
        manager.status.enabled = enabled;
        manager.status.revision = Some(revision);
        if !enabled {
            manager.status.state = "disabled";
            manager.status.error_code = None;
            manager.status.native_error = None;
            manager.status.endpoint = None;
            manager.status.restored_roles.clear();
            manager.status.restore_failures.clear();
            manager.attempted = false;
        } else if !was_enabled || !manager.attempted {
            manager.attempted = true;
            manager.status.state = "checking";
            manager.status.error_code = None;
            manager.status.native_error = None;
            manager.status.endpoint = None;
            manager.status.restored_roles.clear();
            manager.status.restore_failures.clear();
            if manager.worker.is_none() {
                let spawn = thread::Builder::new()
                    .name("steam-audio-driver-install".into())
                    .spawn(run_steam_driver_install);
                match spawn {
                    Ok(worker) => manager.worker = Some(worker),
                    Err(_) => {
                        manager.status.state = "error";
                        manager.status.error_code = Some("steam_driver_worker_unavailable");
                    }
                }
            }
        }
        manager.status.payload()
    };
    if let Some(worker) = completed_worker {
        let _ = worker.join();
    }
    payload
}

pub fn steam_driver_install_status() -> Value {
    STEAM_DRIVER_INSTALL
        .get_or_init(|| Mutex::new(SteamDriverInstallManager::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .status
        .payload()
}

fn reset_steam_driver_install_state() {
    let mut manager = STEAM_DRIVER_INSTALL
        .get_or_init(|| Mutex::new(SteamDriverInstallManager::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    manager.status = SteamDriverInstallStatus::default();
    manager.attempted = false;
}

fn shutdown_steam_driver_install() {
    let worker = {
        let manager =
            STEAM_DRIVER_INSTALL.get_or_init(|| Mutex::new(SteamDriverInstallManager::default()));
        let mut manager = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        manager.status.enabled = false;
        manager.status.state = "disabled";
        manager.worker.take()
    };
    if let Some(worker) = worker {
        let _ = worker.join();
    }
}

fn run_steam_driver_install() {
    let result = (|| {
        let _apartment = Apartment::start()
            .map_err(|_| crate::steam_driver::DriverFailure::new("steam_driver_com_unavailable"))?;
        let mut backend = WindowsSteamDriverBackend;
        crate::steam_driver::ensure_steam_driver(&mut backend)
    })();
    let mut manager = STEAM_DRIVER_INSTALL
        .get_or_init(|| Mutex::new(SteamDriverInstallManager::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !manager.status.enabled {
        return;
    }
    match result {
        Ok(crate::steam_driver::InstallOutcome::AlreadyPresent { endpoint }) => {
            manager.status.state = "already_present";
            manager.status.endpoint = Some(endpoint);
        }
        Ok(crate::steam_driver::InstallOutcome::Installed {
            endpoint,
            restored_roles,
            restore_failures,
        }) => {
            manager.status.state = if restore_failures.is_empty() {
                "installed"
            } else {
                "installed_with_route_restore_errors"
            };
            manager.status.endpoint = Some(endpoint);
            manager.status.restored_roles = restored_roles
                .into_iter()
                .map(crate::steam_driver::DefaultRole::name)
                .collect();
            manager.status.restore_failures = restore_failures
                .into_iter()
                .map(|failure| {
                    json!({
                        "role": failure.role.name(),
                        "error_code": failure.failure.code,
                        "native_error": failure.failure.native_code,
                    })
                })
                .collect();
        }
        Ok(crate::steam_driver::InstallOutcome::InstalledEndpointUnavailable {
            restored_roles,
            restore_failures,
        }) => {
            manager.status.state = "installed_endpoint_unavailable";
            manager.status.restored_roles = restored_roles
                .into_iter()
                .map(crate::steam_driver::DefaultRole::name)
                .collect();
            manager.status.restore_failures = restore_failures
                .into_iter()
                .map(|failure| {
                    json!({
                        "role": failure.role.name(),
                        "error_code": failure.failure.code,
                        "native_error": failure.failure.native_code,
                    })
                })
                .collect();
        }
        Err(failure) => {
            manager.status.state = "error";
            manager.status.error_code = Some(failure.code);
            manager.status.native_error = failure.native_code;
        }
    }
}

struct WindowsSteamDriverBackend;

fn steam_driver_route_role(role: crate::steam_driver::DefaultRole) -> DefaultRole {
    match role {
        crate::steam_driver::DefaultRole::Console => DefaultRole::Console,
        crate::steam_driver::DefaultRole::Multimedia => DefaultRole::Multimedia,
        crate::steam_driver::DefaultRole::Communications => DefaultRole::Communications,
    }
}

fn steam_speaker_device_identity(
    enumerator: &IMMDeviceEnumerator,
    endpoint: &str,
) -> Result<bool, &'static str> {
    let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
    // SAFETY: the endpoint ID remains live for the synchronous GetDevice call,
    // and the returned COM interface stays on this apartment thread.
    let device = unsafe {
        match enumerator.GetDevice(PCWSTR(wide.as_ptr())) {
            Ok(device) => device,
            Err(error) if error.code() == HRESULT_NOT_FOUND => return Ok(false),
            Err(_) => return Err("endpoint_metadata_failed"),
        }
    };
    // Driver provider and description come from the registered PnP driver
    // properties. They remain stable when a user renames the render endpoint.
    let properties =
        unsafe { device.OpenPropertyStore(STGM_READ) }.map_err(|_| "endpoint_metadata_failed")?;
    let Some(provider) = property_text(&properties, &PKEY_Device_DriverProvider) else {
        // Non-PnP and virtual render endpoints can omit this property. They
        // cannot positively identify the Steam driver, so skip them.
        return Ok(false);
    };
    if !provider.eq_ignore_ascii_case(STEAM_DRIVER_PROVIDER) {
        return Ok(false);
    }
    let descriptions = [
        property_text(&properties, &PKEY_Device_DeviceDesc),
        property_text(&properties, &PKEY_Device_DriverDesc),
    ];
    let has_product_description = descriptions.iter().flatten().any(|description| {
        description
            .trim()
            .eq_ignore_ascii_case(STEAM_DRIVER_DESCRIPTION)
    });
    if descriptions.iter().all(Option::is_none) {
        return Err("endpoint_metadata_failed");
    }
    Ok(has_product_description)
}

impl crate::steam_driver::SteamDriverBackend for WindowsSteamDriverBackend {
    fn steam_speaker_endpoint(
        &mut self,
    ) -> Result<Option<String>, crate::steam_driver::DriverFailure> {
        let enumerator = enumerator().map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_endpoint_enumeration_failed")
        })?;
        let endpoints = active_render_endpoints(&enumerator).map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_endpoint_enumeration_failed")
        })?;
        for endpoint in endpoints {
            match steam_speaker_device_identity(&enumerator, &endpoint.id) {
                Ok(true) => return Ok(Some(endpoint.id)),
                Ok(false) => {}
                Err(_) => {
                    return Err(crate::steam_driver::DriverFailure::new(
                        "steam_driver_endpoint_identity_failed",
                    ));
                }
            }
        }
        Ok(None)
    }

    fn default_endpoints(
        &mut self,
    ) -> Result<[Option<String>; 3], crate::steam_driver::DriverFailure> {
        let enumerator = enumerator().map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_route_snapshot_failed")
        })?;
        default_endpoints(&enumerator).map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_route_snapshot_failed")
        })
    }

    fn install_driver(&mut self) -> Result<(), crate::steam_driver::DriverFailure> {
        install_steam_driver_inf()
    }

    fn wait_for_reconfiguration(&mut self, interval: Duration) {
        thread::sleep(interval);
    }

    fn is_steam_speaker_endpoint(
        &mut self,
        endpoint: &str,
    ) -> Result<bool, crate::steam_driver::DriverFailure> {
        let enumerator = enumerator().map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_endpoint_identity_failed")
        })?;
        steam_speaker_device_identity(&enumerator, endpoint).map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_endpoint_identity_failed")
        })
    }

    fn current_default(
        &mut self,
        role: crate::steam_driver::DefaultRole,
    ) -> Result<Option<String>, crate::steam_driver::DriverFailure> {
        let enumerator = enumerator().map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_route_snapshot_failed")
        })?;
        role_endpoint(&enumerator, steam_driver_route_role(role)).map_err(|_| {
            crate::steam_driver::DriverFailure::new("steam_driver_route_snapshot_failed")
        })
    }

    fn set_default(
        &mut self,
        endpoint: &str,
        role: crate::steam_driver::DefaultRole,
    ) -> Result<(), crate::steam_driver::DriverFailure> {
        PolicyConfig::new()
            .and_then(|policy| policy.set_default(endpoint, steam_driver_route_role(role)))
            .map_err(|_| {
                crate::steam_driver::DriverFailure::new("steam_driver_route_restore_failed")
            })
    }
}

fn install_steam_driver_inf() -> Result<(), crate::steam_driver::DriverFailure> {
    let common_files = std::env::var_os("CommonProgramFiles(x86)").ok_or_else(|| {
        crate::steam_driver::DriverFailure::new("steam_driver_common_files_unavailable")
    })?;
    let inf_path =
        crate::steam_driver::steam_driver_inf_path(&std::path::PathBuf::from(common_files));
    if !inf_path.is_file() {
        return Err(crate::steam_driver::DriverFailure::new(
            "steam_driver_inf_not_found",
        ));
    }
    let wide_path: Vec<u16> = inf_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let library_name: Vec<u16> = "newdev.dll".encode_utf16().chain(Some(0)).collect();
    // SAFETY: the library name is fixed and loading is restricted to System32,
    // so the current directory and PATH cannot provide a replacement DLL.
    let library = unsafe {
        LoadLibraryExW(
            library_name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if library.is_null() {
        let error = unsafe { GetLastError() };
        return Err(native_install_failure(error));
    }

    // SAFETY: the module is a live System32 newdev.dll handle and the symbol
    // name is a static NUL-terminated export name.
    let symbol = unsafe { GetProcAddress(library, c"DiInstallDriverW".as_ptr().cast()) };
    let Some(symbol) = symbol else {
        let error = unsafe { GetLastError() };
        unsafe { FreeLibrary(library) };
        return Err(crate::steam_driver::DriverFailure::native(
            "steam_driver_installer_unavailable",
            error,
        ));
    };
    type InstallDriver = unsafe extern "system" fn(*mut c_void, *const u16, u32, *mut i32) -> i32;
    // SAFETY: DiInstallDriverW has the documented BOOL(HWND, PCWSTR, DWORD,
    // PBOOL) system ABI. The symbol comes from newdev.dll on the active Windows
    // process architecture, and the exact signature is checked by the call.
    let install_driver: InstallDriver = unsafe { std::mem::transmute(symbol) };
    let mut _needs_reboot = 0;
    // SAFETY: the INF path is NUL-terminated UTF-16 and remains live through
    // the call; a null parent HWND and zero flags request normal installation.
    let installed = unsafe {
        install_driver(
            std::ptr::null_mut(),
            wide_path.as_ptr(),
            0,
            &mut _needs_reboot,
        )
    };
    let error = if installed == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    // SAFETY: this function owns the reference returned by LoadLibraryExW and
    // no API call is in flight after DiInstallDriverW returns.
    unsafe { FreeLibrary(library) };
    error.map_or(Ok(()), |error| Err(native_install_failure(error)))
}

fn native_install_failure(error: u32) -> crate::steam_driver::DriverFailure {
    let code = match error {
        ERROR_ACCESS_DENIED => "steam_driver_access_denied",
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => "steam_driver_inf_not_found",
        _ => "steam_driver_install_failed",
    };
    crate::steam_driver::DriverFailure::native(code, error)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryExW(file_name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const i8) -> Option<unsafe extern "system" fn()>;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

fn retry_pending_route_restore_once() -> bool {
    if !route_restore_pending() {
        return false;
    }

    // Each attempt owns its apartment and endpoint enumerator on this worker
    // thread; COM references never cross from a capture thread or iteration.
    let Ok(_apartment) = Apartment::start() else {
        return true;
    };
    let Ok(enumerator) = enumerator() else {
        return true;
    };
    let Some(mutex) = ROUTE_POLICY.get() else {
        return false;
    };
    let mut policy = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if policy.endpoint_with_pending_restore().is_none() {
        return false;
    }

    !restore_pending(&enumerator, &mut policy).unwrap_or(false)
}

pub fn route_restore_pending() -> bool {
    ROUTE_POLICY.get().is_some_and(|mutex| {
        let policy = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        policy.endpoint_with_pending_restore().is_some()
    })
}

struct Apartment;
impl Apartment {
    fn start() -> AudioResult<Self> {
        // SAFETY: dedicated thread initializes once, balanced by its guard.
        // STA also satisfies IAudioClient's Windows 8 first-use requirement.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|_| "com_unavailable")?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct TaskString(PWSTR);
impl TaskString {
    fn text(&self) -> AudioResult<String> {
        if self.0.is_null() {
            return Err("invalid_endpoint_metadata");
        }
        // SAFETY: COM returns a NUL-terminated allocation; stop at its terminator.
        // The metadata cap avoids unbounded control replies from endpoint names.
        unsafe {
            for length in 0..4096 {
                if self.0.0.add(length).read() == 0 {
                    return String::from_utf16(std::slice::from_raw_parts(self.0.0, length))
                        .map_err(|_| "invalid_endpoint_metadata");
                }
            }
        }
        Err("endpoint_metadata_limit")
    }
}

impl Drop for TaskString {
    fn drop(&mut self) {
        // SAFETY: endpoint and property helpers transfer one task allocation.
        unsafe {
            CoTaskMemFree(Some(self.0.0.cast()));
        }
    }
}

fn enumerator() -> AudioResult<IMMDeviceEnumerator> {
    // SAFETY: apartment is live; generated COM bindings own the result.
    unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|_| "endpoint_enumeration_failed")
    }
}

fn identifier(device: &IMMDevice) -> AudioResult<String> {
    // SAFETY: GetId transfers a task allocation, freed by TaskString.
    unsafe { TaskString(device.GetId().map_err(|_| "endpoint_metadata_failed")?).text() }
}

pub fn enumerate() -> AudioResult<Vec<Endpoint>> {
    let _apartment = Apartment::start()?;
    let enumerator = enumerator()?;
    active_render_endpoints(&enumerator)
}

fn active_render_endpoints(enumerator: &IMMDeviceEnumerator) -> AudioResult<Vec<Endpoint>> {
    // SAFETY: active render endpoint enumeration opens no capture stream.
    unsafe {
        let default = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .ok()
            .and_then(|device| identifier(&device).ok());
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|_| "endpoint_enumeration_failed")?;
        let count = collection
            .GetCount()
            .map_err(|_| "endpoint_enumeration_failed")?;
        if count > 64 {
            return Err("endpoint_limit");
        }
        let mut endpoints = Vec::new();
        for index in 0..count {
            let device = collection
                .Item(index)
                .map_err(|_| "endpoint_enumeration_failed")?;
            let id = identifier(&device)?;
            let properties = device
                .OpenPropertyStore(STGM_READ)
                .map_err(|_| "endpoint_metadata_failed")?;
            let name = property_text(&properties, &PKEY_Device_FriendlyName)
                .ok_or("endpoint_metadata_failed")?;
            let description = property_text(&properties, &PKEY_Device_DeviceDesc);
            let adapter_name = property_text(&properties, &PKEY_DeviceInterface_FriendlyName);
            endpoints.push(Endpoint {
                default_multimedia: default.as_ref() == Some(&id),
                id,
                name,
                description,
                adapter_name,
            });
        }
        Ok(endpoints)
    }
}

fn default_render_endpoint_is_active(
    enumerator: &IMMDeviceEnumerator,
    endpoints: &[Endpoint],
    role: ERole,
) -> bool {
    // SAFETY: querying the default render endpoint does not open a capture
    // stream, and the returned COM reference stays on this apartment thread.
    unsafe {
        enumerator
            .GetDefaultAudioEndpoint(eRender, role)
            .ok()
            .and_then(|device| identifier(&device).ok())
            .is_some_and(|id| endpoints.iter().any(|endpoint| endpoint.id == id))
    }
}

fn property_text(
    properties: &windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore,
    key: &windows::Win32::Foundation::PROPERTYKEY,
) -> Option<String> {
    // SAFETY: property store is live in its caller's apartment, and the
    // returned PROPVARIANT is converted synchronously before it is dropped.
    let value = unsafe { properties.GetValue(key).ok()? };
    // SAFETY: PropVariantToStringAlloc accepts the live PROPVARIANT and returns
    // one task allocation, owned by TaskString below.
    let text = unsafe { PropVariantToStringAlloc(&value).ok()? };
    TaskString(text).text().ok()
}

struct MixFormat(*mut WAVEFORMATEX);
impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: GetMixFormat transfers one CoTaskMem allocation.
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

impl MixFormat {
    fn valid_bits(&self) -> Option<u16> {
        if self.0.is_null() {
            return None;
        }
        // SAFETY: GetMixFormat owns a live WAVEFORMATEX plus any declared
        // WAVEFORMATEXTENSIBLE bytes until this wrapper is dropped.
        unsafe {
            let base = self.0.read_unaligned();
            let valid_bits = if base.wFormatTag == 0xfffe && base.cbSize >= 22 {
                self.0
                    .cast::<WAVEFORMATEXTENSIBLE>()
                    .read_unaligned()
                    .Samples
                    .wValidBitsPerSample
            } else {
                base.wBitsPerSample
            };
            Some(if valid_bits == 0 {
                base.wBitsPerSample
            } else {
                valid_bits
            })
        }
    }

    fn channel_layout(&self) -> AudioResult<(u32, u32)> {
        if self.0.is_null() {
            return Err("unsupported_channel_layout");
        }
        // SAFETY: GetMixFormat owns a live WAVEFORMATEX plus any declared
        // WAVEFORMATEXTENSIBLE bytes until this wrapper is dropped. Copy packed
        // structures by value rather than forming potentially unaligned refs.
        unsafe {
            let base = self.0.read_unaligned();
            let channels = u32::from(base.nChannels);
            let mask = if base.wFormatTag == 0xfffe {
                if base.cbSize < 22 {
                    return Err("unsupported_format");
                }
                let extended = self.0.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned();
                let subformat = extended.SubFormat;
                if subformat != KSDATAFORMAT_SUBTYPE_PCM
                    && subformat != KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                {
                    return Err("unsupported_format");
                }
                extended.dwChannelMask
            } else {
                if base.wFormatTag != 1 && base.wFormatTag != 3 {
                    return Err("unsupported_format");
                }
                0
            };
            let mask = if mask == 0 {
                default_channel_mask(channels).ok_or("unsupported_channel_layout")?
            } else {
                mask
            };
            if mask == 0 || mask.count_ones() != channels {
                return Err("unsupported_channel_layout");
            }
            Ok((channels, mask))
        }
    }

    #[cfg(test)]
    fn describe(&self) -> AudioResult<Format> {
        if self.0.is_null() {
            return Err("unsupported_format");
        }
        // SAFETY: GetMixFormat provides WAVEFORMATEX and cbSize extension bytes;
        // read packed structures by value, never take aligned field references.
        unsafe {
            let base = self.0.read_unaligned();
            let mut valid_bits = base.wBitsPerSample;
            let mut mask = None;
            let subtype = if base.wFormatTag == 0xfffe {
                if base.cbSize < 22 {
                    return Err("unsupported_format");
                }
                let extended = self.0.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned();
                valid_bits = extended.Samples.wValidBitsPerSample;
                mask = Some(extended.dwChannelMask);
                extended.SubFormat
            } else if base.wFormatTag == 1 {
                KSDATAFORMAT_SUBTYPE_PCM
            } else if base.wFormatTag == 3 {
                KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
            } else {
                return Err("unsupported_format");
            };
            let code = if subtype == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                && base.wBitsPerSample == 32
                && valid_bits == 32
            {
                PCM_F32
            } else if subtype == KSDATAFORMAT_SUBTYPE_PCM
                && base.wBitsPerSample == 16
                && valid_bits == 16
            {
                PCM_I16
            } else {
                return Err("unsupported_format");
            };
            let mut format = Format::new(base.nSamplesPerSec, u32::from(base.nChannels), code)?;
            if base.nBlockAlign as usize != format.block_align()
                || base.nAvgBytesPerSec != base.nSamplesPerSec * u32::from(base.nBlockAlign)
            {
                return Err("unsupported_format");
            }
            if let Some(mask) = mask {
                if mask != 0 && mask.count_ones() != format.channels {
                    return Err("unsupported_channel_layout");
                }
                format.channel_mask = mask;
            }
            Ok(format)
        }
    }
}

fn default_channel_mask(channels: u32) -> Option<u32> {
    match channels {
        1 => Some(0x0004),
        2 => Some(0x0003),
        4 => Some(0x0033),
        6 => Some(0x003f),
        8 => Some(0x063f),
        _ => None,
    }
}

fn virtual_waveformat(candidate: VirtualFormatCandidate, channels: u32) -> WAVEFORMATEXTENSIBLE {
    let sample_format = candidate.sample_format;
    let bits_per_sample = sample_format.container_bits();
    let block_align = (channels * u32::from(bits_per_sample) / 8) as u16;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xfffe,
            nChannels: channels as u16,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: bits_per_sample,
            cbSize: 22,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: sample_format.valid_bits(),
        },
        dwChannelMask: candidate.channel_mask,
        SubFormat: if sample_format.is_float() {
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            KSDATAFORMAT_SUBTYPE_PCM
        },
    }
}

fn default_render_valid_bits(enumerator: &IMMDeviceEnumerator) -> u16 {
    // SAFETY: endpoint activation and GetMixFormat are read-only COM calls in
    // the worker's live apartment. MixFormat frees the returned allocation.
    unsafe {
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) else {
            return 32;
        };
        let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) else {
            return 32;
        };
        let Ok(format) = client.GetMixFormat() else {
            return 32;
        };
        MixFormat(format).valid_bits().unwrap_or(32)
    }
}

fn set_virtual_device_layout(
    endpoint: &str,
    layout: ChannelLayout,
    preferred_valid_bits: u16,
) -> AudioResult<VirtualFormatCandidate> {
    let policy = PolicyConfig::new().map_err(|_| "virtual_sink_format_unavailable")?;
    let candidates = preferred_format_candidates(layout.channels, preferred_valid_bits)?;
    for candidate in candidates {
        let mut format = virtual_waveformat(candidate, layout.channels);
        if policy.set_device_format(endpoint, &mut format).is_ok() {
            return Ok(candidate);
        }
    }
    Err("virtual_sink_format_unavailable")
}

struct VirtualFormatLease {
    endpoint: String,
}

impl VirtualFormatLease {
    fn acquire(
        endpoint: &str,
        layout: ChannelLayout,
        preferred_valid_bits: u16,
    ) -> AudioResult<(Self, VirtualFormatCandidate)> {
        let policy =
            VIRTUAL_FORMAT_POLICY.get_or_init(|| Mutex::new(VirtualFormatPolicy::default()));
        let mut policy = policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let candidate = policy.acquire(endpoint, layout.channels, || {
            set_virtual_device_layout(endpoint, layout, preferred_valid_bits)
        })?;
        Ok((
            Self {
                endpoint: endpoint.to_owned(),
            },
            candidate,
        ))
    }
}

impl Drop for VirtualFormatLease {
    fn drop(&mut self) {
        if let Some(policy) = VIRTUAL_FORMAT_POLICY.get() {
            policy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .release(&self.endpoint);
        }
    }
}

fn validate_virtual_mix(
    mix: &MixFormat,
    layout: ChannelLayout,
    candidate: VirtualFormatCandidate,
) -> AudioResult<()> {
    let (channels, channel_mask) = mix.channel_layout()?;
    if channels != layout.channels || channel_mask != candidate.channel_mask {
        return Err("unsupported_channel_layout");
    }
    Ok(())
}

/// Ordinary endpoint loopback needs a stable format for the media pipeline.
/// Ask shared-mode WASAPI to convert the endpoint mix to 48 kHz float. Without
/// an explicit request, retain the endpoint's supported speaker layout;
/// explicit virtual sinks keep their separately selected device format path.
fn loopback_capture_format(
    mix: &MixFormat,
    requested: Option<PcmCaptureFormat>,
) -> AudioResult<(Format, WAVEFORMATEXTENSIBLE)> {
    let (endpoint_channels, endpoint_mask) = mix.channel_layout()?;
    let (channels, channel_mask) = if let Some(requested) = requested {
        requested.validate()?;
        let channel_mask = match requested.channels {
            2 => 0x0003,
            6 => 0x003f,
            8 => 0x063f,
            _ => return Err("unsupported_channel_layout"),
        };
        (requested.channels, channel_mask)
    } else {
        (endpoint_channels, endpoint_mask)
    };
    let mut format = Format::new(48_000, channels, PCM_F32)?;
    format.channel_mask = channel_mask;
    Ok((
        format,
        virtual_waveformat(
            VirtualFormatCandidate {
                sample_format: VirtualSampleFormat::Float32,
                channel_mask,
            },
            channels,
        ),
    ))
}

fn virtual_mix_error(error: &'static str, recovering: bool) -> &'static str {
    if recovering {
        "virtual_sink_format_changed"
    } else {
        error
    }
}

struct OpenSelectedOptions {
    desired_layout: Option<ChannelLayout>,
    capture_format: Option<PcmCaptureFormat>,
    preferred_valid_bits: Option<u16>,
    capture_sink_without_routing: bool,
    follow_console_default: bool,
    explicit_route: bool,
    recovering: bool,
}

pub struct Capture {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    endpoint: String,
    follow_console_default: bool,
    route_virtual_sink: bool,
    next_endpoint_check: Instant,
    buffer_errors: BufferErrorStreak,
    format: Format,
    timeline: Timeline,
    recovery_config: RecoveryConfig,
    _virtual_format_lease: Option<VirtualFormatLease>,
    _route_lease: RouteLease,
    _apartment: Apartment,
}

trait CaptureInitializer {
    type Capture;

    fn initialize(&mut self, stream_flags: u32, format: &mut WAVEFORMATEX) -> AudioResult<()>;
    fn get_capture(&mut self) -> AudioResult<Self::Capture>;
    fn start(&mut self) -> AudioResult<()>;
}

impl CaptureInitializer for IAudioClient {
    type Capture = IAudioCaptureClient;

    fn initialize(&mut self, stream_flags: u32, format: &mut WAVEFORMATEX) -> AudioResult<()> {
        // SAFETY: format belongs to the caller and remains live through this
        // synchronous shared-mode loopback initialization call.
        unsafe {
            IAudioClient::Initialize(
                self,
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                1_000_000,
                0,
                format,
                None,
            )
            .map_err(|_| "audio_initialize_failed")
        }
    }

    fn get_capture(&mut self) -> AudioResult<Self::Capture> {
        // SAFETY: Initialize succeeded on this client in the same COM apartment.
        unsafe { IAudioClient::GetService(self) }.map_err(|_| "audio_capture_unavailable")
    }

    fn start(&mut self) -> AudioResult<()> {
        // SAFETY: the initialized client and acquired capture service remain live.
        unsafe { IAudioClient::Start(self) }.map_err(|_| "audio_start_failed")
    }
}

/// Consume the newly activated client so every failed initialization or
/// startup step drops its COM reference and any capture service acquired so
/// far. Returning the owner transfers both references into `Capture`.
fn initialize_capture<C: CaptureInitializer>(
    mut client: C,
    format: &mut WAVEFORMATEX,
    stream_flags: u32,
) -> AudioResult<(C, C::Capture)> {
    client.initialize(stream_flags, format)?;
    let capture = client.get_capture()?;
    client.start()?;
    Ok((client, capture))
}

fn validate_render_device(device: &IMMDevice) -> AudioResult<()> {
    // SAFETY: the device was obtained from the current COM apartment's enumerator.
    unsafe {
        let endpoint_type: IMMEndpoint = device.cast().map_err(|_| "invalid_endpoint")?;
        if endpoint_type
            .GetDataFlow()
            .map_err(|_| "invalid_endpoint")?
            != eRender
        {
            return Err("not_render_endpoint");
        }
        Ok(())
    }
}

impl Capture {
    pub fn open(
        requested: &str,
        virtual_sink: Option<&str>,
        channels: Option<u32>,
        capture_format: Option<PcmCaptureFormat>,
        host_audio_enabled: bool,
        capture_sink_without_routing: bool,
    ) -> AudioResult<Self> {
        let apartment = Apartment::start()?;
        let enumerator = enumerator()?;
        let active_endpoints = if virtual_sink.is_some() {
            Some(active_render_endpoints(&enumerator)?)
        } else if !host_audio_enabled || requested.is_empty() || requested == "default" {
            active_render_endpoints(&enumerator).ok()
        } else {
            None
        };
        let console_default_available = active_endpoints.as_deref().is_some_and(|endpoints| {
            default_render_endpoint_is_active(&enumerator, endpoints, eConsole)
        });
        let automatic_virtual_sink = active_endpoints.as_deref().and_then(|endpoints| {
            select_policy_virtual_sink(
                endpoints,
                requested,
                virtual_sink,
                host_audio_enabled,
                console_default_available,
                capture_sink_without_routing,
            )
        });
        let uses_virtual_sink = virtual_sink.is_some() || automatic_virtual_sink.is_some();
        let effective_channels = channels.or_else(|| automatic_virtual_sink.map(|_| 2));
        let desired_layout = uses_virtual_sink
            .then(|| channel_layout(effective_channels.unwrap_or(2)))
            .transpose()?;
        if virtual_sink.is_some() && capture_sink_without_routing {
            return Err("invalid_open_policy");
        }
        let effective_capture_only =
            capture_sink_without_routing && automatic_virtual_sink.is_none();
        let explicit_route = explicit_route_requested(
            requested,
            virtual_sink.is_some(),
            automatic_virtual_sink.is_some(),
        );
        let resolve_console_default =
            super::should_resolve_console_default(requested, uses_virtual_sink);
        let follow_console_default = super::should_follow_console_default(
            requested,
            uses_virtual_sink,
            effective_capture_only,
        );
        let preferred_valid_bits = desired_layout.map(|_| default_render_valid_bits(&enumerator));
        // SAFETY: activation and capture interface references never leave this
        // apartment. The mix-format allocation stays live through Initialize.
        unsafe {
            let device = if let Some(selector) = virtual_sink {
                let endpoints = active_endpoints.as_deref().ok_or("endpoint_not_found")?;
                let selected = select_endpoint(endpoints, selector).ok_or("endpoint_not_found")?;
                let wide: Vec<u16> = selected.id.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            } else if let Some(selected) = automatic_virtual_sink {
                let wide: Vec<u16> = selected.id.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            } else if requested.is_empty() || requested == "default" {
                let role = if resolve_console_default {
                    eConsole
                } else {
                    eMultimedia
                };
                enumerator.GetDefaultAudioEndpoint(eRender, role)
            } else {
                let wide: Vec<u16> = requested.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
            .map_err(|_| "endpoint_not_found")?;
            validate_render_device(&device)?;
            let endpoint = identifier(&device)?;
            Self::open_selected(
                apartment,
                &enumerator,
                device,
                endpoint,
                OpenSelectedOptions {
                    desired_layout,
                    capture_format,
                    preferred_valid_bits,
                    capture_sink_without_routing: effective_capture_only,
                    follow_console_default,
                    explicit_route,
                    recovering: false,
                },
            )
        }
    }

    pub fn reopen(config: &RecoveryConfig) -> AudioResult<Self> {
        let apartment = Apartment::start()?;
        let enumerator = enumerator()?;
        // SAFETY: the endpoint ID was returned by this backend and all COM
        // interfaces remain local to the worker's apartment thread.
        unsafe {
            let device = if config.follow_console_default {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole)
            } else {
                let wide: Vec<u16> = config.endpoint.encode_utf16().chain(Some(0)).collect();
                enumerator.GetDevice(PCWSTR(wide.as_ptr()))
            }
            .map_err(|_| "endpoint_not_found")?;
            validate_render_device(&device)?;
            let endpoint = identifier(&device)?;
            if !config.follow_console_default && endpoint != config.endpoint {
                return Err("audio_endpoint_changed");
            }
            let desired_layout = config.virtual_channels.map(channel_layout).transpose()?;
            Self::open_selected(
                apartment,
                &enumerator,
                device,
                endpoint,
                OpenSelectedOptions {
                    desired_layout,
                    capture_format: config.capture_format,
                    preferred_valid_bits: config.preferred_valid_bits,
                    capture_sink_without_routing: config.capture_sink_without_routing,
                    follow_console_default: config.follow_console_default,
                    explicit_route: config.explicit_route,
                    recovering: true,
                },
            )
        }
    }

    pub fn recovery_config(&self) -> RecoveryConfig {
        self.recovery_config.clone()
    }

    fn open_selected(
        apartment: Apartment,
        enumerator: &IMMDeviceEnumerator,
        device: IMMDevice,
        endpoint: String,
        options: OpenSelectedOptions,
    ) -> AudioResult<Self> {
        let OpenSelectedOptions {
            desired_layout,
            capture_format,
            preferred_valid_bits,
            capture_sink_without_routing,
            follow_console_default,
            explicit_route,
            recovering,
        } = options;
        let route_virtual_sink = desired_layout.is_some();
        // SAFETY: the caller created this device and enumerator in the live
        // worker apartment; all COM operations remain on that thread.
        unsafe {
            let route_lease = RouteLease::acquire(
                enumerator,
                &endpoint,
                capture_sink_without_routing || follow_console_default,
                route_virtual_sink,
                explicit_route,
            )?;
            let (virtual_format_lease, selected_virtual_format) =
                match (desired_layout, preferred_valid_bits) {
                    (Some(layout), Some(valid_bits)) => {
                        let (lease, candidate) =
                            VirtualFormatLease::acquire(&endpoint, layout, valid_bits)?;
                        (Some(lease), Some(candidate))
                    }
                    (None, None) => (None, None),
                    _ => return Err("virtual_sink_format_unavailable"),
                };
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|_| "audio_activation_failed")?;
            let mix = MixFormat(client.GetMixFormat().map_err(|_| "unsupported_format")?);
            let mut capture_target = match (desired_layout, selected_virtual_format) {
                (Some(layout), Some(candidate)) => {
                    if let Some(requested) = capture_format {
                        requested.validate()?;
                        if requested.channels != layout.channels {
                            return Err("invalid_capture_format");
                        }
                    }
                    if let Err(error) = validate_virtual_mix(&mix, layout, candidate) {
                        // Windows can still publish the old mix immediately
                        // after SetDeviceFormat succeeds. A shared capture
                        // also cannot overwrite its peer's selected format.
                        // Retry either recovery until the requested mix is
                        // visible, while initial opens still fail promptly.
                        return Err(virtual_mix_error(error, recovering));
                    }
                    let mut format = Format::new(48_000, layout.channels, PCM_F32)?;
                    format.channel_mask = candidate.channel_mask;
                    let waveformat = virtual_waveformat(
                        VirtualFormatCandidate {
                            sample_format: VirtualSampleFormat::Float32,
                            channel_mask: candidate.channel_mask,
                        },
                        layout.channels,
                    );
                    Some((format, waveformat))
                }
                (None, None) => Some(loopback_capture_format(&mix, capture_format)?),
                _ => return Err("virtual_sink_format_unavailable"),
            };
            let (format, waveformat) = capture_target.as_mut().ok_or("unsupported_format")?;
            let mut stream_flags = AUDCLNT_STREAMFLAGS_LOOPBACK;
            stream_flags |=
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            let (client, capture) =
                initialize_capture(client, &mut waveformat.Format, stream_flags)?;
            let recovery_config = RecoveryConfig {
                endpoint: endpoint.clone(),
                follow_console_default,
                virtual_channels: desired_layout.map(|layout| layout.channels),
                capture_format,
                preferred_valid_bits,
                capture_sink_without_routing,
                explicit_route,
            };
            Ok(Self {
                capture,
                client,
                endpoint,
                follow_console_default,
                route_virtual_sink,
                next_endpoint_check: Instant::now() + DEFAULT_ENDPOINT_POLL_INTERVAL,
                buffer_errors: BufferErrorStreak::default(),
                format: *format,
                timeline: Timeline::new(),
                recovery_config,
                _virtual_format_lease: virtual_format_lease,
                _route_lease: route_lease,
                _apartment: apartment,
            })
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn read(&mut self) -> AudioResult<Option<Packet>> {
        if Instant::now() >= self.next_endpoint_check {
            self.next_endpoint_check = Instant::now() + DEFAULT_ENDPOINT_POLL_INTERVAL;
            if self.follow_console_default && !self.is_current_console_default() {
                return Err("default_endpoint_changed");
            }
            if !self.is_active_endpoint() {
                return Err("device_lost");
            }
            if self.route_virtual_sink
                && let Ok(enumerator) = enumerator()
            {
                // A virtual sink routed for this capture remains the active
                // render default while the stream owns its lease. Retry a
                // transient policy failure at the next poll.
                let _ = self._route_lease.refresh_virtual_sink(&enumerator);
            }
        }
        // SAFETY: GetBuffer/ReleaseBuffer are paired on the same worker thread;
        // copy_samples never borrows uninitialized padding or silent pointers.
        unsafe {
            if matches!(
                packet_size_after_query(
                    self.capture.GetNextPacketSize(),
                    &mut self.buffer_errors,
                    Instant::now(),
                )?,
                None | Some(0)
            ) {
                return Ok(None);
            }
            let mut pointer = std::ptr::null_mut();
            let mut frames = 0;
            let mut flags = 0;
            let mut qpc = 0;
            let acquired =
                self.capture
                    .GetBuffer(&mut pointer, &mut frames, &mut flags, None, Some(&mut qpc));
            let Some(buffer) = borrowed_packet_after_acquisition(
                &self.capture,
                acquired,
                frames,
                &mut self.buffer_errors,
            )?
            else {
                return Ok(None);
            };
            if frames == 0 {
                buffer.release()?;
                return Ok(None);
            }
            let flags = packet_flags(flags);
            let bytes = copy_samples(self.format, frames, flags, pointer);
            buffer.release()?;
            let packet = self.timeline.packet(frames, flags, bytes?, qpc)?;
            Ok(Some(packet))
        }
    }

    fn is_current_console_default(&self) -> bool {
        let Ok(enumerator) = enumerator() else {
            // A transient policy query failure does not prove that the
            // capture endpoint changed. Retry the query at the next poll.
            return true;
        };
        console_default_matches_capture(
            role_endpoint(&enumerator, DefaultRole::Console),
            &self.endpoint,
        )
    }

    fn is_active_endpoint(&self) -> bool {
        let Ok(enumerator) = enumerator() else {
            return true;
        };
        let wide: Vec<u16> = self.endpoint.encode_utf16().chain(Some(0)).collect();
        // SAFETY: the endpoint query and device state read stay on this
        // capture thread's COM apartment. A missing endpoint is lost; other
        // query failures are retried at the next poll.
        unsafe {
            let state = enumerator
                .GetDevice(PCWSTR(wide.as_ptr()))
                .and_then(|device| device.GetState())
                .map_err(|error| error.code());
            endpoint_state_is_active(state)
        }
    }
}

fn endpoint_state_is_active(state: Result<DEVICE_STATE, HRESULT>) -> bool {
    match state {
        Ok(state) => state.0 & DEVICE_STATE_ACTIVE.0 != 0,
        Err(HRESULT_NOT_FOUND) => false,
        Err(_) => true,
    }
}

fn console_default_matches_capture(
    current: AudioResult<Option<String>>,
    captured_endpoint: &str,
) -> bool {
    match current {
        Ok(Some(endpoint)) => endpoint == captured_endpoint,
        Ok(None) => false,
        Err(_) => true,
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: worker stopped polling before capture teardown. COM references
        // drop before the apartment guard, even if Stop reports device loss.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

fn explicit_route_requested(
    requested: &str,
    selected_virtual_sink: bool,
    automatic_virtual_sink: bool,
) -> bool {
    selected_virtual_sink || (!automatic_virtual_sink && !matches!(requested, "" | "default"))
}

trait CaptureBufferRelease {
    fn release_buffer(&self, frames: u32) -> AudioResult<()>;
}

impl CaptureBufferRelease for IAudioCaptureClient {
    fn release_buffer(&self, frames: u32) -> AudioResult<()> {
        // SAFETY: the capture client owns the buffer returned by GetBuffer on
        // this apartment thread; callers release it exactly once.
        unsafe { self.ReleaseBuffer(frames).map_err(capture_error) }
    }
}

struct BorrowedPacket<'a, C: CaptureBufferRelease + ?Sized> {
    capture: &'a C,
    frames: Option<u32>,
}

impl<'a, C: CaptureBufferRelease + ?Sized> BorrowedPacket<'a, C> {
    fn new(capture: &'a C, frames: u32) -> Self {
        Self {
            capture,
            frames: Some(frames),
        }
    }

    fn release(mut self) -> AudioResult<()> {
        let frames = self.frames.take().expect("capture buffer is still owned");
        self.capture.release_buffer(frames)
    }
}

impl<C: CaptureBufferRelease + ?Sized> Drop for BorrowedPacket<'_, C> {
    fn drop(&mut self) {
        if let Some(frames) = self.frames.take() {
            let _ = self.capture.release_buffer(frames);
        }
    }
}

fn borrowed_packet_after_acquisition<'a, C: CaptureBufferRelease + ?Sized>(
    capture: &'a C,
    acquired: windows::core::Result<()>,
    frames: u32,
    buffer_errors: &mut BufferErrorStreak,
) -> AudioResult<Option<BorrowedPacket<'a, C>>> {
    match acquired {
        Ok(()) => {
            buffer_errors.clear();
            Ok(Some(BorrowedPacket::new(capture, frames)))
        }
        // WASAPI may briefly return no buffer even after reporting frames.
        // A failed GetBuffer owns no buffer, so there is nothing to release.
        Err(error) if transient_buffer_error(error.code()) => {
            if buffer_errors.record(Instant::now()) {
                Err("device_lost")
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(capture_error(error)),
    }
}

fn packet_size_after_query(
    queried: windows::core::Result<u32>,
    buffer_errors: &mut BufferErrorStreak,
    now: Instant,
) -> AudioResult<Option<u32>> {
    match queried {
        Ok(size) => {
            // Empty polls are normal, but a long healthy interval must not
            // carry one old buffer error into an unrelated later failure.
            buffer_errors.expire(now);
            Ok(Some(size))
        }
        Err(error) if transient_buffer_error(error.code()) => {
            if buffer_errors.record(now) {
                Err("device_lost")
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(capture_error(error)),
    }
}

fn packet_flags(flags: u32) -> u32 {
    let mut result = 0;
    if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
        result |= AUDIO_DISCONTINUITY;
    }
    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
        result |= AUDIO_SILENT;
    }
    if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
        result |= AUDIO_TIMESTAMP_ESTIMATED;
    }
    result
}

fn capture_error(error: windows::core::Error) -> &'static str {
    if matches!(
        error.code(),
        AUDCLNT_E_DEVICE_INVALIDATED
            | AUDCLNT_E_RESOURCES_INVALIDATED
            | AUDCLNT_E_BUFFER_OPERATION_PENDING
            | AUDCLNT_E_SERVICE_NOT_RUNNING
    ) {
        "device_lost"
    } else {
        "audio_capture_failed"
    }
}

fn transient_buffer_error(code: HRESULT) -> bool {
    code == AUDCLNT_E_BUFFER_ERROR
}

#[derive(Default)]
struct BufferErrorStreak {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl BufferErrorStreak {
    fn clear(&mut self) {
        self.first = None;
        self.last = None;
    }

    fn expire(&mut self, now: Instant) {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) >= BUFFER_ERROR_RECOVERY_DELAY)
        {
            self.clear();
        }
    }

    fn record(&mut self, now: Instant) -> bool {
        self.expire(now);
        let first = *self.first.get_or_insert(now);
        self.last = Some(now);
        now.saturating_duration_since(first) >= BUFFER_ERROR_RECOVERY_DELAY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    fn endpoint_mix(
        sample_rate: u32,
        channels: u16,
        container_bits: u16,
        valid_bits: u16,
        channel_mask: u32,
    ) -> WAVEFORMATEXTENSIBLE {
        let block_align = channels * container_bits / 8;
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: 0xfffe,
                nChannels: channels,
                nSamplesPerSec: sample_rate,
                nAvgBytesPerSec: sample_rate * u32::from(block_align),
                nBlockAlign: block_align,
                wBitsPerSample: container_bits,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: valid_bits,
            },
            dwChannelMask: channel_mask,
            SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
        }
    }

    fn legacy_endpoint_mix(sample_rate: u32, channels: u16, container_bits: u16) -> WAVEFORMATEX {
        let block_align = channels * container_bits / 8;
        WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: channels,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate * u32::from(block_align),
            nBlockAlign: block_align,
            wBitsPerSample: container_bits,
            cbSize: 0,
        }
    }

    fn assert_loopback_float_target(mix: &mut WAVEFORMATEXTENSIBLE, channels: u32, mask: u32) {
        // This descriptor borrows test storage; ManuallyDrop avoids freeing it
        // as if it came from COM's GetMixFormat allocation.
        let descriptor = std::mem::ManuallyDrop::new(MixFormat(
            (mix as *mut WAVEFORMATEXTENSIBLE).cast::<WAVEFORMATEX>(),
        ));
        let (format, target) = loopback_capture_format(&descriptor, None).unwrap();
        assert_eq!(
            (
                format.sample_rate,
                format.channels,
                format.channel_mask,
                format.code()
            ),
            (48_000, channels, mask, PCM_F32)
        );
        assert_eq!(
            (
                target.Format.nSamplesPerSec,
                target.Format.nChannels,
                target.Format.wBitsPerSample,
                target.dwChannelMask,
                target.SubFormat,
            ),
            (
                48_000,
                channels as u16,
                32,
                mask,
                KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
            )
        );
    }

    struct FailingCaptureInitializer {
        drops: Rc<Cell<usize>>,
    }

    impl CaptureInitializer for FailingCaptureInitializer {
        type Capture = ();

        fn initialize(&mut self, _: u32, _: &mut WAVEFORMATEX) -> AudioResult<()> {
            Err("audio_initialize_failed")
        }

        fn get_capture(&mut self) -> AudioResult<Self::Capture> {
            panic!("capture service must not be requested after failed initialization")
        }

        fn start(&mut self) -> AudioResult<()> {
            panic!("client must not start after failed initialization")
        }
    }

    impl Drop for FailingCaptureInitializer {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    #[test]
    fn ordinary_loopback_converts_44100_hz_mix_to_supported_48khz_float() {
        let mut mix = endpoint_mix(44_100, 2, 16, 16, 0x0003);
        assert_loopback_float_target(&mut mix, 2, 0x0003);
    }

    #[test]
    fn ordinary_loopback_converts_24bit_mix_and_preserves_six_channel_layout() {
        let mut mix = endpoint_mix(48_000, 6, 24, 24, 0x003f);
        assert_loopback_float_target(&mut mix, 6, 0x003f);
    }

    #[test]
    fn ordinary_loopback_infers_default_six_channel_layout_from_zero_mask() {
        let mut mix = endpoint_mix(48_000, 6, 24, 24, 0);
        assert_loopback_float_target(&mut mix, 6, 0x003f);
    }

    #[test]
    fn ordinary_loopback_infers_default_layout_from_legacy_multichannel_mix() {
        let mut mix = legacy_endpoint_mix(48_000, 6, 24);
        let descriptor = std::mem::ManuallyDrop::new(MixFormat(&mut mix as *mut WAVEFORMATEX));
        let (format, target) = loopback_capture_format(&descriptor, None).unwrap();
        assert_eq!(
            (
                format.sample_rate,
                format.channels,
                format.channel_mask,
                format.code()
            ),
            (48_000, 6, 0x003f, PCM_F32)
        );
        assert_eq!(
            (
                target.Format.nSamplesPerSec,
                target.Format.nChannels,
                target.dwChannelMask
            ),
            (48_000, 6, 0x003f)
        );
    }

    #[test]
    fn ordinary_loopback_rejects_unrecognized_legacy_format_tag() {
        let mut mix = legacy_endpoint_mix(48_000, 6, 24);
        mix.wFormatTag = 2;
        let descriptor = std::mem::ManuallyDrop::new(MixFormat(&mut mix as *mut WAVEFORMATEX));
        assert!(matches!(
            loopback_capture_format(&descriptor, None),
            Err("unsupported_format")
        ));
    }

    #[test]
    fn requested_stereo_loopback_converts_six_channel_mix_to_48khz_float_stereo() {
        let mut mix = endpoint_mix(48_000, 6, 24, 24, 0);
        let descriptor = std::mem::ManuallyDrop::new(MixFormat(
            (&mut mix as *mut WAVEFORMATEXTENSIBLE).cast::<WAVEFORMATEX>(),
        ));
        let (format, target) = loopback_capture_format(
            &descriptor,
            Some(PcmCaptureFormat {
                sample_rate: 48_000,
                channels: 2,
                sample_format: crate::model::PcmSampleFormat::F32,
            }),
        )
        .unwrap();
        assert_eq!(
            (
                format.sample_rate,
                format.channels,
                format.channel_mask,
                format.code()
            ),
            (48_000, 2, 0x0003, PCM_F32)
        );
        assert_eq!(
            (
                target.Format.nSamplesPerSec,
                target.Format.nChannels,
                target.Format.wBitsPerSample,
                target.dwChannelMask,
                target.SubFormat
            ),
            (48_000, 2, 32, 0x0003, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)
        );
    }

    #[test]
    fn failed_wasapi_initialization_drops_new_capture_client() {
        let mut format = virtual_waveformat(
            VirtualFormatCandidate {
                sample_format: VirtualSampleFormat::Float32,
                channel_mask: 0x0003,
            },
            2,
        );
        let drops = Rc::new(Cell::new(0));
        let client = FailingCaptureInitializer {
            drops: Rc::clone(&drops),
        };
        let result = initialize_capture(
            client,
            &mut format.Format,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        );
        assert!(matches!(result, Err("audio_initialize_failed")));
        assert_eq!(drops.get(), 1, "failed initialization releases client");
    }

    #[test]
    fn route_retry_lifecycle_preserves_peer_and_rearms_after_final_release() {
        let lifecycle = Mutex::new(RouteRestoreLifecycle { active_modules: 0 });
        let shutting_down = AtomicBool::new(true);
        let pending_restore = Cell::new(false);
        let scheduled_retries = Cell::new(0);
        let final_restores = Cell::new(0);
        let start = || {
            start_route_restore_lifecycle(
                &lifecycle,
                &shutting_down,
                || pending_restore.get(),
                || {
                    scheduled_retries.set(scheduled_retries.get() + 1);
                    pending_restore.set(false);
                },
            );
        };
        let stop = || {
            stop_route_restore_lifecycle(&lifecycle, &shutting_down, || {
                final_restores.set(final_restores.get() + 1);
            });
        };

        start();
        assert!(!shutting_down.load(Ordering::Acquire));
        assert_eq!(scheduled_retries.get(), 0);
        pending_restore.set(true);
        start();
        assert_eq!(scheduled_retries.get(), 0, "peer reuses the current worker");
        stop();
        assert!(!shutting_down.load(Ordering::Acquire));
        assert_eq!(
            final_restores.get(),
            0,
            "peer close cannot stop restoration"
        );
        stop();
        assert!(shutting_down.load(Ordering::Acquire));
        assert_eq!(final_restores.get(), 1);
        assert!(
            pending_restore.get(),
            "failed final restore remains pending"
        );

        start();
        assert!(!shutting_down.load(Ordering::Acquire));
        assert_eq!(scheduled_retries.get(), 1, "reload schedules pending retry");
        assert!(!pending_restore.get(), "retry recovers the pending restore");
        stop();
        assert_eq!(final_restores.get(), 2);
        start();
        assert_eq!(scheduled_retries.get(), 1, "clean reload needs no retry");
        stop();
    }

    #[test]
    fn virtual_route_refresh_retries_ambiguous_and_ineffective_assignments() {
        let endpoint = "render.virtual";
        let original = [
            Some("render.console".to_owned()),
            Some("render.multimedia".to_owned()),
            None,
        ];
        let mut policy = RoutePolicy::new();
        let AcquirePlan::Assign(assignments) = policy
            .acquire_with_tracking(endpoint, &original, true)
            .unwrap()
        else {
            panic!("virtual sink must acquire the available default roles");
        };
        let current = RefCell::new(original.clone());
        for assignment in assignments {
            policy.record_assignment(endpoint, assignment.role).unwrap();
            current.borrow_mut()[assignment.role.index()] = Some(endpoint.to_owned());
        }

        current.borrow_mut()[DefaultRole::Console.index()] = Some("render.user".to_owned());
        let attempts = Cell::new(0);
        let refresh = |policy: &mut RoutePolicy| {
            let snapshot = current.borrow().clone();
            refresh_virtual_route(
                policy,
                endpoint,
                &snapshot,
                |role| Ok(current.borrow()[role.index()].clone()),
                |role| {
                    let attempt = attempts.get() + 1;
                    attempts.set(attempt);
                    if attempt == 1 {
                        return Err("audio_route_change_failed");
                    }
                    if attempt == 3 {
                        current.borrow_mut()[role.index()] = Some(endpoint.to_owned());
                    }
                    Ok(())
                },
            )
        };
        assert_eq!(refresh(&mut policy), Err("audio_route_change_failed"));
        assert_eq!(refresh(&mut policy), Err("audio_route_change_failed"));
        assert_eq!(current.borrow()[0].as_deref(), Some("render.user"));
        refresh(&mut policy).unwrap();
        assert_eq!(current.borrow()[0].as_deref(), Some(endpoint));
        assert_eq!(current.borrow()[2], None);
        assert_eq!(attempts.get(), 3);

        let ReleasePlan::Restore(restore) = policy.release(endpoint, &current.borrow()).unwrap()
        else {
            panic!("final lease must restore assigned roles");
        };
        assert_eq!(restore.len(), 2);
        policy.complete_restore(false);
        assert_eq!(
            policy.endpoint_with_pending_restore(),
            Some(endpoint),
            "failed release remains available for retry"
        );
        let pending = policy
            .pending_restore_assignments(&current.borrow())
            .unwrap();
        for assignment in pending {
            current.borrow_mut()[assignment.role.index()] = Some(assignment.endpoint);
        }
        policy.complete_restore(policy.pending_restore_complete(&current.borrow()));
        assert_eq!(*current.borrow(), original);
        assert_eq!(policy.endpoint_with_pending_restore(), None);
    }

    #[test]
    fn virtual_route_refresh_recovers_an_unset_owned_default_role() {
        let endpoint = "render.virtual";
        let original = [Some("render.console".to_owned()), None, None];
        let mut policy = RoutePolicy::new();
        let AcquirePlan::Assign(assignments) = policy
            .acquire_with_tracking(endpoint, &original, true)
            .unwrap()
        else {
            panic!("virtual sink must route the console role");
        };
        for assignment in assignments {
            policy.record_assignment(endpoint, assignment.role).unwrap();
        }
        let current = RefCell::new([None, None, None]);
        let assigned = Cell::new(0);
        let snapshot = current.borrow().clone();
        refresh_virtual_route(
            &mut policy,
            endpoint,
            &snapshot,
            |role| Ok(current.borrow()[role.index()].clone()),
            |role| {
                assigned.set(assigned.get() + 1);
                current.borrow_mut()[role.index()] = Some(endpoint.to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(assigned.get(), 1);
        assert_eq!(
            current.borrow()[DefaultRole::Console.index()].as_deref(),
            Some(endpoint)
        );
        let ReleasePlan::Restore(assignments) =
            policy.release(endpoint, &current.borrow()).unwrap()
        else {
            panic!("the final lease must restore its console role");
        };
        assert_eq!(
            assignments,
            vec![RoleAssignment {
                role: DefaultRole::Console,
                endpoint: "render.console".to_owned(),
            }]
        );
    }

    #[test]
    fn inactive_endpoint_requires_recovery_but_query_failures_are_retried() {
        assert!(endpoint_state_is_active(Ok(DEVICE_STATE_ACTIVE)));
        assert!(!endpoint_state_is_active(Ok(DEVICE_STATE(0))));
        assert!(!endpoint_state_is_active(Err(HRESULT_NOT_FOUND)));
        assert!(endpoint_state_is_active(Err(HRESULT(0x80004005u32 as i32))));
    }

    #[test]
    fn virtual_mix_change_is_transient_during_solo_or_shared_recovery() {
        let error = "unsupported_channel_layout";
        assert_eq!(virtual_mix_error(error, false), error);
        assert_eq!(
            virtual_mix_error(error, true),
            "virtual_sink_format_changed"
        );
    }

    #[test]
    fn automatic_virtual_selection_does_not_abandon_an_older_route() {
        assert!(!explicit_route_requested("default", false, true));
        assert!(!explicit_route_requested("render.explicit", false, true));
        assert!(!explicit_route_requested("default", false, false));
        assert!(explicit_route_requested("render.explicit", false, false));
        assert!(explicit_route_requested("default", true, false));
    }

    #[test]
    fn console_default_poll_reopens_only_for_a_confirmed_change() {
        assert!(console_default_matches_capture(
            Ok(Some("render.current".into())),
            "render.current"
        ));
        assert!(!console_default_matches_capture(
            Ok(Some("render.new".into())),
            "render.current"
        ));
        assert!(!console_default_matches_capture(Ok(None), "render.current"));
        assert!(console_default_matches_capture(
            Err("audio_route_snapshot_failed"),
            "render.current"
        ));
    }

    #[derive(Default)]
    struct FakeCaptureBuffer {
        released: std::sync::Mutex<Vec<u32>>,
    }

    impl CaptureBufferRelease for FakeCaptureBuffer {
        fn release_buffer(&self, frames: u32) -> AudioResult<()> {
            self.released.lock().unwrap().push(frames);
            Ok(())
        }
    }

    #[test]
    fn zero_frame_and_error_paths_release_acquired_buffers_once() {
        let capture = FakeCaptureBuffer::default();
        BorrowedPacket::new(&capture, 0).release().unwrap();
        BorrowedPacket::new(&capture, 12).release().unwrap();
        {
            let _buffer = BorrowedPacket::new(&capture, 4);
        }
        assert_eq!(*capture.released.lock().unwrap(), [0, 12, 4]);
    }

    #[test]
    fn suspended_or_reset_capture_enters_device_recovery() {
        for code in [
            AUDCLNT_E_DEVICE_INVALIDATED,
            AUDCLNT_E_RESOURCES_INVALIDATED,
            AUDCLNT_E_BUFFER_OPERATION_PENDING,
            AUDCLNT_E_SERVICE_NOT_RUNNING,
        ] {
            assert_eq!(
                capture_error(windows::core::Error::from_hresult(code)),
                "device_lost"
            );
        }
        assert_eq!(
            capture_error(windows::core::Error::from_hresult(AUDCLNT_E_OUT_OF_ORDER)),
            "audio_capture_failed"
        );
    }

    #[test]
    fn temporary_capture_buffer_error_waits_for_next_poll() {
        assert!(transient_buffer_error(AUDCLNT_E_BUFFER_ERROR));
        assert!(!transient_buffer_error(AUDCLNT_E_DEVICE_INVALIDATED));
        let started = Instant::now();
        let mut errors = BufferErrorStreak::default();
        assert!(!errors.record(started));
        assert!(!errors.record(started + BUFFER_ERROR_RECOVERY_DELAY - Duration::from_nanos(1)));
        assert!(errors.record(started + BUFFER_ERROR_RECOVERY_DELAY));
        errors.clear(); // A successful acquisition clears the failure streak.
        assert!(!errors.record(started + BUFFER_ERROR_RECOVERY_DELAY));

        let capture = FakeCaptureBuffer::default();
        let acquisition = borrowed_packet_after_acquisition(
            &capture,
            Err(windows::core::Error::from_hresult(AUDCLNT_E_BUFFER_ERROR)),
            8,
            &mut BufferErrorStreak::default(),
        );
        assert!(matches!(acquisition, Ok(None)));
        assert!(capture.released.lock().unwrap().is_empty());
    }

    #[test]
    fn empty_polls_do_not_hide_repeated_buffer_failures_from_recovery() {
        let started = Instant::now();
        let mut errors = BufferErrorStreak::default();
        assert!(!errors.record(started));
        assert_eq!(
            packet_size_after_query(Ok(0), &mut errors, started + Duration::from_millis(1)),
            Ok(Some(0))
        );
        assert_eq!(errors.first, Some(started));

        let buffer_error = || windows::core::Error::from_hresult(AUDCLNT_E_BUFFER_ERROR);
        assert_eq!(
            packet_size_after_query(
                Err(buffer_error()),
                &mut errors,
                started + BUFFER_ERROR_RECOVERY_DELAY - Duration::from_nanos(1),
            ),
            Ok(None)
        );
        assert_eq!(
            packet_size_after_query(Ok(0), &mut errors, started + BUFFER_ERROR_RECOVERY_DELAY),
            Ok(Some(0))
        );
        assert_eq!(
            packet_size_after_query(
                Err(buffer_error()),
                &mut errors,
                started + BUFFER_ERROR_RECOVERY_DELAY,
            ),
            Err("device_lost")
        );

        let capture = FakeCaptureBuffer::default();
        let acquired = borrowed_packet_after_acquisition(&capture, Ok(()), 4, &mut errors)
            .unwrap()
            .unwrap();
        acquired.release().unwrap();
        assert_eq!(errors.first, None);
        assert_eq!(*capture.released.lock().unwrap(), [4]);
    }

    #[test]
    fn isolated_buffer_errors_after_a_quiet_interval_do_not_trigger_recovery() {
        let started = Instant::now();
        let mut errors = BufferErrorStreak::default();
        let buffer_error = || windows::core::Error::from_hresult(AUDCLNT_E_BUFFER_ERROR);

        assert_eq!(
            packet_size_after_query(Err(buffer_error()), &mut errors, started),
            Ok(None)
        );
        assert_eq!(
            packet_size_after_query(Ok(0), &mut errors, started + BUFFER_ERROR_RECOVERY_DELAY,),
            Ok(Some(0))
        );
        assert_eq!(errors.first, None);
        assert_eq!(
            packet_size_after_query(
                Err(buffer_error()),
                &mut errors,
                started + BUFFER_ERROR_RECOVERY_DELAY + Duration::from_millis(1),
            ),
            Ok(None)
        );
    }

    #[test]
    fn route_restore_shutdown_unparks_and_joins_waiting_worker() {
        let (exited_sender, exited_receiver) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            thread::park();
            let _ = exited_sender.send(());
        });

        stop_and_join_route_restore_worker(worker);

        assert!(exited_receiver.try_recv().is_ok());
    }

    #[test]
    fn shutdown_restores_after_retry_worker_exits_even_without_a_worker() {
        let original = [
            Some("render.console".into()),
            Some("render.multimedia".into()),
            Some("render.communications".into()),
        ];
        let mut policy = RoutePolicy::new();
        assert!(matches!(
            policy.acquire("render.selected", &original).unwrap(),
            AcquirePlan::Assign(_)
        ));
        policy
            .record_assignment("render.selected", DefaultRole::Console)
            .unwrap();
        let routed = [
            Some("render.selected".into()),
            Some("render.multimedia".into()),
            Some("render.communications".into()),
        ];
        assert!(matches!(
            policy.release("render.selected", &routed).unwrap(),
            ReleasePlan::Restore(_)
        ));
        let (exited_sender, exited_receiver) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            thread::park();
            let _ = exited_sender.send(());
        });
        let mut attempts = 0;
        stop_worker_then_restore_once(
            Some(worker),
            || {
                assert!(exited_receiver.try_recv().is_ok());
                policy.endpoint_with_pending_restore().is_some()
            },
            || {
                attempts += 1;
            },
        );
        stop_worker_then_restore_once(
            None,
            || policy.endpoint_with_pending_restore().is_some(),
            || attempts += 1,
        );
        assert_eq!(attempts, 2);
        policy.complete_restore(true);
        stop_worker_then_restore_once(
            None,
            || policy.endpoint_with_pending_restore().is_some(),
            || attempts += 1,
        );
        assert_eq!(attempts, 2);
    }

    #[test]
    fn mix_format_preserves_channel_mask_and_rejects_unsupported_container_bits() {
        let mut mix = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: 0xfffe,
                nChannels: 6,
                nSamplesPerSec: 48_000,
                nAvgBytesPerSec: 48_000 * 24,
                nBlockAlign: 24,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: 32,
            },
            dwChannelMask: 0x3f,
            SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        };
        // This borrowed test descriptor is not a COM allocation; ManuallyDrop
        // prevents the production owner from freeing stack storage.
        let descriptor =
            std::mem::ManuallyDrop::new(MixFormat((&mut mix as *mut WAVEFORMATEXTENSIBLE).cast()));
        let format = descriptor.describe().unwrap();
        assert_eq!(
            (format.channels, format.channel_mask, format.code()),
            (6, 0x3f, PCM_F32)
        );
        mix.Format.wBitsPerSample = 24;
        let changed =
            std::mem::ManuallyDrop::new(MixFormat((&mut mix as *mut WAVEFORMATEXTENSIBLE).cast()));
        assert_eq!(changed.describe().unwrap_err(), "unsupported_format");
    }

    #[test]
    fn mix_format_preference_reads_extensible_valid_bits_for_default_depth_selection() {
        let mut default_mix = virtual_waveformat(
            VirtualFormatCandidate {
                sample_format: VirtualSampleFormat::Signed24In32,
                channel_mask: 0x0003,
            },
            2,
        );
        let descriptor = std::mem::ManuallyDrop::new(MixFormat(
            (&mut default_mix as *mut WAVEFORMATEXTENSIBLE).cast(),
        ));
        let preferred_bits = descriptor.valid_bits().unwrap();
        assert_eq!(preferred_bits, 24);
        assert_eq!(
            preferred_format_candidates(2, preferred_bits)
                .unwrap()
                .iter()
                .map(|candidate| candidate.sample_format)
                .collect::<Vec<_>>(),
            [
                VirtualSampleFormat::Signed24In32,
                VirtualSampleFormat::Signed24
            ]
        );
    }

    #[test]
    fn virtual_waveformats_keep_container_depth_subtype_and_valid_bits_distinct() {
        for (sample_format, expected_container_bits, expected_valid_bits, expect_float) in [
            (VirtualSampleFormat::Float32, 32, 32, true),
            (VirtualSampleFormat::Signed32, 32, 32, false),
            (VirtualSampleFormat::Signed24In32, 32, 24, false),
            (VirtualSampleFormat::Signed24, 24, 24, false),
            (VirtualSampleFormat::Signed16, 16, 16, false),
        ] {
            let waveformat = virtual_waveformat(
                VirtualFormatCandidate {
                    sample_format,
                    channel_mask: 0x003f,
                },
                6,
            );
            let container_bits = waveformat.Format.wBitsPerSample;
            let valid_bits = unsafe { waveformat.Samples.wValidBitsPerSample };
            let sample_rate = waveformat.Format.nSamplesPerSec;
            let block_align = waveformat.Format.nBlockAlign;
            let average_bytes_per_second = waveformat.Format.nAvgBytesPerSec;
            let sub_format = waveformat.SubFormat;

            assert_eq!(container_bits, expected_container_bits);
            assert_eq!(valid_bits, expected_valid_bits);
            assert_eq!(sample_rate, 48_000);
            assert_eq!(block_align, 6 * expected_container_bits / 8);
            assert_eq!(average_bytes_per_second, 48_000 * u32::from(block_align));
            assert_eq!(
                sub_format,
                if expect_float {
                    KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
                } else {
                    KSDATAFORMAT_SUBTYPE_PCM
                }
            );
        }
    }

    fn defaults() -> Vec<Option<String>> {
        let _apartment = Apartment::start().unwrap();
        let enumerator = enumerator().unwrap();
        [eConsole, eMultimedia, eCommunications]
            .into_iter()
            .map(|role| {
                // SAFETY: read-only role query in this test's live COM apartment.
                unsafe {
                    enumerator
                        .GetDefaultAudioEndpoint(eRender, role)
                        .ok()
                        .map(|device| identifier(&device).unwrap())
                }
            })
            .collect()
    }

    #[test]
    #[ignore = "requires a Windows test endpoint playing controlled audio; records loopback PCM"]
    fn explicit_endpoint_captures_without_changing_any_default_role() {
        let devices = enumerate().unwrap();
        let endpoint = devices
            .iter()
            .find(|device| !device.default_multimedia)
            .or(devices.first())
            .expect("test needs an active render endpoint");
        let before = defaults();
        let mut capture = Capture::open(&endpoint.id, None, None, None, true, true).unwrap();
        assert_eq!(capture.endpoint(), endpoint.id);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut packets = 0;
        while std::time::Instant::now() < deadline {
            if let Some(packet) = capture.read().unwrap() {
                assert_eq!(
                    packet.bytes.len(),
                    capture.format().packet_bytes(packet.frames).unwrap()
                );
                packets += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        drop(capture);
        assert_eq!(
            defaults(),
            before,
            "capture must not change default render roles"
        );
        assert!(
            packets > 0,
            "play controlled test audio on the explicitly selected endpoint"
        );
    }
}
