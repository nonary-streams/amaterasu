//! Real broker and launch module, interchangeable libraries and synthetic executor.
//! No Catalog, protocol, networking, or packet-delivery service is registered.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_launch::LaunchModule;
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK,
};
use amaterasu_module_sdk::application_library::v2::{
    self, Capabilities, Cursor, Entry, Installed, LaunchTarget, Page, Query, Readiness,
    adapter::{CallerPolicy as LibraryPolicy, Library, LibraryAdapter},
};
use amaterasu_runtime::Broker;
use serde_json::{Value, json};
use std::{cell::RefCell, ffi::c_void, rc::Rc};

const LIBRARY: &str = "example.independent-library/2";
const LAUNCH: &str = "amaterasu.launch";
const SESSION: &str = "amaterasu.session/1";
const APPLICATION: &str = "amaterasu.application/1";

#[derive(Default)]
struct Session {
    key: String,
    state: String,
    lease: Option<String>,
}
struct Shared {
    broker: Broker,
    requests: Vec<(String, Value, Option<u64>)>,
    start_request: Option<u64>,
    lose_start_reply: bool,
    corrupt_target: bool,
    session: Session,
    preparation: Option<(String, &'static str)>,
    fail_preparation: bool,
}
struct Endpoint {
    name: &'static str,
    shared: Rc<RefCell<Shared>>,
}

unsafe extern "C" fn invoke(context: *mut c_void, input: BorrowedBytes, sink: ReplySink) -> i32 {
    // SAFETY: endpoint allocation and the SDK's input remain live throughout this call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    // SAFETY: the SDK lends readable input until this synchronous invocation returns.
    let mut command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let mut shared = endpoint.shared.borrow_mut();
    if endpoint.name == LAUNCH
        && let Command::TakeResult { request } = &command
        && shared.lose_start_reply
        && shared.start_request == Some(*request)
    {
        shared.lose_start_reply = false;
        return STATUS_INTERNAL_ERROR;
    }
    if endpoint.name == "library"
        && shared.corrupt_target
        && let Command::Complete { payload, .. } = &mut command
        && payload.get("target").is_some()
    {
        payload["target"]["executor"] = json!("unapproved");
    }
    let start = matches!(&command, Command::Request { service, payload, .. }
        if endpoint.name == LAUNCH && service == SERVICE && payload["action"] == "launch");
    if let Command::Request {
        service,
        payload,
        parent,
        ..
    } = &command
    {
        shared
            .requests
            .push((service.clone(), payload.clone(), *parent));
    }
    let reply = shared.broker.dispatch(endpoint.name, command);
    if start && let Response::Accepted { id } = reply {
        shared.start_request = Some(id);
    }
    let bytes = serde_json::to_vec(&reply).unwrap();
    drop(shared);
    // SAFETY: the SDK reply sink remains live until this synchronous call returns.
    unsafe { sink.send(&bytes) };
    STATUS_OK
}

fn endpoint(name: &'static str, shared: Rc<RefCell<Shared>>) -> Box<Endpoint> {
    Box::new(Endpoint { name, shared })
}
fn client(endpoint: &mut Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: the test retains the boxed endpoints until after dropping all modules.
    unsafe { RuntimeClient::from_endpoint(api) }
}
fn grants(call: &[&str], provide: &[&str]) -> Grants {
    Grants {
        call: call.iter().map(|s| (*s).into()).collect(),
        provide: provide.iter().map(|s| (*s).into()).collect(),
        ..Default::default()
    }
}

fn entry(id: &str) -> Entry {
    Entry {
        id: id.into(),
        revision: "entry-v7".into(),
        name: "Independent game".into(),
        description: Some("Public description".into()),
        tags: vec!["example".into()],
        source: None,
        portable_ids: vec![],
        installed: Installed::Unknown,
        readiness: Readiness::Ready,
        unavailable_reason: None,
    }
}
fn target(id: &str) -> LaunchTarget {
    LaunchTarget {
        id: id.into(),
        revision: "entry-v7".into(),
        executor: "runner".into(),
        application_id: "fixture-game".into(),
    }
}
fn policies() -> BTreeMap<String, LibraryPolicy> {
    BTreeMap::from([
        (
            "alice".into(),
            LibraryPolicy {
                initiators: ["alice".into()].into(),
                read: true,
                resolve_launch: false,
            },
        ),
        (
            LAUNCH.into(),
            LibraryPolicy {
                initiators: ["alice".into()].into(),
                read: false,
                resolve_launch: true,
            },
        ),
    ])
}

// A second library implementation has no snapshot database, source importer or
// shared implementation with the standalone example. The consumer is unchanged.
struct GeneratedLibrary;
impl Library for GeneratedLibrary {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            version: 2,
            features: [v2::RESOLVE_LAUNCH.into()].into(),
        }
    }
    fn list(
        &mut self,
        _: &Identity<'_>,
        _: &Query,
        cursor: Option<&Cursor>,
        _: usize,
    ) -> Result<Page, &'static str> {
        if cursor.is_some() {
            return Err("invalid_cursor");
        }
        Ok(Page {
            revision: "generated-v1".into(),
            entries: vec![entry("generated:application")],
            next_cursor: None,
        })
    }
    fn resolve(&mut self, _: &Identity<'_>, id: &str) -> Result<Entry, &'static str> {
        if id != "generated:application" {
            return Err("not_found");
        }
        Ok(entry(id))
    }
    fn resolve_launch(
        &mut self,
        identity: &Identity<'_>,
        id: &str,
        revision: &str,
    ) -> Result<LaunchTarget, &'static str> {
        self.resolve(identity, id)?;
        if revision != "entry-v7" {
            return Err("revision_conflict");
        }
        Ok(target(id))
    }
}
enum SelectedLibrary {
    Snapshot(example_application_library_v2::Module),
    Artifact(ArtifactLibrary),
    Generated {
        runtime: RuntimeClient,
        adapter: LibraryAdapter<GeneratedLibrary>,
    },
}
impl SelectedLibrary {
    fn tick(&mut self) {
        match self {
            Self::Snapshot(module) => module.tick().unwrap(),
            Self::Artifact(module) => {
                // SAFETY: the module and its library remain live and calls are serialized.
                assert_eq!(unsafe { (module.api.tick)(module.instance) }, STATUS_OK);
            }
            Self::Generated { runtime, adapter } => {
                if let Some(request) = runtime.poll_serialized().unwrap() {
                    runtime
                        .complete_serialized(request.id, &adapter.dispatch(&request))
                        .unwrap();
                }
            }
        }
    }
}

