//! Registered display action integration through production broker and module ABI.
//! The action dispatcher and native/state services are simulated; no hardware is used.
//! Qualified against display source 1bf5ab8b7d81371aeac432db0ee8b0902e614071,
//! artifact SHA-256 d119cb0ac32a96074f0c7a9f4fca56b72fc6f328158bb3d700512c158ecd1525,
//! and approvals SHA-256 8ce545418800c32cf335ae5e51b83a5b227571ebe58c4bbf737c7b87e7f8e6fc.
#![cfg(target_os = "linux")]
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{CString, c_char, c_void},
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

#[link(name = "dl")]
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}
const COORDINATOR: &str = "amaterasu.session-lifecycle";
const DISPLAY: &str = "nonary.display-settings";
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const DISPLAY_SERVICE: &str = "nonary.display-settings/1";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Endpoint {
    broker: Rc<RefCell<Broker>>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: abi::BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: the harness retains each boxed endpoint for the full module lifetime.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    // SAFETY: the input is borrowed for this synchronous invocation.
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    if endpoint.principal == DISPLAY
        && (matches!(&response, Response::Error { .. })
            || matches!(&response, Response::Result { payload } if payload["status"] == "rejected"))
    {
        eprintln!("display broker response: {response:?}");
    }
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: reply copies the bytes synchronously before this function returns.
    unsafe { reply.send(&bytes) };
    abi::STATUS_OK
}
struct RunningModule {
    api: abi::ModuleApi,
    instance: *mut c_void,
    endpoint: Box<Endpoint>,
}
impl RunningModule {
    fn new(api: abi::ModuleApi, broker: Rc<RefCell<Broker>>, principal: &'static str) -> Self {
        let mut endpoint = Box::new(Endpoint { broker, principal });
        let host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *endpoint as *mut Endpoint).cast(),
            invoke,
        };
        let mut instance = std::ptr::null_mut();
        assert!(api.header.supports::<abi::ModuleApi>());
        // SAFETY: table is valid and endpoint remains boxed until destruction.
        assert_eq!(
            unsafe { (api.create)(&host, &mut instance) },
            abi::STATUS_OK
        );
        assert!(!instance.is_null());
        Self {
            api,
            instance,
            endpoint,
        }
    }
    fn tick(&self) {
        let _keep_alive = &self.endpoint;
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
    }
}
impl Drop for RunningModule {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            unsafe { (self.api.destroy)(self.instance) };
        }
    }
}
fn domain(revision: u64, borrowers: usize) -> Value {
    json!({"id":"provider_session:abc","revision":revision,"borrowers":borrowers,"pending_mutations":0,"scope":"provider_session"})
}
struct Fixture {
    broker: Rc<RefCell<Broker>>,
    lifecycle: RunningModule,
    display: RunningModule,
    library: *mut c_void,
    root: PathBuf,
    next: u64,
    state_revision: u64,
    values: BTreeMap<String, Value>,
    collection_revision: u64,
    entries: BTreeMap<String, Value>,
    metadata: Value,
    action_receipt_revision: u64,
    action_receipt_entries: BTreeMap<String, Value>,
    action_receipt_metadata: Value,
    action_epoch_revision: u64,
    action_epoch_entries: BTreeMap<String, Value>,
    action_epoch_metadata: Value,
    native_calls: Vec<Value>,
    references: BTreeMap<String, String>,
    borrows: BTreeMap<String, String>,
    domain_revision: u64,
    borrow_sequence: u32,
}
impl Fixture {
    fn new() -> Self {
        let examples =
            PathBuf::from(std::env::var_os("AMATERASU_DISPLAY_EXAMPLES").expect("examples path"));
        let approvals_path = std::env::var_os("AMATERASU_DISPLAY_APPROVALS")
            .map(PathBuf::from)
            .unwrap_or_else(|| examples.join("lifecycle-api-approvals.json"));
        let approvals: Value =
            serde_json::from_slice(&std::fs::read(approvals_path).unwrap()).unwrap();
        let descriptor: Value = serde_json::from_slice(
            &std::fs::read(examples.join("settings-descriptor.json")).unwrap(),
        )
        .unwrap();
        let mut settings: BTreeMap<String, Value> = descriptor["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["id"].as_str().unwrap().to_owned(),
                    v["definition"]["default"].clone(),
                )
            })
            .collect();
        settings.insert("enabled".into(), json!(true));
        let root = std::env::temp_dir().join(format!(
            "amaterasu-cross-abi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let method = |name| json!({"service":DISPLAY_SERVICE,"method":name,"interface_major":1,"interface_minor":0,"schema_revision":1});
        let configuration = json!({"principals":{
            COORDINATOR:{"provide":[LIFECYCLE],"call":[LIFECYCLE,DISPLAY_SERVICE,"amaterasu.virtual-display/1","amaterasu.state/1"]},
            DISPLAY:{"provide":[DISPLAY_SERVICE],"call":[LIFECYCLE,"amaterasu.virtual-display/1","amaterasu.state/1"]},
            "transport":{"call":[LIFECYCLE]}, "capture":{"call":[LIFECYCLE]}, "amaterasu.extension-registry":{"call":[DISPLAY_SERVICE]},
            "amaterasu.management":{"call":[DISPLAY_SERVICE]},
            "amaterasu.extension-actions":{"call":[DISPLAY_SERVICE,LIFECYCLE,"amaterasu.virtual-display/1","amaterasu.state/1"]},
            "virtual":{"provide":["amaterasu.virtual-display/1"]}, "state":{"provide":["amaterasu.state/1"]}
        },"api_methods":approvals["api_methods"],"settings":{COORDINATOR:{
            "journal_path":root.join("journal.json"), "action_service":"amaterasu.extension-actions", "trusted_callers":["transport","capture",DISPLAY,"amaterasu.extension-actions"],
            "session_delegates":["capture"], "consumers":{"capture":["capture"]},
            "operation_grants":{"transport":["prepare","lookup","prepare_stream","resume_stream","snapshot","start_stream","pause_stream","end_session","application_retired"],"capture":["ack_binding","retire_stream","lookup"],DISPLAY:["verify_provider_epoch","refresh_provider_observations","describe_action_target","verify_action_effect","check_action_effect"],"amaterasu.extension-actions":["authorize_action","consume_action_permit","begin_action_transition","complete_action"]},
            "consumer_requirements":{"execution_domain":"desktop","capture_providers":["amaterasu.capture-pipewire/1"],"selector_schemas":["amaterasu.kwin-wl-output/1"]},
            "participants":[{"provider":DISPLAY,"plan_method":method("resource_plan"),"callback_method":method("lifecycle_callback"),"status_method":method("lifecycle_status"),"binding_validate_method":method("binding_validate"),"settings_revision":"one","settings":settings,"capabilities":[],"cleanup_methods":[method("lifecycle_callback")]}]
        }}});
        let config = serde_json::from_value(configuration).unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(config).unwrap()));
        for (caller, service) in [
            ("virtual", "amaterasu.virtual-display/1"),
            ("state", "amaterasu.state/1"),
        ] {
            assert!(matches!(
                broker.borrow_mut().dispatch(
                    caller,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        let path =
            CString::new(std::env::var("AMATERASU_DISPLAY_PLUGIN").expect("plugin path")).unwrap();
        // SAFETY: this opt-in test loads an explicitly supplied trusted artifact and retains it through module destruction.
        let library = unsafe { dlopen(path.as_ptr(), 2) };
        assert!(!library.is_null(), "dlopen failed");
        let symbol = CString::new("amaterasu_module_v1").unwrap();
        let symbol = unsafe { dlsym(library, symbol.as_ptr()) };
        assert!(!symbol.is_null());
        // SAFETY: symbol is the documented entry point for the explicitly supplied module.
        let entry: abi::ModuleEntry = unsafe { std::mem::transmute(symbol) };
        let display_api = unsafe { *entry() };
        let display = RunningModule::new(display_api, broker.clone(), DISPLAY);
        let lifecycle_api = unsafe { *amaterasu_session_lifecycle::amaterasu_module_v1() };
        let lifecycle = RunningModule::new(lifecycle_api, broker.clone(), COORDINATOR);
        Self {
            broker,
            lifecycle,
            display,
            library,
            root,
            next: 0,
            state_revision: 0,
            values: BTreeMap::new(),
            collection_revision: 0,
            entries: BTreeMap::new(),
            metadata: Value::Null,
            action_receipt_revision: 0,
            action_receipt_entries: BTreeMap::new(),
            action_receipt_metadata: Value::Null,
            action_epoch_revision: 0,
            action_epoch_entries: BTreeMap::new(),
            action_epoch_metadata: Value::Null,
            native_calls: Vec::new(),
            references: BTreeMap::new(),
            borrows: BTreeMap::new(),
            domain_revision: 1,
            borrow_sequence: 0,
        }
    }
    fn tick(&mut self) {
        self.lifecycle.tick();
        self.display.tick();
        for principal in ["virtual", "state"] {
            let response = self
                .broker
                .borrow_mut()
                .dispatch(principal, Command::Poll {});
            if let Response::Request { id, payload, .. } = response {
                let answer = if principal == "state" {
                    self.state(&payload)
                } else {
                    self.native(&payload)
                };
                assert!(matches!(
                    self.broker.borrow_mut().dispatch(
                        principal,
                        Command::Complete {
                            request: id,
                            payload: answer
                        }
                    ),
                    Response::Ok
                ));
            }
        }
    }
    fn state(&mut self, p: &Value) -> Value {
        match p["action"].as_str().unwrap() {
            "get" => {
                let value = self.values.get(p["key"].as_str().unwrap());
                json!({"ok":true,"revision":self.state_revision,"found":value.is_some(),"value":value})
            }
            "set" => {
                assert_eq!(p["expected_revision"], self.state_revision);
                self.values
                    .insert(p["key"].as_str().unwrap().into(), p["value"].clone());
                self.state_revision += 1;
                json!({"ok":true,"revision":self.state_revision})
            }
            "collection_get"
                if p["collection"].as_str().is_some_and(|name| {
                    name.starts_with("display-settings/action-receipts-v1/")
                }) =>
            {
                json!({"ok":true,"revision":self.action_receipt_revision,"found":self.action_receipt_revision!=0,"metadata":self.action_receipt_metadata,"count":self.action_receipt_entries.len(),"value":self.action_receipt_entries.get(p["key"].as_str().unwrap())})
            }
            "collection_commit"
                if p["collection"].as_str().is_some_and(|name| {
                    name.starts_with("display-settings/action-receipts-v1/")
                }) =>
            {
                assert_eq!(p["expected_revision"], self.action_receipt_revision);
                for edit in p["edits"].as_array().unwrap() {
                    let key = edit["key"].as_str().unwrap().to_owned();
                    if edit["value"].is_null() {
                        self.action_receipt_entries.remove(&key);
                    } else {
                        self.action_receipt_entries
                            .insert(key, edit["value"].clone());
                    }
                }
                self.action_receipt_metadata = p["metadata"].clone();
                self.action_receipt_revision += 1;
                json!({"ok":true,"revision":self.action_receipt_revision})
            }
            "collection_get" if p["collection"] == "display-settings/action-epochs-v1" => {
                json!({"ok":true,"revision":self.action_epoch_revision,"found":self.action_epoch_revision!=0,"metadata":self.action_epoch_metadata,"count":self.action_epoch_entries.len(),"value":self.action_epoch_entries.get(p["key"].as_str().unwrap())})
            }
            "collection_commit" if p["collection"] == "display-settings/action-epochs-v1" => {
                assert_eq!(p["expected_revision"], self.action_epoch_revision);
                for edit in p["edits"].as_array().unwrap() {
                    let key = edit["key"].as_str().unwrap().to_owned();
                    if edit["value"].is_null() {
                        self.action_epoch_entries.remove(&key);
                    } else {
                        self.action_epoch_entries.insert(key, edit["value"].clone());
                    }
                }
                self.action_epoch_metadata = p["metadata"].clone();
                self.action_epoch_revision += 1;
                json!({"ok":true,"revision":self.action_epoch_revision})
            }
            "collection_get" => {
                json!({"ok":true,"revision":self.collection_revision,"found":self.collection_revision!=0,"metadata":self.metadata,"count":self.entries.len(),"value":self.entries.get(p["key"].as_str().unwrap())})
            }
            "collection_commit" => {
                assert_eq!(p["expected_revision"], self.collection_revision);
                for edit in p["edits"].as_array().unwrap() {
                    let key = edit["key"].as_str().unwrap().to_owned();
                    if edit["value"].is_null() {
                        self.entries.remove(&key);
                    } else {
                        self.entries.insert(key, edit["value"].clone());
                    }
                }
                self.metadata = p["metadata"].clone();
                self.collection_revision += 1;
                json!({"ok":true,"revision":self.collection_revision})
            }
            _ => panic!("unexpected state: {p}"),
        }
    }
    fn native(&mut self, p: &Value) -> Value {
        self.native_calls.push(p.clone());
        match p["action"].as_str().unwrap() {
            "status" => {
                let identity = p["identity"].as_str();
                let previous = self.native_calls.iter().find(|call| {
                    call["action"] == "create"
                        && identity.is_none_or(|wanted| call["identity"] == wanted)
                        && self.references.values().any(|id| call["identity"] == *id)
                });
                let reference_owned = p["reference_id"].as_str().map(|reference| {
                    self.references
                        .get(reference)
                        .is_some_and(|id| identity == Some(id.as_str()))
                });
                let monitors = previous.map_or_else(
                    || json!([]),
                    |call| json!([{"reference_owned":reference_owned.unwrap_or(false),"specification":{"identity":call["identity"],"width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu","hdr_enabled":false},"state":"ready","output":"Virtual-1","error":null,"virtual_display_layout":"extended","dd_virtual_display_scale":0,"capture_output":"kwin:Virtual-1","ready":{"device_path":"Virtual-1","active":true}}]),
                );
                json!({"ok":true,"enabled":true,"native_supported":true,"driver":"libvirtualdisplay","ownership_authority":"unix_uid_broker","recovery_ready":true,"reference_owned":reference_owned,"monitors":monitors,"mutation_domain":domain(self.domain_revision,self.borrows.len()),"settings":{"virtual_display_layout":"extended","dd_virtual_display_scale":0}})
            }
            "create" => {
                assert_eq!(
                    self.metadata["pending"], 1,
                    "effect must follow durable reservation"
                );
                assert!(
                    !self.values.is_empty(),
                    "effect must follow durable resource record"
                );
                self.references.insert(
                    p["reference_id"].as_str().unwrap().into(),
                    p["identity"].as_str().unwrap().into(),
                );
                json!({"ok":true,"reference_owned":true,"monitor":{"specification":{"identity":p["identity"],"width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu","hdr_enabled":false},"virtual_display_layout":"extended","dd_virtual_display_scale":0,"state":"ready","error":null,"ready":{"device_path":"Virtual-1","active":true},"capture_output":"kwin:Virtual-1"}})
            }
            "borrow" => {
                let reference = p["reference_id"].as_str().unwrap().to_owned();
                assert!(self.references.contains_key(&reference));
                self.borrow_sequence += 1;
                let borrow_id = format!("{:064x}", self.borrow_sequence);
                self.borrows.insert(reference, borrow_id.clone());
                self.domain_revision += 1;
                json!({"ok":true,"borrow_id":borrow_id,"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000},"mutation_domain":domain(self.domain_revision,self.borrows.len())})
            }
            "validate_borrow" => {
                assert_eq!(
                    self.borrows.get(p["reference_id"].as_str().unwrap()),
                    p["borrow_id"].as_str().map(str::to_owned).as_ref()
                );
                json!({"ok":true,"borrow_id":p["borrow_id"],"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000},"mutation_domain":domain(self.domain_revision,self.borrows.len())})
            }
            "release_borrow" => {
                let reference = p["reference_id"].as_str().unwrap();
                assert_eq!(
                    self.borrows.get(reference),
                    p["borrow_id"].as_str().map(str::to_owned).as_ref()
                );
                self.borrows.remove(reference);
                self.domain_revision += 1;
                json!({"ok":true,"released":true,"mutation_domain":domain(self.domain_revision,self.borrows.len())})
            }
            "remove" => {
                let reference = p["reference_id"].as_str().unwrap();
                assert!(!self.borrows.contains_key(reference));
                assert!(self.references.remove(reference).is_some());
                let still_present = self
                    .references
                    .values()
                    .any(|id| id == p["identity"].as_str().unwrap());
                json!({"ok":true,"removed":!still_present,"identity":p["identity"],"reference_released":true,"reference_owned":false})
            }
            action => panic!("unexpected virtual action {action}: {p}"),
        }
    }
    fn call_as(&mut self, caller: &str, payload: Value) -> Value {
        self.next += 1;
        let response = self.broker.borrow_mut().dispatch(
            caller,
            Command::Request {
                service: LIFECYCLE.into(),
                payload,
                timeout_ms: 5000,
                operation_id: format!("test-{}", self.next),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("request admission: {response:?}")
        };
        for _ in 0..200 {
            self.tick();
            match self
                .broker
                .borrow_mut()
                .dispatch(caller, Command::TakeResult { request: id })
            {
                Response::Pending => {}
                Response::Result { payload } => return payload,
                r => panic!("request result: {r:?}"),
            }
        }
        panic!("request timed out")
    }
    fn call_api(&mut self, method: &str, payload: Value) -> Value {
        self.next += 1;
        let result = self.broker.borrow_mut().dispatch(
            "amaterasu.extension-actions",
            Command::CallApi {
                service: DISPLAY_SERVICE.into(),
                method: method.into(),
                interface_major: 1,
                interface_minor: 0,
                payload,
                timeout_ms: 5000,
                operation_id: format!("action-test-{}", self.next),
                parent: None,
            },
        );
        let Response::Accepted { id } = result else {
            panic!("call API rejected: {result:?}")
        };
        for _ in 0..1000 {
            self.tick();
            let result = self.broker.borrow_mut().dispatch(
                "amaterasu.extension-actions",
                Command::TakeResult { request: id },
            );
            match result {
                Response::Pending => {}
                Response::Result { payload } => return payload,
                other => panic!("API result: {other:?}"),
            }
        }
        panic!("API call timed out")
    }
    fn call(&mut self, payload: Value) -> Value {
        self.call_as("transport", payload)
    }
    fn settled(&mut self, id: &str) -> Value {
        for _ in 0..500 {
            let result = self.call(json!({"operation":"lookup","operation_id":id}));
            if result["status"] != "pending" {
                return result;
            }
        }
        panic!("transition {id} did not settle")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // The library must remain loaded until its module instance is destroyed.
        unsafe { (self.display.api.destroy)(self.display.instance) };
        self.display.instance = std::ptr::null_mut();
        // SAFETY: the plugin instance has been destroyed and no callbacks remain.
        unsafe { dlclose(self.library) };
        self.library = std::ptr::null_mut();
        eprintln!(
            "cross-ABI test evidence retained at {}",
            self.root.display()
        );
    }
}

#[test]
#[ignore = "requires independently built display module and its eight-method approvals"]
fn registered_remove_action_runs_only_after_lifecycle_retirement() {
    use sha2::{Digest, Sha256};
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let prepared = f.call(json!({"operation":"prepare","operation_id":"prepare","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let session = f.settled("prepare");
    assert_eq!(session["status"], "succeeded", "{session}");
    let stream = f.call(json!({"operation":"prepare_stream","operation_id":"stream","target":session["target"],"intent":"initial","requested_video":video,"consumer_requirements":["capture"]}));
    assert_eq!(stream["status"], "pending", "{stream}");
    let stream = f.settled("stream");
    assert_eq!(stream["status"], "succeeded", "{stream}");
    let target = stream["target"].clone();
    let candidate = stream["candidate"].clone();
    let observed = json!({"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000});
    let ack = f.call_as("capture", json!({"operation":"ack_binding","operation_id":"ack","target":target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observed}));
    assert_eq!(ack["status"], "pending", "{ack}");
    for _ in 0..500 {
        let ack = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"ack"}),
        );
        if ack["status"] != "pending" {
            assert_eq!(ack["status"], "succeeded", "{ack}");
            break;
        }
    }
    assert_eq!(
        f.call(json!({"operation":"start_stream","operation_id":"start","target":target}))["status"],
        "succeeded"
    );
    let action_target = json!({"kind":"stream","host_epoch":target["host_epoch"],"session_id":target["session_id"],"stream_id":target["stream_id"],"expected_generation":target["generation"]});
    let operation_id = format!("{}:1", target["host_epoch"].as_str().unwrap());
    let action_id = "nonary.display-settings/remove-owned";
    let request = json!({"version":1,"operation_id":operation_id,"action_id":action_id,"target":action_target,"arguments":{},"expected_settings_revision":null});
    let planned = f.call_api("action_plan", request.clone());
    assert_eq!(planned["status"], "planned", "{planned}");
    let plan = planned["plan"].clone();
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&plan).unwrap()));
    let execute_method = json!({"service":DISPLAY_SERVICE,"method":"action_execute","interface_major":1,"interface_minor":0,"schema_revision":1});
    let auth = f.call_as("amaterasu.extension-actions", json!({"operation":"authorize_action","operation_id":operation_id,"action_id":action_id,"target":action_target,"plan_digest":digest,"plan":plan,"source":{"caller":"transport","initiator":"transport"},"provider":DISPLAY,"execute_method":execute_method}));
    assert_eq!(auth["status"], "succeeded", "{auth}; plan={plan}");
    let permit = auth["permit"].clone();
    let mut execute = request.clone();
    execute["plan"] = plan.clone();
    execute["plan_digest"] = json!(digest);
    execute["permit"] = permit.clone();
    let early = f.call_as(DISPLAY, json!({"operation":"verify_action_effect","operation_id":operation_id,"action_id":action_id,"target":action_target,"plan_digest":digest,"permit":permit,"provider":DISPLAY,"execute_method":execute_method}));
    assert_eq!(
        early["status"], "rejected",
        "premature authorization: {early}"
    );
    assert!(!f.native_calls.iter().any(|p| p["action"] == "remove"));
    let base = json!({"operation":"consume_action_permit","operation_id":operation_id,"action_id":action_id,"target":action_target,"plan_digest":digest,"permit":permit});
    let consumed = f.call_as("amaterasu.extension-actions", base.clone());
    assert_eq!(consumed["status"], "succeeded", "{consumed}");
    let mut transition = base;
    transition["operation"] = json!("begin_action_transition");
    let started = f.call_as("amaterasu.extension-actions", transition.clone());
    assert_eq!(started["status"], "pending", "{started}");
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let mut ready = Value::Null;
    for _ in 0..500 {
        ready = f.call_as("amaterasu.extension-actions", transition.clone());
        if ready["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ready["status"], "succeeded", "{ready}");
    let outcome = f.call_api("action_execute", execute.clone());
    assert_eq!(
        outcome["status"], "succeeded",
        "{outcome}; native_calls={:?}",
        f.native_calls
    );
    let count = f
        .native_calls
        .iter()
        .filter(|p| p["action"] == "remove")
        .count();
    assert_eq!(count, 1);
    let removed = f
        .native_calls
        .iter()
        .position(|p| p["action"] == "remove")
        .unwrap();
    let released = f
        .native_calls
        .iter()
        .position(|p| {
            p["action"] == "release_borrow"
                && p["reference_id"] == f.native_calls[removed]["reference_id"]
        })
        .expect("selected reference borrow was retired");
    assert!(released < removed);
    assert_eq!(f.references.len(), 1, "the session reference remains owned");
    let status = f.call_api("action_status", json!({"version":1,"operation_id":operation_id,"action_id":action_id,"target":action_target,"plan_digest":digest,"provider_receipt":outcome["receipt"]}));
    assert_eq!(status["status"], "succeeded", "{status}");
    assert_eq!(
        f.native_calls
            .iter()
            .filter(|p| p["action"] == "remove")
            .count(),
        count
    );
}
