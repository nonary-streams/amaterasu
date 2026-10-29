//! Cleanup state is conservative: an unobserved ambiguous open is never declared closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Start,
    OpenWait(u64),
    Collect,
    StatusWait(u64),
    Cancel(u64),
    LookupStart,
    LookupWait(u64),
    CloseStart,
    CloseWait(u64),
    VerifyStart,
    VerifyWait(u64),
    Done,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Clean,
    Indeterminate,
}
#[derive(Debug)]
pub struct Lifecycle {
    pub phase: Phase,
    pub stream: Option<u64>,
    pub failure: Option<String>,
    pub outcome: Option<Outcome>,
    pub close_submitted: bool,
}
impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: Phase::Start,
            stream: None,
            failure: None,
            outcome: None,
            close_submitted: false,
        }
    }
    pub fn open_admitted(&mut self, id: u64) {
        self.phase = Phase::OpenWait(id);
    }
    pub fn open_rejected(&mut self) {
        self.failure = Some("open admission rejected".into());
        self.outcome = Some(Outcome::Clean);
        self.phase = Phase::Done;
    }
    pub fn open_uncertain(&mut self, id: Option<u64>, error: &str) {
        self.failure.get_or_insert_with(|| error.into());
        self.phase = id.map_or(Phase::LookupStart, Phase::Cancel);
    }
    pub fn opened(&mut self, stream: u64) {
        self.stream = Some(stream);
        self.phase = Phase::Collect;
    }
    pub fn fail(&mut self, error: &str) {
        self.failure.get_or_insert_with(|| error.into());
        self.phase = match self.phase {
            Phase::Start => {
                self.outcome = Some(Outcome::Clean);
                Phase::Done
            }
            Phase::OpenWait(id) => Phase::Cancel(id),
            Phase::StatusWait(id) => Phase::Cancel(id),
            Phase::CloseWait(id) | Phase::LookupWait(id) | Phase::VerifyWait(id) => {
                Phase::Cancel(id)
            }
            _ if self.stream.is_some() => Phase::CloseStart,
            _ => Phase::LookupStart,
        };
    }
    pub fn cancelled(&mut self) {
        self.phase = if self.stream.is_some() {
            Phase::CloseStart
        } else {
            Phase::LookupStart
        };
    }
    pub fn lookup_admitted(&mut self, id: u64, verify: bool) {
        self.phase = if verify {
            Phase::VerifyWait(id)
        } else {
            Phase::LookupWait(id)
        };
    }
    pub fn lookup_active(&mut self, stream: u64) {
        if self.stream.is_some_and(|known| known != stream) {
            self.failure = Some("lookup stream identity changed".into());
            self.expired();
            return;
        }
        self.stream = Some(stream);
        self.phase = Phase::CloseStart;
    }
    pub fn lookup_missing(&mut self) {
        if self.stream.is_some() && self.close_submitted {
            self.outcome = Some(Outcome::Clean);
            self.phase = Phase::Done;
        } else {
            self.phase = Phase::LookupStart;
        }
    }
    pub fn lookup_failed(&mut self) {
        self.phase = if self.stream.is_some() {
            Phase::CloseStart
        } else {
            Phase::LookupStart
        };
    }
    pub fn close_admitted(&mut self, id: u64) {
        self.close_submitted = true;
        self.phase = Phase::CloseWait(id);
    }
    pub fn close_rejected(&mut self) {
        self.phase = Phase::CloseStart;
    }
    pub fn close_replied(&mut self) {
        self.phase = Phase::VerifyStart;
    }
    pub fn expired(&mut self) {
        self.outcome = Some(Outcome::Indeterminate);
        self.phase = Phase::Done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn denied_open_is_clean_without_a_stream() {
        let mut l = Lifecycle::new();
        l.open_rejected();
        assert_eq!(l.outcome, Some(Outcome::Clean));
    }
    #[test]
    fn timeout_cancels_then_reconciles_late_success() {
        let mut l = Lifecycle::new();
        l.open_admitted(1);
        l.fail("timeout");
        assert_eq!(l.phase, Phase::Cancel(1));
        l.cancelled();
        l.lookup_missing();
        assert_eq!(l.phase, Phase::LookupStart);
        assert_eq!(l.outcome, None);
        l.lookup_active(9);
        assert_eq!(l.phase, Phase::CloseStart);
        l.close_admitted(2);
        l.close_replied();
        l.lookup_missing();
        assert_eq!(l.outcome, Some(Outcome::Clean));
    }
    #[test]
    fn close_rejected_is_retried_and_cannot_claim_cleanup() {
        let mut l = Lifecycle::new();
        l.opened(7);
        l.fail("failure");
        l.close_rejected();
        assert_eq!(l.phase, Phase::CloseStart);
        l.lookup_missing();
        assert_eq!(l.outcome, None);
        l.close_admitted(3);
        l.close_replied();
        l.lookup_active(7);
        assert_eq!(l.phase, Phase::CloseStart);
        l.close_admitted(4);
        l.close_replied();
        l.lookup_missing();
        assert_eq!(l.outcome, Some(Outcome::Clean));
    }
    #[test]
    fn lost_close_reply_is_cancelled_then_retried() {
        let mut l = Lifecycle::new();
        l.opened(7);
        l.close_admitted(4);
        l.fail("lost reply");
        assert_eq!(l.phase, Phase::Cancel(4));
        l.cancelled();
        assert_eq!(l.phase, Phase::CloseStart);
        l.close_admitted(5);
        l.close_replied();
        l.lookup_missing();
        assert_eq!(l.outcome, Some(Outcome::Clean));
    }
    #[test]
    fn unresolved_open_or_cleanup_expires_indeterminate() {
        let mut l = Lifecycle::new();
        l.open_uncertain(Some(1), "timeout");
        l.cancelled();
        l.lookup_missing();
        l.expired();
        assert_eq!(l.outcome, Some(Outcome::Indeterminate));
        let mut l = Lifecycle::new();
        l.opened(2);
        l.fail("error");
        l.close_rejected();
        l.expired();
        assert_eq!(l.outcome, Some(Outcome::Indeterminate));
    }
    #[test]
    fn cancel_status_retries_close() {
        let mut l = Lifecycle::new();
        l.opened(8);
        l.phase = Phase::StatusWait(3);
        l.fail("cancelled");
        assert_eq!(l.phase, Phase::Cancel(3));
        l.cancelled();
        assert_eq!(l.phase, Phase::CloseStart);
    }
    #[test]
    fn changed_lookup_identity_is_indeterminate() {
        let mut l = Lifecycle::new();
        l.opened(3);
        l.fail("cleanup");
        l.lookup_active(4);
        assert_eq!(l.outcome, Some(Outcome::Indeterminate));
    }
}
