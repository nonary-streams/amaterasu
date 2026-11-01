//! Library ownership stays here; policy and feature behavior stay in managed libraries.
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, HostApi, ModuleApi, ModuleEntry, RuntimeApi, RuntimeEntry, STATUS_OK,
    Status,
};
use amaterasu_contracts::{
    Command, HOST_LIFECYCLE_CONTROL_PRINCIPAL, HOST_LOG_SERVICE, HostLifecycleAction, HostLogEvent,
    Response,
};
use amaterasu_logging::{HostLogService, LoggerError, ProductLogger};
use amaterasu_package::{PackageResult, VerifiedPackage, ordered_components};
use libloading::Library;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::path::Path;

pub(crate) mod management_tls;
#[cfg(test)]
mod teardown_tests;

const HOST_CONFIGURATION_COMPONENT: &str = "amaterasu.host-configuration";
const STEAM_COMPONENT: &str = "amaterasu.steam";

pub(crate) fn path_to_unicode_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "native path cannot be represented as Unicode".to_owned())
}

fn inject_package_root(
    runtime_config: &mut amaterasu_contracts::RuntimeConfig,
    root: &Path,
) -> PackageResult<()> {
    let package_root = root.canonicalize().map_err(|error| error.to_string())?;
    if !package_root.is_dir() {
        return Err("package root is not a directory".into());
    }
    // Overwrite every package-supplied host-context value after verification.
    runtime_config.host_context.package_root = Some(path_to_unicode_string(&package_root)?);
    Ok(())
}

fn inject_running_package_digest(
    runtime_config: &mut amaterasu_contracts::RuntimeConfig,
    digest: &str,
) -> PackageResult<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("verified package digest is invalid".into());
    }
    // A signed package may carry host-context fields, so overwrite the value
    // only after package verification with the digest of these loaded bytes.
    runtime_config.host_context.running_package_digest = Some(digest.into());
    Ok(())
}

fn inject_provider_incarnations<'a>(
    runtime_config: &mut amaterasu_contracts::RuntimeConfig,
    component_ids: impl IntoIterator<Item = &'a str>,
) -> PackageResult<()> {
    let mut instances = BTreeMap::new();
    let mut identifiers = BTreeSet::new();
    for component in component_ids {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        if !identifiers.insert(id.clone())
            || instances
                .insert(
                    component.to_owned(),
                    amaterasu_contracts::effect_fence::ProviderIncarnation { version: 1, id },
                )
                .is_some()
        {
            return Err("duplicate provider instance identity".into());
        }
    }
    // Never retain package-controlled identities, including on repeated loads.
    runtime_config.host_context.provider_incarnations = instances;
    Ok(())
}

pub struct LoadedComponents {
    // Fields drop in declaration order: modules must stop before their runtime disappears.
    modules: Vec<FeatureLibrary>,
    product_logger: Option<ProductLogger>,
    host_log_service: Option<HostLogService>,
    host_logs_enabled: bool,
    pending_log_policy: Option<HostLogEvent>,
    lifecycle_endpoint: HostApi,
    runtime: RuntimeLibrary,
}

impl Drop for LoadedComponents {
    fn drop(&mut self) {
        while let Some(module) = self.modules.pop() {
            drop(module);
        }
    }
}

