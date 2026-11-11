//! An ended ticket must not decide whether, or how soon, a later one starts.
use super::concurrent_ports_tests::{admit, fixture, request, settle};
use super::*;
use std::net::Ipv4Addr;

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

#[test]
fn an_unanswered_stop_does_not_delay_the_next_ticket() {
    let _guard = super::tests::NETWORK_TEST_LOCK.lock().unwrap();
    let (mut controller, mut endpoint, runtime) = fixture(LOCAL);
    let first = admit(&mut controller, &runtime, LOCAL, 1);
    settle(&mut controller, &runtime);
    assert_eq!(controller.records[&first].state, State::Ready);

    // The stream session accepts the stop and never answers it.
    endpoint.hold_stops = true;
    controller.close(90, "amaterasu.nvhttp", first, 20, &runtime);
    for now in 21..40 {
        controller.advance_work(&runtime, now);
        controller.schedule_work(now);
    }
    assert_eq!(endpoint.stops.len(), 1);
    assert_eq!(controller.records[&first].state, State::Closing);
    assert!(controller.retiring_jobs.contains_key(&first));
    // The job slot that reads requests and prepares tickets is free.
    assert!(controller.job.is_none());

    let second = admit(&mut controller, &runtime, LOCAL, 2);
    for now in 41..60 {
        controller.advance_work(&runtime, now);
        controller.schedule_work(now);
    }
    assert_eq!(controller.records[&second].state, State::Ready);
    assert_eq!(controller.records[&first].state, State::Closing);
    assert_eq!(endpoint.prepares.len(), 2);
    assert_ne!(
        controller.records[&second].media_binds,
        controller.records[&first].media_binds
    );
}

#[test]
fn every_closing_ticket_retires_on_its_own() {
    let _guard = super::tests::NETWORK_TEST_LOCK.lock().unwrap();
    let (mut controller, mut endpoint, runtime) = fixture(LOCAL);
    let first = admit(&mut controller, &runtime, LOCAL, 1);
    settle(&mut controller, &runtime);
    let second = admit(&mut controller, &runtime, LOCAL, 2);
    settle(&mut controller, &runtime);

    endpoint.hold_stops = true;
    controller.close(90, "amaterasu.nvhttp", first, 20, &runtime);
    controller.advance_work(&runtime, 21);
    // The first stop is never answered. The second ticket still retires.
    endpoint.hold_stops = false;
    controller.close(91, "amaterasu.nvhttp", second, 22, &runtime);
    for now in 23..40 {
        controller.advance_work(&runtime, now);
        controller.schedule_work(now);
    }
    assert_eq!(controller.records[&first].state, State::Closing);
    assert_eq!(controller.records[&second].state, State::Closed);
    assert!(controller.records[&second].cleanup_confirmed);
}

#[test]
fn ended_tickets_do_not_use_up_the_ticket_limit() {
    let _guard = super::tests::NETWORK_TEST_LOCK.lock().unwrap();
    let (mut controller, endpoint, runtime) = fixture(LOCAL);
    assert_eq!(controller.settings.max_live_tickets, 2);
    let first = admit(&mut controller, &runtime, LOCAL, 1);
    settle(&mut controller, &runtime);
    let second = admit(&mut controller, &runtime, LOCAL, 2);
    settle(&mut controller, &runtime);

    controller.open(
        OpenRequest {
            request: 3,
            caller: "amaterasu.nvhttp".into(),
            config: request(LOCAL, 3),
            deadline: 5_000,
            display_preflight_deadline: None,
        },
        30,
        &runtime,
        false,
    );
    assert_eq!(
        endpoint.completed.last().unwrap()["code"],
        "capacity_exceeded"
    );

    // One ticket is closing and the other failed to close. Neither is open.
    controller.records.get_mut(&first).unwrap().state = State::Closing;
    let failed = controller.records.get_mut(&second).unwrap();
    failed.state = State::CleanupFailed;
    failed.cleanup_error = Some("dependency_unavailable");
    let third = admit(&mut controller, &runtime, LOCAL, 4);
    let fourth = admit(&mut controller, &runtime, LOCAL, 5);
    assert_eq!(controller.records[&third].state, State::Preparing);
    assert_eq!(controller.records[&fourth].state, State::Preparing);
    assert_eq!(controller.records.len(), 4);
}

#[test]
fn a_record_table_full_of_ended_tickets_still_admits() {
    let _guard = super::tests::NETWORK_TEST_LOCK.lock().unwrap();
    let (mut controller, _endpoint, runtime) = fixture(LOCAL);
    for id in 1..=MAX_RECORDS as u64 {
        let mut record = super::tests::record(State::CleanupFailed);
        record.id = id;
        record.session = Some(1_000 + id);
        record.cleanup_error = Some("dependency_unavailable");
        record.cleanup_retry_count = MAX_CLEANUP_RETRIES;
        controller.records.insert(id, record);
    }
    let admitted = admit(&mut controller, &runtime, LOCAL, 9);
    assert_eq!(controller.records[&admitted].state, State::Preparing);
    assert_eq!(controller.records.len(), MAX_RECORDS);
}

fn listed(sessions: Value) -> Value {
    let total = sessions.as_array().unwrap().len();
    json!({"ok":true,"sessions":sessions,"total":total,"truncated":false,
        "startup_reconciled":true})
}

#[test]
fn startup_reconciliation_never_stops_a_ticket_of_this_runtime() {
    let current = CurrentTickets {
        operations: BTreeSet::from(["rtsp-0000000000000005".to_owned()]),
        sessions: BTreeSet::from([41]),
        preparing: true,
    };
    let page = listed(json!([
        {"session":30,"state":"active","cleanup_error":null,
            "operation_id":"rtsp-00000000000000aa"},
        {"session":40,"state":"preparing","cleanup_error":null,
            "operation_id":"rtsp-0000000000000005"},
        {"session":41,"state":"active","cleanup_error":null,
            "operation_id":"rtsp-0000000000000006"},
        {"session":42,"state":"stopped","cleanup_error":null,
            "operation_id":"rtsp-00000000000000bb"}
    ]));
    // Only the session an earlier runtime left behind is stopped.
    assert_eq!(
        recovery_list(&page, &current).unwrap(),
        Some(VecDeque::from([30]))
    );
}

#[test]
fn startup_reconciliation_waits_when_it_cannot_tell_a_new_session_from_an_old_one() {
    let unidentified = listed(json!([
        {"session":30,"state":"preparing","cleanup_error":null}
    ]));
    let mut current = CurrentTickets {
        operations: BTreeSet::from(["rtsp-0000000000000005".to_owned()]),
        sessions: BTreeSet::new(),
        preparing: true,
    };
    // A provider that does not name the operation leaves it ambiguous while
    // one of this runtime's tickets is still being prepared.
    assert_eq!(recovery_list(&unidentified, &current).unwrap(), None);
    current.preparing = false;
    assert_eq!(
        recovery_list(&unidentified, &current).unwrap(),
        Some(VecDeque::from([30]))
    );
}
