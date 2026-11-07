//! Opt-in native-module qualification against the real broker and coordinator.
//! Build the frame-limiter cdylib, then supply AMATERASU_FRAME_LIMITER_LIBRARY.
#![cfg(target_os = "linux")]

use amaterasu_module_sdk::{ApiMethod, Command, Response, abi};
use amaterasu_runtime::Broker;
use amaterasu_session_lifecycle::{Coordinator, LifecycleSettings};
use serde_json::{Value, json};
use std::os::unix::fs::DirBuilderExt;
use std::{
    cell::RefCell,
    ffi::c_void,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

const COORDINATOR: &str = "amaterasu.session-lifecycle";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const PROVIDER: &str = "amaterasu.frame-limiter";
const LIMITER: &str = "nonary.frame-limiter/1";

struct Endpoint {
    settings: Value,
    methods: Vec<ApiMethod>,
    broker: Option<Rc<RefCell<Broker>>>,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    bytes: abi::BorrowedBytes,
    sink: abi::ReplySink,
) -> abi::Status {
    // SAFETY: NativeModule owns the boxed endpoint throughout all serialized calls.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    // SAFETY: the module lends these bytes until this synchronous callback returns.
    let Ok(bytes) = (unsafe { bytes.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let response = if let Some(broker) = &endpoint.broker {
        broker.borrow_mut().dispatch(PROVIDER, command)
    } else {
        match command {
            Command::GetSettings {} => Response::Result {
                payload: endpoint.settings.clone(),
            },
            Command::Register { .. } => Response::Ok,
            Command::RegisterApi { method } => {
                endpoint.methods.push(method);
                Response::Ok
            }
            _ => return abi::STATUS_INVALID_ARGUMENT,
        }
    };
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: ReplySink copies the serialized response synchronously.
    unsafe { sink.send(&bytes) };
    abi::STATUS_OK
}

struct NativeModule {
    _library: libloading::Library,
    endpoint: Box<Endpoint>,
    api: abi::ModuleApi,
    instance: *mut c_void,
}

impl NativeModule {
    fn load(path: &std::path::Path, settings: Value, broker: Option<Rc<RefCell<Broker>>>) -> Self {
        // SAFETY: this opt-in test loads the explicitly supplied, locally built module.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        // SAFETY: the selected Amaterasu module exports this exact ABI entrypoint.
        let entry = unsafe {
            library.get::<unsafe extern "C" fn() -> *const abi::ModuleApi>(b"amaterasu_module_v1\0")
        }
        .unwrap();
        // SAFETY: the library stays loaded until after the instance is destroyed.
        let api = unsafe { *entry() };
        assert!(api.header.supports::<abi::ModuleApi>());
        let mut module = Self {
            _library: library,
            endpoint: Box::new(Endpoint {
                settings,
                methods: vec![],
                broker,
            }),
            api,
            instance: std::ptr::null_mut(),
        };
        let host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *module.endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: the boxed endpoint and loaded table outlive this module instance.
        assert_eq!(
            unsafe { (module.api.create)(&host, &mut module.instance) },
            abi::STATUS_OK
        );
        module
    }

    fn tick(&mut self) {
        // SAFETY: this is the live instance created by the retained module table.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
    }

    fn restart(&mut self) {
        // SAFETY: retire this instance before creating its replacement. The
        // endpoint and library remain live for both serialized lifetimes.
        unsafe { (self.api.destroy)(self.instance) };
        self.instance = std::ptr::null_mut();
        let host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *self.endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: the retained endpoint and module table outlive the new instance.
        assert_eq!(
            unsafe { (self.api.create)(&host, &mut self.instance) },
            abi::STATUS_OK
        );
    }
}
impl Drop for NativeModule {
    fn drop(&mut self) {
        // SAFETY: destroy exactly once, before dropping the endpoint or loaded library.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

fn fresh_proton_environment(tool: &std::path::Path, runtime_root: &std::path::Path) -> Value {
    let output = std::process::Command::new("python3")
        .args([
            "-I",
            "-c",
            r#"
import importlib.util, json, sys, types
spec = importlib.util.spec_from_file_location("limiter_fixture", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
environment = {"XDG_RUNTIME_DIR": sys.argv[2], "SteamAppId": "42"}
applied = module.apply({"g_session": types.SimpleNamespace(env=environment)})
print(json.dumps({"applied": applied, "environment": environment}))
"#,
        ])
        .arg(tool.join("_amaterasu_frame_limiter_v1.py"))
        .arg(runtime_root)
        .env("TMPDIR", runtime_root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "requires the explicitly built frame-limiter cdylib; runs only in disposable directories"]
fn native_limiter_prepares_through_unchanged_coordinator_and_broker() {
    let library = PathBuf::from(
        std::env::var_os("AMATERASU_FRAME_LIMITER_LIBRARY").expect("supply frame-limiter library"),
    );
    assert!(library.is_absolute());
    if std::env::var_os("AMATERASU_LIMITER_TEST_CHILD").is_none() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-limiter-native-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        // Set environment only on the child; parallel tests retain their own homes.
        let mut succeeded = true;
        for (provider, automatic, overlay_available) in [
            ("proton", false, true),
            ("mangohud", false, true),
            ("mangohud-proton", false, true),
            ("proton", true, true),
            ("mangohud", true, true),
            ("mangohud-proton", true, true),
            ("auto", false, false),
            ("mangohud-proton", true, false),
        ] {
            let runtime = root.join(format!("{provider}-{automatic}-{overlay_available}"));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&runtime)
                .unwrap();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "native_limiter_prepares_through_unchanged_coordinator_and_broker",
                    "--nocapture",
                ])
                .env("AMATERASU_LIMITER_TEST_CHILD", &runtime)
                .env("AMATERASU_LIMITER_TEST_PROVIDER", provider)
                .env(
                    "AMATERASU_LIMITER_TEST_AUTOMATIC",
                    if automatic { "1" } else { "0" },
                )
                .env(
                    "AMATERASU_LIMITER_TEST_OVERLAY",
                    if overlay_available { "1" } else { "0" },
                )
                .env("XDG_DATA_HOME", &runtime)
                .env("XDG_STATE_HOME", &runtime)
                .status()
                .unwrap();
            succeeded &= status.success();
        }
        std::fs::remove_dir_all(root).unwrap();
        assert!(succeeded);
        return;
    }
    let root = PathBuf::from(std::env::var_os("AMATERASU_LIMITER_TEST_CHILD").unwrap());
    let provider = std::env::var("AMATERASU_LIMITER_TEST_PROVIDER").unwrap();
    let automatic = std::env::var("AMATERASU_LIMITER_TEST_AUTOMATIC").as_deref() == Ok("1");
    let overlay_available = std::env::var("AMATERASU_LIMITER_TEST_OVERLAY").as_deref() == Ok("1");
    let search = root.join("bin");
    std::fs::create_dir(&search).unwrap();
    if overlay_available {
        use std::os::unix::fs::PermissionsExt;
        let command = search.join("mangohud");
        std::fs::write(&command, b"fixture; discovery must not execute this").unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let effective_provider = if overlay_available {
        provider.as_str()
    } else {
        "proton"
    };
    let steam = root.join("steam");
    let tool = steam.join("steamapps/common/ProtonFixture");
    std::fs::create_dir_all(&tool).unwrap();
    std::fs::write(tool.join("proton"), b"fixture").unwrap();
    std::fs::write(
        tool.join("user_settings.sample.py"),
        b"user_settings = {}\n",
    )
    .unwrap();
    let module_settings = json!({"lifecycle_coordinator":{"principal":COORDINATOR,"service":LIFECYCLE},
        "proton_runtime_directory":root,"proton_state_directory":root,
        "proton_steam_roots":[steam],"mangohud_search_paths":[search]});
    // Discover the actual registered schemas from the module, without effects.
    let probe = NativeModule::load(&library, module_settings.clone(), None);
    let methods = probe.endpoint.methods.clone();
    assert_eq!(methods.len(), 3);
    drop(probe);
    let reference = |method: &str| json!({"service":LIMITER,"method":method,"interface_major":1,"interface_minor":0,"schema_revision":1});
    let settings: LifecycleSettings = serde_json::from_value(json!({
        "journal_path":root.join("lifecycle.json"),
        "capture_binding_mode":"external",
        "trusted_callers":["transport",PROVIDER],
        "session_delegates":["transport"],
        "consumers":{"capture":["transport"]},
        "operation_grants":{"transport":["prepare","prepare_stream","ack_stream","start_stream","pause_stream","retire_stream","lookup","snapshot","end_session","application_retired","read_prepared_plans"],PROVIDER:["read_provider_context"]},
        "consumer_requirements":{"execution_domain":"seat","capture_providers":[],"selector_schemas":[]},
        "participants":[{"provider":PROVIDER,"plan_method":reference("resource_plan"),
            "callback_method":reference("lifecycle_callback"),"status_method":reference("lifecycle_status"),
            "settings_revision":"initial","capabilities":["amaterasu.launch-environment",amaterasu_module_sdk::extension_api::STREAM_JOIN_CAPABILITY],"cleanup_methods":[reference("lifecycle_callback")],
            "settings":{"frame_limiter_enable":!automatic,"frame_limiter_provider":provider,"frame_limiter_fps_limit":0,
                "frame_limiter_disable_vsync":false,"frame_limiter_auto_virtual_framegen":"disabled",
                "mangohud_preset":"custom","mangohud_always_show_graph":false,"mangohud_limiter_method":"late"}}]
    })).unwrap();
    let configuration = serde_json::from_value(json!({
        "principals":{COORDINATOR:{"provide":[LIFECYCLE],"call":[LIMITER,"amaterasu.launch/1"]},
            PROVIDER:{"provide":["amaterasu.frame-limiter/1",LIMITER],"call":[LIFECYCLE]},
            "launch-fixture":{"provide":["amaterasu.launch/1"]},
            "transport":{"call":[LIFECYCLE]}},
        "settings":{PROVIDER:module_settings},
        "api_methods":methods.iter().map(|method|json!({"provider":PROVIDER,"callers":[COORDINATOR],"method":method})).collect::<Vec<_>>()
    })).unwrap();
    let broker = Rc::new(RefCell::new(Broker::new(configuration).unwrap()));
    assert!(matches!(
        broker.borrow_mut().dispatch(
            COORDINATOR,
            Command::Register {
                service: LIFECYCLE.into()
            }
        ),
        Response::Ok
    ));
    let mut coordinator = Coordinator::configured(&settings).unwrap();
    assert!(matches!(
        broker.borrow_mut().dispatch(
            "launch-fixture",
            Command::Register {
                service: "amaterasu.launch/1".into()
            }
        ),
        Response::Ok
    ));
    let mut module = NativeModule::load(&library, Value::Null, Some(broker.clone()));
    let call = |payload: Value,
                id: u64,
                coordinator: &mut Coordinator,
                module: &mut NativeModule| {
        let Response::Accepted { id: request } = broker.borrow_mut().dispatch(
            "transport",
            Command::Request {
                service: LIFECYCLE.into(),
                payload,
                timeout_ms: 5000,
                operation_id: format!("transport-{id}"),
                parent: None,
            },
        ) else {
            panic!("transport request rejected");
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            coordinator
                .advance(&mut |command| Ok(broker.borrow_mut().dispatch(COORDINATOR, command)))
                .unwrap();
            let retirement = broker
                .borrow_mut()
                .dispatch("launch-fixture", Command::Poll {});
            if let Response::Request { id, payload, .. } = retirement {
                assert_eq!(payload["action"], "verify_retirement");
                assert_eq!(payload["operation_id"], "launch-one");
                assert!(matches!(broker.borrow_mut().dispatch("launch-fixture", Command::Complete {
                        request: id, payload: json!({"ok":true,"retired":true,"never_admitted":true,"launch_id":null,
                            "owner":payload["owner"],"operation_id":payload["operation_id"]})
                    }), Response::Ok));
            }
            let incoming = broker.borrow_mut().dispatch(COORDINATOR, Command::Poll {});
            if let Response::Request {
                id,
                caller,
                initiator,
                payload,
                ..
            } = incoming
            {
                let payload = coordinator.handle(&caller, &initiator, payload);
                assert!(matches!(
                    broker.borrow_mut().dispatch(
                        COORDINATOR,
                        Command::Complete {
                            request: id,
                            payload
                        }
                    ),
                    Response::Ok
                ));
            }
            module.tick();
            let response = broker
                .borrow_mut()
                .dispatch("transport", Command::TakeResult { request });
            if let Response::Result { payload } = response {
                assert!(matches!(
                    broker
                        .borrow_mut()
                        .dispatch("transport", Command::ReleaseResult { request }),
                    Response::Ok
                ));
                break payload;
            }
            assert!(Instant::now() < deadline, "broker request did not settle");
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    let admitted = call(
        json!({"operation":"prepare","operation_id":"limiter-session","application_operation_id":"launch-one","application_ref":{"kind":"desktop","properties":if automatic {json!({"frame_generation_mode":"game-provided"})} else {Value::Null}},
        "client_context":{"client_ref":"client","display_mode":{"width":1920,"height":1080,"refresh_millihz":59940},"properties":{"stream_audio":null}},
        "requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}),
        1,
        &mut coordinator,
        &mut module,
    );
    assert_eq!(admitted["status"], "pending", "{admitted}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut id = 2;
    loop {
        let status = call(
            json!({"operation":"lookup","operation_id":"limiter-session"}),
            id,
            &mut coordinator,
            &mut module,
        );
        if status["status"] != "pending" {
            assert_eq!(status["status"], "succeeded", "{status}");
            break;
        }
        id += 1;
        assert!(Instant::now() < deadline, "prepare still pending: {status}");
    }
    let policy: Value = serde_json::from_slice(
        &std::fs::read(root.join("amaterasu/frame-limiter/active.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(policy["policies"][0]["limit_millihz"], 59940);
    let context_request = json!({"operation":"read_prepared_plans","target":admitted["target"],"owner":"transport","application_operation_id":"launch-one"});
    id += 1;
    let prepared = call(context_request.clone(), id, &mut coordinator, &mut module);
    assert_eq!(prepared["status"], "succeeded", "{prepared}");
    let prepared: amaterasu_module_sdk::extension_api::PreparedPlansResponse =
        serde_json::from_value(prepared).unwrap();
    let contributions =
        amaterasu_module_sdk::extension_api::launch_environment_contributions(&prepared.plans)
            .unwrap();
    let environment = amaterasu_module_sdk::extension_api::merge_launch_environment(
        &Default::default(),
        &contributions,
    )
    .unwrap();
    assert_eq!(policy["policies"][0]["provider"], effective_provider);
    if effective_provider != "mangohud" {
        assert_eq!(environment["VKD3D_FRAME_RATE"], "59.94");
        assert_eq!(environment["DXVK_FRAME_RATE"], "60");
    } else {
        assert!(!environment.contains_key("DXVK_FRAME_RATE"));
    }
    if effective_provider != "proton" {
        assert_eq!(environment["MANGOHUD"], "1");
        assert!(
            environment["MANGOHUD_CONFIG"].ends_with(if effective_provider == "mangohud" {
                "fps_limit=59.94"
            } else {
                "fps_limit=0"
            })
        );
    } else {
        assert!(!environment.contains_key("MANGOHUD"));
    }
    let mut foreign = context_request.clone();
    foreign["application_operation_id"] = json!("different-launch");
    id += 1;
    assert_eq!(
        call(foreign, id, &mut coordinator, &mut module)["status"],
        "rejected"
    );
    assert_eq!(
        coordinator.handle("stranger", "stranger", context_request.clone())["status"],
        "rejected"
    );
    assert!(
        std::fs::read_to_string(tool.join("user_settings.py"))
            .unwrap()
            .contains("BEGIN Amaterasu frame limiter")
    );
    for (cycle, refresh) in [60000_u32, 120000].into_iter().enumerate() {
        id += 1;
        let operation = format!("stream-{cycle}");
        let started = call(
            json!({"operation":"prepare_stream","operation_id":operation,
            "target":admitted["target"],"intent":if cycle == 0 { "initial" } else { "resume" },
            "consumer_requirements":["capture"],
            "client_context":{"client_ref":"client","display_mode":null,"properties":{"stream_audio":null}},
            "requested_video":{"width":1920,"height":1080,"refresh_millihz":refresh,"hdr":false}}),
            id,
            &mut coordinator,
            &mut module,
        );
        assert_eq!(started["status"], "pending", "{started}");
        let deadline = Instant::now() + Duration::from_secs(5);
        let prepared = loop {
            id += 1;
            let status = call(
                json!({"operation":"lookup","operation_id":operation}),
                id,
                &mut coordinator,
                &mut module,
            );
            if status["status"] != "pending" {
                break status;
            }
            assert!(Instant::now() < deadline, "stream still pending: {status}");
        };
        assert_eq!(prepared["status"], "succeeded", "{prepared}");
        assert_eq!(prepared["capture_binding_mode"], "external");
        assert!(prepared["candidate"].is_null());
        let target = prepared["target"].clone();
        id += 1;
        let denied = call(
            json!({"operation":"start_stream","operation_id":format!("early-{cycle}"),"target":target}),
            id,
            &mut coordinator,
            &mut module,
        );
        assert_eq!(denied["status"], "rejected");
        id += 1;
        let ack = call(
            json!({"operation":"ack_stream","operation_id":format!("ack-{cycle}"),"target":target,"consumer":"capture"}),
            id,
            &mut coordinator,
            &mut module,
        );
        assert_eq!(ack["status"], "succeeded", "{ack}");
        id += 1;
        let active = call(
            json!({"operation":"start_stream","operation_id":format!("start-{cycle}"),"target":target}),
            id,
            &mut coordinator,
            &mut module,
        );
        assert_eq!(active["all_consumers_acknowledged"], true, "{active}");
        let policy: Value = serde_json::from_slice(
            &std::fs::read(root.join("amaterasu/frame-limiter/active.json")).unwrap(),
        )
        .unwrap();
        assert!(
            policy["policies"]
                .as_array()
                .unwrap()
                .iter()
                .all(|policy| policy["limit_millihz"] == 59940)
        );
        if let Some(order) = std::env::var_os("AMATERASU_LIMITER_TEST_JOIN") {
            let source_first = order == "source-first";
            id += 1;
            let joined = call(
                json!({"operation":"prepare_stream","operation_id":"shared-native-join","target":admitted["target"],"intent":"join","consumer_requirements":["capture"],"client_context":{"client_ref":"second-client","properties":{}},"requested_video":{"width":1280,"height":720,"refresh_millihz":30000,"hdr":false}}),
                id,
                &mut coordinator,
                &mut module,
            );
            assert_eq!(joined["status"], "pending", "{joined}");
            let joined = loop {
                id += 1;
                let response = call(
                    json!({"operation":"lookup","operation_id":"shared-native-join"}),
                    id,
                    &mut coordinator,
                    &mut module,
                );
                if response["status"] != "pending" {
                    break response;
                }
            };
            assert_eq!(joined["status"], "succeeded", "{joined}");
            assert_eq!(joined["shared_source"], target);
            assert_eq!(joined["candidate"], Value::Null);
            assert_eq!(
                serde_json::from_slice::<Value>(
                    &std::fs::read(root.join("amaterasu/frame-limiter/active.json")).unwrap()
                )
                .unwrap(),
                policy,
                "joining must inherit active host policy"
            );
            id += 1;
            assert_eq!(
                call(
                    json!({"operation":"ack_stream","operation_id":"shared-ack","target":joined["target"],"consumer":"capture"}),
                    id,
                    &mut coordinator,
                    &mut module
                )["status"],
                "succeeded"
            );
            id += 1;
            assert_eq!(
                call(
                    json!({"operation":"start_stream","operation_id":"shared-start","target":joined["target"]}),
                    id,
                    &mut coordinator,
                    &mut module
                )["status"],
                "succeeded"
            );
            let departing = if source_first {
                [target.clone(), joined["target"].clone()]
            } else {
                [joined["target"].clone(), target.clone()]
            };
            for (index, target) in departing.into_iter().enumerate() {
                let operation = format!("shared-native-pause-{index}");
                id += 1;
                assert_eq!(
                    call(
                        json!({"operation":"pause_stream","operation_id":operation,"target":target}),
                        id,
                        &mut coordinator,
                        &mut module
                    )["status"],
                    "pending"
                );
                id += 1;
                let retired = call(
                    json!({"operation":"retire_stream","operation_id":format!("shared-native-retire-{index}"),"target":target,"retirement_receipt":{"response":{"state":"not_found"}}}),
                    id,
                    &mut coordinator,
                    &mut module,
                );
                assert!(
                    matches!(retired["status"].as_str(), Some("pending" | "succeeded")),
                    "{retired}"
                );
                let deadline = Instant::now() + Duration::from_secs(5);
                let retired = loop {
                    id += 1;
                    let response = call(
                        json!({"operation":"lookup","operation_id":operation}),
                        id,
                        &mut coordinator,
                        &mut module,
                    );
                    if response["status"] != "pending" {
                        break response;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "native shared cleanup stalled: {response}"
                    );
                };
                assert_eq!(retired["status"], "succeeded", "{retired}");
                assert_eq!(retired["consumer_retired"], true);
                assert_eq!(retired["shared_retained"], index == 0);
                assert_eq!(retired["resources_released"], index == 1);
                assert!(
                    root.join("amaterasu/frame-limiter/active.json").exists(),
                    "the application root keeps policy after either client departs"
                );
                assert_eq!(fresh_proton_environment(&tool, &root)["applied"], true);
            }
            break;
        }
        if cycle == 0 && std::env::var_os("AMATERASU_LIMITER_TEST_RESTART").is_some() {
            module.restart();
            let deadline = Instant::now() + Duration::from_secs(5);
            while root.join("amaterasu/frame-limiter/active.json").exists() {
                module.tick();
                assert!(
                    Instant::now() < deadline,
                    "restarted module did not retire its policy"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if cycle == 0 && std::env::var_os("AMATERASU_LIMITER_TEST_RESTART").is_some() {
            drop(coordinator);
            coordinator = Coordinator::configured(&settings).unwrap();
            module.restart();
            id += 1;
            let operation = "retire-after-process-restart";
            let pending = call(
                json!({"operation":"retire_stream","operation_id":operation,
                "restart_recovery":true,"target":target,
                "retirement_receipt":{"service":"video/1","operation_id":"native-video",
                    "response":{"code":"not_found"}}}),
                id,
                &mut coordinator,
                &mut module,
            );
            assert_eq!(pending["status"], "pending", "{pending}");
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                id += 1;
                let result = call(
                    json!({"operation":"lookup","operation_id":operation}),
                    id,
                    &mut coordinator,
                    &mut module,
                );
                if result["status"] != "pending" {
                    assert_eq!(result["status"], "succeeded", "{result}");
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "restart cleanup stalled: {result}"
                );
            }
            assert!(!root.join("amaterasu/frame-limiter/active.json").exists());
            return;
        }
        id += 1;
        let pause_id = format!("pause-{cycle}");
        let pause = call(
            json!({"operation":"pause_stream","operation_id":pause_id,"target":target}),
            id,
            &mut coordinator,
            &mut module,
        );
        assert_eq!(pause["fenced"], true, "{pause}");
        id += 1;
        let retired = call(
            json!({"operation":"retire_stream","operation_id":format!("retire-{cycle}"),
            "target":target,"retirement_receipt":{"response":{"state":"not_found"}}}),
            id,
            &mut coordinator,
            &mut module,
        );
        assert_eq!(retired["status"], "pending", "{retired}");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            id += 1;
            let status = call(
                json!({"operation":"lookup","operation_id":pause_id}),
                id,
                &mut coordinator,
                &mut module,
            );
            if status["status"] != "pending" {
                assert_eq!(status["status"], "succeeded", "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "pause still pending: {status}");
        }
        assert!(root.join("amaterasu/frame-limiter/active.json").exists());
        let fresh = fresh_proton_environment(&tool, &root);
        assert_eq!(
            fresh["applied"], true,
            "a paused application lost its launch policy"
        );
        if effective_provider != "mangohud" {
            assert_eq!(fresh["environment"]["VKD3D_FRAME_RATE"], "59.94");
        }
    }
    id += 1;
    let ending = call(
        json!({"operation":"end_session","operation_id":"limiter-end","target":admitted["target"]}),
        id,
        &mut coordinator,
        &mut module,
    );
    assert_eq!(ending["status"], "pending", "{ending}");
    id += 1;
    let retired = call(
        json!({"operation":"application_retired","operation_id":"desktop-retired","target":admitted["target"]}),
        id,
        &mut coordinator,
        &mut module,
    );
    assert_eq!(retired["status"], "pending", "{retired}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        id += 1;
        let status = call(
            json!({"operation":"lookup","operation_id":"limiter-end"}),
            id,
            &mut coordinator,
            &mut module,
        );
        if status["status"] != "pending" {
            assert_eq!(status["status"], "succeeded", "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "cleanup still pending: {status}");
    }
    assert!(!root.join("amaterasu/frame-limiter/active.json").exists());
    id += 1;
    assert_eq!(
        call(context_request, id, &mut coordinator, &mut module)["status"],
        "rejected"
    );
    drop(module);
}

#[test]
#[ignore = "requires the installed frame-limiter cdylib; uses disposable provider state"]
fn native_limiter_reconciles_old_stream_context_after_coordinator_restart() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "native_limiter_prepares_through_unchanged_coordinator_and_broker",
            "--nocapture",
        ])
        .env("AMATERASU_LIMITER_TEST_RESTART", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "requires rebuilt frame-limiter cdylib; only disposable Proton fixtures are modified"]
fn native_limiter_shared_external_clients_preserve_policy_until_last_retirement() {
    use std::os::unix::fs::DirBuilderExt;
    let library = std::env::var_os("AMATERASU_FRAME_LIMITER_LIBRARY")
        .expect("supply rebuilt frame-limiter library");
    let root =
        std::env::temp_dir().join(format!("amaterasu-limiter-shared-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let mut passed = true;
    for order in ["source-first", "subscriber-first"] {
        let directory = root.join(order);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        passed &= std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_limiter_prepares_through_unchanged_coordinator_and_broker",
                "--ignored",
                "--nocapture",
            ])
            .env("AMATERASU_FRAME_LIMITER_LIBRARY", &library)
            .env("AMATERASU_LIMITER_TEST_CHILD", &directory)
            .env("XDG_DATA_HOME", &directory)
            .env("XDG_STATE_HOME", &directory)
            .env("AMATERASU_LIMITER_TEST_AUTOMATIC", "0")
            .env("AMATERASU_LIMITER_TEST_OVERLAY", "0")
            .env("AMATERASU_LIMITER_TEST_JOIN", order)
            .env("AMATERASU_LIMITER_TEST_PROVIDER", "proton")
            .env_remove("AMATERASU_LIMITER_TEST_RESTART")
            .status()
            .unwrap()
            .success();
    }
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        passed,
        "native shared-source lifecycle qualification failed"
    );
}
