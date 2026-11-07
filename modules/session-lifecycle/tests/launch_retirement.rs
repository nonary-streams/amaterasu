//! Exercise lifecycle and a separately built Launch plugin through the real broker.
//!
//! Build Launch, then set `AMATERASU_LAUNCH_PLUGIN` to the resulting cdylib and
//! run this test with `cargo test -p amaterasu-session-lifecycle --test
//! launch_retirement -- --ignored`. The plugin must be trusted native code.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    ffi::c_void,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

const LIFECYCLE: &str = "amaterasu.session-lifecycle";
const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";
const LAUNCH: &str = "amaterasu.launch";
const LAUNCH_SERVICE: &str = "amaterasu.launch/1";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
type Trace = Rc<RefCell<Vec<(String, Command, Response)>>>;

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Endpoint {
    broker: Rc<RefCell<Broker>>,
    principal: &'static str,
    trace: Trace,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: abi::BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: The harness retains the boxed endpoint until its module is destroyed.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    // SAFETY: The SDK lends these bytes for the duration of this synchronous call.
    let Ok(input) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(input) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command.clone());
    endpoint
        .trace
        .borrow_mut()
        .push((endpoint.principal.to_owned(), command, response.clone()));
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: The sink copies the bytes synchronously.
    unsafe { reply.send(&bytes) };
    abi::STATUS_OK
}

struct Module {
    api: abi::ModuleApi,
    instance: *mut c_void,
    _endpoint: Box<Endpoint>,
}

impl Module {
    fn start(
        api: abi::ModuleApi,
        broker: Rc<RefCell<Broker>>,
        principal: &'static str,
        trace: Trace,
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
        // SAFETY: The host table is valid and the boxed endpoint outlives the instance.
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

    fn tick(&self) {
        // SAFETY: The live instance belongs to this table and calls are serialized.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: All calls are complete and the matching table destroys the instance once.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

struct Harness {
    broker: Rc<RefCell<Broker>>,
    lifecycle: Module,
    launch: Module,
    _launch_library: libloading::Library,
    trace: Trace,
    _root: TemporaryDirectory,
    sequence: u64,
}

impl Harness {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-launch-retirement-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let root = TemporaryDirectory(root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let configuration = serde_json::from_value(json!({
            "principals": {
                LIFECYCLE: {"provide":[LIFECYCLE_SERVICE],"call":[LAUNCH_SERVICE]},
                LAUNCH: {"provide":[LAUNCH_SERVICE]},
                "transport": {"call":[LIFECYCLE_SERVICE,LAUNCH_SERVICE]}
            },
            "settings": {
                LIFECYCLE: {
                    "journal_path":root.0.join("journal.json"),
                    "consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["example/1"]},
                    "trusted_callers":["transport"],
                    "operation_grants":{"transport":["prepare","lookup","end_session","application_retired"]}
                },
                LAUNCH: {"retirement_observers":[LIFECYCLE]}
            }
        })).unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(configuration).unwrap()));
        let trace = Rc::new(RefCell::new(Vec::new()));
        let path = std::env::var_os("AMATERASU_LAUNCH_PLUGIN").expect("Launch plugin path");
        // SAFETY: The test operator supplies a trusted matching plugin; the
        // library is retained until after its module instance is destroyed.
        let launch_library = unsafe { libloading::Library::new(path) }.expect("load Launch plugin");
        // SAFETY: This is the documented module entry point, held by launch_library.
        let launch_api = unsafe {
            let entry = launch_library
                .get::<abi::ModuleEntry>(b"amaterasu_module_v1\0")
                .expect("Launch entry point");
            *entry()
        };
        assert!(launch_api.header.supports::<abi::ModuleApi>());
        let launch = Module::start(launch_api, broker.clone(), LAUNCH, trace.clone());
        // SAFETY: This table is the lifecycle crate's static ABI table.
        let lifecycle_api = unsafe { *amaterasu_session_lifecycle::amaterasu_module_v1() };
        let lifecycle = Module::start(lifecycle_api, broker.clone(), LIFECYCLE, trace.clone());
        Self {
            broker,
            lifecycle,
            launch,
            _launch_library: launch_library,
            trace,
            _root: root,
            sequence: 0,
        }
    }

    fn tick(&self) {
        self.lifecycle.tick();
        self.launch.tick();
    }

    fn call(&mut self, service: &str, payload: Value) -> Value {
        self.sequence += 1;
        let accepted = self.broker.borrow_mut().dispatch(
            "transport",
            Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 5_000,
                operation_id: format!("test-{}", self.sequence),
                parent: None,
            },
        );
        let Response::Accepted { id } = accepted else {
            panic!("request rejected: {accepted:?}")
        };
        for _ in 0..100 {
            self.tick();
            let result = self
                .broker
                .borrow_mut()
                .dispatch("transport", Command::TakeResult { request: id });
            match result {
                Response::Pending => {}
                Response::Result { payload } => {
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch("transport", Command::ReleaseResult { request: id }),
                        Response::Ok
                    ));
                    return payload;
                }
                other => panic!("unexpected broker result: {other:?}"),
            }
        }
        panic!("request did not settle");
    }

    fn settled(&mut self, operation_id: &str) -> Value {
        for _ in 0..100 {
            let result = self.call(
                LIFECYCLE_SERVICE,
                json!({"operation":"lookup","operation_id":operation_id}),
            );
            if result["status"] != "pending" {
                return result;
            }
        }
        panic!("lifecycle operation did not settle: {operation_id}");
    }
}

