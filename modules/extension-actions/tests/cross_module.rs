//! Production actions and lifecycle ABIs against a real broker; other peers are simulated.
//! Build both cdylibs first, set AMATERASU_ACTIONS_LIBRARY and
//! AMATERASU_LIFECYCLE_LIBRARY, then run this test with `-- --ignored`.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use libloading::Library;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::c_void,
    rc::Rc,
};

const ACTIONS: &str = "amaterasu.extension-actions";
const ACTION_SERVICE: &str = "amaterasu.extension-actions/1";
const STATE: &str = "amaterasu.state/1";
const REGISTRY: &str = "amaterasu.extension-registry/1";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const PROVIDER: &str = "example.provider/1";
const INPUT: &str = "amaterasu.input/1";
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Endpoint {
    broker: Rc<RefCell<Broker>>,
    principal: &'static str,
    lifecycle_calls: Rc<Cell<usize>>,
    lifecycle_replies: Rc<RefCell<Vec<Value>>>,
}
unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: abi::BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: endpoint is boxed for the complete module lifetime and used on this thread.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    // SAFETY: broker input is borrowed for the duration of this synchronous call.
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    if let Command::Complete { payload, .. } = &command
        && endpoint.principal == "lifecycle"
    {
        endpoint
            .lifecycle_replies
            .borrow_mut()
            .push(payload.clone());
    }
    if let Command::Request { service, .. } = &command
        && endpoint.principal == ACTIONS
        && service == LIFECYCLE
    {
        endpoint
            .lifecycle_calls
            .set(endpoint.lifecycle_calls.get() + 1);
    }
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the reply sink copies bytes synchronously before this function returns.
    unsafe { reply.send(&bytes) };
    abi::STATUS_OK
}

struct Harness {
    broker: Rc<RefCell<Broker>>,
    endpoint: Box<Endpoint>,
    api: abi::ModuleApi,
    actions_lib: Library,
    instance: *mut c_void,
    lifecycle_lib: Library,
    lifecycle_endpoint: Box<Endpoint>,
    lifecycle_api: abi::ModuleApi,
    lifecycle_instance: *mut c_void,
    lifecycle_calls: Rc<Cell<usize>>,
    lifecycle_replies: Rc<RefCell<Vec<Value>>>,
    journal_dir: std::path::PathBuf,
    state: BTreeMap<String, Value>,
    revision: u64,
    metadata: Value,
    sequence: u64,
    effects: usize,
    observed_delegated_registry: bool,
    observed_delegated_plan: bool,
    mutation_action: bool,
    expansive_plan: bool,
    hold_running_commit: bool,
    held_state: Option<(u64, Value)>,
    delivery: &'static str,
}
impl Harness {
    fn new() -> Self {
        Self::with_lifecycle_grant(true)
    }