impl LoadedComponents {
    pub fn load(
        root: &Path,
        directory: &Path,
        package: &VerifiedPackage,
        state_root: &Path,
        product_logger: Option<ProductLogger>,
        progress: &mut dyn FnMut(usize) -> PackageResult<()>,
    ) -> PackageResult<Self> {
        crate::privilege::require_user_process()?;
        if let Some(logger) = &product_logger {
            for diagnostic in &package.service_diagnostics {
                let message = format!(
                    "optional {:?} service {} for {} is absent",
                    diagnostic.transport, diagnostic.service, diagnostic.consumer
                );
                let _ = logger.emit(2, "host::services", &message);
            }
        }
        let runtime_component = package
            .manifest
            .components
            .iter()
            .find(|component| component.runtime)
            .ok_or("package has no runtime")?;
        let state_root = validated_state_root(state_root)?;
        let state_root_string = path_to_unicode_string(&state_root)?;
        let mut runtime_config = package.runtime_config.clone();
        runtime_config.host_context.schema = 1;
        runtime_config.host_context.state_root = Some(state_root_string.clone());
        runtime_config.host_lifecycle_control = true;
        runtime_config.principals.insert(
            HOST_LIFECYCLE_CONTROL_PRINCIPAL.into(),
            amaterasu_contracts::Grants::default(),
        );
        inject_package_root(&mut runtime_config, root)?;
        inject_running_package_digest(&mut runtime_config, &package.digest)?;
        inject_provider_incarnations(
            &mut runtime_config,
            package
                .manifest
                .components
                .iter()
                .filter(|component| !component.runtime)
                .map(|component| component.id.as_str()),
        )?;
        #[cfg(target_os = "linux")]
        crate::capture_authority::inject(package, &mut runtime_config.settings)?;
        if package
            .manifest
            .components
            .iter()
            .any(|component| component.id == HOST_CONFIGURATION_COMPONENT)
        {
            inject_host_configuration_root(&mut runtime_config, &state_root_string)?;
        }
        if package
            .manifest
            .components
            .iter()
            .any(|component| component.id == "amaterasu.management")
        {
            management_tls::validate(&mut runtime_config, &state_root)?;
        }
        if package
            .manifest
            .components
            .iter()
            .any(|component| component.id == STEAM_COMPONENT)
        {
            // Cache setup is optional. Always discard the package value, even
            // when the private directory is unavailable, so Steam falls back.
            let cache_parent = prepare_steam_artwork_cache_parent(&state_root)
                .and_then(|path| path_to_unicode_string(&path));
            inject_steam_artwork_cache_parent(&mut runtime_config, cache_parent.ok().as_deref())?;
        }
        let host_logs_enabled = runtime_config
            .principals
            .get("amaterasu.management")
            .is_some_and(|grants| grants.call.contains(HOST_LOG_SERVICE));
        let configuration =
            serde_json::to_vec(&runtime_config).map_err(|error| error.to_string())?;
        let mut runtime =
            RuntimeLibrary::load(&directory.join(&runtime_component.file), &configuration)?;
        let components = ordered_components(&package.manifest.components)?;
        let lifecycle_endpoint = runtime.endpoint(HOST_LIFECYCLE_CONTROL_PRINCIPAL)?;
        Self::load_features(
            runtime,
            product_logger,
            lifecycle_endpoint,
            host_logs_enabled,
            components
                .into_iter()
                .filter(|component| !component.runtime),
            |runtime, component| {
                let endpoint = runtime.endpoint(&component.id)?;
                FeatureLibrary::load(&directory.join(&component.file), &component.id, &endpoint)
            },
            progress,
        )
    }

    fn load_features<Component>(
        runtime: RuntimeLibrary,
        product_logger: Option<ProductLogger>,
        lifecycle_endpoint: HostApi,
        host_logs_enabled: bool,
        components: impl IntoIterator<Item = Component>,
        mut load: impl FnMut(&mut RuntimeLibrary, Component) -> PackageResult<FeatureLibrary>,
        progress: &mut dyn FnMut(usize) -> PackageResult<()>,
    ) -> PackageResult<Self> {
        // Own partial initialization immediately so every error path stops consumers
        // before their native providers, including progress and endpoint failures.
        let mut loaded = Self {
            modules: Vec::new(),
            product_logger,
            host_log_service: None,
            host_logs_enabled,
            pending_log_policy: None,
            lifecycle_endpoint,
            runtime,
        };
        progress(1)?;
        for (index, component) in components.into_iter().enumerate() {
            let module = load(&mut loaded.runtime, component)?;
            loaded.modules.push(module);
            progress(index + 2)?;
        }
        Ok(loaded)
    }

