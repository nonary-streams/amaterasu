//! Source-level replacement proof for the public display-target SDK. These
//! stateful providers exercise broker authority, not native output retirement.
use super::Broker;
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::display::{self, targets};
use amaterasu_module_sdk::requests::RequestOptions;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

const CONSUMER: &str = "org.example.target-consumer";
const OTHER_CONSUMER: &str = "org.example.other-consumer";
const UNSELECTED: &str = "org.example.unselected-display";
const OUTSIDER: &str = "org.example.outsider";
const MAX_REFERENCES: usize = 4;

trait DisplayProvider {
    fn principal(&self) -> &'static str;
    fn reply(&mut self, caller: &str, operation: targets::Operation) -> Value;
    fn active_references(&self) -> usize;
    fn binding(&self) -> targets::Binding;
}

fn descriptor(selector: &str, binding: &targets::Binding) -> targets::Descriptor {
    targets::Descriptor {
        selector: selector.into(),
        label: "Existing output".into(),
        ready: true,
        mode: Some(binding.mode.clone()),
        error: None,
    }
}

fn borrowed(selector: &str, reference: &str, binding: &targets::Binding) -> Value {
    json!({"ok":true,"selector":selector,"reference_id":reference,"binding":binding})
}

fn released(selector: &str, reference: &str) -> Value {
    json!({"ok":true,"released":true,"native_removed":false,
        "selector":selector,"reference_id":reference})
}

fn rejected(code: &str) -> Value {
    json!({"ok":false,"error":{"code":code}})
}

// This implementation indexes active references by authenticated owner and
// keeps a bounded FIFO of release receipts. An output is never allocated here.
#[derive(Default)]
struct IndexedDisplay {
    active: BTreeMap<(String, String), String>,
    receipts: VecDeque<(String, String, String)>,
}

impl IndexedDisplay {
    const SELECTOR: &str = "index:opaque-physical-73";
}

