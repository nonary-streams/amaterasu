//! Stacking through the real runtime: layers that wrap a service while its
//! consumer and provider stay as they are, hooks an owner runs across its
//! participants, and typed events.
use amaterasu_module_sdk::events::{Backlog, Delivery, Subscription, Topic};
use amaterasu_module_sdk::hooks::{HookOutcome, HookRun};
use amaterasu_module_sdk::requests::{
    CallOutcome, OutboundCall, Relay, RequestOptions, unique_operation_id,
};
use amaterasu_module_sdk::{
    DirectCall, DirectReply, ErrorCode, ErrorOrigin, ManagedModule, ModuleResult, RuntimeClient,
};
use amaterasu_module_testkit::{TestHost, TestHostBuilder, grants, settle, settle_hook};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const GREETER: &str = "example.greeter/1";

/// The wrapped provider. It answers only the principals its settings allow,
/// and reports who it answered, so a test can see whose authority applied.
struct Greeter {
    runtime: RuntimeClient,
    allowed: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GreeterSettings {
    allowed: Vec<String>,
}

impl Greeter {
    fn greet(&self, caller: &str, payload: &Value) -> Value {
        if !self.allowed.iter().any(|allowed| allowed == caller) {
            return json!({"ok": false, "code": "forbidden", "caller": caller});
        }
        json!({"ok": true, "greeting": format!("hello {}", payload["name"]), "caller": caller})
    }
}

impl ManagedModule for Greeter {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings: GreeterSettings = runtime.settings()?;
        runtime.provide(GREETER)?;
        Ok(Self {
            runtime,
            allowed: settings.allowed,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.runtime.next_request()? {
            let reply = self.greet(&request.caller, &request.payload);
            request.reply(&self.runtime, &reply)?;
        }
        Ok(())
    }

    fn call_direct(&mut self, call: DirectCall<'_>) -> Option<Value> {
        Some(self.greet(call.caller, &call.payload))
    }
}

/// A layer that capitalizes the name on the way in and signs the reply on the
/// way out. It declines a request for a name its settings block.
struct Capitalizer {
    runtime: RuntimeClient,
    settings: LayerSettings,
    relay: Relay,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LayerSettings {
    signature: String,
    blocked_name: Option<String>,
    serves_direct_calls: bool,
}

impl Capitalizer {
    fn rewritten(payload: &Value) -> Value {
        let name = payload["name"].as_str().unwrap_or_default().to_uppercase();
        json!({"name": name})
    }

    fn signed(signature: &str, mut reply: Value) -> Value {
        let mut signatures = reply["signed_by"].as_array().cloned().unwrap_or_default();
        signatures.push(json!(signature));
        reply["signed_by"] = Value::Array(signatures);
        reply
    }