    pub fn tick(&mut self) -> PackageResult<()> {
        for module in &mut self.modules {
            module.tick()?;
        }
        self.drain_log_events();
        self.service_host_logs();
        Ok(())
    }

    /// Pulls a bounded batch through the generic host transport. File I/O and
    /// filtering stay on the product logger's worker thread; unavailable or
    /// saturated logging only drops records and never delays module ticks.
    fn drain_log_events(&mut self) {
        let Ok(input) = serde_json::to_vec(&Command::DrainLogEvents {}) else {
            return;
        };
        // SAFETY: the reserved endpoint belongs to this live runtime; the ABI
        // copies its bounded response before returning.
        let Ok(output) = (unsafe { self.lifecycle_endpoint.call(&input) }) else {
            return;
        };
        let Ok(Response::LogEvents { events, .. }) = serde_json::from_slice(&output) else {
            return;
        };
        let Some(logger) = &self.product_logger else {
            return;
        };
        let mut pending_policy = self.pending_log_policy.take();
        if let Some(HostLogEvent::Policy {
            revision,
            minimum_level,
            log_path,
        }) = pending_policy.take()
        {
            match logger.update_policy(revision, minimum_level, &log_path) {
                Err(LoggerError::QueueFull) => {
                    pending_policy = Some(HostLogEvent::Policy {
                        revision,
                        minimum_level,
                        log_path,
                    });
                }
                Ok(())
                | Err(
                    LoggerError::InvalidRecord
                    | LoggerError::InvalidPolicy
                    | LoggerError::WorkerUnavailable,
                ) => {}
            }
        }
        let mut policy_queue_blocked = pending_policy.is_some();
        for event in events {
            match event {
                HostLogEvent::Record {
                    level,
                    target,
                    message,
                } => {
                    if !policy_queue_blocked {
                        let _ = logger.emit(level, &target, &message);
                    }
                }
                HostLogEvent::Policy {
                    revision,
                    minimum_level,
                    log_path,
                } => {
                    let newer_than_pending = pending_policy.as_ref().is_none_or(|event| {
                        matches!(event, HostLogEvent::Policy { revision: current, .. } if revision > *current)
                    });
                    if !newer_than_pending {
                        continue;
                    }
                    match logger.update_policy(revision, minimum_level, &log_path) {
                        Ok(()) => {
                            pending_policy = None;
                            policy_queue_blocked = false;
                        }
                        Err(LoggerError::QueueFull) => {
                            pending_policy = Some(HostLogEvent::Policy {
                                revision,
                                minimum_level,
                                log_path,
                            });
                            policy_queue_blocked = true;
                        }
                        Err(
                            LoggerError::InvalidRecord
                            | LoggerError::InvalidPolicy
                            | LoggerError::WorkerUnavailable,
                        ) => {
                            pending_policy = None;
                            policy_queue_blocked = false;
                        }
                    }
                }
            }
        }
        self.pending_log_policy = pending_policy;
    }

    /// Route a bounded number of host-log requests without performing disk I/O
    /// on the host tick. RequestRouter enforces ownership and deadlines.
    fn service_host_logs(&mut self) {
        if !self.host_logs_enabled {
            return;
        }
        for _ in 0..4 {
            let result = self
                .host_log_service
                .as_ref()
                .and_then(|service| service.try_complete().ok())
                .flatten();
            let Some((request, payload)) = result else {
                break;
            };
            let _ = self.host_control(Command::Complete { request, payload });
        }
        for _ in 0..4 {
            let Some(Response::Request {
                id,
                caller,
                service,
                payload,
                ..
            }) = self.host_control(Command::Poll {})
            else {
                break;
            };
            if service != HOST_LOG_SERVICE || caller != "amaterasu.management" {
                let _ = self.host_control(Command::Complete {
                    request: id,
                    payload: json!({"ok":false,"code":"permission_denied"}),
                });
                continue;
            }
            if self.host_log_service.is_none() {
                self.host_log_service = self
                    .product_logger
                    .as_ref()
                    .and_then(|logger| HostLogService::start(logger.clone()).ok());
            }
            let submitted = self
                .host_log_service
                .as_ref()
                .is_some_and(|handler| handler.submit(id, payload).is_ok());
            if !submitted {
                let _ = self.host_control(Command::Complete {
                    request: id,
                    payload: json!({"ok":false,"code":"log_unavailable"}),
                });
            }
        }
    }

