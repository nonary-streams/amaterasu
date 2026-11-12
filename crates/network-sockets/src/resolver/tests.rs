use super::*;
use std::sync::{Arc, mpsc};

const FIXTURE_BYTES: &[u8] = include_bytes!(env!("AMATERASU_NETWORK_RESOLVER_FIXTURE_PATH"));

fn resolver(mode: &str) -> Resolver {
    Resolver::with_executable(Some(
        HelperExecutable::for_test_fixture(FIXTURE_BYTES, &[mode]).unwrap(),
    ))
}

fn await_lookup_count(resolver: &Resolver, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while resolver.state().unwrap().lookups.len() != expected {
        assert!(Instant::now() < deadline, "lookup was not admitted");
        thread::sleep(POLL_INTERVAL);
    }
}

fn await_quiescence(resolver: &Resolver) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !resolver.quiesce().unwrap() {
        assert!(Instant::now() < deadline, "helper process did not retire");
        thread::sleep(POLL_INTERVAL);
    }
}

#[test]
fn resolver_drop_does_not_wait_for_a_stalled_lookup() {
    let resolver = resolver("block");
    let request = Request {
        host: "fixture.invalid".into(),
        port: 42,
    }
    .encode()
    .unwrap();
    let child = resolver
        .state()
        .unwrap()
        .executable
        .as_ref()
        .unwrap()
        .spawn(&request)
        .unwrap();
    resolver.state().unwrap().lookups.push(Lookup {
        id: 1,
        child,
        response: Vec::new(),
        stdout_closed: false,
        cancelled: Arc::new(AtomicBool::new(false)),
        kill_requested: false,
    });
    let started = Instant::now();
    drop(resolver);
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn a_response_without_helper_retirement_is_not_success() {
    let resolver = resolver("addresses_then_block");
    let started = Instant::now();
    let error = resolver.resolve("fixture.invalid", 42, 50).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_millis(500));
    await_quiescence(&resolver);
}