    fn with_lifecycle_grant(allowed: bool) -> Self {
        let calls = if allowed {
            vec![STATE, REGISTRY, LIFECYCLE, PROVIDER, INPUT]
        } else {
            vec![STATE, REGISTRY, PROVIDER, INPUT]
        };
        let journal_dir = std::env::temp_dir().join(format!(
            "amaterasu-cross-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&journal_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&journal_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let schema = json!({"required":{},"optional":{},"allow_extra":true});
        let plan = json!({"service":PROVIDER,"method":"action_plan","interface_major":1,"interface_minor":0,"request":schema,"response":schema,"privilege":"user"});
        let execute = json!({"service":PROVIDER,"method":"action_execute","interface_major":1,"interface_minor":0,"request":schema,"response":schema,"privilege":"user"});
        let lifecycle_methods: Vec<Value> = ["resource_plan", "lifecycle_callback", "lifecycle_status", "binding_validate"].into_iter().map(|name| json!({"service":PROVIDER,"method":name,"interface_major":1,"interface_minor":0,"request":schema,"response":schema,"privilege":"user"})).collect();
        let method_ref = |name: &str| json!({"service":PROVIDER,"method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
        let settings = json!({"input_service":"input","grants":[{"principal":"management","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["read_only","session_mutation"]},{"principal":"input","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["read_only","session_mutation"]}]});
        let mut config = json!({"principals":{
            ACTIONS:{"provide":[ACTION_SERVICE],"call":calls},
            "management":{"call":[ACTION_SERVICE,REGISTRY,PROVIDER,LIFECYCLE]},
            "input":{"provide":[INPUT],"call":[ACTION_SERVICE,REGISTRY,PROVIDER,LIFECYCLE]},
            "capture":{"call":[LIFECYCLE]},
            "state":{"provide":[STATE]}, "registry":{"provide":[REGISTRY]},
            "lifecycle":{"provide":[LIFECYCLE],"call":[PROVIDER,INPUT]}, "provider":{"provide":[PROVIDER]}
        },"settings":{ACTIONS:settings, "lifecycle":{"journal_path":journal_dir.join("journal.json"),"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},"trusted_callers":[ACTIONS,"management","capture","input"],"session_delegates":["capture","input"],"consumers":{"capture":["capture"],"input":["input"]},"operation_grants":{"management":["prepare","lookup","prepare_stream","start_stream","issue_input_binding","bind_application"],"capture":["ack_binding","retire_stream","lookup"],"input":["ack_binding","lookup"],ACTIONS:["authorize_action","consume_action_permit","begin_action_transition","complete_action","consume_input_reservation"]},"action_service":ACTIONS,"participants":[{"provider":"provider","plan_method":method_ref("resource_plan"),"callback_method":method_ref("lifecycle_callback"),"status_method":method_ref("lifecycle_status"),"binding_validate_method":method_ref("binding_validate"),"settings_revision":"one","settings":{},"capabilities":[],"cleanup_methods":[]}]}},"api_methods":[
            {"provider":"provider","callers":[ACTIONS,"management","input"],"method":plan},
            {"provider":"provider","callers":[ACTIONS],"method":execute}
        ]});
        for method in &lifecycle_methods {
            config["api_methods"]
                .as_array_mut()
                .unwrap()
                .push(json!({"provider":"provider","callers":["lifecycle"],"method":method}));
        }
        let config = serde_json::from_value(config).unwrap();
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [("state", STATE), ("registry", REGISTRY), ("input", INPUT)] {
            assert!(matches!(
                broker.dispatch(
                    principal,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        for value in [plan, execute].into_iter().chain(lifecycle_methods) {
            assert!(matches!(
                broker.dispatch(
                    "provider",
                    Command::RegisterApi {
                        method: serde_json::from_value(value).unwrap()
                    }
                ),
                Response::Ok
            ));
        }
        let broker = Rc::new(RefCell::new(broker));
        let lifecycle_calls = Rc::new(Cell::new(0));
        let lifecycle_replies = Rc::new(RefCell::new(Vec::new()));
        let endpoint = Box::new(Endpoint {
            broker: broker.clone(),
            principal: ACTIONS,
            lifecycle_calls: lifecycle_calls.clone(),
            lifecycle_replies: lifecycle_replies.clone(),
        });
        let mut lifecycle_endpoint = Box::new(Endpoint {
            broker: broker.clone(),
            principal: "lifecycle",
            lifecycle_calls: lifecycle_calls.clone(),
            lifecycle_replies: lifecycle_replies.clone(),
        });
        let path = std::env::var_os("AMATERASU_LIFECYCLE_LIBRARY")
            .expect("set AMATERASU_LIFECYCLE_LIBRARY to the freshly built lifecycle cdylib");
        // SAFETY: the path is supplied by the test invocation and the library outlives its instance.
        let lifecycle_lib = unsafe { Library::new(path) }.unwrap();
        // SAFETY: this symbol is the documented module entry point of the built lifecycle cdylib.
        let lifecycle_api = unsafe {
            *lifecycle_lib
                .get::<unsafe extern "C" fn() -> *const abi::ModuleApi>(b"amaterasu_module_v1")
                .unwrap()()
        };
        let mut lifecycle_instance = std::ptr::null_mut();
        let lifecycle_host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *lifecycle_endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: the endpoint and module library remain live until the module is destroyed.
        assert_eq!(
            unsafe { (lifecycle_api.create)(&lifecycle_host, &mut lifecycle_instance) },
            abi::STATUS_OK
        );
        let actions_path = std::env::var_os("AMATERASU_ACTIONS_LIBRARY")
            .expect("set AMATERASU_ACTIONS_LIBRARY to the freshly built actions cdylib");
        // SAFETY: the test supplies the library path, and the library outlives its instance.
        let actions_lib = unsafe { Library::new(actions_path) }.unwrap();
        // SAFETY: this is the module's documented ABI entry point.
        let api = unsafe {
            *actions_lib
                .get::<unsafe extern "C" fn() -> *const abi::ModuleApi>(b"amaterasu_module_v1")
                .unwrap()()
        };
        let mut result = Self {
            broker,
            endpoint,
            api,
            actions_lib,
            instance: std::ptr::null_mut(),
            lifecycle_lib,
            lifecycle_endpoint,
            lifecycle_api,
            lifecycle_instance,
            lifecycle_calls,
            lifecycle_replies,
            journal_dir,
            state: BTreeMap::new(),
            revision: 0,
            metadata: Value::Null,
            sequence: 0,
            effects: 0,
            observed_delegated_registry: false,
            observed_delegated_plan: false,
            mutation_action: false,
            expansive_plan: false,
            hold_running_commit: false,
            held_state: None,
            delivery: "delivered",
        };
        let host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *result.endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: endpoint remains live until after the module instance is destroyed.
        assert_eq!(
            unsafe { (result.api.create)(&host, &mut result.instance) },
            abi::STATUS_OK
        );
        result
    }
    fn complete(&mut self, principal: &str, id: u64, payload: Value) {
        assert!(matches!(
            self.broker.borrow_mut().dispatch(
                principal,
                Command::Complete {
                    request: id,
                    payload
                }
            ),
            Response::Ok
        ));
    }
    fn pump(&mut self) {
        // SAFETY: instance is live and calls are serialized on the test thread.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
        // SAFETY: lifecycle_instance belongs to lifecycle_api and is used serially.
        assert_eq!(
            unsafe { (self.lifecycle_api.tick)(self.lifecycle_instance) },
            abi::STATUS_OK
        );
        for principal in ["state", "registry", "provider", "input"] {
            let request = self
                .broker
                .borrow_mut()
                .dispatch(principal, Command::Poll {});
            let Response::Request {
                id,
                caller,
                initiator,
                payload,
                ..
            } = request
            else {
                continue;
            };
            let response = match principal {
                "state" => match payload["action"].as_str().unwrap() {
                    "collection_get" => {
                        let value = payload["key"]
                            .as_str()
                            .and_then(|key| self.state.get(key))
                            .cloned();
                        json!({"ok":true,"revision":self.revision,"metadata":self.metadata,"count":self.state.len(),"value":value})
                    }
                    "collection_page" => {
                        let after = payload["after"].as_str();
                        let entries: Vec<_> = self
                            .state
                            .iter()
                            .filter(|(key, _)| after.is_none_or(|after| key.as_str() > after))
                            .take(16)
                            .map(|(key, value)| json!({"key":key,"value":value}))
                            .collect();
                        json!({"ok":true,"revision":self.revision,"entries":entries,"next":null})
                    }
                    "collection_commit" if payload["expected_revision"] == self.revision => {
                        for edit in payload["edits"].as_array().unwrap() {
                            let key = edit["key"].as_str().unwrap();
                            if edit["value"].is_null() {
                                self.state.remove(key);
                            } else {
                                self.state.insert(key.into(), edit["value"].clone());
                            }
                        }
                        self.revision += 1;
                        self.metadata = payload["metadata"].clone();
                        json!({"ok":true,"revision":self.revision})
                    }
                    "collection_commit" => json!({"ok":false,"error":{"code":"revision_conflict"}}),
                    _ => panic!("unexpected state operation"),
                },
                "registry" => {
                    self.observed_delegated_registry =
                        caller == ACTIONS && initiator == "management";
                    let plan = json!({"service":PROVIDER,"method":"action_plan","interface_major":1,"interface_minor":0,"schema_revision":1});
                    let execute = json!({"service":PROVIDER,"method":"action_execute","interface_major":1,"interface_minor":0,"schema_revision":1});
                    let execution = if self.mutation_action {
                        "session_mutation"
                    } else {
                        "read_only"
                    };
                    json!({"ok":true,"provider":"provider","revision":1,"action":{"id":"example/action","label":"Example","description":"Example","execution":execution,"arguments":{},"allowed_targets":["session"],"required_capabilities":[],"plan":plan,"execute":execute,"status":null}})
                }
                "input" => match payload["action"].as_str() {
                    Some("binding_status") => {
                        json!({"ok":true,"status":"bound","token":payload["token"],"target":payload["target"],"geometry_revision":1,"lease":"lease-1"})
                    }
                    Some("revoke_binding") => {
                        json!({"status":"retired","token":payload["token"],"target":payload["target"]})
                    }
                    Some("confirm_delivery") => {
                        json!({"ok":true,"status":if self.delivery == "forged" {"delivered"} else {self.delivery},"epoch":payload["epoch"],"sequence":payload["sequence"],"operation_id":payload["operation_id"],"reservation_id": if self.delivery == "forged" {json!("forged")} else {payload["reservation_id"].clone()},"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"batch_epoch":"batch","batch_sequence":1})
                    }
                    _ => panic!("unexpected input operation: {payload}"),
                },
                "provider" => {
                    let arguments = &payload["arguments"];
                    match payload["method"].as_str().unwrap() {
                        "action_plan" => {
                            self.observed_delegated_plan =
                                caller == ACTIONS && initiator == "management";
                            let required = if self.expansive_plan {
                                json!(["example/extra"])
                            } else {
                                json!([])
                            };
                            json!({"status":"planned","operation_id":arguments["operation_id"],"action_id":arguments["action_id"],"target":arguments["target"],"plan":{"plan_id":"p1","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":required,"constraints":{},"obligations":[],"affected_consumers":if self.mutation_action {json!(["capture"])} else {json!([])},"consumer_change":if self.mutation_action {"reconnect_required"} else {"live"},"candidate_constraint":null,"allowed_degradation":[]}})
                        }
                        "resource_plan" => {
                            let stream = arguments["phase"] == "stream.prepare";
                            let resources = if stream {
                                json!([{"provider":"provider","resource_id":"output","incarnation":null,"expected_revision":null,"execution_domain":"seat","mutation_domains":[],"access":"read"}])
                            } else {
                                json!([])
                            };
                            let constraint = if stream {
                                json!({"capture_provider":"capture","selector_schema":"example/1","resource_provider":"provider","resource_id":"output","incarnation":null,"execution_domain":"seat","mode":arguments["requested_video"],"plan_revision":1})
                            } else {
                                Value::Null
                            };
                            json!({"status":"planned","plan":{"plan_id":"plan","revision":1,"resources":resources,"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":constraint,"allowed_degradation":[]}})
                        }
                        "binding_validate" => {
                            json!({"status":"succeeded","target":arguments["target"],"binding":arguments["binding"],"observation":arguments["observation"],"plan_revision":arguments["plan_revision"]})
                        }
                        "lifecycle_callback" => {
                            if arguments["phase"] == "stream.prepare" {
                                json!({"status":"prepared","candidate":{"capture_provider":"capture","selector_schema":"example/1","selector":{},"resource":{"provider":"provider","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"mode":arguments["requested_video"],"plan_revision":1,"geometry_revision":1,"generation":arguments["target"]["generation"]},"obligations":[]})
                            } else if arguments["phase"] == "stream.release"
                                || arguments["phase"] == "session.release"
                            {
                                json!({"status":"cleanup","outcomes":[]})
                            } else {
                                json!({"status":"prepared","candidate":null,"obligations":[]})
                            }
                        }
                        "action_execute" => {
                            assert_eq!(caller, ACTIONS);
                            assert_eq!(initiator, ACTIONS);
                            let epoch = arguments["operation_id"]
                                .as_str()
                                .unwrap()
                                .split(':')
                                .next()
                                .unwrap();
                            let record = &self.state[&format!("epoch/{epoch}")]["receipts"]["1"];
                            assert_eq!(record["dispatch"]["stage"]["phase"], "running");
                            assert_eq!(record["receipt"]["status"], "running");
                            self.effects += 1;
                            json!({"status":"succeeded","receipt":{"effect":"confirmed"}})
                        }
                        _ => panic!("unexpected provider method"),
                    }
                }
                _ => unreachable!(),
            };
            if principal == "state"
                && self.hold_running_commit
                && payload["action"] == "collection_commit"
                && payload["edits"].as_array().is_some_and(|edits| {
                    edits.iter().any(|edit| {
                        edit["value"]["receipts"]["1"]["dispatch"]["stage"]["phase"] == "running"
                    })
                })
            {
                self.held_state = Some((id, response));
                self.hold_running_commit = false;
                continue;
            }
            self.complete(principal, id, response);
        }
    }
    fn call(&mut self, payload: Value) -> Value {
        self.call_as("management", payload)
    }
    fn call_as(&mut self, principal: &str, payload: Value) -> Value {
        self.call_service(principal, ACTION_SERVICE, payload)
    }

    fn call_service(&mut self, principal: &str, service: &str, payload: Value) -> Value {
        self.sequence += 1;
        let response = self.broker.borrow_mut().dispatch(
            principal,
            Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 5_000,
                operation_id: format!("request.{}", self.sequence),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("request rejected: {response:?}")
        };
        for _ in 0..500 {
            self.pump();
            let result = self
                .broker
                .borrow_mut()
                .dispatch(principal, Command::TakeResult { request: id });
            match result {
                Response::Pending => (),
                Response::Result { payload } => {
                    let _ = self
                        .broker
                        .borrow_mut()
                        .dispatch(principal, Command::ReleaseResult { request: id });
                    return payload;
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        panic!("request stalled");
    }
    fn submit_action(&mut self) -> (String, Value) {
        for _ in 0..12 {
            self.pump();
        }
        let open = self.call(json!({"operation":"open_epoch","operation_id":"open.1"}));
        let epoch = open["epoch"].as_str().unwrap().to_owned();
        let receipt=self.call(json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
        (epoch, receipt)
    }
    fn invoke_action(&mut self) -> String {
        let (epoch, receipt) = self.submit_action();
        assert_eq!(receipt["status"], "queued");
        epoch
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            // SAFETY: one destroy for the live instance, before its endpoint is dropped.
            unsafe { (self.api.destroy)(self.instance) };
            self.instance = std::ptr::null_mut();
        }
        // SAFETY: destroy lifecycle before dropping its endpoint and library.
        unsafe { (self.lifecycle_api.destroy)(self.lifecycle_instance) };
        let _ = (
            &self.actions_lib,
            &self.lifecycle_lib,
            &self.lifecycle_endpoint,
        );
        let _ = std::fs::remove_dir_all(&self.journal_dir);
    }
}

#[test]
#[ignore = "requires freshly built actions and lifecycle cdylibs; see module documentation"]
fn real_lifecycle_denies_action_for_unknown_target() {
    let mut harness = Harness::new();
    let epoch = harness.invoke_action();
    for _ in 0..500 {
        harness.pump();
        if harness.lifecycle_calls.get() > 0 {
            break;
        }
    }
    assert!(
        harness.lifecycle_calls.get() > 0,
        "action must reach production lifecycle"
    );
    for _ in 0..100 {
        harness.pump();
    }
    assert!(
        harness
            .lifecycle_replies
            .borrow()
            .iter()
            .any(|reply| reply["reason"] == "target_not_active"),
        "actual lifecycle must reject unknown target: {:?}",
        harness.lifecycle_replies.borrow()
    );
    let phase =
        &harness.state[&format!("epoch/{epoch}")]["receipts"]["1"]["dispatch"]["stage"]["phase"];
    assert_eq!(
        phase, "recovery_required",
        "real lifecycle rejection must be processed"
    );
    assert_eq!(
        harness.effects, 0,
        "unknown session must never reach provider effect"
    );
}

#[test]
#[ignore = "requires freshly built actions and lifecycle cdylibs; see module documentation"]
fn real_lifecycle_authorizes_a_prepared_session_read() {
    let mut harness = Harness::new();
    let prepared = harness.call_service("management", LIFECYCLE, json!({"operation":"prepare","operation_id":"prepare.session","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let mut ready = Value::Null;
    for _ in 0..100 {
        let response = harness.call_service(
            "management",
            LIFECYCLE,
            json!({"operation":"lookup","operation_id":"prepare.session"}),
        );
        if response["status"] != "pending" {
            ready = response;
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let target = &ready["target"];
    let action_target = json!({"kind":"session","host_epoch":target["host_epoch"],"session_id":target["session_id"],"expected_generation":target["generation"]});
    let opened = harness.call(json!({"operation":"open_epoch","operation_id":"open.1"}));
    let epoch = opened["epoch"].as_str().unwrap();
    let receipt = harness.call(json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":action_target,"arguments":{},"expected_settings_revision":null}));
    assert_eq!(receipt["status"], "queued", "{receipt}");
    for _ in 0..500 {
        harness.pump();
        if harness.effects > 0 {
            break;
        }
    }
    assert_eq!(
        harness.effects,
        1,
        "lifecycle replies: {:?}",
        harness.lifecycle_replies.borrow()
    );
}

#[test]
#[ignore = "requires freshly built actions and lifecycle cdylibs; see module documentation"]
fn real_lifecycle_fences_mutation_until_capture_retires() {
    let mut harness = Harness::new();
    let prepared = harness.call_service("management", LIFECYCLE, json!({"operation":"prepare","operation_id":"prepare.session","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let ready = settle(&mut harness, "management", "prepare.session");
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call_service("management", LIFECYCLE, json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let started = harness.call_service("management", LIFECYCLE, json!({"operation":"prepare_stream","operation_id":"stream.prepare","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let ready = settle(&mut harness, "management", "stream.prepare");
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let stream = ready["target"].clone();
    let candidate = ready["candidate"].clone();
    let observation = json!({"version":1,"protocol":"wayland","compositor_id":"seat","registry_global":1,"output_name":"output","width":1920,"height":1080,"refresh_millihz":60000});
    let ack = harness.call_service("capture", LIFECYCLE, json!({"operation":"ack_binding","operation_id":"capture.ack","target":stream,"consumer":"capture","plan_revision":1,"candidate":candidate,"observed_capture":observation}));
    assert_eq!(ack["status"], "pending", "{ack}");
    let ack = settle(&mut harness, "capture", "capture.ack");
    assert_eq!(ack["status"], "succeeded", "{ack}");
    let binding = harness.call_service(
        "management",
        LIFECYCLE,
        json!({"operation":"issue_input_binding","operation_id":"input.token","target":stream}),
    );
    assert_eq!(binding["status"], "succeeded", "{binding}");
    let ack = harness.call_service("input", LIFECYCLE, json!({"operation":"ack_binding","operation_id":"input.ack","target":stream,"consumer":"input","plan_revision":1,"candidate":candidate}));
    assert_eq!(ack["status"], "pending", "{ack}");
    let ack = settle(&mut harness, "input", "input.ack");
    assert_eq!(ack["status"], "succeeded", "{ack}");
    let active = harness.call_service(
        "management",
        LIFECYCLE,
        json!({"operation":"start_stream","operation_id":"stream.start","target":stream}),
    );
    assert_eq!(active["status"], "succeeded", "{active}");
    harness.mutation_action = true;
    let action_target = json!({"kind":"session","host_epoch":session["host_epoch"],"session_id":session["session_id"],"expected_generation":session["generation"]});
    let opened = harness.call(json!({"operation":"open_epoch","operation_id":"open.1"}));
    let epoch = opened["epoch"].as_str().unwrap();
    let receipt = harness.call(json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":action_target,"arguments":{},"expected_settings_revision":null}));
    assert_eq!(receipt["status"], "queued", "{receipt}");
    for _ in 0..500 {
        harness.pump();
        if harness
            .lifecycle_replies
            .borrow()
            .iter()
            .any(|reply| reply["status"] == "pending" && reply["plan_digest"].is_string())
        {
            break;
        }
    }
    assert!(
        harness
            .lifecycle_replies
            .borrow()
            .iter()
            .any(|reply| reply["status"] == "pending" && reply["plan_digest"].is_string()),
        "transition never became pending: {:?}",
        harness.lifecycle_replies.borrow()
    );
    assert_eq!(harness.effects, 0);
    let retired = harness.call_service("capture", LIFECYCLE, json!({"operation":"retire_stream","operation_id":"capture.retire","target":stream,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    for _ in 0..1000 {
        harness.pump();
        if harness.effects > 0 {
            break;
        }
    }
    assert_eq!(
        harness.effects,
        1,
        "lifecycle replies: {:?}",
        harness.lifecycle_replies.borrow()
    );
}

fn settle(harness: &mut Harness, principal: &str, operation_id: &str) -> Value {
    for _ in 0..120 {
        let result = harness.call_service(
            principal,
            LIFECYCLE,
            json!({"operation":"lookup","operation_id":operation_id}),
        );
        if result["status"] != "pending" {
            return result;
        }
    }
    panic!("operation {operation_id} did not settle")
}