struct ArtifactLibrary {
    api: amaterasu_module_sdk::abi::ModuleApi,
    instance: *mut c_void,
    _library: libloading::Library,
}
impl ArtifactLibrary {
    fn load(path: &std::path::Path, endpoint: &mut Endpoint) -> Self {
        // SAFETY: this opt-in qualification loads the explicitly built trusted
        // fixture artifact. Its handle outlives every table pointer and instance.
        let library = unsafe { libloading::Library::new(path) }.unwrap();
        // SAFETY: the fixture exports the documented C ABI entrypoint.
        let entry = unsafe {
            library.get::<amaterasu_module_sdk::abi::ModuleEntry>(b"amaterasu_module_v1")
        }
        .unwrap();
        // SAFETY: the trusted fixture returns a live table for the library lifetime.
        let pointer = unsafe { entry() };
        assert!(!pointer.is_null());
        // SAFETY: the returned pointer contains at least an ABI header.
        assert!(unsafe {
            (*pointer.cast::<AbiHeader>()).supports::<amaterasu_module_sdk::abi::ModuleApi>()
        });
        // SAFETY: size/version were checked before copying the table.
        let api = unsafe { *pointer };
        let host = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (endpoint as *mut Endpoint).cast(),
            invoke,
        };
        let mut instance = std::ptr::null_mut();
        // SAFETY: host endpoint outlives the module; output is writable and no
        // other lifecycle invocation runs concurrently.
        assert_eq!(unsafe { (api.create)(&host, &mut instance) }, STATUS_OK);
        assert!(!instance.is_null());
        Self {
            api,
            instance,
            _library: library,
        }
    }
}
impl Drop for ArtifactLibrary {
    fn drop(&mut self) {
        // SAFETY: consume this library's live instance once before unloading it.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

struct Composition {
    shared: Rc<RefCell<Shared>>,
    // Drop modules before their endpoint allocations.
    launch: LaunchModule,
    library: SelectedLibrary,
    provider: Module,
    application: Option<ArtifactLibrary>,
    // HostApi context pointers must survive moving endpoints into this vector.
    #[allow(clippy::vec_box)]
    _endpoints: Vec<Box<Endpoint>>,
    request_sequence: u64,
}
impl Composition {
    fn new(generated: bool) -> Self {
        Self::with_artifact(generated, None)
    }

    fn with_artifact(generated: bool, artifact: Option<&std::path::Path>) -> Self {
        Self::with_preparation(generated, artifact, false, false)
    }

    fn with_preparation(
        generated: bool,
        artifact: Option<&std::path::Path>,
        use_preparation: bool,
        fail_preparation: bool,
    ) -> Self {
        Self::configure(
            generated,
            artifact,
            use_preparation,
            fail_preparation,
            None,
            false,
        )
    }

    fn configure(
        generated: bool,
        artifact: Option<&std::path::Path>,
        use_preparation: bool,
        fail_preparation: bool,
        real_preparation_log: Option<&std::path::Path>,
        hold_preparation: bool,
    ) -> Self {
        let mut config = RuntimeConfig::default();
        for principal in ["alice", "mallory"] {
            config.principals.insert(
                principal.into(),
                grants(
                    &[v2::LAUNCH_SERVICE, LIBRARY, SESSION, SERVICE, APPLICATION],
                    &[],
                ),
            );
        }
        config.principals.insert(
            LAUNCH.into(),
            grants(
                &[LIBRARY, SESSION, SERVICE, APPLICATION],
                &["amaterasu.launch/1", v2::LAUNCH_SERVICE],
            ),
        );
        config
            .principals
            .insert("library".into(), grants(&[], &[LIBRARY]));
        config
            .principals
            .insert("provider".into(), grants(&[], &[SERVICE]));
        config
            .principals
            .insert("session".into(), grants(&[], &[SESSION]));
        config
            .principals
            .insert("application".into(), grants(&[], &[APPLICATION]));
        let policy = if use_preparation {
            let command = |mark: &str| {
                let mut command = json!({"executable":std::env::current_exe().unwrap()});
                if let Some(path) = real_preparation_log {
                    command["args"] = json!([
                        "--exact",
                        "library_composition::preparation_subprocess",
                        "--ignored"
                    ]);
                    command["environment"] =
                        json!({"AMATERASU_TEST_LOG":path,"AMATERASU_TEST_MARK":mark});
                }
                command
            };
            let mut preparation = vec![json!({"run":command("run"),"undo":command("undo")})];
            if fail_preparation && real_preparation_log.is_some() {
                preparation.push(json!({"run":command("fail")}));
            }
            if hold_preparation && real_preparation_log.is_some() {
                preparation.push(json!({"run":command("hold")}));
            }
            let settings = json!({"revision":"entry-v7","preparation":preparation});
            json!({"opaque/library/game":settings,"generated:application":settings})
        } else {
            json!({})
        };
        config.settings.insert(
            LAUNCH.into(),
            json!({"library_routes":{"games":{
            "service":LIBRARY,"callers":["alice"],"executors":{"runner":SERVICE},
            "require_application_policy":use_preparation,"application_policies":policy}}}),
        );
        config.settings.insert(
            "provider".into(),
            json!({"callers":{LAUNCH:{"initiators":["alice",LAUNCH],"lifecycle":true}}}),
        );
        config.settings.insert("library".into(), json!({"service":LIBRARY,
            "callers":{"alice":{"initiators":["alice"],"read":true},LAUNCH:{"initiators":["alice"],"resolve_launch":true}},
            "revision":"snapshot-v1","entries":[entry("opaque/library/game"),entry("unrelated")],
            "targets":[target("opaque/library/game")]}));
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(
                "session",
                Command::Register {
                    service: SESSION.into()
                }
            ),
            Response::Ok
        ));
        if real_preparation_log.is_none() {
            assert!(matches!(
                broker.dispatch(
                    "application",
                    Command::Register {
                        service: APPLICATION.into()
                    }
                ),
                Response::Ok
            ));
        }
        let shared = Rc::new(RefCell::new(Shared {
            broker,
            requests: vec![],
            start_request: None,
            lose_start_reply: false,
            corrupt_target: false,
            session: Session::default(),
            preparation: None,
            fail_preparation,
        }));
        let mut launch_endpoint = endpoint(LAUNCH, shared.clone());
        let mut library_endpoint = endpoint("library", shared.clone());
        let mut provider_endpoint = endpoint("provider", shared.clone());
        let mut application_endpoint = endpoint("application", shared.clone());
        let launch = LaunchModule::start(client(&mut launch_endpoint)).unwrap();
        let library = if let Some(path) = artifact {
            SelectedLibrary::Artifact(ArtifactLibrary::load(path, &mut library_endpoint))
        } else if generated {
            let runtime = client(&mut library_endpoint);
            runtime
                .require_success(Command::Register {
                    service: LIBRARY.into(),
                })
                .unwrap();
            SelectedLibrary::Generated {
                runtime,
                adapter: LibraryAdapter::new(GeneratedLibrary, LIBRARY.into(), policies()).unwrap(),
            }
        } else {
            SelectedLibrary::Snapshot(
                example_application_library_v2::Module::start(client(&mut library_endpoint))
                    .unwrap(),
            )
        };
        let provider = Module::start(client(&mut provider_endpoint)).unwrap();
        let application = real_preparation_log.map(|_| {
            let path = std::env::var_os("AMATERASU_APPLICATION_ARTIFACT")
                .expect("independently built application artifact");
            ArtifactLibrary::load(std::path::Path::new(&path), &mut application_endpoint)
        });
        Self {
            shared,
            launch,
            library,
            provider,
            application,
            _endpoints: vec![
                launch_endpoint,
                library_endpoint,
                provider_endpoint,
                application_endpoint,
            ],
            request_sequence: 0,
        }
    }