    fn host_control(&self, command: Command) -> Option<Response> {
        let input = serde_json::to_vec(&command).ok()?;
        // SAFETY: the private endpoint belongs to the live runtime and copies
        // its bounded response before returning; LoadedComponents owns both.
        let output = unsafe { self.lifecycle_endpoint.call(&input) }.ok()?;
        serde_json::from_slice(&output).ok()
    }

    pub fn health(&self) -> PackageResult<()> {
        for module in &self.modules {
            module.health()?;
        }
        Ok(())
    }

    pub fn console_endpoint(&mut self) -> PackageResult<HostApi> {
        self.runtime.endpoint("amaterasu.console")
    }

    pub fn take_host_lifecycle_action(&self) -> PackageResult<Option<HostLifecycleAction>> {
        let input = serde_json::to_vec(&Command::HostLifecycleDrain {})
            .map_err(|error| error.to_string())?;
        // SAFETY: the private endpoint belongs to this live runtime and is used
        // only while LoadedComponents keeps the runtime alive.
        let output = unsafe { self.lifecycle_endpoint.call(&input) }
            .map_err(|status| format!("host lifecycle endpoint returned status {status}"))?;
        match serde_json::from_slice(&output).map_err(|error| error.to_string())? {
            Response::HostLifecycle { action } => Ok(action),
            Response::Error { code, message } => Err(format!("{code}: {message}")),
            response => Err(format!("unexpected host lifecycle response: {response:?}")),
        }
    }
}

/// The host accepts only an existing, canonical directory supplied by its SCM
/// boundary. This prevents a module package from redirecting durable state
/// through a relative path or a reparse point.
fn validated_state_root(path: &Path) -> PackageResult<std::path::PathBuf> {
    let path_string = path_to_unicode_string(path)?;
    if !path.is_absolute() {
        return Err("runtime state root must be absolute".into());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("runtime state root is not a real directory".into());
    }
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    let canonical_string = path_to_unicode_string(&canonical)?;
    if !same_path(&canonical_string, &path_string) {
        return Err("runtime state root contains a reparse point".into());
    }
    Ok(canonical)
}

/// Only the host may select this root. A package's component settings are
/// signed configuration data, so overwrite any supplied value after package
/// verification instead of trusting it to choose a filesystem location.
fn inject_host_configuration_root(
    runtime_config: &mut amaterasu_contracts::RuntimeConfig,
    state_root: &str,
) -> PackageResult<()> {
    let settings = runtime_config
        .settings
        .entry(HOST_CONFIGURATION_COMPONENT.into())
        .or_insert_with(|| json!({}));
    let settings = settings
        .as_object_mut()
        .ok_or("host configuration component settings must be an object")?;
    settings.insert("host_state_root".into(), json!(state_root));
    Ok(())
}