impl DisplayProvider for IndexedDisplay {
    fn principal(&self) -> &'static str {
        "vendor.indexed-display"
    }

    fn binding(&self) -> targets::Binding {
        targets::Binding {
            output: "indexed-source:73".into(),
            route_key: "index-capture".into(),
            mode: targets::Mode {
                width: 1920,
                height: 1080,
                refresh_millihz: 60_000,
                hdr_enabled: false,
            },
        }
    }

    fn active_references(&self) -> usize {
        self.active.len()
    }

    fn reply(&mut self, caller: &str, operation: targets::Operation) -> Value {
        match operation {
            targets::Operation::CaptureTargets {} => {
                json!({"ok":true,"targets":[descriptor(Self::SELECTOR, &self.binding())]})
            }
            targets::Operation::ResolveCaptureTarget { selector } => {
                if selector != Self::SELECTOR {
                    return rejected("target_unavailable");
                }
                let binding = self.binding();
                json!({"ok":true,"target":descriptor(&selector, &binding),
                    "capture_output":binding.output,"capture_route_key":binding.route_key})
            }
            targets::Operation::BorrowTarget {
                selector,
                reference_id,
                ..
            } => {
                if selector != Self::SELECTOR {
                    return rejected("target_unavailable");
                }
                let key = (caller.into(), reference_id.clone());
                if !self.active.contains_key(&key) && self.active.len() == MAX_REFERENCES {
                    return rejected("reference_capacity");
                }
                self.receipts
                    .retain(|(owner, reference, _)| owner != caller || reference != &reference_id);
                self.active.insert(key, selector.clone());
                borrowed(&selector, &reference_id, &self.binding())
            }
            targets::Operation::ReleaseTarget {
                selector,
                reference_id,
                ..
            } => {
                let receipt = (caller.into(), reference_id.clone(), selector.clone());
                let key = (caller.into(), reference_id.clone());
                if self.active.get(&key) == Some(&selector) {
                    self.active.remove(&key);
                    if self.receipts.len() == MAX_REFERENCES {
                        self.receipts.pop_front();
                    }
                    self.receipts.push_back(receipt);
                } else if !self.receipts.contains(&receipt) {
                    return rejected("reference_not_owned");
                }
                released(&selector, &reference_id)
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SlotReference {
    owner: String,
    reference: String,
    selector: String,
}

// The replacement uses fixed slots and a circular release-receipt ledger. It
// advertises a different selector, capture route, source and native mode.
#[derive(Default)]
struct SlottedDisplay {
    active: [Option<SlotReference>; MAX_REFERENCES],
    receipts: [Option<SlotReference>; MAX_REFERENCES],
    receipt_cursor: usize,
}

impl SlottedDisplay {
    const SELECTOR: &str = "slot:9b-opaque-existing-output";
}

impl DisplayProvider for SlottedDisplay {
    fn principal(&self) -> &'static str {
        "replacement.slotted-display"
    }

    fn binding(&self) -> targets::Binding {
        targets::Binding {
            output: "slot-capture:9b".into(),
            route_key: "slot-route".into(),
            mode: targets::Mode {
                width: 2560,
                height: 1440,
                refresh_millihz: 119_880,
                hdr_enabled: true,
            },
        }
    }

    fn active_references(&self) -> usize {
        self.active.iter().flatten().count()
    }

    fn reply(&mut self, caller: &str, operation: targets::Operation) -> Value {
        match operation {
            targets::Operation::CaptureTargets {} => {
                json!({"ok":true,"targets":[descriptor(Self::SELECTOR, &self.binding())]})
            }
            targets::Operation::ResolveCaptureTarget { selector } => {
                if selector != Self::SELECTOR {
                    return rejected("target_unavailable");
                }
                let binding = self.binding();
                json!({"ok":true,"target":descriptor(&selector, &binding),
                    "capture_output":binding.output,"capture_route_key":binding.route_key})
            }
            targets::Operation::BorrowTarget {
                selector,
                reference_id,
                ..
            } => {
                if selector != Self::SELECTOR {
                    return rejected("target_unavailable");
                }
                let reference = SlotReference {
                    owner: caller.into(),
                    reference: reference_id.clone(),
                    selector: selector.clone(),
                };
                if !self
                    .active
                    .iter()
                    .flatten()
                    .any(|entry| entry == &reference)
                {
                    let Some(empty) = self.active.iter_mut().find(|entry| entry.is_none()) else {
                        return rejected("reference_capacity");
                    };
                    *empty = Some(reference.clone());
                }
                for receipt in &mut self.receipts {
                    if receipt.as_ref() == Some(&reference) {
                        *receipt = None;
                    }
                }
                borrowed(&selector, &reference_id, &self.binding())
            }
            targets::Operation::ReleaseTarget {
                selector,
                reference_id,
                ..
            } => {
                let reference = SlotReference {
                    owner: caller.into(),
                    reference: reference_id.clone(),
                    selector: selector.clone(),
                };
                if let Some(active) = self
                    .active
                    .iter_mut()
                    .find(|entry| entry.as_ref() == Some(&reference))
                {
                    *active = None;
                    self.receipts[self.receipt_cursor] = Some(reference);
                    self.receipt_cursor = (self.receipt_cursor + 1) % MAX_REFERENCES;
                } else if !self
                    .receipts
                    .iter()
                    .flatten()
                    .any(|entry| entry == &reference)
                {
                    return rejected("reference_not_owned");
                }
                released(&selector, &reference_id)
            }
        }
    }
}

fn providers() -> [Box<dyn DisplayProvider>; 2] {
    [
        Box::<IndexedDisplay>::default(),
        Box::<SlottedDisplay>::default(),
    ]
}

fn selected_broker(provider: &str) -> Broker {
    let mut configuration = RuntimeConfig::default();
    for caller in [CONSUMER, OTHER_CONSUMER] {
        configuration.principals.insert(
            caller.into(),
            Grants {
                call: [display::SERVICE.into()].into(),
                ..Default::default()
            },
        );
    }
    configuration.principals.insert(
        provider.into(),
        Grants {
            provide: [display::SERVICE.into()].into(),
            ..Default::default()
        },
    );
    for principal in [UNSELECTED, OUTSIDER] {
        configuration
            .principals
            .insert(principal.into(), Grants::default());
    }
    Broker::new(configuration).unwrap()
}

fn register(broker: &mut Broker, provider: &dyn DisplayProvider) {
    assert_ok(broker.dispatch(
        provider.principal(),
        Command::Register {
            service: display::SERVICE.into(),
        },
    ));
}

fn command(operation: targets::Operation, operation_id: &str) -> Command {
    operation
        .command(RequestOptions {
            service: display::SERVICE,
            timeout_ms: 1_000,
            operation_id,
            parent: None,
        })
        .unwrap()
}

fn assert_ok(response: Response) {
    assert!(matches!(response, Response::Ok), "{response:?}");
}

fn assert_error(response: Response, expected: &str) {
    assert!(
        matches!(&response, Response::Error { code, .. } if code == expected),
        "expected {expected}, got {response:?}"
    );
}

fn accepted(response: Response) -> u64 {
    let Response::Accepted { id } = response else {
        panic!("request was not admitted: {response:?}");
    };
    id
}

fn deliver(broker: &mut Broker, provider: &str, request: u64, owner: &str) -> targets::Operation {
    let Response::Request {
        id,
        caller,
        initiator,
        service,
        payload,
        ..
    } = broker.dispatch(provider, Command::Poll {})
    else {
        panic!("selected display provider did not receive the request");
    };
    assert_eq!(id, request);
    assert_eq!(caller, owner);
    assert_eq!(initiator, owner);
    assert_eq!(service, display::SERVICE);
    assert_ok(broker.dispatch(provider, Command::CheckRequest { request }));
    serde_json::from_value(payload).unwrap()
}

fn consume(broker: &mut Broker, caller: &str, request: u64) -> Value {
    let Response::Result { payload } = broker.dispatch(caller, Command::TakeResult { request })
    else {
        panic!("target consumer did not receive its result");
    };
    assert_ok(broker.dispatch(caller, Command::ReleaseResult { request }));
    assert_error(
        broker.dispatch(caller, Command::TakeResult { request }),
        "not_found",
    );
    payload
}

fn exchange(
    broker: &mut Broker,
    provider: &mut dyn DisplayProvider,
    caller: &str,
    command: Command,
) -> Value {
    let request = accepted(broker.dispatch(caller, command));
    let operation = deliver(broker, provider.principal(), request, caller);
    let payload = provider.reply(caller, operation);
    assert_ok(broker.dispatch(provider.principal(), Command::Complete { request, payload }));
    consume(broker, caller, request)
}

fn consume_borrowed_binding(
    payload: Value,
    selector: &str,
    reference: &str,
    expected: &targets::Binding,
) -> Result<targets::Binding, String> {
    let binding = targets::borrowed_from_reply(payload, selector, reference)?;
    if &binding != expected {
        return Err("capture_target_binding_changed".into());
    }
    Ok(binding)
}

// This unchanged consumer receives only the public SDK and an opaque request
// transport. Neither provider principal nor a provider-specific selector enters
// its policy. The resolved source, mode and route must survive borrowing intact.
fn unchanged_consumer(mut transport: impl FnMut(Command) -> Value) -> targets::Binding {
    let inventory = targets::inventory_from_reply(transport(command(
        targets::Operation::CaptureTargets {},
        "discover-target",
    )))
    .unwrap();
    let target = inventory.into_iter().find(|target| target.ready).unwrap();
    let binding = targets::resolved_from_reply(
        transport(command(
            targets::Operation::ResolveCaptureTarget {
                selector: target.selector.clone(),
            },
            "resolve-target",
        )),
        &target.selector,
    )
    .unwrap();
    assert_eq!(Some(&binding.mode), target.mode.as_ref());
    for operation_id in ["borrow-target", "retry-borrow-target"] {
        consume_borrowed_binding(
            transport(command(
                targets::Operation::BorrowTarget {
                    operation_id: "source-borrow".into(),
                    selector: target.selector.clone(),
                    reference_id: "owned-read-only-reference".into(),
                },
                operation_id,
            )),
            &target.selector,
            "owned-read-only-reference",
            &binding,
        )
        .unwrap();
    }
    for operation_id in ["release-target", "retry-release-target"] {
        assert!(targets::released_from_reply(
            &transport(command(
                targets::Operation::ReleaseTarget {
                    operation_id: "source-release".into(),
                    selector: target.selector.clone(),
                    reference_id: "owned-read-only-reference".into(),
                },
                operation_id,
            )),
            &target.selector,
            "owned-read-only-reference",
        ));
    }
    binding
}

#[test]
fn display_target_consumer_survives_selected_provider_replacement() {
    let mut bindings = Vec::new();
    for mut provider in providers() {
        let expected = provider.binding();
        let mut broker = selected_broker(provider.principal());
        register(&mut broker, provider.as_ref());
        let binding = unchanged_consumer(|command| {
            exchange(&mut broker, provider.as_mut(), CONSUMER, command)
        });
        assert_eq!(binding, expected);
        assert_eq!(provider.active_references(), 0);
        // Release retired only the caller's reference. Discovery and resolution
        // still expose the same existing physical source after both releases.
        let after_release = targets::inventory_from_reply(exchange(
            &mut broker,
            provider.as_mut(),
            CONSUMER,
            command(
                targets::Operation::CaptureTargets {},
                "discover-after-release",
            ),
        ))
        .unwrap();
        assert_eq!(after_release.len(), 1);
        assert_eq!(after_release[0].mode.as_ref(), Some(&binding.mode));
        let resolved_after_release = targets::resolved_from_reply(
            exchange(
                &mut broker,
                provider.as_mut(),
                CONSUMER,
                command(
                    targets::Operation::ResolveCaptureTarget {
                        selector: after_release[0].selector.clone(),
                    },
                    "resolve-after-release",
                ),
            ),
            &after_release[0].selector,
        )
        .unwrap();
        assert_eq!(resolved_after_release, binding);
        bindings.push(binding);
    }
    assert_ne!(bindings[0], bindings[1]);
}

#[test]
fn display_target_selection_has_no_unselected_provider_or_fallback_authority() {
    for provider in providers() {
        let mut broker = selected_broker(provider.principal());
        let inventory = || command(targets::Operation::CaptureTargets {}, "discover");
        assert_error(broker.dispatch(CONSUMER, inventory()), "unavailable");
        for outsider in [UNSELECTED, OUTSIDER, CONSUMER] {
            assert_error(
                broker.dispatch(
                    outsider,
                    Command::Register {
                        service: display::SERVICE.into(),
                    },
                ),
                "permission_denied",
            );
        }
        register(&mut broker, provider.as_ref());
        assert_error(broker.dispatch(OUTSIDER, inventory()), "permission_denied");
        let request = accepted(broker.dispatch(CONSUMER, inventory()));
        assert!(matches!(
            broker.dispatch(UNSELECTED, Command::Poll {}),
            Response::Pending
        ));
        deliver(&mut broker, provider.principal(), request, CONSUMER);
        assert_error(
            broker.dispatch(
                UNSELECTED,
                Command::Complete {
                    request,
                    payload: json!({"ok":true}),
                },
            ),
            "permission_denied",
        );
        assert_ok(broker.dispatch(CONSUMER, Command::Cancel { request }));
        assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
    }
}

fn reference_operation(selector: &str, reference: &str, release: bool) -> targets::Operation {
    if release {
        targets::Operation::ReleaseTarget {
            operation_id: "reference-release".into(),
            selector: selector.into(),
            reference_id: reference.into(),
        }
    } else {
        targets::Operation::BorrowTarget {
            operation_id: "reference-borrow".into(),
            selector: selector.into(),
            reference_id: reference.into(),
        }
    }
}

#[test]
fn display_target_references_are_exact_caller_owned_bounded_and_reusable() {
    for mut provider in providers() {
        let mut broker = selected_broker(provider.principal());
        register(&mut broker, provider.as_ref());
        let inventory = targets::inventory_from_reply(exchange(
            &mut broker,
            provider.as_mut(),
            CONSUMER,
            command(targets::Operation::CaptureTargets {}, "discover"),
        ))
        .unwrap();
        let selector = &inventory[0].selector;
        for index in 0..MAX_REFERENCES {
            let reference = format!("reference-{index}");
            let reply = exchange(
                &mut broker,
                provider.as_mut(),
                CONSUMER,
                command(
                    reference_operation(selector, &reference, false),
                    &format!("borrow-{index}"),
                ),
            );
            assert!(targets::borrowed_from_reply(reply, selector, &reference).is_ok());
        }
        let overflow = exchange(
            &mut broker,
            provider.as_mut(),
            CONSUMER,
            command(reference_operation(selector, "overflow", false), "overflow"),
        );
        assert!(targets::borrowed_from_reply(overflow, selector, "overflow").is_err());
        assert_eq!(provider.active_references(), MAX_REFERENCES);
        for (caller, target, reference) in [
            (OTHER_CONSUMER, selector.as_str(), "reference-0"),
            (CONSUMER, "another-target", "reference-0"),
            (CONSUMER, selector.as_str(), "another-reference"),
        ] {
            let reply = exchange(
                &mut broker,
                provider.as_mut(),
                caller,
                command(
                    reference_operation(target, reference, true),
                    "wrong-release",
                ),
            );
            assert!(!targets::released_from_reply(&reply, target, reference));
            assert_eq!(provider.active_references(), MAX_REFERENCES);
        }
        for index in 0..MAX_REFERENCES {
            let reference = format!("reference-{index}");
            let reply = exchange(
                &mut broker,
                provider.as_mut(),
                CONSUMER,
                command(
                    reference_operation(selector, &reference, true),
                    &format!("release-{index}"),
                ),
            );
            assert!(targets::released_from_reply(&reply, selector, &reference));
        }
        assert_eq!(provider.active_references(), 0);
        for index in 0..32 {
            let reference = format!("later-reference-{index}");
            let reply = exchange(
                &mut broker,
                provider.as_mut(),
                CONSUMER,
                command(
                    reference_operation(selector, &reference, false),
                    &format!("later-borrow-{index}"),
                ),
            );
            assert!(targets::borrowed_from_reply(reply, selector, &reference).is_ok());
            let reply = exchange(
                &mut broker,
                provider.as_mut(),
                CONSUMER,
                command(
                    reference_operation(selector, &reference, true),
                    &format!("later-release-{index}"),
                ),
            );
            assert!(targets::released_from_reply(&reply, selector, &reference));
        }
        assert_eq!(provider.active_references(), 0);
    }
}

#[test]
fn display_target_requests_preserve_retry_result_and_cancellation_authority() {
    for mut provider in providers() {
        let mut broker = selected_broker(provider.principal());
        register(&mut broker, provider.as_ref());
        let inventory = || command(targets::Operation::CaptureTargets {}, "owned-request");
        let request = accepted(broker.dispatch(CONSUMER, inventory()));
        assert_eq!(accepted(broker.dispatch(CONSUMER, inventory())), request);
        let operation = deliver(&mut broker, provider.principal(), request, CONSUMER);
        for outsider in [OTHER_CONSUMER, UNSELECTED, provider.principal()] {
            for command in [
                Command::TakeResult { request },
                Command::ReleaseResult { request },
                Command::Cancel { request },
            ] {
                assert_error(broker.dispatch(outsider, command), "permission_denied");
            }
        }
        assert_error(
            broker.dispatch(CONSUMER, Command::ReleaseResult { request }),
            "request_pending",
        );
        let payload = provider.reply(CONSUMER, operation);
        assert_ok(broker.dispatch(provider.principal(), Command::Complete { request, payload }));
        assert_error(
            broker.dispatch(OTHER_CONSUMER, Command::ReleaseResult { request }),
            "permission_denied",
        );
        let targets =
            targets::inventory_from_reply(consume(&mut broker, CONSUMER, request)).unwrap();

        let cancelled = accepted(broker.dispatch(
            CONSUMER,
            command(
                reference_operation(&targets[0].selector, "cancelled-before-borrow", false),
                "cancelled-borrow",
            ),
        ));
        assert!(cancelled > request);
        deliver(&mut broker, provider.principal(), cancelled, CONSUMER);
        assert_ok(broker.dispatch(CONSUMER, Command::Cancel { request: cancelled }));
        assert_error(
            broker.dispatch(
                provider.principal(),
                Command::CheckRequest { request: cancelled },
            ),
            "request_ended",
        );
        assert_error(
            broker.dispatch(
                provider.principal(),
                Command::Complete {
                    request: cancelled,
                    payload: json!({"ok":true}),
                },
            ),
            "request_ended",
        );
        assert_error(
            broker.dispatch(CONSUMER, Command::TakeResult { request: cancelled }),
            "cancelled",
        );
        assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request: cancelled }));
        assert_eq!(provider.active_references(), 0);
    }
}