#[test]
fn malformed_or_oversized_helper_output_is_bounded_and_retired() {
    for mode in ["malformed", "oversized"] {
        let resolver = resolver(mode);
        assert_eq!(
            resolver
                .resolve("fixture.invalid", 42, 1_000)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        await_quiescence(&resolver);
    }
}

#[test]
fn helper_admission_is_bounded_until_exact_processes_retire() {
    let resolver = Arc::new(resolver("block"));
    let mut callers = Vec::new();
    for _ in 0..MAX_WORKERS {
        let resolver = resolver.clone();
        callers.push(thread::spawn(move || {
            loop {
                match resolver.resolve("fixture.invalid", 42, 10_000) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL)
                    }
                    outcome => break outcome,
                }
            }
        }));
    }
    await_lookup_count(&resolver, MAX_WORKERS);
    assert_eq!(
        resolver
            .resolve("fixture.invalid", 42, 1_000)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    await_quiescence(&resolver);
    for caller in callers {
        assert_eq!(
            caller.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
}

#[test]
fn shutdown_announcements_cap_existing_lookup_without_renewing_budget() {
    let resolver = Arc::new(resolver("block"));
    let (done, result) = mpsc::sync_channel(1);
    let caller_resolver = resolver.clone();
    let caller = thread::spawn(move || {
        done.send(caller_resolver.resolve("fixture.invalid", 42, 10_000))
            .unwrap();
    });
    await_lookup_count(&resolver, 1);
    resolver.prepare_shutdown(25).unwrap();
    resolver.prepare_shutdown(10_000).unwrap();
    let completed = result.recv_timeout(Duration::from_millis(500));
    await_quiescence(&resolver);
    caller.join().unwrap();
    assert_eq!(
        completed.unwrap().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn announcement_preserves_admission_until_provider_quiescence() {
    let resolver = resolver("block");
    resolver.prepare_shutdown(1_000).unwrap();
    assert_eq!(
        resolver.resolve("127.0.0.1", 42, 1_000).unwrap(),
        ["127.0.0.1:42".parse::<SocketAddr>().unwrap()]
    );
    assert!(resolver.quiesce().unwrap());
    assert_eq!(
        resolver.resolve("127.0.0.1", 42, 1_000).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn quiescence_interrupts_a_waiting_lookup_and_proves_retirement() {
    let resolver = Arc::new(resolver("block"));
    let caller_resolver = resolver.clone();
    let caller = thread::spawn(move || caller_resolver.resolve("fixture.invalid", 42, 10_000));
    await_lookup_count(&resolver, 1);
    await_quiescence(&resolver);
    assert_eq!(
        caller.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    assert!(resolver.state().unwrap().lookups.is_empty());
}

#[test]
fn expired_shutdown_rejects_new_lookup_without_starting_a_helper() {
    let resolver = resolver("block");
    resolver.prepare_shutdown(0).unwrap();
    assert_eq!(
        resolver
            .resolve("fixture.invalid", 42, 1_000)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(resolver.state().unwrap().lookups.is_empty());
}

#[test]
fn shutdown_hooks_do_not_wait_for_the_lookup_registry_lock() {
    let resolver = Arc::new(resolver("block"));
    let held_registry = resolver.state().unwrap();
    let (done, completion) = mpsc::sync_channel(1);
    let caller = resolver.clone();
    let hooks = thread::spawn(move || {
        caller.prepare_shutdown(25).unwrap();
        caller.poll().unwrap();
        done.send(caller.quiesce().unwrap()).unwrap();
    });
    let result = completion.recv_timeout(Duration::from_millis(100));
    drop(held_registry);
    hooks.join().unwrap();
    assert!(!result.unwrap(), "a busy registry must remain pending");
    await_quiescence(&resolver);
}

#[test]
fn caller_timeout_is_not_blocked_by_another_spawn_holding_the_registry() {
    let resolver = Arc::new(resolver("block"));
    let (done, completion) = mpsc::sync_channel(1);
    let caller_resolver = resolver.clone();
    let caller = thread::spawn(move || {
        done.send(caller_resolver.resolve("fixture.invalid", 42, 50))
            .unwrap();
    });
    await_lookup_count(&resolver, 1);
    let held_registry = resolver.state().unwrap();
    let result = completion.recv_timeout(Duration::from_millis(150));
    drop(held_registry);
    caller.join().unwrap();
    assert_eq!(result.unwrap().unwrap_err().kind(), io::ErrorKind::TimedOut);
    await_quiescence(&resolver);
}

#[test]
fn owned_shutdown_does_not_wait_for_inflight_handle_admission() {
    let factory = crate::Factory::with_resolver(1, resolver("block"));
    let identity = (&*factory as *const crate::Factory) as usize;
    let shared = factory.shared.clone();
    let (held, admission_held) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let admitting = thread::spawn(move || {
        let _admission = shared.admission.lock().unwrap();
        held.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_millis(500));
    });
    admission_held.recv().unwrap();
    let started = Instant::now();
    let outcome = factory.try_shutdown();
    let elapsed = started.elapsed();
    let _ = release.send(());
    admitting.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(100),
        "shutdown waited for handle admission"
    );
    let failure = outcome.unwrap_err();
    assert_eq!(failure.reason().kind(), io::ErrorKind::WouldBlock);
    let (factory, _) = failure.into_parts();
    assert_eq!((&*factory as *const crate::Factory) as usize, identity);
    factory.try_shutdown().unwrap();
}

#[test]
fn owned_shutdown_returns_the_exact_factory_while_retirement_is_pending() {
    let factory = crate::Factory::with_resolver(1, resolver("block"));
    let identity = (&*factory as *const crate::Factory) as usize;
    let shared = factory.shared.clone();
    let held_registry = shared.resolver.state().unwrap();
    let pending = factory.try_shutdown().unwrap_err();
    assert_eq!(pending.reason().kind(), io::ErrorKind::WouldBlock);
    let (factory, reason) = pending.into_parts();
    assert_eq!(reason.kind(), io::ErrorKind::WouldBlock);
    assert_eq!((&*factory as *const crate::Factory) as usize, identity);
    drop(held_registry);
    factory.try_shutdown().unwrap();
}

#[test]
fn owned_shutdown_returns_the_exact_factory_and_provider_error() {
    let factory = crate::Factory::with_resolver(1, resolver("block"));
    let identity = (&*factory as *const crate::Factory) as usize;
    let shared = factory.shared.clone();
    let poison = thread::spawn(move || {
        let _registry = shared.resolver.state().unwrap();
        panic!("injected registry failure before any child admission");
    });
    assert!(poison.join().is_err());
    let failure = factory.try_shutdown().unwrap_err();
    assert_eq!(failure.reason().kind(), io::ErrorKind::Other);
    let (factory, reason) = failure.into_parts();
    assert_eq!((&*factory as *const crate::Factory) as usize, identity);
    assert_eq!(reason.kind(), io::ErrorKind::Other);
    {
        match factory.shared.resolver.state.lock() {
            Err(poisoned) => assert!(poisoned.get_ref().lookups.is_empty()),
            Ok(_) => panic!("expected the injected registry failure"),
        }
    }
    drop(factory);
}
