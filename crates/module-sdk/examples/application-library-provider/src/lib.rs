//! Synthetic in-memory provider fixture. It does not start a real application.
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient,
    application_library::{
        provider::{Application, ArtworkChunk, ArtworkReference, DiscoveryPage},
        provider_adapter::{CallerPolicy, Identity, Provider, ProviderAdapter},
        provider_lifecycle::{
            Action, Observation, ProofKind, Request as LifecycleRequest, RetirementProof, State,
        },
    },
};
use serde::Deserialize;
use std::collections::BTreeMap;

const SERVICE: &str = "example.library.provider/1";
const COVER: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0,
    0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31, 0, 3, 3, 2, 0,
    239, 191, 105, 119, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    callers: BTreeMap<String, CallerPolicy>,
}

struct Fixture {
    incarnation: String,
    operations: BTreeMap<(String, String), (String, Option<String>, State)>,
}

fn random_token() -> Result<String, &'static str> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "entropy_unavailable")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl Fixture {
    fn new() -> Result<Self, &'static str> {
        Ok(Self {
            incarnation: random_token()?,
            operations: BTreeMap::new(),
        })
    }
}

impl Provider for Fixture {
    fn discover(
        &mut self,
        _: &Identity<'_>,
        cursor: Option<&str>,
        _: usize,
    ) -> Result<DiscoveryPage, &'static str> {
        if cursor.is_some() {
            return Err("invalid_cursor");
        }
        Ok(DiscoveryPage {
            revision: "fixture-v1".into(),
            entries: vec![Application {
                id: "fixture-game".into(),
                name: "Fixture Game".into(),
                artwork: Some(ArtworkReference {
                    id: "fixture-cover".into(),
                    revision: "fixture-v1".into(),
                }),
            }],
            next_cursor: None,
        })
    }

    fn artwork(
        &mut self,
        _: &Identity<'_>,
        reference: &ArtworkReference,
        offset: u64,
    ) -> Result<ArtworkChunk, &'static str> {
        if reference.id != "fixture-cover" || reference.revision != "fixture-v1" || offset != 0 {
            return Err("not_found");
        }
        Ok(ArtworkChunk {
            reference: reference.clone(),
            mime: "image/png".into(),
            offset,
            total_bytes: COVER.len() as u64,
            bytes: COVER.into(),
        })
    }

    fn incarnation(&self) -> &str {
        &self.incarnation
    }

    fn lifecycle(
        &mut self,
        identity: &Identity<'_>,
        request: LifecycleRequest,
    ) -> Result<Observation, &'static str> {
        let key = (identity.caller.to_owned(), request.operation_id.clone());
        if self
            .operations
            .get(&key)
            .is_some_and(|entry| entry.0 != request.application_id)
        {
            return Err("operation_conflict");
        }
        if !self.operations.contains_key(&key)
            && matches!(request.action, Action::Launch | Action::RecoverStop)
            && self.operations.len() >= 4096
        {
            return Err("capacity_exhausted");
        }
        match request.action {
            Action::Launch => {
                if request.application_id != "fixture-game" {
                    return Err("not_found");
                }
                if !self.operations.contains_key(&key) {
                    self.operations.insert(
                        key.clone(),
                        (
                            request.application_id.clone(),
                            Some(random_token()?),
                            State::Running,
                        ),
                    );
                }
            }
            Action::RecoverStop => {
                let entry = self
                    .operations
                    .entry(key.clone())
                    .or_insert_with(|| (request.application_id.clone(), None, State::Stopped));
                entry.2 = State::Stopped;
            }
            Action::Stop => {
                if let Some(entry) = self.operations.get_mut(&key) {
                    if entry.1 != request.ticket {
                        return Err("ticket_mismatch");
                    }
                    entry.2 = State::Stopped;
                }
            }
            Action::Lookup => {}
        }
        let (ticket, state) = self
            .operations
            .get(&key)
            .map_or((None, State::Unknown), |entry| {
                (entry.1.clone(), entry.2.clone())
            });
        let retirement_proof = (state == State::Stopped).then(|| RetirementProof {
            kind: ProofKind::FencedStopped,
            operation_id: request.operation_id.clone(),
            application_id: request.application_id.clone(),
            provider_incarnation: request.provider_incarnation.clone(),
            ticket: ticket.clone(),
        });
        Ok(Observation {
            operation_id: request.operation_id,
            application_id: request.application_id,
            provider_incarnation: request.provider_incarnation,
            ticket,
            state,
            retirement_proof,
        })
    }
}

