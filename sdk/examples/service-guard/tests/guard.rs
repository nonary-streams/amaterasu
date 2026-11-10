//! The guard stacked on a provider through the real runtime. The provider and
//! the consumers are written without any knowledge of it.
use amaterasu_module_sdk::requests::{
    CallOutcome, OutboundCall, RequestOptions, unique_operation_id,
};
use amaterasu_module_sdk::{
    DirectCall, DirectReply, ErrorCode, ErrorOrigin, ManagedModule, ModuleResult, RuntimeClient,
};
use amaterasu_module_testkit::{TestHost, grants, settle};
use example_service_guard::ServiceGuard;
use serde_json::{Value, json};

const NOTES: &str = "example.notes/1";
const GUARD: &str = "example.service-guard";

/// Stores one note per caller.
struct Notes {
    runtime: RuntimeClient,
    written: Vec<(String, Value)>,
}

impl Notes {
    fn write(&mut self, caller: &str, payload: Value) -> Value {
        self.written.push((caller.to_owned(), payload));
        json!({"ok": true, "author": caller, "count": self.written.len()})
    }
}

impl ManagedModule for Notes {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.provide(NOTES)?;
        Ok(Self {
            runtime,
            written: Vec::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.runtime.next_request()? {
            let reply = self.write(&request.caller, request.payload.clone());
            request.reply(&self.runtime, &reply)?;
        }
        Ok(())
    }

    fn call_direct(&mut self, call: DirectCall<'_>) -> Option<Value> {
        Some(self.write(call.caller, call.payload))
    }
}

fn host(rule: Value) -> TestHost {
    TestHost::builder()
        .principal("example.notes", grants().provide(NOTES))
        .principal(GUARD, grants().call(NOTES))
        .principal("console", grants().call(NOTES))
        .principal("tray", grants().call(NOTES))
        .settings(GUARD, json!({"services": {NOTES: rule}}))
        .layers(NOTES, &[GUARD])
        .build()
}

fn write_note(text: &str) -> OutboundCall {
    let operation_id = unique_operation_id("test.note");
    OutboundCall::request(
        RequestOptions {
            service: NOTES,
            timeout_ms: 5_000,
            operation_id: &operation_id,
            parent: None,
        },
        &json!({"text": text}),
    )
    .unwrap()
}

#[test]
fn an_admitted_call_passes_through_unchanged_as_its_own_caller() {
    let host = host(json!({"deny": ["tray"]}));
    let mut notes = host.load::<Notes>("example.notes");
    let mut guard = host.load::<ServiceGuard>(GUARD);
    let console = host.client("console");
    let outcome = settle(&console, &mut write_note("hello"), || {
        guard.tick_times(1);
        notes.tick_times(1);
    });
    assert_eq!(
        outcome,
        CallOutcome::Completed(json!({"ok": true, "author": "console", "count": 1}))
    );
}

#[test]
fn an_excluded_caller_is_declined_before_the_provider_sees_it() {
    let host = host(json!({"deny": ["tray"]}));
    let mut notes = host.load::<Notes>("example.notes");
    let mut guard = host.load::<ServiceGuard>(GUARD);
    let tray = host.client("tray");
    let declined = settle(&tray, &mut write_note("hello"), || {
        guard.tick_times(1);
        notes.tick_times(1);
    });
    let CallOutcome::Refused(refusal) = declined else {
        panic!("{declined:?}");
    };
    assert_eq!(refusal.code, ErrorCode::PermissionDenied);
    assert_eq!(refusal.origin, ErrorOrigin::Provider);

    // The provider wrote nothing: the next admitted note is its first.
    let console = host.client("console");
    let outcome = settle(&console, &mut write_note("hello"), || {
        guard.tick_times(1);
        notes.tick_times(1);
    });
    assert_eq!(
        outcome,
        CallOutcome::Completed(json!({"ok": true, "author": "console", "count": 1}))
    );
}

#[test]
fn an_allow_list_admits_only_the_callers_it_names() {
    let host = host(json!({"allow": ["tray"]}));
    let mut notes = host.load::<Notes>("example.notes");
    let mut guard = host.load::<ServiceGuard>(GUARD);
    let mut advance = || {
        guard.tick_times(1);
        notes.tick_times(1);
    };
    let console = host.client("console");
    assert!(matches!(
        settle(&console, &mut write_note("hello"), &mut advance),
        CallOutcome::Refused(_)
    ));
    let tray = host.client("tray");
    assert!(matches!(
        settle(&tray, &mut write_note("hello"), &mut advance),
        CallOutcome::Completed(_)
    ));
}

#[test]
fn direct_calls_obey_the_same_rule() {
    let host = host(json!({"deny": ["tray"]}));
    let _notes = host.load::<Notes>("example.notes");
    let _guard = host.load::<ServiceGuard>(GUARD);

    let console = host.client("console");
    assert_eq!(
        console
            .call_direct(NOTES, json!({"text": "hello"}))
            .unwrap(),
        DirectReply::Completed(json!({"ok": true, "author": "console", "count": 1}))
    );
    let tray = host.client("tray");
    assert!(matches!(
        tray.call_direct(NOTES, json!({"text": "hello"})).unwrap(),
        DirectReply::Unavailable { .. }
    ));
}

#[test]
fn the_guard_does_not_start_for_a_service_policy_did_not_stack_it_on() {
    let host = TestHost::builder()
        .principal("example.notes", grants().provide(NOTES))
        .principal(GUARD, grants().call(NOTES))
        .settings(GUARD, json!({"services": {NOTES: {}}}))
        .build();
    assert!(host.try_load::<ServiceGuard>(GUARD).is_err());
}