    fn blocks(&self, payload: &Value) -> bool {
        self.settings.blocked_name.is_some()
            && payload["name"].as_str() == self.settings.blocked_name.as_deref()
    }
}

impl ManagedModule for Capitalizer {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings = runtime.settings()?;
        runtime.wrap(GREETER)?;
        Ok(Self {
            runtime,
            settings,
            relay: Relay::new(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.runtime.next_request()? {
            if self.blocks(&request.payload) {
                request.refuse(&self.runtime, "permission_denied", "this name is blocked")?;
                continue;
            }
            let rewritten = Self::rewritten(&request.payload);
            self.relay.forward_with(request, &rewritten)?;
        }
        let signature = &self.settings.signature;
        self.relay.poll(&self.runtime, |_request, reply| {
            Self::signed(signature, reply)
        })?;
        Ok(())
    }

    fn call_direct(&mut self, call: DirectCall<'_>) -> Option<Value> {
        if !self.settings.serves_direct_calls || self.blocks(&call.payload) {
            return None;
        }
        match self
            .runtime
            .call_direct(call.service, Self::rewritten(&call.payload))
        {
            Ok(DirectReply::Completed(reply)) => {
                Some(Self::signed(&self.settings.signature, reply))
            }
            _ => None,
        }
    }
}

fn greeter_host(layers: &[(&str, Value)]) -> TestHostBuilder {
    let mut builder = TestHost::builder()
        .principal("example.greeter", grants().provide(GREETER))
        .principal("console", grants().call(GREETER))
        .principal("stranger", grants().call(GREETER))
        .settings("example.greeter", json!({"allowed": ["console"]}));
    for (layer, settings) in layers {
        builder = builder
            .principal(layer, grants().call(GREETER))
            .settings(layer, settings.clone());
    }
    if !layers.is_empty() {
        let names: Vec<&str> = layers.iter().map(|(layer, _)| *layer).collect();
        builder = builder.layers(GREETER, &names);
    }
    builder
}

/// The consumer. Nothing here knows whether the service is layered.
fn greet(name: &str) -> OutboundCall {
    let operation_id = unique_operation_id("test.greet");
    OutboundCall::request(
        RequestOptions {
            service: GREETER,
            timeout_ms: 5_000,
            operation_id: &operation_id,
            parent: None,
        },
        &json!({"name": name}),
    )
    .unwrap()
}

#[test]
fn a_layer_wraps_a_service_without_changing_its_consumer_or_provider() {
    // Without the layer.
    let host = greeter_host(&[]).build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let console = host.client("console");
    let plain = settle(&console, &mut greet("ada"), || greeter.tick_times(1));
    assert_eq!(
        plain,
        CallOutcome::Completed(
            json!({"ok": true, "greeting": "hello \"ada\"", "caller": "console"})
        )
    );

    // The same consumer and provider, with a layer composed in between.
    let host = greeter_host(&[("example.capitalizer", json!({"signature": "caps"}))]).build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let mut layer = host.load::<Capitalizer>("example.capitalizer");
    let console = host.client("console");
    let wrapped = settle(&console, &mut greet("ada"), || {
        layer.tick_times(1);
        greeter.tick_times(1);
    });
    assert_eq!(
        wrapped,
        CallOutcome::Completed(json!({
            "ok": true,
            "greeting": "hello \"ADA\"",
            // The provider still authorized the consumer, not the layer.
            "caller": "console",
            "signed_by": ["caps"],
        }))
    );
}

#[test]
fn the_provider_still_turns_away_a_caller_it_does_not_allow() {
    let host = greeter_host(&[("example.capitalizer", json!({"signature": "caps"}))]).build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let mut layer = host.load::<Capitalizer>("example.capitalizer");
    let stranger = host.client("stranger");
    let outcome = settle(&stranger, &mut greet("ada"), || {
        layer.tick_times(1);
        greeter.tick_times(1);
    });
    let CallOutcome::Completed(reply) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(reply["code"], "forbidden");
    assert_eq!(reply["caller"], "stranger");
}

#[test]
fn layers_apply_outermost_first_and_unwind_innermost_first() {
    let host = greeter_host(&[
        ("example.outer", json!({"signature": "outer"})),
        ("example.inner", json!({"signature": "inner"})),
    ])
    .build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let mut outer = host.load::<Capitalizer>("example.outer");
    let mut inner = host.load::<Capitalizer>("example.inner");
    let console = host.client("console");
    let outcome = settle(&console, &mut greet("ada"), || {
        outer.tick_times(1);
        inner.tick_times(1);
        greeter.tick_times(1);
    });
    let CallOutcome::Completed(reply) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(reply["caller"], "console");
    assert_eq!(reply["signed_by"], json!(["inner", "outer"]));
}

#[test]
fn a_layers_refusal_tells_the_consumer_nothing_happened() {
    let host = greeter_host(&[(
        "example.capitalizer",
        json!({"signature": "caps", "blocked_name": "mallory"}),
    )])
    .build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let mut layer = host.load::<Capitalizer>("example.capitalizer");
    let console = host.client("console");
    let outcome = settle(&console, &mut greet("mallory"), || {
        layer.tick_times(1);
        greeter.tick_times(1);
    });
    let CallOutcome::Refused(refusal) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(refusal.code, ErrorCode::PermissionDenied);
    assert_eq!(refusal.origin, ErrorOrigin::Provider);
    assert_eq!(refusal.message, "this name is blocked");
}

#[test]
fn a_service_is_unavailable_until_its_layer_has_started() {
    let host = greeter_host(&[("example.capitalizer", json!({"signature": "caps"}))]).build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let console = host.client("console");
    // The provider alone must not be reachable around its layer.
    let early = settle(&console, &mut greet("ada"), || greeter.tick_times(1));
    assert!(matches!(
        early,
        CallOutcome::Refused(refusal)
            if refusal.code == ErrorCode::Unavailable && refusal.origin == ErrorOrigin::Broker
    ));

    let mut layer = host.load::<Capitalizer>("example.capitalizer");
    let ready = settle(&console, &mut greet("ada"), || {
        layer.tick_times(1);
        greeter.tick_times(1);
    });
    assert!(matches!(ready, CallOutcome::Completed(_)));
}

#[test]
fn a_module_that_is_not_listed_cannot_wrap_a_service() {
    let host = greeter_host(&[])
        .principal("example.capitalizer", grants().call(GREETER))
        .build();
    assert!(host.try_load::<Capitalizer>("example.capitalizer").is_err());
}

#[test]
fn a_direct_call_passes_through_a_layer_as_the_consumer() {
    let host = greeter_host(&[(
        "example.capitalizer",
        json!({"signature": "caps", "serves_direct_calls": true}),
    )])
    .build();
    let _greeter = host.load::<Greeter>("example.greeter");
    let _layer = host.load::<Capitalizer>("example.capitalizer");
    let console = host.client("console");
    let reply = console
        .call_direct(GREETER, json!({"name": "ada"}))
        .unwrap();
    assert_eq!(
        reply,
        DirectReply::Completed(json!({
            "ok": true,
            "greeting": "hello \"ADA\"",
            "caller": "console",
            "signed_by": ["caps"],
        }))
    );
}

#[test]
fn a_layer_that_declines_direct_calls_is_not_bypassed() {
    let host = greeter_host(&[("example.capitalizer", json!({"signature": "caps"}))]).build();
    let mut greeter = host.load::<Greeter>("example.greeter");
    let mut layer = host.load::<Capitalizer>("example.capitalizer");
    let console = host.client("console");
    // The layer declined, so the provider was not entered around it.
    assert!(matches!(
        console
            .call_direct(GREETER, json!({"name": "ada"}))
            .unwrap(),
        DirectReply::Unavailable { .. }
    ));
    // The consumer's ordinary fallback goes through the layer.
    let queued = settle(&console, &mut greet("ada"), || {
        layer.tick_times(1);
        greeter.tick_times(1);
    });
    let CallOutcome::Completed(reply) = queued else {
        panic!("{queued:?}");
    };
    assert_eq!(reply["signed_by"], json!(["caps"]));
}

const ENVIRONMENT: &str = "example.launch.environment/1";

/// A hook participant. It appends its entry to the path it receives, or
/// declines when its settings say so.
struct PathContributor {
    runtime: RuntimeClient,
    settings: ContributorSettings,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ContributorSettings {
    entry: String,
    declines: bool,
}

impl ManagedModule for PathContributor {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings = runtime.settings()?;
        runtime.join_hook(ENVIRONMENT)?;
        Ok(Self { runtime, settings })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.runtime.next_request()? {
            assert_eq!(request.hook(), Some(ENVIRONMENT));
            if self.settings.declines {
                request.refuse(&self.runtime, "stream_active", "a stream is running")?;
                continue;
            }
            let mut path = request.payload["path"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            path.push(json!(self.settings.entry));
            request.reply(&self.runtime, &json!({"path": path}))?;
        }
        Ok(())
    }
}

fn hook_host(first: Value, second: Value) -> TestHost {
    TestHost::builder()
        .principal("example.launcher", grants())
        .principal("example.first", grants().hook(ENVIRONMENT))
        .principal("example.second", grants().hook(ENVIRONMENT))
        .settings("example.first", first)
        .settings("example.second", second)
        .extension_point(
            ENVIRONMENT,
            "example.launcher",
            &["example.first", "example.second"],
        )
        .build()
}

fn environment_run() -> HookRun {
    HookRun::new(
        ENVIRONMENT,
        &unique_operation_id("test.launch"),
        &json!({"path": ["base"]}),
    )
    .unwrap()
}

#[test]
fn a_chained_hook_lets_each_participant_build_on_the_one_before() {
    let host = hook_host(json!({"entry": "one"}), json!({"entry": "two"}));
    let mut first = host.load::<PathContributor>("example.first");
    let mut second = host.load::<PathContributor>("example.second");
    let launcher = host.client("example.launcher");
    let outcome = settle_hook(&launcher, &mut environment_run().chained(), || {
        first.tick_times(1);
        second.tick_times(1);
    });
    let HookOutcome::Completed(report) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(report.value(), &json!({"path": ["base", "one", "two"]}));
}

#[test]
fn a_broadcast_hook_gives_every_participant_the_owners_input() {
    let host = hook_host(json!({"entry": "one"}), json!({"entry": "two"}));
    let mut first = host.load::<PathContributor>("example.first");
    let mut second = host.load::<PathContributor>("example.second");
    let launcher = host.client("example.launcher");
    let outcome = settle_hook(&launcher, &mut environment_run(), || {
        first.tick_times(1);
        second.tick_times(1);
    });
    let HookOutcome::Completed(report) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        report.answers().collect::<Vec<_>>(),
        [
            ("example.first", &json!({"path": ["base", "one"]})),
            ("example.second", &json!({"path": ["base", "two"]})),
        ]
    );
}

#[test]
fn a_refusing_participant_stops_a_required_run_and_is_named() {
    let host = hook_host(json!({"declines": true}), json!({"entry": "two"}));
    let mut first = host.load::<PathContributor>("example.first");
    let mut second = host.load::<PathContributor>("example.second");
    let launcher = host.client("example.launcher");
    let outcome = settle_hook(&launcher, &mut environment_run(), || {
        first.tick_times(1);
        second.tick_times(1);
    });
    let HookOutcome::Stopped(stop) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(stop.is_refusal());
    assert_eq!(stop.code, ErrorCode::Other("stream_active".into()));
    assert_eq!(stop.message, "example.first: a stream is running");
}

#[test]
fn a_best_effort_chain_carries_on_past_participants_that_give_no_answer() {
    // The first participant declines and the second never starts.
    let host = hook_host(json!({"declines": true}), json!({"entry": "two"}));
    let mut first = host.load::<PathContributor>("example.first");
    let launcher = host.client("example.launcher");
    let outcome = settle_hook(
        &launcher,
        &mut environment_run().best_effort().chained(),
        || first.tick_times(1),
    );
    let HookOutcome::Completed(report) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(report.value(), &json!({"path": ["base"]}));
    let failures: Vec<_> = report
        .failures()
        .map(|(participant, failure)| (participant, failure.code.clone(), failure.origin))
        .collect();
    assert_eq!(
        failures,
        [
            (
                "example.first",
                ErrorCode::Other("stream_active".into()),
                ErrorOrigin::Provider
            ),
            (
                "example.second",
                ErrorCode::Unavailable,
                ErrorOrigin::Broker
            ),
        ]
    );
}

#[test]
fn a_required_run_does_not_start_while_a_participant_is_absent() {
    let host = hook_host(json!({"entry": "one"}), json!({"entry": "two"}));
    let mut first = host.load::<PathContributor>("example.first");
    let launcher = host.client("example.launcher");
    let outcome = settle_hook(&launcher, &mut environment_run(), || first.tick_times(1));
    assert!(matches!(
        outcome,
        HookOutcome::Refused(refusal) if refusal.code == ErrorCode::Unavailable
    ));
}

#[test]
fn an_extension_point_can_run_far_more_often_than_the_fenced_identifiers_allow() {
    let host = hook_host(json!({"entry": "one"}), json!({"entry": "two"}));
    let mut first = host.load::<PathContributor>("example.first");
    let mut second = host.load::<PathContributor>("example.second");
    let launcher = host.client("example.launcher");
    for _ in 0..600 {
        let outcome = settle_hook(&launcher, &mut environment_run().chained(), || {
            first.tick_times(1);
            second.tick_times(1);
        });
        assert!(matches!(outcome, HookOutcome::Completed(_)), "{outcome:?}");
    }
}

#[test]
fn only_the_hooks_owner_can_run_it() {
    let host = hook_host(json!({"entry": "one"}), json!({"entry": "two"}));
    let _first = host.load::<PathContributor>("example.first");
    let _second = host.load::<PathContributor>("example.second");
    let participant = host.client("example.first");
    let outcome = settle_hook(&participant, &mut environment_run(), || {});
    assert!(matches!(
        outcome,
        HookOutcome::Refused(refusal) if refusal.code == ErrorCode::PermissionDenied
    ));
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct LibraryChanged {
    revision: u64,
}

const LIBRARY_CHANGED: Topic<LibraryChanged> = Topic::new("example.library.changed/1");

fn event_host() -> TestHost {
    TestHost::builder()
        .principal("example.library", grants().publish(LIBRARY_CHANGED.name()))
        .principal("example.shelf", grants().subscribe(LIBRARY_CHANGED.name()))
        .principal("stranger", grants())
        .build()
}

#[test]
fn a_typed_event_reaches_a_subscriber_with_its_publisher_and_sequence() {
    let host = event_host();
    let library = host.client("example.library");
    let shelf = host.client("example.shelf");
    let mut changes = Subscription::open(&shelf, &LIBRARY_CHANGED, Backlog::Complete(4)).unwrap();
    assert_eq!(changes.next(&shelf).unwrap(), None);

    library
        .publish(&LIBRARY_CHANGED, &LibraryChanged { revision: 3 })
        .unwrap();
    library
        .publish(&LIBRARY_CHANGED, &LibraryChanged { revision: 4 })
        .unwrap();
    let mut seen = Vec::new();
    while let Some(Delivery::Event(event)) = changes.next(&shelf).unwrap() {
        assert_eq!(event.publisher, "example.library");
        seen.push((event.sequence, event.payload.revision));
    }
    assert_eq!(seen, [(1, 3), (2, 4)]);
    changes.close(&shelf).unwrap();
}

#[test]
fn a_subscriber_that_falls_behind_is_told_of_the_gap_and_keeps_following() {
    let host = event_host();
    let library = host.client("example.library");
    let shelf = host.client("example.shelf");
    let mut changes = Subscription::open(&shelf, &LIBRARY_CHANGED, Backlog::Complete(2)).unwrap();
    for revision in 1..=3 {
        library
            .publish(&LIBRARY_CHANGED, &LibraryChanged { revision })
            .unwrap();
    }
    assert_eq!(changes.next(&shelf).unwrap(), Some(Delivery::Gap));
    assert_eq!(changes.next(&shelf).unwrap(), None);

    library
        .publish(&LIBRARY_CHANGED, &LibraryChanged { revision: 4 })
        .unwrap();
    assert!(matches!(
        changes.next(&shelf).unwrap(),
        Some(Delivery::Event(event)) if event.payload.revision == 4
    ));
}

#[test]
fn a_latest_only_subscriber_sees_the_newest_event_without_a_gap() {
    let host = event_host();
    let library = host.client("example.library");
    let shelf = host.client("example.shelf");
    let mut changes = Subscription::open(&shelf, &LIBRARY_CHANGED, Backlog::Latest(1)).unwrap();
    for revision in 1..=3 {
        library
            .publish(&LIBRARY_CHANGED, &LibraryChanged { revision })
            .unwrap();
    }
    assert!(matches!(
        changes.next(&shelf).unwrap(),
        Some(Delivery::Event(event)) if event.payload.revision == 3 && event.sequence == 3
    ));
    assert_eq!(changes.next(&shelf).unwrap(), None);
}

#[test]
fn publishing_and_subscribing_each_need_their_own_grant() {
    let host = event_host();
    let stranger = host.client("stranger");
    let shelf = host.client("example.shelf");
    assert!(
        stranger
            .publish(&LIBRARY_CHANGED, &LibraryChanged { revision: 1 })
            .is_err_and(|error| error.is(&ErrorCode::PermissionDenied))
    );
    assert!(
        shelf
            .publish(&LIBRARY_CHANGED, &LibraryChanged { revision: 1 })
            .is_err_and(|error| error.is(&ErrorCode::PermissionDenied))
    );
    assert!(
        Subscription::open(&stranger, &LIBRARY_CHANGED, Backlog::Latest(1))
            .is_err_and(|error| error.is(&ErrorCode::PermissionDenied))
    );
}
