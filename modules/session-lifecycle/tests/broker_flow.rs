//! Cross-module tests run the production lifecycle ABI against the actual broker.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVICE: &str = "amaterasu.session-lifecycle/1";
const COORDINATOR: &str = "amaterasu.session-lifecycle";
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Endpoint {
    broker: Rc<RefCell<Broker>>,
    principal: &'static str,
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: abi::BorrowedBytes,
    reply: abi::ReplySink,
) -> abi::Status {
    // SAFETY: Harness retains its boxed Endpoint until the module is destroyed;
    // module calls and broker pumping are serialized on this test thread.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    // SAFETY: the SDK lends valid input bytes for the synchronous ABI call.
    let Ok(input) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice(input) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let response = endpoint
        .broker
        .borrow_mut()
        .dispatch(endpoint.principal, command);
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the SDK's reply sink synchronously copies the temporary bytes.
    unsafe { reply.send(&bytes) };
    abi::STATUS_OK
}

struct Harness {
    broker: Rc<RefCell<Broker>>,
    endpoint: Box<Endpoint>,
    api: abi::ModuleApi,
    instance: *mut c_void,
    root: PathBuf,
    request_sequence: u64,
}

impl Harness {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-lifecycle-broker-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let configuration = serde_json::from_value(json!({
            "principals": {
                COORDINATOR: {"provide": [SERVICE]},
                "transport": {"call": [SERVICE]},
                "other-transport": {"call": [SERVICE]},
                "untrusted": {"call": [SERVICE]}
            },
            "settings": {
                COORDINATOR: {
                    "journal_path": root.join("journal.json"),
                    "trusted_callers": ["transport", "other-transport"],
                    "operation_grants": {
                        "transport": ["prepare", "lookup", "snapshot"],
                        "other-transport": ["prepare", "lookup", "snapshot"]
                    }
                }
            }
        }))
        .unwrap();
        let broker = Rc::new(RefCell::new(Broker::new(configuration).unwrap()));
        let endpoint = Box::new(Endpoint {
            broker: broker.clone(),
            principal: COORDINATOR,
        });
        // SAFETY: the exported table is static and has the ABI version checked below.
        let api = unsafe { *amaterasu_session_lifecycle::amaterasu_module_v1() };
        assert!(api.header.supports::<abi::ModuleApi>());
        let mut harness = Self {
            broker,
            endpoint,
            api,
            instance: std::ptr::null_mut(),
            root,
            request_sequence: 0,
        };
        harness.start();
        harness
    }

    fn start(&mut self) {
        let endpoint = abi::HostApi {
            header: abi::AbiHeader::of::<abi::HostApi>(),
            context: (&mut *self.endpoint as *mut Endpoint).cast(),
            invoke,
        };
        // SAFETY: Endpoint is boxed and outlives the module; creation and all
        // following lifecycle calls are serialized and use this module's table.
        assert_eq!(
            unsafe { (self.api.create)(&endpoint, &mut self.instance) },
            abi::STATUS_OK
        );
        assert!(!self.instance.is_null());
    }

    fn tick(&mut self) {
        // SAFETY: instance is a live module created by this exact table.
        assert_eq!(unsafe { (self.api.tick)(self.instance) }, abi::STATUS_OK);
    }

    fn call(&mut self, caller: &str, payload: Value) -> Value {
        self.request_sequence += 1;
        let response = self.broker.borrow_mut().dispatch(
            caller,
            Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 5_000,
                operation_id: format!("test-request-{}", self.request_sequence),
                parent: None,
            },
        );
        let Response::Accepted { id } = response else {
            panic!("request was not admitted by broker: {response:?}");
        };
        for _ in 0..32 {
            self.tick();
            let response = self
                .broker
                .borrow_mut()
                .dispatch(caller, Command::TakeResult { request: id });
            match response {
                Response::Pending => continue,
                Response::Result { payload } => {
                    assert!(matches!(
                        self.broker
                            .borrow_mut()
                            .dispatch(caller, Command::ReleaseResult { request: id }),
                        Response::Ok
                    ));
                    return payload;
                }
                response => panic!("unexpected lifecycle result: {response:?}"),
            }
        }
        panic!("lifecycle failed to reply within bounded test pump");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            // SAFETY: destroys this harness's live instance once, before Endpoint.
            unsafe { (self.api.destroy)(self.instance) };
            self.instance = std::ptr::null_mut();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn real_broker_identity_controls_admission_and_receipt_visibility() {
    let mut harness = Harness::new();
    let forged = harness.call(
        "untrusted",
        json!({"operation":"prepare", "operation_id":"one", "caller":"transport"}),
    );
    assert_eq!(forged["code"], "permission_denied");
    let admission = harness.call(
        "transport",
        json!({"operation":"prepare", "operation_id":"one"}),
    );
    assert_eq!(admission["status"], "pending");
    assert_eq!(
        harness.call(
            "transport",
            json!({"operation":"lookup", "operation_id":"one"})
        ),
        admission
    );
    assert_eq!(
        harness.call(
            "other-transport",
            json!({"operation":"lookup", "operation_id":"one"})
        )["status"],
        "not_found"
    );
}
