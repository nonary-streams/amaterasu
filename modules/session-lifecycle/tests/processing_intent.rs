//! Opt-in cross-ABI qualification of read-only processing policy. The installed
//! RTX library is loaded only into a private broker; no processor is bound or
//! created and no capture/display service exists in this fixture.
#![cfg(target_os = "linux")]
use amaterasu_extension_api::{
    ConsumerChange, ProviderOperation, ProviderRequest, ProviderResponse, ResourcePlan,
};
use amaterasu_module_sdk::{ApiMethod, Command, Response, abi};
use amaterasu_runtime::Broker;
use amaterasu_session_lifecycle::{Coordinator, LifecycleSettings};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    ffi::c_void,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    rc::Rc,
};

const COORDINATOR: &str = "amaterasu.session-lifecycle";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const PROVIDER: &str = "amaterasu.rtx-hdr";
const POLICY: &str = "nonary.rtx-hdr/1";
const PROCESSOR: &str = "amaterasu.rtx-hdr/1";

struct Endpoint {
    settings: Value,
    methods: Vec<ApiMethod>,
    broker: Option<Rc<RefCell<Broker>>>,
    intents: Vec<Value>,
    native_bind_attempts: usize,
    native_factory_registrations: usize,
}
unsafe extern "C" fn invoke(
    context: *mut c_void,
    bytes: abi::BorrowedBytes,
    sink: abi::ReplySink,
) -> abi::Status {
    // SAFETY: NativeModule retains its boxed endpoint for every serialized call.
    let endpoint = unsafe { &mut *context.cast::<Endpoint>() };
    // SAFETY: the module lends the input until this synchronous callback returns.
    let Ok(bytes) = (unsafe { bytes.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    if matches!(
        command,
        Command::NativeBind { .. } | Command::NativeBindTyped { .. }
    ) {
        endpoint.native_bind_attempts += 1;
        return abi::STATUS_INVALID_ARGUMENT;
    }
    if let Command::Complete { payload, .. } = &command
        && payload.get("processor_settings").is_some()
    {
        endpoint.intents.push(payload.clone());
    }
    // This fixture intentionally exposes no GPU native binding endpoint. The
    // trusted provider may publish its unused factory; policy API traffic still
    // goes through the real broker, and any attempt to bind is rejected above.
    let response = if matches!(
        command,
        Command::NativeRegisterTyped { .. } | Command::NativeRegister { .. }
    ) {
        endpoint.native_factory_registrations += 1;
        Response::Ok
    } else if let Some(broker) = &endpoint.broker {
        broker.borrow_mut().dispatch(PROVIDER, command)
    } else {
        match command {
            Command::GetSettings {} => Response::Result {
                payload: endpoint.settings.clone(),
            },
            Command::RegisterApi { method } => {
                endpoint.methods.push(method);
                Response::Ok
            }
            Command::Register { .. }
            | Command::NativeRegisterTyped { .. }
            | Command::NativeRegister { .. } => Response::Ok,
            _ => return abi::STATUS_INVALID_ARGUMENT,
        }
    };
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: ReplySink copies these bytes before the borrowed vector is dropped.
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
    fn load(path: &Path, settings: Value, broker: Option<Rc<RefCell<Broker>>>) -> Self {
        // SAFETY: this opt-in fixture loads the explicitly selected installed
        // module, retaining the library until its instance is destroyed.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        // SAFETY: the trusted library exports the documented Amaterasu ABI entry.
        let entry = unsafe {
            library.get::<unsafe extern "C" fn() -> *const abi::ModuleApi>(b"amaterasu_module_v1\0")
        }
        .unwrap();
        // SAFETY: the module table remains owned by the retained loaded library.
        let api = unsafe { *entry() };
        assert!(api.header.supports::<abi::ModuleApi>());
        let mut module = Self {
            _library: library,
            endpoint: Box::new(Endpoint {
                settings,
                methods: vec![],
                broker,
                intents: vec![],
                native_bind_attempts: 0,
                native_factory_registrations: 0,
            }),
            api,
            instance: std::ptr::null_mut(),
        };
        let host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *module.endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: the boxed endpoint and module table outlive the created instance.
        assert_eq!(
            unsafe { (module.api.create)(&host, &mut module.instance) },
            abi::STATUS_OK
        );
        module
    }
    fn tick(&mut self) {
        // SAFETY: the instance was created by this retained live module table.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
    }
}
impl Drop for NativeModule {
    fn drop(&mut self) {
        // SAFETY: destroy exactly once before releasing the endpoint or library.
        unsafe { (self.api.destroy)(self.instance) };
    }
}
struct PrivateRoot(PathBuf);
impl PrivateRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-processing-policy-{}",
            amaterasu_storage::random_identifier().unwrap()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }
}
impl Drop for PrivateRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Public frozen values read from failed Desktop session
/// 2c88016c833b6cb8c93adad01dd755e7, revision registry-0. No installed journals
/// or settings are read or changed by this test.
fn frozen_settings(enabled: bool) -> Value {
    json!({
        "rtx_hdr_contrast":0,"rtx_hdr_debanding":true,"rtx_hdr_enable":enabled,
        "rtx_hdr_foreground_only":false,"rtx_hdr_fullscreen_only":false,"rtx_hdr_middle_gray":50,
        "rtx_hdr_peak_brightness":1000,"rtx_hdr_saturation":0,"rtx_hdr_sdr_brightness":0
    })
}
fn startup_settings(root: &Path) -> Value {
    json!({"runtime_directory":root,"require_verified_runtime":false,
    "lifecycle_coordinator":{"principal":COORDINATOR,"service":LIFECYCLE},
    "processing_consumer_principals":[COORDINATOR]})
}

