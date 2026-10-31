//! Cross-module tests run the production lifecycle ABI against the actual broker.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
    provider_invalid: bool,
    simulate_stage_refresh: bool,
    stage_refresh_request: Option<u64>,
    stage_refresh_done: bool,
    refresh_round: u64,
    deferred_finalize_plan: Option<(u64, Value, Value)>,
    verify_finalize: bool,
    finalize_verify_request: Option<(u64, u64, Value)>,
    finalize_pending: bool,
    pending_finalize_result: Option<Value>,
    cleanup_deferred: bool,
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
                "example.display/2",
                SERVICE,
                "amaterasu.input/1",
                "amaterasu.launch/1"
            ]);
            configuration["principals"]["display"] =
                json!({"provide":["example.display/2"],"call":[SERVICE]});
            configuration["settings"][COORDINATOR]["trusted_callers"] =
                json!(["transport", "capture", "amaterasu.input", "display"]);
            configuration["settings"][COORDINATOR]["operation_grants"]["display"] = json!([
                "verify_provider_epoch",
                "refresh_provider_observations",
                "verify_resource_finalize"
            ]);
            let method_ref = |name: &str| json!({"service":"example.display/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
            configuration["settings"][COORDINATOR]["consumer_requirements"] = json!({"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]});
            configuration["settings"][COORDINATOR]["participants"] = json!([{"provider":"display","contract_version":2,"plan_method":method_ref("resource_plan"),"callback_method":method_ref("lifecycle_callback"),"status_method":method_ref("lifecycle_status"),"binding_validate_method":method_ref("binding_validate"),"settings_revision":"one","settings":{},"capabilities":[],"cleanup_methods":[method_ref("lifecycle_callback")]}]);
            let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
            let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
            configuration["settings"][COORDINATOR]["schema_documents"] =
                json!([{"reference":schema,"document":document}]);
            configuration["settings"][COORDINATOR]["finalize_policies"] = json!([{"owner_provider":"display","owner_service":"example.display/2","method":method_ref("finalize"),"status_method":method_ref("lifecycle_status"),"output_capability":"display/output","output_schema":schema,"capture_provider":"capture","selector_schema":"example/1","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]}]);
            for name in [
                "resource_plan",
                "lifecycle_callback",
                "lifecycle_status",
                "binding_validate",
                "finalize",
            ] {
                let method = json!({"service":"example.display/2","method":name,"interface_major":2,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"});
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
            provider_invalid: false,
            simulate_stage_refresh: false,
            stage_refresh_request: None,
            stage_refresh_done: false,
            refresh_round: 0,
            deferred_finalize_plan: None,
            verify_finalize: false,
            finalize_verify_request: None,
            finalize_pending: false,
            pending_finalize_result: None,
            cleanup_deferred: false,
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
                    let stream =
                        args["phase"] == "stream.prepare" || args["phase"] == "stream.release";
                    let release = args["phase"] == "stream.release";
                    let finalize = args["operation"] == "resource_finalize_plan";
                    let resource = json!({"provider":"display","resource_id":"output","incarnation":if stream {json!("native")} else {Value::Null},"expected_revision":if stream {json!(1)} else {Value::Null},"execution_domain":"seat","mutation_domains":["topology"],"access":if finalize || release {"exclusive"} else {"read"}});
                    let constraint = if finalize {
                        json!({"capture_provider":"capture","selector_schema":"example/1","resource_provider":"display","resource_id":"output","incarnation":"native","execution_domain":"seat","mode":args["requested_video"],"plan_revision":1,"geometry_revision":1})
                    } else {
                        Value::Null
                    };
                    let obligations = if finalize || release {
                        json!([{"obligation_id":"finalize-cleanup","provider":"display","resource_id":"output","mutation_domain":"topology","cleanup_method":{"service":"example.display/2","method":"lifecycle_callback","interface_major":2,"interface_minor":0,"schema_revision":1},"payload":{},"depends_on":[]}])
                    } else {
                        json!([])
                    };
                    let plan = json!({"plan_id":if finalize {"finalize-plan"} else {"session-plan"},"revision":1,"resources":[resource],"mutation_domains":[{"domain":"topology","expected_revision":1,"borrowers":{},"access":if finalize || release {"exclusive"} else {"read"}}],"required_capabilities":[],"constraints":{},"obligations":obligations,"affected_consumers":[],"consumer_change":"live","candidate_constraint":constraint,"allowed_degradation":[]});
                    if stream && self.simulate_stage_refresh {
                        self.stage_refresh_done = false;
                        self.stage_refresh_request = None;
                        self.refresh_round += 1;
                        self.deferred_finalize_plan = Some((id, plan, args["target"].clone()));
                    } else {
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
                    }
                } else if payload["method"] == "lifecycle_callback"
                    && args["phase"] == "stream.release"
                {
                    let outcomes: Vec<Value> = args["approved_plan"]["obligations"].as_array().unwrap().iter().map(|obligation| json!({"obligation_id":obligation["obligation_id"],"state":if self.cleanup_deferred {"deferred_restoration"} else {"released"},"reason":null})).collect();
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: id,
                                payload: json!({"status":"cleanup","outcomes":outcomes})
                            }
                        ),
                        Response::Ok
                    ));
                } else if payload["method"] == "lifecycle_callback" {
                    let document = r#"{"$id":"output/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
                    let schema = json!({"id":"output/1","major":1,"minor":0,"digest":format!("{:x}",Sha256::digest(document.as_bytes()))});
                    let origin = if args["phase"] == "session.prepare" {
                        args["target"].clone()
                    } else {
                        self.provider_calls
                            .iter()
                            .find(|call| {
                                call["method"] == "lifecycle_callback"
                                    && call["arguments"]["phase"] == "session.prepare"
                            })
                            .unwrap()["arguments"]["target"]
                            .clone()
                    };
                    let output = json!({"output_id":"screen-output","capability":"display/output","schema":schema,"producer":"display","producer_service":"example.display/2","origin":origin,"resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]},"domain_observations":[{"domain":"topology","revision":1,"borrowers":{},"complete":true}],"data":if self.provider_invalid {json!("invalid")} else {json!({"ready":true})}});
                    assert!(matches!(self.broker.borrow_mut().dispatch("display",Command::Complete {request:id,payload:json!({"status":"prepared","candidate":null,"obligations":[],"outputs":[output]})}),Response::Ok));
                } else if payload["method"] == "lifecycle_status" {
                    assert_eq!(args["receipt_id"], "finalize-receipt");
                    let result = self
                        .pending_finalize_result
                        .take()
                        .expect("pending finalized result");
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
                } else if payload["method"] == "finalize" {
                    let candidate = json!({"capture_provider":"capture","selector_schema":"example/1","selector":{},"resource":args["output"]["resource"],"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":args["target"]["generation"]});
                    let result =
                        json!({"status":"applied","output":args["output"],"candidate":candidate});
                    if self.verify_finalize {
                        let payload =
                            json!({"operation":"verify_resource_finalize","request":args});
                        let response = self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Request {
                                service: SERVICE.into(),
                                payload,
                                timeout_ms: 5000,
                                operation_id: "verify-finalize-request".into(),
                                parent: Some(id),
                            },
                        );
                        let Response::Accepted { id: verify_id } = response else {
                            panic!("verify request rejected: {response:?}");
                        };
                        self.finalize_verify_request = Some((verify_id, id, result));
                    } else {
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
                    }
                } else {
                    panic!("unexpected provider method: {payload}");
                }
            }
        }
        if self.simulate_stage_refresh && !self.stage_refresh_done {
            if let Some(request) = self.stage_refresh_request {
                let response = self
                    .broker
                    .borrow_mut()
                    .dispatch("display", Command::TakeResult { request });
                match response {
                    Response::Pending => {}
                    Response::Result { payload } => {
                        assert_eq!(payload["status"], "succeeded", "{payload}");
                        assert!(matches!(
                            self.broker
                                .borrow_mut()
                                .dispatch("display", Command::ReleaseResult { request }),
                            Response::Ok
                        ));
                        self.stage_refresh_done = true;
                        let (owner_request, plan, _) = self
                            .deferred_finalize_plan
                            .take()
                            .expect("pending finalize plan");
                        assert!(matches!(
                            self.broker.borrow_mut().dispatch(
                                "display",
                                Command::Complete {
                                    request: owner_request,
                                    payload: json!({"status":"planned","plan":plan})
                                }
                            ),
                            Response::Ok
                        ));
                    }
                    other => panic!("stage refresh failed: {other:?}"),
                }
            } else if let Some((owner_request, _, target)) = &self.deferred_finalize_plan {
                let payload = json!({"operation":"refresh_provider_observations","operation_id":format!("stage-observe-{}",self.refresh_round),"target":target,"domains":[{"domain":"topology","revision":1,"borrowers":{}}],"resources":[{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domains":["topology"]}]});
                let response = self.broker.borrow_mut().dispatch(
                    "display",
                    Command::Request {
                        service: SERVICE.into(),
                        payload,
                        timeout_ms: 5000,
                        operation_id: format!("stage-refresh-request-{}", self.refresh_round),
                        parent: Some(*owner_request),
                    },
                );
                let Response::Accepted { id } = response else {
                    panic!("stage observation rejected: {response:?}");
                };
                self.stage_refresh_request = Some(id);
            }
        }
        if let Some((verify_id, owner_id, result)) = &self.finalize_verify_request {
            let response = self.broker.borrow_mut().dispatch(
                "display",
                Command::TakeResult {
                    request: *verify_id,
                },
            );
            match response {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert_eq!(payload["status"], "execute", "{payload}");
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::ReleaseResult {
                                request: *verify_id
                            }
                        ),
                        Response::Ok
                    ));
                    let owner_payload = if self.finalize_pending {
                        self.pending_finalize_result = Some(result.clone());
                        json!({"status":"pending","receipt_id":"finalize-receipt"})
                    } else {
                        result.clone()
                    };
                    assert!(matches!(
                        self.broker.borrow_mut().dispatch(
                            "display",
                            Command::Complete {
                                request: *owner_id,
                                payload: owner_payload
                            }
                        ),
                        Response::Ok
                    ));
                    self.finalize_verify_request = None;
                }
                other => panic!("finalize verify failed: {other:?}"),
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

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            // SAFETY: the module instance is live and destroyed once.
            unsafe { (self.api.destroy)(self.instance) };
            self.instance = std::ptr::null_mut();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn real_broker_persists_v2_session_output() {
    let mut harness = Harness::with_provider(true);
    let started = harness.call("transport", json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..120 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        journal["composition"]["outputs"].as_array().unwrap().len(),
        1
    );
    let stored = &journal["composition"]["outputs"][0];
    assert_eq!(stored["output"]["output_id"], "screen-output");
    assert_eq!(
        stored["callback_proof"]["terminal"]["state"]["state"],
        "completed"
    );
    let prior_calls = harness.provider_calls.len();
    // SAFETY: the live instance is destroyed once before creating a replacement.
    unsafe { (harness.api.destroy)(harness.instance) };
    harness.instance = std::ptr::null_mut();
    harness.start();
    let recovered = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"session.prepare"}),
    );
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    let recovered_journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        recovered_journal["composition"]["outputs"][0]["callback_proof"]["terminal"]["state"]["state"],
        "completed"
    );
    assert_eq!(
        recovered_journal["composition"]["outputs"][0]["state"],
        "recovery_required"
    );
    assert_eq!(harness.provider_calls.len(), prior_calls);
}

#[test]
fn real_broker_rejects_malformed_v2_output() {
    let mut harness = Harness::with_provider(true);
    harness.provider_invalid = true;
    let started = harness.call("transport", json!({"operation":"prepare","operation_id":"invalid.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..120 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"invalid.prepare"}),
        );
        if result["status"] != "pending" {
            break;
        }
    }
    assert_ne!(result["status"], "succeeded", "{result}");
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert!(
        journal["composition"]["outputs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn real_broker_rejects_unverified_finalize_effect() {
    let mut harness = Harness::with_provider(true);
    harness.simulate_stage_refresh = true;
    let started = harness.call("transport",json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut ready = Value::Null;
    for _ in 0..120 {
        ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let started = harness.call("transport",json!({"operation":"prepare_stream","operation_id":"stream.prepare","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..200 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"stream.prepare"}),
        );
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "recovery_required", "{result}");
    assert_eq!(result["reason"], "finalize permit not consumed");
    assert!(result.get("candidate").is_none());
}

#[test]
fn real_broker_stages_and_finalizes_with_verified_owner() {
    let mut harness = Harness::with_provider(true);
    harness.simulate_stage_refresh = true;
    harness.verify_finalize = true;
    let started = harness.call("transport",json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut ready = Value::Null;
    for _ in 0..120 {
        ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let started = harness.call("transport",json!({"operation":"prepare_stream","operation_id":"stream.prepare","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..200 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"stream.prepare"}),
        );
        if result.get("reason").is_some() {
            eprintln!("stream progress: {result}");
        }
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["candidate"]["geometry_revision"], 1);
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        journal["finalize_results"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let record = &journal["finalize_results"]["records"][0];
    assert_eq!(record["selection"]["stage"]["target"], result["target"]);
    assert_eq!(record["plan_terminal"]["state"]["state"], "completed");
    assert_eq!(record["terminal"]["state"]["state"], "completed");
    let permit = record["request"]["permit_id"].as_str().unwrap();
    assert_eq!(
        journal["authority"]["finalize_permits"][permit]["settled"],
        true
    );
    let calls = harness.provider_calls.len();
    // SAFETY: the current instance is live and replaced only after destruction.
    unsafe { (harness.api.destroy)(harness.instance) };
    harness.instance = std::ptr::null_mut();
    harness.start();
    let recovered = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"stream.prepare"}),
    );
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    assert_eq!(harness.provider_calls.len(), calls);
    let recovered_journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        recovered_journal["composition"]["outputs"][0]["state"],
        "recovery_required"
    );
    assert_eq!(
        recovered_journal["finalize_results"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn real_broker_stream_release_preserves_session_root() {
    let mut harness = Harness::with_provider(true);
    harness.simulate_stage_refresh = true;
    harness.verify_finalize = true;
    let started = harness.call("transport",json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut ready = Value::Null;
    for _ in 0..120 {
        ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let started = harness.call("transport",json!({"operation":"prepare_stream","operation_id":"stream.prepare","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..200 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"stream.prepare"}),
        );
        if result.get("reason").is_some() {
            eprintln!("stream progress: {result}");
        }
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["candidate"]["geometry_revision"], 1);
    let stream = result["target"].clone();
    let pause = harness.call(
        "transport",
        json!({"operation":"pause_stream","operation_id":"pause","target":stream}),
    );
    assert_eq!(pause["status"], "pending", "{pause}");
    let retirement = harness.call("capture",json!({"operation":"retire_stream","operation_id":"capture.retire","target":stream,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retirement["status"], "pending", "{retirement}");
    let mut released = Value::Null;
    for _ in 0..200 {
        released = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"pause"}),
        );
        if released["status"] != "pending" {
            break;
        }
    }
    assert_eq!(released["status"], "succeeded", "{released}");
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(journal["composition"]["outputs"][0]["state"], "retained");
    assert_eq!(
        journal["stream_releases"]["proofs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let calls = harness.provider_calls.len();
    // SAFETY: destroy the live module once before creating its replacement.
    unsafe { (harness.api.destroy)(harness.instance) };
    harness.instance = std::ptr::null_mut();
    harness.start();
    let recovered = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"pause"}),
    );
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    assert_eq!(harness.provider_calls.len(), calls);
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        journal["stream_releases"]["proofs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn real_broker_recovers_pending_finalize_through_status() {
    let mut harness = Harness::with_provider(true);
    harness.simulate_stage_refresh = true;
    harness.verify_finalize = true;
    harness.finalize_pending = true;
    let started = harness.call("transport",json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut ready = Value::Null;
    for _ in 0..120 {
        ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let started = harness.call("transport",json!({"operation":"prepare_stream","operation_id":"stream.prepare","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..200 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"stream.prepare"}),
        );
        if result.get("reason").is_some() {
            eprintln!("stream progress: {result}");
        }
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["candidate"]["geometry_revision"], 1);
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        journal["finalize_results"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let record = &journal["finalize_results"]["records"][0];
    assert_eq!(record["selection"]["stage"]["target"], result["target"]);
    assert_eq!(record["plan_terminal"]["state"]["state"], "completed");
    assert_eq!(record["terminal"]["state"]["state"], "completed");
    assert_eq!(record["terminal"]["call"]["method"], "lifecycle_status");
    assert_eq!(record["pending_receipt"], "finalize-receipt");
    assert_eq!(
        record["pending_proof"]["state"]["payload"]["receipt_id"],
        "finalize-receipt"
    );
    let permit = record["request"]["permit_id"].as_str().unwrap();
    assert_eq!(
        journal["authority"]["finalize_permits"][permit]["settled"],
        true
    );
    let calls = harness.provider_calls.len();
    // SAFETY: the current instance is live and replaced only after destruction.
    unsafe { (harness.api.destroy)(harness.instance) };
    harness.instance = std::ptr::null_mut();
    harness.start();
    let recovered = harness.call(
        "transport",
        json!({"operation":"lookup","operation_id":"stream.prepare"}),
    );
    assert_eq!(recovered["status"], "succeeded", "{recovered}");
    assert_eq!(harness.provider_calls.len(), calls);
    let recovered_journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(
        recovered_journal["composition"]["outputs"][0]["state"],
        "recovery_required"
    );
    assert_eq!(
        recovered_journal["finalize_results"]["records"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn real_broker_deferred_cleanup_keeps_root_and_no_release_proof() {
    let mut harness = Harness::with_provider(true);
    harness.simulate_stage_refresh = true;
    harness.verify_finalize = true;
    harness.cleanup_deferred = true;
    let started = harness.call("transport",json!({"operation":"prepare","operation_id":"session.prepare","application_ref":{"kind":"application","app_id":"app"}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut ready = Value::Null;
    for _ in 0..120 {
        ready = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session.prepare"}),
        );
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let session = ready["target"].clone();
    let bound = harness.call("transport", json!({"operation":"bind_application","operation_id":"app.bind","target":session,"launch_id":42,"application_operation_id":"launch-42"}));
    assert_eq!(bound["status"], "succeeded", "{bound}");
    let started = harness.call("transport",json!({"operation":"prepare_stream","operation_id":"stream.prepare","target":session,"intent":"initial","requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}}));
    assert_eq!(started["status"], "pending", "{started}");
    let mut result = Value::Null;
    for _ in 0..200 {
        result = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"stream.prepare"}),
        );
        if result.get("reason").is_some() {
            eprintln!("stream progress: {result}");
        }
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["candidate"]["geometry_revision"], 1);
    let stream = result["target"].clone();
    let pause = harness.call(
        "transport",
        json!({"operation":"pause_stream","operation_id":"pause","target":stream}),
    );
    assert_eq!(pause["status"], "pending", "{pause}");
    let retirement = harness.call("capture",json!({"operation":"retire_stream","operation_id":"capture.retire","target":stream,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retirement["status"], "pending", "{retirement}");
    let mut released = Value::Null;
    for _ in 0..200 {
        released = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"pause"}),
        );
        if released["status"] != "pending" {
            break;
        }
    }
    assert_eq!(released["status"], "recovery_required", "{released}");
    let journal: Value =
        serde_json::from_slice(&std::fs::read(harness.root.join("journal.json")).unwrap()).unwrap();
    assert_eq!(journal["composition"]["outputs"][0]["state"], "retained");
    assert!(
        journal["stream_releases"]["proofs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
