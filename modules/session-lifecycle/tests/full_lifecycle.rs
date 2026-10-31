//! Cross-module tests run the production lifecycle ABI against the actual broker.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVICE: &str = "amaterasu.session-lifecycle/1";
const COORDINATOR: &str = "amaterasu.session-lifecycle";
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Endpoint {
    broker: Rc<RefCell<Broker>>,
    principal: &'static str,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: abi::BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: Harness retains its boxed Endpoint until the module is destroyed;
    // module calls and broker pumping are serialized on this test thread.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    // SAFETY: the SDK lends valid input bytes for the synchronous ABI call.
    let Ok(input) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice(input) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the SDK's reply sink synchronously copies the temporary bytes.
    unsafe { reply.send(&bytes) };
    abi::STATUS_OK
}

struct Harness {
    broker: Rc<RefCell<Broker>>,
    endpoint: Box<Endpoint>,
    api: abi::ModuleApi,
    instance: *mut c_void,
    root: PathBuf,
    request_sequence: u64,
    provider_calls: Vec<Value>,
    provider: bool,
    registry_settings: Option<(u64, Value)>,
}

impl Harness {
    fn with_provider(provider: bool) -> Self {
        Self::with_registry(provider, None)
    }

    fn with_registry(provider: bool, registry_settings: Option<(u64, Value)>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-lifecycle-broker-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut configuration = json!({
            "principals": {
                COORDINATOR: {"provide": [SERVICE]},
                "transport": {"call": [SERVICE]},
                "capture": {"call": [SERVICE]},
                "amaterasu.input": {"call": [SERVICE], "provide": ["amaterasu.input/1"]},
                "launch": {"provide": ["amaterasu.launch/1"]}
            },
            "settings": {
                COORDINATOR: {
                    "journal_path": root.join("journal.json"),
                    "consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},
                    "trusted_callers": ["transport", "capture", "amaterasu.input"],
                    "session_delegates": ["capture", "amaterasu.input"],
                    "consumers": {"capture":["capture"], "input":["amaterasu.input"]},
                    "operation_grants": {
                        "transport": ["prepare", "lookup", "snapshot", "prepare_stream", "start_stream", "pause_stream", "resume_stream", "end_session", "bind_application", "application_retired", "issue_input_binding"],
                        "capture": ["ack_binding", "retire_stream", "lookup"],
                        "amaterasu.input": ["ack_binding", "lookup", "validate_input_binding"]
                    }
                }
            }
        });
        let mut methods = Vec::new();
        if provider {
            configuration["principals"][COORDINATOR]["call"] = json!([
                "example.display/1",
                "amaterasu.input/1",
                "amaterasu.launch/1"
            ]);
            configuration["principals"]["display"] =
                json!({"provide":["example.display/1"],"call":[SERVICE]});
            configuration["settings"][COORDINATOR]["trusted_callers"] =
                json!(["transport", "capture", "amaterasu.input", "display"]);
            configuration["settings"][COORDINATOR]["operation_grants"]["display"] =
                json!(["verify_provider_epoch"]);
            let method_ref = |name: &str| json!({"service":"example.display/1","method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
            configuration["settings"][COORDINATOR]["consumer_requirements"] = json!({"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]});
            configuration["settings"][COORDINATOR]["participants"] = json!([{"provider":"display","plan_method":method_ref("resource_plan"),"callback_method":method_ref("lifecycle_callback"),"status_method":method_ref("lifecycle_status"),"binding_validate_method":method_ref("binding_validate"),"settings_revision":"one","settings":{},"capabilities":[],"cleanup_methods":[]}]);
            for name in [
                "resource_plan",
                "lifecycle_callback",
                "lifecycle_status",
                "binding_validate",
            ] {
                let method = json!({"service":"example.display/1","method":name,"interface_major":1,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"});
                methods.push(method);
            }
            configuration["api_methods"]=json!(methods.iter().map(|method|json!({"provider":"display","callers":[COORDINATOR],"method":method})).collect::<Vec<_>>());
        }
        if registry_settings.is_some() {
            configuration["principals"][COORDINATOR]["call"]
                .as_array_mut()
                .unwrap()
                .push(json!("amaterasu.extension-registry/1"));
            configuration["principals"]["registry"] =
                json!({"provide":["amaterasu.extension-registry/1"]});
            configuration["settings"][COORDINATOR]["settings_registry_service"] =
                json!("amaterasu.extension-registry/1");
        }
        let configuration = serde_json::from_value(configuration).unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(configuration).unwrap()));
        for method in methods {
            let response = broker.borrow_mut().dispatch(
                "display",
                Command::RegisterApi {
                    method: serde_json::from_value(method).unwrap(),
                },
            );
            assert!(matches!(response, Response::Ok));
        }
        for (principal, service) in [
            ("amaterasu.input", "amaterasu.input/1"),
            ("launch", "amaterasu.launch/1"),
        ] {
            assert!(matches!(
                broker.borrow_mut().dispatch(
                    principal,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        if registry_settings.is_some() {
            assert!(matches!(
                broker.borrow_mut().dispatch(
                    "registry",
                    Command::Register {
                        service: "amaterasu.extension-registry/1".into()
                    }
                ),
                Response::Ok
            ));
        }
        let endpoint = Box::new(Endpoint {
            broker: broker.clone(),
            principal: COORDINATOR,
        });
        // SAFETY: the exported table is static and has the ABI version checked below.
        let api = unsafe { *amaterasu_session_lifecycle::amaterasu_module_v1() };
        assert!(api.header.supports::<abi::ModuleApi>());
        let mut harness = Self {
            broker,
            endpoint,
            api,
            instance: std::ptr::null_mut(),
            root,
            request_sequence: 0,
            provider_calls: Vec::new(),
            provider,
            registry_settings,
        };
        harness.start();
        harness
    }

    fn start(&mut self) {
        let endpoint = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *self.endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: Endpoint is boxed and outlives the module; creation and all
        // following lifecycle calls are serialized and use this module's table.
        assert_eq!(
            unsafe { (self.api.create)(&endpoint, &mut self.instance) },
            abi::STATUS_OK
        );
        assert!(!self.instance.is_null());
    }

    fn tick(&mut self) {
        // SAFETY: instance is a live module created by this exact table.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
        if self.provider {
            let request = self
                .broker
                .borrow_mut()
                .dispatch("display", Command::Poll {});
            if let Response::Request {
                id,
                caller,
                payload,
                ..
            } = request
            {
                assert_eq!(caller, COORDINATOR);
                self.provider_calls.push(payload.clone());
                let args = &payload["arguments"];
                if payload["method"] == "resource_plan" {
                    let stream = args["phase"] == "stream.prepare";
                    let resources = if stream {
                        json!([{"provider":"display","resource_id":"output","incarnation":null,"expected_revision":null,"execution_domain":"seat","mutation_domains":[],"access":"read"}])
                    } else {
                        json!([])
                    };
                    let constraint = if stream {
                        json!({"capture_provider":"capture","selector_schema":"example/1","resource_provider":"display","resource_id":"output","incarnation":null,"execution_domain":"seat","mode":args["requested_video"],"plan_revision":1})
                    } else {
                        Value::Null
                    };
                    let plan = json!({"plan_id":"plan","revision":1,"resources":resources,"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":constraint,"allowed_degradation":[]});
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: json!({"status":"planned","plan":plan})
                            }
                        ),
                        Response::Ok
                    ));
                } else if payload["method"] == "binding_validate" {
                    // Producer incarnation is not a consumer's native borrow token.
                    assert!(args.get("borrow_id").is_none());
                    assert_eq!(args["binding"]["resource"]["incarnation"], "native");
                    let result = json!({"status":"succeeded","target":args["target"],"binding":args["binding"],"observation":args["observation"],"plan_revision":args["plan_revision"]});
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: result
                            }
                        ),
                        Response::Ok
                    ));
                } else if payload["method"] == "lifecycle_callback" {
                    let response = if args["phase"] == "stream.prepare" {
                        json!({"status":"prepared","candidate":{"capture_provider":"capture","selector_schema":"example/1","selector":{},"resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"mode":args["requested_video"],"plan_revision":1,"geometry_revision":1,"generation":args["target"]["generation"]},"obligations":[]})
                    } else if args["phase"] == "stream.release"
                        || args["phase"] == "session.release"
                    {
                        json!({"status":"cleanup","outcomes":[]})
                    } else {
                        json!({"status":"prepared","candidate":null,"obligations":[]})
                    };
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: response
                            }
                        ),
                        Response::Ok
                    ));
                } else {
                    panic!("unexpected provider method: {payload}");
                }
            }
        }
        if let Some((revision, settings)) = &self.registry_settings {
            let request = self
                .broker
                .borrow_mut()
                .dispatch("registry", Command::Poll {});
            if let Response::Request {
                id,
                caller,
                payload,
                ..
            } = request
            {
                assert_eq!(caller, COORDINATOR);
                assert_eq!(payload["operation"], "settings_get");
                assert_eq!(payload["provider"], "display");
                assert_eq!(payload["scope"], json!({"kind":"host"}));
                let effective: serde_json::Map<String, Value> = settings
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| {
                        (key.clone(), json!({"value":value,"source":{"kind":"host"}}))
                    })
                    .collect();
                assert!(matches!(self.broker.borrow_mut().dispatch("registry", Command::Complete { request: id, payload: json!({"ok":true,"status":"ok","revision":revision,"effective":effective}) }), Response::Ok));
            }
        }
        for principal in ["amaterasu.input", "launch"] {
            let request = self
                .broker
                .borrow_mut()
                .dispatch(principal, Command::Poll {});
            if let Response::Request { id, payload, .. } = request {
                let response = match payload["action"].as_str() {
                    Some("binding_status") => {
                        json!({"ok":true,"status":"bound","token":payload["token"],"target":payload["target"],"geometry_revision":1,"lease":"lease-1"})
                    }
                    Some("revoke_binding") => {
                        json!({"status":"retired","token":payload["token"],"target":payload["target"]})
                    }
                    Some("verify_retirement") => {
                        json!({"ok":true,"retired":true,"owner":payload["owner"],"launch_id":payload["launch_id"],"operation_id":payload["operation_id"]})
                    }
                    _ => panic!("unexpected {principal} request: {payload}"),
                };
                assert!(matches!(
                    self.broker.borrow_mut().dispatch(
                        principal,
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

    fn call(&mut self, caller: &str, payload: Value) -> Value {
        self.request_sequence += 1;
        let response = self.broker.borrow_mut().dispatch(
            caller,
            Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 5_000,
                operation_id: format!("test-request-{}", self.request_sequence),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("request was not admitted by broker: {response:?}");
        };
        for _ in 0..32 {
            self.tick();
            let response = self
                .broker
                .borrow_mut()
                .dispatch(caller, Command::TakeResult { request: id });
            match response {
                Response::Pending => continue,
                Response::Result { payload } => {
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch(caller, Command::ReleaseResult { request: id }),
                        Response::Ok
                    ));
                    return payload;
                }
                response => panic!("unexpected lifecycle result: {response:?}"),
            }
        }
        panic!("lifecycle failed to reply within bounded test pump");
    }
}

impl Harness {
    fn settled(&mut self, caller: &str, operation_id: &str) -> Value {
        for _ in 0..120 {
            let result = self.call(
                caller,
                json!({"operation":"lookup","operation_id":operation_id}),
            );
            if result["status"] != "pending" {
                return result;
            }
        }
        panic!("operation {operation_id} did not settle");
    }

    fn prepare_and_start_stream(
        &mut self,
        session: &Value,
        operation: &str,
        resume: bool,
    ) -> Value {
        let id = format!("{operation}.prepare");
        let started = self.call("transport", json!({"operation":if resume {"resume_stream"} else {"prepare_stream"}, "operation_id":id,"target":session,"intent":if resume {"resume"} else {"initial"},"requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
        assert_eq!(started["status"], "pending", "{started}");
        let ready = self.settled("transport", &id);
        assert_eq!(ready["status"], "succeeded", "{ready}");
        let target = ready["target"].clone();
        let candidate = ready["candidate"].clone();
        let premature = self.call("transport", json!({"operation":"start_stream","operation_id":format!("{operation}.premature-start"),"target":target}));
        assert_eq!(premature["status"], "rejected", "{premature}");
        assert_eq!(premature["reason"], "consumers_not_acknowledged");
        let capture_id = format!("{operation}.capture");
        let observed = json!({"version":1,"protocol":"wayland","compositor_id":"seat","registry_global":1,"output_name":"output","width":1920,"height":1080,"refresh_millihz":60000});
        let ack = self.call("capture", json!({"operation":"ack_binding","operation_id":capture_id,"target":target,"consumer":"capture","plan_revision":1,"candidate":candidate,"observed_capture":observed}));
        assert_eq!(ack["status"], "pending", "{ack}");
        let ack = self.settled("capture", &capture_id);
        assert_eq!(ack["status"], "succeeded", "{ack}");
        let binding = self.call("transport", json!({"operation":"issue_input_binding","operation_id":format!("{operation}.input-token"),"target":target}));
        assert_eq!(binding["status"], "succeeded", "{binding}");
        let token = binding["token"].clone();
        let validate = |boot: &str| json!({"operation":"validate_input_binding","token":token,"input_boot_epoch":boot});
        let valid = self.call("amaterasu.input", validate("fake-input-boot"));
        assert_eq!(valid["status"], "succeeded", "{valid}");
        let repeated = self.call("amaterasu.input", validate("fake-input-boot"));
        assert_eq!(repeated["status"], "succeeded", "{repeated}");
        let restarted = self.call("amaterasu.input", validate("different-input-boot"));
        assert_eq!(restarted["status"], "rejected", "{restarted}");
        assert_eq!(restarted["reason"], "input_provider_restarted");
        let input_id = format!("{operation}.input");
        let ack = self.call("amaterasu.input", json!({"operation":"ack_binding","operation_id":input_id,"target":target,"consumer":"input","plan_revision":1,"candidate":candidate}));
        assert_eq!(ack["status"], "pending", "{ack}");
        let ack = self.settled("amaterasu.input", &input_id);
        assert_eq!(ack["status"], "succeeded", "{ack}");
        let started = self.call("transport", json!({"operation":"start_stream","operation_id":format!("{operation}.start"),"target":target}));
        assert_eq!(started["status"], "succeeded", "{started}");
        assert_eq!(started["all_consumers_acknowledged"], true);
        target
    }

    fn pause(&mut self, target: &Value, name: &str) {
        let id = format!("{name}.pause");
        let fence = self.call(
            "transport",
            json!({"operation":"pause_stream","operation_id":id,"target":target}),
        );
        assert_eq!(fence["status"], "pending", "{fence}");
        assert_eq!(fence["fenced"], true);
        let retired = self.call("capture", json!({"operation":"retire_stream","operation_id":format!("{name}.retire"),"target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
        let result = self.settled("transport", &id);
        assert_eq!(result["status"], "succeeded", "{result}");
    }
}

#[test]
fn production_abi_drives_prepare_pause_resume_and_application_retirement() {
    let mut harness = Harness::with_provider(true);
    let prepared = harness.call("transport", json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let ready = harness.settled("transport", "session.prepare");
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let first = harness.prepare_and_start_stream(&session, "first", false);
    harness.pause(&first, "first");
    let second = harness.prepare_and_start_stream(&session, "second", true);
    assert_ne!(first["stream_id"], second["stream_id"]);
    harness.pause(&second, "second");
    let end = harness.call(
        "transport",
        json!({"operation":"end_session","operation_id":"session.end","target":session}),
    );
    assert_eq!(end["status"], "pending", "{end}");
    let before_app = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"session.end"}),
    );
    assert_eq!(before_app["status"], "pending", "{before_app}");
    let app = harness.call(
        "transport",
        json!({"operation":"application_retired","operation_id":"app.retired","target":session}),
    );
    assert_eq!(app["status"], "pending", "{app}");
    let app = harness.settled("transport", "app.retired");
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = harness.settled("transport", "session.end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    for (phase, expected) in [
        ("session.prepare", 1),
        ("stream.prepare", 2),
        ("stream.release", 2),
        ("session.release", 1),
    ] {
        assert_eq!(
            harness
                .provider_calls
                .iter()
                .filter(|call| call["method"] == "lifecycle_callback"
                    && call["arguments"]["phase"] == phase)
                .count(),
            expected,
            "phase {phase}"
        );
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            // SAFETY: destroys this harness's live instance once, before Endpoint.
            unsafe { (self.api.destroy)(self.instance) };
            self.instance = std::ptr::null_mut();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
#[ignore = "long-running 130-session journal retention qualification"]
fn completed_sessions_roll_over_without_replaying_creation() {
    let mut harness = Harness::with_provider(true);
    for sequence in 0..130 {
        let prefix = format!("roll-{sequence}");
        let prepare = format!("{prefix}.prepare");
        let accepted = harness.call("transport", json!({"operation":"prepare","operation_id":prepare,"application_ref":{"kind":"application","app_id":"app"}}));
        assert_eq!(
            accepted["status"], "pending",
            "session {sequence}: {accepted}"
        );
        let ready = harness.settled("transport", &prepare);
        assert_eq!(ready["status"], "succeeded", "session {sequence}: {ready}");
        let target = ready["target"].clone();
        let bind = harness.call("transport", json!({"operation":"bind_application","operation_id":format!("{prefix}.bind"),"target":target,"launch_id":sequence + 1,"application_operation_id":format!("{prefix}.launch")}));
        assert_eq!(bind["status"], "succeeded", "session {sequence}: {bind}");
        let end_id = format!("{prefix}.end");
        let end = harness.call(
            "transport",
            json!({"operation":"end_session","operation_id":end_id,"target":target}),
        );
        assert_eq!(end["status"], "pending", "session {sequence}: {end}");
        let retire_id = format!("{prefix}.retire");
        let retire = harness.call(
            "transport",
            json!({"operation":"application_retired","operation_id":retire_id,"target":target}),
        );
        assert_eq!(retire["status"], "pending", "session {sequence}: {retire}");
        assert_eq!(
            harness.settled("transport", &retire_id)["status"],
            "succeeded"
        );
        assert_eq!(harness.settled("transport", &end_id)["status"], "succeeded");
    }
    let replay = harness.call("transport", json!({"operation":"prepare","operation_id":"roll-0.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(replay["reason"], "receipt_expired", "{replay}");
    let snapshot = harness.call("transport", json!({"operation":"snapshot"}));
    assert!(snapshot["sessions"].as_array().unwrap().len() < 128);
}

#[test]
fn registry_settings_are_frozen_per_session_and_stream_lifecycle() {
    let mut harness = Harness::with_registry(true, Some((1, json!({"mode":"session"}))));
    let prepared = harness.call("transport", json!({"operation":"prepare","operation_id":"snapshot.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let ready = harness.settled("transport", "snapshot.prepare");
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"snapshot.bind","target":session,"launch_id":42,"application_operation_id":"snapshot.launch"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    harness.registry_settings = Some((2, json!({"mode":"stream"})));
    let stream = harness.prepare_and_start_stream(&session, "snapshot.stream", false);
    harness.registry_settings = Some((3, json!({"mode":"later"})));
    harness.pause(&stream, "snapshot.stream");
    let end = harness.call(
        "transport",
        json!({"operation":"end_session","operation_id":"snapshot.end","target":session}),
    );
    assert_eq!(end["status"], "pending", "{end}");
    let retired = harness.call("transport", json!({"operation":"application_retired","operation_id":"snapshot.retire-app","target":session}));
    assert_eq!(retired["status"], "pending", "{retired}");
    assert_eq!(
        harness.settled("transport", "snapshot.retire-app")["status"],
        "succeeded"
    );
    assert_eq!(
        harness.settled("transport", "snapshot.end")["status"],
        "succeeded"
    );
    for (phase, revision, value) in [
        ("session.prepare", "registry-1", "session"),
        ("stream.prepare", "registry-2", "stream"),
        ("stream.release", "registry-2", "stream"),
        ("session.release", "registry-1", "session"),
    ] {
        let matching: Vec<_> = harness
            .provider_calls
            .iter()
            .filter(|call| {
                call["method"] == "lifecycle_callback" && call["arguments"]["phase"] == phase
            })
            .collect();
        assert_eq!(matching.len(), 1, "{phase}: {matching:?}");
        assert_eq!(
            matching[0]["arguments"]["settings_revision"], revision,
            "{phase}"
        );
        assert_eq!(
            matching[0]["arguments"]["settings"],
            json!({"mode":value}),
            "{phase}"
        );
    }
}
