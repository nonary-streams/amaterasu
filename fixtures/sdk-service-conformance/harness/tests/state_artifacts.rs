mod support;
use amaterasu_abi::{
    BorrowedBytes, HostApi, ModuleApi, ModuleEntry, RuntimeApi, RuntimeEntry, STATUS_OK,
};
use amaterasu_contracts::{Command, Response, RuntimeConfig};
use amaterasu_module_sdk::{RuntimeClient, state::STATE_SERVICE};
use libloading::Library;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const CONSUMER_SERVICE: &str = "example.state-consumer/1";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Runtime {
    _library: Library,
    api: RuntimeApi,
    instance: *mut c_void,
}
impl Runtime {
    fn new(path: &Path, config: &RuntimeConfig) -> Self {
        // SAFETY: explicitly supplied test artifact remains loaded until after runtime destruction.
        let library = unsafe { Library::new(path) }.unwrap();
        let api = unsafe {
            *library
                .get::<RuntimeEntry>(b"amaterasu_runtime_v1\0")
                .unwrap()()
        };
        assert!(api.header.supports::<RuntimeApi>());
        let bytes = serde_json::to_vec(config).unwrap();
        let mut instance = std::ptr::null_mut();
        // SAFETY: the configuration and writable output live throughout creation.
        assert_eq!(
            unsafe { (api.create)(BorrowedBytes::new(&bytes), &mut instance) },
            STATUS_OK
        );
        Self {
            _library: library,
            api,
            instance,
        }
    }
    fn endpoint(&self, name: &str) -> HostApi {
        let mut output = std::mem::MaybeUninit::uninit();
        // SAFETY: runtime and principal bytes are live; output is writable.
        assert_eq!(
            unsafe {
                (self.api.endpoint)(
                    self.instance,
                    BorrowedBytes::new(name.as_bytes()),
                    output.as_mut_ptr(),
                )
            },
            STATUS_OK
        );
        unsafe { output.assume_init() }
    }
    fn client(&self, name: &str) -> RuntimeClient {
        // SAFETY: test drops each client before destroying this runtime.
        unsafe { RuntimeClient::from_endpoint(self.endpoint(name)) }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.instance) };
    }
}
struct Module {
    _library: Library,
    api: ModuleApi,
    instance: *mut c_void,
}
impl Module {
    fn start(path: &Path, endpoint: HostApi) -> Self {
        // SAFETY: explicit test plugin stays loaded until its instance is destroyed.
        let library = unsafe { Library::new(path) }.unwrap();
        let api = unsafe {
            *library
                .get::<ModuleEntry>(b"amaterasu_module_v1\0")
                .unwrap()()
        };
        assert!(api.header.supports::<ModuleApi>());
        let mut instance = std::ptr::null_mut();
        // SAFETY: endpoint and output are valid, runtime outlives the module.
        assert_eq!(unsafe { (api.create)(&endpoint, &mut instance) }, STATUS_OK);
        Self {
            _library: library,
            api,
            instance,
        }
    }
    fn tick(&self) {
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, STATUS_OK);
    }
}
impl Drop for Module {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.instance) };
    }
}

