//! Cross-module selected-library qualification. The protocol modules and broker
//! run their production entrypoints; only hardware and persistent state are synthetic.
use super::*;
use amaterasu_module_sdk::abi::{self, BorrowedBytes, HostApi, ModuleApi, RuntimeApi, STATUS_OK};
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use serde_json::{Value, json};
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
};
mod authenticated_tls;
mod fixture_capture;
mod fixture_video;
mod full;
mod host_package;
mod rtsp_client;

const CLIENT_CERT: &str = include_str!("../../../../pairing/tests/fixtures/client-cert.pem");
const SECOND_CLIENT_CERT: &str =
    include_str!("../../../../management/tests/fixtures/browser-cert.pem");
const SECOND_CLIENT_KEY: &str =
    include_str!("../../../../management/tests/fixtures/browser-key.pem");
const HOST_CERT: &str = include_str!("../../../../pairing/tests/fixtures/host-cert.pem");
const HOST_KEY: &str = include_str!("../../../../pairing/tests/fixtures/host-key.pem");
static QUALIFICATION_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

struct LoadedModule {
    api: Option<ModuleApi>,
    instance: *mut c_void,
    _library: Option<libloading::Library>,
    _observer: Option<Box<ObserveEndpoint>>,
    host_components: Option<std::rc::Rc<std::cell::RefCell<crate::host_loading::LoadedComponents>>>,
}

struct ObserveEndpoint {
    inner: HostApi,
    observed: std::sync::Arc<
        std::sync::Mutex<Option<amaterasu_module_sdk::nvhttp_source::BindFeedbackSource>>,
    >,
}

unsafe extern "C" fn observe_invoke(
    context: *mut c_void,
    message: BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: the boxed observer and its inner endpoint outlive the module,
    // including its workers, and message bytes are borrowed only for this call.
    let observer = unsafe { &*(context.cast::<ObserveEndpoint>()) };
    if let Ok(bytes) = unsafe { message.read() }
        && let Ok(Command::Request {
            service,
            mut payload,
            ..
        }) = serde_json::from_slice(bytes)
        && service == "amaterasu.nvhttp/1"
        && payload["action"] == "bind_feedback_source"
    {
        payload
            .as_object_mut()
            .map(|object| object.remove("action"));
        if let Ok(request) = serde_json::from_value(payload)
            && let Ok(mut last) = observer.observed.lock()
        {
            *last = Some(request);
        }
    }
    // SAFETY: the unchanged request and reply sink are passed directly to the
    // original runtime-attested endpoint, which remains live through destroy.
    unsafe { (observer.inner.invoke)(observer.inner.context, message, reply) }
}

impl LoadedModule {
    fn start(path: &Path, host: &HostApi) -> Self {
        Self::start_entry(path, host, b"amaterasu_module_v1")
    }
    fn start_entry(path: &Path, host: &HostApi, symbol: &[u8]) -> Self {
        // SAFETY: This test loads locally built trusted fixtures and retains each
        // shared object until its instance has been destroyed.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        let entry = unsafe { library.get::<abi::ModuleEntry>(symbol) }.unwrap();
        let pointer = unsafe { entry() };
        assert!(!pointer.is_null());
        assert!(unsafe { (*pointer.cast::<abi::AbiHeader>()).supports::<ModuleApi>() });
        let api = unsafe { *pointer };
        let mut instance = std::ptr::null_mut();
        // SAFETY: host's runtime and endpoint outlive this module; create is serialized.
        assert_eq!(
            unsafe { (api.create)(host, &mut instance) },
            STATUS_OK,
            "{}",
            path.display()
        );
        assert!(!instance.is_null());
        Self {
            api: Some(api),
            instance,
            _library: Some(library),
            _observer: None,
            host_components: None,
        }
    }
    fn start_observed(
        path: &Path,
        host: HostApi,
        observed: std::sync::Arc<
            std::sync::Mutex<Option<amaterasu_module_sdk::nvhttp_source::BindFeedbackSource>>,
        >,
    ) -> Self {
        let (proxy, observer) = observe_host_api(host, observed);
        let mut module = Self::start(path, &proxy);
        module._observer = Some(observer);
        module
    }
    fn host_proxy(
        components: std::rc::Rc<std::cell::RefCell<crate::host_loading::LoadedComponents>>,
    ) -> Self {
        Self {
            api: None,
            instance: std::ptr::null_mut(),
            _library: None,
            _observer: None,
            host_components: Some(components),
        }
    }
    fn tick(&mut self) {
        if let Some(api) = self.api {
            // SAFETY: instance is live, belongs to this table, and calls are serialized.
            assert_eq!(unsafe { (api.tick)(self.instance) }, STATUS_OK);
        } else if let Some(components) = &self.host_components {
            components
                .borrow_mut()
                .tick()
                .expect("host components tick");
        }
    }
}
impl Drop for LoadedModule {
    fn drop(&mut self) {
        if let Some(api) = self.api {
            // SAFETY: destroy exactly once before releasing the library handle.
            unsafe { (api.destroy)(self.instance) }
        }
    }
}
fn observe_host_api(
    host: HostApi,
    observed: std::sync::Arc<
        std::sync::Mutex<Option<amaterasu_module_sdk::nvhttp_source::BindFeedbackSource>>,
    >,
) -> (HostApi, Box<ObserveEndpoint>) {
    let mut observer = Box::new(ObserveEndpoint {
        inner: host,
        observed,
    });
    let proxy = HostApi {
        header: host.header,
        context: (&mut *observer as *mut ObserveEndpoint).cast(),
        invoke: observe_invoke,
    };
    (proxy, observer)
}
struct Runtime {
    api: Option<RuntimeApi>,
    handle: *mut c_void,
    _library: Option<libloading::Library>,
    host_components: Option<std::rc::Rc<std::cell::RefCell<crate::host_loading::LoadedComponents>>>,
    // Boxed addresses are retained in HostApi contexts while this collection
    // grows; moving the observer values in a Vec would invalidate those pointers.
    #[allow(clippy::vec_box)]
    _host_observers: Option<std::rc::Rc<std::cell::RefCell<Vec<Box<ObserveEndpoint>>>>>,
}
impl Runtime {
    fn new(path: &Path, configuration: Value) -> Self {
        // SAFETY: the test owns the trusted, locally built runtime artifact.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        let entry = unsafe { library.get::<abi::RuntimeEntry>(b"amaterasu_runtime_v1") }.unwrap();
        let pointer = unsafe { entry() };
        assert!(!pointer.is_null());
        assert!(unsafe { (*pointer.cast::<abi::AbiHeader>()).supports::<RuntimeApi>() });
        let api = unsafe { *pointer };
        let bytes = serde_json::to_vec(&configuration).unwrap();
        let mut handle = std::ptr::null_mut();
        // SAFETY: bytes and output are live for create; no concurrent calls occur.
        assert_eq!(
            unsafe { (api.create)(BorrowedBytes::new(&bytes), &mut handle) },
            STATUS_OK
        );
        Self {
            api: Some(api),
            handle,
            _library: Some(library),
            host_components: None,
            _host_observers: None,
        }
    }
    #[allow(clippy::vec_box)]
    fn from_host_components(
        components: crate::host_loading::LoadedComponents,
        observers: std::rc::Rc<std::cell::RefCell<Vec<Box<ObserveEndpoint>>>>,
    ) -> Self {
        Self {
            api: None,
            handle: std::ptr::null_mut(),
            _library: None,
            host_components: Some(std::rc::Rc::new(std::cell::RefCell::new(components))),
            _host_observers: Some(observers),
        }
    }
    fn endpoint(&self, principal: &str) -> HostApi {
        if let Some(components) = &self.host_components {
            return components
                .borrow_mut()
                .test_endpoint(principal)
                .unwrap_or_else(|error| panic!("host endpoint {principal}: {error}"));
        }
        let api = self.api.expect("direct runtime API");
        let mut host = std::mem::MaybeUninit::uninit();
        // SAFETY: runtime and output are live and principal bytes are borrowed synchronously.
        assert_eq!(
            unsafe {
                (api.endpoint)(
                    self.handle,
                    BorrowedBytes::new(principal.as_bytes()),
                    host.as_mut_ptr(),
                )
            },
            STATUS_OK,
            "{principal}"
        );
        unsafe { host.assume_init() }
    }
    fn client(&self, principal: &str) -> RuntimeClient {
        // SAFETY: clients and all module instances are dropped before runtime.
        unsafe { RuntimeClient::from_endpoint(self.endpoint(principal)) }
    }
    fn host_components(
        &self,
    ) -> Option<std::rc::Rc<std::cell::RefCell<crate::host_loading::LoadedComponents>>> {
        self.host_components.clone()
    }
    fn is_host_loaded(&self) -> bool {
        self.host_components.is_some()
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(api) = self.api {
            // SAFETY: all modules and clients have been retired before runtime teardown.
            unsafe { (api.destroy)(self.handle) }
        }
        // Host observers wrap endpoints retained by LoadedComponents. Keep them
        // alive until that owner has destroyed all modules and the runtime.
        self.host_components.take();
    }
}
fn artifact(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("lib{name}.so"))
}
fn selected_module(runtime: &Runtime, path: &Path, principal: &str) -> LoadedModule {
    if let Some(components) = runtime.host_components() {
        LoadedModule::host_proxy(components)
    } else {
        LoadedModule::start(path, &runtime.endpoint(principal))
    }
}
fn register(client: &RuntimeClient, service: &str) {
    client
        .require_success(Command::Register {
            service: service.into(),
        })
        .unwrap();
}
fn broker_request(client: &RuntimeClient, service: &str, payload: Value, operation: &str) -> u64 {
    match client
        .send(Command::Request {
            service: service.into(),
            payload,
            operation_id: operation.into(),
            timeout_ms: 30_000,
            parent: None,
        })
        .unwrap()
    {
        Response::Accepted { id } => id,
        response => panic!("request rejected for {service} ({operation}): {response:?}"),
    }
}
fn take(client: &RuntimeClient, id: u64) -> Option<Value> {
    match client.send(Command::TakeResult { request: id }).unwrap() {
        Response::Pending => None,
        Response::Result { payload } => {
            client
                .require_success(Command::ReleaseResult { request: id })
                .unwrap();
            Some(payload)
        }
        other => panic!("unexpected result: {other:?}"),
    }
}

