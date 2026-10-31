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
    let Ok(command) = serde_json::from_slice::<Command>(bytes) else {
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
fn domain(revision: u64, borrowers: usize) -> Value {
    json!({"id":"provider_session:abc","revision":revision,"borrowers":borrowers,"pending_mutations":0,"scope":"provider_session"})
}
#[derive(Default, serde::Serialize)]
struct Collection {
    revision: u64,
    entries: BTreeMap<String, Value>,
    metadata: Value,
}
struct Fixture {
    broker: Rc<RefCell<Broker>>,
    lifecycle: RunningModule,
    display: RunningModule,
    library: *mut c_void,
    lifecycle_library: *mut c_void,
    root: PathBuf,
    next: u64,
    state_revision: u64,
    values: BTreeMap<String, Value>,
    collections: BTreeMap<String, Collection>,
    native_calls: Vec<Value>,
    references: BTreeMap<String, String>,
    borrows: BTreeMap<String, String>,
    domain_revision: u64,
    borrow_sequence: u32,
    mode: (u64, u64, u64),
    reconfigurations: BTreeMap<String, Value>,
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
            "amaterasu.extension-actions":{"call":[DISPLAY_SERVICE]},
            "amaterasu.management":{"call":[DISPLAY_SERVICE]},
            "virtual":{"provide":["amaterasu.virtual-display/1"]}, "state":{"provide":["amaterasu.state/1"]}
        },"api_methods":approvals["api_methods"],"settings":{COORDINATOR:{
            "journal_path":root.join("journal.json"), "trusted_callers":["transport","capture",DISPLAY],
            "session_delegates":["capture"], "consumers":{"capture":["capture"]},
            "operation_grants":{"transport":["prepare","lookup","prepare_stream","resume_stream","snapshot","start_stream","pause_stream","end_session","application_retired"],"capture":["ack_binding","retire_stream","lookup"],DISPLAY:["verify_provider_epoch","refresh_provider_observations"]},
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
        let mut lifecycle_library = std::ptr::null_mut();
        let lifecycle_api = if let Some(path) = std::env::var_os("AMATERASU_LIFECYCLE_PLUGIN") {
            let path = CString::new(path.as_encoded_bytes()).unwrap();
            // SAFETY: this opt-in test retains the explicitly supplied trusted library until after module destruction.
            lifecycle_library = unsafe { dlopen(path.as_ptr(), 2) };
            assert!(!lifecycle_library.is_null(), "lifecycle dlopen failed");
            let symbol = CString::new("amaterasu_module_v1").unwrap();
            let address = unsafe { dlsym(lifecycle_library, symbol.as_ptr()) };
            assert!(!address.is_null());
            // SAFETY: the symbol is the documented entry point of the selected lifecycle module.
            let entry: abi::ModuleEntry = unsafe { std::mem::transmute(address) };
            unsafe { *entry() }
        } else {
            unsafe { *amaterasu_session_lifecycle::amaterasu_module_v1() }
        };
        let lifecycle = RunningModule::new(lifecycle_api, broker.clone(), COORDINATOR);
        Self {
            broker,
            lifecycle,
            display,
            library,
            lifecycle_library,
            root,
            next: 0,
            state_revision: 0,
            values: BTreeMap::new(),
            collections: BTreeMap::new(),
            native_calls: Vec::new(),
            references: BTreeMap::new(),
            borrows: BTreeMap::new(),
            domain_revision: 1,
            borrow_sequence: 0,
            mode: (1920, 1080, 60000),
            reconfigurations: BTreeMap::new(),
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
                let collection = self
                    .collections
                    .entry(p["collection"].as_str().unwrap().into())
                    .or_default();
                json!({"ok":true,"revision":collection.revision,"found":collection.revision!=0,"metadata":collection.metadata,"count":collection.entries.len(),"value":collection.entries.get(p["key"].as_str().unwrap())})
            }
            "collection_commit" => {
                let collection = self
                    .collections
                    .entry(p["collection"].as_str().unwrap().into())
                    .or_default();
                assert_eq!(p["expected_revision"], collection.revision);
                for edit in p["edits"].as_array().unwrap() {
                    let key = edit["key"].as_str().unwrap().to_owned();
                    if edit["value"].is_null() {
                        collection.entries.remove(&key);
                    } else {
                        collection.entries.insert(key, edit["value"].clone());
                    }
                }
                collection.metadata = p["metadata"].clone();
                collection.revision += 1;
                json!({"ok":true,"revision":collection.revision})
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
                    |call| json!([{"reference_owned":reference_owned.unwrap_or(false),"specification":{"identity":call["identity"],"width":self.mode.0,"height":self.mode.1,"refresh_millihz":self.mode.2,"name":"Amaterasu","hdr_enabled":false},"state":"ready","output":"Virtual-1","error":null,"virtual_display_layout":"extended","dd_virtual_display_scale":0,"capture_output":"kwin:Virtual-1","ready":{"device_path":"Virtual-1","active":true}}]),
                );
                json!({"ok":true,"enabled":true,"native_supported":true,"driver":"libvirtualdisplay","ownership_authority":"unix_uid_broker","recovery_ready":true,"reference_owned":reference_owned,"monitors":monitors,"mutation_domain":domain(self.domain_revision,self.borrows.len()),"settings":{"virtual_display_layout":"extended","dd_virtual_display_scale":0}})
            }
            "create" => {
                assert_eq!(
                    self.collections["display-settings/operation-receipts-v2"].metadata["pending"],
                    1,
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
                assert_eq!(p["width"], self.mode.0);
                assert_eq!(p["height"], self.mode.1);
                json!({"ok":true,"reference_owned":true,"monitor":{"specification":{"identity":p["identity"],"width":self.mode.0,"height":self.mode.1,"refresh_millihz":self.mode.2,"name":"Amaterasu","hdr_enabled":false},"virtual_display_layout":"extended","dd_virtual_display_scale":0,"state":"ready","error":null,"ready":{"device_path":"Virtual-1","active":true},"capture_output":"kwin:Virtual-1"}})
            }
            "borrow" => {
                let reference = p["reference_id"].as_str().unwrap().to_owned();
                assert!(self.references.contains_key(&reference));
                self.borrow_sequence += 1;
                let borrow_id = format!("{:064x}", self.borrow_sequence);
                self.borrows.insert(reference, borrow_id.clone());
                self.domain_revision += 1;
                json!({"ok":true,"borrow_id":borrow_id,"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":self.mode.0,"height":self.mode.1,"refresh_millihz":self.mode.2},"mutation_domain":domain(self.domain_revision,self.borrows.len())})
            }
            "validate_borrow" => {
                assert_eq!(
                    self.borrows.get(p["reference_id"].as_str().unwrap()),
                    p["borrow_id"].as_str().map(str::to_owned).as_ref()
                );
                json!({"ok":true,"borrow_id":p["borrow_id"],"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":self.mode.0,"height":self.mode.1,"refresh_millihz":self.mode.2},"mutation_domain":domain(self.domain_revision,self.borrows.len())})
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
            "reconfigure" => {
                let operation = p["operation_id"].as_str().unwrap().to_owned();
                if let Some(result) = self.reconfigurations.get(&operation) {
                    return result.clone();
                }
                let reference = p["reference_id"].as_str().unwrap();
                assert_eq!(
                    self.borrows.len(),
                    1,
                    "resize requires retirement of other borrowers"
                );
                assert_eq!(
                    self.borrows.get(reference),
                    p["borrow_id"].as_str().map(str::to_owned).as_ref()
                );
                assert_eq!(p["expected_topology_revision"], self.domain_revision);
                self.mode = (
                    p["width"].as_u64().unwrap(),
                    p["height"].as_u64().unwrap(),
                    p["refresh_millihz"].as_u64().unwrap(),
                );
                self.domain_revision += 1;
                self.borrow_sequence += 1;
                let new_borrow = format!("{:064x}", self.borrow_sequence);
                self.borrows.insert(reference.into(), new_borrow.clone());
                let completed = json!({"ok":true,"state":"succeeded","operation_id":operation,"borrow_id":new_borrow,"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":self.mode.0,"height":self.mode.1,"refresh_millihz":self.mode.2},"mutation_domain":domain(self.domain_revision,self.borrows.len())});
                self.reconfigurations.insert(operation.clone(), completed);
                json!({"ok":true,"state":"pending","operation_id":operation})
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
            let response = self
                .broker
                .borrow_mut()
                .dispatch(caller, Command::TakeResult { request: id });
            match response {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch(caller, Command::ReleaseResult { request: id }),
                        Response::Ok
                    ));
                    return payload;
                }
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
        let evidence = json!({"values":self.values,"collections":self.collections,"native_calls":self.native_calls});
        std::fs::write(
            self.root.join("simulated-backend.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        // The library must remain loaded until its module instance is destroyed.
        unsafe { (self.display.api.destroy)(self.display.instance) };
        self.display.instance = std::ptr::null_mut();
        // SAFETY: the plugin instance has been destroyed and no callbacks remain.
        unsafe { dlclose(self.library) };
        self.library = std::ptr::null_mut();
        if !self.lifecycle_library.is_null() {
            // SAFETY: lifecycle calls have stopped and its instance is destroyed before unloading.
            unsafe { (self.lifecycle.api.destroy)(self.lifecycle.instance) };
            self.lifecycle.instance = std::ptr::null_mut();
            unsafe { dlclose(self.lifecycle_library) };
            self.lifecycle_library = std::ptr::null_mut();
        }
        eprintln!(
            "cross-ABI test evidence retained at {}",
            self.root.display()
        );
    }
}
fn display_round_trip(resizes: usize) {
    let changed_mode = resizes > 0;
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let prepared =
        f.call(json!({"operation":"prepare","operation_id":"prepare","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let ready = f.settled("prepare");
    assert_eq!(
        ready["status"], "succeeded",
        "{ready}; virtual calls: {:?}",
        f.native_calls
    );
    let stream=f.call(json!({"operation":"prepare_stream","operation_id":"stream","target":ready["target"],"intent":"initial","requested_video":video,"consumer_requirements":["capture"]}));
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
    let started =
        f.call(json!({"operation":"start_stream","operation_id":"start","target":target}));
    assert_eq!(started["status"], "succeeded", "{started}");
    let pause = f.call(json!({"operation":"pause_stream","operation_id":"pause","target":target}));
    assert_eq!(pause["status"], "pending", "{pause}");
    assert_eq!(pause["fenced"], true);
    let retired=f.call_as("capture",json!({"operation":"retire_stream","operation_id":"retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let paused = f.settled("pause");
    assert_eq!(
        paused["status"], "succeeded",
        "{paused}; native calls: {:?}",
        f.native_calls
    );
    assert_eq!(f.references.len(), 1, "pause retains the session reference");
    assert_eq!(f.borrows.len(), 1, "pause retires only the stream borrow");
    let resume_video = if changed_mode {
        json!({"width":1280,"height":720,"refresh_millihz":60000,"hdr":false})
    } else {
        video.clone()
    };
    let resume=f.call(json!({"operation":"resume_stream","operation_id":"resume","target":ready["target"],"intent":"resume","requested_video":resume_video,"consumer_requirements":["capture"]}));
    assert_eq!(resume["status"], "pending", "{resume}");
    let resumed = f.settled("resume");
    assert_eq!(
        resumed["status"], "succeeded",
        "{resumed}; native calls: {:?}",
        f.native_calls
    );
    let mut resumed_target = resumed["target"].clone();
    assert_ne!(resumed_target["stream_id"], target["stream_id"]);
    let resumed_candidate = resumed["candidate"].clone();
    assert_eq!(resumed_candidate["mode"], resume_video);
    let mut resumed_observation = observed.clone();
    resumed_observation["width"] = resume_video["width"].clone();
    resumed_observation["height"] = resume_video["height"].clone();
    let resumed_ack=f.call_as("capture",json!({"operation":"ack_binding","operation_id":"resume-ack","target":resumed_target,"consumer":"capture","plan_revision":resumed_candidate["plan_revision"],"candidate":resumed_candidate,"observed_capture":resumed_observation}));
    assert_eq!(resumed_ack["status"], "pending", "{resumed_ack}");
    let mut result = Value::Null;
    for _ in 0..500 {
        result = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"resume-ack"}),
        );
        if result["status"] != "pending" {
            break;
        }
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    let restarted = f.call(
        json!({"operation":"start_stream","operation_id":"resume-start","target":resumed_target}),
    );
    assert_eq!(restarted["status"], "succeeded", "{restarted}");
    for cycle in 1..resizes {
        let pause_id = format!("cycle-{cycle}-pause");
        let fenced = f.call(
            json!({"operation":"pause_stream","operation_id":pause_id,"target":resumed_target}),
        );
        assert_eq!(fenced["status"], "pending", "{fenced}");
        let retired=f.call_as("capture",json!({"operation":"retire_stream","operation_id":format!("cycle-{cycle}-retire"),"target":resumed_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
        let paused = f.settled(&pause_id);
        assert_eq!(paused["status"], "succeeded", "{paused}");
        let video = if cycle % 2 == 1 {
            json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false})
        } else {
            json!({"width":1280,"height":720,"refresh_millihz":60000,"hdr":false})
        };
        let resume_id = format!("cycle-{cycle}-resume");
        let requested=f.call(json!({"operation":"resume_stream","operation_id":resume_id,"target":ready["target"],"intent":"resume","requested_video":video,"consumer_requirements":["capture"]}));
        assert_eq!(requested["status"], "pending", "{requested}");
        let prepared = f.settled(&resume_id);
        assert_eq!(
            prepared["status"], "succeeded",
            "{prepared}; native calls: {:?}",
            f.native_calls
        );
        let next_target = prepared["target"].clone();
        assert_ne!(next_target["stream_id"], resumed_target["stream_id"]);
        let candidate = prepared["candidate"].clone();
        assert_eq!(candidate["mode"], video);
        let mut observation = observed.clone();
        observation["width"] = video["width"].clone();
        observation["height"] = video["height"].clone();
        let ack_id = format!("cycle-{cycle}-ack");
        let ack=f.call_as("capture",json!({"operation":"ack_binding","operation_id":ack_id,"target":next_target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observation}));
        assert_eq!(ack["status"], "pending", "{ack}");
        let mut ack_result = Value::Null;
        for _ in 0..500 {
            ack_result = f.call_as(
                "capture",
                json!({"operation":"lookup","operation_id":ack_id}),
            );
            if ack_result["status"] != "pending" {
                break;
            }
        }
        assert_eq!(ack_result["status"], "succeeded", "{ack_result}");
        let started=f.call(json!({"operation":"start_stream","operation_id":format!("cycle-{cycle}-start"),"target":next_target}));
        assert_eq!(started["status"], "succeeded", "{started}");
        resumed_target = next_target;
    }
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":ready["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    let retired=f.call_as("capture",json!({"operation":"retire_stream","operation_id":"resume-retire","target":resumed_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let app=f.call(json!({"operation":"application_retired","operation_id":"desktop-retired","target":ready["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(
        ended["status"], "succeeded",
        "{ended}; native calls: {:?}",
        f.native_calls
    );
    assert!(f.references.is_empty());
    assert!(f.borrows.is_empty());
    assert_eq!(f.reconfigurations.len(), resizes);
}

#[test]
#[ignore = "requires an independently built display module and its example approvals"]
fn real_display_and_lifecycle_round_trip_with_simulated_backends() {
    display_round_trip(0);
}

#[test]
#[ignore = "requires a display module with native resize support and its example approvals"]
fn real_display_resumes_with_changed_mode() {
    display_round_trip(1);
}

#[test]
#[ignore = "requires a display module with native resize support and its example approvals"]
fn real_display_repeats_changed_mode_beyond_checkpoint() {
    display_round_trip(10);
}

#[test]
#[ignore = "requires an independently built display module and its example approvals"]
fn real_display_releases_session_before_any_stream() {
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let prepared = f.call(json!({"operation":"prepare","operation_id":"prepare","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let ready = f.settled("prepare");
    assert_eq!(ready["status"], "succeeded", "{ready}");
    assert_eq!(f.references.len(), 1);
    assert_eq!(f.borrows.len(), 1);
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":ready["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    let app=f.call(json!({"operation":"application_retired","operation_id":"desktop-retired","target":ready["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(
        ended["status"], "succeeded",
        "{ended}; native calls: {:?}",
        f.native_calls
    );
    assert!(f.references.is_empty());
    assert!(f.borrows.is_empty());
}
