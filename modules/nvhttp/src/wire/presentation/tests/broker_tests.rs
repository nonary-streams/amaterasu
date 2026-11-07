//! Exercise the unchanged wire consumer through real runtime admission and API
//! envelope validation. Test owners supply results without opening native I/O.
use super::*;
use amaterasu_contracts::{ApiMethodApproval, Grants, RuntimeConfig};
use amaterasu_runtime::Broker;

const CONSUMER: &str = "amaterasu.nvhttp";
const LIBRARY: &str = "org.example.library";
const PAIRING: &str = "org.example.pairing";

struct RuntimeProvider {
    broker: Broker,
    principal: String,
    implementation: Provider,
    admitted: Vec<(String, u64)>,
}

impl RuntimeProvider {
    fn new(principal: &str, title: &str) -> Self {
        let mut config = RuntimeConfig::default();
        config.principals.insert(
            CONSUMER.into(),
            Grants {
                call: [
                    session::SERVICE.into(),
                    LIBRARY_SERVICE.into(),
                    super::super::super::PAIRING_SERVICE.into(),
                ]
                .into(),
                ..Default::default()
            },
        );
        for (owner, service) in [
            (principal, session::SERVICE),
            (LIBRARY, LIBRARY_SERVICE),
            (PAIRING, super::super::super::PAIRING_SERVICE),
        ] {
            config.principals.insert(
                owner.into(),
                Grants {
                    provide: [service.into()].into(),
                    ..Default::default()
                },
            );
        }
        for method in session::methods() {
            config.api_methods.push(ApiMethodApproval {
                provider: principal.into(),
                callers: [CONSUMER.into()].into(),
                method,
            });
        }
        let mut broker = Broker::new(config).unwrap();
        for method in session::methods() {
            assert!(matches!(
                broker.dispatch(principal, Command::RegisterApi { method }),
                Response::Ok
            ));
        }
        for (owner, service) in [
            (LIBRARY, LIBRARY_SERVICE),
            (PAIRING, super::super::super::PAIRING_SERVICE),
        ] {
            assert!(matches!(
                broker.dispatch(
                    owner,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        Self {
            broker,
            principal: principal.into(),
            implementation: Provider::new(title),
            admitted: vec![],
        }
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        let invocation = match &command {
            Command::Request {
                payload,
                timeout_ms,
                ..
            } => Some((payload["action"].as_str().unwrap().to_owned(), *timeout_ms)),
            Command::CallApi {
                method, timeout_ms, ..
            } => Some((method.clone(), *timeout_ms)),
            _ => None,
        };
        let response = self.broker.dispatch(CONSUMER, command);
        if let Some((method, timeout_ms)) = invocation {
            assert!(
                matches!(response, Response::Accepted { .. }),
                "{method}: {response:?}"
            );
            self.admitted.push((method, timeout_ms));
        }
        for owner in [LIBRARY, PAIRING, self.principal.as_str()] {
            loop {
                match self.broker.dispatch(owner, Command::Poll {}) {
                    Response::Pending => break,
                    Response::Request {
                        id,
                        service,
                        payload,
                        ..
                    } => {
                        let payload = if service == session::SERVICE {
                            json!(session::decode(&service, payload).unwrap())
                        } else {
                            payload
                        };
                        let method = payload["action"].as_str().unwrap();
                        let result = self.implementation.result(method, &payload);
                        assert!(matches!(
                            self.broker.dispatch(
                                owner,
                                Command::Complete {
                                    request: id,
                                    payload: result
                                }
                            ),
                            Response::Ok
                        ));
                    }
                    other => panic!("unexpected owner delivery: {other:?}"),
                }
            }
        }
        Ok(response)
    }
}

#[test]
fn paired_applist_completes_through_real_broker_deadlines_and_replacement_api_providers() {
    for (principal, title) in [
        ("org.example.presentation", "First runtime tile"),
        ("org.example.replacement", "Replacement runtime tile"),
    ] {
        let mut router = router();
        let mut provider = RuntimeProvider::new(principal, title);
        let now = Instant::now();
        assert!(dispatch(&mut router, "/applist", peer(), now).is_none());
        let mut delivered = None;
        for step in 0..100 {
            let replies = router.poll_with_identity(
                now + Duration::from_millis(step * 10),
                &mut |command| provider.send(command),
                |_, _| true,
            );
            if let Some((_, reply)) = replies.into_iter().next() {
                delivered = Some(reply);
                break;
            }
        }
        let reply = delivered.expect("paired catalog transaction completes");
        assert_eq!(reply.status, 200);
        assert_eq!(reply.permission, Some(Permission::List));
        assert!(String::from_utf8_lossy(&reply.body).contains(title));
        for expected in [
            "identity_inventory",
            "begin_project",
            "append_project",
            "read_project",
            "release_project",
        ] {
            assert!(
                provider
                    .admitted
                    .iter()
                    .any(|(method, _)| method == expected)
            );
        }
        assert!(
            provider
                .admitted
                .iter()
                .all(|(_, timeout)| (1..=30_000).contains(timeout))
        );
        assert!(router.presentation.as_ref().unwrap().jobs.is_empty());
    }
}
