use super::*;

#[test]
fn idle_retirement_rechecks_after_a_durable_change() {
    let directory = TestDirectory::new();
    let mut coordinator = Coordinator::open(&directory.journal()).unwrap();
    assert!(!coordinator.retire_ended_sessions_if_idle().unwrap());
    assert!(!coordinator.shared.borrow().retirement_scan_pending);
    for _ in 0..10 {
        assert!(!coordinator.retire_ended_sessions_if_idle().unwrap());
    }

    let prepared = coordinator.handle(
        "transport",
        "client",
        json!({"operation":"prepare", "operation_id":"prepare"}),
    );
    assert_eq!(prepared["status"], "pending");
    assert!(coordinator.shared.borrow().retirement_scan_pending);

    let receipt: Receipt = serde_json::from_value(json!({
        "caller":"transport", "initiator":"client", "request":{},
        "response":{"status":"pending"}
    }))
    .unwrap();
    coordinator
        .ephemeral
        .insert("busy".into(), (receipt, Instant::now()));
    assert!(!coordinator.retire_ended_sessions_if_idle().unwrap());
    assert!(coordinator.shared.borrow().retirement_scan_pending);
    coordinator.ephemeral.clear();
    assert!(!coordinator.retire_ended_sessions_if_idle().unwrap());
    assert!(!coordinator.shared.borrow().retirement_scan_pending);
}

#[test]
#[ignore = "opt-in benchmark; AMATERASU_BENCH_JOURNAL selects a read-only journal snapshot"]
fn benchmark_idle_scheduler_from_journal_snapshot() {
    let snapshot = std::env::var("AMATERASU_BENCH_JOURNAL").expect("set AMATERASU_BENCH_JOURNAL");
    let journal: Journal = serde_json::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
    validate_journal(&journal).unwrap();
    assert!(
        journal
            .receipts
            .values()
            .all(|receipt| receipt.response["status"] != "pending")
    );
    let directory = TestDirectory::new();
    let mut coordinator = Coordinator::open(&directory.journal()).unwrap();
    coordinator.shared.borrow_mut().value = journal;
    coordinator.configure_for_test(LifecycleSettings::default());
    let started = Instant::now();
    for _ in 0..500 {
        coordinator
            .advance(&mut |_| panic!("idle scheduler must not dispatch"))
            .unwrap();
    }
    eprintln!(
        "idle lifecycle: {:.1} us/tick",
        started.elapsed().as_secs_f64() * 1e6 / 500.0
    );
}