fn inject_steam_artwork_cache_parent(
    runtime_config: &mut amaterasu_contracts::RuntimeConfig,
    cache_parent: Option<&str>,
) -> PackageResult<()> {
    let settings = runtime_config
        .settings
        .entry(STEAM_COMPONENT.into())
        .or_insert_with(|| json!({}));
    let settings = settings
        .as_object_mut()
        .ok_or("Steam component settings must be an object")?;
    settings.remove("steam_artwork_cache_root");
    if let Some(cache_parent) = cache_parent {
        settings.insert("steam_artwork_cache_root".into(), json!(cache_parent));
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_steam_artwork_cache_parent(state_root: &Path) -> PackageResult<std::path::PathBuf> {
    crate::state_root::prepare_steam_artwork_cache_parent(state_root)
}

#[cfg(not(windows))]
fn prepare_steam_artwork_cache_parent(state_root: &Path) -> PackageResult<std::path::PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no pointers and returns the process's effective UID.
    let user_id = unsafe { geteuid() };

    let mut parent = state_root.to_path_buf();
    for segment in [None, Some("provider-cache"), Some("steam")] {
        if let Some(segment) = segment {
            parent = parent.join(segment);
            if !parent.try_exists().map_err(|error| error.to_string())? {
                amaterasu_package::create_private_directory(&parent)?;
            }
        }
        let metadata = std::fs::symlink_metadata(&parent).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Steam artwork cache parent is not a real directory".into());
        }
        let canonical = parent.canonicalize().map_err(|error| error.to_string())?;
        if canonical != parent
            || metadata.uid() != user_id
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("Steam artwork cache parent is not private and canonical".into());
        }
        parent = canonical;
    }
    Ok(parent)
}

