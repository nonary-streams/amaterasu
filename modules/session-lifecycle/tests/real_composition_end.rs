//! Opt-in cross-ABI integration with independently built composition participants.
//!
//! Set `AMATERASU_{LIFECYCLE,DISPLAY,SCREEN}_PLUGIN` to matching trusted cdylibs
//! and `AMATERASU_COMPOSITION_CONFIG` to their approved broker configuration.
//! The configuration must select the two v2 providers with display settings
//! depending on the virtual-screen owner, include their exact method approvals,
//! target/control schemas and control/finalize policies, and grant the owner
//! `lookup_control_baseline` and `lookup_stream_closure`. Transport and capture
//! principals need the lifecycle operations used below. The supplied libraries
//! must match that configuration and be trusted native code.
//!
//! Run with `cargo test -p amaterasu-session-lifecycle --test
//! real_composition_end -- --ignored`. The selected coordinator must support
//! the configured dependent profile.
//! The fixture replaces the configured lifecycle journal path with a private
//! temporary path; native display and state services are deterministic fakes.
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
const DISPLAY_SERVICE: &str = "nonary.display-settings/2";
const SCREEN: &str = "nonary.virtual-screen";
const SCREEN_SERVICE: &str = "nonary.virtual-screen/2";
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

fn read_configuration(variable: &str) -> Value {
    let path = std::env::var_os(variable).expect(variable);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
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
    display: Option<RunningModule>,
    screen: RunningModule,
    display_library: *mut c_void,
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
    reconfigurations: BTreeMap<String, Value>,
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
        let state_root = root.join("owner-state");
        std::fs::create_dir(&state_root).unwrap();
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut configuration = read_configuration("AMATERASU_COMPOSITION_CONFIG");
        configuration["settings"][COORDINATOR]["journal_path"] = json!(root.join("journal.json"));
        configuration["host_context"] = json!({"schema":1,"state_root":state_root});
        let participants = configuration["settings"][COORDINATOR]["participants"]
            .as_array()
            .expect("participants");
        let owner = participants
            .iter()
            .find(|p| p["provider"] == SCREEN)
            .expect("screen participant");
        assert_eq!(owner["plan_method"]["service"], SCREEN_SERVICE);
        {
            let dependent = participants
                .iter()
                .find(|p| p["provider"] == DISPLAY)
                .expect("settings participant");
            assert_eq!(dependent["plan_method"]["service"], DISPLAY_SERVICE);
            assert!(
                dependent["depends_on"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p == SCREEN)
            );
        }
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
        let (display_library, display_api) = load_plugin("AMATERASU_DISPLAY_PLUGIN");
        let display = Some(RunningModule::new(
            display_api,
            broker.clone(),
            DISPLAY,
            trace.clone(),
        ));
        let (screen_library, screen_api) = load_plugin("AMATERASU_SCREEN_PLUGIN");
        let (lifecycle_library, lifecycle_api) = load_plugin("AMATERASU_LIFECYCLE_PLUGIN");
        let screen = RunningModule::new(screen_api, broker.clone(), SCREEN, trace.clone());
        let lifecycle =
            RunningModule::new(lifecycle_api, broker.clone(), COORDINATOR, trace.clone());
        Self {
            broker,
            lifecycle,
            display,
            screen,
            display_library,
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
            reconfigurations: BTreeMap::new(),
        }
    }
    fn tick(&mut self) {
        self.lifecycle.tick();
        if let Some(display) = &self.display {
            display.tick();
        }
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
            "reconfigure" | "reconfigure_mode_only" => {
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
                let readback = json!({"id":"Virtual-1","enabled":true,"x":0,"y":0,"scale":1.0,"rotation":1,"priority":1,"hdr":false,"wcg":false,"vrr":"always","mode":null});
                use sha2::{Digest, Sha256};
                let digest = format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&readback).unwrap())
                );
                let completed = json!({"ok":true,"state":"succeeded","operation_id":operation,"borrow_id":new_borrow,"observed_output":{"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":self.mode.0,"height":self.mode.1,"refresh_millihz":self.mode.2},"mutation_domain":domain(self.domain_revision,self.borrows.len()),"mode_only_preserved":true,"preservation_before_digest":digest,"preservation_after_digest":digest,"preservation_readback":readback});
                self.reconfigurations.insert(operation.clone(), completed);
                json!({"ok":true,"state":"pending","operation_id":operation})
            }
            "reconfigure_status" | "reconfigure_mode_only_status" => self
                .reconfigurations
                .get(p["operation_id"].as_str().unwrap())
                .cloned()
                .unwrap_or_else(|| json!({"ok":false,"error":"operation_not_found"})),
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
        for _ in 0..5000 {
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
        // The library must remain loaded until its module instance is destroyed.
        if let Some(display) = &mut self.display {
            unsafe { (display.api.destroy)(display.instance) };
            display.instance = std::ptr::null_mut();
            // SAFETY: the plugin instance has been destroyed and no callbacks remain.
            unsafe { dlclose(self.display_library) };
            self.display_library = std::ptr::null_mut();
        }
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

fn observation(video: &Value) -> Value {
    json!({"version":1,"protocol":"wayland-wl-output","compositor_id":"b".repeat(64),"registry_global":4,"output_name":"Virtual-1","width":video["width"],"height":video["height"],"refresh_millihz":video["refresh_millihz"]})
}

fn start_capture(f: &mut Fixture, result: &Value, video: &Value, prefix: &str) -> Value {
    let target = result["target"].clone();
    let candidate = result["candidate"].clone();
    let ack_id = format!("{prefix}-ack");
    let ack = f.call_as("capture", json!({"operation":"ack_binding","operation_id":ack_id,"target":target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observation(video)}));
    assert_eq!(ack["status"], "pending", "{ack}");
    let mut acknowledged = Value::Null;
    for _ in 0..500 {
        acknowledged = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":ack_id}),
        );
        if acknowledged["status"] != "pending" {
            break;
        }
    }
    assert_eq!(acknowledged["status"], "succeeded", "{acknowledged}");
    let started = f.call(json!({"operation":"start_stream","operation_id":format!("{prefix}-start"),"target":target}));
    assert_eq!(started["status"], "succeeded", "{started}");
    target
}

