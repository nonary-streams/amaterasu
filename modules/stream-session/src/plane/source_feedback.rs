//! Single-item source feedback ownership. No source route is installed here.
use super::{
    FeedbackEncodeError, HostFeedback, HostFeedbackDelivery, HostFeedbackMessage, PlaneResult,
    encode_host_feedback, feedback_scope,
};
use amaterasu_module_sdk::effect_authority::consumer::FeedbackLease;
use amaterasu_module_sdk::input::extensions::MAX_PAYLOAD_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    Pending,
    AlreadyPending,
    AlreadySent,
    AlreadyDropped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Stopped,
    Busy,
    Invalid(FeedbackEncodeError),
    InvalidIdentity,
    WrongDevice,
    Stale,
    ConflictingRetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Progress {
    Empty,
    Busy,
    Sent(u64),
    AwaitingAcknowledgment(u64),
}

struct Item {
    sequence: u64,
    operation_id: String,
    feedback: HostFeedback,
    encoded: HostFeedbackMessage,
    busy: bool,
    dropped: bool,
}

/// A bounded source queue that does not acknowledge producer work until the
/// native transport has accepted it. A sent item is retained for exact retry
/// reconciliation until the producer acknowledges it. This type is dormant
/// until authenticated composition installs a scoped lease.
pub struct SourceFeedbackPending {
    lease: Option<FeedbackLease>,
    pending: Option<Item>,
    sent: Option<Item>,
    high_water: u64,
    stopped: bool,
}

impl SourceFeedbackPending {
    pub fn new(lease: FeedbackLease) -> Self {
        Self {
            lease: Some(lease),
            pending: None,
            sent: None,
            high_water: 0,
            stopped: false,
        }
    }

    /// Never signals producer acknowledgment; ownership is reconciled by
    /// `Progress::Sent` followed by `acknowledge` after the producer confirms.
    pub fn enqueue(
        &mut self,
        sequence: u64,
        operation_id: &str,
        feedback: HostFeedback,
    ) -> Result<Admission, AdmissionError> {
        if self.stopped {
            return Err(AdmissionError::Stopped);
        }
        if sequence == 0
            || operation_id.is_empty()
            || operation_id.len() > 128
            || operation_id.chars().any(char::is_control)
        {
            return Err(AdmissionError::InvalidIdentity);
        }
        let lease = self.lease.as_ref().ok_or(AdmissionError::Stopped)?;
        if feedback_scope(&feedback).0 != lease.controller_slot() {
            return Err(AdmissionError::WrongDevice);
        }
        let encoded = encode_host_feedback(feedback.clone()).map_err(AdmissionError::Invalid)?;
        if encoded.payload.len().saturating_add(4) > MAX_PAYLOAD_BYTES {
            return Err(AdmissionError::InvalidIdentity);
        }
        if let Some(item) = self.pending.as_ref().or(self.sent.as_ref()) {
            if item.sequence == sequence {
                if item.operation_id != operation_id || item.encoded != encoded {
                    return Err(AdmissionError::ConflictingRetry);
                }
                return Ok(if self.pending.is_some() {
                    Admission::AlreadyPending
                } else if item.dropped {
                    Admission::AlreadyDropped
                } else {
                    Admission::AlreadySent
                });
            }
            return Err(if sequence <= self.high_water {
                AdmissionError::Stale
            } else {
                AdmissionError::Busy
            });
        }
        if sequence <= self.high_water {
            return Err(AdmissionError::Stale);
        }
        self.high_water = sequence;
        self.pending = Some(Item {
            sequence,
            operation_id: operation_id.into(),
            feedback,
            encoded,
            busy: false,
            dropped: false,
        });
        Ok(Admission::Pending)
    }

