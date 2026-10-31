//! Cross-module tests run the production lifecycle ABI against the actual broker.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVICE: &str = "amaterasu.extension-registry/1";
const COORDINATOR: &str = "amaterasu.extension-registry";
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
    state: Option<Value>,
    state_revision: u64,
    provider_accepts: bool,
    provider_hangs: bool,
    conflict_on_set: bool,
    scope_allow: bool,
    scope_spoof: bool,
    scope_calls: usize,
    scope_source: Option<Value>,
}

impl Harness {
    fn new() -> Self {
        Self::new_config(false)
    }

    fn new_config(migrating: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-registry-broker-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let descriptor = json!({"provider":"example.demo","revision":if migrating {2} else {1},"options":[{"id":"enabled","label":"Enabled","help":"","definition":{"type":"boolean","default":true},"allowed_scopes":["host","application","session"],"apply_policy":"next_session","required_capabilities":[]}],"actions":[],"capabilities":[],"settings_validate":{"service":"example.demo/1","method":"settings_validate","interface_major":1,"interface_minor":0,"schema_revision":1}});
        use sha2::{Digest, Sha256};
        let typed: amaterasu_extension_api::ExtensionDescriptor =
            serde_json::from_value(descriptor.clone()).unwrap();
        let descriptor_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&typed).unwrap()));
        let migrations = if migrating {
            json!([{"from_schema_revision":1,"old_fields":{},"old_allowed":{},"renames":{},"removed":[],"replacements":[]}])
        } else {
            json!([])
        };
        let method = json!({"service":"example.demo/1","method":"settings_validate","interface_major":1,"interface_minor":0,"request":{"required":{"values":"object"},"optional":{},"allow_extra":false},"response":{"required":{"ok":"boolean","valid":"boolean"},"optional":{},"allow_extra":false},"privilege":"user"});
        let configuration = serde_json::from_value(json!({
            "principals": { COORDINATOR:{"provide":[SERVICE],"call":["example.demo/1","amaterasu.state/1","amaterasu.session-lifecycle/1"]}, "amaterasu.state":{"provide":["amaterasu.state/1"]}, "lifecycle":{"provide":["amaterasu.session-lifecycle/1"]}, "example.demo":{"provide":["example.demo/1"]}, "management":{"call":[SERVICE,"example.demo/1","amaterasu.state/1","amaterasu.session-lifecycle/1","input/1"]}, "input":{"provide":["input/1"],"call":[SERVICE,"amaterasu.session-lifecycle/1","example.demo/1","amaterasu.state/1"]}, "outsider":{"call":[SERVICE,"example.demo/1"]} },
            "api_methods":[{"provider":"example.demo","callers":[COORDINATOR,"management"],"method":method}],
            "settings": {COORDINATOR:{"descriptors":[{"descriptor":descriptor,"descriptor_sha256":descriptor_hash,"approved_methods":[method],"grants":[{"caller":"management","scope":{"kind":"host"},"read":true,"write":true},{"caller":"input","scope":{"kind":"host"},"read":true,"write":false},{"caller":"outsider","scope":{"kind":"host"},"read":true,"write":false}],"migrations":migrations}]}}
        })).unwrap();
        let mut broker = Broker::new(configuration).unwrap();
        let method = serde_json::from_value(method).unwrap();
        assert!(matches!(
            broker.dispatch("example.demo", Command::RegisterApi { method }),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(
                "amaterasu.state",
                Command::Register {
                    service: "amaterasu.state/1".into()
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(
                "lifecycle",
                Command::Register {
                    service: "amaterasu.session-lifecycle/1".into()
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(
                "input",
                Command::Register {
                    service: "input/1".into()
                }
            ),
            Response::Ok
        ));
        let broker = Rc::new(RefCell::new(broker));
        let endpoint = Box::new(Endpoint {
            broker: broker.clone(),
            principal: COORDINATOR,
        });
        let api = unsafe { *amaterasu_extension_registry::amaterasu_module_v1() };
        let mut harness = Self {
            broker,
            endpoint,
            api,
            instance: std::ptr::null_mut(),
            root,
            request_sequence: 0,
            state: if migrating {
                Some(json!({"revision":3,"schema_revision":1,"layers":[]}))
            } else {
                None
            },
            state_revision: u64::from(migrating),
            provider_accepts: true,
            provider_hangs: false,
            conflict_on_set: false,
            scope_allow: true,
            scope_spoof: false,
            scope_calls: 0,
            scope_source: None,
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
    }

    fn pump_providers(&mut self) {
        let scope_request = self
            .broker
            .borrow_mut()
            .dispatch("lifecycle", Command::Poll {});
        if let Response::Request {
            id,
            caller,
            initiator,
            payload,
            ..
        } = scope_request
        {
            self.scope_calls += 1;
            if payload["input_binding"].is_string() {
                assert_eq!(
                    payload["input_boot_epoch"],
                    "0123456789abcdef0123456789abcdef"
                );
            }
            self.scope_source = Some(payload["source"].clone());
            assert_eq!(caller, COORDINATOR);
            assert_eq!(initiator, "management");
            assert_eq!(payload["source"]["initiator"], "management");
            assert!(
                payload["source"]["caller"] == "management"
                    || payload["source"]["caller"] == "input"
            );
            let result = if self.scope_allow {
                json!({"status":"succeeded","target":payload["target"],"scope":payload["scope"],"scope_id":if self.scope_spoof {"other"} else {payload["scope_id"].as_str().unwrap()}})
            } else {
                json!({"status":"rejected"})
            };
            assert!(matches!(
                self.broker.borrow_mut().dispatch(
                    "lifecycle",
                    Command::Complete {
                        request: id,
                        payload: result
                    }
                ),
                Response::Ok
            ));
        }
        let state_request = self
            .broker
            .borrow_mut()
            .dispatch("amaterasu.state", Command::Poll {});
        if let Response::Request { id, payload, .. } = state_request {
            if payload["action"] == "set" && self.conflict_on_set {
                self.state_revision += 1;
                self.conflict_on_set = false;
            }
            let response = match payload["action"].as_str().unwrap() {
                "get" => {
                    json!({"ok":true,"found":self.state.is_some(),"value":self.state,"revision":self.state_revision})
                }
                "set" if payload["expected_revision"].as_u64() == Some(self.state_revision) => {
                    self.state_revision += 1;
                    self.state = Some(payload["value"].clone());
                    json!({"ok":true,"revision":self.state_revision})
                }
                "set" => {
                    json!({"ok":false,"error":{"code":"revision_conflict","current_revision":self.state_revision}})
                }
                _ => panic!("unexpected state action"),
            };
            assert!(matches!(
                self.broker.borrow_mut().dispatch(
                    "amaterasu.state",
                    Command::Complete {
                        request: id,
                        payload: response
                    }
                ),
                Response::Ok
            ));
        }
        if !self.provider_hangs {
            let provider_request = self
                .broker
                .borrow_mut()
                .dispatch("example.demo", Command::Poll {});
            if let Response::Request { id, payload, .. } = provider_request {
                assert_eq!(payload["method"], "settings_validate");
                let result = json!({"ok":true,"valid":self.provider_accepts});
                assert!(matches!(
                    self.broker.borrow_mut().dispatch(
                        "example.demo",
                        Command::Complete {
                            request: id,
                            payload: result
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
            self.pump_providers();
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
fn discovery_uses_actual_broker_approval_and_initiator() {
    let mut harness = Harness::new();
    let page = harness.call(
        "management",
        json!({"operation":"list","after":null,"limit":16,"revision":null}),
    );
    assert_eq!(page["ok"], true);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let denied = harness.call(
        "outsider",
        json!({"operation":"list","after":null,"limit":16,"revision":null}),
    );
    assert_eq!(denied["ok"], true);
    assert!(denied["items"].as_array().unwrap().is_empty());
    let stale = harness.call(
        "management",
        json!({"operation":"list","after":"x","limit":16,"revision":0}),
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
}

#[test]
fn settings_provider_rejection_and_compare_and_swap_use_actual_broker() {
    let mut harness = Harness::new();
    let request = |revision| json!({"operation":"settings_update","provider":"example.demo","scope":{"kind":"host"},"expected_revision":revision,"values":{}});
    harness.provider_accepts = false;
    let rejected = harness.call("management", request(0));
    assert_eq!(rejected["error"]["code"], "invalid_values");
    assert!(harness.state.is_none());
    harness.provider_accepts = true;
    let accepted = harness.call("management", request(0));
    assert_eq!(accepted["ok"], true);
    assert_eq!(accepted["revision"], 1);
    let stale = harness.call("management", request(0));
    assert_eq!(stale["error"]["code"], "revision_conflict");
    let get = harness.call("management",json!({"operation":"settings_get","provider":"example.demo","scope":{"kind":"host"},"application":null,"paired_client":null,"session":null}));
    assert_eq!(get["revision"], 1);
}

#[test]
fn concurrent_state_write_conflicts_before_commit() {
    let mut harness = Harness::new();
    harness.conflict_on_set = true;
    let result = harness.call("management",json!({"operation":"settings_update","provider":"example.demo","scope":{"kind":"host"},"expected_revision":0,"values":{}}));
    assert_eq!(result["error"]["code"], "revision_conflict");
    assert!(harness.state.is_none());
}

#[test]
fn stalled_provider_never_commits_settings_after_request_deadline() {
    let mut harness = Harness::new();
    harness.provider_hangs = true;
    let response = harness.broker.borrow_mut().dispatch("management", Command::Request {
        service: SERVICE.into(),
        payload: json!({"operation":"settings_update","provider":"example.demo","scope":{"kind":"host"},"expected_revision":0,"values":{}}),
        timeout_ms: 30,
        operation_id:"timeout-test".into(),
        parent:None,
    });
    let Response::Accepted { id } = response else {
        panic!("request not accepted");
    };
    for _ in 0..12 {
        harness.tick();
        harness.pump_providers();
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    for _ in 0..4 {
        harness.tick();
        harness.pump_providers();
    }
    assert!(harness.state.is_none());
    assert!(
        !matches!(harness.broker.borrow_mut().dispatch("management", Command::TakeResult {request:id}),Response::Result {payload} if payload["ok"] == true)
    );
}

#[test]
fn installed_migration_plan_validates_and_commits_through_broker() {
    let mut harness = Harness::new_config(true);
    harness.provider_accepts = false;
    let request =
        || json!({"operation":"settings_migrate","provider":"example.demo","expected_revision":3});
    let rejected = harness.call("management", request());
    assert_eq!(rejected["error"]["code"], "invalid_values");
    assert_eq!(harness.state.as_ref().unwrap()["schema_revision"], 1);
    harness.provider_accepts = true;
    let success = harness.call("management", request());
    assert_eq!(success["ok"], true);
    assert_eq!(success["revision"], 4);
    assert_eq!(harness.state.as_ref().unwrap()["schema_revision"], 2);
}

#[test]
fn dynamic_scope_requires_matching_lifecycle_authority() {
    let mut harness = Harness::new();
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":7});
    let request = || json!({"operation":"settings_get","provider":"example.demo","scope":{"kind":"session","id":"session"},"application":"app","paired_client":null,"session":"session","target":target,"input_binding":"token","input_boot_epoch":"0123456789abcdef0123456789abcdef"});
    let result = harness.call("management", request());
    assert_eq!(result["ok"], true);
    assert_eq!(harness.scope_calls, 2);
    harness.scope_spoof = true;
    let rejected = harness.call("management", request());
    assert_eq!(rejected["error"]["code"], "permission_denied");
    harness.scope_spoof = false;
    harness.scope_allow = false;
    let rejected = harness.call("management", request());
    assert_eq!(rejected["error"]["code"], "permission_denied");
    let mut forged = request();
    forged["source"] = json!({"caller":"input","initiator":"other"});
    assert_eq!(
        harness.call("management", forged)["error"]["code"],
        "invalid_request"
    );
    let mut no_target = request();
    no_target.as_object_mut().unwrap().remove("target");
    assert_eq!(
        harness.call("management", no_target)["error"]["code"],
        "invalid_request"
    );
    let mut missing_epoch = request();
    missing_epoch
        .as_object_mut()
        .unwrap()
        .remove("input_boot_epoch");
    assert_eq!(
        harness.call("management", missing_epoch)["error"]["code"],
        "invalid_request"
    );
    let mut malformed_epoch = request();
    malformed_epoch["input_boot_epoch"] = json!("ABC");
    assert_eq!(
        harness.call("management", malformed_epoch)["error"]["code"],
        "invalid_request"
    );
    let before = harness.scope_calls;
    let rejected=harness.call("management",json!({"operation":"settings_get","provider":"example.demo","scope":{"kind":"session","id":"session"},"application":null,"paired_client":"client","session":"session","target":target,"input_binding":"token","input_boot_epoch":"0123456789abcdef0123456789abcdef"}));
    assert_eq!(rejected["error"]["code"], "permission_denied");
    assert_eq!(before, harness.scope_calls);
}

#[test]
fn delegated_input_source_is_attested_by_broker_not_request_payload() {
    let mut harness = Harness::new();
    let parent = harness.broker.borrow_mut().dispatch(
        "management",
        Command::Request {
            service: "input/1".into(),
            payload: json!({}),
            timeout_ms: 5_000,
            operation_id: "input.parent".into(),
            parent: None,
        },
    );
    let Response::Accepted { id: parent } = parent else {
        panic!("input parent rejected")
    };
    assert!(
        matches!(harness.broker.borrow_mut().dispatch("input",Command::Poll {}), Response::Request {id,..} if id==parent)
    );
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":7});
    let payload = json!({"operation":"settings_get","provider":"example.demo","scope":{"kind":"session","id":"session"},"application":null,"paired_client":null,"session":"session","target":target,"input_binding":"token","input_boot_epoch":"0123456789abcdef0123456789abcdef"});
    let child = harness.broker.borrow_mut().dispatch(
        "input",
        Command::Request {
            service: SERVICE.into(),
            payload,
            timeout_ms: 5_000,
            operation_id: "input.child".into(),
            parent: Some(parent),
        },
    );
    let Response::Accepted { id: child } = child else {
        panic!("registry request rejected: {child:?}")
    };
    let mut result = None;
    for _ in 0..100 {
        harness.tick();
        harness.pump_providers();
        if let Response::Result { payload } = harness
            .broker
            .borrow_mut()
            .dispatch("input", Command::TakeResult { request: child })
        {
            result = Some(payload);
            break;
        }
    }
    let result = result.unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(harness.scope_calls, 1);
    assert_eq!(
        harness.scope_source,
        Some(json!({"caller":"input","initiator":"management"}))
    );
}

#[test]
fn canonical_scope_also_selects_effective_overlay() {
    let mut harness = Harness::new();
    harness.state = Some(
        json!({"revision":1,"schema_revision":1,"layers":[{"scope":{"kind":"session","id":"session"},"values":{"enabled":false}}]}),
    );
    harness.state_revision = 1;
    let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":7});
    let result=harness.call("management",json!({"operation":"settings_get","provider":"example.demo","scope":{"kind":"session","id":"session"},"application":null,"paired_client":null,"session":null,"target":target,"input_binding":"token","input_boot_epoch":"0123456789abcdef0123456789abcdef"}));
    assert_eq!(result["ok"], true);
    assert_eq!(result["values"]["enabled"], false);
    assert_eq!(result["effective"]["enabled"]["value"], false);
    assert_eq!(
        result["effective"]["enabled"]["source"],
        json!({"kind":"session","id":"session"})
    );
    let mismatch=harness.call("management",json!({"operation":"settings_get","provider":"example.demo","scope":{"kind":"session","id":"session"},"application":null,"paired_client":null,"session":"other","target":target,"input_binding":"token","input_boot_epoch":"0123456789abcdef0123456789abcdef"}));
    assert_eq!(mismatch["error"]["code"], "invalid_request");
}
