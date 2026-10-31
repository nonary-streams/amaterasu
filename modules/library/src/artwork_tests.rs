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
    playnite: bool,
    bad_release_echo: bool,
    deny_detached: bool,
    lose_take_once: bool,
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
                if let Some(parent) = parent {
                    assert!((10..=12).contains(&parent));
                } else {
                    assert_eq!(service, "amaterasu.playnite/1");
                    assert_eq!(payload["action"], "artwork_release");
                }
                self.next_child += 1;
                let result = if service == CATALOG {
                    assert_eq!(
                        payload,
                        json!({"action":"resolve_exported_artwork","id":"app-0000000000000001"})
                    );
                    if self.playnite {
                        json!({"ok":true,"artwork":{"provider":"playnite","game_id":"12345678-1234-1234-1234-123456789abc","revision":"a".repeat(64)}})
                    } else {
                        json!({"ok":true,"artwork":{"provider":"steam","app_id":42,"revision":self.artwork_revision}})
                    }
                } else if self.playnite {
                    assert_eq!(service, "amaterasu.playnite/1");
                    assert_eq!(payload["transfer_id"], "00000000000000000000000000000001");
                    if payload["action"] == "artwork_release" {
                        if parent.is_none() && self.deny_detached {
                            return Response::error("forbidden", "denied");
                        }
                        let transfer_id = if self.bad_release_echo {
                            self.bad_release_echo = false;
                            json!("ffffffffffffffffffffffffffffffff")
                        } else {
                            payload["transfer_id"].clone()
                        };
                        json!({"ok":true,"transfer_id":transfer_id})
                    } else {
                        assert_eq!(payload["game_id"], "12345678-1234-1234-1234-123456789abc");
                        json!({"ok":true,"game_id":"12345678-1234-1234-1234-123456789abc","transfer_id":payload["transfer_id"],"revision":"a".repeat(64),"offset":0,"next_offset":null,"total_bytes":3,"mime_type":"image/png","data":"YWJj"})
                    }
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
                    self.results.is_empty() || payload["ok"] == false,
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
            service: SERVICE.into(), payload: json!({"action":"artwork_read","id":"app-0000000000000001","revision":revision,"offset":offset,"transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000,
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
    if host.lose_take_once && matches!(command, Command::TakeResult { .. }) {
        host.lose_take_once = false;
        return abi::STATUS_INTERNAL_ERROR;
    }
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

#[test]
fn release_waits_for_inflight_read_and_fences_late_reuse() {
    let mut host = FakeHost {
        artwork_revision: "17",
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    host.incoming.push_back(Response::Request { id: 11, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000 });
    for _ in 0..12 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&10]["ok"], true);
    assert_eq!(host.completions[&11]["ok"], true);
    host.enqueue(12, None, 0);
    for _ in 0..3 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&12]["ok"], false);
    let provider_requests = host
        .commands
        .iter()
        .filter(|c| matches!(c, Command::Request { service, .. } if service == "amaterasu.steam/1"))
        .count();
    assert_eq!(provider_requests, 1);
}

#[test]
fn playnite_release_is_forwarded_after_read_and_is_idempotent() {
    let mut host = FakeHost {
        playnite: true,
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    host.incoming.push_back(Response::Request { id: 11, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000 });
    for _ in 0..12 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&10]["ok"], true);
    assert_eq!(host.completions[&11]["ok"], true);
    let calls: Vec<_> = host
        .commands
        .iter()
        .filter_map(|c| match c {
            Command::Request {
                service,
                payload,
                parent,
                ..
            } if service == "amaterasu.playnite/1" => {
                Some((payload["action"].as_str().unwrap().to_owned(), *parent))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            ("artwork_read".into(), Some(10)),
            ("artwork_release".into(), Some(11))
        ]
    );
    host.incoming.push_back(Response::Request { id: 12, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000 });
    library.tick().unwrap();
    library.tick().unwrap();
    assert_eq!(host.completions[&12]["ok"], true);
}

#[test]
fn playnite_release_rejects_wrong_token_and_allows_retry() {
    let mut host = FakeHost {
        playnite: true,
        bad_release_echo: true,
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    for _ in 0..8 {
        library.tick().unwrap();
    }
    for id in [11, 12] {
        host.incoming.push_back(Response::Request { id, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000 });
        for _ in 0..8 {
            library.tick().unwrap();
        }
    }
    assert_eq!(host.completions[&11]["ok"], false);
    assert_eq!(host.completions[&12]["ok"], true);
}

#[test]
fn expired_release_parent_triggers_recorded_detached_cleanup() {
    let mut host = FakeHost {
        playnite: true,
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    for _ in 0..8 {
        library.tick().unwrap();
    }
    let token = "00000000000000000000000000000001";
    let transfer = library.transfers.get_mut(token).unwrap();
    transfer.release = Some((11, Instant::now() - Duration::from_millis(1)));
    for _ in 0..8 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&11]["ok"], false);
    assert!(host.commands.iter().any(|c| matches!(c, Command::Request { service, payload, parent: None, .. } if service == "amaterasu.playnite/1" && payload == &json!({"action":"artwork_release","transfer_id":token}))));
    assert!(!library.cleanup.contains_key(token));
    assert!(library.transfers[token].closed);
}