fn same_path(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.strip_prefix(r"\\?\")
            .unwrap_or(left)
            .replace('/', "\\")
            .eq_ignore_ascii_case(
                &right
                    .strip_prefix(r"\\?\")
                    .unwrap_or(right)
                    .replace('/', "\\"),
            )
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

struct RuntimeLibrary {
    api: RuntimeApi,
    instance: *mut c_void,
    _library: Library,
}

impl RuntimeLibrary {
    fn load(path: &Path, configuration: &[u8]) -> PackageResult<Self> {
        let library = open_verified_library(path)?;

        // SAFETY: the package was verified before executing any library code. The
        // named entry point and table layout are the trusted component's ABI contract.
        let api = unsafe {
            let entry = library
                .get::<RuntimeEntry>(b"amaterasu_runtime_v1\0")
                .map_err(|error| error.to_string())?;
            read_api_table(entry())?
        };
        let mut instance = std::ptr::null_mut();
        // SAFETY: configuration lives throughout create; output points to writable storage.
        let status = unsafe { (api.create)(BorrowedBytes::new(configuration), &mut instance) };
        require_success(status, "create runtime")?;
        if instance.is_null() {
            return Err("runtime returned a null instance".into());
        }
        Ok(Self {
            api,
            instance,
            _library: library,
        })
    }

    fn endpoint(&mut self, principal: &str) -> PackageResult<HostApi> {
        let mut endpoint = MaybeUninit::uninit();
        // SAFETY: instance belongs to this live library; endpoint storage is correctly sized.
        let status = unsafe {
            (self.api.endpoint)(
                self.instance,
                BorrowedBytes::new(principal.as_bytes()),
                endpoint.as_mut_ptr(),
            )
        };
        require_success(status, "create endpoint")?;
        // SAFETY: a successful runtime call initializes the complete endpoint table.
        let endpoint = unsafe { endpoint.assume_init() };
        if !endpoint.header.supports::<HostApi>() {
            return Err("runtime returned an incompatible endpoint".into());
        }
        Ok(endpoint)
    }
}

impl Drop for RuntimeLibrary {
    fn drop(&mut self) {
        // SAFETY: every feature module has already been destroyed by LoadedComponents.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

struct FeatureLibrary {
    id: String,
    api: ModuleApi,
    instance: *mut c_void,
    _library: Library,
}

impl FeatureLibrary {
    fn load(path: &Path, id: &str, endpoint: &HostApi) -> PackageResult<Self> {
        let library = open_verified_library(path)?;
        // SAFETY: the verified native component promises this symbol and C table layout.
        let api = unsafe {
            let entry = library
                .get::<ModuleEntry>(b"amaterasu_module_v1\0")
                .map_err(|error| error.to_string())?;
            read_api_table(entry())?
        };
        let mut instance = std::ptr::null_mut();
        // SAFETY: the module borrows the table during creation and copies it if needed later.
        let status = unsafe { (api.create)(endpoint, &mut instance) };
        require_success(status, &format!("create {id}"))?;
        if instance.is_null() {
            return Err(format!("module {id} returned a null instance"));
        }
        Ok(Self {
            id: id.to_owned(),
            api,
            instance,
            _library: library,
        })
    }

    fn tick(&mut self) -> PackageResult<()> {
        // SAFETY: this instance is exclusively borrowed, live, and owned by this library.
        let status = unsafe { (self.api.tick)(self.instance) };
        require_success(status, &format!("tick {}", self.id))
    }

    fn health(&self) -> PackageResult<()> {
        // SAFETY: no tick runs concurrently and the instance remains alive.
        let status = unsafe { (self.api.health)(self.instance) };
        require_success(status, &format!("health {}", self.id))
    }
}

impl Drop for FeatureLibrary {
    fn drop(&mut self) {
        // SAFETY: the host relinquishes this instance exactly once before unloading its library.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

fn open_verified_library(path: &Path) -> PackageResult<Library> {
    let absolute_path = path.canonicalize().map_err(|error| error.to_string())?;

    #[cfg(windows)]
    {
        const SEARCH_DLL_DIRECTORY: u32 = 0x100;
        const SEARCH_DEFAULT_DIRECTORIES: u32 = 0x1000;
        // SAFETY: only a verified package artifact is loaded; restrict dependency search.
        unsafe {
            libloading::os::windows::Library::load_with_flags(
                absolute_path,
                SEARCH_DLL_DIRECTORY | SEARCH_DEFAULT_DIRECTORIES,
            )
        }
        .map(Library::from)
        .map_err(|error| error.to_string())
    }

    #[cfg(not(windows))]
    {
        // SAFETY: the caller verified the complete package before this constructor can run.
        unsafe { Library::new(absolute_path) }.map_err(|error| error.to_string())
    }
}

unsafe fn read_api_table<T: Copy>(table: *const T) -> PackageResult<T> {
    if table.is_null() {
        return Err("library returned a null API table".into());
    }
    // SAFETY: every API table begins with an AbiHeader; check it before reading the tail.
    let header = unsafe { table.cast::<AbiHeader>().read() };
    if !header.supports::<T>() {
        return Err("incompatible library ABI version or table size".into());
    }
    // SAFETY: the trusted library advertises a complete compatible table.
    Ok(unsafe { table.read() })
}

fn require_success(status: Status, operation: &str) -> PackageResult<()> {
    if status != STATUS_OK {
        return Err(format!("{operation} failed with ABI status {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        inject_host_configuration_root, inject_package_root, inject_provider_incarnations,
        inject_running_package_digest, inject_steam_artwork_cache_parent, path_to_unicode_string,
    };
    use amaterasu_contracts::RuntimeConfig;
    use serde_json::json;

    #[test]
    fn provider_instances_are_fresh_and_replace_package_values() {
        let mut config = RuntimeConfig::default();
        inject_provider_incarnations(&mut config, ["provider", "other"]).unwrap();
        let first = config.host_context.provider_incarnations.clone();
        assert_ne!(first["provider"], first["other"]);
        assert_eq!(first["provider"].version, 1);
        assert_eq!(first["provider"].id.len(), 64);
        inject_provider_incarnations(&mut config, ["provider"]).unwrap();
        assert_ne!(
            first["provider"],
            config.host_context.provider_incarnations["provider"]
        );
        assert!(
            !config
                .host_context
                .provider_incarnations
                .contains_key("other")
        );
        assert!(inject_provider_incarnations(&mut config, ["same", "same"]).is_err());
    }

    #[test]
    fn package_root_overrides_package_controlled_host_context() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-host-package-root-{}-{}",
            std::process::id(),
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut config = RuntimeConfig::default();
        config.host_context.package_root = Some("package-controlled".into());
        inject_package_root(&mut config, &root).unwrap();
        assert_eq!(
            config.host_context.package_root.as_deref(),
            root.canonicalize().unwrap().to_str()
        );
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn running_digest_overrides_package_controlled_host_context() {
        let mut config = RuntimeConfig::default();
        config.host_context.running_package_digest = Some("package-controlled".into());
        let digest = "a".repeat(64);
        inject_running_package_digest(&mut config, &digest).unwrap();
        assert_eq!(
            config.host_context.running_package_digest.as_deref(),
            Some(digest.as_str())
        );
        assert!(inject_running_package_digest(&mut config, "invalid").is_err());
        assert_eq!(
            config.host_context.running_package_digest.as_deref(),
            Some(digest.as_str())
        );
    }

    #[test]
    fn host_configuration_root_overrides_signed_component_settings() {
        let mut runtime_config = RuntimeConfig::default();
        runtime_config.settings.insert(
            super::HOST_CONFIGURATION_COMPONENT.into(),
            json!({"host_state_root":"package-controlled","retained":true}),
        );

        inject_host_configuration_root(&mut runtime_config, r"C:\state\user").unwrap();

        assert_eq!(
            runtime_config.settings[super::HOST_CONFIGURATION_COMPONENT],
            json!({
                "host_state_root":r"C:\state\user",
                "retained":true
            })
        );
    }

    #[test]
    fn host_configuration_root_rejects_non_object_component_settings() {
        let mut runtime_config = RuntimeConfig::default();
        runtime_config.settings.insert(
            super::HOST_CONFIGURATION_COMPONENT.into(),
            json!("package-controlled"),
        );

        assert!(inject_host_configuration_root(&mut runtime_config, r"C:\state\user").is_err());
    }

    #[test]
    fn steam_cache_parent_overrides_package_path_and_falls_back_without_one() {
        let mut runtime_config = RuntimeConfig::default();
        runtime_config.settings.insert(
            super::STEAM_COMPONENT.into(),
            json!({"steam_artwork_cache_root":"package-controlled","retained":true}),
        );
        inject_steam_artwork_cache_parent(
            &mut runtime_config,
            Some("/private/state/provider-cache/steam"),
        )
        .unwrap();
        assert_eq!(
            runtime_config.settings[super::STEAM_COMPONENT],
            json!({
                "steam_artwork_cache_root":"/private/state/provider-cache/steam",
                "retained":true
            })
        );
        inject_steam_artwork_cache_parent(&mut runtime_config, None).unwrap();
        assert_eq!(
            runtime_config.settings[super::STEAM_COMPONENT],
            json!({"retained":true})
        );
    }

    #[cfg(unix)]
    #[test]
    fn steam_cache_parent_is_private_on_first_start_and_restart() {
        use std::os::unix::fs::PermissionsExt;

        let state_root = std::env::temp_dir().join(format!(
            "amaterasu-host-steam-cache-{}-{}",
            std::process::id(),
            amaterasu_package::random_identifier().unwrap()
        ));
        amaterasu_package::create_private_directory(&state_root).unwrap();
        let state_root = state_root.canonicalize().unwrap();
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::prepare_steam_artwork_cache_parent(&state_root).is_err());
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = super::prepare_steam_artwork_cache_parent(&state_root).unwrap();
        assert_eq!(first, state_root.join("provider-cache/steam"));
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            super::prepare_steam_artwork_cache_parent(&state_root).unwrap(),
            first
        );
        std::fs::set_permissions(&first, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(super::prepare_steam_artwork_cache_parent(&state_root).is_err());
        std::fs::remove_dir_all(state_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn path_to_unicode_string_rejects_non_unicode_unix_paths() {
        use std::os::unix::ffi::OsStrExt;

        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/state/\xff"));
        assert_eq!(
            path_to_unicode_string(path),
            Err("native path cannot be represented as Unicode".into())
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_to_unicode_string_rejects_non_unicode_windows_paths() {
        use std::os::windows::ffi::OsStringExt;

        let path = std::path::PathBuf::from(std::ffi::OsString::from_wide(&[0xD800]));
        assert_eq!(
            path_to_unicode_string(&path),
            Err("native path cannot be represented as Unicode".into())
        );
    }
}
