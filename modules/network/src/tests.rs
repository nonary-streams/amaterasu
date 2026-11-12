use super::*;
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
use amaterasu_module_sdk::network::Network;
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

struct PrivateRoot(PathBuf);

impl PrivateRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "amaterasu-network-startup-{}-{nonce}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root).unwrap();
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateRoot {
    fn drop(&mut self) {
        // Every test owns this newly created exact directory. Never change modes
        // on its ancestors or remove another test's helper materialization.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct EndpointState {
    settings: Response,
    context: Response,
    registration: Response,
    commands: Vec<Command>,
}

impl EndpointState {
    fn new(root: &Path) -> Self {
        Self {
            settings: Response::Result {
                payload: json!({"max_handles": 7}),
            },
            context: Response::Result {
                payload: json!({"schema": 1, "state_root": root.to_str().unwrap()}),
            },
            registration: Response::Ok,
            commands: Vec::new(),
        }
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: each test retains its Arc<Mutex<EndpointState>> through every
    // synchronous runtime call and until the network provider has been destroyed.
    let state = unsafe { &*context.cast::<Mutex<EndpointState>>() };
    // SAFETY: the SDK supplies borrowed command bytes for this synchronous call.
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(mut state) = state.lock() else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    state.commands.push(command.clone());
    let response = match command {
        Command::GetSettings {} => state.settings.clone(),
        Command::GetHostContext {} => state.context.clone(),
        Command::NativeRegisterTyped { .. } => state.registration.clone(),
        _ => Response::error("unexpected_command", "unexpected network startup command"),
    };
    let Ok(output) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    drop(state);
    // SAFETY: ReplySink copies the bounded response before this buffer is dropped.
    unsafe { reply.send(&output) };
    abi::STATUS_OK
}

fn runtime(state: &Arc<Mutex<EndpointState>>) -> RuntimeClient {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: Arc::as_ptr(state).cast_mut().cast(),
        invoke,
    };
    // SAFETY: callers retain the Arc through every module call and its destruction.
    unsafe { RuntimeClient::from_endpoint(endpoint) }
}

fn assert_no_registration(state: &Arc<Mutex<EndpointState>>) {
    assert!(state.lock().unwrap().commands.iter().all(|command| {
        !matches!(
            command,
            Command::NativeRegister { .. } | Command::NativeRegisterTyped { .. }
        )
    }));
}

#[test]
fn missing_or_denied_host_context_prevents_native_registration() {
    let root = PrivateRoot::new();
    for context in [
        Response::error("permission_denied", "host-state-context grant absent"),
        Response::Ok,
        Response::Result {
            payload: json!({"schema": 1}),
        },
        Response::Result {
            payload: json!({"schema": 1, "state_root": null}),
        },
    ] {
        let mut endpoint = EndpointState::new(root.path());
        endpoint.context = context;
        let state = Arc::new(Mutex::new(endpoint));

        assert!(NetworkModule::start(runtime(&state)).is_err());
        assert_no_registration(&state);
        assert!(matches!(
            state.lock().unwrap().commands.as_slice(),
            [Command::GetSettings {}, Command::GetHostContext {}]
        ));
    }
}

#[test]
fn malformed_host_context_prevents_native_registration() {
    let root = PrivateRoot::new();
    for context in [
        Value::Null,
        json!({"schema": 2, "state_root": root.path()}),
        json!({"schema": "1", "state_root": root.path()}),
        json!({"schema": 1, "state_root": 42}),
        json!({"schema": 1, "state_root": ""}),
        json!({"schema": 1, "state_root": root.path(), "untrusted_extra": true}),
    ] {
        let mut endpoint = EndpointState::new(root.path());
        endpoint.context = Response::Result { payload: context };
        let state = Arc::new(Mutex::new(endpoint));

        assert!(NetworkModule::start(runtime(&state)).is_err());
        assert_no_registration(&state);
    }
}

#[test]
fn malformed_runtime_state_paths_prevent_native_registration() {
    let root = PrivateRoot::new();
    for path in [
        "relative-state".to_owned(),
        "../relative-state".to_owned(),
        format!("{}\0invalid", root.path().display()),
        root.path()
            .join("..")
            .join("escape")
            .to_str()
            .unwrap()
            .to_owned(),
    ] {
        let mut endpoint = EndpointState::new(root.path());
        endpoint.context = Response::Result {
            payload: json!({"schema": 1, "state_root": path}),
        };
        let state = Arc::new(Mutex::new(endpoint));

        assert!(
            NetworkModule::start(runtime(&state)).is_err(),
            "invalid state root was accepted: {path:?}"
        );
        assert_no_registration(&state);
    }
}

#[test]
fn malformed_or_package_selected_paths_in_settings_fail_before_host_context() {
    let root = PrivateRoot::new();
    for settings in [
        Value::Null,
        json!([]),
        json!({"max_handles": 0}),
        json!({"max_handles": 65_537}),
        json!({"max_handles": "7"}),
        json!({"state_root": root.path()}),
        json!({"max_handles": 7, "helper_root": root.path()}),
    ] {
        let mut endpoint = EndpointState::new(root.path());
        endpoint.settings = Response::Result { payload: settings };
        let state = Arc::new(Mutex::new(endpoint));

        assert!(NetworkModule::start(runtime(&state)).is_err());
        assert_no_registration(&state);
        assert!(matches!(
            state.lock().unwrap().commands.as_slice(),
            [Command::GetSettings {}]
        ));
    }
}

#[test]
fn denied_settings_fail_before_requesting_host_context() {
    let root = PrivateRoot::new();
    let mut endpoint = EndpointState::new(root.path());
    endpoint.settings = Response::error("permission_denied", "settings unavailable");
    let state = Arc::new(Mutex::new(endpoint));

    assert!(NetworkModule::start(runtime(&state)).is_err());
    assert_no_registration(&state);
    assert!(matches!(
        state.lock().unwrap().commands.as_slice(),
        [Command::GetSettings {}]
    ));
}

#[test]
fn rejected_native_registration_fails_startup_and_releases_helper_state() {
    let root = PrivateRoot::new();
    let mut endpoint = EndpointState::new(root.path());
    endpoint.registration = Response::error("permission_denied", "native provider grant absent");
    let state = Arc::new(Mutex::new(endpoint));

    let error = NetworkModule::start(runtime(&state)).err().unwrap();
    assert!(error.contains("native registration failed"));
    assert!(matches!(
        state.lock().unwrap().commands.as_slice(),
        [
            Command::GetSettings {},
            Command::GetHostContext {},
            Command::NativeRegisterTyped { .. }
        ]
    ));
    std::fs::remove_dir_all(root.path()).expect("failed startup releases helper file locks");
}

#[test]
fn authorized_host_root_registers_the_public_contract_and_drains_lifecycle() {
    let root = PrivateRoot::new();
    let state = Arc::new(Mutex::new(EndpointState::new(root.path())));
    let mut module = NetworkModule::start(runtime(&state)).unwrap();
    let factory_address = {
        let state = state.lock().unwrap();
        let [
            Command::GetSettings {},
            Command::GetHostContext {},
            Command::NativeRegisterTyped {
                service,
                contract,
                factory,
            },
        ] = state.commands.as_slice()
        else {
            panic!("expected ordered, typed public network registration");
        };
        assert_eq!(service, amaterasu_network_api::SERVICE);
        assert_eq!(contract, amaterasu_network_api::CONTRACT);
        assert_eq!(*factory, &module.factory.api as *const _ as usize);
        *factory
    };
    // SAFETY: successful module startup retains this exact registered factory
    // through the consumer's destruction and every synchronous ABI call below.
    let factory = unsafe { &*(factory_address as *const abi::NativeServiceFactory) };
    let query = abi::NativeInterfaceQuery {
        header: AbiHeader::of::<abi::NativeInterfaceQuery>(),
        interface_major: amaterasu_network_api::MAJOR,
        min_minor: amaterasu_network_api::MINOR,
        min_size: size_of::<amaterasu_network_api::NetworkApi>() as u32,
    };
    let mut table = std::ptr::null();
    // SAFETY: principal/query/output are live synchronous borrows. On success
    // the returned table transfers one binding to the SDK consumer below.
    assert_eq!(
        unsafe {
            (factory.bind.unwrap())(
                factory.context,
                BorrowedBytes::new(b"test.network-consumer"),
                &query,
                &mut table,
            )
        },
        abi::STATUS_OK
    );
    // SAFETY: the registered public factory returned NetworkApi v1 and its
    // provider module remains alive until after this consumer is dropped.
    let network = unsafe { Network::from_native_table(table.cast()) }.unwrap();
    assert_eq!(network.capabilities().max_handles, 7);
    let expected = vec!["127.0.0.1:48000".parse().unwrap()];
    assert_eq!(
        network
            .resolve_timeout("127.0.0.1", 48000, Duration::from_millis(30))
            .unwrap(),
        expected
    );
    module.prepare_shutdown(1_000).unwrap();
    assert_eq!(
        network
            .resolve_timeout("127.0.0.1", 48000, Duration::from_millis(30))
            .unwrap(),
        expected,
        "deadline announcement preserves consumer cleanup admission"
    );
    module.tick().unwrap();
    drop(network);
    assert!(module.quiesce().unwrap());
    module.tick().unwrap();
    assert!(module.quiesce().unwrap());
    drop(module);

    #[cfg(target_os = "linux")]
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        0,
        "Linux helper preparation uses sealed memory rather than state files"
    );
    std::fs::remove_dir_all(root.path()).expect("provider destruction releases helper file locks");
}
