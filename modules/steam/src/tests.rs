use super::*;
use crate::configuration::CONFIGURATION_TIMEOUT_MS;
use crate::settings::{Exclusion, ProductSettings};
use amaterasu_module_sdk::abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_INVALID_ARGUMENT,
    STATUS_OK,
};
use amaterasu_module_sdk::{Command, ManagedModule, Response};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;

#[test]
fn product_settings_actions_require_the_management_principal() {
    assert!(is_management_settings_caller("amaterasu.management"));
    assert!(!is_management_settings_caller("amaterasu.console"));
    assert!(!is_management_settings_caller("amaterasu.nvhttp"));
}

#[test]
fn disabled_steam_rejects_refresh_and_drops_the_prior_discovery_snapshot() {
    let mut provider = Provider::new(Vec::new());
    provider.enabled = true;
    provider.revision = 4;
    provider.scan = Some(Scan::new(&[]));
    provider.games.insert(
        42,
        Game {
            stable_id: "steam:42".into(),
            app_id: 42,
            name: "Example".into(),
            install_directory: "C:/Steam/steamapps/common/Example".into(),
            library_directory: "C:/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
        },
    );

    provider.set_enabled(false);
    assert!(provider.scan.is_none());
    assert!(provider.games.is_empty());
    assert_eq!(provider.revision, 5);
    assert_eq!(provider.status()["enabled"], false);

    let ActionResult::Immediate(refresh) = provider.handle_action(Action::Refresh {}) else {
        panic!("refresh should return an immediate disabled error");
    };
    assert_eq!(refresh["code"], "steam_disabled");
    let ActionResult::Immediate(list) = provider.handle_action(Action::List {
        revision: None,
        after: None,
        limit: None,
    }) else {
        panic!("list should return the empty disabled snapshot");
    };
    assert_eq!(list["enabled"], false);
    assert_eq!(list["games"], json!([]));
}

#[test]
fn enabling_steam_allows_explicit_refresh_of_configured_roots() {
    let mut provider = Provider::new(Vec::new());
    let ActionResult::Immediate(disabled) = provider.handle_action(Action::Refresh {}) else {
        panic!("disabled refresh should fail synchronously");
    };
    assert_eq!(disabled["code"], "steam_disabled");

    provider.set_enabled(true);
    let ActionResult::Immediate(refresh) = provider.handle_action(Action::Refresh {}) else {
        panic!("enabled refresh should be accepted");
    };
    assert_eq!(refresh["enabled"], true);
    assert_eq!(refresh["scanning"], true);

    provider.tick();
    assert_eq!(provider.status()["scanning"], false);
    assert_eq!(provider.status()["revision"], 1);
}

#[test]
fn unsupported_tool_classification_cannot_be_smuggled_into_product_settings() {
    let parsed = serde_json::from_value::<ProductSettings>(json!({
        "enabled": true,
        "include_tools": true
    }));
    assert!(parsed.is_err());
}

#[derive(Default)]
struct FakeHost {
    next_child: u64,
    product_get_results: VecDeque<Response>,
    product_update_results: VecDeque<Response>,
    child_results: BTreeMap<u64, Response>,
    service_requests: VecDeque<Response>,
    completed: Vec<(u64, Value)>,
    released: Vec<u64>,
    configuration_actions: Vec<String>,
    catalog_requests: Vec<Value>,
    catalog_revision: u64,
    catalog_generation: u64,
    catalog_next_sequence: u64,
}

impl FakeHost {
    fn queue_service_request(&mut self, id: u64, caller: &str, payload: Value) {
        self.service_requests.push_back(Response::Request {
            id,
            caller: caller.into(),
            initiator: caller.into(),
            service: SERVICE.into(),
            payload,
            remaining_ms: CONFIGURATION_TIMEOUT_MS,
        });
    }

