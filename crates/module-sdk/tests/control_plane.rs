//! The typed control-plane helpers against the real runtime, through the
//! module test kit: a provider written the way a plugin author writes one, and
//! peers that call it.
use amaterasu_module_sdk::requests::{
    CallOutcome, OutboundCall, ReplyDelivery, RequestOptions, unique_operation_id,
};
use amaterasu_module_sdk::{
    ControlError, DirectCall, DirectReply, ErrorCode, ManagedModule, ModuleResult, RuntimeClient,
};
use amaterasu_module_testkit::{TestHost, grants, settle};
use serde::Deserialize;
use serde_json::{Value, json};
use std::task::Poll;

const COUNTER: &str = "example.counter/1";

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    step: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { step: 1 }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Add {},
    Read {},
    /// Accepted now and answered on a later tick.
    AddLater {},
}

/// A provider with one mutating, one read-only and one deferred operation.
struct Counter {
    runtime: RuntimeClient,
    step: u64,
    total: u64,
    deferred: Option<u64>,
}

impl ManagedModule for Counter {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings: Settings = runtime.settings()?;
        runtime.provide(COUNTER)?;
        Ok(Self {
            runtime,
            step: settings.step,
            total: 0,
            deferred: None,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.deferred.take() {
            self.total += self.step;
            self.runtime.reply(request, &json!({"total": self.total}))?;
        }
        let Some(request) = self.runtime.next_request()? else {
            return Ok(());
        };
        let request = match request.parse::<Operation>() {
            Ok(request) => request,
            Err(malformed) => {
                malformed
                    .request
                    .reply(&self.runtime, &json!({"error":"invalid_request"}))?;
                return Ok(());
            }
        };
        match request.payload {
            Operation::Add {} => {
                self.total += self.step;
                request.reply(&self.runtime, &json!({"total": self.total}))?;
            }
            Operation::Read {} => {
                request.reply(&self.runtime, &json!({"total": self.total}))?;
            }
            Operation::AddLater {} => self.deferred = Some(request.id),
        }
        Ok(())
    }

    fn call_direct(&mut self, call: DirectCall<'_>) -> Option<Value> {
        (call.payload == json!({"action":"read"})).then(|| json!({"total": self.total}))
    }
}

fn host() -> TestHost {
    TestHost::builder()
        .principal("example.counter", grants().provide(COUNTER))
        .principal("console", grants().call(COUNTER))
        .principal("stranger", grants())
        .settings("example.counter", json!({"step": 5}))
        .build()
}

fn call(action: &str) -> OutboundCall {
    let operation_id = unique_operation_id("test.counter");
    OutboundCall::request(
        RequestOptions {
            service: COUNTER,
            timeout_ms: 5_000,
            operation_id: &operation_id,
            parent: None,
        },
        &json!({"action": action}),
    )
    .unwrap()
}

#[test]
fn a_provider_serves_typed_operations_with_its_configured_settings() {
    let host = host();
    let mut counter = host.load::<Counter>("example.counter");
    let console = host.client("console");

    let added = settle(&console, &mut call("add"), || counter.tick_times(1));
    assert_eq!(added, CallOutcome::Completed(json!({"total": 5})));
    let read = settle(&console, &mut call("read").read_only(), || {
        counter.tick_times(1)
    });
    assert_eq!(read, CallOutcome::Completed(json!({"total": 5})));
    let invalid = settle(&console, &mut call("subtract"), || counter.tick_times(1));
    assert_eq!(
        invalid,
        CallOutcome::Completed(json!({"error":"invalid_request"}))
    );
}

#[test]
fn a_caller_without_the_grant_is_refused_before_any_provider_runs() {
    let host = host();
    let mut counter = host.load::<Counter>("example.counter");
    let stranger = host.client("stranger");
    let outcome = settle(&stranger, &mut call("add"), || counter.tick_times(1));
    assert!(matches!(
        outcome,
        CallOutcome::Refused(refusal) if refusal.code == ErrorCode::PermissionDenied
    ));

    let console = host.client("console");
    let read = settle(&console, &mut call("read").read_only(), || {
        counter.tick_times(1)
    });
    assert_eq!(read, CallOutcome::Completed(json!({"total": 0})));
}

#[test]
fn a_call_to_an_unregistered_service_is_refused() {
    let host = host();
    let console = host.client("console");
    let Poll::Ready(CallOutcome::Refused(refusal)) = call("add").poll(&console) else {
        panic!("no provider is registered");
    };
    assert_eq!(refusal.code, ErrorCode::Unavailable);
}

#[test]
fn a_reply_after_cancellation_does_not_fail_the_provider_tick() {
    let host = host();
    let mut counter = host.load::<Counter>("example.counter");
    let console = host.client("console");

    let mut later = call("add_later");
    assert!(later.poll(&console).is_pending());
    counter.tick_times(1);
    later.cancel();
    // The provider replies on its next tick, after the caller gave up. The
    // tick still succeeds and the committed work stands.
    let outcome = settle(&console, &mut later, || counter.tick_times(1));
    assert!(matches!(
        outcome,
        CallOutcome::Unconfirmed(error) if error.is(&ErrorCode::Cancelled)
    ));
    counter.tick_times(2);
    let read = settle(&console, &mut call("read").read_only(), || {
        counter.tick_times(1)
    });
    assert_eq!(read, CallOutcome::Completed(json!({"total": 5})));
}

#[test]
fn an_identical_submission_finds_the_request_already_admitted() {
    let host = host();
    let mut counter = host.load::<Counter>("example.counter");
    let console = host.client("console");
    let submit = || {
        OutboundCall::request(
            RequestOptions {
                service: COUNTER,
                timeout_ms: 5_000,
                operation_id: "test.counter.fixed",
                parent: None,
            },
            &json!({"action":"add"}),
        )
        .unwrap()
    };
    let mut first = submit();
    let mut second = submit();
    assert!(first.poll(&console).is_pending());
    assert!(second.poll(&console).is_pending());
    assert_eq!(first.request_id(), second.request_id());
    counter.tick_times(3);
    // One provider effect served both; the first to collect releases the record.
    assert_eq!(
        first.poll(&console),
        Poll::Ready(CallOutcome::Completed(json!({"total": 5})))
    );
    assert!(matches!(
        second.poll(&console),
        Poll::Ready(CallOutcome::Unconfirmed(error)) if error.is(&ErrorCode::NotFound)
    ));
}

#[test]
fn a_request_that_outlives_its_deadline_is_unconfirmed() {
    let host = host();
    let _counter = host.load::<Counter>("example.counter");
    let console = host.client("console");
    let mut expiring = OutboundCall::request(
        RequestOptions {
            service: COUNTER,
            timeout_ms: 1,
            operation_id: "test.counter.expiring",
            parent: None,
        },
        &json!({"action":"add"}),
    )
    .unwrap();
    // The provider is never ticked, so the broker ends the request itself.
    let outcome = settle(&console, &mut expiring, || {});
    assert!(matches!(
        outcome,
        CallOutcome::Unconfirmed(error) if error.is(&ErrorCode::DeadlineExceeded)
    ));
}

#[test]
fn a_loaded_module_serves_direct_calls_and_declines_what_needs_a_tick() {
    let host = host();
    let mut counter = host.load::<Counter>("example.counter");
    let console = host.client("console");
    settle(&console, &mut call("add"), || counter.tick_times(1));

    assert_eq!(
        console
            .call_direct(COUNTER, json!({"action":"read"}))
            .unwrap(),
        DirectReply::Completed(json!({"total": 5}))
    );
    assert!(matches!(
        console
            .call_direct(COUNTER, json!({"action":"add"}))
            .unwrap(),
        DirectReply::Unavailable { .. }
    ));
}

#[test]
fn a_peer_can_play_the_provider_by_hand() {
    let host = TestHost::builder()
        .principal("fake.provider", grants().provide(COUNTER))
        .principal("console", grants().call(COUNTER))
        .build();
    let provider = host.client("fake.provider");
    provider.provide(COUNTER).unwrap();
    let console = host.client("console");

    let mut pending = call("add");
    assert!(pending.poll(&console).is_pending());
    let request = provider.next_request().unwrap().unwrap();
    assert_eq!(
        (request.caller.as_str(), request.initiator.as_str()),
        ("console", "console")
    );
    assert_eq!(request.payload, json!({"action":"add"}));
    assert!(request.is_active(&provider).unwrap());
    assert_eq!(
        request.reply(&provider, &json!({"total": 1})).unwrap(),
        ReplyDelivery::Recorded
    );
    // The request has its terminal result now; a second reply is discarded.
    assert_eq!(
        request.reply(&provider, &json!({"total": 2})).unwrap(),
        ReplyDelivery::RequestEnded
    );
    assert!(!request.is_active(&provider).unwrap());
    assert_eq!(
        pending.poll(&console),
        Poll::Ready(CallOutcome::Completed(json!({"total": 1})))
    );
    assert!(provider.next_request().unwrap().is_none());
}

#[test]
fn settings_that_do_not_decode_fail_startup_with_the_reason() {
    let host = TestHost::builder()
        .principal("example.counter", grants().provide(COUNTER))
        .settings("example.counter", json!({"step": "five"}))
        .build();
    let error = host
        .client("example.counter")
        .settings::<Settings>()
        .err()
        .unwrap();
    assert!(matches!(error, ControlError::Payload(_)));
    assert!(host.try_load::<Counter>("example.counter").is_err());

    // A principal with no configured settings reads an empty object.
    let host = TestHost::builder()
        .principal("example.counter", grants().provide(COUNTER))
        .build();
    assert_eq!(
        host.client("example.counter")
            .settings::<Settings>()
            .unwrap()
            .step,
        1
    );
}

#[test]
fn only_the_granted_principal_may_provide_a_service() {
    let host = host();
    assert!(
        host.client("console")
            .provide(COUNTER)
            .is_err_and(|error| error.is(&ErrorCode::PermissionDenied))
    );
    let counter = host.client("example.counter");
    counter.provide(COUNTER).unwrap();
    counter.provide(COUNTER).unwrap();
}