    fn tick(&mut self) {
        self.launch.tick().unwrap();
        self.library.tick();
        self.provider.tick().unwrap();
        if let Some(application) = &mut self.application {
            // SAFETY: the loaded artifact and its instance remain live and are
            // accessed serially until the composition has been dropped.
            assert_eq!(
                unsafe { (application.api.tick)(application.instance) },
                STATUS_OK
            );
        }
        let mut shared = self.shared.borrow_mut();
        if self.application.is_none()
            && let Response::Request { id, payload, .. } =
                shared.broker.dispatch("application", Command::Poll {})
        {
            let action = payload["action"].as_str().unwrap();
            let operation = payload["operation_id"].as_str().unwrap().to_owned();
            let state = match action {
                "prepare" => {
                    let state = if shared.fail_preparation {
                        "preparation_failed"
                    } else {
                        "ready"
                    };
                    shared.preparation = Some((operation.clone(), state));
                    state
                }
                "preparation_stop" => {
                    let (_, state) = shared.preparation.as_mut().expect("owned preparation");
                    *state = if *state == "preparation_failed" {
                        "preparation_failed"
                    } else {
                        "complete"
                    };
                    *state
                }
                "preparation_status" => shared.preparation.as_ref().expect("owned preparation").1,
                other => panic!("unexpected application action {other}"),
            };
            let reply = json!({"ok":true,"preparation":{"operation_id":operation,"state":state,
                "preparation":{"completed":1},"cleanup":{"state":if state == "complete" {"complete"} else {"pending"}}}});
            assert!(matches!(
                shared.broker.dispatch(
                    "application",
                    Command::Complete {
                        request: id,
                        payload: reply
                    }
                ),
                Response::Ok
            ));
        }
        let Response::Request { id, payload, .. } =
            shared.broker.dispatch("session", Command::Poll {})
        else {
            return;
        };
        let session = &mut shared.session;
        match payload["action"].as_str().unwrap() {
            "create" => {
                session.key = payload["key"].as_str().unwrap().into();
                session.state = "preparing".into();
            }
            "acquire" => session.lease = Some(payload["resource"].as_str().unwrap().into()),
            "activate" => session.state = "active".into(),
            "stop" => session.state = "stopping".into(),
            "finish_stop" | "cancel" | "fail" => {
                session.state = "stopped".into();
                session.lease = None;
            }
            "lookup" => {}
            other => panic!("unexpected session action {other}"),
        }
        let leases: BTreeMap<_, _> = session.lease.iter().map(|key| (key, "exclusive")).collect();
        let payload = json!({"ok":true,"revision":1,"sessions":[{"id":1,"key":session.key,"state":session.state,"leases":leases}]});
        assert!(matches!(
            shared.broker.dispatch(
                "session",
                Command::Complete {
                    request: id,
                    payload
                }
            ),
            Response::Ok
        ));
    }

