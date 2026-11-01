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

const CLIENT_CERT: &str = include_str!("../../../../pairing/tests/fixtures/client-cert.pem");
const HOST_CERT: &str = include_str!("../../../../pairing/tests/fixtures/host-cert.pem");
const HOST_KEY: &str = include_str!("../../../../pairing/tests/fixtures/host-key.pem");

struct LoadedModule {
    api: ModuleApi,
    instance: *mut c_void,
    _library: libloading::Library,
}
impl LoadedModule {
    fn start(path: &Path, host: &HostApi) -> Self {
        // SAFETY: This test loads locally built trusted fixtures and retains each
        // shared object until its instance has been destroyed.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        let entry = unsafe { library.get::<abi::ModuleEntry>(b"amaterasu_module_v1") }.unwrap();
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
            api,
            instance,
            _library: library,
        }
    }
    fn tick(&mut self) {
        // SAFETY: instance is live, belongs to this table, and calls are serialized.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, STATUS_OK);
    }
}
impl Drop for LoadedModule {
    fn drop(&mut self) {
        // SAFETY: destroy exactly once before releasing the library handle.
        unsafe { (self.api.destroy)(self.instance) }
    }
}
struct Runtime {
    api: RuntimeApi,
    handle: *mut c_void,
    _library: libloading::Library,
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
            api,
            handle,
            _library: library,
        }
    }
    fn endpoint(&self, principal: &str) -> HostApi {
        let mut host = std::mem::MaybeUninit::uninit();
        // SAFETY: runtime and output are live and principal bytes are borrowed synchronously.
        assert_eq!(
            unsafe {
                (self.api.endpoint)(
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
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: all modules and clients have been retired before runtime teardown.
        unsafe { (self.api.destroy)(self.handle) }
    }
}
fn artifact(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("lib{name}.so"))
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
        response => panic!("request rejected: {response:?}"),
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

#[derive(Default)]
struct MemoryState {
    revision: u64,
    document: Option<Value>,
}
impl MemoryState {
    fn pump(&mut self, runtime: &RuntimeClient) {
        while let Some(request) = runtime.poll_serialized().unwrap() {
            let payload = if request.payload["action"] == "get" {
                json!({"ok":true,"revision":self.revision,"found":self.document.is_some(),"value":self.document})
            } else if request.payload["expected_revision"] == self.revision {
                self.revision += 1;
                self.document = Some(request.payload["value"].clone());
                json!({"ok":true,"revision":self.revision})
            } else {
                json!({"ok":false,"error":{"code":"revision_conflict","current_revision":self.revision}})
            };
            runtime
                .require_success(Command::Complete {
                    request: request.id,
                    payload,
                })
                .unwrap();
        }
    }
}

#[test]
#[ignore = "requires independently built protocol and fixture artifacts plus an ephemeral loopback listener"]
fn real_selected_composition() {
    let filter = std::env::var("AMATERASU_COMPOSITION_CASE").ok();
    let mut executed = false;
    for (replacement, scenario) in [
        (false, "success"),
        (true, "success"),
        (true, "stale"),
        (true, "limiter_unsupported"),
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
        {
            executed = true;
            run_composition(replacement, scenario);
        }
    }
    assert!(executed, "unknown AMATERASU_COMPOSITION_CASE");
}

fn run_composition(replacement: bool, scenario: &str) {
    let artifacts = PathBuf::from(
        std::env::var_os("AMATERASU_COMPOSITION_ARTIFACT_DIR").expect("set artifact directory"),
    );
    let executor = PathBuf::from(
        std::env::var_os("AMATERASU_EXECUTOR_ARTIFACT").expect("set executor artifact"),
    );
    let temp = tempfile_dir();
    let (_, cert) = pem_rfc7468::decode_vec(CLIENT_CERT.as_bytes()).unwrap();
    let cert_id = hex(&Sha256::digest(&cert));
    let services = json!([
        "amaterasu.pairing/1",
        "amaterasu.state/1",
        "amaterasu.launch/1",
        "amaterasu.launch/3",
        "amaterasu.rtsp/1",
        "amaterasu.session-lifecycle/1",
        "amaterasu.session/1",
        "amaterasu.stream-session/1",
        "amaterasu.input/1",
        "amaterasu.application/1",
        "amaterasu.display/1",
        "fixture.library/2",
        "fixture.policy/2",
        "example.library.provider/1"
    ]);
    let config = json!({"principals":{
        "console":{"call": ["amaterasu.pairing/1","amaterasu.state/1"]},
        "attacker":{"call":["amaterasu.launch/1","amaterasu.launch/3"]},
        "amaterasu.nvhttp":{"call":services},
        "amaterasu.pairing":{"provide":["amaterasu.pairing/1"],"call":["amaterasu.state/1"],"native_provide":["amaterasu.identity/1"]},
        "amaterasu.launch":{"provide":["amaterasu.launch/1","amaterasu.launch/2","amaterasu.launch/3"],"call":["fixture.library/2","fixture.policy/2","example.library.provider/1","amaterasu.session/1","amaterasu.application/1"]},
        "amaterasu.rtsp":{"provide":["amaterasu.rtsp/1"],"call":["amaterasu.launch/1","amaterasu.stream-session/1"],"native_use":["amaterasu.network/1"]},
        "amaterasu.session-lifecycle":{"provide":["amaterasu.session-lifecycle/1"],"call":["amaterasu.launch/1"]},
        "network":{"native_provide":["amaterasu.network/1"]},
        "capture":{"call":["amaterasu.session-lifecycle/1"]},
        "foreign":{"call":["amaterasu.session-lifecycle/1"]},
        "state":{"provide":["amaterasu.state/1"]},
        "session":{"provide":["amaterasu.session/1"]},
        "stream":{"provide":["amaterasu.stream-session/1"]},
        "input":{"provide":["amaterasu.input/1"]},
        "display":{"provide":["amaterasu.display/1"]},
        "application":{"provide":["amaterasu.application/1"]},
        "library":{"provide":["fixture.library/2"]},
        "policy":{"provide":["fixture.policy/2"]},
        "executor":{"provide":["example.library.provider/1"]},
        "replacement":{"provide":["fixture.library/2","fixture.policy/2","example.library.provider/1"]}
    },"settings":{
        "amaterasu.pairing":{"administrator_principals":["console"],"transport_principals":["amaterasu.nvhttp"]},
        "amaterasu.launch":{"enable_library_preflight":true,"retirement_observers":["amaterasu.session-lifecycle"],"library_routes":{"games":{
            "service":"fixture.library/2","callers":["amaterasu.nvhttp"],"executors":{"runner":"example.library.provider/1"},
            "streaming_policy_service":"fixture.policy/2","streaming_policy_source":"test-policy"}}},
        "amaterasu.rtsp":{"enabled":true,"bind":"127.0.0.1:0","allowed_principals":["amaterasu.nvhttp"]},
        "amaterasu.session-lifecycle":{"journal_path":temp.0.join("journal.json"),"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},"trusted_callers":["amaterasu.nvhttp","capture","foreign"],"session_delegates":["capture"],"consumers":{"capture":["capture"],"input":[]},"operation_grants":{"amaterasu.nvhttp":["prepare","prepare_stream","pause_stream","lookup","snapshot","bind_application","end_session","application_retired"],"capture":["retire_stream"],"foreign":["prepare","lookup","snapshot"]}},
        "library":{"service":"fixture.library/2","callers":{"amaterasu.launch":{"initiators":["amaterasu.nvhttp"],"resolve_launch":true}},"revision":"snapshot:one",
            "entries":[{"id":"opaque/game","revision":"entry:opaque","name":"Game","description":null,"tags":[],"source":null,"portable_ids":[],"installed":"unknown","readiness":"ready","unavailable_reason":null}],
            "targets":[{"id":"opaque/game","revision":"entry:opaque","executor":"runner","application_id":"fixture-game"}]},
        "policy":{"service":"fixture.policy/2","callers":{"amaterasu.launch":["amaterasu.nvhttp"]},"rules":[{"certificates":[cert_id],"policy":{"application":{"library":"games","id":"opaque/game"},"entry_revision":"entry:opaque","policy_revision":"policy:one","preparation":[]},"config_overrides":{},"application_display_settings":null}]},
        "executor":{"callers":{"amaterasu.launch":{"initiators":["amaterasu.nvhttp","amaterasu.launch"],"lifecycle":true}}, "limiter_mode":match scenario { "limiter_success" => "supported", "limiter_mismatch" => "mismatch", "limiter_stale" => "stale", _ => "" }},
        "replacement":{"certificate_id":cert_id,"mode":scenario}
    }});
    let runtime = Runtime::new(&artifact(&artifacts, "amaterasu_runtime"), config);
    let state = runtime.client("state");
    register(&state, "amaterasu.state/1");
    let network = runtime.client("network");
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
    let stream = runtime.client("stream");
    register(&stream, "amaterasu.stream-session/1");
    let session = runtime.client("session");
    register(&session, "amaterasu.session/1");
    let input = runtime.client("input");
    register(&input, "amaterasu.input/1");
    let display = runtime.client("display");
    register(&display, "amaterasu.display/1");
    let application = runtime.client("application");
    register(&application, "amaterasu.application/1");
    let mut pairing = LoadedModule::start(
        &artifact(&artifacts, "amaterasu_pairing"),
        &runtime.endpoint("amaterasu.pairing"),
    );
    let mut launch = LoadedModule::start(
        &artifact(&artifacts, "amaterasu_launch"),
        &runtime.endpoint("amaterasu.launch"),
    );
    let mut rtsp = LoadedModule::start(
        &artifact(&artifacts, "amaterasu_rtsp"),
        &runtime.endpoint("amaterasu.rtsp"),
    );
    let mut lifecycle = LoadedModule::start(
        &artifact(&artifacts, "amaterasu_session_lifecycle"),
        &runtime.endpoint("amaterasu.session-lifecycle"),
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
    let mut library = (!replacement)
        .then(|| LoadedModule::start(&library_artifact, &runtime.endpoint("library")));
    let mut policy =
        (!replacement).then(|| LoadedModule::start(&policy_artifact, &runtime.endpoint("policy")));
    let mut generated = replacement
        .then(|| LoadedModule::start(&replacement_artifact, &runtime.endpoint("replacement")));
    let mut provider =
        (!replacement).then(|| LoadedModule::start(&executor, &runtime.endpoint("executor")));
    let console = runtime.client("console");
    let mut memory = MemoryState::default();
    for (sequence, payload) in [json!({"action":"import_identity","certificate_pem":HOST_CERT,"private_key_pem":HOST_KEY}), json!({"action":"import_client","client_id":"test-client","name":"Client","certificate_pem":CLIENT_CERT})].into_iter().enumerate() {
        let pending = broker_request(&console, "amaterasu.pairing/1", payload, &format!("pairing-{sequence}"));
        let mut result = None;
        for _ in 0..500 {
            pairing.tick(); memory.pump(&state);
            if let Some(value) = take(&console, pending) { result = Some(value); break; }
        }
        assert_eq!(result.as_ref().map(|value| &value["ok"]), Some(&json!(true)), "{result:?}");
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
            if let Some(value) = take(&attacker, forged) {
                rejected = Some(value);
                break;
            }
        }
        let rejected = rejected.expect("cross-owner result");
        assert_eq!(rejected["code"], "permission_denied", "{rejected}");
    }
    let streaming = crate::launch::StreamingConfig {
        capture: CaptureConfig {
            capture_provider: CaptureProvider::Dxgi,
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
            certificates: [cert_id.clone()].into(),
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
    let display_policy = if scenario == "display_rejected" {
        json!({"dd_configuration_option":"ensure_active"})
    } else {
        json!({})
    };
    router.set_global_display_policy(1, display_policy);
    if scenario.starts_with("limiter_") || scenario == "resume_limiter" {
        router.set_global_frame_limiter_settings(&crate::frame_limiter::FrameLimiterSettings {
            frame_limiter_enable: Some(true),
            ..Default::default()
        });
    }
    let nvhttp = runtime.client("amaterasu.nvhttp");
    let target =
        "/launch?appid=1&mode=1280x720x60&rikey=00112233445566778899aabbccddeeff&rikeyid=17";
    // The transport is synthetic, but the router still performs its production
    // broker-backed pairing check using the actual imported certificate.
    let peer = RequestContext {
        secure: true,
        remote: "127.0.0.1".parse().unwrap(),
        peer_certificate: Some(cert.clone()),
        peer_trusted: true,
        list_allowed: true,
        launch_allowed: true,
    };
    assert!(router.dispatch(1, request(target), peer, &nvhttp).is_none());
    let mut backends = Hardware::default();
    let mut replies = vec![];
    let mut cancelled = false;
    for _ in 0..5000 {
        pairing.tick();
        memory.pump(&state);
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
        replies.extend(router.poll_with_trust(&nvhttp, |presented, _| presented == cert));
        if scenario == "cancelled"
            && !cancelled
            && backends.calls.iter().any(|(service, payload)| {
                service == "stream" && payload["action"] == "reserve_history_activity"
            })
        {
            router.cancel(1, &nvhttp);
            cancelled = true;
        }
        if scenario == "cancelled" {
            if backends.calls.iter().any(|(service, payload)| {
                service == "stream" && payload["action"] == "release_history_activity"
            }) {
                break;
            }
        } else if !replies.is_empty() {
            break;
        }
    }
    let prepared = backends
        .calls
        .iter()
        .any(|(service, payload)| service == "stream" && payload["action"] == "prepare");
    match scenario {
        "success"
        | "limiter_success"
        | "limiter_stale_recheck"
        | "resume_changed"
        | "resume_limiter"
        | "resume_stale"
        | "resume_foreign"
        | "resume_failed"
        | "resume_unpaired" => {
            assert_eq!(replies.len(), 1, "hardware requests: {:?}", backends.calls);
            assert_eq!(
                replies[0].1.status,
                200,
                "{}; hardware requests: {:?}",
                String::from_utf8_lossy(&replies[0].1.body),
                backends.calls
            );
            assert!(prepared, "{:?}", backends.calls);
            assert_eq!(backends.session_state, "active");
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
                        pairing.tick();
                        memory.pump(&state);
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
                    pairing.tick();
                    memory.pump(&state);
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
        "cancelled" => {
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
        }
        "stale"
        | "display_rejected"
        | "failed_preparation"
        | "limiter_unsupported"
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