struct Module {
    runtime: RuntimeClient,
    adapter: ProviderAdapter<Fixture>,
}
impl ManagedModule for Module {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("settings unavailable".into());
        };
        let settings: Settings =
            serde_json::from_value(payload).map_err(|error| error.to_string())?;
        let adapter = ProviderAdapter::with_policies(
            Fixture::new().map_err(str::to_owned)?,
            SERVICE.into(),
            settings.callers,
        )
        .map_err(str::to_owned)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self { runtime, adapter })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            let response = self.adapter.dispatch(&request);
            self.runtime.complete_serialized(request.id, &response)?;
        }
        Ok(())
    }
}
#[cfg(not(test))]
amaterasu_module_sdk::export_module!(Module);

#[cfg(test)]
mod launch_composition;

#[cfg(test)]
mod library_composition;

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::requests::IncomingRequest;
    use serde_json::{Value, json};
    use std::collections::BTreeSet;

    fn request(payload: Value) -> IncomingRequest<Value> {
        IncomingRequest {
            id: 9,
            caller: "library".into(),
            initiator: "consumer".into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: 100,
        }
    }

    #[test]
    fn registers_and_completes_discovery_from_broker_transport() {
        use amaterasu_module_sdk::abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK};
        use std::{cell::RefCell, collections::VecDeque, ffi::c_void};
        struct Host {
            commands: RefCell<Vec<Value>>,
            replies: RefCell<VecDeque<Value>>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            sink: ReplySink,
        ) -> i32 {
            // SAFETY: the fixture and input live throughout the synchronous call.
            let host = unsafe { &*context.cast::<Host>() };
            // SAFETY: the SDK provides readable input until this call returns.
            host.commands
                .borrow_mut()
                .push(serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap());
            let bytes =
                serde_json::to_vec(&host.replies.borrow_mut().pop_front().unwrap()).unwrap();
            // SAFETY: the SDK supplies a valid reply callback for this call.
            unsafe { sink.send(&bytes) };
            STATUS_OK
        }
        let host = Host {
            commands: RefCell::new(vec![]),
            replies: RefCell::new(VecDeque::from([
                json!({"kind":"result","payload":{"callers":{"library":{"initiators":["consumer"],"discovery":true}}}}),
                json!({"kind":"ok"}),
                json!({"kind":"request","id":9,"caller":"library","initiator":"consumer","service":SERVICE,"payload":{"action":"discover","cursor":null,"limit":1},"remaining_ms":100}),
                json!({"kind":"ok"}),
            ])),
        };
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&host as *const Host).cast_mut().cast(),
            invoke,
        };
        // SAFETY: the host fixture outlives the runtime and module calls.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = Module::start(runtime).unwrap();
        module.tick().unwrap();
        let commands = host.commands.borrow();
        assert_eq!(commands[1]["service"], SERVICE);
        assert_eq!(commands[3]["request"], 9);
        assert_eq!(commands[3]["payload"]["entries"][0]["id"], "fixture-game");
    }

    #[test]
    fn broker_routes_delegated_discovery_with_attested_identity() {
        use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
        use amaterasu_runtime::Broker;
        let grants = |calls: &[&str], provides: &[&str]| Grants {
            call: calls.iter().map(|name| (*name).to_owned()).collect(),
            provide: provides.iter().map(|name| (*name).to_owned()).collect(),
            ..Default::default()
        };
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            "consumer".into(),
            grants(&["amaterasu.library/1", SERVICE], &[]),
        );
        config.principals.insert(
            "library".into(),
            grants(&[SERVICE], &["amaterasu.library/1"]),
        );
        config
            .principals
            .insert("fixture".into(), grants(&[], &[SERVICE]));
        let mut broker = Broker::new(config).unwrap();
        assert!(matches!(
            broker.dispatch(
                "library",
                Command::Register {
                    service: "amaterasu.library/1".into()
                }
            ),
            Response::Ok
        ));
        assert!(matches!(
            broker.dispatch(
                "fixture",
                Command::Register {
                    service: SERVICE.into()
                }
            ),
            Response::Ok
        ));
        let payload = json!({"action":"discover","cursor":null,"limit":20});
        let Response::Accepted { id: parent } = broker.dispatch(
            "consumer",
            Command::Request {
                service: "amaterasu.library/1".into(),
                payload: payload.clone(),
                timeout_ms: 10_000,
                operation_id: "parent".into(),
                parent: None,
            },
        ) else {
            panic!("parent admission failed")
        };
        assert!(
            matches!(broker.dispatch("library", Command::Poll {}), Response::Request { id, .. } if id == parent)
        );
        let Response::Accepted { id: child } = broker.dispatch(
            "library",
            Command::Request {
                service: SERVICE.into(),
                payload,
                timeout_ms: 5_000,
                operation_id: "child".into(),
                parent: Some(parent),
            },
        ) else {
            panic!("child admission failed")
        };
        let Response::Request {
            id,
            caller,
            initiator,
            service,
            payload,
            remaining_ms,
        } = broker.dispatch("fixture", Command::Poll {})
        else {
            panic!("provider request missing")
        };
        assert_eq!(
            (&*caller, &*initiator, &*service),
            ("library", "consumer", SERVICE)
        );
        let mut adapter = ProviderAdapter::new(
            Fixture::new().unwrap(),
            SERVICE.into(),
            "library".into(),
            BTreeSet::from(["consumer".into()]),
        )
        .unwrap();
        let reply = adapter.dispatch(&IncomingRequest {
            id,
            caller,
            initiator,
            service,
            payload,
            remaining_ms,
        });
        assert!(matches!(
            broker.dispatch(
                "fixture",
                Command::Complete {
                    request: child,
                    payload: reply
                }
            ),
            Response::Ok
        ));
        let Response::Result { payload } =
            broker.dispatch("library", Command::TakeResult { request: child })
        else {
            panic!("provider result missing")
        };
        assert_eq!(payload["entries"][0]["id"], "fixture-game");
        assert_eq!(payload["revision"], "fixture-v1");
    }

    #[test]
    fn capability_policy_separates_discovery_and_lifecycle() {
        let mut adapter = ProviderAdapter::with_policies(
            Fixture::new().unwrap(),
            SERVICE.into(),
            BTreeMap::from([(
                "library".into(),
                CallerPolicy {
                    initiators: BTreeSet::from(["consumer".into()]),
                    discovery: true,
                    ..Default::default()
                },
            )]),
        )
        .unwrap();
        assert_eq!(
            adapter.dispatch(&request(json!({"action":"discover","limit":1})))["ok"],
            true
        );
        assert_eq!(
            adapter.dispatch(&request(json!({"action":"identity"})))["error"]["code"],
            "unauthorized"
        );
    }

    #[test]
    fn provider_routes_discovery_artwork_and_synthetic_lifecycle() {
        let mut adapter = ProviderAdapter::new(
            Fixture::new().unwrap(),
            SERVICE.into(),
            "library".into(),
            BTreeSet::from(["consumer".into(), "library".into()]),
        )
        .unwrap();
        let discover = adapter.dispatch(&request(
            json!({"action":"discover","cursor":null,"limit":20}),
        ));
        assert_eq!(discover["entries"][0]["id"], "fixture-game");
        let cover = adapter.dispatch(&request(json!({"action":"artwork","reference":{"id":"fixture-cover","revision":"fixture-v1"},"offset":0})));
        assert_eq!(cover["mime"], "image/png");
        assert!(cover["data_base64"].as_str().unwrap().starts_with("iVBOR"));
        let identity = adapter.dispatch(&request(json!({"action":"identity"})));
        let incarnation = identity["provider_incarnation"].as_str().unwrap();
        let launch = json!({"action":"launch","operation_id":"op1","application_id":"fixture-game","provider_incarnation":incarnation});
        let launched = adapter.dispatch(&request(launch.clone()));
        assert_eq!(launched["state"], "running");
        assert_eq!(
            adapter.dispatch(&request(launch))["ticket"],
            launched["ticket"]
        );
        let mut monitor = request(
            json!({"action":"lookup","operation_id":"op1","application_id":"fixture-game","provider_incarnation":incarnation}),
        );
        monitor.initiator = "library".into();
        assert_eq!(adapter.dispatch(&monitor)["ticket"], launched["ticket"]);
        let ticket = launched["ticket"].as_str().unwrap();
        let stop = json!({"action":"stop","operation_id":"op1","application_id":"fixture-game","provider_incarnation":incarnation,"ticket":ticket});
        let stopped = adapter.dispatch(&request(stop));
        assert_eq!(stopped["state"], "stopped");
        assert_eq!(stopped["retirement_proof"]["kind"], "fenced_stopped");
        let lookup = json!({"action":"lookup","operation_id":"missing","application_id":"fixture-game","provider_incarnation":incarnation});
        assert_eq!(adapter.dispatch(&request(lookup))["state"], "unknown");
        let recover = json!({"action":"recover_stop","operation_id":"missing","application_id":"fixture-game","provider_incarnation":incarnation});
        let fenced = adapter.dispatch(&request(recover));
        assert_eq!(fenced["state"], "stopped");
        assert!(fenced["ticket"].is_null());
        let delayed = json!({"action":"launch","operation_id":"missing","application_id":"fixture-game","provider_incarnation":incarnation});
        assert_eq!(adapter.dispatch(&request(delayed))["state"], "stopped");
        let mut restarted = ProviderAdapter::new(
            Fixture::new().unwrap(),
            SERVICE.into(),
            "library".into(),
            BTreeSet::from(["consumer".into()]),
        )
        .unwrap();
        let after_restart = adapter.dispatch(&request(json!({"action":"identity"})));
        let new_identity = restarted.dispatch(&request(json!({"action":"identity"})));
        assert_ne!(
            after_restart["provider_incarnation"],
            new_identity["provider_incarnation"]
        );
        assert_eq!(restarted.dispatch(&request(json!({"action":"recover_stop","operation_id":"op1","application_id":"fixture-game","provider_incarnation":incarnation})))["ok"], false);
        let stale = json!({"action":"launch","operation_id":"op2","application_id":"fixture-game","provider_incarnation":"old"});
        assert_eq!(adapter.dispatch(&request(stale))["ok"], false);
        let mut spoofed = request(json!({"action":"discover","limit":1}));
        spoofed.initiator = "other".into();
        assert_eq!(adapter.dispatch(&spoofed)["error"]["code"], "unauthorized");
        spoofed.initiator = "consumer".into();
        spoofed.caller = "other".into();
        assert_eq!(adapter.dispatch(&spoofed)["error"]["code"], "unauthorized");
        spoofed.caller = "library".into();
        spoofed.remaining_ms = 0;
        assert_eq!(
            adapter.dispatch(&spoofed)["error"]["code"],
            "deadline_expired"
        );
        assert_eq!(
            adapter.dispatch(&request(json!({"action":"discover","limit":33})))["error"]["code"],
            "invalid_request"
        );
        assert_eq!(
            adapter.dispatch(&request(
                json!({"action":"discover","limit":1,"initiator":"consumer"})
            ))["error"]["code"],
            "invalid_request"
        );
    }
}