#[cfg(target_os = "linux")]
fn prepare_bootstrap_installation(package_root: &Path) -> PathBuf {
    amaterasu_package::live_feedback_bootstrap::create_new_root(package_root)
        .expect("create fresh package provenance");
    let state_root = package_root.join("data/state");
    let credentials = state_root.join("credentials");
    std::fs::create_dir_all(&credentials).expect("create test credentials directory");
    std::fs::write(credentials.join("cacert.pem"), HOST_CERT).expect("write test host certificate");
    std::fs::write(credentials.join("cakey.pem"), HOST_KEY).expect("write test host key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o700))
            .expect("restrict test credentials directory");
        std::fs::set_permissions(
            credentials.join("cakey.pem"),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("restrict test host key");
    }
    state_root
        .canonicalize()
        .expect("canonical test State root")
}

#[cfg(not(target_os = "linux"))]
fn prepare_bootstrap_installation(_package_root: &Path) -> PathBuf {
    panic!("normal-ABI feedback bootstrap qualification requires Linux")
}

fn tick_authority_modules(
    pairing: &mut LoadedModule,
    state: &mut LoadedModule,
    host_configuration: &mut Option<LoadedModule>,
) {
    pairing.tick();
    state.tick();
    if let Some(configuration) = host_configuration {
        configuration.tick();
    }
}

#[test]
#[ignore = "requires independently built protocol and fixture artifacts plus an ephemeral loopback listener"]
fn real_selected_composition() {
    let filter = std::env::var("AMATERASU_COMPOSITION_CASE").ok();
    let selected_library = std::env::var("AMATERASU_COMPOSITION_LIBRARY").ok();
    let mut executed = false;
    for (replacement, scenario) in [
        (false, "success"),
        (true, "success"),
        (true, "stale"),
        (true, "limiter_unsupported"),
        (true, "limiter_no_retirement"),
        (true, "limiter_unconfigured"),
        (true, "retirement_cancel"),
        (true, "retirement_restart"),
        (true, "retirement_restart_running"),
        (true, "retirement_uncertain_write"),
        (true, "retirement_unconfirmed"),
        (false, "limiter_success"),
        (true, "limiter_success"),
        (true, "limiter_mismatch"),
        (true, "limiter_stale"),
        (true, "limiter_stale_recheck"),
        (true, "display_rejected"),
        (true, "cancelled"),
        (true, "failed_preparation"),
        (true, "resume_changed"),
        (true, "resume_limiter"),
        (true, "resume_stale"),
        (true, "resume_foreign"),
        (true, "resume_failed"),
        (true, "resume_unpaired"),
    ] {
        if filter
            .as_deref()
            .is_none_or(|selected| selected == scenario)
            && selected_library
                .as_deref()
                .is_none_or(|selected| match selected {
                    "first" => !replacement,
                    "replacement" => replacement,
                    _ => false,
                })
        {
            executed = true;
            let first = run_composition(replacement, scenario, false, None);
            if std::env::var_os("AMATERASU_COMPOSITION_FULL").is_some()
                && std::env::var_os("AMATERASU_COMPOSITION_ONE_CLIENT").is_none()
            {
                let second = run_composition(replacement, scenario, true, first.as_ref());
                assert!(second.is_some());
            }
        }
    }
    assert!(executed, "unknown AMATERASU_COMPOSITION_CASE");
}

fn run_composition(
    replacement: bool,
    scenario: &str,
    swap_clients: bool,
    previous: Option<&amaterasu_module_sdk::nvhttp_source::BindFeedbackSource>,
) -> Option<amaterasu_module_sdk::nvhttp_source::BindFeedbackSource> {
    let artifacts = PathBuf::from(
        std::env::var_os("AMATERASU_COMPOSITION_ARTIFACT_DIR").expect("set artifact directory"),
    );
    let executor = PathBuf::from(
        std::env::var_os("AMATERASU_EXECUTOR_ARTIFACT").expect("set executor artifact"),
    );
    let temp = tempfile_dir();
    let (first_pem, first_key, second_pem, second_key) = if swap_clients {
        (
            SECOND_CLIENT_CERT,
            SECOND_CLIENT_KEY,
            CLIENT_CERT,
            authenticated_tls::CLIENT_KEY,
        )
    } else {
        (
            CLIENT_CERT,
            authenticated_tls::CLIENT_KEY,
            SECOND_CLIENT_CERT,
            SECOND_CLIENT_KEY,
        )
    };
    let (_, cert) = pem_rfc7468::decode_vec(first_pem.as_bytes()).unwrap();
    let cert_id = hex(&Sha256::digest(&cert));
    let (_, second_cert) = pem_rfc7468::decode_vec(second_pem.as_bytes()).unwrap();
    let second_cert_id = hex(&Sha256::digest(&second_cert));
    let play = std::env::var_os("AMATERASU_COMPOSITION_PLAY").is_some();
    let full = std::env::var_os("AMATERASU_COMPOSITION_FULL").is_some();
    let normal_abi = full && std::env::var_os("AMATERASU_COMPOSITION_NORMAL_ABI").is_some();
    let package_root = temp.0.join("package");
    let state_root = if normal_abi {
        prepare_bootstrap_installation(&package_root)
    } else {
        temp.0.join("state")
    };
    let streaming = crate::launch::StreamingConfig {
        capture: CaptureConfig {
            capture_provider: if play {
                CaptureProvider::PipeWire
            } else {
                CaptureProvider::Dxgi
            },
            output: "00000000:00000001/\\\\.\\DISPLAY1".into(),
            encoder: amaterasu_module_sdk::EncoderSelection::Index(0),
            audio_endpoint: "default".into(),
        },
        desktop: crate::launch::DesktopSettings::default(),
        selected_applications: vec![crate::launch::SelectedApplication {
            id: 1,
            name: "Game".into(),
            application: amaterasu_module_sdk::application_library::v2::ApplicationReference {
                library: "games".into(),
                id: "opaque/game".into(),
            },
            entry_revision: "entry:opaque".into(),
            certificates: if full {
                [cert_id.clone(), second_cert_id.clone()].into()
            } else {
                [cert_id.clone()].into()
            },
        }],
        profile: StreamProfile {
            codec: amaterasu_media::VideoCodec::H264,
            max_width: 1920,
            max_height: 1080,
            max_fps: 60,
            bitrate_kbps: 10_000,
            max_reference_frames: 1,
            packet_size: 1024,
            fec_percentage: 20,
            min_parity_shards: 2,
            max_fec_blocks: 1,
            packet_ms: 10,
            audio_bitrate: 128_000,
            encrypt_video: true,
            encrypt_audio: true,
            permit_legacy_audio_iv_wrap: false,
        },
    };
    streaming.validate().unwrap();
    let services = json!([
        "amaterasu.pairing/1",
        "amaterasu.state/1",
        "amaterasu.host-configuration/1",
        "amaterasu.launch/1",
        "amaterasu.launch/3",
        "amaterasu.rtsp/1",
        "amaterasu.session-lifecycle/1",
        "amaterasu.session/1",
        "amaterasu.stream-session/1",
        "amaterasu.input/1",
        "amaterasu.media-transport/1",
        "amaterasu.control-transport/1",
        "amaterasu.video/1",
        "amaterasu.application/1",
        "amaterasu.display/1",
        "fixture.library/2",
        "fixture.policy/2",
        "example.library.provider/1"
    ]);
    let mut config = json!({"principals":{
        "console":{"call": ["amaterasu.pairing/1","amaterasu.state/1"]},
        "attacker":{"call":["amaterasu.launch/1","amaterasu.launch/3","example.library.provider/1"]},
        "amaterasu.nvhttp":{"call":services,"native_use":["amaterasu.identity/1"]},
        "amaterasu.pairing":{"provide":["amaterasu.pairing/1"],"call":["amaterasu.state/1"],"native_provide":["amaterasu.identity/1"]},
        "amaterasu.launch":{"provide":["amaterasu.launch/1","amaterasu.launch/2","amaterasu.launch/3"],"call":["fixture.library/2","fixture.policy/2","example.library.provider/1","amaterasu.session/1","amaterasu.application/1","amaterasu.state/1"]},
        "amaterasu.rtsp":{"provide":["amaterasu.rtsp/1"],"call":["amaterasu.launch/1","amaterasu.stream-session/1","amaterasu.session/1","amaterasu.state/1","amaterasu.media-transport/1","amaterasu.control-transport/1","amaterasu.input/1","amaterasu.session-lifecycle/1","amaterasu.display/1","amaterasu.video/1"],"native_use":["amaterasu.network/1"]},
        "amaterasu.session-lifecycle":{"provide":["amaterasu.session-lifecycle/1"],"call":["amaterasu.launch/1"]},
        "network":{"native_provide":["amaterasu.network/1"]},
        "capture":{"call":["amaterasu.session-lifecycle/1"]},
        "foreign":{"call":["amaterasu.session-lifecycle/1"]},
        "amaterasu.state":{"provide":["amaterasu.state/1"]},
        "session":{"provide":["amaterasu.session/1"]},
        "stream":{"provide":["amaterasu.stream-session/1"],"call":["amaterasu.state/1","amaterasu.session/1","amaterasu.input/1","amaterasu.session-lifecycle/1","amaterasu.display/1","amaterasu.media-transport/1","amaterasu.control-transport/1","amaterasu.video/1"],"native_use":["amaterasu.media-transport/1","amaterasu.control-transport/1","amaterasu.video/1"]},
        "fixture-video":{"provide":["amaterasu.video/1"],"native_provide":["amaterasu.video/1"]},
        "control":{"provide":["amaterasu.control-transport/1"],"native_provide":["amaterasu.control-transport/1"],"native_use":["amaterasu.network/1"]},
        "media":{"provide":["amaterasu.media-transport/1"],"native_provide":["amaterasu.media-transport/1"],"native_use":["amaterasu.network/1"]},
        "input":{"provide":["amaterasu.input/1"],"call":["amaterasu.configuration/1"]},
        "amaterasu.host-configuration":{"provide":["amaterasu.host-configuration/1"],"call":["amaterasu.pairing/1"]},
        "fixture-configuration":{"provide":["amaterasu.configuration/1"]},
        "display":{"provide":["amaterasu.display/1"]},
        "application":{"provide":["amaterasu.application/1"]},
        "library":{"provide":["fixture.library/2"]},
        "policy":{"provide":["fixture.policy/2"]},
        "executor":{"provide":["example.library.provider/1"]},
        "replacement":{"provide":["fixture.library/2","fixture.policy/2","example.library.provider/1"]}
    },"settings":{
        "input":{"allow_injection":true},
        "stream":{"enabled":true,"allowed_principals":["amaterasu.nvhttp","amaterasu.rtsp"]},
        "control":{"enabled":true,"allowed_principals":["stream"]},
        "media":{"enabled":true,"allowed_principals":["stream"]},
        "amaterasu.pairing":{"administrator_principals":["console"],"transport_principals":["amaterasu.nvhttp"]},
        "amaterasu.launch":{"enable_library_preflight":true,"limiter_journal_service":if scenario == "limiter_unconfigured" { Value::Null } else { json!("amaterasu.state/1") },"retirement_observers":["amaterasu.session-lifecycle"],"library_routes":{"games":{
            "service":"fixture.library/2","callers":["amaterasu.nvhttp"],"executors":{"runner":"example.library.provider/1"},
            "streaming_policy_service":"fixture.policy/2","streaming_policy_source":"test-policy"}}},
        "amaterasu.rtsp":{"enabled":true,"bind":"127.0.0.1:0","allowed_principals":["amaterasu.nvhttp"]},
        "amaterasu.session-lifecycle":{"journal_path":temp.0.join("journal.json"),"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},"trusted_callers":["amaterasu.nvhttp","capture","foreign"],"session_delegates":["capture"],"consumers":{"capture":["capture"],"input":[]},"operation_grants":{"amaterasu.nvhttp":["prepare","prepare_stream","pause_stream","lookup","snapshot","bind_application","end_session","application_retired"],"capture":["retire_stream"],"foreign":["prepare","lookup","snapshot"]}},
        "library":{"service":"fixture.library/2","callers":{"amaterasu.launch":{"initiators":["amaterasu.nvhttp"],"resolve_launch":true}},"revision":"snapshot:one",
            "entries":[{"id":"opaque/game","revision":"entry:opaque","name":"Game","description":null,"tags":[],"source":null,"portable_ids":[],"installed":"unknown","readiness":"ready","unavailable_reason":null}],
            "targets":[{"id":"opaque/game","revision":"entry:opaque","executor":"runner","application_id":"fixture-game"}]},
        "policy":{"service":"fixture.policy/2","callers":{"amaterasu.launch":["amaterasu.nvhttp"]},"rules":[{"certificates":[cert_id],"policy":{"application":{"library":"games","id":"opaque/game"},"entry_revision":"entry:opaque","policy_revision":"policy:one","preparation":[]},"config_overrides":{},"application_display_settings":null}]},
        "executor":{"callers":{"amaterasu.launch":{"initiators":["amaterasu.nvhttp","amaterasu.launch"],"lifecycle":true}}, "limiter_mode":match scenario { "limiter_success" => "supported", "limiter_mismatch" => "mismatch", "limiter_stale" => "stale", _ => "" }},
        "replacement":{"certificate_id":cert_id,"mode":scenario,"limiter_capacity":2}
    },"host_context":{"schema":1,"state_root":state_root}});
    if play {
        fixture_capture::configure(&mut config);
    } else {
        // The harness obtains this endpoint in both modes; legacy launch does
        // not register a capture service or require capture permissions.
        config["principals"]["fixture-capture"] = json!({});
    }
    if full {
        fixture_capture::configure_input(&mut config);
    }
    if full {
        config["settings"]["policy"]["rules"][0]["certificates"] = json!([cert_id, second_cert_id]);
        configure_full(&mut config, &streaming);
    }
    let real_input = std::env::var_os("AMATERASU_COMPOSITION_REAL_INPUT").is_some();
    let real_stream = std::env::var_os("AMATERASU_COMPOSITION_REAL_STREAM").is_some();
    let observed_feedback_source = std::sync::Arc::new(std::sync::Mutex::new(None));
    let fixture_video_owner = std::rc::Rc::new(std::cell::RefCell::new(None));
    #[allow(clippy::vec_box)]
    let host_observers = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let runtime = if normal_abi {
        config["settings"]["network"] = json!({"max_handles":1024});
        let placeholder = PathBuf::from(
            std::env::var_os("AMATERASU_COMPOSITION_PLACEHOLDER_ARTIFACT")
                .expect("signed-package placeholder artifact"),
        );
        let signed = host_package::create_signed_package(
            &config,
            replacement,
            &package_root,
            &artifacts,
            &placeholder,
        );
        let fixture_video_owner = fixture_video_owner.clone();
        let hook_observers = host_observers.clone();
        let observed_feedback_source = observed_feedback_source.clone();
        let mut endpoint_hook = move |principal: &str, endpoint: HostApi| {
            // These test-only providers are registered at the package-loader
            // point where their signed principal is created. The production
            // modules consume the same broker and native endpoints unchanged.
            let client = unsafe { RuntimeClient::from_endpoint(endpoint) };
            match principal {
                "fixture-capture" if play => fixture_capture::register(&client),
                "fixture-configuration" if real_input => {
                    register(&client, "amaterasu.configuration/1")
                }
                "application" => register(&client, "amaterasu.application/1"),
                "display" => register(&client, "amaterasu.display/1"),
                "fixture-video" if play => {
                    *fixture_video_owner.borrow_mut() = Some(fixture_video::Video::start(&client));
                }
                _ => {}
            }
            if principal == "stream" {
                let (observed_endpoint, observer) =
                    observe_host_api(endpoint, observed_feedback_source.clone());
                hook_observers.borrow_mut().push(observer);
                Ok(observed_endpoint)
            } else {
                Ok(endpoint)
            }
        };
        let mut progress = |_| Ok(());
        let loaded = crate::host_loading::LoadedComponents::load_for_test(
            &package_root,
            &signed.directory,
            &signed.verified,
            &state_root,
            &mut endpoint_hook,
            &mut progress,
        )
        .unwrap_or_else(|error| panic!("production host package load: {error}"));
        let _ = signed.bootstrap;
        Runtime::from_host_components(loaded, host_observers.clone())
    } else {
        Runtime::new(&artifact(&artifacts, "amaterasu_runtime"), config)
    };
    let capture_fixture = runtime.client("fixture-capture");
    if play && !normal_abi {
        fixture_capture::register(&capture_fixture);
    }
    let host_components = runtime.host_components();
    let mut memory = host_components.as_ref().map_or_else(
        || {
            LoadedModule::start(
                &artifact(&artifacts, "amaterasu_state"),
                &runtime.endpoint("amaterasu.state"),
            )
        },
        |loaded| LoadedModule::host_proxy(loaded.clone()),
    );
    let mut selected_host_configuration = normal_abi.then(|| {
        LoadedModule::host_proxy(
            host_components
                .as_ref()
                .expect("host-loaded package")
                .clone(),
        )
    });
    let network = runtime.client("network");
    let _factory = if normal_abi {
        None
    } else {
        let factory = amaterasu_network_sockets::Factory::new();
        // SAFETY: factory lives until after RTSP and its native binding are destroyed.
        unsafe {
            network
                .register_native_typed(
                    amaterasu_network_api::SERVICE,
                    amaterasu_network_api::CONTRACT,
                    &factory.api,
                )
                .unwrap()
        };
        Some(factory)
    };
    let stream = runtime.client("stream");
    assert!(
        !play || real_stream,
        "PLAY qualification needs the selected stream"
    );
    let video_runtime = runtime.client("fixture-video");
    let mut video_fixture = if normal_abi {
        fixture_video_owner.borrow_mut().take()
    } else {
        play.then(|| fixture_video::Video::start(&video_runtime))
    };
    if !real_stream {
        register(&stream, "amaterasu.stream-session/1");
    }
    let mut selected_control = real_stream.then(|| {
        selected_module(
            &runtime,
            &artifact(&artifacts, "amaterasu_control_transport"),
            "control",
        )
    });
    let mut selected_media = real_stream.then(|| {
        selected_module(
            &runtime,
            &artifact(&artifacts, "amaterasu_media_transport"),
            "media",
        )
    });
    let mut selected_stream = (real_stream && !full).then(|| {
        selected_module(
            &runtime,
            &artifact(&artifacts, "amaterasu_stream_session"),
            "stream",
        )
    });
    let session = runtime.client("session");
    let real_session = full;
    let mut selected_session = if real_session {
        Some(selected_module(
            &runtime,
            &artifact(&artifacts, "amaterasu_session"),
            "session",
        ))
    } else {
        register(&session, "amaterasu.session/1");
        None
    };
    let input = runtime.client("input");
    let real_input = std::env::var_os("AMATERASU_COMPOSITION_REAL_INPUT").is_some();
    if !real_input {
        register(&input, "amaterasu.input/1");
    }
    let configuration = runtime.client("fixture-configuration");
    if real_input && !normal_abi {
        register(&configuration, "amaterasu.configuration/1");
    }
    let mut selected_input = real_input.then(|| {
        let input_artifact = std::env::var_os("AMATERASU_INPUT_QUALIFICATION_ARTIFACT")
            .map(PathBuf::from)
            .unwrap_or_else(|| artifact(&artifacts, "amaterasu_input"));
        let module = selected_module(&runtime, &input_artifact, "input");
        if std::env::var_os("AMATERASU_INPUT_QUALIFICATION_ALTERNATE").is_some() {
            // Inspect the package-selected artifact independently of the host's
            // module handle so this assertion also covers LoadedComponents.
            let marker_library = unsafe { libloading::Library::new(&input_artifact) }
                .expect("open alternate input artifact marker");
            let marker = unsafe {
                marker_library.get::<unsafe extern "C" fn() -> u32>(
                    b"amaterasu_input_alternate_qualification_marker",
                )
            }
            .expect("alternate backend marker");
            assert_eq!(unsafe { marker() }, 0xa17e_0001);
        }
        module
    });
    if let Some(module) = &mut selected_input {
        for _ in 0..20 {
            module.tick();
            pump_input_configuration(&configuration);
        }
    }
    let display = runtime.client("display");
    if !normal_abi {
        register(&display, "amaterasu.display/1");
    }
    let application = runtime.client("application");
    if !normal_abi {
        register(&application, "amaterasu.application/1");
    }
    let mut pairing = if let Some(components) = runtime.host_components() {
        LoadedModule::host_proxy(components)
    } else if full {
        let qualification = PathBuf::from(
            std::env::var_os("AMATERASU_FEEDBACK_QUALIFICATION_ARTIFACT_DIR")
                .expect("set qualification artifact directory"),
        );
        if let Some(path) = std::env::var_os("AMATERASU_PAIRING_NORMAL_ARTIFACT") {
            LoadedModule::start(&PathBuf::from(path), &runtime.endpoint("amaterasu.pairing"))
        } else {
            LoadedModule::start_entry(
                &artifact(&qualification, "amaterasu_pairing"),
                &runtime.endpoint("amaterasu.pairing"),
                b"amaterasu_qualification_module_v1",
            )
        }
    } else {
        LoadedModule::start(
            &artifact(&artifacts, "amaterasu_pairing"),
            &runtime.endpoint("amaterasu.pairing"),
        )
    };
    if normal_abi {
        let owner = runtime.client("stream");
        let mut pending = broker_request(
            &owner,
            "amaterasu.pairing-effects/1",
            json!({"action":"status"}),
            "host-loaded-first-boot-status-0",
        );
        let mut recovered = None;
        for attempt in 0..5_000 {
            tick_authority_modules(&mut pairing, &mut memory, &mut selected_host_configuration);
            if let Some(status) = take(&owner, pending) {
                if status["startup_recovery_required"] == false
                    && status["bound_sources"] == 0
                    && status["fenced"] == false
                {
                    recovered = Some(status);
                    break;
                }
                pending = broker_request(
                    &owner,
                    "amaterasu.pairing-effects/1",
                    json!({"action":"status"}),
                    &format!("host-loaded-first-boot-status-{}", attempt + 1),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            recovered.is_some(),
            "normal host-loaded startup did not complete Pairing recovery"
        );
    }
    let mut launch = selected_module(
        &runtime,
        &artifact(&artifacts, "amaterasu_launch"),
        "amaterasu.launch",
    );
    let mut rtsp = selected_module(
        &runtime,
        &artifact(&artifacts, "amaterasu_rtsp"),
        "amaterasu.rtsp",
    );
    let mut lifecycle = selected_module(
        &runtime,
        &artifact(&artifacts, "amaterasu_session_lifecycle"),
        "amaterasu.session-lifecycle",
    );
    let library_artifact = PathBuf::from(
        std::env::var_os("AMATERASU_LIBRARY_ARTIFACT").expect("set library artifact"),
    );
    let policy_artifact = PathBuf::from(
        std::env::var_os("AMATERASU_STREAMING_POLICY_ARTIFACT")
            .expect("set streaming policy artifact"),
    );
    let replacement_artifact = PathBuf::from(
        std::env::var_os("AMATERASU_REPLACEMENT_ARTIFACT").expect("set replacement artifact"),
    );
    let mut library =
        (!replacement).then(|| selected_module(&runtime, &library_artifact, "library"));
    let mut policy = (!replacement).then(|| selected_module(&runtime, &policy_artifact, "policy"));
    let mut generated =
        replacement.then(|| selected_module(&runtime, &replacement_artifact, "replacement"));
    let mut provider = (!replacement).then(|| selected_module(&runtime, &executor, "executor"));
    let console = runtime.client("console");
    if full && !normal_abi {
        // This test owns a fresh runtime, fresh state directory and no prior
        // transports. Recover the initial empty authority before provisioning.
        let owner = runtime.client("stream");
        let pending = broker_request(
            &owner,
            "amaterasu.pairing-effects/1",
            json!({"action":"status"}),
            "initial-authority-status",
        );
        let mut status = None;
        for _ in 0..500 {
            tick_authority_modules(&mut pairing, &mut memory, &mut selected_host_configuration);
            if let Some(value) = take(&owner, pending) {
                status = Some(value);
                break;
            }
        }
        let status = status.expect("initial authority status");
        assert_eq!(status["startup_recovery_required"], true, "{status}");
        assert_eq!(status["bound_sources"], 0, "{status}");
        let pending = broker_request(
            &owner,
            "amaterasu.pairing-effects/1",
            json!({"action":"recover","epoch":status["epoch"],"owner_epoch":"fresh-fixture"}),
            "initial-authority-recover",
        );
        let mut recovered = None;
        for _ in 0..500 {
            tick_authority_modules(&mut pairing, &mut memory, &mut selected_host_configuration);
            if let Some(value) = take(&owner, pending) {
                recovered = Some(value);
                break;
            }
        }
        assert_eq!(
            recovered.as_ref().map(|value| &value["ok"]),
            Some(&json!(true)),
            "{recovered:?}"
        );
    }
    let mut identities = vec![
        json!({"action":"import_identity","certificate_pem":HOST_CERT,"private_key_pem":HOST_KEY}),
        json!({"action":"import_client","client_id":"test-client","name":"Client","certificate_pem":first_pem}),
    ];
    if full {
        identities.push(json!({"action":"import_client","client_id":"second-client","name":"Second","certificate_pem":second_pem}));
    }
    for (sequence, payload) in identities.into_iter().enumerate() {
        let pending = broker_request(
            &console,
            "amaterasu.pairing/1",
            payload,
            &format!("pairing-{sequence}"),
        );
        let mut result = None;
        for _ in 0..500 {
            tick_authority_modules(&mut pairing, &mut memory, &mut selected_host_configuration);
            if let Some(value) = take(&console, pending) {
                result = Some(value);
                break;
            }
        }
        assert_eq!(
            result.as_ref().map(|value| &value["ok"]),
            Some(&json!(true)),
            "{result:?}"
        );
    }
    if !replacement {
        // Obtain a real owner-bound token, then present it from another broker
        // principal to the real Launch service. The token alone is not authority.
        let nvhttp = runtime.client("amaterasu.nvhttp");
        let attacker = runtime.client("attacker");
        let epoch_request = broker_request(
            &nvhttp,
            "amaterasu.launch/1",
            json!({"action":"operation_epoch"}),
            "fixture-epoch",
        );
        let mut epoch = None;
        for _ in 0..100 {
            launch.tick();
            memory.tick();
            if let Some(value) = take(&nvhttp, epoch_request) {
                epoch = value["operation_epoch"].as_str().map(str::to_owned);
                break;
            }
        }
        let operation = format!("ale1:{}:foreign-test", epoch.expect("launch epoch"));
        let application = json!({"library":"games","id":"opaque/game"});
        let prepare = broker_request(
            &nvhttp,
            "amaterasu.launch/3",
            json!({"action":"prepare_streaming","operation_id":operation,"application":application,"entry_revision":"entry:opaque","certificate_id":cert_id}),
            "fixture-preflight",
        );
        let mut preflight = None;
        for _ in 0..200 {
            launch.tick();
            memory.tick();
            if let Some(module) = &mut library {
                module.tick();
            }
            if let Some(module) = &mut policy {
                module.tick();
            }
            if let Some(value) = take(&nvhttp, prepare) {
                preflight = Some(value);
                break;
            }
        }
        let preflight = preflight.expect("selected preflight");
        assert_eq!(preflight["admission_capable"], true, "{preflight}");
        let forged = broker_request(
            &attacker,
            "amaterasu.launch/3",
            json!({"action":"accept_streaming","operation_id":operation,"application":application,"entry_revision":"entry:opaque","certificate_id":cert_id,"preflight_token":preflight["preflight_token"],"policy_identity":preflight["policy_identity"],"frame_limiter_settings":{},"client_frame_limiter_overrides":{}}),
            "fixture-cross-owner",
        );
        let mut rejected = None;
        for _ in 0..100 {
            launch.tick();
            memory.tick();
            if let Some(value) = take(&attacker, forged) {
                rejected = Some(value);
                break;
            }
        }
        let rejected = rejected.expect("cross-owner result");
        assert_eq!(rejected["code"], "permission_denied", "{rejected}");
    }
    if full {
        let source = full::run(full::Context {
            runtime: &runtime,
            console: &console,
            pairing: &mut pairing,
            memory: &mut memory,
            host_configuration: &mut selected_host_configuration,
            launch: &mut launch,
            lifecycle: &mut lifecycle,
            rtsp: &mut rtsp,
            selected_session: &mut selected_session,
            selected_stream: &mut selected_stream,
            selected_control: &mut selected_control,
            selected_media: &mut selected_media,
            selected_input: &mut selected_input,
            library: &mut library,
            policy: &mut policy,
            generated: &mut generated,
            provider: &mut provider,
            video_fixture: &mut video_fixture,
            observed: observed_feedback_source.clone(),
            configuration: &configuration,
            capture_fixture: &capture_fixture,
            video_runtime: &video_runtime,
            session: &session,
            stream: &stream,
            input: &input,
            application: &application,
            display: &display,
            cert_id: &cert_id,
            cert: &cert,
            second_cert: &second_cert,
            second_cert_id: &second_cert_id,
            second_pem,
            first_key,
            second_key,
            previous,
        });
        return Some(source);
    }
    let mut controller = LaunchController::new(Some(streaming)).unwrap();
    controller.set_session_lifecycle_required(true);
    let mut router = Router::new(
        RouterConfig {
            hostname: "Test".into(),
            http_port: 47989,
            https_port: 47984,
        },
        controller,
    );
    if real_stream {
        router.set_global_stream_audio(false);
    }
    let display_policy = if scenario == "display_rejected" {
        json!({"dd_configuration_option":"ensure_active"})
    } else {
        json!({})
    };
    router.set_global_display_policy(1, display_policy);
    if scenario.starts_with("limiter_")
        || matches!(
            scenario,
            "resume_limiter"
                | "retirement_cancel"
                | "retirement_restart"
                | "retirement_restart_running"
                | "retirement_uncertain_write"
                | "retirement_unconfirmed"
        )
    {
        router.set_global_frame_limiter_settings(&crate::frame_limiter::FrameLimiterSettings {
            frame_limiter_enable: Some(true),
            ..Default::default()
        });
    }
    if matches!(scenario, "retirement_restart" | "retirement_unconfirmed") {
        router.launches.pause_selected_after_accept_for_test();
    }
    let nvhttp = runtime.client("amaterasu.nvhttp");
    let target =
        "/launch?appid=1&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17";
    let use_tls = std::env::var_os("AMATERASU_COMPOSITION_USE_TLS").is_some();
    let identity = use_tls.then(|| crate::tls::Identity::bind(&nvhttp).unwrap());
    let mut tls = identity.as_ref().map(|identity| {
        let (_, host_der) = pem_rfc7468::decode_vec(HOST_CERT.as_bytes()).unwrap();
        authenticated_tls::Connection::new(identity, &host_der, &cert, target)
    });
    let (request_id, actual_request, peer) =
        if let (Some(tls), Some(identity)) = (&mut tls, &identity) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let incoming = loop {
                let mut events = tls.tick(identity);
                if let Some(incoming) = events.requests.pop() {
                    break incoming;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "TLS launch request timed out"
                );
                std::thread::yield_now();
            };
            assert!(incoming.secure);
            assert_eq!(incoming.peer_certificate.as_deref(), Some(cert.as_slice()));
            let trusted = incoming
                .peer_certificate
                .as_ref()
                .is_some_and(|cert| identity.trusted(cert));
            let allowed = |permission| {
                incoming
                    .peer_certificate
                    .as_ref()
                    .is_some_and(|cert| identity.allowed(cert, permission))
            };
            let context = RequestContext {
                secure: incoming.secure,
                remote: incoming.remote,
                peer_trusted: trusted,
                list_allowed: trusted && allowed(Permission::List),
                launch_allowed: trusted && allowed(Permission::Launch),
                peer_certificate: incoming.peer_certificate,
            };
            assert!(context.peer_trusted && context.launch_allowed);
            (incoming.id, incoming.request, context)
        } else {
            // The legacy fixture injects transport trust and is not TLS qualification.
            (
                1,
                request(target),
                RequestContext {
                    secure: true,
                    remote: "127.0.0.1".parse().unwrap(),
                    peer_certificate: Some(cert.clone()),
                    peer_trusted: true,
                    list_allowed: true,
                    launch_allowed: true,
                },
            )
        };
    assert!(
        router
            .dispatch(request_id, actual_request, peer, &nvhttp)
            .is_none()
    );
    let mut backends = Hardware {
        real_input,
        real_stream,
        real_session,
        ..Hardware::default()
    };
    let mut replies = vec![];
    let mut cancelled = false;
    for turn in 0..5000 {
        if let Some(video) = &mut video_fixture {
            video.pump(&video_runtime);
        }
        if let Some(module) = &mut selected_session {
            module.tick();
        }
        if let Some(module) = &mut selected_stream {
            module.tick();
        }
        if let Some(module) = &mut selected_control {
            module.tick();
        }
        if let Some(module) = &mut selected_media {
            module.tick();
        }
        if let Some(module) = &mut selected_input {
            module.tick();
            pump_input_configuration(&configuration);
        }
        if let (Some(tls), Some(identity)) = (&mut tls, &identity) {
            let events = tls.tick(identity);
            assert!(events.requests.is_empty());
            assert!(
                events.closed.is_empty(),
                "TLS connection closed before launch response"
            );
        }
        tick_authority_modules(&mut pairing, &mut memory, &mut selected_host_configuration);
        if scenario == "retirement_uncertain_write"
            && !cancelled
            && router.launches.selected_preflight_state_for_test() == Some((true, false))
        {
            let coordinator = runtime.client("amaterasu.launch");
            let probe = broker_request(
                &coordinator,
                "amaterasu.state/1",
                json!({"action":"snapshot","cursor":null,"limit":128}),
                &format!("uncertain-probe-{turn}"),
            );
            memory.tick();
            let snapshot = take(&coordinator, probe).expect("journal probe");
            if snapshot["values"]
                .as_object()
                .unwrap()
                .values()
                .any(|entry| entry["phase"] == "reservation_pending")
            {
                // State committed, but Launch has not consumed the write reply.
                router.cancel(1, &nvhttp);
                cancelled = true;
            }
        }
        launch.tick();
        lifecycle.tick();
        if play {
            fixture_capture::pump(&capture_fixture);
        }
        if let Some(module) = &mut library {
            module.tick();
        }
        if let Some(module) = &mut policy {
            module.tick();
        }
        if let Some(module) = &mut generated {
            module.tick();
        }
        if let Some(module) = &mut provider {
            module.tick();
        }
        rtsp.tick();
        backends.pump(&session, &stream, &input, &application, &display);
        replies.extend(router.poll_with_trust(&nvhttp, |presented, permission| {
            identity.as_ref().map_or(presented == cert, |identity| {
                identity.allowed(presented, permission)
            })
        }));
        if scenario == "retirement_cancel"
            && !cancelled
            && router.launches.selected_preflight_state_for_test() == Some((true, false))
        {
            router.cancel(1, &nvhttp);
            cancelled = true;
        }
        if scenario == "retirement_restart"
            && !cancelled
            && router.launches.selected_preflight_state_for_test() == Some((true, true))
        {
            // Restart Launch and State; the executor and its reservation stay live.
            let cancel = router
                .launches
                .selected_preflight_cancel_for_test()
                .unwrap();
            drop(launch);
            drop(memory);
            memory = LoadedModule::start(
                &artifact(&artifacts, "amaterasu_state"),
                &runtime.endpoint("amaterasu.state"),
            );
            launch = LoadedModule::start(
                &artifact(&artifacts, "amaterasu_launch"),
                &runtime.endpoint("amaterasu.launch"),
            );
            let attacker = runtime.client("attacker");
            let stolen = broker_request(
                &attacker,
                "amaterasu.launch/3",
                cancel.clone(),
                "restart-foreign-owner",
            );
            let stolen = await_launch(
                &attacker,
                stolen,
                &mut launch,
                &mut generated,
                &mut provider,
                &mut memory,
            );
            assert_ne!(stolen["cancelled"], true, "{stolen}");
            // Startup recovery must retire the durable obligation even before
            // NVHTTP sends a cancellation for it.
            let coordinator = runtime.client("amaterasu.launch");
            let snapshot = broker_request(
                &coordinator,
                "amaterasu.state/1",
                json!({"action":"snapshot","cursor":null,"limit":128}),
                "startup-recovered-journal",
            );
            memory.tick();
            let snapshot = take(&coordinator, snapshot).expect("startup journal snapshot");
            assert!(
                snapshot["values"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|entry| entry["phase"] == "retired"),
                "{snapshot}"
            );
            let mut changed = cancel;
            changed["preflight_token"] = json!("f".repeat(64));
            let changed = broker_request(
                &nvhttp,
                "amaterasu.launch/3",
                changed,
                "restart-foreign-token",
            );
            let changed = await_launch(
                &nvhttp,
                changed,
                &mut launch,
                &mut generated,
                &mut provider,
                &mut memory,
            );
            assert_ne!(changed["cancelled"], true, "{changed}");
            router.cancel(1, &nvhttp);
            cancelled = true;
        }
        if scenario == "retirement_unconfirmed"
            && !cancelled
            && router.launches.selected_preflight_state_for_test() == Some((true, true))
        {
            drop(launch);
            launch = LoadedModule::start(
                &artifact(&artifacts, "amaterasu_launch"),
                &runtime.endpoint("amaterasu.launch"),
            );
            router.cancel(1, &nvhttp);
            cancelled = true;
        }
        if scenario == "cancelled"
            && !cancelled
            && backends.calls.iter().any(|(service, payload)| {
                service == "stream" && payload["action"] == "reserve_history_activity"
            })
        {
            router.cancel(request_id, &nvhttp);
            cancelled = true;
        }
        if scenario == "retirement_unconfirmed" {
            if router.launches.selected_preflight_cleanup_for_test()
                == Some("preflight_retirement_unconfirmed")
            {
                break;
            }
        } else if matches!(
            scenario,
            "cancelled" | "retirement_cancel" | "retirement_restart" | "retirement_uncertain_write"
        ) {
            if backends.calls.iter().any(|(service, payload)| {
                service == "stream" && payload["action"] == "release_history_activity"
            }) {
                break;
            }
        } else if !replies.is_empty() {
            break;
        }
    }
    if let (Some(tls), Some(identity), Some((id, reply))) = (&mut tls, &identity, replies.first()) {
        let outbound = Reply {
            status: reply.status,
            content_type: reply.content_type,
            body: reply.body.clone(),
            requires_paired_tls: reply.requires_paired_tls,
            permission: reply.permission,
        };
        assert!(
            tls.server
                .respond_checked(*id, outbound, |cert, permission| permission.map_or_else(
                    || identity.trusted(cert),
                    |permission| identity.allowed(cert, permission)
                ))
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut response = Vec::new();
        loop {
            tls.tick(identity);
            tls.read_plaintext(&mut response);
            if response
                .windows(reply.body.len())
                .any(|window| window == reply.body)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "TLS launch response timed out"
            );
            std::thread::yield_now();
        }
        assert!(response.starts_with(format!("HTTP/1.1 {}", reply.status).as_bytes()));
    }
    let selected_stream_prepared = if let Some(module) = &mut selected_stream {
        let stream_owner = runtime.client("amaterasu.rtsp");
        let request = broker_request(
            &stream_owner,
            "amaterasu.stream-session/1",
            json!({"action":"status"}),
            "selected-stream-status",
        );
        let mut status = None;
        for _ in 0..200 {
            module.tick();
            if let Some(value) = take(&stream_owner, request) {
                status = Some(value);
                break;
            }
        }
        let status = status.expect("selected stream status");
        assert_eq!(status["ok"], true, "{status}");
        let session = status["sessions"].as_array().and_then(|sessions| {
            sessions
                .iter()
                .find(|session| matches!(session["state"].as_str(), Some("prepared" | "armed")))
        });
        if let Some(session) = session {
            assert!(session["display_prepare_timeout_ms"].is_null(), "{session}");
        }
        session.is_some()
    } else {
        false
    };
    let prepared = selected_stream_prepared
        || backends
            .calls
            .iter()
            .any(|(service, payload)| service == "stream" && payload["action"] == "prepare");
    if play {
        let body = std::str::from_utf8(&replies.first().expect("launch reply").1.body).unwrap();
        let url = body
            .split_once("<sessionUrl0>")
            .and_then(|(_, tail)| tail.split_once("</sessionUrl0>"))
            .map(|(value, _)| value.to_owned())
            .expect("RTSP URL in launch reply");
        let _control_client = rtsp_client::play(&url, || {
            if let Some(module) = &mut selected_session {
                module.tick();
            }
            if let Some(module) = &mut selected_stream {
                module.tick();
            }
            if let Some(module) = &mut selected_control {
                module.tick();
            }
            if let Some(module) = &mut selected_media {
                module.tick();
            }
            if let Some(module) = &mut selected_input {
                module.tick();
                pump_input_configuration(&configuration);
            }
            if let Some(video) = &mut video_fixture {
                video.pump(&video_runtime);
            }
            tick_authority_modules(&mut pairing, &mut memory, &mut selected_host_configuration);
            launch.tick();
            lifecycle.tick();
            rtsp.tick();
            fixture_capture::pump(&capture_fixture);
            if let Some(module) = &mut library {
                module.tick();
            }
            if let Some(module) = &mut policy {
                module.tick();
            }
            if let Some(module) = &mut generated {
                module.tick();
            }
            if let Some(module) = &mut provider {
                module.tick();
            }
            backends.pump(&session, &stream, &input, &application, &display);
        });
        let pending = broker_request(
            &stream,
            "amaterasu.control-transport/1",
            json!({"action":"status"}),
            "control-authenticated-check",
        );
        let mut status = None;
        for _ in 0..200 {
            selected_control.as_mut().unwrap().tick();
            if let Some(value) = take(&stream, pending) {
                status = Some(value);
                break;
            }
        }
        let status = status.expect("control status");
        assert!(
            status["streams"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["authenticated"] == true
                    && item["received_messages"].as_u64().unwrap_or(0) >= 1),
            "{status}"
        );
    }
    match scenario {
        "retirement_unconfirmed" => {
            assert!(cancelled);
            assert_eq!(
                router.launches.selected_preflight_state_for_test(),
                Some((true, true))
            );
            assert_eq!(
                router.launches.selected_preflight_cleanup_for_test(),
                Some("preflight_retirement_unconfirmed")
            );
            assert!(
                !backends
                    .calls
                    .iter()
                    .any(|(service, payload)| service == "stream"
                        && payload["action"] == "release_history_activity")
            );
            let coordinator = runtime.client("amaterasu.launch");
            let snapshot = broker_request(
                &coordinator,
                "amaterasu.state/1",
                json!({"action":"snapshot","cursor":null,"limit":128}),
                "unconfirmed-journal",
            );
            memory.tick();
            let snapshot = take(&coordinator, snapshot).expect("unconfirmed journal");
            assert!(
                snapshot["values"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|entry| entry["phase"] == "reservation_pending"),
                "{snapshot}"
            );
        }
        "retirement_restart" => {
            assert!(cancelled);
            assert_eq!(
                router.launches.selected_preflight_state_for_test(),
                Some((false, true))
            );
            assert_eq!(router.launches.selected_preflight_cleanup_for_test(), None);
            assert!(
                backends
                    .calls
                    .iter()
                    .any(|(service, payload)| service == "stream"
                        && payload["action"] == "release_history_activity")
            );
            let coordinator = runtime.client("amaterasu.launch");
            let snapshot = broker_request(
                &coordinator,
                "amaterasu.state/1",
                json!({"action":"snapshot","cursor":null,"limit":128}),
                "recovered-journal",
            );
            let mut journal = None;
            for _ in 0..200 {
                memory.tick();
                if let Some(value) = take(&coordinator, snapshot) {
                    journal = Some(value);
                    break;
                }
            }
            let journal = journal.expect("journal snapshot");
            let entry = journal["values"]
                .as_object()
                .unwrap()
                .values()
                .find(|value| value["phase"] == "retired")
                .expect("retirement tombstone");
            let retirement = &entry["retirement"];
            let late = broker_request(
                &coordinator,
                "example.library.provider/1",
                json!({"action":"attest_limiter_target_v1","request":{
                "version":1,"stage":"reserve","operation_id":retirement["operation_id"],
                "application_id":retirement["application_id"],"provider_incarnation":retirement["provider_incarnation"],
                "ticket":null,"policy_identity":retirement["policy_identity"]}}),
                "recovered-late-reserve",
            );
            let late = await_provider(&coordinator, late, &mut generated);
            assert_ne!(late["ok"], true, "{late}");
        }
        "success"
        | "limiter_success"
        | "limiter_stale_recheck"
        | "resume_changed"
        | "resume_limiter"
        | "resume_stale"
        | "resume_foreign"
        | "resume_failed"
        | "resume_unpaired"
        | "retirement_restart_running" => {
            assert_eq!(replies.len(), 1, "hardware requests: {:?}", backends.calls);
            assert_eq!(
                replies[0].1.status,
                200,
                "{}; hardware requests: {:?}",
                String::from_utf8_lossy(&replies[0].1.body),
                backends.calls
            );
            assert!(prepared, "{:?}", backends.calls);
            if !real_session {
                assert_eq!(backends.session_state, "active");
            }
            if scenario == "retirement_restart_running" {
                drop(launch);
                launch = LoadedModule::start(
                    &artifact(&artifacts, "amaterasu_launch"),
                    &runtime.endpoint("amaterasu.launch"),
                );
                let epoch = broker_request(
                    &nvhttp,
                    "amaterasu.launch/1",
                    json!({"action":"operation_epoch"}),
                    "restart-running-epoch",
                );
                let epoch = await_launch(
                    &nvhttp,
                    epoch,
                    &mut launch,
                    &mut generated,
                    &mut provider,
                    &mut memory,
                );
                assert_eq!(epoch["ok"], true, "{epoch}");
                let coordinator = runtime.client("amaterasu.launch");
                let snapshot = broker_request(
                    &coordinator,
                    "amaterasu.state/1",
                    json!({"action":"snapshot","cursor":null,"limit":128}),
                    "running-recovered-journal",
                );
                memory.tick();
                let snapshot = take(&coordinator, snapshot).expect("running journal snapshot");
                let entry = snapshot["values"]
                    .as_object()
                    .unwrap()
                    .values()
                    .find(|entry| entry["phase"] == "retired")
                    .expect("running retirement tombstone");
                let retired = &entry["retirement"];
                let lookup = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"lookup","operation_id":retired["operation_id"],"application_id":retired["application_id"],"provider_incarnation":retired["provider_incarnation"]}),
                    "running-recovered-lookup",
                );
                let lookup = await_provider(&coordinator, lookup, &mut generated);
                assert_eq!(lookup["state"], "stopped", "{lookup}");
                assert_eq!(
                    lookup["retirement_proof"]["kind"], "fenced_stopped",
                    "{lookup}"
                );
            }
            if scenario.starts_with("resume_") {
                router.reply_delivered(1);
                let (old_ticket, session_target) = router
                    .launches
                    .selected_session_for_test()
                    .expect("retained lifecycle target");
                let session_target = json!(session_target);
                let prepare = lifecycle_request(
                    &nvhttp,
                    &mut lifecycle,
                    json!({"operation":"prepare_stream","operation_id":"fixture-paused-stream","target":session_target,"requested_video":{"width":1280,"height":720,"refresh_millihz":60000,"hdr":false},"consumer_requirements":["capture"]}),
                    "fixture-stream-prepare",
                );
                assert_eq!(prepare["status"], "pending", "{prepare}");
                let stream_target = prepare["target"].clone();
                let paused = lifecycle_request(
                    &nvhttp,
                    &mut lifecycle,
                    json!({"operation":"pause_stream","operation_id":"fixture-pause","target":stream_target}),
                    "fixture-stream-pause",
                );
                assert_eq!(paused["fenced"], true, "{paused}");
                let capture = runtime.client("capture");
                let retired = lifecycle_request(
                    &capture,
                    &mut lifecycle,
                    json!({"operation":"retire_stream","operation_id":"fixture-retire","target":stream_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}),
                    "fixture-stream-retire",
                );
                assert!(
                    retired["status"] == "pending" || retired["reason"] == "stream_not_fenced",
                    "{retired}"
                );
                let mut settled = None;
                for attempt in 0..100 {
                    let result = lifecycle_request(
                        &nvhttp,
                        &mut lifecycle,
                        json!({"operation":"lookup","operation_id":"fixture-pause"}),
                        &format!("fixture-pause-lookup-{attempt}"),
                    );
                    if result["status"] != "pending" {
                        settled = Some(result);
                        break;
                    }
                }
                let settled = settled.expect("pause settles");
                assert_eq!(settled["status"], "succeeded", "{settled}");
                let snapshot = lifecycle_request(
                    &nvhttp,
                    &mut lifecycle,
                    json!({"operation":"snapshot","target":session_target}),
                    "fixture-paused-snapshot",
                );
                assert!(
                    snapshot["sessions"].as_array().unwrap().iter().any(
                        |entry| entry["session_id"] == session_target["session_id"]
                            && entry["generation"] == session_target["generation"]
                            && entry["phase"] == "paused"
                    ),
                    "{snapshot}"
                );
                let close = broker_request(
                    &nvhttp,
                    "amaterasu.rtsp/1",
                    json!({"action":"close","ticket":old_ticket}),
                    "fixture-close",
                );
                let mut closed = None;
                for _ in 0..1000 {
                    rtsp.tick();
                    backends.pump(&session, &stream, &input, &application, &display);
                    if let Some(result) = take(&nvhttp, close) {
                        closed = Some(result);
                        break;
                    }
                }
                let closed = closed.expect("real RTSP close");
                assert_eq!(closed["state"], "closed", "{closed}");
                let mut target_value = session_target.clone();
                if scenario == "resume_stale" {
                    target_value["generation"] =
                        json!(session_target["generation"].as_u64().unwrap() + 1);
                }
                if scenario == "resume_foreign" {
                    let foreign = runtime.client("foreign");
                    let foreign_prepare = lifecycle_request(
                        &foreign,
                        &mut lifecycle,
                        json!({"operation":"prepare","operation_id":"foreign-session"}),
                        "foreign-prepare",
                    );
                    assert_eq!(foreign_prepare["status"], "pending", "{foreign_prepare}");
                    let mut ready = None;
                    for attempt in 0..100 {
                        let result = lifecycle_request(
                            &foreign,
                            &mut lifecycle,
                            json!({"operation":"lookup","operation_id":"foreign-session"}),
                            &format!("foreign-lookup-{attempt}"),
                        );
                        if result["status"] != "pending" {
                            ready = Some(result);
                            break;
                        }
                    }
                    let ready = ready.expect("foreign session settles");
                    assert_eq!(ready["status"], "succeeded", "{ready}");
                    target_value = ready["target"].clone();
                }
                let target = serde_json::from_value(target_value).unwrap();
                assert_eq!(
                    router.launches.stage_paused_for_resume_test(target),
                    Some(old_ticket)
                );
                if scenario == "resume_failed" {
                    backends.fail_next_prepare = true;
                }
                if scenario == "resume_unpaired" {
                    let unpair = broker_request(
                        &nvhttp,
                        "amaterasu.pairing/1",
                        json!({"action":"unpair_presented_client","certificate_pem":CLIENT_CERT}),
                        "fixture-unpair",
                    );
                    let mut result = None;
                    for _ in 0..500 {
                        tick_authority_modules(
                            &mut pairing,
                            &mut memory,
                            &mut selected_host_configuration,
                        );
                        if let Some(value) = take(&nvhttp, unpair) {
                            result = Some(value);
                            break;
                        }
                    }
                    assert_eq!(
                        result.as_ref().map(|value| &value["unpaired"]),
                        Some(&json!(1)),
                        "{result:?}"
                    );
                }
                let resume_peer = RequestContext {
                    secure: true,
                    remote: "127.0.0.1".parse().unwrap(),
                    peer_certificate: Some(cert.clone()),
                    peer_trusted: true,
                    list_allowed: true,
                    launch_allowed: true,
                };
                let resume = "/resume?appid=1&mode=1280x720x60&rikey=10112233445566778899aabbccddeeff&rikeyid=18";
                assert!(
                    router
                        .dispatch(2, request(resume), resume_peer, &nvhttp)
                        .is_none()
                );
                let mut resumed = vec![];
                for _ in 0..5000 {
                    tick_authority_modules(
                        &mut pairing,
                        &mut memory,
                        &mut selected_host_configuration,
                    );
                    launch.tick();
                    lifecycle.tick();
                    if let Some(module) = &mut library {
                        module.tick();
                    }
                    if let Some(module) = &mut policy {
                        module.tick();
                    }
                    if let Some(module) = &mut generated {
                        module.tick();
                    }
                    if let Some(module) = &mut provider {
                        module.tick();
                    }
                    rtsp.tick();
                    backends.pump(&session, &stream, &input, &application, &display);
                    resumed
                        .extend(router.poll_with_trust(&nvhttp, |presented, _| presented == cert));
                    if !resumed.is_empty() {
                        break;
                    }
                }
                assert_eq!(resumed.len(), 1, "hardware requests: {:?}", backends.calls);
                if matches!(scenario, "resume_changed" | "resume_limiter") {
                    assert_eq!(
                        resumed[0].1.status,
                        200,
                        "{}; hardware requests: {:?}",
                        String::from_utf8_lossy(&resumed[0].1.body),
                        backends.calls
                    );
                    assert_eq!(backends.session_state, "active");
                    assert_eq!(
                        backends
                            .calls
                            .iter()
                            .filter(|(service, payload)| service == "stream"
                                && payload["action"] == "prepare")
                            .count(),
                        2
                    );
                } else {
                    assert_ne!(
                        resumed[0].1.status,
                        200,
                        "{}",
                        String::from_utf8_lossy(&resumed[0].1.body)
                    );
                    let prepares = backends
                        .calls
                        .iter()
                        .filter(|(service, payload)| {
                            service == "stream" && payload["action"] == "prepare"
                        })
                        .count();
                    assert_eq!(
                        prepares,
                        if scenario == "resume_failed" { 2 } else { 1 },
                        "{:?}",
                        backends.calls
                    );
                    if scenario == "resume_failed" {
                        assert!(
                            backends
                                .calls
                                .iter()
                                .filter(|(service, payload)| service == "stream"
                                    && payload["action"] == "stop")
                                .count()
                                >= 2,
                            "{:?}",
                            backends.calls
                        );
                    }
                }
            }
            if matches!(
                scenario,
                "limiter_success" | "limiter_stale_recheck" | "resume_limiter"
            ) {
                let verify = router
                    .launches
                    .selected_limiter_verification_for_test()
                    .expect("real admitted limiter target");
                assert_eq!(
                    verify["frame_limiter_executable_path"],
                    if replacement {
                        "/synthetic/replacement-game"
                    } else {
                        "/synthetic/game"
                    }
                );
                let rtsp_client = runtime.client("amaterasu.rtsp");
                let attacker = runtime.client("attacker");
                for (index, (client, payload, expected)) in [
                    (
                        &rtsp_client,
                        verify.clone(),
                        scenario != "limiter_stale_recheck",
                    ),
                    (
                        &rtsp_client,
                        {
                            let mut altered = verify.clone();
                            altered["frame_limiter_executable_path"] = json!("/synthetic/other");
                            altered
                        },
                        false,
                    ),
                    (
                        &rtsp_client,
                        {
                            let mut altered = verify.clone();
                            altered["policy_identity"]["snapshot_id"] = json!("foreign-snapshot");
                            altered
                        },
                        false,
                    ),
                    (&attacker, verify, false),
                ]
                .into_iter()
                .enumerate()
                {
                    let pending = broker_request(
                        client,
                        "amaterasu.launch/1",
                        payload,
                        &format!("limiter-verify-{index}"),
                    );
                    let mut reply = None;
                    for _ in 0..200 {
                        launch.tick();
                        memory.tick();
                        if let Some(module) = &mut generated {
                            module.tick();
                        }
                        if let Some(module) = &mut provider {
                            module.tick();
                        }
                        if let Some(value) = take(client, pending) {
                            reply = Some(value);
                            break;
                        }
                    }
                    let reply = reply.expect("Launch limiter verification");
                    assert_eq!(reply["verified"] == true, expected, "{reply}");
                    if !expected {
                        assert_eq!(reply["code"], "frame_limiter_snapshot_denied", "{reply}");
                    }
                }
            }
        }
        "cancelled" | "retirement_cancel" | "retirement_uncertain_write" => {
            assert!(cancelled);
            assert!(
                backends
                    .calls
                    .iter()
                    .any(|(service, payload)| service == "stream"
                        && payload["action"] == "release_history_activity"),
                "{:?}",
                backends.calls
            );
            assert!(!prepared);
            if scenario == "retirement_uncertain_write" {
                let coordinator = runtime.client("amaterasu.launch");
                let snapshot = broker_request(
                    &coordinator,
                    "amaterasu.state/1",
                    json!({"action":"snapshot","cursor":null,"limit":128}),
                    "uncertain-final-journal",
                );
                memory.tick();
                let snapshot = take(&coordinator, snapshot).expect("uncertain final journal");
                let entry = snapshot["values"]
                    .as_object()
                    .unwrap()
                    .values()
                    .find(|entry| entry["phase"] == "retired")
                    .expect("uncertain write tombstone");
                let retired = &entry["retirement"];
                let late = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"attest_limiter_target_v1","request":{
                    "version":1,"stage":"reserve","operation_id":retired["operation_id"],"application_id":retired["application_id"],
                    "provider_incarnation":retired["provider_incarnation"],"ticket":null,"policy_identity":retired["policy_identity"]}}),
                    "uncertain-late-reserve",
                );
                let late = await_provider(&coordinator, late, &mut generated);
                assert_ne!(late["ok"], true, "{late}");
            }
            if scenario == "retirement_cancel" {
                assert_eq!(
                    router.launches.selected_preflight_state_for_test(),
                    Some((false, false))
                );
                let epoch_request = broker_request(
                    &nvhttp,
                    "amaterasu.launch/1",
                    json!({"action":"operation_epoch"}),
                    "retire-epoch",
                );
                let epoch = await_launch(
                    &nvhttp,
                    epoch_request,
                    &mut launch,
                    &mut generated,
                    &mut provider,
                    &mut memory,
                );
                let epoch = epoch["operation_epoch"].as_str().unwrap();
                let application = json!({"library":"games","id":"opaque/game"});
                let snapshot = json!({"version":1,"settings":{"frame_limiter_enable":true,"frame_limiter_fps_limit_millihz":120000,"frame_limiter_provider":"auto","frame_limiter_disable_vsync":false,"frame_limiter_auto_virtual_framegen":"enabled","rtss_frame_limit_type":"async","rtss_allow_virtual_display_override":false,"rtss_install_path":"","mangohud_limiter_method":"late","mangohud_preset":"custom","mangohud_always_show_graph":false},"requested_stream_fps_millihz":60000,"client_display_mode_override_millihz":null,"lossless_scaling_limit_millihz":null,"virtual_display_active":null,"physical_framegen_capture":null,"game_frame_generation_mode":null,"rtss_frame_limit_type_explicit_override":null});
                let mut last_policy = None;
                for index in 0..7 {
                    let operation = format!("ale1:{epoch}:retirement-{index}");
                    let pre = broker_request(
                        &nvhttp,
                        "amaterasu.launch/3",
                        json!({"action":"prepare_streaming","operation_id":operation,"application":application,"entry_revision":"entry:opaque","certificate_id":cert_id}),
                        &format!("retire-prepare-{index}"),
                    );
                    let pre = await_launch(
                        &nvhttp,
                        pre,
                        &mut launch,
                        &mut generated,
                        &mut provider,
                        &mut memory,
                    );
                    assert_eq!(pre["ok"], true, "{pre}");
                    last_policy = Some(pre["policy_identity"].clone());
                    let accept = broker_request(
                        &nvhttp,
                        "amaterasu.launch/3",
                        json!({"action":"accept_streaming","operation_id":operation,"application":application,"entry_revision":"entry:opaque","certificate_id":cert_id,"preflight_token":pre["preflight_token"],"policy_identity":pre["policy_identity"],"frame_limiter_settings":snapshot,"client_frame_limiter_overrides":{}}),
                        &format!("retire-accept-{index}"),
                    );
                    // Hold the admission response at the broker while a lifecycle
                    // observer asks whether the now-reserved operation is retired.
                    for _ in 0..100 {
                        launch.tick();
                        memory.tick();
                        if let Some(module) = &mut generated {
                            module.tick();
                        }
                        if let Some(module) = &mut provider {
                            module.tick();
                        }
                    }
                    let observer = runtime.client("amaterasu.session-lifecycle");
                    let verify = json!({"action":"verify_retirement","owner":"amaterasu.nvhttp","launch_id":null,"operation_id":operation});
                    let request = broker_request(
                        &observer,
                        "amaterasu.launch/1",
                        verify.clone(),
                        &format!("verify-reserved-{index}"),
                    );
                    let result = await_launch(
                        &observer,
                        request,
                        &mut launch,
                        &mut generated,
                        &mut provider,
                        &mut memory,
                    );
                    assert_eq!(result["code"], "retirement_unconfirmed", "{result}");
                    let accepted = take(&nvhttp, accept).expect("delayed admission response");
                    assert_eq!(accepted["accepted"], true, "{accepted}");
                    let cancel = json!({"action":"cancel_streaming","operation_id":operation,"certificate_id":cert_id,"preflight_token":pre["preflight_token"],"policy_identity":pre["policy_identity"]});
                    for duplicate in 0..2 {
                        let request = broker_request(
                            &nvhttp,
                            "amaterasu.launch/3",
                            cancel.clone(),
                            &format!("retire-cancel-{index}-{duplicate}"),
                        );
                        let result = await_launch(
                            &nvhttp,
                            request,
                            &mut launch,
                            &mut generated,
                            &mut provider,
                            &mut memory,
                        );
                        assert_eq!(result["cancelled"], true, "{result}");
                    }
                    let request = broker_request(
                        &observer,
                        "amaterasu.launch/1",
                        verify,
                        &format!("verify-retired-{index}"),
                    );
                    let result = await_launch(
                        &observer,
                        request,
                        &mut launch,
                        &mut generated,
                        &mut provider,
                        &mut memory,
                    );
                    assert_eq!(result["retired"], true, "{result}");
                    let replay = broker_request(
                        &nvhttp,
                        "amaterasu.launch/3",
                        json!({"action":"prepare_streaming","operation_id":operation,"application":application,"entry_revision":"entry:opaque","certificate_id":cert_id}),
                        &format!("retire-replay-{index}"),
                    );
                    let rejected = await_launch(
                        &nvhttp,
                        replay,
                        &mut launch,
                        &mut generated,
                        &mut provider,
                        &mut memory,
                    );
                    assert_ne!(rejected["ok"], true, "{rejected}");
                }
                let coordinator = runtime.client("amaterasu.launch");
                let identity = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"identity"}),
                    "late-identity",
                );
                let identity = await_provider(&coordinator, identity, &mut generated);
                let incarnation = identity["provider_incarnation"].as_str().unwrap();
                let policy = last_policy.expect("qualified selected policy");
                // Each failed lookup exercises a terminal State result through
                // the real broker. Exceed its shared 512-request capacity.
                for index in 0..650 {
                    let request = broker_request(
                        &nvhttp,
                        "amaterasu.launch/3",
                        json!({
                            "action":"cancel_streaming", "operation_id":format!("missing-{index}"),
                            "certificate_id":cert_id,"preflight_token":"f".repeat(64),"policy_identity":policy
                        }),
                        &format!("missing-recovery-{index}"),
                    );
                    let result = await_launch(
                        &nvhttp,
                        request,
                        &mut launch,
                        &mut generated,
                        &mut provider,
                        &mut memory,
                    );
                    assert_eq!(
                        result["code"], "preflight_retirement_unconfirmed",
                        "{index}: {result}"
                    );
                }
                let retirement = json!({"version":1,"operation_id":"late-reservation","application_id":"fixture-game","provider_incarnation":incarnation,"original_initiator":"amaterasu.nvhttp","policy_identity":policy});
                for index in 0..2 {
                    let request = broker_request(
                        &coordinator,
                        "example.library.provider/1",
                        json!({"action":"retire_limiter_target_v1","request":retirement}),
                        &format!("late-retire-{index}"),
                    );
                    let proof = await_provider(&coordinator, request, &mut generated);
                    assert_eq!(proof["retirement"]["retired"], true, "{proof}");
                }
                let late = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"attest_limiter_target_v1","request":{"version":1,"stage":"reserve","operation_id":"late-reservation","application_id":"fixture-game","provider_incarnation":incarnation,"ticket":null,"policy_identity":policy}}),
                    "late-reserve",
                );
                let late = await_provider(&coordinator, late, &mut generated);
                assert_ne!(late["ok"], true, "{late}");
                // Deliver the successful Reserve response only after its fence
                // is confirmed, modelling an acknowledgement delayed in transit.
                let held_request = json!({"version":1,"stage":"reserve","operation_id":"held-response","application_id":"fixture-game","provider_incarnation":incarnation,"ticket":null,"policy_identity":policy});
                let held = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"attest_limiter_target_v1","request":held_request}),
                    "held-reserve",
                );
                generated.as_mut().unwrap().tick();
                let held_retirement = json!({"version":1,"operation_id":"held-response","application_id":"fixture-game","provider_incarnation":incarnation,"original_initiator":"amaterasu.launch","policy_identity":policy});
                let fence = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"retire_limiter_target_v1","request":held_retirement}),
                    "held-retire",
                );
                let fence = await_provider(&coordinator, fence, &mut generated);
                assert_eq!(fence["retirement"]["retired"], true, "{fence}");
                let delayed = take(&coordinator, held).expect("delayed reservation response");
                assert_eq!(delayed["ok"], true, "{delayed}");
                let replay = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"attest_limiter_target_v1","request":held_request}),
                    "held-replay",
                );
                let replay = await_provider(&coordinator, replay, &mut generated);
                assert_ne!(replay["ok"], true, "{replay}");
                let attacker = runtime.client("attacker");
                let foreign = broker_request(
                    &attacker,
                    "example.library.provider/1",
                    json!({"action":"retire_limiter_target_v1","request":retirement}),
                    "foreign-retire",
                );
                let foreign = await_provider(&attacker, foreign, &mut generated);
                assert_ne!(foreign["ok"], true, "{foreign}");
                let mut stale = retirement.clone();
                stale["operation_id"] = json!("stale-reservation");
                stale["provider_incarnation"] = json!("stale-incarnation");
                let stale = broker_request(
                    &coordinator,
                    "example.library.provider/1",
                    json!({"action":"retire_limiter_target_v1","request":stale}),
                    "stale-retire",
                );
                let stale = await_provider(&coordinator, stale, &mut generated);
                assert_ne!(stale["ok"], true, "{stale}");
            }
        }
        "stale"
        | "display_rejected"
        | "failed_preparation"
        | "limiter_unsupported"
        | "limiter_no_retirement"
        | "limiter_unconfigured"
        | "limiter_mismatch"
        | "limiter_stale" => {
            assert_eq!(replies.len(), 1, "hardware requests: {:?}", backends.calls);
            assert_ne!(
                replies[0].1.status,
                200,
                "{}",
                String::from_utf8_lossy(&replies[0].1.body)
            );
            if !matches!(
                scenario,
                "failed_preparation" | "limiter_mismatch" | "limiter_stale"
            ) {
                assert!(!prepared, "{:?}", backends.calls);
                assert!(backends.session_state.is_empty());
            }
            if scenario == "display_rejected" {
                assert!(
                    backends
                        .calls
                        .iter()
                        .any(|(service, payload)| service == "display"
                            && payload["action"] == "snapshot"),
                    "{:?}",
                    backends.calls
                );
            }
            if matches!(
                scenario,
                "failed_preparation" | "limiter_mismatch" | "limiter_stale"
            ) {
                let mut expected = vec![
                    ("session", "fail"),
                    ("stream", "stop"),
                    ("stream", "release_history_activity"),
                ];
                if scenario == "failed_preparation" {
                    expected.extend([
                        ("application", "prepare"),
                        ("application", "preparation_stop"),
                    ]);
                }
                for (service, action) in expected {
                    assert!(
                        backends
                            .calls
                            .iter()
                            .any(|(seen_service, payload)| seen_service == service
                                && payload["action"] == action),
                        "missing {service}/{action}: {:?}",
                        backends.calls
                    );
                }
                assert_eq!(backends.session_state, "stopped");
                assert!(backends.lease.is_none());
            }
        }
        _ => unreachable!(),
    }
    None
}
fn pump_input_configuration(configuration: &RuntimeClient) {
    while let Some(request) = configuration.poll_serialized().unwrap() {
        assert_eq!(request.payload["action"], "product_get");
        let payload = if request.caller == "amaterasu.nvhttp" {
            json!({"ok":true,"found":true,"revision":1,"version":5,"values":{"hostname":"Test","port":QUALIFICATION_PORT.load(std::sync::atomic::Ordering::Relaxed),"address_family":null,"bind_address":null,"output_name":null,"stream_audio":false}})
        } else {
            json!({"ok":true,"found":true,"revision":1,"version":14,"values":{"proton_dualsense_compatibility":false}})
        };
        configuration
            .require_success(Command::Complete {
                request: request.id,
                payload,
            })
            .unwrap();
    }
}