    fn request(&mut self, caller: &str, service: &str, payload: Value) -> Value {
        self.request_sequence += 1;
        let Response::Accepted { id } = self.shared.borrow_mut().broker.dispatch(
            caller,
            Command::Request {
                service: service.into(),
                payload,
                timeout_ms: 30_000,
                operation_id: format!("consumer-{}", self.request_sequence),
                parent: None,
            },
        ) else {
            panic!("request was not admitted");
        };
        for _ in 0..1000 {
            self.tick();
            if self.application.is_some() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let reply = self
                .shared
                .borrow_mut()
                .broker
                .dispatch(caller, Command::TakeResult { request: id });
            if let Response::Result { payload } = reply {
                assert!(matches!(
                    self.shared
                        .borrow_mut()
                        .broker
                        .dispatch(caller, Command::ReleaseResult { request: id }),
                    Response::Ok
                ));
                return payload;
            }
            assert!(matches!(reply, Response::Pending), "{reply:?}");
        }
        panic!("composition stalled");
    }

    fn selection(&mut self) -> Value {
        let description = self.request("alice", LIBRARY, json!({"action":"describe"}));
        assert_eq!(description["capabilities"]["version"], 2);
        assert!(
            description["capabilities"]["features"]
                .as_array()
                .unwrap()
                .contains(&json!(v2::RESOLVE_LAUNCH))
        );
        let page = self.request(
            "alice",
            LIBRARY,
            json!({"action":"list","query":{"text":""},"cursor":null,"limit":1}),
        );
        let entry = &page["page"]["entries"][0];
        assert_eq!(entry["installed"], "unknown");
        assert!(entry.get("command").is_none());
        json!({"action":"launch","operation_id":"owned-operation",
            "application":{"library":"games","id":entry["id"]},"revision":entry["revision"]})
    }
}

