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
    pending_callbacks: Vec<(u64, Value)>,
    provider: bool,
}

impl Harness {
    fn new() -> Self {
        Self::with_provider(false)
    }

    fn with_provider(provider: bool) -> Self {
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
                "other-transport": {"call": [SERVICE]},
                "untrusted": {"call": [SERVICE]}
            },
            "settings": {
                COORDINATOR: {
                    "journal_path": root.join("journal.json"),
                    "consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},
                    "trusted_callers": ["transport", "other-transport"],
                    "operation_grants": {
                        "transport": ["prepare", "lookup", "snapshot", "describe_resource_control", "verify_resource_control", "validate_resource_control", "verify_control_settlement", "describe_resource_finalize", "verify_resource_finalize", "validate_resource_finalize"],
                        "other-transport": ["prepare", "lookup", "snapshot", "describe_resource_control", "verify_resource_control", "validate_resource_control", "verify_control_settlement", "describe_resource_finalize", "verify_resource_finalize", "validate_resource_finalize"]
                    }
                }
            }
        });
        let mut methods = Vec::new();
        if provider {
            configuration["principals"][COORDINATOR]["call"] = json!(["example.display/1"]);
            configuration["principals"]["display"] =
                json!({"provide":["example.display/1"],"call":[SERVICE]});
            configuration["settings"][COORDINATOR]["trusted_callers"] =
                json!(["transport", "other-transport", "display"]);
            configuration["settings"][COORDINATOR]["operation_grants"]["display"] =
                json!(["verify_provider_epoch"]);
            let method_ref = |name: &str| json!({"service":"example.display/1","method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
            configuration["settings"][COORDINATOR]["consumer_requirements"] = json!({"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]});
            configuration["settings"][COORDINATOR]["participants"] = json!([{"provider":"display","plan_method":method_ref("resource_plan"),"callback_method":method_ref("lifecycle_callback"),"status_method":method_ref("lifecycle_status"),"settings_revision":"one","settings":{},"capabilities":[],"cleanup_methods":[]}]);
            for name in ["resource_plan", "lifecycle_callback", "lifecycle_status"] {
                let method = json!({"service":"example.display/1","method":name,"interface_major":1,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":true},"response":{"required":{},"optional":{},"allow_extra":true},"privilege":"user"});
                methods.push(method);
            }
            configuration["api_methods"]=json!(methods.iter().map(|method|json!({"provider":"display","callers":[COORDINATOR],"method":method})).collect::<Vec<_>>());
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
            pending_callbacks: Vec::new(),
            provider,
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
                if payload["method"] == "resource_plan" {
                    let plan = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
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
                } else {
                    self.pending_callbacks.push((id, payload));
                }
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
            // SAFETY: destroys this harness's live instance once, before Endpoint.
            unsafe { (self.api.destroy)(self.instance) };
            self.instance = std::ptr::null_mut();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn control_routes_reject_unadmitted_and_foreign_callers_through_broker() {
    let mut harness = Harness::new();
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
    let method = json!({"service":"transport/2","method":"resource_control","interface_major":2,"interface_minor":0,"schema_revision":1});
    let resource = json!({"provider":"transport","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]});
    let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
    let request = json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"transport","owner_service":"transport/2","target":target,"resource":resource,"method":method,"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":[]},"admission_id":"missing","permit_id":"missing","remaining_ms":5000});
    for operation in [
        "describe_resource_control",
        "verify_resource_control",
        "validate_resource_control",
    ] {
        let payload = json!({"operation":operation,"request":request});
        for caller in ["transport", "other-transport"] {
            let response = harness.call(caller, payload.clone());
            assert_eq!(response["status"], "rejected");
            assert_eq!(response["reason"], "resource_control_not_authorized");
        }
    }
    let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"transport","producer_service":"transport/2","origin":target,"resource":resource,"domain_observations":[],"data":{}});
    let settlement = json!({"operation":"verify_control_settlement","request":request,"output":output,"settlement_id":"settlement"});
    for caller in ["transport", "other-transport"] {
        let response = harness.call(caller, settlement.clone());
        assert_eq!(response["status"], "rejected");
        assert_eq!(response["reason"], "resource_control_not_authorized");
    }
    let finalize = json!({"version":2,"transition_id":"transition","operation_id":"finalize","provider_sequence":2,"target":target,"output":output,"control_results":[],"admission_id":"missing","permit_id":"missing","remaining_ms":5000});
    for operation in [
        "describe_resource_finalize",
        "verify_resource_finalize",
        "validate_resource_finalize",
    ] {
        for caller in ["transport", "other-transport"] {
            let response = harness.call(caller, json!({"operation":operation,"request":finalize}));
            assert_eq!(response["status"], "rejected");
            assert_eq!(response["reason"], "resource_finalize_not_authorized");
        }
    }
}

#[test]
fn real_broker_identity_controls_admission_and_receipt_visibility() {
    let mut harness = Harness::new();
    let forged = harness.call(
        "untrusted",
        json!({"operation":"prepare", "operation_id":"one", "caller":"transport"}),
    );
    assert_eq!(forged["code"], "permission_denied");
    let admission = harness.call(
        "transport",
        json!({"operation":"prepare", "operation_id":"one"}),
    );
    assert_eq!(admission["status"], "pending");
    let looked_up = harness.call(
        "transport",
        json!({"operation":"lookup", "operation_id":"one"}),
    );
    assert_eq!(looked_up["session_id"], admission["session_id"]);
    assert_eq!(looked_up["status"], "succeeded");
    assert_eq!(
        harness.call(
            "other-transport",
            json!({"operation":"lookup", "operation_id":"one"})
        )["status"],
        "not_found"
    );
}

#[test]
fn preparation_waits_for_provider_and_verifies_its_exact_durable_callback() {
    let mut harness = Harness::with_provider(true);
    let admitted = harness.call(
        "transport",
        json!({"operation":"prepare","operation_id":"session-one"}),
    );
    assert_eq!(admitted["status"], "pending");
    for _ in 0..64 {
        if !harness.pending_callbacks.is_empty() {
            break;
        }
        harness.tick();
    }
    assert_eq!(harness.pending_callbacks.len(), 1);
    let (request, payload) = harness.pending_callbacks[0].clone();
    let callback: amaterasu_module_sdk::extension_api::ProviderRequest =
        serde_json::from_value(payload["arguments"].clone()).unwrap();
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&callback).unwrap())
    );
    let target = callback.target.clone();
    let epoch = admitted["host_epoch"].clone();
    let verification = json!({"operation":"verify_provider_epoch","host_epoch":epoch,"provider_sequence":callback.provider_sequence,"operation_id":callback.operation_id,"transition_id":callback.transition_id,"target":target,"fingerprint":digest});
    assert_eq!(
        harness.call("display", verification.clone())["status"],
        "succeeded"
    );
    let mut forged = verification;
    forged["fingerprint"] = json!("0".repeat(64));
    assert_eq!(harness.call("display", forged)["status"], "rejected");
    assert!(matches!(
        harness.broker.borrow_mut().dispatch(
            "display",
            Command::Complete {
                request,
                payload: json!({"status":"prepared","candidate":null,"obligations":[]})
            }
        ),
        Response::Ok
    ));
    let mut final_receipt = Value::Null;
    for _ in 0..64 {
        final_receipt = harness.call(
            "transport",
            json!({"operation":"lookup","operation_id":"session-one"}),
        );
        if final_receipt["status"] != "pending" {
            break;
        }
    }
    assert_eq!(final_receipt["status"], "succeeded");
    assert_eq!(harness.provider_calls.len(), 2);
    assert_eq!(harness.provider_calls[0]["method"], "resource_plan");
    assert_eq!(harness.provider_calls[1]["method"], "lifecycle_callback");
}