fn lifecycle_request(
    client: &RuntimeClient,
    lifecycle: &mut LoadedModule,
    payload: Value,
    operation: &str,
) -> Value {
    let request = broker_request(client, "amaterasu.session-lifecycle/1", payload, operation);
    for _ in 0..200 {
        lifecycle.tick();
        if let Some(result) = take(client, request) {
            return result;
        }
    }
    panic!("lifecycle request stalled: {operation}")
}

fn await_provider(
    client: &RuntimeClient,
    request: u64,
    generated: &mut Option<LoadedModule>,
) -> Value {
    for _ in 0..200 {
        generated.as_mut().expect("replacement provider").tick();
        if let Some(result) = take(client, request) {
            return result;
        }
    }
    panic!("provider request stalled")
}

fn await_launch(
    client: &RuntimeClient,
    request: u64,
    launch: &mut LoadedModule,
    generated: &mut Option<LoadedModule>,
    provider: &mut Option<LoadedModule>,
    memory: &mut LoadedModule,
) -> Value {
    for _ in 0..500 {
        launch.tick();
        memory.tick();
        if let Some(module) = generated {
            module.tick();
        }
        if let Some(module) = provider {
            module.tick();
        }
        if let Some(result) = take(client, request) {
            return result;
        }
    }
    panic!("Launch request stalled")
}
struct TestRoot(PathBuf);
impl Drop for TestRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove test-owned journal directory");
    }
}
fn tempfile_dir() -> TestRoot {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "amaterasu-composition-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    TestRoot(dir)
}