#[test]
fn two_library_implementations_browse_launch_retry_and_stop_without_catalog() {
    for generated in [false, true] {
        let mut composition = Composition::new(generated);
        let launch = composition.selection();
        let started = composition.request("alice", v2::LAUNCH_SERVICE, launch.clone());
        assert_eq!(started["launch"]["state"], "active", "{started}");
        let id = started["launch"]["launch_id"].clone();
        assert_eq!(
            composition.request("alice", v2::LAUNCH_SERVICE, launch.clone())["launch"]["launch_id"],
            id
        );
        let mut conflict = launch.clone();
        conflict["revision"] = json!("different");
        assert_eq!(
            composition.request("alice", v2::LAUNCH_SERVICE, conflict)["code"],
            "operation_conflict"
        );
        assert_eq!(
            composition.request(
                "mallory",
                v2::LAUNCH_SERVICE,
                json!({"action":"stop","launch_id":id})
            )["ok"],
            false
        );
        let stopped = composition.request(
            "alice",
            v2::LAUNCH_SERVICE,
            json!({"action":"stop","launch_id":id}),
        );
        assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
        assert!(composition.shared.borrow().session.lease.is_none());
        let state = composition.shared.borrow();
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|(service, payload, _)| service == SERVICE && payload["action"] == "launch")
                .count(),
            1
        );
        assert!(
            state
                .requests
                .iter()
                .any(|(service, payload, parent)| service == LIBRARY
                    && payload["action"] == "resolve_launch"
                    && parent.is_some())
        );
        assert!(
            !state
                .requests
                .iter()
                .any(|(service, _, _)| service.contains("catalog") || service.contains("nvhttp"))
        );
    }
}

