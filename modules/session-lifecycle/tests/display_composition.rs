//! Opt-in cross-ABI integration with independently built composition participants.
//!
//! Set `AMATERASU_{LIFECYCLE,DISPLAY,SCREEN}_PLUGIN` to matching trusted cdylibs
//! and `AMATERASU_COMPOSITION_CONFIG` to their approved broker configuration.
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
    fault: Rc<RefCell<OutputFault>>,
}
#[derive(Default)]
struct OutputFault {
    kind: Option<&'static str>,
    injected: bool,
    drop_completion_result: bool,
    completion_request: Option<u64>,
    completion_result_dropped: bool,
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
    let Ok(mut command) = serde_json::from_slice::<Command>(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    if endpoint.principal == SCREEN
        && let Command::Complete { payload, .. } = &mut command
        && let Some(output) = payload
            .get_mut("outputs")
            .and_then(Value::as_array_mut)
            .and_then(|outputs| outputs.first_mut())
    {
        let mut fault = endpoint.fault.borrow_mut();
        match fault.kind {
            Some("foreign") => {
                output["resource"]["provider"] = json!("foreign.provider");
                fault.injected = true;
                fault.kind = None;
            }
            Some("stale") => {
                if let Some(revision) = output["resource"]["revision"].as_u64() {
                    output["resource"]["revision"] = json!(revision.saturating_add(1));
                    fault.injected = true;
                    fault.kind = None;
                }
            }
            _ => {}
        }
    }
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
    let completing = endpoint.principal == "amaterasu.extension-actions"
        && matches!(&command, Command::Request { service, payload, .. }
            if service == LIFECYCLE && payload["operation"] == "complete_action");
    let polling = match &command {
        Command::TakeResult { request } if endpoint.principal == "amaterasu.extension-actions" => {
            Some(*request)
        }
        _ => None,
    };
    let mut response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    {
        let mut fault = endpoint.fault.borrow_mut();
        if completing && let Response::Accepted { id } = &response {
            fault.completion_request = Some(*id);
        }
        if fault.drop_completion_result
            && polling == fault.completion_request
            && polling.is_some()
            && matches!(&response, Response::Result { .. })
        {
            fault.drop_completion_result = false;
            fault.completion_result_dropped = true;
            response = Response::Pending;
        }
    }
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
        fault: Rc<RefCell<OutputFault>>,
    ) -> Self {
        let mut endpoint = Box::new(Endpoint {
            broker,
            principal,
            trace,
            fault,
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
    fault: Rc<RefCell<OutputFault>>,
    additional: Vec<AdditionalModule>,
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
    fail_next_configuration: bool,
}
impl Fixture {
    fn new() -> Self {
        Self::with_options(false, false)
    }

    fn with_keyboard(keyboard: bool) -> Self {
        Self::with_options(keyboard, false)
    }

    fn with_options(keyboard: bool, owner_only: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-composition-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut configuration = read_configuration(if owner_only {
            "AMATERASU_OWNER_CONFIG"
        } else if keyboard {
            "AMATERASU_KEYBOARD_COMPOSITION_CONFIG"
        } else {
            "AMATERASU_COMPOSITION_CONFIG"
        });
        // The owner receives a private host-owned state root through Broker.
        let state_root = root.join("owner-state");
        std::fs::create_dir(&state_root).unwrap();
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        configuration["host_context"] = json!({"schema": 1, "state_root": state_root});
        configuration["principals"][SCREEN]["call"]
            .as_array_mut()
            .unwrap()
            .push(json!("amaterasu.host-state-context/1"));
        configuration["settings"][COORDINATOR]["journal_path"] = json!(root.join("journal.json"));
        let participants = configuration["settings"][COORDINATOR]["participants"]
            .as_array()
            .expect("participants");
        let owner = participants
            .iter()
            .find(|p| p["provider"] == SCREEN)
            .expect("screen participant");
        assert_eq!(owner["plan_method"]["service"], SCREEN_SERVICE);
        if !owner_only {
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
        if keyboard {
            assert!(matches!(
                broker.borrow_mut().dispatch(
                    "configuration",
                    Command::Register {
                        service: "amaterasu.configuration/1".into()
                    }
                ),
                Response::Ok
            ));
        }
        let trace = Rc::new(RefCell::new(Vec::new()));
        let fault = Rc::new(RefCell::new(OutputFault::default()));
        let (display_library, display) = if owner_only {
            (std::ptr::null_mut(), None)
        } else {
            let (library, api) = load_plugin("AMATERASU_DISPLAY_PLUGIN");
            (
                library,
                Some(RunningModule::new(
                    api,
                    broker.clone(),
                    DISPLAY,
                    trace.clone(),
                    fault.clone(),
                )),
            )
        };
        let (screen_library, screen_api) = load_plugin("AMATERASU_SCREEN_PLUGIN");
        let (lifecycle_library, lifecycle_api) = load_plugin("AMATERASU_LIFECYCLE_PLUGIN");
        let screen = RunningModule::new(
            screen_api,
            broker.clone(),
            SCREEN,
            trace.clone(),
            fault.clone(),
        );
        let lifecycle = RunningModule::new(
            lifecycle_api,
            broker.clone(),
            COORDINATOR,
            trace.clone(),
            fault.clone(),
        );
        let mut additional = Vec::new();
        if keyboard {
            for (variable, principal) in [
                ("AMATERASU_REGISTRY_PLUGIN", "amaterasu.extension-registry"),
                ("AMATERASU_ACTIONS_PLUGIN", "amaterasu.extension-actions"),
                ("AMATERASU_INPUT_PLUGIN", "amaterasu.input"),
            ] {
                let (library, api) = load_plugin(variable);
                additional.push(AdditionalModule {
                    module: RunningModule::new(
                        api,
                        broker.clone(),
                        principal,
                        trace.clone(),
                        fault.clone(),
                    ),
                    library,
                });
            }
        }
        Self {
            broker,
            lifecycle,
            display,
            screen,
            display_library,
            screen_library,
            trace,
            fault,
            additional,
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
            fail_next_configuration: false,
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
        for principal in ["virtual", "state", "configuration"] {
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
                } else if principal == "configuration" {
                    assert_eq!(payload["action"], "product_get");
                    json!({"ok":true,"found":false,"revision":0,"version":null,"values":{}})
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
                json!({"ok":true,"revision":collection.revision,"found":collection.revision!=0,"metadata":collection.metadata,"count":collection.entries.len(),"value":p["key"].as_str().and_then(|key| collection.entries.get(key))})
            }
            "collection_page" => {
                let collection = self
                    .collections
                    .entry(format!("{caller}/{}", p["collection"].as_str().unwrap()))
                    .or_default();
                let after = p["after"].as_str();
                let limit = p["limit"].as_u64().unwrap() as usize;
                let mut matching = collection
                    .entries
                    .iter()
                    .filter(|(key, _)| after.is_none_or(|after| key.as_str() > after));
                let page: Vec<_> = matching.by_ref().take(limit).collect();
                let next = matching
                    .next()
                    .and_then(|_| page.last().map(|(key, _)| (*key).clone()));
                let entries: Vec<_> = page
                    .into_iter()
                    .map(|(key, value)| json!({"key":key,"value":value}))
                    .collect();
                json!({"ok":true,"revision":collection.revision,"entries":entries,"next":next})
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
                if p["action"] == "reconfigure_mode_only" && self.fail_next_configuration {
                    self.fail_next_configuration = false;
                    return json!({"ok":false,"error":"injected_configuration_failure"});
                }
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
                timeout_ms: 30000,
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while std::time::Instant::now() < deadline {
            let result = self.call(json!({"operation":"lookup","operation_id":id}));
            if result["status"] != "pending" {
                return result;
            }
        }
        panic!("transition {id} did not settle within 180 seconds")
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
    for cycle in 0..20 {
        let pause_id = format!("pause-{cycle}");
        let paused =
            f.call(json!({"operation":"pause_stream","operation_id":pause_id,"target":target}));
        assert_eq!(paused["status"], "pending", "{paused}");
        let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":format!("retire-{cycle}"),"target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
        assert_eq!(retired["status"], "pending", "{retired}");
        let paused = f.settled(&pause_id);
        assert_eq!(paused["status"], "succeeded", "{paused}");
        let video = if cycle < 10 || cycle % 2 == 1 {
            initial.clone()
        } else {
            json!({"width":1280,"height":720,"refresh_millihz":60000,"hdr":false})
        };
        let resume_id = format!("resume-{cycle}");
        let resume = f.call(json!({"operation":"resume_stream","operation_id":resume_id,"target":session["target"],"intent":"resume","requested_video":video,"consumer_requirements":["capture"]}));
        assert_eq!(resume["status"], "pending", "{resume}");
        let resumed = f.settled(&resume_id);
        assert_eq!(resumed["status"], "succeeded", "{resumed}");
        assert_eq!(resumed["candidate"]["mode"], video);
        target = start_capture(&mut f, &resumed, &video, &resume_id);
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
#[ignore = "requires both independently built v2 plugins, matching lifecycle artifact and approved composition configuration"]
fn rejected_configuration_never_exposes_capture_and_releases_owned_screen() {
    let mut f = Fixture::new();
    let original = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let changed = json!({"width":1280,"height":720,"refresh_millihz":60000,"hdr":false});
    let prepared = f.call(json!({"operation":"prepare","operation_id":"session","requested_video":original,"application_ref":{"kind":"desktop"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let session = f.settled("session");
    assert_eq!(session["status"], "succeeded", "{session}");
    f.fail_next_configuration = true;
    let stream = f.call(json!({"operation":"prepare_stream","operation_id":"failed-stream","target":session["target"],"intent":"initial","requested_video":changed,"consumer_requirements":["capture"]}));
    assert_eq!(stream["status"], "pending", "{stream}");
    let failed = f.settled("failed-stream");
    assert_ne!(failed["status"], "succeeded", "{failed}");
    assert!(
        failed.get("candidate").is_none_or(Value::is_null),
        "{failed}"
    );
    assert!(
        !f.fail_next_configuration,
        "failure injection did not reach native configuration"
    );
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":session["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    let app = f.call(json!({"operation":"application_retired","operation_id":"app-retired","target":session["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(f.references.is_empty());
    assert!(f.borrows.is_empty());
}

#[test]
#[ignore = "requires a matching lifecycle artifact and approved composition configuration"]
fn lifecycle_rejects_missing_and_cyclic_participant_dependencies() {
    for graph in ["valid", "missing", "cycle"] {
        let mut configuration = read_configuration("AMATERASU_COMPOSITION_CONFIG");
        let root = std::env::temp_dir().join(format!(
            "amaterasu-composition-invalid-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        configuration["settings"][COORDINATOR]["journal_path"] = json!(root.join("journal.json"));
        let participants = configuration["settings"][COORDINATOR]["participants"]
            .as_array_mut()
            .unwrap();
        let owner = participants
            .iter()
            .position(|p| p["provider"] == SCREEN)
            .unwrap();
        let dependent = participants
            .iter()
            .position(|p| p["provider"] == DISPLAY)
            .unwrap();
        if graph == "cycle" {
            participants[owner]["depends_on"] = json!([DISPLAY]);
            participants[dependent]["depends_on"] = json!([SCREEN]);
        } else if graph == "missing" {
            participants[dependent]["depends_on"] = json!(["missing.provider"]);
        }
        let broker = Rc::new(RefCell::new(
            Broker::new(serde_json::from_value(configuration).unwrap()).unwrap(),
        ));
        let mut endpoint = Box::new(Endpoint {
            broker,
            principal: COORDINATOR,
            trace: Rc::new(RefCell::new(Vec::new())),
            fault: Rc::new(RefCell::new(OutputFault::default())),
        });
        let host = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *endpoint as *mut Endpoint).cast(),
            invoke,
        };
        let (library, api) = load_plugin("AMATERASU_LIFECYCLE_PLUGIN");
        let mut instance = std::ptr::null_mut();
        // SAFETY: the host and its boxed context remain alive for this create call.
        let status = unsafe { (api.create)(&host, &mut instance) };
        if !instance.is_null() {
            // SAFETY: a non-null instance was produced by this module's create function.
            unsafe { (api.destroy)(instance) };
        }
        // SAFETY: no instance or callbacks remain when the library is unloaded.
        unsafe { dlclose(library) };
        if graph == "valid" {
            assert_eq!(status, abi::STATUS_OK, "valid graph rejected");
        } else {
            assert_ne!(status, abi::STATUS_OK, "invalid graph accepted: {graph}");
        }
        assert!(
            endpoint.trace.borrow().is_empty(),
            "invalid graph dispatched provider effects"
        );
    }
}

#[test]
#[ignore = "requires real input, registry, actions and v2 participants with an approved active-stream keybinding"]
fn active_stream_keyboard_remove_retires_dependents_before_owner_effect() {
    let mut f = Fixture::with_keyboard(true);
    let counter_name = CString::new("amaterasu_input_qualification_injected").unwrap();
    // SAFETY: the input library remains loaded for the lifetime of the fixture.
    let counter_address = unsafe { dlsym(f.additional[2].library, counter_name.as_ptr()) };
    assert!(
        !counter_address.is_null(),
        "qualification input counter unavailable"
    );
    // SAFETY: this symbol has the documented qualification-only C ABI signature.
    let injected: unsafe extern "C" fn() -> u64 = unsafe { std::mem::transmute(counter_address) };
    // SAFETY: the loaded qualification input module owns the counter.
    let initial_injections = unsafe { injected() };
    for _ in 0..100 {
        f.tick();
    }
    let binding = json!({"id":"remove-owned","action_id":"nonary.virtual-screen/remove-owned","arguments":{},"context":"active","enabled":true,"modifiers":[],"propagation":"consume_trigger","source":"session_input","target":"current_stream","trigger":{"code":30,"platform":"linux_evdev"}});
    let configured = f.request_as("amaterasu.management", "amaterasu.extension-registry/1", json!({"operation":"settings_update","provider":"nonary.virtual-screen","scope":{"kind":"host"},"expected_revision":0,"values":{"remove_owned_shortcut":binding}}));
    assert_eq!(configured["ok"], true, "shortcut setup: {configured}");
    let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
    let prepared = f.call(json!({"operation":"prepare","operation_id":"session","requested_video":video,"application_ref":{"kind":"desktop"}}));
    assert_eq!(prepared["status"], "pending", "{prepared}");
    let session = f.settled("session");
    assert_eq!(session["status"], "succeeded", "{session}");
    let stream = f.call(json!({"operation":"prepare_stream","operation_id":"stream","target":session["target"],"intent":"initial","requested_video":video,"consumer_requirements":["capture","input"]}));
    assert_eq!(stream["status"], "pending", "{stream}");
    let stream = f.settled("stream");
    assert_eq!(stream["status"], "succeeded", "{stream}");
    let target = stream["target"].clone();
    let candidate = stream["candidate"].clone();
    let capture = f.call_as("capture", json!({"operation":"ack_binding","operation_id":"capture-ack","target":target,"consumer":"capture","plan_revision":candidate["plan_revision"],"candidate":candidate,"observed_capture":observation(&video)}));
    assert_eq!(capture["status"], "pending", "{capture}");
    let mut ack = Value::Null;
    for _ in 0..500 {
        ack = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"capture-ack"}),
        );
        if ack["status"] != "pending" {
            break;
        }
    }
    assert_eq!(ack["status"], "succeeded", "{ack}");
    let binding = f.call(
        json!({"operation":"issue_input_binding","operation_id":"input-binding","target":target}),
    );
    assert_eq!(binding["status"], "succeeded", "{binding}");
    let acquired = f.request_as("amaterasu.stream-session", "amaterasu.input/1", json!({"action":"acquire","operation_id":"input-acquire","ttl_ms":30000,"lifecycle_binding_token":binding["token"]}));
    assert_eq!(acquired["ok"], true, "{acquired}");
    assert_eq!(acquired["input_acknowledged"], true, "{acquired}");
    let input_ack = f.call_as("amaterasu.stream-session", json!({"operation":"ack_binding","operation_id":"input-ack","target":target,"consumer":"input","plan_revision":candidate["plan_revision"],"candidate":candidate}));
    assert_eq!(input_ack["status"], "pending", "{input_ack}");
    for _ in 0..500 {
        let ack = f.call_as(
            "amaterasu.stream-session",
            json!({"operation":"lookup","operation_id":"input-ack"}),
        );
        if ack["status"] != "pending" {
            assert_eq!(ack["status"], "succeeded", "{ack}");
            break;
        }
    }
    let started =
        f.call(json!({"operation":"start_stream","operation_id":"start","target":target}));
    assert_eq!(started["status"], "succeeded", "{started}");
    f.fault.borrow_mut().drop_completion_result = true;
    let batch = f.begin_as("amaterasu.stream-session", "amaterasu.input/1", json!({"action":"batch","lease":acquired["lease"],"input_binding":binding["token"],"operation_id":format!("{}:1", acquired["batch_epoch"].as_str().unwrap()),"events":[{"kind":"key","key":{"platform":"linux_evdev","code":30},"state":"down"}]}));
    let mut began_transition = false;
    for _ in 0..5000 {
        f.tick();
        began_transition = f.trace.borrow().iter().any(|e| {
            e["principal"] == "amaterasu.extension-actions"
                && e["command"]["payload"]["operation"] == "begin_action_transition"
        });
        if began_transition {
            break;
        }
        if matches!(
            f.broker.borrow_mut().dispatch(
                "amaterasu.stream-session",
                Command::TakeResult { request: batch }
            ),
            Response::Result { .. }
        ) {
            break;
        }
    }
    assert!(
        began_transition,
        "keyboard did not reach action transition; see retained trace"
    );
    assert!(
        !f.native_calls.iter().any(|call| call["action"] == "remove"),
        "owner effect preceded consumer retirement"
    );
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"capture-retire","target":target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let mut completed = false;
    for _ in 0..5000 {
        f.tick();
        match f.broker.borrow_mut().dispatch(
            "amaterasu.stream-session",
            Command::TakeResult { request: batch },
        ) {
            Response::Pending => {}
            Response::Result { payload } => {
                assert_eq!(payload["ok"], true, "{payload}");
                completed = true;
                break;
            }
            other => panic!("keyboard batch result: {other:?}"),
        }
    }
    assert!(completed, "keyboard batch did not settle");
    assert!(
        f.fault.borrow().completion_result_dropped,
        "completion reply was not dropped"
    );
    let trace = f.trace.borrow();
    let settings_release = trace
        .iter()
        .position(|e| {
            e["principal"] == COORDINATOR
                && e["command"]["service"] == DISPLAY_SERVICE
                && e["command"]["method"] == "lifecycle_callback"
                && e["command"]["payload"]["phase"] == "stream.release"
        })
        .expect("dependent settings did not retire");
    let remove = trace
        .iter()
        .position(|e| e["native"]["action"] == "remove")
        .expect("owner did not remove screen");
    assert!(settings_release < remove, "{trace:?}");
    assert!(
        trace
            .iter()
            .any(|e| e["principal"] == "amaterasu.extension-actions"
                && e["command"]["service"] == SCREEN_SERVICE
                && e["command"]["method"] == "action_execute"),
        "actual action provider was not invoked"
    );
    assert!(
        trace
            .iter()
            .any(|e| e["principal"] == "amaterasu.extension-actions"
                && e["command"]["payload"]["operation"] == "rebind_action_plan"),
        "fresh owner plan was not rebound"
    );
    let completion = trace
        .iter()
        .find(|e| {
            e["principal"] == "amaterasu.extension-actions"
                && e["command"]["payload"]["operation"] == "complete_action"
        })
        .expect("action completion was not sent")["command"]["payload"]
        .clone();
    drop(trace);
    assert!(
        f.references.is_empty(),
        "owner reference remains after removal"
    );
    assert!(
        f.borrows.is_empty(),
        "dependent borrow remains after removal"
    );
    let removed_reference = f
        .native_calls
        .iter()
        .find(|call| call["action"] == "create")
        .expect("initial owner creation")["reference_id"]
        .clone();
    let resumed = f.call(json!({"operation":"resume_stream","operation_id":"after-remove","target":session["target"],"intent":"resume","requested_video":video,"consumer_requirements":["capture","input"]}));
    assert_eq!(resumed["status"], "pending", "{resumed}");
    let resumed = f.settled("after-remove");
    assert_eq!(resumed["status"], "succeeded", "{resumed}");
    assert!(
        f.native_calls
            .iter()
            .any(|call| call["action"] == "create" && call["reference_id"] != removed_reference),
        "fresh owner reference was not created"
    );
    let removals_before_retry = f
        .native_calls
        .iter()
        .filter(|call| call["action"] == "remove")
        .count();
    let retry = f.call_as("amaterasu.extension-actions", completion);
    assert_eq!(retry["status"], "succeeded", "{retry}");
    assert_eq!(
        f.native_calls
            .iter()
            .filter(|call| call["action"] == "remove")
            .count(),
        removals_before_retry,
        "completion replay invoked a native effect"
    );
    let next_target = resumed["target"].clone();
    let next_candidate = resumed["candidate"].clone();
    let capture = f.call_as("capture", json!({"operation":"ack_binding","operation_id":"second-capture-ack","target":next_target,"consumer":"capture","plan_revision":next_candidate["plan_revision"],"candidate":next_candidate,"observed_capture":observation(&video)}));
    assert_eq!(capture["status"], "pending", "{capture}");
    for _ in 0..500 {
        let ack = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"second-capture-ack"}),
        );
        if ack["status"] != "pending" {
            assert_eq!(ack["status"], "succeeded", "{ack}");
            break;
        }
    }
    let binding = f.call(json!({"operation":"issue_input_binding","operation_id":"second-input-binding","target":next_target}));
    assert_eq!(binding["status"], "succeeded", "{binding}");
    let acquired = f.request_as("amaterasu.stream-session", "amaterasu.input/1", json!({"action":"acquire","operation_id":"second-input-acquire","ttl_ms":30000,"lifecycle_binding_token":binding["token"]}));
    assert_eq!(acquired["ok"], true, "{acquired}");
    let input_ack = f.call_as("amaterasu.stream-session", json!({"operation":"ack_binding","operation_id":"second-input-ack","target":next_target,"consumer":"input","plan_revision":next_candidate["plan_revision"],"candidate":next_candidate}));
    assert_eq!(input_ack["status"], "pending", "{input_ack}");
    for _ in 0..500 {
        let ack = f.call_as(
            "amaterasu.stream-session",
            json!({"operation":"lookup","operation_id":"second-input-ack"}),
        );
        if ack["status"] != "pending" {
            assert_eq!(ack["status"], "succeeded", "{ack}");
            break;
        }
    }
    let started = f.call(
        json!({"operation":"start_stream","operation_id":"second-start","target":next_target}),
    );
    assert_eq!(started["status"], "succeeded", "{started}");
    let batch = f.begin_as("amaterasu.stream-session", "amaterasu.input/1", json!({"action":"batch","lease":acquired["lease"],"input_binding":binding["token"],"operation_id":format!("{}:1", acquired["batch_epoch"].as_str().unwrap()),"events":[{"kind":"key","key":{"platform":"linux_evdev","code":30},"state":"down"}]}));
    let prior_transitions = f
        .trace
        .borrow()
        .iter()
        .filter(|e| {
            e["principal"] == "amaterasu.extension-actions"
                && e["command"]["payload"]["operation"] == "begin_action_transition"
        })
        .count();
    let mut second_transition = false;
    for _ in 0..5000 {
        f.tick();
        if f.trace
            .borrow()
            .iter()
            .filter(|e| {
                e["principal"] == "amaterasu.extension-actions"
                    && e["command"]["payload"]["operation"] == "begin_action_transition"
            })
            .count()
            > prior_transitions
        {
            second_transition = true;
            break;
        }
    }
    assert!(second_transition, "second action transition did not begin");
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"second-capture-retire","target":next_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let mut second_completed = false;
    for _ in 0..5000 {
        f.tick();
        match f.broker.borrow_mut().dispatch(
            "amaterasu.stream-session",
            Command::TakeResult { request: batch },
        ) {
            Response::Pending => {}
            Response::Result { payload } => {
                assert_eq!(payload["ok"], true, "{payload}");
                second_completed = true;
                break;
            }
            other => panic!("second keyboard batch: {other:?}"),
        }
    }
    assert!(second_completed, "second removal did not settle");
    assert!(
        f.references.is_empty() && f.borrows.is_empty(),
        "second removal left native ownership"
    );
    // SAFETY: the loaded qualification input module remains live.
    assert_eq!(
        unsafe { injected() },
        initial_injections,
        "consumed shortcut reached input backend"
    );
    let removed: Vec<_> = f
        .native_calls
        .iter()
        .filter(|call| call["action"] == "remove")
        .collect();
    assert_eq!(
        removed.len(),
        removals_before_retry + 2,
        "second stream and root must each release once"
    );
    assert_ne!(
        removed[removed.len() - 2]["reference_id"],
        removed[removed.len() - 1]["reference_id"],
        "stream and root release must target distinct references"
    );
    let resumed = f.call(json!({"operation":"resume_stream","operation_id":"after-second-remove","target":session["target"],"intent":"resume","requested_video":video,"consumer_requirements":["capture","input"]}));
    assert_eq!(resumed["status"], "pending", "{resumed}");
    let resumed = f.settled("after-second-remove");
    assert_eq!(resumed["status"], "succeeded", "{resumed}");
    let final_target = resumed["target"].clone();
    let capture_ack = f.call_as("capture", json!({"operation":"ack_binding","operation_id":"final-ack","target":final_target,"consumer":"capture","plan_revision":resumed["candidate"]["plan_revision"],"candidate":resumed["candidate"],"observed_capture":observation(&video)}));
    assert_eq!(capture_ack["status"], "pending", "{capture_ack}");
    for _ in 0..500 {
        let ack = f.call_as(
            "capture",
            json!({"operation":"lookup","operation_id":"final-ack"}),
        );
        if ack["status"] != "pending" {
            assert_eq!(ack["status"], "succeeded", "{ack}");
            break;
        }
    }
    let final_binding = f.call(json!({"operation":"issue_input_binding","operation_id":"final-input-binding","target":resumed["target"]}));
    assert_eq!(final_binding["status"], "succeeded", "{final_binding}");
    let acquired = f.request_as("amaterasu.stream-session", "amaterasu.input/1", json!({"action":"acquire","operation_id":"final-input-acquire","ttl_ms":30000,"lifecycle_binding_token":final_binding["token"]}));
    assert_eq!(acquired["ok"], true, "{acquired}");
    let input_ack = f.call_as("amaterasu.stream-session", json!({"operation":"ack_binding","operation_id":"final-input-ack","target":resumed["target"],"consumer":"input","plan_revision":resumed["candidate"]["plan_revision"],"candidate":resumed["candidate"]}));
    assert_eq!(input_ack["status"], "pending", "{input_ack}");
    for _ in 0..500 {
        let ack = f.call_as(
            "amaterasu.stream-session",
            json!({"operation":"lookup","operation_id":"final-input-ack"}),
        );
        if ack["status"] != "pending" {
            assert_eq!(ack["status"], "succeeded", "{ack}");
            break;
        }
    }
    let started = f.call(
        json!({"operation":"start_stream","operation_id":"final-start","target":final_target}),
    );
    assert_eq!(started["status"], "succeeded", "{started}");
    let end =
        f.call(json!({"operation":"end_session","operation_id":"end","target":session["target"]}));
    assert_eq!(end["status"], "pending", "{end}");
    let retired = f.call_as("capture", json!({"operation":"retire_stream","operation_id":"final-capture-retire","target":final_target,"consumer":"capture","retirement_receipt":{"response":{"status":"not_found"}}}));
    assert_eq!(retired["status"], "pending", "{retired}");
    let app = f.call(json!({"operation":"application_retired","operation_id":"application-retired","target":session["target"]}));
    assert_eq!(app["status"], "succeeded", "{app}");
    let ended = f.settled("end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    assert!(f.references.is_empty() && f.borrows.is_empty());
}

#[test]
#[ignore = "requires both independently built v2 plugins, matching lifecycle artifact and approved composition configuration"]
fn foreign_and_stale_owner_outputs_never_reach_dependent_configuration() {
    for kind in ["foreign", "stale"] {
        let mut f = Fixture::new();
        f.fault.borrow_mut().kind = Some(kind);
        let video = json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false});
        let pending = f.call(json!({"operation":"prepare","operation_id":"session","requested_video":video,"application_ref":{"kind":"desktop"}}));
        assert_eq!(pending["status"], "pending", "{pending}");
        let outcome = f.settled("session");
        assert!(
            f.fault.borrow().injected,
            "owner did not emit an output to tamper with"
        );
        assert_ne!(outcome["status"], "succeeded", "{kind}: {outcome}");
        assert!(outcome.get("candidate").is_none_or(Value::is_null));
        assert!(
            !f.trace
                .borrow()
                .iter()
                .any(|e| e["command"]["service"] == SCREEN_SERVICE
                    && e["command"]["method"] == "resource_control"),
            "{kind} output authorized a control"
        );
    }
}

#[test]
#[ignore = "requires the partial owner artifact and its exact five-method approvals"]
fn partial_owner_profile_is_rejected_without_registered_finalize() {
    let mut configuration = read_configuration("AMATERASU_OWNER_CONFIG");
    let root = std::env::temp_dir().join(format!(
        "amaterasu-partial-owner-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    configuration["settings"][COORDINATOR]["journal_path"] = json!(root.join("journal.json"));
    assert!(
        configuration["settings"][COORDINATOR]
            .get("finalize_policies")
            .is_none()
    );
    let broker = Rc::new(RefCell::new(
        Broker::new(serde_json::from_value(configuration).unwrap()).unwrap(),
    ));
    let trace = Rc::new(RefCell::new(Vec::new()));
    let fault = Rc::new(RefCell::new(OutputFault::default()));
    let (owner_library, owner_api) = load_plugin("AMATERASU_SCREEN_PLUGIN");
    let owner = RunningModule::new(
        owner_api,
        broker.clone(),
        SCREEN,
        trace.clone(),
        fault.clone(),
    );
    let mut endpoint = Box::new(Endpoint {
        broker,
        principal: COORDINATOR,
        trace: trace.clone(),
        fault,
    });
    let host = abi::HostApi {
        header: abi::AbiHeader::of::<abi::HostApi>(),
        context: (&mut *endpoint as *mut Endpoint).cast(),
        invoke,
    };
    let (lifecycle_library, lifecycle_api) = load_plugin("AMATERASU_LIFECYCLE_PLUGIN");
    let mut instance = std::ptr::null_mut();
    // SAFETY: the host and boxed context remain valid for the create call.
    let status = unsafe { (lifecycle_api.create)(&host, &mut instance) };
    if !instance.is_null() {
        // SAFETY: the instance was returned by this module's create function.
        unsafe { (lifecycle_api.destroy)(instance) };
    }
    // SAFETY: lifecycle has no surviving instance or callbacks.
    unsafe { dlclose(lifecycle_library) };
    drop(owner);
    // SAFETY: owner instance is destroyed and callbacks have stopped.
    unsafe { dlclose(owner_library) };
    assert_ne!(status, abi::STATUS_OK, "incomplete v2 profile was accepted");
    assert!(
        !trace
            .borrow()
            .iter()
            .any(|e| e["command"]["op"] == "call_api" || e["command"]["op"] == "request"),
        "provider effect was dispatched"
    );
}

#[test]
#[ignore = "requires a session-prepare-capable owner, matching lifecycle and approved owner configuration"]
fn real_owner_prepares_session_with_simulated_native_and_state() {
    let mut f = Fixture::with_options(false, true);
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
