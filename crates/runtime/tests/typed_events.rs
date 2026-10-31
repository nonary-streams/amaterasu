use amaterasu_abi::{
    AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_INTERNAL_ERROR, STATUS_OK, Status,
};
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::{
    RuntimeClient,
    events::{EventPoll, EventTopic},
};
use amaterasu_runtime::Broker;
use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Changes;
impl EventTopic for Changes {
    const TOPIC: &'static str = "changed/1";
    type Payload = u64;
}

struct Endpoint {
    broker: Arc<Mutex<Broker>>,
    principal: &'static str,
    fail_unsubscribe: Arc<AtomicUsize>,
    lose_unsubscribe_reply: Arc<AtomicUsize>,
    lose_next_reply: Arc<AtomicUsize>,
}
unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: each boxed endpoint and the input remain live throughout this call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let unsubscribing = matches!(command, Command::Unsubscribe { .. });
    let polling = matches!(command, Command::NextEvent { .. });
    if unsubscribing && endpoint.fail_unsubscribe.swap(0, Ordering::SeqCst) > 0 {
        return STATUS_INTERNAL_ERROR;
    }
    let response = endpoint
        .broker
        .lock()
        .unwrap()
        .dispatch(endpoint.principal, command);
    if (unsubscribing && endpoint.lose_unsubscribe_reply.swap(0, Ordering::SeqCst) > 0)
        || (polling && endpoint.lose_next_reply.swap(0, Ordering::SeqCst) > 0)
    {
        return STATUS_INTERNAL_ERROR;
    }
    let bytes = serde_json::to_vec(&response).unwrap();
    // SAFETY: the host reply callback borrows bytes only during this call.
    unsafe { reply.send(&bytes) };
    STATUS_OK
}
fn client(endpoint: &mut Endpoint) -> RuntimeClient {
    let api = HostApi {
        header: AbiHeader::of::<HostApi>(),
        context: (endpoint as *mut Endpoint).cast(),
        invoke,
    };
    // SAFETY: the boxed endpoints and broker outlive all clients and calls in each test.
    unsafe { RuntimeClient::from_endpoint(api) }
}
fn endpoints() -> (Box<Endpoint>, Box<Endpoint>, Box<Endpoint>) {
    let config = RuntimeConfig {
        principals: [
            (
                "publisher".into(),
                Grants {
                    publish: [Changes::TOPIC.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "subscriber".into(),
                Grants {
                    subscribe: [Changes::TOPIC.into()].into(),
                    ..Default::default()
                },
            ),
            ("outsider".into(), Grants::default()),
        ]
        .into(),
        ..Default::default()
    };
    let broker = Arc::new(Mutex::new(Broker::new(config).unwrap()));
    let endpoint = |principal| {
        Box::new(Endpoint {
            broker: broker.clone(),
            principal,
            fail_unsubscribe: Arc::new(AtomicUsize::new(0)),
            lose_unsubscribe_reply: Arc::new(AtomicUsize::new(0)),
            lose_next_reply: Arc::new(AtomicUsize::new(0)),
        })
    };
    (
        endpoint("publisher"),
        endpoint("subscriber"),
        endpoint("outsider"),
    )
}

#[test]
fn typed_delivery_retains_caller_identity_and_enforces_grants() {
    let (mut publisher, mut subscriber, mut outsider) = endpoints();
    let publisher = client(&mut publisher);
    let subscriber = client(&mut subscriber);
    let outsider = client(&mut outsider);
    assert!(outsider.subscribe_events::<Changes>(1).is_err());
    assert!(outsider.publish_event::<Changes>(&5).is_err());
    let mut subscription = subscriber.subscribe_events::<Changes>(2).unwrap();
    assert_eq!(subscription.next().unwrap(), EventPoll::Pending);
    publisher.publish_event::<Changes>(&42).unwrap();
    assert_eq!(
        subscription.next().unwrap(),
        EventPoll::Event(amaterasu_module_sdk::events::Event {
            publisher: "publisher".into(),
            sequence: 1,
            payload: 42
        })
    );
    subscription.close().unwrap();
}

#[test]
fn overflow_recovery_subscribes_before_snapshot_and_cleanup_frees_capacity() {
    let (mut publisher, mut subscriber, _) = endpoints();
    let publisher = client(&mut publisher);
    let subscriber = client(&mut subscriber);
    let subscription = subscriber.subscribe_events::<Changes>(1).unwrap();
    publisher.publish_event::<Changes>(&1).unwrap();
    publisher.publish_event::<Changes>(&2).unwrap();
    assert_eq!(subscription.next().unwrap(), EventPoll::ResyncRequired);
    let recovered = match subscription.resubscribe_with_snapshot(|| {
        publisher.publish_event::<Changes>(&3)?;
        Ok(2)
    }) {
        Ok(result) => result,
        Err(_) => panic!("resubscribe failed"),
    };
    assert!(recovered.cleanup.is_none());
    let replacement = recovered.subscription;
    assert_eq!(recovered.snapshot, 2);
    assert_eq!(
        replacement.next().unwrap(),
        EventPoll::Event(amaterasu_module_sdk::events::Event {
            publisher: "publisher".into(),
            sequence: 3,
            payload: 3
        })
    );
    drop(replacement);
    // The broker allows only 128 subscriptions, so repeated drop proves cleanup.
    for _ in 0..256 {
        drop(subscriber.subscribe_events::<Changes>(1).unwrap());
    }
    assert!(matches!(
        subscriber
            .send(Command::Unsubscribe { subscription: 1 })
            .unwrap(),
        Response::Error { .. }
    ));
}

#[test]
fn failed_snapshot_keeps_old_subscription_and_retires_replacement() {
    let (_, mut subscriber, _) = endpoints();
    let subscriber = client(&mut subscriber);
    let mut current = subscriber.subscribe_events::<Changes>(1).unwrap();
    for _ in 0..256 {
        current = match current.resubscribe_with_snapshot::<()>(|| Err("snapshot failed".into())) {
            Err(failure) => {
                assert_eq!(failure.error, "snapshot failed");
                assert!(failure.cleanup.is_none());
                failure.subscription
            }
            Ok(_) => panic!("snapshot should fail"),
        };
    }
    assert_eq!(current.next().unwrap(), EventPoll::Pending);
}

#[test]
fn overflow_during_snapshot_requires_another_resynchronization() {
    let (mut publisher, mut subscriber, _) = endpoints();
    let publisher = client(&mut publisher);
    let subscriber = client(&mut subscriber);
    let subscription = subscriber.subscribe_events::<Changes>(1).unwrap();
    let recovered = match subscription.resubscribe_with_snapshot(|| {
        publisher.publish_event::<Changes>(&1)?;
        publisher.publish_event::<Changes>(&2)?;
        Ok(())
    }) {
        Ok(result) => result,
        Err(_) => panic!("resubscribe failed"),
    };
    assert!(recovered.cleanup.is_none());
    assert_eq!(
        recovered.subscription.next().unwrap(),
        EventPoll::ResyncRequired
    );
    assert_eq!(
        recovered.subscription.next().unwrap(),
        EventPoll::ResyncRequired
    );
}

#[test]
fn another_principal_cannot_poll_or_close_a_subscription() {
    let (_, mut subscriber, mut outsider) = endpoints();
    let subscriber = client(&mut subscriber);
    let outsider = client(&mut outsider);
    let subscription = subscriber.subscribe_events::<Changes>(1).unwrap();
    for command in [
        Command::NextEvent { subscription: 1 },
        Command::Unsubscribe { subscription: 1 },
    ] {
        assert!(
            matches!(outsider.send(command).unwrap(), Response::Error { code, .. } if code == "permission_denied")
        );
    }
    assert_eq!(subscription.next().unwrap(), EventPoll::Pending);
}

#[test]
fn close_preserves_handle_until_failed_and_lost_reply_cleanup_is_confirmed() {
    let (_, mut endpoint, _) = endpoints();
    let failures = endpoint.fail_unsubscribe.clone();
    let lost_reply = endpoint.lose_unsubscribe_reply.clone();
    let subscriber = client(&mut endpoint);
    let mut subscription = subscriber.subscribe_events::<Changes>(1).unwrap();
    failures.store(1, Ordering::SeqCst);
    assert!(subscription.close().is_err());
    failures.store(1, Ordering::SeqCst);
    assert!(subscription.close().is_err());
    assert_eq!(subscription.next().unwrap(), EventPoll::Pending);
    lost_reply.store(1, Ordering::SeqCst);
    assert!(subscription.close().is_err());
    subscription.close().unwrap(); // A not-found reply confirms the lost reply retired it.
    subscription.close().unwrap();
    for _ in 0..256 {
        drop(subscriber.subscribe_events::<Changes>(1).unwrap());
    }
}

#[test]
fn recovery_success_returns_failed_old_cleanup_for_retry() {
    let (_, mut endpoint, _) = endpoints();
    let failures = endpoint.fail_unsubscribe.clone();
    let subscriber = client(&mut endpoint);
    for _ in 0..256 {
        let original = subscriber.subscribe_events::<Changes>(1).unwrap();
        failures.store(1, Ordering::SeqCst);
        let recovered = match original.resubscribe_with_snapshot(|| Ok(42)) {
            Ok(recovered) => recovered,
            Err(_) => panic!("snapshot should succeed"),
        };
        assert_eq!(recovered.snapshot, 42);
        assert_eq!(recovered.subscription.next().unwrap(), EventPoll::Pending);
        let mut cleanup = recovered
            .cleanup
            .expect("failed retirement must be returned");
        assert!(cleanup.error.contains("transport"));
        cleanup.subscription.close().unwrap();
    }
}

#[test]
fn failed_snapshot_returns_failed_replacement_cleanup_for_retry() {
    let (_, mut endpoint, _) = endpoints();
    let failures = endpoint.fail_unsubscribe.clone();
    let subscriber = client(&mut endpoint);
    let mut original = subscriber.subscribe_events::<Changes>(1).unwrap();
    for _ in 0..256 {
        failures.store(1, Ordering::SeqCst);
        let failure =
            match original.resubscribe_with_snapshot::<()>(|| Err("snapshot failed".into())) {
                Ok(_) => panic!("snapshot should fail"),
                Err(failure) => failure,
            };
        assert_eq!(failure.error, "snapshot failed");
        original = failure.subscription;
        let mut cleanup = failure.cleanup.expect("failed retirement must be returned");
        cleanup.subscription.close().unwrap();
    }
    assert_eq!(original.next().unwrap(), EventPoll::Pending);
}

#[test]
fn malformed_payload_fences_the_stream_before_a_later_valid_event() {
    let (mut publisher, mut subscriber, _) = endpoints();
    let publisher = client(&mut publisher);
    let subscriber = client(&mut subscriber);
    let subscription = subscriber.subscribe_events::<Changes>(2).unwrap();
    publisher
        .require_success(Command::Publish {
            topic: Changes::TOPIC.into(),
            payload: serde_json::json!("invalid"),
        })
        .unwrap();
    publisher.publish_event::<Changes>(&42).unwrap();
    assert!(subscription.next().is_err());
    assert_eq!(subscription.next().unwrap(), EventPoll::ResyncRequired);
    assert_eq!(subscription.next().unwrap(), EventPoll::ResyncRequired);
}

#[test]
fn lost_poll_reply_fences_the_stream_before_a_later_valid_event() {
    let (mut publisher, mut endpoint, _) = endpoints();
    let lost_reply = endpoint.lose_next_reply.clone();
    let publisher = client(&mut publisher);
    let subscriber = client(&mut endpoint);
    let subscription = subscriber.subscribe_events::<Changes>(2).unwrap();
    publisher.publish_event::<Changes>(&1).unwrap();
    publisher.publish_event::<Changes>(&2).unwrap();
    lost_reply.store(1, Ordering::SeqCst);
    assert!(subscription.next().is_err());
    assert_eq!(subscription.next().unwrap(), EventPoll::ResyncRequired);
}
