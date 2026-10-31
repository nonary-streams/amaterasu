//! Production actions ABI against the real broker and simulated peer services.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, ffi::c_void, rc::Rc};

const ACTIONS: &str = "amaterasu.extension-actions";
const ACTION_SERVICE: &str = "amaterasu.extension-actions/1";
const STATE: &str = "amaterasu.state/1";
const REGISTRY: &str = "amaterasu.extension-registry/1";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const PROVIDER: &str = "example.provider/1";

struct Endpoint {
    broker: Rc<RefCell<Broker>>,
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
    let response = endpoint.broker.borrow_mut().dispatch(ACTIONS, command);
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
    instance: *mut c_void,
    state: BTreeMap<String, Value>,
    revision: u64,
    metadata: Value,
    sequence: u64,
    effects: usize,
    authorizations: usize,
    consumptions: usize,
    revoke_consume: bool,
    allow_authorize: bool,
    observed_delegated_registry: bool,
    observed_delegated_plan: bool,
}
impl Harness {
    fn new() -> Self {
        Self::with_lifecycle_grant(true)
    }

    fn with_lifecycle_grant(allowed: bool) -> Self {
        let calls = if allowed {
            vec![STATE, REGISTRY, LIFECYCLE, PROVIDER]
        } else {
            vec![STATE, REGISTRY, PROVIDER]
        };
        let schema = json!({"required":{},"optional":{},"allow_extra":true});
        let plan = json!({"service":PROVIDER,"method":"action_plan","interface_major":1,"interface_minor":0,"request":schema,"response":schema,"privilege":"user"});
        let execute = json!({"service":PROVIDER,"method":"action_execute","interface_major":1,"interface_minor":0,"request":schema,"response":schema,"privilege":"user"});
        let settings = json!({"grants":[{"principal":"management","action_ids":["example/action"],"target_kinds":["session"],"execution_classes":["session_mutation"]}]});
        let config = serde_json::from_value(json!({"principals":{
            ACTIONS:{"provide":[ACTION_SERVICE],"call":calls},
            "management":{"call":[ACTION_SERVICE,REGISTRY,PROVIDER]},
            "state":{"provide":[STATE]}, "registry":{"provide":[REGISTRY]},
            "lifecycle":{"provide":[LIFECYCLE]}, "provider":{"provide":[PROVIDER]}
        },"settings":{ACTIONS:settings},"api_methods":[
            {"provider":"provider","callers":[ACTIONS,"management"],"method":plan},
            {"provider":"provider","callers":[ACTIONS],"method":execute}
        ]}))
        .unwrap();
        let mut broker = Broker::new(config).unwrap();
        for (principal, service) in [
            ("state", STATE),
            ("registry", REGISTRY),
            ("lifecycle", LIFECYCLE),
        ] {
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
        for value in [plan, execute] {
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
        let endpoint = Box::new(Endpoint {
            broker: broker.clone(),
        });
        // SAFETY: exported module table is static and valid for the process lifetime.
        let api = unsafe { *amaterasu_extension_actions::amaterasu_module_v1() };
        let mut result = Self {
            broker,
            endpoint,
            api,
            instance: std::ptr::null_mut(),
            state: BTreeMap::new(),
            revision: 0,
            metadata: Value::Null,
            sequence: 0,
            effects: 0,
            authorizations: 0,
            consumptions: 0,
            revoke_consume: false,
            allow_authorize: true,
            observed_delegated_registry: false,
            observed_delegated_plan: false,
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
        for principal in ["state", "registry", "lifecycle", "provider"] {
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
                    json!({"ok":true,"provider":"provider","revision":1,"action":{"id":"example/action","label":"Example","description":"Example","execution":"session_mutation","arguments":{},"allowed_targets":["session"],"required_capabilities":[],"plan":plan,"execute":execute,"status":null}})
                }
                "lifecycle" => {
                    assert_eq!(caller, ACTIONS);
                    assert_eq!(initiator, ACTIONS);
                    if payload["operation"] == "authorize_action" {
                        assert_eq!(
                            payload["source"],
                            json!({"caller":"management","initiator":"management"})
                        );
                    }
                    match payload["operation"].as_str().unwrap() {
                        "authorize_action" if self.allow_authorize => {
                            self.authorizations += 1;
                            json!({"status":"succeeded","operation_id":payload["operation_id"],"target":payload["target"],"permit":"permit"})
                        }
                        "consume_action_permit" if self.allow_authorize && !self.revoke_consume => {
                            self.consumptions += 1;
                            json!({"status":"succeeded","operation_id":payload["operation_id"],"target":payload["target"],"plan_digest":payload["plan_digest"]})
                        }
                        "complete_action" => {
                            json!({"status":"succeeded","operation_id":payload["operation_id"]})
                        }
                        "authorize_action" => {
                            self.authorizations += 1;
                            json!({"status":"rejected"})
                        }
                        "consume_action_permit" => {
                            self.consumptions += 1;
                            json!({"status":"rejected"})
                        }
                        _ => json!({"status":"rejected"}),
                    }
                }
                "provider" => {
                    let arguments = &payload["arguments"];
                    match payload["method"].as_str().unwrap() {
                        "action_plan" => {
                            self.observed_delegated_plan =
                                caller == ACTIONS && initiator == "management";
                            json!({"status":"planned","operation_id":arguments["operation_id"],"action_id":arguments["action_id"],"target":arguments["target"],"plan":{"mutation_domains":[]}})
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
            self.complete(principal, id, response);
        }
    }
    fn call(&mut self, payload: Value) -> Value {
        self.sequence += 1;
        let response = self.broker.borrow_mut().dispatch(
            "management",
            Command::Request {
                service: ACTION_SERVICE.into(),
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
                .dispatch("management", Command::TakeResult { request: id });
            match result {
                Response::Pending => (),
                Response::Result { payload } => {
                    let _ = self
                        .broker
                        .borrow_mut()
                        .dispatch("management", Command::ReleaseResult { request: id });
                    return payload;
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        panic!("request stalled");
    }
    fn invoke_action(&mut self) -> String {
        for _ in 0..12 {
            self.pump();
        }
        let open = self.call(json!({"operation":"open_epoch","operation_id":"open.1"}));
        let epoch = open["epoch"].as_str().unwrap().to_owned();
        let receipt=self.call(json!({"operation":"invoke","epoch":epoch,"sequence":1,"operation_id":format!("{epoch}:1"),"action_id":"example/action","target":{"kind":"session","host_epoch":"h","session_id":"s","expected_generation":1},"arguments":{},"expected_settings_revision":null}));
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
    }
}

#[test]
fn granted_action_persists_running_before_provider_execution() {
    let mut harness = Harness::new();
    let epoch = harness.invoke_action();
    for _ in 0..500 {
        harness.pump();
        if harness.effects > 0 {
            break;
        }
    }
    assert!(harness.observed_delegated_registry && harness.observed_delegated_plan);
    assert_eq!(harness.effects, 1, "state: {:?}", harness.state);
    for _ in 0..100 {
        harness.pump();
    }
    let receipt = harness.call(json!({"operation":"status","epoch":epoch,"sequence":1}));
    assert_eq!(receipt["status"], "succeeded");
}
#[test]
fn revoked_consume_never_executes_provider() {
    let mut harness = Harness::new();
    harness.revoke_consume = true;
    let _ = harness.invoke_action();
    for _ in 0..500 {
        harness.pump();
    }
    assert_eq!(harness.effects, 0);
    assert_eq!(harness.consumptions, 1);
}

#[test]
fn denied_authorization_never_executes_provider() {
    let mut harness = Harness::new();
    harness.allow_authorize = false;
    let _ = harness.invoke_action();
    for _ in 0..500 {
        harness.pump();
    }
    assert_eq!(harness.effects, 0);
    assert_eq!(harness.authorizations, 1);
    assert_eq!(harness.consumptions, 0);
}

#[test]
fn missing_lifecycle_grant_denies_authorization_and_effect() {
    let mut harness = Harness::with_lifecycle_grant(false);
    let _ = harness.invoke_action();
    for _ in 0..500 {
        harness.pump();
    }
    assert!(harness.observed_delegated_registry && harness.observed_delegated_plan);
    assert_eq!(harness.authorizations, 0);
    assert_eq!(harness.effects, 0);
}
