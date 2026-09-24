use super::*;
use crate::{
    backend::Backend,
    controller::Controller,
    model::{Event, InputError, InputResult, Key, KeyState, Operation},
};
use amaterasu_module_sdk::{Command, Response};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Instant};

#[derive(Default)]
struct TestBackend {
    events: Rc<RefCell<Vec<Event>>>,
    fail_releases: bool,
}

impl Backend for TestBackend {
    fn name(&self) -> &'static str {
        "settings_test"
    }
    fn supported(&self) -> bool {
        true
    }
    fn absolute_pointer(&self) -> bool {
        true
    }
    fn key_encoding(&self) -> &'static str {
        "windows_set1"
    }
    fn validate(&self, event: &Event) -> InputResult<()> {
        event.validate()
    }
    fn start(&mut self) -> InputResult<()> {
        Ok(())
    }
    fn inject(&mut self, event: &Event) -> InputResult<()> {
        self.events.borrow_mut().push(event.clone());
        if self.fail_releases && event.transition().is_some_and(|(_, down)| !down) {
            Err(InputError::new(
                "release_failed",
                "test native release failure",
            ))
        } else {
            Ok(())
        }
    }
    fn stop(&mut self) {}
}

struct Script {
    responses: VecDeque<Response>,
    commands: Vec<Command>,
}

impl Script {
    fn new(responses: impl IntoIterator<Item = Response>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
            commands: Vec::new(),
        }
    }

    fn send(&mut self, command: Command) -> ModuleResult<Response> {
        self.commands.push(command);
        Ok(self.responses.pop_front().expect("scripted response"))
    }
}

#[test]
fn absent_product_document_uses_enabled_bootstrap_defaults() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":false,
        "version":null,
        "revision":0,
        "values":{},
    }))
    .unwrap();
    assert_eq!(policy, InputPolicy::default());
    assert_eq!(policy.values(), json!({"keyboard":true,"mouse":true}));
    assert_eq!(revision, 0);
}

#[test]
fn settings_read_does_not_report_bootstrap_defaults_as_persisted_before_load() {
    let settings = ProductSettings::new();
    let controller = Controller::new(TestBackend::default(), false);

    assert_eq!(
        settings.get_response(&controller),
        json!({"ok":false,"error":{"code":"settings_unavailable"}}),
    );
}

#[test]
fn product_v1_load_preserves_values_and_defaults_a_missing_legacy_field() {
    let (policy, revision) = read_document(&json!({
        "ok":true,
        "found":true,
        "version":1,
        "revision":9,
        "values":{"keyboard":false},
    }))
    .unwrap();
    assert_eq!(
        policy,
        InputPolicy {
            keyboard: false,
            mouse: true
        }
    );
    assert_eq!(revision, 9);
}

#[test]
fn product_documents_reject_bad_versions_revisions_types_and_unknown_keys() {
    for document in [
        json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"keyboard":true,"mouse":true}}),
        json!({"ok":true,"found":true,"version":1,"revision":0,"values":{"keyboard":true,"mouse":true}}),
        json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"keyboard":"true","mouse":true}}),
        json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"keyboard":true,"mouse":true,"controller":true}}),
        json!({"ok":true,"found":false,"version":null,"revision":1,"values":{}}),
    ] {
        assert!(read_document(&document).is_err(), "accepted {document}");
    }
}

#[test]
fn update_requires_current_revision_and_sends_whole_v1_document() {
    let mut settings = ProductSettings::new();
    settings.ready = true;
    settings.revision = 4;
    settings.pending = None;

    let conflict = settings
        .begin_update(
            71,
            3,
            InputPolicy {
                keyboard: false,
                mouse: true,
            },
            10_000,
        )
        .unwrap_err();
    assert_eq!(conflict["error"]["code"], "revision_conflict");
    assert_eq!(conflict["error"]["current_revision"], 4);
    assert!(!settings.busy());

    settings
        .begin_update(
            72,
            4,
            InputPolicy {
                keyboard: false,
                mouse: false,
            },
            10_000,
        )
        .unwrap();
    let pending = settings.pending.as_ref().unwrap();
    assert!(matches!(
        pending.work,
        Work::Update {
            management_request: 72,
            expected_revision: 4,
            ..
        }
    ));
    let Call::Submit {
        payload, parent, ..
    } = &pending.call
    else {
        panic!("update did not begin with a product request");
    };
    assert_eq!(parent, &Some(72));
    assert_eq!(
        payload,
        &json!({
            "action":"product_update",
            "expected_revision":4,
            "version":1,
            "values":{"keyboard":false,"mouse":false},
        })
    );
}