#[test]
fn selected_preparation_is_preserved_across_library_replacements_and_failure() {
    for generated in [false, true] {
        for fail_preparation in [false, true] {
            let mut composition =
                Composition::with_preparation(generated, None, true, fail_preparation);
            let launch = composition.selection();
            let result = composition.request("alice", v2::LAUNCH_SERVICE, launch);
            if fail_preparation {
                assert_eq!(result["launch"]["state"], "failed", "{result}");
            } else {
                assert_eq!(result["launch"]["state"], "active", "{result}");
                let stopped = composition.request(
                    "alice",
                    v2::LAUNCH_SERVICE,
                    json!({"action":"stop","launch_id":result["launch"]["launch_id"]}),
                );
                assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
            }
            let state = composition.shared.borrow();
            assert!(state.session.lease.is_none());
            let actions: Vec<_> = state
                .requests
                .iter()
                .map(|(service, payload, _)| {
                    (service.as_str(), payload["action"].as_str().unwrap())
                })
                .collect();
            let prepare = actions
                .iter()
                .position(|action| *action == (APPLICATION, "prepare"))
                .unwrap();
            let cleanup = actions
                .iter()
                .position(|action| *action == (APPLICATION, "preparation_stop"))
                .unwrap();
            assert!(prepare < cleanup);
            let provider = actions
                .iter()
                .position(|action| *action == (SERVICE, "launch"));
            if fail_preparation {
                assert!(provider.is_none());
            } else {
                assert!(prepare < provider.unwrap());
                let provider_stop = actions
                    .iter()
                    .position(|action| *action == (SERVICE, "recover_stop"))
                    .unwrap();
                assert!(provider_stop < cleanup);
            }
        }
    }
}