enum PolicyProvider {
    Installed(NativeModule),
    Replacement { intents: Vec<Value> },
}
impl PolicyProvider {
    fn tick(&mut self, broker: &Rc<RefCell<Broker>>) {
        match self {
            Self::Installed(module) => module.tick(),
            Self::Replacement { intents } => {
                let incoming = broker.borrow_mut().dispatch(PROVIDER, Command::Poll {});
                let Response::Request {
                    id,
                    caller,
                    payload,
                    ..
                } = incoming
                else {
                    return;
                };
                assert_eq!(caller, COORDINATOR);
                let args = &payload["arguments"];
                let response = if payload["method"] == "processing_intent" {
                    let enabled = args["settings"]["rtx_hdr_enable"].as_bool().unwrap();
                    let mut configuration = args["settings"].clone();
                    configuration["replacement_context"] =
                        json!({"target":args["target"],"version":1});
                    let response = json!({"status":"succeeded","target":args["target"],"revision":args["revision"],
                    "enabled":enabled,"source_hdr_required":enabled.then_some(false),"output_hdr_required":enabled.then_some(true),
                    "processor_service":PROCESSOR,"processor_settings":configuration});
                    intents.push(response.clone());
                    response
                } else {
                    let request: ProviderRequest = serde_json::from_value(args.clone()).unwrap();
                    amaterasu_extension_api::validate_provider_request(&request).unwrap();
                    let response = match request.operation {
                        ProviderOperation::ResourcePlan => ProviderResponse::Planned {
                            plan: ResourcePlan {
                                plan_id: "replacement-policy".into(),
                                revision: 1,
                                resources: vec![],
                                mutation_domains: vec![],
                                required_capabilities: vec![],
                                constraints: json!({}),
                                obligations: vec![],
                                affected_consumers: vec![],
                                consumer_change: ConsumerChange::Live,
                                candidate_constraint: None,
                                allowed_degradation: vec![],
                                control_proposals: vec![],
                                resource_disposition: None,
                            },
                        },
                        ProviderOperation::LifecycleCallback => ProviderResponse::Prepared {
                            candidate: None,
                            obligations: vec![],
                            outputs: vec![],
                        },
                        _ => panic!("fixture never requests status or native operations"),
                    };
                    amaterasu_extension_api::validate_provider_response(&request, &response)
                        .unwrap();
                    serde_json::to_value(response).unwrap()
                };
                assert!(matches!(
                    broker.borrow_mut().dispatch(
                        PROVIDER,
                        Command::Complete {
                            request: id,
                            payload: response
                        }
                    ),
                    Response::Ok
                ));
            }
        }
    }
    fn intents(&self) -> &[Value] {
        match self {
            Self::Installed(module) => &module.endpoint.intents,
            Self::Replacement { intents } => intents,
        }
    }
}
struct Fixture {
    broker: Rc<RefCell<Broker>>,
    coordinator: Coordinator,
    provider: PolicyProvider,
    next: u64,
    _root: PrivateRoot,
}
impl Fixture {
    fn new(library: &Path, installed: bool, enabled: bool) -> Self {
        let root = PrivateRoot::new();
        let startup = startup_settings(&root.0);
        let probe = NativeModule::load(library, startup.clone(), None);
        let methods = probe.endpoint.methods.clone();
        drop(probe);
        assert!(
            methods
                .iter()
                .any(|method| method.service == POLICY && method.method == "processing_intent")
        );
        let method = |name: &str| json!({"service":POLICY,"method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
        let settings:LifecycleSettings=serde_json::from_value(json!({"journal_path":root.0.join("journal.json"),
            "capture_binding_mode":"external","trusted_callers":["transport",PROVIDER],"session_delegates":["transport"],
            "consumers":{"capture":["transport"]},"operation_grants":{"transport":["prepare","prepare_stream","lookup","snapshot"]},
            "consumer_requirements":{"execution_domain":"desktop","capture_providers":[],"selector_schemas":[]},
            "processing_policy":{"provider":PROVIDER,"service":POLICY,"processor_service":PROCESSOR,"source_mode_providers":[]},
            "participants":[{"provider":PROVIDER,"plan_method":method("resource_plan"),"callback_method":method("lifecycle_callback"),
                "status_method":method("lifecycle_status"),"settings_revision":"registry-0","settings":frozen_settings(enabled),
                "capabilities":[],"cleanup_methods":[method("lifecycle_callback")]}]})).unwrap();
        let config=serde_json::from_value(json!({"principals":{
            COORDINATOR:{"provide":[LIFECYCLE],"call":[POLICY]},PROVIDER:{"provide":[POLICY,PROCESSOR]},
            "transport":{"call":[LIFECYCLE]}},"settings":{PROVIDER:startup},
            "api_methods":methods.iter().map(|method|json!({"provider":PROVIDER,"callers":[COORDINATOR],"method":method})).collect::<Vec<_>>() })).unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(config).unwrap()));
        assert!(matches!(
            broker.borrow_mut().dispatch(
                COORDINATOR,
                Command::Register {
                    service: LIFECYCLE.into()
                }
            ),
            Response::Ok
        ));
        let coordinator = Coordinator::configured(&settings).unwrap();
        let provider = if installed {
            PolicyProvider::Installed(NativeModule::load(
                library,
                Value::Null,
                Some(broker.clone()),
            ))
        } else {
            for method in methods {
                assert!(matches!(
                    broker
                        .borrow_mut()
                        .dispatch(PROVIDER, Command::RegisterApi { method }),
                    Response::Ok
                ));
            }
            PolicyProvider::Replacement { intents: vec![] }
        };
        Self {
            broker,
            coordinator,
            provider,
            next: 0,
            _root: root,
        }
    }
    fn tick(&mut self) {
        self.coordinator
            .advance(&mut |command| Ok(self.broker.borrow_mut().dispatch(COORDINATOR, command)))
            .unwrap();
        let incoming = self
            .broker
            .borrow_mut()
            .dispatch(COORDINATOR, Command::Poll {});
        if let Response::Request {
            id,
            caller,
            initiator,
            payload,
            ..
        } = incoming
        {
            let response = self.coordinator.handle(&caller, &initiator, payload);
            assert!(matches!(
                self.broker.borrow_mut().dispatch(
                    COORDINATOR,
                    Command::Complete {
                        request: id,
                        payload: response
                    }
                ),
                Response::Ok
            ));
        }
        self.provider.tick(&self.broker);
    }
    fn call(&mut self, payload: Value) -> Value {
        self.next += 1;
        let Response::Accepted { id } = self.broker.borrow_mut().dispatch(
            "transport",
            Command::Request {
                service: LIFECYCLE.into(),
                payload,
                timeout_ms: 5000,
                operation_id: format!("fixture-{}", self.next),
                parent: None,
            },
        ) else {
            panic!("broker rejected fixture request");
        };
        for _ in 0..512 {
            self.tick();
            let response = self
                .broker
                .borrow_mut()
                .dispatch("transport", Command::TakeResult { request: id });
            if let Response::Result { payload } = response {
                assert!(matches!(
                    self.broker
                        .borrow_mut()
                        .dispatch("transport", Command::ReleaseResult { request: id }),
                    Response::Ok
                ));
                return payload;
            }
        }
        panic!("bounded processing-policy fixture did not answer");
    }
    fn settled(&mut self, operation: &str) -> Value {
        for _ in 0..128 {
            let response = self.call(json!({"operation":"lookup","operation_id":operation}));
            if response["status"] != "pending" {
                return response;
            }
        }
        panic!("processing preflight did not settle");
    }
}