#[test]
fn display_target_consumed_and_cancelled_requests_do_not_exhaust_broker_capacity() {
    for mut provider in providers() {
        let mut broker = selected_broker(provider.principal());
        register(&mut broker, provider.as_ref());
        for sequence in 0..1_100 {
            let command = command(
                targets::Operation::CaptureTargets {},
                &format!("bounded-{sequence}"),
            );
            if sequence % 2 == 0 {
                targets::inventory_from_reply(exchange(
                    &mut broker,
                    provider.as_mut(),
                    CONSUMER,
                    command,
                ))
                .unwrap();
            } else {
                let request = accepted(broker.dispatch(CONSUMER, command));
                assert_ok(broker.dispatch(CONSUMER, Command::Cancel { request }));
                assert_ok(broker.dispatch(CONSUMER, Command::ReleaseResult { request }));
            }
        }
        assert!(matches!(
            broker.dispatch(provider.principal(), Command::Poll {}),
            Response::Pending
        ));
    }
}

// Corrupt readback still crosses the real broker transport before the public
// SDK rejects it. Raw service registration does not confer schema validation.
fn readback(operation: targets::Operation, payload: Value) -> Value {
    let provider = IndexedDisplay::default();
    let mut broker = selected_broker(provider.principal());
    register(&mut broker, &provider);
    let request = accepted(broker.dispatch(CONSUMER, command(operation, "bad-readback")));
    deliver(&mut broker, provider.principal(), request, CONSUMER);
    assert_ok(broker.dispatch(provider.principal(), Command::Complete { request, payload }));
    consume(&mut broker, CONSUMER, request)
}

