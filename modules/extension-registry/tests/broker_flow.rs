//! Cross-module tests run the production lifecycle ABI against the actual broker.
use amaterasu_module_sdk::{Command, Response, abi};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVICE: &str = "amaterasu.extension-registry/1";
const COORDINATOR: &str = "amaterasu.extension-registry";
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
            "amaterasu-registry-broker-{}-{}",
            std::process::id(),
            DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let descriptor = json!({"provider":"example.demo","revision":1,"options":[],"actions":[],"capabilities":[],"settings_validate":{"service":"example.demo/1","method":"settings_validate","interface_major":1,"interface_minor":0,"schema_revision":1}});
        let method = json!({"service":"example.demo/1","method":"settings_validate","interface_major":1,"interface_minor":0,"request":{"required":{"values":"object"},"optional":{},"allow_extra":false},"response":{"required":{"ok":"boolean","valid":"boolean"},"optional":{},"allow_extra":false},"privilege":"user"});
        let configuration = serde_json::from_value(json!({
            "principals": { COORDINATOR:{"provide":[SERVICE],"call":["example.demo/1"]}, "example.demo":{"provide":["example.demo/1"]}, "management":{"call":[SERVICE,"example.demo/1"]}, "outsider":{"call":[SERVICE,"example.demo/1"]} },
            "api_methods":[{"provider":"example.demo","callers":[COORDINATOR,"management"],"method":method}],
            "settings": {COORDINATOR:{"descriptors":[{"descriptor":descriptor,"descriptor_sha256":"b26f4d1080cf077a280b9f888307a37a6b3e35aad9a20925077009feebd2cede","approved_methods":[method],"grants":[{"caller":"management","scope":{"kind":"host"},"read":true,"write":true},{"caller":"outsider","scope":{"kind":"host"},"read":true,"write":false}],"migrations":[]}]}}
        })).unwrap();
        let mut broker = Broker::new(configuration).unwrap();
        let method = serde_json::from_value(method).unwrap();
        assert!(matches!(
            broker.dispatch("example.demo", Command::RegisterApi { method }),
            Response::Ok
        ));
        let broker = Rc::new(RefCell::new(broker));
        let endpoint = Box::new(Endpoint {
            broker: broker.clone(),
            principal: COORDINATOR,
        });
        let api = unsafe { *amaterasu_extension_registry::amaterasu_module_v1() };
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
fn discovery_uses_actual_broker_approval_and_initiator() {
    let mut harness = Harness::new();
    let page = harness.call(
        "management",
        json!({"operation":"list","after":null,"limit":16,"revision":null}),
    );
    assert_eq!(page["ok"], true);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let denied = harness.call(
        "outsider",
        json!({"operation":"list","after":null,"limit":16,"revision":null}),
    );
    assert_eq!(denied["ok"], true);
    assert!(denied["items"].as_array().unwrap().is_empty());
    let stale = harness.call(
        "management",
        json!({"operation":"list","after":"x","limit":16,"revision":0}),
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
}
