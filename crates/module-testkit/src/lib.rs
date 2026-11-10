//! Run a module against the real control runtime inside a test.
//!
//! A [`TestHost`] is the same runtime the installed host loads, created in the
//! test process: grants are enforced, requests are routed and deduplicated,
//! deadlines expire and direct calls reach their provider. A test describes
//! the principals and their grants, then either loads a module through its
//! exported lifecycle ([`TestHost::load`]) or takes a [`RuntimeClient`] and
//! plays a peer by hand ([`TestHost::client`]).
//!
//! ```ignore
//! let host = TestHost::builder()
//!     .principal("example.echo", grants().provide("example.echo/1"))
//!     .principal("console", grants().call("example.echo/1"))
//!     .build();
//! let mut echo = host.load::<EchoModule>("example.echo");
//! let console = host.client("console");
//! let mut call = OutboundCall::request(options, &json!({"text": "hi"})).unwrap();
//! let outcome = settle(&console, &mut call, || echo.tick().unwrap());
//! ```
//!
//! This exercises routing and authority. It does not load a signed package,
//! verify a manifest or start a worker process.
//!
//! The builder also composes what trusted policy composes on an installed
//! host: [`layers`](TestHostBuilder::layers) stacked on a service and the
//! [`extension_point`](TestHostBuilder::extension_point)s an owner runs.
//!
//! [`ScriptedHost`] is the alternative for faults the real runtime will not
//! produce on request, such as a reply lost in transport.
mod scripted;

pub use scripted::ScriptedHost;

use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, RuntimeApi, RuntimeDirectApi, STATUS_OK, Status,
};
use amaterasu_module_sdk::hooks::{HookOutcome, HookRun};
use amaterasu_module_sdk::requests::{CallOutcome, OutboundCall};
use amaterasu_module_sdk::{ManagedModule, RuntimeClient, exports};
use amaterasu_runtime::{Broker, amaterasu_runtime_direct_v1, amaterasu_runtime_v1};
use serde_json::Value;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::task::Poll;
use std::time::{Duration, Instant};

/// The policy types a test composes a runtime from.
pub use amaterasu_contracts::{
    ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema, Grants, HookSpec,
    RuntimeConfig,
};

/// How long [`settle`] keeps polling. A call still pending after this is not
/// being advanced by the test; failing beats waiting out a long deadline.
const SETTLE_LIMIT: Duration = Duration::from_secs(10);

/// Starts an empty set of grants: `grants().provide("a/1").call("b/1")`.
pub fn grants() -> GrantSet {
    GrantSet::default()
}

/// The broker operations a principal is allowed, built one name at a time.
#[derive(Clone, Debug, Default)]
pub struct GrantSet(Grants);

impl GrantSet {
    /// May register as the provider of `service`.
    pub fn provide(mut self, service: &str) -> Self {
        self.0.provide.insert(service.to_owned());
        self
    }

    /// May send requests and direct calls to `service`.
    pub fn call(mut self, service: &str) -> Self {
        self.0.call.insert(service.to_owned());
        self
    }

    /// May publish events on `topic`.
    pub fn publish(mut self, topic: &str) -> Self {
        self.0.publish.insert(topic.to_owned());
        self
    }

    /// May subscribe to events on `topic`.
    pub fn subscribe(mut self, topic: &str) -> Self {
        self.0.subscribe.insert(topic.to_owned());
        self
    }

    /// May register as a participant of `hook`.
    pub fn hook(mut self, hook: &str) -> Self {
        self.0.hook.insert(hook.to_owned());
        self
    }

    /// May register a native factory for `service`.
    pub fn native_provide(mut self, service: &str) -> Self {
        self.0.native_provide.insert(service.to_owned());
        self
    }

    /// May bind the native interface of `service`.
    pub fn native_use(mut self, service: &str) -> Self {
        self.0.native_use.insert(service.to_owned());
        self
    }
}

impl From<GrantSet> for Grants {
    fn from(set: GrantSet) -> Self {
        set.0
    }
}

/// Describes the principals, grants and settings of a test runtime.
#[derive(Default)]
pub struct TestHostBuilder {
    configuration: RuntimeConfig,
}

impl TestHostBuilder {
    /// Add an endpoint identity and what it may do. A module is loaded, and a
    /// peer client is taken, under one of these names.
    pub fn principal(mut self, name: &str, grants: impl Into<Grants>) -> Self {
        self.configuration
            .principals
            .insert(name.to_owned(), grants.into());
        self
    }

    /// The startup settings `principal` reads with `RuntimeClient::settings`.
    pub fn settings(mut self, principal: &str, settings: Value) -> Self {
        self.configuration
            .settings
            .insert(principal.to_owned(), settings);
        self
    }