#[test]
fn management_readback_distinguishes_persisted_and_applied_policy() {
    let cleanup_pending = settings_response_with_state(
        InputPolicy {
            keyboard: false,
            mouse: true,
        },
        12,
        true,
        false,
        true,
    );
    assert_eq!(cleanup_pending["ok"], true);
    assert_eq!(cleanup_pending["persisted"], true);
    assert_eq!(cleanup_pending["applied"], false);
    assert_eq!(cleanup_pending["cleanup_required"], true);
    assert_eq!(cleanup_pending["revision"], 12);
}

#[test]
fn product_errors_forward_only_bounded_codes_and_revision() {
    let conflict = product_failure(&json!({
        "ok":false,
        "error":{"code":"revision_conflict","message":"private details","current_revision":8},
    }))
    .unwrap();
    assert_eq!(
        conflict,
        json!({"ok":false,"error":{"code":"revision_conflict","current_revision":8}})
    );
    let unsafe_response = product_failure(&json!({
        "ok":false,
        "error":{"code":"Bad code","message":"detail"},
    }))
    .unwrap();
    assert_eq!(
        unsafe_response["error"]["code"],
        "configuration_unavailable"
    );
}

#[test]
fn terminal_configuration_result_is_retained_until_release_then_applied_once() {
    let absent = json!({
        "ok":true,"found":false,"version":null,"revision":0,"values":{}
    });
    let updated = json!({
        "ok":true,
        "found":true,
        "version":1,
        "revision":1,
        "values":{"keyboard":false,"mouse":true},
    });
    let mut script = Script::new([
        Response::Accepted { id: 11 },
        Response::Result { payload: absent },
        Response::Ok,
        Response::Accepted { id: 12 },
        Response::Result { payload: updated },
        Response::Error {
            code: "temporarily_unavailable".into(),
            message: "retry".into(),
        },
        Response::Ok,
        Response::Ok,
    ]);
    let mut settings = ProductSettings::new();
    let mut controller = Controller::new(TestBackend::default(), true);

    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(settings.ready());

    settings
        .begin_update(
            72,
            0,
            InputPolicy {
                keyboard: false,
                mouse: true,
            },
            10_000,
        )
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(settings.busy());
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(
        settings.busy(),
        "failed ReleaseResult must retain the payload outcome"
    );

    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    assert!(!settings.busy());
    assert_eq!(settings.revision, 1);
    assert!(!controller.device_policy().keyboard);
    let request_count = script
        .commands
        .iter()
        .filter(|command| matches!(command, Command::Request { .. }))
        .count();
    assert_eq!(
        request_count, 2,
        "release retry must not resubmit the write"
    );
    assert!(matches!(
        script.commands[5],
        Command::ReleaseResult { request: 12 }
    ));
    assert!(matches!(
        script.commands[6],
        Command::ReleaseResult { request: 12 }
    ));
    let Command::Complete {
        request: 72,
        payload,
    } = &script.commands[7]
    else {
        panic!("management request was not completed after result release");
    };
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["persisted"], true);
    assert_eq!(payload["applied"], true);
}

#[test]
fn native_release_failure_reports_saved_policy_as_pending() {
    let now = Instant::now();
    let mut controller = Controller::new(
        TestBackend {
            fail_releases: true,
            ..TestBackend::default()
        },
        true,
    );
    let acquired = controller.handle(
        "owner",
        Operation::Acquire {
            operation_id: None,
            ttl_ms: None,
        },
        now,
        &mut || Ok(()),
    );
    let token = acquired["lease"].as_str().unwrap().to_owned();
    let key_down = Event::Key {
        key: Key::WindowsScan {
            code: 30,
            extended: false,
        },
        state: KeyState::Down,
    };
    assert_eq!(
        controller.handle(
            "owner",
            Operation::Batch {
                lease: token,
                events: vec![key_down]
            },
            now,
            &mut || Ok(()),
        )["ok"],
        true
    );

    let product_result = json!({
        "ok":true,"found":true,"version":1,"revision":1,
        "values":{"keyboard":false,"mouse":true}
    });
    let mut script = Script::new([Response::Ok, Response::Ok]);
    let mut settings = ProductSettings::new();
    settings.ready = true;
    settings.pending = Some(Pending {
        work: Work::Update {
            management_request: 88,
            expected_revision: 0,
            desired: InputPolicy {
                keyboard: false,
                mouse: true,
            },
        },
        call: Call::Releasing {
            request: 18,
            outcome: ChildOutcome::Payload(product_result),
        },
    });
    settings
        .advance(&mut |command| script.send(command), &mut controller)
        .unwrap();
    let Command::Complete {
        request: 88,
        payload,
    } = &script.commands[1]
    else {
        panic!("pending settings update was not completed");
    };
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"]["code"], "cleanup_pending");
    assert_eq!(payload["persisted"], true);
    assert_eq!(payload["applied"], false);
    assert_eq!(payload["cleanup_required"], true);
    assert_eq!(
        settings.get_response(&controller)["values"]["keyboard"],
        false
    );
    assert_eq!(settings.get_response(&controller)["applied"], false);
}