#[test]
fn display_target_consumer_rejects_incomplete_not_ready_ambiguous_or_malformed_readback() {
    let provider = IndexedDisplay::default();
    let target =
        serde_json::to_value(descriptor(IndexedDisplay::SELECTOR, &provider.binding())).unwrap();
    for payload in [
        json!({"ok":true}),
        json!({"ok":true,"targets":[target.clone(),target.clone()]}),
        json!({"ok":true,"targets":[{"selector":"opaque","label":"Display","ready":true}]}),
        json!({"ok":true,"targets":vec![target.clone();targets::MAX_TARGETS + 1]}),
    ] {
        assert!(
            targets::inventory_from_reply(readback(targets::Operation::CaptureTargets {}, payload))
                .is_err()
        );
    }
    let valid_resolution = json!({"ok":true,"target":target,
        "capture_output":provider.binding().output,"capture_route_key":provider.binding().route_key});
    let mut invalid_resolutions = Vec::new();
    let mut not_ready = valid_resolution.clone();
    not_ready["target"] = json!({"selector":IndexedDisplay::SELECTOR,"label":"Display","ready":false,"error":"not_ready"});
    invalid_resolutions.push(not_ready);
    for (pointer, value) in [
        ("/target/selector", json!("foreign-selector")),
        ("/target/mode/width", json!(0)),
        ("/capture_output", json!("")),
        ("/capture_route_key", json!("malformed route")),
    ] {
        let mut payload = valid_resolution.clone();
        *payload.pointer_mut(pointer).unwrap() = value;
        invalid_resolutions.push(payload);
    }
    let mut missing_mode = valid_resolution;
    missing_mode["target"]
        .as_object_mut()
        .unwrap()
        .remove("mode");
    invalid_resolutions.push(missing_mode);
    for payload in invalid_resolutions {
        assert!(
            targets::resolved_from_reply(
                readback(
                    targets::Operation::ResolveCaptureTarget {
                        selector: IndexedDisplay::SELECTOR.into(),
                    },
                    payload,
                ),
                IndexedDisplay::SELECTOR,
            )
            .is_err()
        );
    }
    for payload in [
        json!({"ok":true,"selector":"foreign","reference_id":"owned","binding":provider.binding()}),
        json!({"ok":true,"selector":IndexedDisplay::SELECTOR,"reference_id":"foreign","binding":provider.binding()}),
        json!({"ok":true,"selector":IndexedDisplay::SELECTOR,"reference_id":"owned","binding":{}}),
    ] {
        assert!(
            targets::borrowed_from_reply(
                readback(
                    reference_operation(IndexedDisplay::SELECTOR, "owned", false),
                    payload
                ),
                IndexedDisplay::SELECTOR,
                "owned",
            )
            .is_err()
        );
    }
    for field in ["output", "route_key", "mode"] {
        let mut changed_binding = serde_json::to_value(provider.binding()).unwrap();
        changed_binding[field] = match field {
            "output" => json!("another-valid-source"),
            "route_key" => json!("another-route"),
            "mode" => serde_json::to_value(SlottedDisplay::default().binding().mode).unwrap(),
            _ => unreachable!(),
        };
        let payload = json!({"ok":true,"selector":IndexedDisplay::SELECTOR,
            "reference_id":"owned","binding":changed_binding});
        assert!(
            consume_borrowed_binding(
                readback(
                    reference_operation(IndexedDisplay::SELECTOR, "owned", false),
                    payload
                ),
                IndexedDisplay::SELECTOR,
                "owned",
                &provider.binding(),
            )
            .is_err()
        );
    }
    for payload in [
        json!({"ok":true,"released":true,"native_removed":true,"selector":IndexedDisplay::SELECTOR,"reference_id":"owned"}),
        json!({"ok":true,"released":true,"selector":IndexedDisplay::SELECTOR,"reference_id":"owned"}),
        released("foreign-selector", "owned"),
        released(IndexedDisplay::SELECTOR, "foreign-reference"),
    ] {
        assert!(!targets::released_from_reply(
            &readback(
                reference_operation(IndexedDisplay::SELECTOR, "owned", true),
                payload
            ),
            IndexedDisplay::SELECTOR,
            "owned"
        ));
    }
}