#[test]
#[ignore = "loads explicitly supplied installed linux-nvidia RTX library in an isolated read-only policy fixture"]
fn installed_processing_context_and_replacement_pass_unchanged_coordinator_preflight() {
    let library = PathBuf::from(
        std::env::var_os("AMATERASU_RTX_HDR_LIBRARY").expect("supply installed RTX HDR library"),
    );
    assert!(library.is_absolute());
    for installed in [true, false] {
        for (enabled, hdr) in [(false, false), (false, true), (true, true)] {
            let mut fixture = Fixture::new(&library, installed, enabled);
            let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":hdr});
            let admitted=fixture.call(json!({"operation":"prepare","operation_id":"desktop","application_ref":{
            "kind":"desktop","app_id":"app-000000007fffffff","executable_path":null,"properties":null},"requested_video":video}));
            assert_eq!(admitted["status"], "pending", "{admitted}");
            let session = fixture.settled("desktop");
            assert_eq!(session["status"], "succeeded", "{session}");
            let stream=fixture.call(json!({"operation":"prepare_stream","operation_id":"stream","target":session["target"],
            "intent":"initial","requested_video":video,"consumer_requirements":["capture"]}));
            assert_eq!(stream["status"], "pending", "{stream}");
            let stream = fixture.settled("stream");
            assert_eq!(stream["status"], "succeeded", "{stream}");
            assert_eq!(
                fixture.provider.intents().len(),
                2,
                "session and stream preflights use the selected provider"
            );
            for intent in fixture.provider.intents() {
                assert_eq!(intent["enabled"], enabled);
                let configuration = &intent["processor_settings"];
                for (key, value) in frozen_settings(enabled).as_object().unwrap() {
                    assert_eq!(&configuration[key], value);
                }
                assert_ne!(
                    *configuration,
                    frozen_settings(enabled),
                    "provider supplies additive context"
                );
                if installed {
                    assert_eq!(configuration["foreground_target"], intent["target"]);
                } else {
                    assert_eq!(
                        configuration["replacement_context"]["target"],
                        intent["target"]
                    );
                }
            }
            if let PolicyProvider::Installed(module) = &fixture.provider {
                assert_eq!(module.endpoint.native_bind_attempts, 0);
                assert_eq!(module.endpoint.native_factory_registrations, 1);
            }
        }
    }
}
