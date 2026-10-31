//! Synthetic in-memory provider fixture. It does not start a real application.
use amaterasu_module_sdk::{
    Command, ManagedModule, ModuleResult, Response, RuntimeClient,
    application_library::{
        provider::{
            Application, ArtworkChunk, ArtworkReference, DiscoveryPage, LifecycleRequest,
            LifecycleState,
        },
        provider_adapter::{Identity, Provider, ProviderAdapter},
    },
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const SERVICE: &str = "example.library.provider/1";
const COVER: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0,
    0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 252, 255, 31, 0, 3, 3, 2, 0,
    239, 191, 105, 119, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    library_caller: String,
    allowed_initiators: BTreeSet<String>,
}

#[derive(Default)]
struct Fixture {
    operations: BTreeMap<(String, String), (String, LifecycleState)>,
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

    fn lifecycle(
        &mut self,
        identity: &Identity<'_>,
        request: LifecycleRequest,
    ) -> Result<LifecycleState, &'static str> {
        match request {
            LifecycleRequest::Launch {
                operation_id,
                application_id,
            } => {
                if application_id != "fixture-game" {
                    return Err("not_found");
                }
                let key = (identity.initiator.to_owned(), operation_id);
                let entry = self
                    .operations
                    .entry(key)
                    .or_insert_with(|| (application_id.clone(), LifecycleState::Running));
                if entry.0 != application_id {
                    return Err("operation_conflict");
                }
                Ok(entry.1.clone())
            }
            LifecycleRequest::Lookup { operation_id } => Ok(self
                .operations
                .get(&(identity.initiator.to_owned(), operation_id))
                .map_or(LifecycleState::Unknown, |entry| entry.1.clone())),
            LifecycleRequest::Stop { operation_id } => {
                let Some(entry) = self
                    .operations
                    .get_mut(&(identity.initiator.to_owned(), operation_id))
                else {
                    return Ok(LifecycleState::Unknown);
                };
                entry.1 = LifecycleState::Stopped;
                Ok(entry.1.clone())
            }
        }
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
        let adapter = ProviderAdapter::new(
            Fixture::default(),
            SERVICE.into(),
            settings.library_caller,
            settings.allowed_initiators,
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
amaterasu_module_sdk::export_module!(Module);

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::requests::IncomingRequest;
    use serde_json::{Value, json};

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
                json!({"kind":"result","payload":{"library_caller":"library","allowed_initiators":["consumer"]}}),
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
    fn provider_routes_discovery_artwork_and_synthetic_lifecycle() {
        let mut adapter = ProviderAdapter::new(
            Fixture::default(),
            SERVICE.into(),
            "library".into(),
            BTreeSet::from(["consumer".into()]),
        )
        .unwrap();
        let discover = adapter.dispatch(&request(
            json!({"action":"discover","cursor":null,"limit":20}),
        ));
        assert_eq!(discover["entries"][0]["id"], "fixture-game");
        let cover = adapter.dispatch(&request(json!({"action":"artwork","reference":{"id":"fixture-cover","revision":"fixture-v1"},"offset":0})));
        assert_eq!(cover["mime"], "image/png");
        assert!(cover["data_base64"].as_str().unwrap().starts_with("iVBOR"));
        let launch =
            json!({"action":"launch","operation_id":"op1","application_id":"fixture-game"});
        assert_eq!(
            adapter.dispatch(&request(launch.clone()))["state"],
            "running"
        );
        assert_eq!(adapter.dispatch(&request(launch))["state"], "running");
        assert_eq!(
            adapter.dispatch(&request(json!({"action":"stop","operation_id":"op1"})))["state"],
            "stopped"
        );
        assert_eq!(
            adapter.dispatch(&request(json!({"action":"lookup","operation_id":"op1"})))["state"],
            "stopped"
        );
        assert_eq!(
            adapter.dispatch(&request(
                json!({"action":"lookup","operation_id":"missing"})
            ))["state"],
            "unknown"
        );
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