    fn dispatch(&mut self, command: Command) -> Response {
        match command {
            Command::GetSettings {} => Response::Result {
                payload: json!({"roots":[]}),
            },
            Command::Register { .. } => Response::Ok,
            Command::Request {
                service, payload, ..
            } => {
                self.next_child += 1;
                let request = self.next_child;
                let action = payload["action"].as_str().unwrap_or_default();
                let result = if service == "amaterasu.configuration/1" {
                    self.configuration_actions.push(action.to_owned());
                    match action {
                        "product_get" => self
                            .product_get_results
                            .pop_front()
                            .unwrap_or(Response::Pending),
                        "product_update" => self
                            .product_update_results
                            .pop_front()
                            .unwrap_or(Response::Pending),
                        _ => Response::error("invalid_request", "unexpected configuration action"),
                    }
                } else if service == "amaterasu.catalog/1" {
                    self.catalog_requests.push(payload.clone());
                    let result = match action {
                        "provider_snapshot_begin" => {
                            self.catalog_generation += 1;
                            self.catalog_next_sequence = 0;
                            json!({
                                "ok":true,
                                "revision":self.catalog_revision,
                                "generation":self.catalog_generation,
                                "next_sequence":0
                            })
                        }
                        "provider_snapshot_batch" => {
                            self.catalog_revision += 1;
                            self.catalog_next_sequence += 1;
                            json!({
                                "ok":true,
                                "revision":self.catalog_revision,
                                "generation":self.catalog_generation,
                                "next_sequence":self.catalog_next_sequence
                            })
                        }
                        "provider_snapshot_finish" => json!({
                            "ok":true,
                            "revision":self.catalog_revision,
                            "generation":self.catalog_generation,
                            "complete":true,
                            "cleanup_pending":false
                        }),
                        _ => {
                            json!({"ok":false,"error":{"code":"invalid_request","message":"unexpected action"}})
                        }
                    };
                    Response::Result { payload: result }
                } else {
                    Response::error("service_unavailable", "unexpected target service")
                };
                self.child_results.insert(request, result);
                Response::Accepted { id: request }
            }
            Command::Poll {} => self
                .service_requests
                .pop_front()
                .unwrap_or(Response::Pending),
            Command::TakeResult { request } => self
                .child_results
                .get(&request)
                .cloned()
                .unwrap_or_else(|| Response::error("not_found", "result was released")),
            Command::ReleaseResult { request } => {
                if self.child_results.remove(&request).is_some() {
                    self.released.push(request);
                    Response::Ok
                } else {
                    Response::error("not_found", "result was already released")
                }
            }
            Command::Complete { request, payload } => {
                self.completed.push((request, payload));
                Response::Ok
            }
            _ => Response::error("unexpected", "test host received an unsupported command"),
        }
    }
}

unsafe extern "C" fn invoke_fake_host(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> amaterasu_module_sdk::abi::Status {
    // SAFETY: the test owns the boxed host for the full SteamModule lifetime;
    // the ABI input and reply buffers are borrowed only for this callback.
    let Ok(input) = (unsafe { input.read() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_slice::<Command>(input) else {
        return STATUS_INVALID_ARGUMENT;
    };
    // SAFETY: context points to the live `FakeHost` box installed by the test.
    let response = unsafe { &mut *(context as *mut FakeHost) }.dispatch(command);
    let Ok(output) = serde_json::to_vec(&response) else {
        return STATUS_INTERNAL_ERROR;
    };
    // SAFETY: the output vector remains alive until the reply callback copies it.
    unsafe { reply.send(&output) };
    STATUS_OK
}

fn start_test_module(host: &mut Box<FakeHost>) -> SteamModule {
    let endpoint = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (&mut **host as *mut FakeHost).cast(),
        invoke: invoke_fake_host,
    };
    // SAFETY: `host` remains boxed and alive for each call made by the module.
    let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
    SteamModule::start(runtime).unwrap()
}

fn persisted_settings(enabled: bool, revision: u64) -> Response {
    Response::Result {
        payload: json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":revision,
            "values":{"enabled":enabled}
        }),
    }
}

