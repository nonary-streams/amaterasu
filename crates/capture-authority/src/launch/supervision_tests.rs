use super::*;
use std::sync::mpsc;

fn session(task: impl FnOnce() -> AuthorityResult<()> + Send + 'static) -> SessionOwner {
    let (channel, _peer) = SeqpacketChannel::pair().unwrap();
    SessionOwner::new(channel.shutdown_handle().unwrap(), thread::spawn(task))
}

#[test]
fn failed_session_does_not_skip_other_joins_or_hide_retirement_failure() {
    let (release, held) = mpsc::channel();
    let (entered, ready) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    // Registry drains from the end: observe failure before the held owner.
    let mut sessions = registry(vec![
        session(move || {
            entered.send(()).unwrap();
            held.recv().unwrap();
            Ok(())
        }),
        session(|| Err("first session failed".into())),
    ]);
    ready.recv().unwrap();
    let supervisor = thread::spawn(move || {
        finished
            .send(finish_supervision(Ok(()), &mut sessions, || Ok(())))
            .unwrap()
    });
    let early = completion.recv_timeout(Duration::from_millis(30));
    release.send(()).unwrap();
    let result = early.clone().unwrap_or_else(|_| completion.recv().unwrap());
    supervisor.join().unwrap();
    assert!(
        early.is_err(),
        "one failed session must not detach another held owner"
    );
    assert_eq!(result.unwrap_err(), "first session failed");
}

#[test]
fn session_panic_and_worker_retirement_error_are_not_success() {
    let result = finish_supervision(
        Ok(()),
        &mut registry(vec![session(|| panic!("fixture session panic"))]),
        || Err("worker reap failed".into()),
    );
    let error = result.unwrap_err();
    assert!(error.contains("panicked"));
    assert!(error.contains("worker reap failed"));
}

fn registry(owners: Vec<SessionOwner>) -> SessionRegistry {
    let mut registry = SessionRegistry::default();
    for owner in owners {
        registry.insert(owner).unwrap();
    }
    registry
}

#[test]
fn unwinding_supervisor_keeps_each_thread_owned_until_it_finishes() {
    let (release, held) = mpsc::channel();
    let (entered, ready) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let (child_finished, child_completion) = mpsc::channel();
    let owner = session(move || {
        entered.send(()).unwrap();
        held.recv().unwrap();
        child_finished.send(()).unwrap();
        Ok(())
    });
    ready.recv().unwrap();
    let supervisor = thread::spawn(move || {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _sessions = registry(vec![owner]);
            panic!("fixture supervisor unwind");
        }));
        assert!(panic.is_err());
        finished.send(()).unwrap();
    });
    let early = completion.recv_timeout(Duration::from_millis(30));
    release.send(()).unwrap();
    child_completion.recv().unwrap();
    supervisor.join().unwrap();
    assert!(
        early.is_err(),
        "unwind detached a still-running session thread"
    );
}

#[test]
fn failed_handoff_guard_keeps_worker_owned_before_registry_insertion() {
    let (release, held) = mpsc::channel();
    let (entered, ready) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let owner = session(move || {
        entered.send(()).unwrap();
        held.recv().unwrap();
        Ok(())
    });
    ready.recv().unwrap();
    let supervisor = thread::spawn(move || {
        drop(owner);
        finished.send(()).unwrap();
    });
    let early = completion.recv_timeout(Duration::from_millis(30));
    release.send(()).unwrap();
    supervisor.join().unwrap();
    assert!(early.is_err(), "failed handoff detached a held worker");
}

#[test]
fn completed_session_error_stays_bounded_while_new_sessions_continue() {
    let mut sessions = registry(vec![session(|| Err("retired session failed".into()))]);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while sessions.len() != 0 {
        sessions.reap_finished();
        assert!(std::time::Instant::now() < deadline);
        thread::yield_now();
    }
    sessions.insert(session(|| Ok(()))).unwrap();
    assert_eq!(sessions.drain().unwrap_err(), "retired session failed");
    assert_eq!(sessions.len(), 0);
    assert!(sessions.drain().is_ok());
}

#[test]
fn lost_worker_identity_is_latched_even_after_a_later_observation() {
    let mut state = WorkerState::default();
    assert!(
        state
            .observe::<Option<ExitStatus>>(Err(io::Error::from_raw_os_error(libc::ECHILD)))
            .is_err()
    );
    assert!(state.identity_lost);
    assert!(state.observe(Ok(())).is_ok());
    assert!(
        state.identity_lost,
        "a reused PID cannot restore this child's ownership"
    );
}

#[test]
fn worker_retirement_runs_after_all_session_owners_despite_serve_error() {
    let (retired, observed) = mpsc::channel();
    let mut sessions = registry(vec![session(move || {
        retired.send(()).unwrap();
        Ok(())
    })]);
    let error = finish_supervision(Err("control failed".into()), &mut sessions, || {
        observed
            .try_recv()
            .expect("worker retirement must follow all session joins");
        Err("worker retirement failed".into())
    })
    .unwrap_err();
    assert!(error.contains("control failed"));
    assert!(error.contains("worker retirement failed"));
}
