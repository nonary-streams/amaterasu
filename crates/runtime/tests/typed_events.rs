use amaterasu_abi::{AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status};
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_module_sdk::{
    RuntimeClient,
    events::{EventPoll, EventTopic},
};
use amaterasu_runtime::Broker;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};

struct Changes;
impl EventTopic for Changes {
    const TOPIC: &'static str = "changed/1";
    type Payload = u64;
}

struct Endpoint {
    broker: Arc<Mutex<Broker>>,
    principal: &'static str,
}
unsafe extern "C" fn invoke(
    context: *mut c_void,
    input: BorrowedBytes,
    reply: ReplySink,
) -> Status {
    // SAFETY: each boxed endpoint and the input remain live throughout this call.
    let endpoint = unsafe { &*context.cast::<Endpoint>() };
    let command: Command = serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
    let response = endpoint
        .broker
        .lock()
        .unwrap()
        .dispatch(endpoint.principal, command);
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
    let subscription = subscriber.subscribe_events::<Changes>(2).unwrap();
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
    let (replacement, snapshot) = match subscription.resubscribe_with_snapshot(|| {
        publisher.publish_event::<Changes>(&3)?;
        Ok(2)
    }) {
        Ok(result) => result,
        Err(_) => panic!("resubscribe failed"),
    };
    assert_eq!(snapshot, 2);
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
            Err((old, error)) => {
                assert_eq!(error, "snapshot failed");
                old
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
    let (replacement, _) = match subscription.resubscribe_with_snapshot(|| {
        publisher.publish_event::<Changes>(&1)?;
        publisher.publish_event::<Changes>(&2)?;
        Ok(())
    }) {
        Ok(result) => result,
        Err(_) => panic!("resubscribe failed"),
    };
    assert_eq!(replacement.next().unwrap(), EventPoll::ResyncRequired);
    assert_eq!(replacement.next().unwrap(), EventPoll::ResyncRequired);
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