fn persisted_settings_with_auto_sync(exclusions: Value, revision: u64) -> Response {
    Response::Result {
        payload: json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":revision,
            "values":{"enabled":true,"auto_sync":true,"exclude_games":exclusions}
        }),
    }
}

#[test]
fn terminal_results_are_released_and_lost_release_ack_retries_safely() {
    let mut released = Vec::new();
    let result = configuration::release_terminal_child_result(
        44,
        Response::Result {
            payload: json!({"ok":true}),
        },
        |request| {
            released.push(request);
            Ok(Response::Ok)
        },
    )
    .unwrap();
    assert!(matches!(result, Response::Result { .. }));
    assert_eq!(released, vec![44]);

    assert!(
        configuration::release_terminal_child_result(
            45,
            Response::Result {
                payload: json!({"ok":true}),
            },
            |_| Err("release acknowledgement was lost".into()),
        )
        .is_none()
    );
    let expired = configuration::release_terminal_child_result(
        45,
        Response::error(
            "not_found",
            "result was released after lost acknowledgement",
        ),
        |_| panic!("already released result must not be released again"),
    )
    .unwrap();
    assert!(matches!(expired, Response::Error { ref code, .. } if code == "not_found"));
}

#[test]
fn status_and_list_are_served_while_startup_settings_are_pending() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results.push_back(Response::Pending);
    host.queue_service_request(10, "amaterasu.console", json!({"action":"status"}));
    host.queue_service_request(11, "amaterasu.console", json!({"action":"list"}));
    let mut module = start_test_module(&mut host);

    module.tick().unwrap();
    module.tick().unwrap();

    assert_eq!(host.completed.len(), 2);
    assert_eq!(host.completed[0].0, 10);
    assert_eq!(host.completed[0].1["settings_ready"], false);
    assert_eq!(host.completed[1].0, 11);
    assert_eq!(host.completed[1].1["settings_ready"], false);
    assert_eq!(host.completed[1].1["games"], json!([]));
}

#[test]
fn cancelled_update_releases_results_reads_back_state_and_never_claims_success() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings(true, 3));
    // Simulate the update having committed but its state result acknowledgement
    // being lost before Steam can consume it.
    host.product_update_results
        .push_back(Response::error("deadline_exceeded", "acknowledgement lost"));
    host.product_get_results.push_back(Response::error(
        "unavailable",
        "the first authoritative readback failed",
    ));
    host.product_get_results
        .push_back(persisted_settings(false, 4));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();
    assert_eq!(module.configuration.revision(), 3);
    assert!(module.provider.enabled);

    host.queue_service_request(
        20,
        MANAGEMENT_CALLER,
        json!({"action":"settings_update","enabled":false,"expected_revision":3}),
    );
    module.tick().unwrap(); // accept the management update
    module.tick().unwrap(); // submit the product update

    host.queue_service_request(21, "amaterasu.console", json!({"action":"status"}));
    host.queue_service_request(22, "amaterasu.console", json!({"action":"refresh"}));
    host.queue_service_request(23, "amaterasu.console", json!({"action":"list"}));
    host.queue_service_request(24, "amaterasu.console", json!({"action":"status"}));
    module.tick().unwrap(); // consume the ambiguous update and begin readback
    module.tick().unwrap(); // submit readback and serve status
    module.tick().unwrap(); // fail readback and serve refresh
    assert_eq!(host.configuration_actions.len(), 3);
    module.tick().unwrap(); // backoff tick still serves list
    assert_eq!(host.configuration_actions.len(), 3);
    module.tick().unwrap(); // retry readback while serving status
    module.tick().unwrap(); // apply retried authoritative settings

    let update = host
        .completed
        .iter()
        .find(|(id, _)| *id == 20)
        .expect("management update is completed");
    assert_eq!(update.1["ok"], false);
    assert_eq!(update.1["error"]["code"], "settings_outcome_unknown");
    assert_eq!(update.1["error"]["current_revision"], 3);
    assert_eq!(
        host.completed.iter().find(|(id, _)| *id == 21).unwrap().1["enabled"],
        true
    );
    let refresh = &host.completed.iter().find(|(id, _)| *id == 22).unwrap().1;
    assert_eq!(refresh["enabled"], true);
    assert_eq!(refresh["scanning"], true);
    let list = &host.completed.iter().find(|(id, _)| *id == 23).unwrap().1;
    assert_eq!(list["enabled"], true);
    assert_eq!(list["settings_ready"], true);
    let retried_status = &host.completed.iter().find(|(id, _)| *id == 24).unwrap().1;
    assert_eq!(retried_status["enabled"], true);
    assert_eq!(host.configuration_actions.len(), 4);
    assert_eq!(host.released.len(), 4);
    assert!(!module.provider.enabled);
    assert_eq!(module.configuration.revision(), 4);
}