    /// Approve one versioned API method, its provider and its callers.
    pub fn api_method(mut self, approval: ApiMethodApproval) -> Self {
        self.configuration.api_methods.push(approval);
        self
    }

    /// Declare a hook, its owner and its ordered participants.
    pub fn hook(mut self, name: &str, specification: HookSpec) -> Self {
        self.configuration
            .hooks
            .insert(name.to_owned(), specification);
        self
    }

    /// Declare a hook that `owner` runs and `participants` answer in this
    /// order, with a one-second deadline for the whole run. Each participant
    /// also needs the hook's grant: `grants().hook(name)`.
    pub fn extension_point(self, name: &str, owner: &str, participants: &[&str]) -> Self {
        let specification = HookSpec {
            owner: owner.to_owned(),
            participants: participants.iter().map(|name| (*name).to_owned()).collect(),
            timeout_ms: 1_000,
        };
        self.hook(name, specification)
    }

    /// Stack `layers` on `service`, outermost first. A consumer's call reaches
    /// the first layer, and each layer's own call to the service reaches the
    /// next one, then the provider. A layer needs the service's `call` grant
    /// to pass a request on.
    pub fn layers(mut self, service: &str, layers: &[&str]) -> Self {
        self.configuration.service_layers.insert(
            service.to_owned(),
            layers.iter().map(|layer| (*layer).to_owned()).collect(),
        );
        self
    }

    /// Adjust any other part of the runtime policy.
    pub fn configure(mut self, adjust: impl FnOnce(&mut RuntimeConfig)) -> Self {
        adjust(&mut self.configuration);
        self
    }

    /// Create the runtime. Panics with the broker's reason when the policy is
    /// not one a real host could load.
    pub fn build(self) -> TestHost {
        let configuration = self.configuration;
        if let Err(reason) = Broker::new(configuration.clone()) {
            panic!("test runtime policy is invalid: {reason}");
        }
        let encoded = serde_json::to_vec(&configuration).expect("runtime policy serializes");
        let mut runtime = std::ptr::null_mut();
        // SAFETY: the configuration bytes and the output slot live across the
        // call, and the table belongs to the runtime linked into this test.
        let status = unsafe { (runtime_api().create)(BorrowedBytes::new(&encoded), &mut runtime) };
        assert_eq!(status, STATUS_OK, "the runtime rejected its configuration");
        assert!(!runtime.is_null(), "the runtime returned no instance");
        TestHost { runtime }
    }
}

fn runtime_api() -> &'static RuntimeApi {
    // SAFETY: the entry point returns the address of an immutable static table.
    unsafe { &*amaterasu_runtime_v1() }
}

fn direct_api() -> &'static RuntimeDirectApi {
    // SAFETY: the entry point returns the address of an immutable static table.
    unsafe { &*amaterasu_runtime_direct_v1() }
}

/// A control runtime living inside the test process.
///
/// The runtime is never destroyed: it is deliberately leaked so that every
/// client and module created from it stays valid however the test orders
/// their drops. A test process ends shortly afterwards.
pub struct TestHost {
    runtime: *mut c_void,
}

impl TestHost {
    pub fn builder() -> TestHostBuilder {
        TestHostBuilder::default()
    }

    /// A client bound to `principal`, with exactly that principal's grants.
    /// Use it to start a module directly or to act as one of its peers.
    pub fn client(&self, principal: &str) -> RuntimeClient {
        // SAFETY: the runtime is never destroyed, so it outlives the client,
        // and its endpoints accept concurrent calls from any thread.
        unsafe { RuntimeClient::from_endpoint(self.endpoint(principal)) }
    }

    /// Start a module under `principal` through the lifecycle table it would
    /// export, including panic containment and direct-call registration.
    pub fn load<M: ManagedModule>(&self, principal: &str) -> LoadedModule<M> {
        match self.try_load(principal) {
            Ok(module) => module,
            Err(status) => panic!("module {principal} failed to start with status {status}"),
        }
    }

    /// Like [`load`](Self::load), for a test that expects startup to fail.
    pub fn try_load<M: ManagedModule>(&self, principal: &str) -> Result<LoadedModule<M>, Status> {
        let endpoint = self.endpoint(principal);
        let mut instance = std::ptr::null_mut();
        // SAFETY: the endpoint table and the output slot are valid for the
        // call, and the leaked runtime outlives the module.
        let status = unsafe { exports::create::<M>(&endpoint, &mut instance) };
        if status != STATUS_OK {
            return Err(status);
        }
        // SAFETY: the instance came from `create::<M>` on this thread and
        // stays live until `LoadedModule` unregisters it and destroys it.
        let registered = unsafe {
            (direct_api().register)(
                self.runtime,
                BorrowedBytes::new(principal.as_bytes()),
                instance,
                exports::direct::<M>,
            )
        };
        assert_eq!(
            registered, STATUS_OK,
            "the runtime refused direct calls for {principal}"
        );
        Ok(LoadedModule {
            instance,
            runtime: self.runtime,
            principal: principal.to_owned(),
            module: PhantomData,
        })
    }

