use super::*;
use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;

#[derive(Default)]
struct FakeHost {
    incoming: VecDeque<Response>,
    results: BTreeMap<u64, Value>,
    commands: Vec<Command>,
    completions: BTreeMap<u64, Value>,
    next_child: u64,
    artwork_revision: &'static str,
    fail_one_release: bool,
}

impl FakeHost {
    fn dispatch(&mut self, command: Command) -> Response {
        self.commands.push(command.clone());
        match command {
            Command::GetSettings {} => Response::Result { payload: json!({}) },
            Command::Register { .. } => Response::Ok,
            Command::Poll {} => self.incoming.pop_front().unwrap_or(Response::Pending),
            Command::Request {
                service,
                payload,
                parent,
                ..
            } => {
                let parent = parent.expect("each delegated call is parent linked");
                assert!((10..=12).contains(&parent));
                self.next_child += 1;
                let result = if service == CATALOG {
                    assert_eq!(
                        payload,
                        json!({"action":"resolve_exported_artwork","id":"app-0000000000000001"})
                    );
                    json!({"ok":true,"artwork":{"provider":"steam","app_id":42,"revision":self.artwork_revision}})
                } else {
                    assert!(
                        self.results.is_empty(),
                        "catalog result must be released before provider admission"
                    );
                    assert_eq!(service, "amaterasu.steam/1");
                    assert_eq!(payload["action"], "artwork_read");
                    assert_eq!(payload["app_id"], 42);
                    assert_eq!(payload["revision"], "17");
                    let offset = payload["offset"].as_u64().unwrap();
                    let data = if offset == 0 {
                        format!("{}YWE=", "YWFh".repeat(2730))
                    } else {
                        "YWJj".to_owned()
                    };
                    json!({"ok":true,"app_id":42,"revision":"17","offset":offset,"next_offset":if offset == 0 { Some(8192) } else { None },"total_bytes":8195,"mime_type":"image/png","data":data})
                };
                self.results.insert(self.next_child, result);
                Response::Accepted {
                    id: self.next_child,
                }
            }
            Command::TakeResult { request } => Response::Result {
                payload: self.results[&request].clone(),
            },
            Command::ReleaseResult { request: _ } if self.fail_one_release => {
                self.fail_one_release = false;
                Response::error("transient", "try release again")
            }
            Command::ReleaseResult { request } => {
                assert!(self.results.remove(&request).is_some());
                Response::Ok
            }
            Command::Complete { request, payload } => {
                assert!(
                    self.results.is_empty(),
                    "child result must be released before completion"
                );
                self.completions.insert(request, payload);
                Response::Ok
            }
            _ => panic!("unexpected fake host command"),
        }
    }
    fn enqueue(&mut self, id: u64, revision: Option<&str>, offset: u64) {
        self.incoming.push_back(Response::Request {
            id, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(),
            service: SERVICE.into(), payload: json!({"action":"artwork_read","id":"app-0000000000000001","revision":revision,"offset":offset}), remaining_ms: 30_000,
        });
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> abi::Status {
    // SAFETY: the test retains the host for the lifetime of the module and calls it serially.
    let host = unsafe { &mut *context.cast::<FakeHost>() };
    // SAFETY: the SDK supplies a valid input buffer for this synchronous call.
    let Ok(bytes) = (unsafe { input.read() }) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice(bytes) else {
        return abi::STATUS_INVALID_ARGUMENT;
    };
    let Ok(output) = serde_json::to_vec(&host.dispatch(command)) else {
        return abi::STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the reply sink copies the output before this call returns.
    unsafe { reply.send(&output) };
    abi::STATUS_OK
}

#[test]
fn artwork_reads_resolve_every_chunk_release_results_and_reject_changed_revision() {
    let mut host = FakeHost {
        artwork_revision: "17",
        fail_one_release: true,
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host outlives the library and is accessed only on this test thread.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    for _ in 0..12 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&10]["revision"], "17");
    assert_eq!(host.completions[&10]["next_offset"], 8192);
    host.enqueue(11, Some("17"), 8192);
    for _ in 0..12 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&11]["data"], "YWJj");
    assert!(host.completions[&11]["next_offset"].is_null());
    host.artwork_revision = "18";
    host.enqueue(12, Some("17"), 8192);
    for _ in 0..12 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&12]["ok"], false);
    let requests: Vec<_> = host
        .commands
        .iter()
        .filter_map(|c| {
            if let Command::Request {
                service, parent, ..
            } = c
            {
                Some((service.as_str(), *parent))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        requests,
        vec![
            (CATALOG, Some(10)),
            ("amaterasu.steam/1", Some(10)),
            (CATALOG, Some(11)),
            ("amaterasu.steam/1", Some(11)),
            (CATALOG, Some(12))
        ]
    );
    assert_eq!(
        host.commands
            .iter()
            .filter(|c| matches!(c, Command::ReleaseResult { .. }))
            .count(),
        6
    );
    assert!(host.results.is_empty());
}