#[test]
#[ignore = "requires an independently built trusted Launch plugin"]
fn never_admitted_application_is_fenced_before_session_end_completes() {
    let mut h = Harness::new();
    let epoch = h.call(LAUNCH_SERVICE, json!({"action":"operation_epoch"}));
    let epoch = epoch["operation_epoch"]
        .as_str()
        .expect("real Launch epoch");
    let application_operation = format!("ale1:{epoch}:never-admitted");
    let prepare = h.call(LIFECYCLE_SERVICE, json!({"operation":"prepare","operation_id":"prepare","application_ref":{"kind":"application","app_id":"app"},"application_operation_id":application_operation}));
    assert_eq!(prepare["status"], "pending", "{prepare}");
    let prepared = h.settled("prepare");
    assert_eq!(prepared["status"], "succeeded", "{prepared}");
    let target = prepared["target"].clone();
    let end = h.call(
        LIFECYCLE_SERVICE,
        json!({"operation":"end_session","operation_id":"end","target":target}),
    );
    assert_eq!(end["status"], "pending", "{end}");
    let awaiting_application = h.call(
        LIFECYCLE_SERVICE,
        json!({"operation":"lookup","operation_id":"end"}),
    );
    assert_eq!(
        awaiting_application["status"], "pending",
        "{awaiting_application}"
    );
    let application = h.call(
        LIFECYCLE_SERVICE,
        json!({"operation":"application_retired","operation_id":"retired","target":target}),
    );
    assert_eq!(application["status"], "pending", "{application}");
    let application = h.settled("retired");
    assert_eq!(application["status"], "succeeded", "{application}");
    let ended = h.settled("end");
    assert_eq!(ended["status"], "succeeded", "{ended}");
    let trace = h.trace.borrow();
    let request = trace.iter().find_map(|(principal, command, response)| {
        if principal == LIFECYCLE && matches!(command, Command::Request { service, payload, .. } if service == LAUNCH_SERVICE && payload == &json!({"action":"verify_retirement","owner":"transport","launch_id":null,"operation_id":application_operation}))
            && let Response::Accepted { id } = response {
                Some(*id)
            } else { None }
    }).expect("exact retirement request admitted");
    assert!(trace.iter().any(|(principal, command, response)| principal == LAUNCH && matches!(response, Response::Ok) && matches!(command, Command::Complete { request: completed, payload } if *completed == request && payload == &json!({"ok":true,"retired":true,"never_admitted":true,"owner":"transport","launch_id":null,"operation_id":application_operation}))));
    drop(trace);
    let delayed = h.call(LAUNCH_SERVICE, json!({"action":"preflight_export","operation_id":application_operation,"app_id":"app-0000000000000001"}));
    assert_eq!(delayed["code"], "operation_retired", "{delayed}");
}