fn run_real_participants_end(paused: bool) {
    let mut f = Fixture::new();
    let initial = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let prepared = f.call(json!({"operation":"prepare","operation_id":"session","requested_video":initial,"application_ref":{"kind":"desktop"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let session = f.settled("session");
    assert_eq!(session["status"], "succeeded", "{session}");
    let stream = f.call(json!({"operation":"prepare_stream","operation_id":"stream","target":session["target"],"intent":"initial","requested_video":initial,"consumer_requirements":["capture"]}));
    assert_eq!(stream["status"], "pending", "{stream}");
    let stream = f.settled("stream");
    assert_eq!(stream["status"], "succeeded", "{stream}");
    assert_eq!(stream["candidate"]["resource"]["provider"], SCREEN);
    let target = start_capture(&mut f, &stream, &initial, "initial");
    if paused {
        let pause =
            f.call(json!({"operation":"pause_stream","operation_id":"pause","target":target}));
        assert_eq!(pause["status"], "pending", "{pause}");
        let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"pause-retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
        let pause = f.settled("pause");
        assert_eq!(pause["status"], "succeeded", "{pause}");
    }
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":session["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    if !paused {
        let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"final-retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
    }
    let app = f.call(json!({"operation":"application_retired","operation_id":"app-retired","target":session["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(f.references.is_empty());
    assert!(f.borrows.is_empty());
    let trace = f.trace.borrow();
    let calls: Vec<_> = trace
        .iter()
        .filter_map(|e| e.get("command"))
        .filter(|c| c["op"] == "call_api")
        .collect();
    let phase = |call: &&Value, service: &str, name: &str| {
        call["service"] == service
            && call["method"] == "lifecycle_callback"
            && call["payload"]["phase"] == name
    };
    let owner_prepare = calls
        .iter()
        .position(|c| phase(c, SCREEN_SERVICE, "stream.prepare"))
        .expect("owner prepares");
    let settings_prepare = calls
        .iter()
        .position(|c| phase(c, DISPLAY_SERVICE, "stream.prepare"))
        .expect("settings prepares");
    let settings_release = calls
        .iter()
        .rposition(|c| phase(c, DISPLAY_SERVICE, "stream.release"))
        .expect("settings releases");
    let owner_release = calls
        .iter()
        .rposition(|c| phase(c, SCREEN_SERVICE, "stream.release"))
        .expect("owner releases");
    assert!(
        owner_prepare < settings_prepare && settings_release < owner_release,
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c["service"] == SCREEN_SERVICE && c["method"] == "resource_control"),
        "no owner-mediated configuration"
    );
    let proof = trace
        .iter()
        .filter(|event| event["principal"] == SCREEN)
        .find_map(|event| {
            let proof = &event["command"]["payload"]["output"]["data"]["preservation_proof"];
            (!proof.is_null()).then_some(proof)
        })
        .expect("owner did not report preservation proof");
    assert_eq!(proof["kind"], "mode_only_preserved");
    assert_eq!(proof["before_digest"], proof["after_digest"]);
}

#[test]
#[ignore = "requires matching real lifecycle, display settings, virtual screen and approved configuration"]
fn real_participants_end_active_stream() {
    run_real_participants_end(false);
}

#[test]
#[ignore = "requires matching real lifecycle, display settings, virtual screen and approved configuration"]
fn real_participants_end_paused_stream() {
    run_real_participants_end(true);
}

#[test]
#[ignore = "requires a lock-enabled owner and broker host-state grant"]
fn real_owner_holds_lifetime_lock() {
    let fixture = Fixture::new();
    let path = fixture
        .root
        .join("owner-state/nonary.virtual-screen/owner.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("owner did not create its lock file");
    assert!(lock.try_lock().is_err(), "owner lifetime lock is not held");
    drop(fixture);
    lock.try_lock()
        .expect("owner lock was not released after destroy");
}
