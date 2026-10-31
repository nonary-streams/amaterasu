//! Opt-in cross-ABI integration with independently built composition participants.
//!
//! Set `AMATERASU_{LIFECYCLE,DISPLAY,SCREEN,STATE}_PLUGIN` to matching trusted
//! cdylibs and `AMATERASU_COMPOSITION_CONFIG` to their approved broker policy.
//! The policy needs the exact v2 participant approvals, schemas, control and
//! finalize policies, owner host-state grant, and closure/baseline operations.
//! A private host-state root and journal are created for each run. Native display
//! and capture observations are simulated; the broker and state module are real.
//!
//! The production coordinator currently gates dependent profiles. This ignored
//! test may run against an isolated qualification build; the gate remains in
//! place until the full flow and bounded retirement are qualified. It reports
//! logical namespace/collection counts and checks full-record growth after
//! warmup. Run `cargo test -p amaterasu-session-lifecycle --test
//! real_composition_cycles -- --ignored --nocapture` with matching artifacts.
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

struct AdditionalModule {
    module: RunningModule,
    library: *mut c_void,
}

impl Drop for AdditionalModule {
    fn drop(&mut self) {
        // SAFETY: the instance belongs to this loaded module and no more ticks run.
        unsafe { (self.module.api.destroy)(self.module.instance) };
        self.module.instance = std::ptr::null_mut();
        // SAFETY: its instance has been destroyed before the library is unloaded.
        unsafe { dlclose(self.library) };
    }
}
struct Fixture {
    broker: Rc<RefCell<Broker>>,
    lifecycle: RunningModule,
    display: Option<RunningModule>,
    screen: RunningModule,
    display_library: *mut c_void,
    screen_library: *mut c_void,
    trace: Rc<RefCell<Vec<Value>>>,
    additional: Vec<AdditionalModule>,
    lifecycle_library: *mut c_void,
    root: PathBuf,
    next: u64,
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
        {
            let (caller, service) = ("virtual", "amaterasu.virtual-display/1");
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
        let (state_library, state_api) = load_plugin("AMATERASU_STATE_PLUGIN");
        let additional = vec![AdditionalModule {
            module: RunningModule::new(state_api, broker.clone(), "amaterasu.state", trace.clone()),
            library: state_library,
        }];
        Self {
            broker,
            lifecycle,
            display,
            screen,
            display_library,
            screen_library,
            trace,
            additional,
            lifecycle_library,
            root,
            next: 0,
            native_calls: Vec::new(),
            references: BTreeMap::new(),
            borrows: BTreeMap::new(),
            domain_revision: 0,
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
        for module in &self.additional {
            module.module.tick();
        }
        {
            let principal = "virtual";
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
                let _ = caller;
                let answer = self.native(&payload);
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
                    self.trace
                        .borrow()
                        .iter()
                        .any(|event| event["principal"] == SCREEN
                            && event["command"]["payload"]["action"] == "collection_commit"),
                    "effect must follow a durable reservation request"
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
        let evidence = json!({"native_calls":self.native_calls,"trace":*self.trace.borrow()});
        std::fs::write(
            self.root.join("simulated-backend.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        self.additional.clear();
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

#[derive(Default, Debug)]
struct StateUsage {
    files: usize,
    bytes: u64,
    namespace_keys: usize,
    tombstone_keys: usize,
    collection_records: usize,
    max_record_bytes: usize,
}
fn state_disk_usage(path: &std::path::Path) -> StateUsage {
    let mut result = StateUsage::default();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                let child = state_disk_usage(&entry.path());
                result.files += child.files;
                result.bytes += child.bytes;
                result.namespace_keys += child.namespace_keys;
                result.tombstone_keys += child.tombstone_keys;
                result.collection_records += child.collection_records;
                result.max_record_bytes = result.max_record_bytes.max(child.max_record_bytes);
            } else {
                result.files += 1;
                result.bytes += metadata.len();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "state.json" || name == "manifest.json" || name.starts_with("shard-") {
                    let value: Value =
                        serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
                    if name == "manifest.json" {
                        result.collection_records += value["count"].as_u64().unwrap() as usize;
                    } else {
                        let entries = if name == "state.json" {
                            &value["values"]
                        } else {
                            &value
                        };
                        let entries = entries.as_object().unwrap();
                        if name == "state.json" {
                            result.namespace_keys += entries.len();
                            result.tombstone_keys += entries
                                .keys()
                                .filter(|key| key.starts_with("virtual-screen/gc-tombstone/"))
                                .count();
                        }
                        for value in entries.values() {
                            result.max_record_bytes = result
                                .max_record_bytes
                                .max(serde_json::to_vec(value).unwrap().len());
                        }
                    }
                }
            }
        }
    }
    result
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

#[test]
#[ignore = "requires both independently built v2 plugins, matching lifecycle artifact and approved composition configuration"]
fn real_participants_prepare_resume_and_release_in_dependency_order() {
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
    let mut target = start_capture(&mut f, &stream, &initial, "initial");
    let mut warmup_full_records = None;
    for cycle in 0..10 {
        let pause_id = format!("pause-{cycle}");
        let paused =
            f.call(json!({"operation":"pause_stream","operation_id":pause_id,"target":target}));
        assert_eq!(paused["status"], "pending", "{paused}");
        let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":format!("retire-{cycle}"),"target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
        let paused = f.settled(&pause_id);
        assert_eq!(paused["status"], "succeeded", "{paused}");
        assert_eq!(
            f.mode,
            (1920, 1080, 60000),
            "pause must restore baseline mode"
        );
        let video = if cycle % 2 == 0 {
            json!({"width":1280,"height":720,"refresh_millihz":90000,"hdr":false})
        } else {
            initial.clone()
        };
        let resume_id = format!("resume-{cycle}");
        let resume = f.call(json!({"operation":"resume_stream","operation_id":resume_id,"target":session["target"],"intent":"resume","requested_video":video,"consumer_requirements":["capture"]}));
        assert_eq!(resume["status"], "pending", "{resume}");
        let resumed = f.settled(&resume_id);
        assert_eq!(resumed["status"], "succeeded", "{resumed}");
        assert_eq!(resumed["candidate"]["mode"], video);
        target = start_capture(&mut f, &resumed, &video, &resume_id);
        assert_eq!(
            f.mode,
            (
                video["width"].as_u64().unwrap(),
                video["height"].as_u64().unwrap(),
                video["refresh_millihz"].as_u64().unwrap()
            ),
            "resume must configure requested mode"
        );
        let usage = state_disk_usage(&f.root.join("owner-state/namespaces"));
        eprintln!("qualification cycle {cycle}: state_usage={usage:?}");
        assert!(usage.tombstone_keys <= 8192, "tombstone capacity exceeded");
        let full_records = (
            usage.namespace_keys - usage.tombstone_keys,
            usage.collection_records,
        );
        if cycle == 2 {
            warmup_full_records = Some(full_records);
        } else if cycle > 2 {
            let warmup = warmup_full_records.expect("warmup measurement");
            assert!(
                full_records.0 <= warmup.0 && full_records.1 <= warmup.1,
                "full records grew after warmup: {warmup:?} -> {full_records:?}"
            );
        }
    }
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":session["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"final-retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let app = f.call(json!({"operation":"application_retired","operation_id":"app-retired","target":session["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(f.references.is_empty());
    assert!(f.borrows.is_empty());
    // Keep the same broker, state and native topology domain for a fresh session.
    let second = f.call(json!({"operation":"prepare","operation_id":"second-session","requested_video":initial,"application_ref":{"kind":"desktop"}}));
    assert_eq!(second["status"], "pending", "{second}");
    let second = f.settled("second-session");
    assert_eq!(second["status"], "succeeded", "{second}");
    assert_ne!(
        second["target"]["session_id"],
        session["target"]["session_id"]
    );
    let next = f.call(json!({"operation":"prepare_stream","operation_id":"second-stream","target":second["target"],"intent":"initial","requested_video":initial,"consumer_requirements":["capture"]}));
    assert_eq!(next["status"], "pending", "{next}");
    let next = f.settled("second-stream");
    assert_eq!(next["status"], "succeeded", "{next}");
    let next_target = start_capture(&mut f, &next, &initial, "second");
    let end = f.call(
        json!({"operation":"end_session","operation_id":"second-end","target":second["target"]}),
    );
    assert_eq!(end["status"], "pending", "{end}");
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"second-retire","target":next_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let app = f.call(json!({"operation":"application_retired","operation_id":"second-app-retired","target":second["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("second-end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(f.references.is_empty(), "second session leaked references");
    assert!(f.borrows.is_empty(), "second session leaked borrows");
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