#[test]
fn denied_detached_cleanup_retains_fence_until_acknowledged() {
    let mut host = FakeHost {
        playnite: true,
        deny_detached: true,
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    for _ in 0..8 {
        library.tick().unwrap();
    }
    let token = "00000000000000000000000000000001";
    library.transfers.get_mut(token).unwrap().release =
        Some((11, Instant::now() - Duration::from_millis(1)));
    for _ in 0..4 {
        library.tick().unwrap();
    }
    assert!(library.cleanup.contains_key(token));
    assert!(library.transfers[token].active && library.transfers[token].closed);
    for _ in 0..65 {
        library.cleanup.get_mut(token).unwrap().retry_after = Instant::now();
        library.tick().unwrap();
    }
    assert!(library.cleanup[token].attempts > 60);
    assert!(
        library.cleanup[token]
            .retry_after
            .duration_since(Instant::now())
            > Duration::from_secs(29)
    );
    host.deny_detached = false;
    library.cleanup.get_mut(token).unwrap().retry_after = Instant::now();
    for _ in 0..4 {
        library.tick().unwrap();
    }
    assert!(!library.cleanup.contains_key(token));
    assert!(!library.transfers[token].active && library.transfers[token].closed);
}
#[test]
fn another_initiator_cannot_release_a_transfer() {
    let mut host = FakeHost {
        artwork_revision: "17",
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    for _ in 0..8 {
        library.tick().unwrap();
    }
    host.incoming.push_back(Response::Request { id: 11, caller: "other".into(), initiator: "other".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000 });
    for _ in 0..3 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&11]["ok"], false);
    assert!(!library.transfers["00000000000000000000000000000001"].closed);
}

#[test]
fn detached_cleanup_retries_lost_result_reply() {
    let mut host = FakeHost {
        playnite: true,
        ..Default::default()
    };
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    host.enqueue(10, None, 0);
    for _ in 0..8 {
        library.tick().unwrap();
    }
    let token = "00000000000000000000000000000001";
    library.transfers.get_mut(token).unwrap().release =
        Some((11, Instant::now() - Duration::from_millis(1)));
    library.tick().unwrap();
    library.tick().unwrap();
    host.lose_take_once = true;
    library.tick().unwrap();
    assert!(library.cleanup.contains_key(token));
    library.cleanup.get_mut(token).unwrap().retry_after = Instant::now();
    for _ in 0..3 {
        library.tick().unwrap();
    }
    assert!(!library.cleanup.contains_key(token));
    assert!(host.results.is_empty());
}
#[test]
fn transfer_fences_allow_thousands_and_enforce_bound() {
    let mut host = FakeHost::default();
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    assert_eq!(MAX_TRANSFERS, 4096);
    for n in 0..MAX_TRANSFERS - 1 {
        library.transfers.insert(
            format!("{n:032x}"),
            Transfer {
                initiator: "amaterasu.nvhttp".into(),
                id: "app-0000000000000001".into(),
                artwork: None,
                active: false,
                release: None,
                closed: true,
                proven: true,
                touched: Instant::now(),
            },
        );
    }
    let send_release = |host: &mut FakeHost, id, transfer_id| {
        host.incoming.push_back(Response::Request { id, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":transfer_id}), remaining_ms: 30_000 })
    };
    send_release(&mut host, 11, "fffffffffffffffffffffffffffffffe");
    for _ in 0..3 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&11]["ok"], false);
    assert_eq!(library.transfers.len(), MAX_TRANSFERS);
    send_release(&mut host, 12, "ffffffffffffffffffffffffffffffff");
    for _ in 0..3 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&12]["ok"], false);
    assert_eq!(library.transfers.len(), MAX_TRANSFERS);
}

#[test]
fn unknown_release_is_unverified_and_fences_late_read() {
    let mut host = FakeHost::default();
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut host as *mut FakeHost).cast(),
        invoke,
    };
    // SAFETY: host remains live and is called synchronously while the library exists.
    let mut library = Library::start(unsafe { RuntimeClient::from_endpoint(endpoint) }).unwrap();
    for id in [10, 11] {
        host.incoming.push_back(Response::Request { id, caller: "amaterasu.nvhttp".into(), initiator: "amaterasu.nvhttp".into(), service: SERVICE.into(), payload: json!({"action":"artwork_release","id":"app-0000000000000001","transfer_id":"00000000000000000000000000000001"}), remaining_ms: 30_000 });
        for _ in 0..3 {
            library.tick().unwrap();
        }
        assert_eq!(host.completions[&id]["ok"], false);
    }
    host.enqueue(12, None, 0);
    for _ in 0..3 {
        library.tick().unwrap();
    }
    assert_eq!(host.completions[&12]["ok"], false);
    assert!(
        !host
            .commands
            .iter()
            .any(|c| matches!(c, Command::Request { .. }))
    );
}
