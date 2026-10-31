//! Opt-in session and stream preparation against a real virtual-screen provider.
//!
//! Supply matching trusted `AMATERASU_LIFECYCLE_PLUGIN` and
//! `AMATERASU_SCREEN_PLUGIN` cdylibs, `AMATERASU_OWNER_EXAMPLES`, and
//! `AMATERASU_DISPLAY_TARGET_SCHEMA`. Native display and state are simulated.
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
const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const SCREEN: &str = "nonary.virtual-screen";
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Endpoint {
    broker: Rc<RefCell<Broker>>,
    principal: &'static str,
    trace: Rc<RefCell<Vec<Value>>>,
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
    if matches!(
        &command,
        Command::CallApi { .. }
            | Command::Complete { .. }
            | Command::Request { .. }
            | Command::QueryApiApproval { .. }
    ) {
        endpoint
            .trace
            .borrow_mut()
            .push(json!({"principal": endpoint.principal, "command": command}));
    }
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    if matches!(&response, Response::Error { .. }) {
        endpoint
            .trace
            .borrow_mut()
            .push(json!({"principal":endpoint.principal,"broker_error":response}));
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
    fn new(
        api: abi::ModuleApi,
        broker: Rc<RefCell<Broker>>,
        principal: &'static str,
        trace: Rc<RefCell<Vec<Value>>>,
    ) -> Self {
        let mut endpoint = Box::new(Endpoint {
            broker,
            principal,
            trace,
        });
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
fn load_plugin(variable: &str) -> (*mut c_void, abi::ModuleApi) {
    let path = CString::new(std::env::var(variable).expect(variable)).unwrap();
    // SAFETY: opt-in artifacts are trusted and remain loaded until their instances are destroyed.
    let library = unsafe { dlopen(path.as_ptr(), 2) };
    assert!(!library.is_null(), "dlopen failed for {variable}");
    let symbol = CString::new("amaterasu_module_v1").unwrap();
    // SAFETY: library is loaded and the symbol name is NUL terminated.
    let address = unsafe { dlsym(library, symbol.as_ptr()) };
    assert!(!address.is_null(), "entry point missing for {variable}");
    // SAFETY: the symbol is the documented module ABI entry point.
    let entry: abi::ModuleEntry = unsafe { std::mem::transmute(address) };
    // SAFETY: documented entry point returns a valid module API table.
    (library, unsafe { *entry() })
}

fn owner_configuration() -> Value {
    let directory =
        PathBuf::from(std::env::var_os("AMATERASU_OWNER_EXAMPLES").expect("owner examples"));
    let approvals: Value = serde_json::from_slice(
        &std::fs::read(directory.join("lifecycle-api-approvals.json")).unwrap(),
    )
    .unwrap();
    let descriptor: Value =
        serde_json::from_slice(&std::fs::read(directory.join("settings-descriptor.json")).unwrap())
            .unwrap();
    let mut settings = serde_json::Map::new();
    for option in descriptor["options"].as_array().unwrap() {
        settings.insert(
            option["id"].as_str().unwrap().into(),
            option["definition"]["default"].clone(),
        );
    }
    settings.insert("enabled".into(), json!(true));
    let method = |name| json!({"service":"nonary.virtual-screen/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
    let mut configuration = json!({"principals":{
        COORDINATOR:{"provide":[LIFECYCLE],"call":["nonary.virtual-screen/2",LIFECYCLE,"amaterasu.state/1","amaterasu.virtual-display/1"]},
        SCREEN:{"provide":["nonary.virtual-screen/2"],"call":[LIFECYCLE,"amaterasu.state/1","amaterasu.virtual-display/1"]},
        "transport":{"call":[LIFECYCLE]}, "capture":{"call":[LIFECYCLE]},
        "amaterasu.extension-registry":{"call":["nonary.virtual-screen/2"]},
        "amaterasu.extension-actions":{"call":["nonary.virtual-screen/2"]},
        "amaterasu.management":{"call":["nonary.virtual-screen/2"]},
        "virtual":{"provide":["amaterasu.virtual-display/1"]}, "state":{"provide":["amaterasu.state/1"]}
    },"api_methods":approvals["api_methods"],"settings":{COORDINATOR:{
        "journal_path":"/replaced","trusted_callers":["transport","capture",SCREEN],
        "session_delegates":["capture"],"consumers":{"capture":["capture"]},
        "operation_grants":{"transport":["prepare","prepare_stream","lookup","start_stream","end_session","application_retired"],"capture":["ack_binding","lookup","retire_stream"],SCREEN:["verify_provider_epoch","describe_resource_finalize","verify_resource_finalize","validate_resource_finalize","verify_finalize_stage","verify_finalize_result","refresh_provider_observations"]},
        "consumer_requirements":{"execution_domain":"desktop","capture_providers":["amaterasu.capture-pipewire/1"],"selector_schemas":["amaterasu.kwin-wl-output/1"]},
        "participants":[{"provider":SCREEN,"contract_version":2,"plan_method":method("resource_plan"),"callback_method":method("lifecycle_callback"),"status_method":method("lifecycle_status"),"settings_revision":"one","settings":settings,"capabilities":[],"cleanup_methods":[method("lifecycle_callback")]}]
    }}});
    let schema_path =
        std::env::var_os("AMATERASU_DISPLAY_TARGET_SCHEMA").expect("display target schema");
    let document = std::fs::read_to_string(schema_path).unwrap();
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(document.as_bytes()));
    let schema = json!({"id":"amaterasu.display-target/1","major":1,"minor":0,"digest":digest});
    let lifecycle = &mut configuration["settings"][COORDINATOR];
    if approvals["api_methods"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["method"]["method"] == "binding_validate")
    {
        lifecycle["participants"][0]["binding_validate_method"] = method("binding_validate");
    }
    lifecycle["schema_documents"] = json!([{"reference":schema,"document":document}]);
    lifecycle["finalize_policies"] = json!([{"owner_provider":SCREEN,"owner_service":"nonary.virtual-screen/2","method":method("resource_finalize"),"status_method":method("lifecycle_status"),"output_capability":"amaterasu.display-target","output_schema":schema,"capture_provider":"amaterasu.capture-pipewire/1","selector_schema":"amaterasu.kwin-wl-output/1","required_access":"exclusive","domain_scope":"resource","mutation_domains":[]}]);
    configuration
}

#[derive(Default, serde::Serialize)]
struct Collection {
    revision: u64,
    entries: BTreeMap<String, Value>,
    metadata: Value,
}
#[derive(Default, serde::Serialize)]
struct Store {
    revision: u64,
    values: BTreeMap<String, Value>,
}
struct Fixture {
    broker: Rc<RefCell<Broker>>,
    lifecycle: RunningModule,
    screen: RunningModule,
    screen_library: *mut c_void,
    trace: Rc<RefCell<Vec<Value>>>,
    lifecycle_library: *mut c_void,
    root: PathBuf,
    next: u64,
    stores: BTreeMap<String, Store>,
    collections: BTreeMap<String, Collection>,
    native_calls: Vec<Value>,
    references: BTreeMap<String, String>,
    borrows: BTreeMap<String, String>,
    domain_revision: u64,
    borrow_sequence: u32,
    mode: (u64, u64, u64),
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-composition-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut configuration = owner_configuration();
        configuration["settings"][COORDINATOR]["journal_path"] = json!(root.join("journal.json"));
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
        let trace = Rc::new(RefCell::new(Vec::new()));
        let (screen_library, screen_api) = load_plugin("AMATERASU_SCREEN_PLUGIN");
        let (lifecycle_library, lifecycle_api) = load_plugin("AMATERASU_LIFECYCLE_PLUGIN");
        let screen = RunningModule::new(screen_api, broker.clone(), SCREEN, trace.clone());
        let lifecycle =
            RunningModule::new(lifecycle_api, broker.clone(), COORDINATOR, trace.clone());
        Self {
            broker,
            lifecycle,
            screen,
            screen_library,
            trace,
            lifecycle_library,
            root,
            next: 0,
            stores: BTreeMap::new(),
            collections: BTreeMap::new(),
            native_calls: Vec::new(),
            references: BTreeMap::new(),
            borrows: BTreeMap::new(),
            domain_revision: 1,
            borrow_sequence: 0,
            mode: (1920, 1080, 60000),
        }
    }
    fn tick(&mut self) {
        self.lifecycle.tick();
        self.screen.tick();
        for principal in ["virtual", "state"] {
            let response = self
                .broker
                .borrow_mut()
                .dispatch(principal, Command::Poll {});
            if let Response::Request {
                id,
                caller,
                payload,
                ..
            } = response
            {
                let answer = if principal == "state" {
                    self.state(&caller, &payload)
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
    fn state(&mut self, caller: &str, p: &Value) -> Value {
        let store = self.stores.entry(caller.to_owned()).or_default();
        match p["action"].as_str().unwrap() {
            "get" => {
                let value = store.values.get(p["key"].as_str().unwrap());
                json!({"ok":true,"revision":store.revision,"found":value.is_some(),"value":value})
            }
            "set" => {
                assert_eq!(p["expected_revision"], store.revision);
                store
                    .values
                    .insert(p["key"].as_str().unwrap().into(), p["value"].clone());
                store.revision += 1;
                json!({"ok":true,"revision":store.revision})
            }
            "collection_get" => {
                let collection = self
                    .collections
                    .entry(format!("{caller}/{}", p["collection"].as_str().unwrap()))
                    .or_default();
                json!({"ok":true,"revision":collection.revision,"found":collection.revision!=0,"metadata":collection.metadata,"count":collection.entries.len(),"value":collection.entries.get(p["key"].as_str().unwrap())})
            }
            "collection_commit" => {
                let collection = self
                    .collections
                    .entry(format!("{caller}/{}", p["collection"].as_str().unwrap()))
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
        self.trace.borrow_mut().push(json!({"native":p}));
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
                json!({"ok":true,"enabled":true,"native_supported":true,"driver":"libvirtualdisplay","ownership_authority":"unix_uid_broker","recovery_ready":true,"reference_owned":reference_owned,"monitors":monitors,"mutation_domain":domain(self.domain_revision,self.borrows.len()),"mode_only_preservation":"kscreen-strict-v1","settings":{"virtual_display_layout":"extended","dd_virtual_display_scale":0}})
            }
            "create" => {
                assert!(
                    self.collections
                        .iter()
                        .any(|(name, c)| name.starts_with(SCREEN) && c.metadata["pending"] == 1),
                    "effect must follow durable reservation"
                );
                assert!(
                    self.stores
                        .get(SCREEN)
                        .is_some_and(|s| !s.values.is_empty()),
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
            "remove" => {
                let reference = p["reference_id"].as_str().unwrap();
                assert!(!self.borrows.contains_key(reference));
                assert!(self.references.remove(reference).is_some());
                let present = self
                    .references
                    .values()
                    .any(|id| id == p["identity"].as_str().unwrap());
                json!({"ok":true,"removed":!present,"identity":p["identity"],"reference_released":true,"reference_owned":false})
            }
            action => panic!("unexpected virtual action {action}: {p}"),
        }
    }
    fn begin_as(&mut self, caller: &str, service: &str, payload: Value) -> u64 {
        self.next += 1;
        let response = self.broker.borrow_mut().dispatch(
            caller,
            Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 5000,
                operation_id: format!("test-{}", self.next),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("request admission: {response:?}")
        };
        id
    }
    fn request_as(&mut self, caller: &str, service: &str, payload: Value) -> Value {
        let id = self.begin_as(caller, service, payload);
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
    fn call_as(&mut self, caller: &str, payload: Value) -> Value {
        self.request_as(caller, LIFECYCLE, payload)
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
        let evidence = json!({"stores":self.stores,"collections":self.collections,"native_calls":self.native_calls,"trace":*self.trace.borrow()});
        std::fs::write(
            self.root.join("simulated-backend.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        unsafe { (self.screen.api.destroy)(self.screen.instance) };
        self.screen.instance = std::ptr::null_mut();
        unsafe { dlclose(self.screen_library) };
        self.screen_library = std::ptr::null_mut();
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

#[test]
#[ignore = "requires a session-prepare-capable owner, matching lifecycle and approved owner configuration"]
fn real_owner_prepares_session_with_simulated_native_and_state() {
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let pending = f.call(json!({"operation":"prepare","operation_id":"owner-session","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(pending["status"], "pending", "{pending}");
    let result = f.settled("owner-session");
    assert_eq!(
        result["status"],
        "succeeded",
        "{result}; trace={:?}; native={:?}",
        f.trace.borrow(),
        f.native_calls
    );
    assert!(f.native_calls.iter().any(|call| call["action"] == "create"));
    assert!(f.native_calls.iter().any(|call| call["action"] == "borrow"));
    assert!(f.trace.borrow().iter().any(|event| {
        event["principal"] == SCREEN
            && event["command"]["op"] == "complete"
            && event["command"]["payload"]["status"] == "prepared"
            && event["command"]["payload"]["outputs"]
                .as_array()
                .is_some_and(|outputs| !outputs.is_empty())
    }));
}

#[test]
#[ignore = "requires a stream-capable owner and lifecycle with finalize stage support"]
fn real_owner_finalizes_stream_and_validates_capture_binding() {
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let pending = f.call(json!({"operation":"prepare","operation_id":"owner-session","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(pending["status"], "pending", "{pending}");
    let session = f.settled("owner-session");
    assert_eq!(session["status"], "succeeded", "{session}");
    let requested = f.call(json!({"operation":"prepare_stream","operation_id":"owner-stream","target":session["target"],"intent":"initial","requested_video":video,"consumer_requirements":["capture"]}));
    assert_eq!(requested["status"], "pending", "{requested}");
    let stream = f.settled("owner-stream");
    assert_eq!(
        stream["status"],
        "succeeded",
        "{stream}; native={:?}; trace={:?}",
        f.native_calls,
        f.trace.borrow()
    );
    let target = stream["target"].clone();
    let candidate = stream["candidate"].clone();
    assert_eq!(candidate["resource"]["provider"], SCREEN);
    assert_eq!(candidate["mode"], video);
    let trace = f.trace.borrow();
    assert!(
        trace.iter().any(|event| event["principal"] == COORDINATOR
            && event["command"]["method"] == "resource_finalize"),
        "finalize not dispatched"
    );
    drop(trace);
    let observed = json!({"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000});
    let ack = f.call_as("capture", json!({"operation":"ack_binding","operation_id":"capture-ack","target":target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observed}));
    assert_eq!(ack["status"], "pending", "{ack}");
    let mut outcome = Value::Null;
    for _ in 0..500 {
        outcome = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"capture-ack"}),
        );
        if outcome["status"] != "pending" {
            break;
        }
    }
    assert_eq!(
        outcome["status"],
        "succeeded",
        "{outcome}; trace={:?}",
        f.trace.borrow()
    );
    assert!(
        f.trace
            .borrow()
            .iter()
            .any(|event| event["principal"] == COORDINATOR
                && event["command"]["method"] == "binding_validate"),
        "binding validator not dispatched"
    );
    let started =
        f.call(json!({"operation":"start_stream","operation_id":"start","target":target}));
    assert_eq!(started["status"], "succeeded", "{started}");
}

#[test]
#[ignore = "requires a cleanup-capable owner and lifecycle with stream/session release support"]
fn real_owner_releases_stream_then_session_after_capture_retirement() {
    let mut f = Fixture::new();
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let pending = f.call(json!({"operation":"prepare","operation_id":"owner-session","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(pending["status"], "pending", "{pending}");
    let session = f.settled("owner-session");
    assert_eq!(session["status"], "succeeded", "{session}");
    let requested = f.call(json!({"operation":"prepare_stream","operation_id":"owner-stream","target":session["target"],"intent":"initial","requested_video":video,"consumer_requirements":["capture"]}));
    assert_eq!(requested["status"], "pending", "{requested}");
    let stream = f.settled("owner-stream");
    assert_eq!(
        stream["status"],
        "succeeded",
        "{stream}; native={:?}; trace={:?}",
        f.native_calls,
        f.trace.borrow()
    );
    let target = stream["target"].clone();
    let candidate = stream["candidate"].clone();
    assert_eq!(candidate["resource"]["provider"], SCREEN);
    assert_eq!(candidate["mode"], video);
    let trace = f.trace.borrow();
    assert!(
        trace.iter().any(|event| event["principal"] == COORDINATOR
            && event["command"]["method"] == "resource_finalize"),
        "finalize not dispatched"
    );
    drop(trace);
    let observed = json!({"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":1920,"height":1080,"refresh_millihz":60000});
    let ack = f.call_as("capture", json!({"operation":"ack_binding","operation_id":"capture-ack","target":target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observed}));
    assert_eq!(ack["status"], "pending", "{ack}");
    let mut outcome = Value::Null;
    for _ in 0..500 {
        outcome = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"capture-ack"}),
        );
        if outcome["status"] != "pending" {
            break;
        }
    }
    assert_eq!(
        outcome["status"],
        "succeeded",
        "{outcome}; trace={:?}",
        f.trace.borrow()
    );
    assert!(
        f.trace
            .borrow()
            .iter()
            .any(|event| event["principal"] == COORDINATOR
                && event["command"]["method"] == "binding_validate"),
        "binding validator not dispatched"
    );
    let started =
        f.call(json!({"operation":"start_stream","operation_id":"start","target":target}));
    assert_eq!(started["status"], "succeeded", "{started}");
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":session["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"capture-retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let app = f.call(json!({"operation":"application_retired","operation_id":"app-retired","target":session["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(
        ended["status"],
        "succeeded",
        "{ended}; trace={:?}; native={:?}",
        f.trace.borrow(),
        f.native_calls
    );
    assert!(
        f.references.is_empty(),
        "owned references remain: {:?}",
        f.references
    );
    assert!(
        f.borrows.is_empty(),
        "native borrows remain: {:?}",
        f.borrows
    );
    let trace = f.trace.borrow();
    let release = |event: &&Value, phase: &str| {
        event["principal"] == COORDINATOR
            && event["command"]["method"] == "lifecycle_callback"
            && event["command"]["payload"]["phase"] == phase
    };
    let stream_release = trace
        .iter()
        .position(|event| release(&event, "stream.release"))
        .expect("stream release");
    let session_release = trace
        .iter()
        .position(|event| release(&event, "session.release"))
        .expect("session release");
    assert!(stream_release < session_release, "{trace:?}");
}
