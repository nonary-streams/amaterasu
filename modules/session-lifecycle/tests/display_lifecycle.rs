//! Opt-in cross-ABI integration with a separately built display provider.
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
    let Ok(command) = serde_json::from_slice(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
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
    native_calls: Vec<Value>,
}
impl Fixture {
    fn new() -> Self {
        let examples =
            PathBuf::from(std::env::var_os("AMATERASU_DISPLAY_EXAMPLES").expect("examples path"));
        let approvals: Value = serde_json::from_slice(
            &std::fs::read(examples.join("lifecycle-api-approvals.json")).unwrap(),
        )
        .unwrap();
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
            COORDINATOR:{"provide":[LIFECYCLE],"call":[DISPLAY_SERVICE,"amaterasu.virtual-display/1","amaterasu.state/1"]},
            DISPLAY:{"provide":[DISPLAY_SERVICE],"call":[LIFECYCLE,"amaterasu.virtual-display/1","amaterasu.state/1"]},
            "transport":{"call":[LIFECYCLE]}, "capture":{"call":[LIFECYCLE]}, "amaterasu.extension-registry":{"call":[DISPLAY_SERVICE]},
            "amaterasu.management":{"call":[DISPLAY_SERVICE]},
            "virtual":{"provide":["amaterasu.virtual-display/1"]}, "state":{"provide":["amaterasu.state/1"]}
        },"api_methods":approvals["api_methods"],"settings":{COORDINATOR:{
            "journal_path":root.join("journal.json"), "trusted_callers":["transport","capture",DISPLAY],
            "session_delegates":["capture"], "consumers":{"capture":["capture"]},
            "operation_grants":{"transport":["prepare","lookup","prepare_stream","snapshot","start_stream"],"capture":["ack_binding","lookup"],DISPLAY:["verify_provider_epoch"]},
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
            native_calls: Vec::new(),
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
        let borrowers = self
            .native_calls
            .iter()
            .filter(|call| call["action"] == "borrow")
            .count();
        let domain = json!({"id":"provider_session:abc","revision":1+borrowers,"borrowers":borrowers,"pending_mutations":0,"scope":"provider_session"});
        match p["action"].as_str().unwrap() {
            "status" => {
                let previous = self
                    .native_calls
                    .iter()
                    .find(|call| call["action"] == "create" && call["identity"] == p["identity"]);
                let reference_owned = self.native_calls.iter().any(|call| {
                    call["action"] == "create"
                        && call["identity"] == p["identity"]
                        && call["reference_id"] == p["reference_id"]
                });
                let monitors = previous.map_or_else(
                    || json!([]),
                    |call| json!([{"reference_owned":reference_owned,"specification":{"identity":call["identity"],"width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu","hdr_enabled":false},"state":"ready","output":"Virtual-1","error":null,"virtual_display_layout":"extended","dd_virtual_display_scale":0,"capture_output":"kwin:Virtual-1","ready":{"device_path":"Virtual-1","active":true}}]),
                );
                json!({"ok":true,"enabled":true,"native_supported":true,"driver":"libvirtualdisplay","ownership_authority":"unix_uid_broker","recovery_ready":true,"reference_owned":reference_owned,"monitors":monitors,"mutation_domain":domain,"settings":{"virtual_display_layout":"extended","dd_virtual_display_scale":0}})
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
                json!({"ok":true,"reference_owned":true,"monitor":{"specification":{"identity":p["identity"],"width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu","hdr_enabled":false},"virtual_display_layout":"extended","dd_virtual_display_scale":0,"state":"ready","error":null,"ready":{"device_path":"Virtual-1","active":true},"capture_output":"kwin:Virtual-1"}})
            }
            "borrow" | "validate_borrow" => {
                let borrow_id = if p["action"] == "validate_borrow" {
                    p["borrow_id"].as_str().unwrap().to_owned()
                } else if borrowers == 1 {
                    "a".repeat(64)
                } else {
                    "c".repeat(64)
                };
                json!({"ok":true,"borrow_id":borrow_id,"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000},"mutation_domain":domain})
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
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
#[test]
#[ignore = "requires an independently built display module and its example approvals"]
fn real_display_and_lifecycle_round_trip_with_simulated_backends() {
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let prepared =
        f.call(json!({"operation":"prepare","operation_id":"prepare","requested_video":video}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let ready = f.settled("prepare");
    assert_eq!(
        ready["status"], "succeeded",
        "{ready}; virtual calls: {:?}",
        f.native_calls
    );
    let stream=f.call(json!({"operation":"prepare_stream","operation_id":"stream","target":ready["target"],"intent":"initial","requested_video":video}));
    assert_eq!(stream["status"], "pending", "{stream}");
    let result = f.settled("stream");
    assert_eq!(
        result["status"], "succeeded",
        "{result}; virtual calls: {:?}",
        f.native_calls
    );
    assert_eq!(result["candidate"]["resource"]["provider"], DISPLAY);
    let target = result["target"].clone();
    let candidate = result["candidate"].clone();
    let observed = json!({"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000});
    let ack=f.call_as("capture",json!({"operation":"ack_binding","operation_id":"capture-ack","target":target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observed}));
    assert_eq!(ack["status"], "pending", "{ack}");
    let mut ack_result = Value::Null;
    for _ in 0..500 {
        ack_result = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"capture-ack"}),
        );
        if ack_result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ack_result["status"], "succeeded", "{ack_result}");
    let creates: Vec<_> = f
        .native_calls
        .iter()
        .filter(|p| p["action"] == "create")
        .collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0]["identity"], creates[1]["identity"]);
    assert_ne!(creates[0]["reference_id"], creates[1]["reference_id"]);
    assert_eq!(
        f.native_calls
            .iter()
            .filter(|p| p["action"] == "borrow")
            .count(),
        2
    );
}
