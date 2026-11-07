//! Bounded ownership of session workers, including failed descriptor handoffs.
use super::*;

pub(super) struct SessionOwner {
    shutdown: ChannelShutdown,
    thread: Option<JoinHandle<AuthorityResult<()>>>,
}

impl SessionOwner {
    pub(super) fn new(shutdown: ChannelShutdown, thread: JoinHandle<AuthorityResult<()>>) -> Self {
        Self {
            shutdown,
            thread: Some(thread),
        }
    }

    fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn join(&mut self) -> AuthorityResult<()> {
        self.shutdown.shutdown();
        match self.thread.take() {
            None => Ok(()),
            Some(thread) => thread
                .join()
                .map_err(|_| "capture authority session thread panicked".to_owned())?,
        }
    }
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        if let Err(error) = self.join() {
            eprintln!("capture authority session containment failed: {error}");
        }
    }
}

#[derive(Default)]
pub(super) struct SessionRegistry {
    active: Vec<SessionOwner>,
    first_error: Option<String>,
}

impl SessionRegistry {
    pub(super) fn len(&self) -> usize {
        self.active.len()
    }

    pub(super) fn insert(&mut self, owner: SessionOwner) -> AuthorityResult<()> {
        if self.active.len() >= MAX_SESSIONS {
            return Err("capture authority session limit reached".into());
        }
        self.active.push(owner);
        Ok(())
    }

    fn record(&mut self, result: AuthorityResult<()>) {
        if let Err(error) = result {
            eprintln!("capture authority session failed: {error}");
            self.first_error.get_or_insert(error);
        }
    }

    pub(super) fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.active.len() {
            if self.active[index].is_finished() {
                let mut owner = self.active.swap_remove(index);
                let result = owner.join();
                self.record(result);
            } else {
                index += 1;
            }
        }
    }

    pub(super) fn drain(&mut self) -> AuthorityResult<()> {
        // Signal every independent owner before waiting for any one of them.
        for owner in &self.active {
            owner.shutdown.shutdown();
        }
        while let Some(mut owner) = self.active.pop() {
            let result = owner.join();
            self.record(result);
        }
        self.first_error.take().map_or(Ok(()), Err)
    }
}

impl Drop for SessionRegistry {
    fn drop(&mut self) {
        if let Err(error) = self.drain() {
            eprintln!("capture authority session drain failed: {error}");
        }
    }
}