#[derive(Default)]
struct Hardware {
    real_input: bool,
    real_stream: bool,
    real_session: bool,
    session_key: String,
    session_state: String,
    lease: Option<String>,
    calls: Vec<(String, Value)>,
    fail_next_prepare: bool,
}
impl Hardware {
    fn pump(
        &mut self,
        session: &RuntimeClient,
        stream: &RuntimeClient,
        input: &RuntimeClient,
        application: &RuntimeClient,
        display: &RuntimeClient,
    ) {
        for (name, runtime) in [
            ("session", session),
            ("stream", stream),
            ("input", input),
            ("application", application),
            ("display", display),
        ] {
            if (name == "input" && self.real_input)
                || (name == "stream" && self.real_stream)
                || (name == "session" && self.real_session)
            {
                continue;
            }
            while let Some(request) = runtime.poll_serialized().unwrap() {
                let p = &request.payload;
                self.calls.push((name.into(), p.clone()));
                let result = match (name, p["action"].as_str().unwrap_or("")) {
                    ("stream", "status") => {
                        json!({"ok":true,"sessions":[],"total":0,"truncated":false})
                    }
                    ("stream", "reserve_history_activity") => {
                        json!({"ok":true,"reserved":true,"activity_id":p["activity_id"],"rtsp_operation_id":p["rtsp_operation_id"]})
                    }
                    ("stream", "release_history_activity") => {
                        json!({"ok":true,"released":true,"activity_id":p["activity_id"],"rtsp_operation_id":p["rtsp_operation_id"]})
                    }
                    ("stream", "prepare") if self.fail_next_prepare => {
                        self.fail_next_prepare = false;
                        json!({"ok":false,"session":24,"state":"failed","code":"synthetic_prepare_failure"})
                    }
                    ("stream", "prepare") => {
                        json!({"ok":true,"session":23,"state":"prepared","audio_local":"127.0.0.1:51000","video_local":"127.0.0.1:51002","control_local":"127.0.0.1:51004","history_activity_id":p["history_activity_id"],"rtsp_operation_id":p["rtsp_operation_id"],"lifecycle_session_target":p["lifecycle_session_target"],"lifecycle_input_required":false,"display_prepare_timeout_ms":p["display_prepare_timeout_ms"],"frame_limiter_settings":p["frame_limiter_settings"],"frame_limiter_executable_path":p["frame_limiter_executable_path"]})
                    }
                    ("display", _) => json!({"ok":false,"code":"synthetic_display_rejection"}),
                    ("application", "prepare" | "preparation_status" | "preparation_stop") => {
                        json!({"ok":true,"preparation":{"operation_id":p["operation_id"],"state":"preparation_failed","preparation":{"completed":0},"cleanup":{"state":"complete"}}})
                    }
                    ("stream", "stop") => {
                        json!({"ok":true,"session":p["session"],"state":"stopped","cleanup_error":null})
                    }
                    ("input", "settings_get") => {
                        json!({"ok":true,"persisted":true,"revision":1,"values":{"proton_dualsense_compatibility":false}})
                    }
                    ("session", action) => {
                        match action {
                            "create" => {
                                self.session_key = p["key"].as_str().unwrap().into();
                                self.session_state = "preparing".into();
                            }
                            "acquire" => self.lease = Some(p["resource"].as_str().unwrap().into()),
                            "activate" => self.session_state = "active".into(),
                            "stop" => self.session_state = "stopping".into(),
                            "finish_stop" | "cancel" | "fail" => {
                                self.session_state = "stopped".into();
                                self.lease = None;
                            }
                            "lookup" => {}
                            other => panic!("unexpected session action: {other}: {p}"),
                        }
                        let leases: BTreeMap<_, _> =
                            self.lease.iter().map(|key| (key, "exclusive")).collect();
                        json!({"ok":true,"revision":1,"sessions":[{"id":1,"key":self.session_key,"state":self.session_state,"leases":leases}]})
                    }
                    _ => panic!("unexpected hardware request {name}: {p}"),
                };
                runtime
                    .require_success(Command::Complete {
                        request: request.id,
                        payload: result,
                    })
                    .unwrap();
            }
        }
    }
}