fn artifact(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {name} to its built artifact"))
}
fn digest(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}
fn request(client: &RuntimeClient, service: &str, operation: &str, payload: Value) -> u64 {
    match client
        .send(Command::Request {
            service: service.into(),
            payload,
            timeout_ms: 30_000,
            operation_id: operation.into(),
            parent: None,
        })
        .unwrap()
    {
        Response::Accepted { id } => id,
        response => panic!("request rejected: {response:?}"),
    }
}
fn complete(
    client: &RuntimeClient,
    provider: &Module,
    consumer: Option<&Module>,
    id: u64,
) -> Value {
    for _ in 0..20 {
        if let Some(consumer) = consumer {
            consumer.tick();
        }
        provider.tick();
        if let Response::Result { payload } =
            client.send(Command::TakeResult { request: id }).unwrap()
        {
            client
                .require_success(Command::ReleaseResult { request: id })
                .unwrap();
            return payload;
        }
    }
    panic!("request did not complete")
}
fn invoke(
    client: &RuntimeClient,
    provider: &Module,
    consumer: &Module,
    op: &str,
    payload: Value,
) -> Value {
    let id = request(client, CONSUMER_SERVICE, op, payload);
    complete(client, provider, Some(consumer), id)
}
fn run(provider_path: &Path, consumer_path: &Path, runtime_path: &Path, allow_delegation: bool) {
    let dir = std::env::temp_dir().join(format!(
        "amaterasu-state-conformance-{}-{}",
        std::process::id(),
        NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    // Package verification resolves trusted metadata, grants, and the selection
    // before the host injects its process-local State root.
    let package =
        support::Package::new(runtime_path, provider_path, consumer_path, allow_delegation);
    let verified = package.verify().unwrap();
    assert!(verified.service_diagnostics.is_empty());
    let mut config = verified.runtime_config;
    config.host_context.schema = 1;
    config.host_context.state_root = Some(dir.to_str().unwrap().into());
    let packaged_consumer = package.artifact_path(support::CONSUMER);
    assert_eq!(digest(consumer_path), digest(&packaged_consumer));
    let runtime = Runtime::new(&package.artifact_path("amaterasu.runtime"), &config);
    // This endpoint has a provide grant but is not the selected provider.
    let alternate = runtime.client("fixture.other");
    assert!(
        matches!(alternate.send(Command::Register { service: STATE_SERVICE.into() }).unwrap(), Response::Error { code, .. } if code == "permission_denied")
    );
    let provider = Module::start(
        &package.artifact_path(support::PROVIDER),
        runtime.endpoint("amaterasu.state"),
    );
    let consumer = Module::start(&packaged_consumer, runtime.endpoint("fixture.consumer"));
    let driver = runtime.client("fixture.driver");
    let other = runtime.client("fixture.other");
    let set = json!({"action":"set","key":"one","value":null,"expected_revision":0});
    if !allow_delegation {
        let denied = invoke(&driver, &provider, &consumer, "denied", set);
        assert!(
            denied["fixture_error"]
                .as_str()
                .unwrap()
                .contains("permission_denied"),
            "{denied}"
        );
    } else {
        assert_eq!(
            invoke(&driver, &provider, &consumer, "set", set.clone())["revision"],
            1
        );
        let get = json!({"action":"get","key":"one"});
        let own = invoke(&driver, &provider, &consumer, "get", get.clone());
        assert_eq!(own["found"], true);
        assert_eq!(own["value"], Value::Null);
        let id = request(&other, STATE_SERVICE, "other", get.clone());
        assert_eq!(complete(&other, &provider, None, id)["found"], false);
        let conflict = invoke(&driver, &provider, &consumer, "conflict", set);
        assert_eq!(conflict["error"]["code"], "revision_conflict");
        assert_eq!(conflict["error"]["current_revision"], 1);
        let cancelled = request(
            &driver,
            CONSUMER_SERVICE,
            "cancel",
            json!({"action":"set","key":"cancelled","value":1,"expected_revision":1}),
        );
        consumer.tick(); // A child is queued but has not reached the provider.
        driver
            .require_success(Command::Cancel { request: cancelled })
            .unwrap();
        provider.tick();
        consumer.tick();
        let check = invoke(
            &driver,
            &provider,
            &consumer,
            "after-cancel",
            json!({"action":"get","key":"cancelled"}),
        );
        assert_eq!(check["found"], false);
        driver
            .require_success(Command::ReleaseResult { request: cancelled })
            .unwrap();
        let deleted = invoke(
            &driver,
            &provider,
            &consumer,
            "delete",
            json!({"action":"delete","key":"one","expected_revision":1}),
        );
        assert_eq!(deleted["deleted"], true);
        assert_eq!(deleted["revision"], 2);
    }
    if allow_delegation {
        // Fill the broker's retained request budget without running the provider.
        // Every cancellation is released, after which admission must recover.
        let mut retained = Vec::new();
        for index in 0..1024 {
            let response = driver
                .send(Command::Request {
                    service: STATE_SERVICE.into(),
                    payload: json!({"action":"get","key":"one"}),
                    timeout_ms: 30_000,
                    operation_id: format!("capacity.{index}"),
                    parent: None,
                })
                .unwrap();
            match response {
                Response::Accepted { id } => retained.push(id),
                Response::Error { code, .. } if code == "resource_exhausted" => break,
                other => panic!("unexpected capacity response: {other:?}"),
            }
        }
        assert!(
            !retained.is_empty() && retained.len() < 1024,
            "capacity was not enforced"
        );
        for id in retained {
            driver
                .require_success(Command::Cancel { request: id })
                .unwrap();
            driver
                .require_success(Command::ReleaseResult { request: id })
                .unwrap();
        }
        let id = request(
            &driver,
            STATE_SERVICE,
            "capacity-recovered",
            json!({"action":"get","key":"one"}),
        );
        assert_eq!(complete(&driver, &provider, None, id)["found"], false);

        // Destroying a consumer with queued work retires its child before unloading.
        let parent = request(
            &driver,
            CONSUMER_SERVICE,
            "shutdown",
            json!({"action":"set","key":"shutdown","value":1,"expected_revision":2}),
        );
        consumer.tick();
        drop(consumer);
        provider.tick();
        driver
            .require_success(Command::Cancel { request: parent })
            .unwrap();
        driver
            .require_success(Command::ReleaseResult { request: parent })
            .unwrap();
        let retired_consumer = runtime.client("fixture.consumer");
        let id = request(
            &retired_consumer,
            STATE_SERVICE,
            "shutdown-check",
            json!({"action":"get","key":"shutdown"}),
        );
        assert_eq!(
            complete(&retired_consumer, &provider, None, id)["found"],
            false
        );
    } else {
        drop(consumer);
    }
    drop(provider);
    drop(runtime);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires independently built runtime, State, consumer and replacement artifacts"]
fn same_consumer_artifact_uses_real_state_and_independent_replacement() {
    let runtime = artifact("AMATERASU_CONFORMANCE_RUNTIME");
    let consumer = artifact("AMATERASU_CONFORMANCE_CONSUMER");
    let original_digest = digest(&consumer);
    for provider in [
        artifact("AMATERASU_CONFORMANCE_STATE"),
        artifact("AMATERASU_CONFORMANCE_REPLACEMENT"),
    ] {
        assert_eq!(digest(&consumer), original_digest);
        run(&provider, &consumer, &runtime, true);
        run(&provider, &consumer, &runtime, false);
        assert_eq!(digest(&consumer), original_digest);
    }
    println!(
        "unchanged consumer SHA-256: {:x}",
        Sha256::digest(std::fs::read(consumer).unwrap())
    );
}

#[test]
#[ignore = "requires independently built runtime, State and consumer artifacts"]
fn signed_package_selection_rejects_incompatible_contracts_and_invalid_policy() {
    use amaterasu_contracts::SelectedService;
    let runtime_path = artifact("AMATERASU_CONFORMANCE_RUNTIME");
    let provider = artifact("AMATERASU_CONFORMANCE_STATE");
    let consumer = artifact("AMATERASU_CONFORMANCE_CONSUMER");
    let mut package = support::Package::new(&runtime_path, &provider, &consumer, true);
    assert!(package.verify().is_ok());
    let manifest = package.manifest.clone();
    let bootstrap = package.bootstrap.clone();
    let reject = |package: &support::Package, expected: &str| {
        let error = match package.verify() {
            Ok(_) => panic!("package unexpectedly accepted"),
            Err(error) => error,
        };
        assert!(error.contains(expected), "expected {expected}, got {error}");
    };

    package.manifest.components[2].service_requires[0]
        .capabilities
        .insert("missing".into());
    reject(&package, "incompatible service");
    package.manifest = manifest.clone();
    package.manifest.components[2].service_requires[0].contract = "example.other/1".into();
    reject(&package, "incompatible service");
    package.manifest = manifest.clone();
    package.manifest.components[2].service_requires[0].major = 2;
    reject(&package, "incompatible service");
    package.manifest = manifest.clone();
    package.manifest.components[2].service_requires[0].minor = 1;
    reject(&package, "incompatible service");
    package.manifest = manifest.clone();
    package
        .bootstrap
        .policy
        .principals
        .get_mut(support::CONSUMER)
        .unwrap()
        .call
        .clear();
    reject(&package, "lacks grant");
    package.bootstrap = bootstrap.clone();
    package
        .bootstrap
        .policy
        .principals
        .get_mut(support::PROVIDER)
        .unwrap()
        .provide
        .clear();
    reject(&package, "ungranted selected provider");
    package.bootstrap = bootstrap.clone();
    package.manifest.components[2].service_requires[0].optional = true;
    package.bootstrap.policy.service_selections.broker.insert(
        support::STATE.into(),
        Some(SelectedService {
            provider: "fixture.missing".into(),
            contract: support::CONTRACT.into(),
        }),
    );
    reject(&package, "selected provider");
    package.bootstrap = bootstrap;
    package.bootstrap.policy.service_selections.broker.clear();
    let verified = package.verify().unwrap();
    assert_eq!(verified.service_diagnostics.len(), 1);
    assert_eq!(verified.service_diagnostics[0].consumer, support::CONSUMER);
    assert_eq!(
        verified
            .runtime_config
            .service_selections
            .broker
            .get(support::STATE),
        Some(&None)
    );
    let runtime = Runtime::new(&runtime_path, &verified.runtime_config);
    let provider_client = runtime.client(support::PROVIDER);
    assert!(
        matches!(provider_client.send(Command::Register { service: support::STATE.into() }).unwrap(), Response::Error { code, .. } if code == "permission_denied")
    );
}