    /// `send` must be the selected plane's source-scoped feedback operation.
    /// An uncertain error permanently stops local admission and requires the
    /// owner to retire the whole stream before completing the lease.
    pub fn pump(
        &mut self,
        mut send: impl FnMut(&mut FeedbackLease, u64, &str, HostFeedback) -> PlaneResult<bool>,
    ) -> PlaneResult<Progress> {
        if self.stopped {
            return Err("source_feedback_stopped");
        }
        if let Some(item) = &self.sent {
            return Ok(Progress::AwaitingAcknowledgment(item.sequence));
        }
        let Some(item) = self.pending.as_mut() else {
            return Ok(Progress::Empty);
        };
        let lease = self.lease.as_mut().ok_or("source_feedback_stopped")?;
        match send(
            lease,
            item.sequence,
            &item.operation_id,
            item.feedback.clone(),
        ) {
            Ok(false) => {
                item.busy = true;
                Ok(Progress::Busy)
            }
            Ok(true) => {
                let item = self.pending.take().expect("pending feedback exists");
                let sequence = item.sequence;
                self.sent = Some(item);
                Ok(Progress::Sent(sequence))
            }
            Err(code) => {
                lease.fence();
                self.stopped = true;
                Err(code)
            }
        }
    }

    /// Resolve a pending best-effort block without sending it. Reliable stop
    /// controls are never dropped by this operation. The returned sequence
    /// still needs producer acknowledgment before a later item is accepted.
    pub fn drop_best_effort(&mut self) -> PlaneResult<Option<u64>> {
        if self.stopped {
            return Err("source_feedback_stopped");
        }
        let Some(item) = self.pending.as_ref() else {
            return Ok(None);
        };
        if item.encoded.delivery != HostFeedbackDelivery::SequencedUnreliable {
            return Ok(None);
        }
        if item.busy {
            self.lease
                .as_mut()
                .ok_or("source_feedback_stopped")?
                .cancel_retry(item.sequence, &item.operation_id)
                .map_err(|_| "source_feedback_cleanup_failed")?;
        }
        let mut item = self.pending.take().expect("pending feedback exists");
        let sequence = item.sequence;
        item.busy = false;
        item.dropped = true;
        self.sent = Some(item);
        Ok(Some(sequence))
    }

    /// Call only after the source confirms its acknowledgment. Repeated peeks
    /// before then are reconciled without retransmitting the effect.
    pub fn acknowledge(&mut self, sequence: u64, operation_id: &str) -> Result<(), AdmissionError> {
        if self
            .sent
            .as_ref()
            .is_some_and(|item| item.sequence == sequence && item.operation_id == operation_id)
        {
            self.sent = None;
            Ok(())
        } else {
            Err(AdmissionError::ConflictingRetry)
        }
    }

    /// Reconcile a busy-then-uncertain send after the selected transport has
    /// retired. `FeedbackLease::retire` still checks the exact receipt and
    /// refuses completion if any unmatched retry remains.
    pub fn retire_after_receipt(
        &mut self,
        receipt: &amaterasu_module_sdk::transport::control_retirement::Receipt,
    ) -> PlaneResult<amaterasu_module_sdk::effect_authority::consumer::StreamLeaseTicket> {
        let lease = self.lease.as_mut().ok_or("source_feedback_stopped")?;
        lease.fence();
        self.stopped = true;
        if let Some(item) = self.pending.as_ref().filter(|item| item.busy) {
            // A failed retry may have consumed the SDK retry record, or may
            // have failed before it reached the SDK. The lease's retirement
            // check below remains authoritative in either case.
            let _ = lease.cancel_retry(item.sequence, &item.operation_id);
        }
        let ticket = lease
            .retire(receipt)
            .map_err(|_| "source_feedback_retirement_failed")?;
        self.pending = None;
        self.sent = None;
        self.lease = None;
        Ok(ticket)
    }

    /// Stop admission and return the still-live lease for actual transport
    /// retirement. Previously admitted packets cannot be recalled here.
    pub fn fence(&mut self) -> PlaneResult<FeedbackLease> {
        let lease = self.lease.as_mut().ok_or("source_feedback_stopped")?;
        lease.fence();
        if let Some(item) = self.pending.as_ref().filter(|item| item.busy) {
            lease
                .cancel_retry(item.sequence, &item.operation_id)
                .map_err(|_| "source_feedback_cleanup_failed")?;
        }
        self.pending = None;
        self.sent = None;
        self.stopped = true;
        self.lease.take().ok_or("source_feedback_stopped")
    }
}
