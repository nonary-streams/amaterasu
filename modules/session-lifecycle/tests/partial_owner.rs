//! Cross-ABI guard for an owner artifact missing mandatory v2 finalization.
//!
#![cfg(target_os = "linux")]
use amaterasu_module_sdk::{Command, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
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
    _endpoint: Box<Endpoint>,
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
            _endpoint: endpoint,
        }
    }
}
impl Drop for RunningModule {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            unsafe { (self.api.destroy)(self.instance) };
        }
    }
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
    assert!(
        approvals["api_methods"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| { entry["method"]["method"] != "resource_finalize" }),
        "this test requires the partial five-method owner artifact"
    );
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
    json!({"principals":{
        COORDINATOR:{"provide":[LIFECYCLE],"call":["nonary.virtual-screen/2"]},
        SCREEN:{"provide":["nonary.virtual-screen/2"],"call":[LIFECYCLE,"amaterasu.state/1","amaterasu.virtual-display/1"]},
        "transport":{"call":[LIFECYCLE]}, "capture":{"call":[LIFECYCLE]},
        "amaterasu.extension-registry":{"call":["nonary.virtual-screen/2"]},
        "amaterasu.management":{"call":["nonary.virtual-screen/2"]}
    },"api_methods":approvals["api_methods"],"settings":{COORDINATOR:{
        "journal_path":"/replaced","trusted_callers":["transport","capture",SCREEN],
        "operation_grants":{"transport":["prepare","lookup"],SCREEN:["verify_provider_epoch"]},
        "consumer_requirements":{"execution_domain":"desktop","capture_providers":["amaterasu.capture-pipewire/1"],"selector_schemas":["amaterasu.kwin-wl-output/1"]},
        "participants":[{"provider":SCREEN,"contract_version":2,"plan_method":method("resource_plan"),"callback_method":method("lifecycle_callback"),"status_method":method("lifecycle_status"),"settings_revision":"one","settings":settings,"capabilities":[],"cleanup_methods":[]}]
    }}})
}
#[test]
#[ignore = "requires the partial owner artifact and its exact five-method approvals"]
fn partial_owner_profile_is_rejected_without_registered_finalize() {
    let mut configuration = owner_configuration();
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
    let (owner_library, owner_api) = load_plugin("AMATERASU_SCREEN_PLUGIN");
    let owner = RunningModule::new(owner_api, broker.clone(), SCREEN, trace.clone());
    let mut endpoint = Box::new(Endpoint {
        broker,
        principal: COORDINATOR,
        trace: trace.clone(),
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