#[test]
fn automatic_sync_imports_only_included_installed_games_with_a_complete_snapshot() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(
            json!([{"id":"20","name":"Excluded game"}]),
            2,
        ));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module.provider.scan = None;
    module.provider.steam_executable = Some(std::env::current_exe().unwrap());
    module.provider.games.insert(
        10,
        Game {
            stable_id: "steam:10".into(),
            app_id: 10,
            name: "Included game".into(),
            install_directory: "/Steam/steamapps/common/Included".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
        },
    );
    module.provider.games.insert(
        20,
        Game {
            stable_id: "steam:20".into(),
            app_id: 20,
            name: "Excluded game".into(),
            install_directory: "/Steam/steamapps/common/Excluded".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
        },
    );
    module.provider.revision = 1;
    module.provider.snapshot_complete = true;

    for _ in 0..8 {
        module.tick().unwrap();
    }

    assert_eq!(
        host.catalog_requests
            .iter()
            .map(|request| request["action"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "provider_snapshot_begin",
            "provider_snapshot_batch",
            "provider_snapshot_finish"
        ]
    );
    let batch = &host.catalog_requests[1];
    assert_eq!(batch["source_revision"], 1);
    assert_eq!(batch["sequence"], 0);
    assert_eq!(batch["entries"].as_array().unwrap().len(), 1);
    assert_eq!(batch["entries"][0]["source_id"], "steam:10");
    assert_eq!(
        batch["entries"][0]["entry"]["launch"]["arguments"],
        json!(["-applaunch", "10"])
    );
    assert_eq!(host.catalog_requests[2]["complete"], true);
    assert_eq!(host.released.len(), 4);
}

#[test]
fn a_settings_change_stops_before_the_next_catalog_snapshot_step() {
    let mut host = Box::new(FakeHost::default());
    host.product_get_results
        .push_back(persisted_settings_with_auto_sync(json!([]), 2));
    let mut module = start_test_module(&mut host);
    module.tick().unwrap();
    module.tick().unwrap();

    module.provider.scan = None;
    module.provider.steam_executable = Some(std::env::current_exe().unwrap());
    module.provider.games.insert(
        10,
        Game {
            stable_id: "steam:10".into(),
            app_id: 10,
            name: "Included game".into(),
            install_directory: "/Steam/steamapps/common/Included".into(),
            library_directory: "/Steam".into(),
            state_flags: 4,
            installed: true,
            last_updated: 0,
        },
    );
    module.provider.revision = 1;
    module.provider.snapshot_complete = true;

    module.tick().unwrap();
    assert_eq!(host.catalog_requests.len(), 1);
    assert_eq!(
        host.catalog_requests[0]["action"],
        "provider_snapshot_begin"
    );

    let changed_settings = ProductSettings {
        enabled: true,
        auto_sync: true,
        exclude_games: vec![Exclusion {
            id: Some("20".into()),
            name: None,
        }],
    };
    module.provider.revision = 2;
    module
        .catalog_sync
        .tick(&module.runtime, &mut module.provider, &changed_settings)
        .unwrap();

    assert_eq!(host.catalog_requests.len(), 1);
    assert_eq!(module.catalog_sync.status()["syncing"], false);
}
