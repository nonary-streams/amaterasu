//! A test host shared by the ABI caller and the lifecycle service worker.
use amaterasu_module_sdk::{Command, Response};
use amaterasu_runtime::Broker;
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

const LIFECYCLE: &str = "amaterasu.session-lifecycle";

struct BrokerState {
    broker: Broker,
    lifecycle_turns: u64,
}

pub struct TestBroker {
    state: Mutex<BrokerState>,
    lifecycle_progress: Condvar,
}

impl TestBroker {
    pub fn new(broker: Broker) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BrokerState {
                broker,
                lifecycle_turns: 0,
            }),
            lifecycle_progress: Condvar::new(),
        })
    }

    pub fn dispatch(&self, principal: &str, command: Command) -> Response {
        let mut state = self.state.lock().unwrap();
        let poll = matches!(command, Command::Poll {});
        let complete = matches!(command, Command::Complete { .. });
        let check = matches!(command, Command::CheckRequest { .. });
        let response = state.broker.dispatch(principal, command);
        let turn_ended = match &response {
            Response::Pending => poll,
            Response::Ok => complete,
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                check || complete
            }
            _ => false,
        };
        if principal == LIFECYCLE && turn_ended {
            state.lifecycle_turns += 1;
            self.lifecycle_progress.notify_all();
        }
        response
    }

    /// Advance deterministic provider fixtures only after this lifecycle turn
    /// has finished handling its request or observed an empty queue.
    #[allow(dead_code, reason = "startup-only fixtures never tick")]
    pub fn tick(&self, principal: &str, tick: impl FnOnce()) {
        if principal != LIFECYCLE {
            tick();
            return;
        }
        let previous = self.state.lock().unwrap().lifecycle_turns;
        tick();
        let (_state, timeout) = self
            .lifecycle_progress
            .wait_timeout_while(
                self.state.lock().unwrap(),
                Duration::from_secs(5),
                |state| state.lifecycle_turns == previous,
            )
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "lifecycle worker did not finish its turn"
        );
    }
}