fn configure_full(config: &mut Value, streaming: &crate::launch::StreamingConfig) {
    if std::env::var_os("AMATERASU_COMPOSITION_NORMAL_ABI").is_some() {
        config["live_feedback"] = json!({"protocol_owner":"amaterasu.nvhttp","pairing_owner":"amaterasu.pairing","stream_owner":"stream","input_owner":"input","lifecycle_owner":"amaterasu.session-lifecycle","control_owner":"control","host_configuration_owner":"amaterasu.host-configuration"});
        for (service, owner) in [
            ("amaterasu.nvhttp/1", "amaterasu.nvhttp"),
            ("amaterasu.pairing-effects/1", "amaterasu.pairing"),
            ("amaterasu.pairing/1", "amaterasu.pairing"),
            ("amaterasu.state/1", "amaterasu.state"),
            (
                "amaterasu.host-configuration/1",
                "amaterasu.host-configuration",
            ),
            ("amaterasu.stream-session/1", "stream"),
            ("amaterasu.input/1", "input"),
            (
                "amaterasu.session-lifecycle/1",
                "amaterasu.session-lifecycle",
            ),
            ("amaterasu.control-transport/1", "control"),
        ] {
            config["service_selections"]["broker"][service] =
                json!({"provider":owner,"contract":service});
        }
        config["service_selections"]["native"]["amaterasu.identity/1"] =
            json!({"provider":"amaterasu.pairing","contract":"amaterasu.identity.native/1"});
        config["service_selections"]["native"]["amaterasu.control-transport/1"] =
            json!({"provider":"control","contract":"amaterasu.control-transport.native/1"});
    }
    config["principals"]["attacker"]["call"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.nvhttp/1"));
    let nvhttp = &mut config["principals"]["amaterasu.nvhttp"];
    nvhttp["provide"] = json!(["amaterasu.nvhttp/1"]);
    nvhttp["call"].as_array_mut().unwrap().extend([
        json!("amaterasu.pairing-effects/1"),
        json!("amaterasu.configuration/1"),
    ]);
    nvhttp["native_use"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.network/1"));
    config["principals"]["amaterasu.pairing"]["provide"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.pairing-effects/1"));
    if std::env::var_os("AMATERASU_COMPOSITION_NORMAL_ABI").is_some() {
        config["principals"]["amaterasu.host-configuration"]["call"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!("amaterasu.pairing-effects/1"),
                json!("amaterasu.state/1"),
            ]);
        config["principals"]["amaterasu.pairing"]["call"]
            .as_array_mut()
            .unwrap()
            .push(json!("amaterasu.host-configuration/1"));
        config["settings"]["amaterasu.host-configuration"] =
            json!({"pairing_effect_authority":{"principal":"amaterasu.pairing"}});
        config["settings"]["amaterasu.pairing"]["host_configuration"] = json!(true);
        config["settings"]["amaterasu.pairing"]["effect_authority"]["configuration_owner"] =
            json!("amaterasu.host-configuration");
    }
    config["principals"]["stream"]["call"]
        .as_array_mut()
        .unwrap()
        .extend([
            json!("amaterasu.pairing-effects/1"),
            json!("amaterasu.nvhttp/1"),
        ]);
    config["principals"]["input"]["call"]
        .as_array_mut()
        .unwrap()
        .push(json!("amaterasu.stream-session/1"));
    let port = loop {
        let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = first.local_addr().unwrap().port();
        if !(1029..=65514).contains(&port) {
            continue;
        }
        if let Ok(second) = std::net::TcpListener::bind(("127.0.0.1", port + 5)) {
            drop(second);
            drop(first);
            break port;
        }
    };
    QUALIFICATION_PORT.store(port, std::sync::atomic::Ordering::Relaxed);
    config["settings"]["amaterasu.nvhttp"] = json!({"enabled":true,"http_bind":format!("127.0.0.1:{port}"),"https_bind":format!("127.0.0.1:{}",port+5),"session_lifecycle_required":true,"effect_source_join":{"effect_owner":"stream","input_consumer":"input"},"streaming":streaming});
    config["settings"]["amaterasu.pairing"]["effect_authority"] = json!({"protocol_owner":"amaterasu.nvhttp","effect_owner":"stream","configuration_owner":if std::env::var_os("AMATERASU_COMPOSITION_NORMAL_ABI").is_some() { json!("amaterasu.host-configuration") } else { Value::Null },"feedback_policies":[{"capability":{"schema":"org.amaterasu.feedback.rgb_led","version":1,"direction":"to_client"},"required_permission":"view"}]});
    config["settings"]["stream"]["feedback_retirement_input_principal"] = json!("input");
    config["settings"]["stream"]["feedback_source_protocol_service"] = json!("amaterasu.nvhttp/1");
    config["settings"]["stream"]["feedback_owner_restart_recovery"] = json!(true);
    config["settings"]["input"]["feedback_retirement_service"] =
        json!("amaterasu.stream-session/1");
}