    fn endpoint(&self, principal: &str) -> HostApi {
        let mut endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: std::ptr::null_mut(),
            invoke: refuse_unbound_endpoint,
        };
        // SAFETY: the runtime is live, the name is readable for the call and
        // the output is a complete, writable table.
        let status = unsafe {
            (runtime_api().endpoint)(
                self.runtime,
                BorrowedBytes::new(principal.as_bytes()),
                &mut endpoint,
            )
        };
        assert_eq!(
            status, STATUS_OK,
            "{principal} is not a principal of this test runtime"
        );
        endpoint
    }
}

/// Placeholder for an endpoint table before the runtime fills it in.
unsafe extern "C" fn refuse_unbound_endpoint(
    _context: *mut c_void,
    _input: BorrowedBytes,
    _reply: amaterasu_module_sdk::abi::ReplySink,
) -> Status {
    amaterasu_module_sdk::abi::STATUS_INTERNAL_ERROR
}

/// A module started through its exported lifecycle. Every method makes the
/// call the host makes; a failure is the status the host would see.
pub struct LoadedModule<M: ManagedModule> {
    instance: *mut c_void,
    runtime: *mut c_void,
    principal: String,
    module: PhantomData<M>,
}

impl<M: ManagedModule> LoadedModule<M> {
    /// One host tick.
    pub fn tick(&mut self) -> Result<(), Status> {
        // SAFETY: the instance is live and this exclusive borrow serializes
        // lifecycle calls, as the host does.
        succeeded(unsafe { exports::tick::<M>(self.instance) })
    }

    /// Tick `count` times, failing the test on the first failed tick.
    pub fn tick_times(&mut self, count: usize) {
        for _ in 0..count {
            if let Err(status) = self.tick() {
                panic!("module {} tick failed with status {status}", self.principal);
            }
        }
    }

    pub fn health(&mut self) -> Result<(), Status> {
        // SAFETY: as for `tick`.
        succeeded(unsafe { exports::health::<M>(self.instance) })
    }

    /// Announce shutdown with the remaining host budget.
    pub fn prepare_shutdown(&mut self, remaining_ms: u64) -> Result<(), Status> {
        // SAFETY: as for `tick`.
        succeeded(unsafe { exports::prepare_shutdown::<M>(self.instance, remaining_ms) })
    }

    /// One quiescence poll: whether the module is ready to be destroyed.
    pub fn quiesce(&mut self) -> Result<bool, Status> {
        let mut ready = 0;
        // SAFETY: as for `tick`; the readiness byte is writable for the call.
        succeeded(unsafe { exports::quiesce::<M>(self.instance, &mut ready) })?;
        Ok(ready == 1)
    }
}

impl<M: ManagedModule> Drop for LoadedModule<M> {
    fn drop(&mut self) {
        // SAFETY: the leaked runtime is live. Once `unregister` returns the
        // runtime never calls the instance again, so it can be destroyed; this
        // is the order the host uses.
        unsafe {
            (direct_api().unregister)(self.runtime, BorrowedBytes::new(self.principal.as_bytes()));
            exports::destroy::<M>(self.instance);
        }
    }
}

fn succeeded(status: Status) -> Result<(), Status> {
    if status == STATUS_OK {
        Ok(())
    } else {
        Err(status)
    }
}

/// Poll `call` through `runtime` until it settles, running `advance` between
/// polls. `advance` normally ticks the modules under test.
pub fn settle(
    runtime: &RuntimeClient,
    call: &mut OutboundCall,
    mut advance: impl FnMut(),
) -> CallOutcome {
    let started = Instant::now();
    while started.elapsed() < SETTLE_LIMIT {
        if let Poll::Ready(outcome) = call.poll(runtime) {
            return outcome;
        }
        advance();
        std::thread::yield_now();
    }
    panic!(
        "call {} did not settle; is its provider being ticked?",
        call.operation_id()
    );
}

/// Poll a hook run through its owner's `runtime` until it settles, running
/// `advance` between polls. `advance` normally ticks the participants.
pub fn settle_hook(
    runtime: &RuntimeClient,
    run: &mut HookRun,
    mut advance: impl FnMut(),
) -> HookOutcome {
    let started = Instant::now();
    while started.elapsed() < SETTLE_LIMIT {
        if let Poll::Ready(outcome) = run.poll(runtime) {
            return outcome;
        }
        advance();
        std::thread::yield_now();
    }
    panic!(
        "hook run {} did not settle; are its participants being ticked?",
        run.operation_id()
    );
}