#[test]
#[ignore = "subprocess fixture invoked by real_preparation_is_retired_after_failure_and_stop"]
fn preparation_subprocess() {
    use std::io::Write;
    let path = std::env::var_os("AMATERASU_TEST_LOG").unwrap();
    let mark = std::env::var("AMATERASU_TEST_MARK").unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{mark}").unwrap();
    if mark == "hold" {
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
    assert_ne!(mark, "fail", "requested preparation failure");
}

#[test]
#[ignore = "requires AMATERASU_APPLICATION_ARTIFACT pointing to the separately built application cdylib"]
fn real_preparation_is_retired_after_failure_and_stop() {
    for generated in [false, true] {
        for fail in [false, true] {
            let unique = format!(
                "amaterasu-preparation-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let dir = std::env::temp_dir().join(unique);
            std::fs::create_dir(&dir).unwrap();
            let log = dir.join("steps.log");
            let artifact = std::env::var_os("AMATERASU_LIBRARY_V2_ARTIFACT");
            let artifact = if generated {
                None
            } else {
                artifact.as_deref().map(std::path::Path::new)
            };
            let mut composition =
                Composition::configure(generated, artifact, true, fail, Some(&log), false);
            let launch = composition.selection();
            let result = composition.request("alice", v2::LAUNCH_SERVICE, launch);
            if fail {
                assert_eq!(result["launch"]["state"], "failed", "{result}");
            } else {
                assert_eq!(result["launch"]["state"], "active", "{result}");
                let stopped = composition.request(
                    "alice",
                    v2::LAUNCH_SERVICE,
                    json!({"action":"stop","launch_id":result["launch"]["launch_id"]}),
                );
                assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
            }
            assert!(composition.shared.borrow().session.lease.is_none());
            let output = std::fs::read_to_string(&log).unwrap();
            let expected = if fail {
                "run\nfail\nundo\n"
            } else {
                "run\nundo\n"
            };
            assert_eq!(output, expected);
            drop(composition);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
#[ignore = "requires AMATERASU_APPLICATION_ARTIFACT pointing to the separately built application cdylib"]
fn real_preparation_cancellation_undoes_completed_steps_before_lease_release() {
    let unique = format!(
        "amaterasu-cancel-preparation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir(&dir).unwrap();
    let log = dir.join("steps.log");
    let mut composition = Composition::configure(false, None, true, false, Some(&log), true);
    let launch = composition.selection();
    let Response::Accepted { id } = composition.shared.borrow_mut().broker.dispatch(
        "alice",
        Command::Request {
            service: v2::LAUNCH_SERVICE.into(),
            payload: launch,
            timeout_ms: 30_000,
            operation_id: "cancel-request".into(),
            parent: None,
        },
    ) else {
        panic!("launch admission failed")
    };
    let mut saw_hold = false;
    for _ in 0..3000 {
        composition.tick();
        if std::fs::read_to_string(&log).is_ok_and(|contents| contents.contains("hold\n")) {
            saw_hold = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(saw_hold, "preparation did not reach the held step");
    assert!(matches!(
        composition
            .shared
            .borrow_mut()
            .broker
            .dispatch("alice", Command::Cancel { request: id }),
        Response::Ok
    ));
    let mut retired = false;
    for _ in 0..3000 {
        composition.tick();
        let state = composition.shared.borrow();
        let stopped = state.requests.iter().any(|(service, payload, _)| {
            service == APPLICATION && payload["action"] == "preparation_stop"
        });
        if stopped && state.session.lease.is_none() {
            retired = true;
            break;
        }
        drop(state);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(retired, "preparation was not retired");
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "run\nhold\nundo\n");
    assert!(
        !composition
            .shared
            .borrow()
            .requests
            .iter()
            .any(|(service, payload, _)| service == SERVICE && payload["action"] == "launch")
    );
    drop(composition);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unauthorized_stale_and_unapproved_handoffs_fail_before_execution() {
    for (caller, stale, corrupt) in [
        ("mallory", false, false),
        ("alice", true, false),
        ("alice", false, true),
    ] {
        let mut composition = Composition::new(false);
        let mut launch = composition.selection();
        if stale {
            launch["revision"] = json!("old");
        }
        composition.shared.borrow_mut().corrupt_target = corrupt;
        let reply = composition.request(caller, v2::LAUNCH_SERVICE, launch);
        assert!(
            reply["ok"] == false || reply["launch"]["state"] == "failed",
            "{reply}"
        );
        let state = composition.shared.borrow();
        assert!(state.session.key.is_empty());
        assert!(
            !state
                .requests
                .iter()
                .any(|(service, _, _)| service == SERVICE)
        );
    }
}

#[test]
fn lost_executor_acknowledgment_reconciles_and_retires_before_releasing_lease() {
    let mut composition = Composition::new(false);
    let launch = composition.selection();
    composition.shared.borrow_mut().lose_start_reply = true;
    let failed = composition.request("alice", v2::LAUNCH_SERVICE, launch.clone());
    assert_eq!(failed["launch"]["state"], "failed", "{failed}");
    assert!(composition.shared.borrow().session.lease.is_none());
    let replay = composition.request("alice", v2::LAUNCH_SERVICE, launch);
    assert_eq!(replay["launch"]["launch_id"], failed["launch"]["launch_id"]);
    let state = composition.shared.borrow();
    let actions: Vec<_> = state
        .requests
        .iter()
        .filter(|(service, _, _)| service == SERVICE)
        .map(|(_, payload, _)| payload["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["identity", "launch", "recover_stop"]);
}

#[test]
#[ignore = "requires AMATERASU_LIBRARY_V2_ARTIFACT pointing to the separately built fixture cdylib"]
fn separately_built_library_artifact_browses_launches_and_stops() {
    let artifact = std::env::var_os("AMATERASU_LIBRARY_V2_ARTIFACT").expect("artifact path");
    let mut composition =
        Composition::with_preparation(false, Some(std::path::Path::new(&artifact)), true, false);
    let launch = composition.selection();
    let started = composition.request("alice", v2::LAUNCH_SERVICE, launch);
    assert_eq!(started["launch"]["state"], "active", "{started}");
    let stopped = composition.request(
        "alice",
        v2::LAUNCH_SERVICE,
        json!({"action":"stop","launch_id":started["launch"]["launch_id"]}),
    );
    assert_eq!(stopped["launch"]["state"], "stopped", "{stopped}");
    assert!(composition.shared.borrow().session.lease.is_none());
    let state = composition.shared.borrow();
    assert!(
        state
            .requests
            .iter()
            .any(|(service, payload, _)| service == APPLICATION && payload["action"] == "prepare")
    );
    assert!(
        state
            .requests
            .iter()
            .any(|(service, payload, _)| service == APPLICATION
                && payload["action"] == "preparation_stop")
    );
}
