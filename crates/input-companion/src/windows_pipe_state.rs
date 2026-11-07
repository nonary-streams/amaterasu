//! Ownership and deadline rules shared by the Windows pipe I/O boundary.
use crate::{InputError, InputResult};
use std::cell::UnsafeCell;
use std::time::Instant;

pub(crate) const MAX_FRAME: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoKind {
    Connect,
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoResult {
    Transferred(usize),
    Aborted,
    Disconnected,
    Failed,
}

enum Phase {
    NotStarted,
    Pending { cancellation_requested: bool },
    Retired(IoResult),
}

/// The allocation remains owned while the kernel may still borrow its bytes.
pub(crate) struct OwnedIo {
    kind: IoKind,
    bytes: Vec<UnsafeCell<u8>>,
    phase: Phase,
}

pub(crate) struct RetiredIo {
    pub(crate) kind: IoKind,
    pub(crate) bytes: Vec<u8>,
    pub(crate) result: IoResult,
}

impl OwnedIo {
    pub(crate) fn new(kind: IoKind, bytes: Vec<u8>) -> Self {
        Self {
            kind,
            bytes: bytes.into_iter().map(UnsafeCell::new).collect(),
            phase: Phase::NotStarted,
        }
    }

    pub(crate) fn kind(&self) -> IoKind {
        self.kind
    }

    pub(crate) fn start(&mut self) -> InputResult<()> {
        if !matches!(self.phase, Phase::NotStarted) {
            return Err(InputError::Device);
        }
        self.phase = Phase::Pending {
            cancellation_requested: false,
        };
        Ok(())
    }

    pub(crate) fn buffer(&mut self) -> (*mut u8, usize) {
        // UnsafeCell is transparent over u8. Only the platform boundary may
        // access these bytes until it observes terminal kernel completion.
        (self.bytes.as_mut_ptr().cast(), self.bytes.len())
    }

    pub(crate) fn is_pending(&self) -> bool {
        matches!(self.phase, Phase::Pending { .. })
    }

    pub(crate) fn request_cancellation(&mut self) -> bool {
        match &mut self.phase {
            Phase::Pending {
                cancellation_requested,
            } if !*cancellation_requested => {
                *cancellation_requested = true;
                true
            }
            _ => false,
        }
    }

    /// Called only after observing terminal completion of this exact operation.
    pub(crate) fn complete(&mut self, result: IoResult) {
        if self.is_pending() {
            self.phase = Phase::Retired(match result {
                IoResult::Transferred(length) if length > self.bytes.len() => IoResult::Failed,
                result => result,
            });
        }
    }

    pub(crate) fn into_retired(self) -> Result<RetiredIo, Self> {
        let Phase::Retired(result) = self.phase else {
            return Err(self);
        };
        Ok(RetiredIo {
            kind: self.kind,
            bytes: self.bytes.into_iter().map(UnsafeCell::into_inner).collect(),
            result,
        })
    }
}

/// Waits are recalculated against one absolute deadline, never INFINITE.
pub(crate) fn remaining_wait_ms(now: Instant, deadline: Instant) -> Option<u32> {
    let remaining = deadline.checked_duration_since(now)?;
    if remaining.is_zero() {
        return None;
    }
    Some(
        remaining
            .as_millis()
            .saturating_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
            .min(u128::from(u32::MAX - 1)) as u32,
    )
}

/// Cancellation checks slice the same budget; no slice starts a fresh timeout.
pub(crate) fn cancellable_wait_ms(now: Instant, deadline: Instant) -> Option<u32> {
    remaining_wait_ms(now, deadline).map(|milliseconds| milliseconds.min(10))
}

pub(crate) fn take_frame(buffered: &mut Vec<u8>) -> InputResult<Option<Vec<u8>>> {
    if let Some(end) = buffered.iter().position(|byte| *byte == b'\n') {
        if end >= MAX_FRAME {
            return Err(InputError::InvalidEvent);
        }
        let mut frame: Vec<_> = buffered.drain(..=end).collect();
        frame.pop();
        return Ok(Some(frame));
    }
    if buffered.len() >= MAX_FRAME {
        return Err(InputError::InvalidEvent);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cancellation_never_releases_a_kernel_borrow_without_completion() {
        let mut operation = OwnedIo::new(IoKind::Write, vec![1, 2, 3]);
        assert_eq!(operation.kind(), IoKind::Write);
        let borrowed_buffer = operation.buffer();
        operation.start().unwrap();
        assert!(operation.request_cancellation());
        assert!(!operation.request_cancellation());
        assert!(operation.is_pending());
        let mut operation = match operation.into_retired() {
            Err(operation) => operation,
            Ok(_) => panic!("cancellation is not retirement"),
        };
        assert_eq!(operation.buffer(), borrowed_buffer);
        operation.complete(IoResult::Aborted);
        let retired = operation.into_retired().ok().unwrap();
        assert_eq!(retired.kind, IoKind::Write);
        assert_eq!(retired.bytes, [1, 2, 3]);
        assert_eq!(retired.result, IoResult::Aborted);
    }

    #[test]
    fn cancellation_racing_success_preserves_the_actual_transferred_reply() {
        let mut operation = OwnedIo::new(IoKind::Read, vec![0; 9]);
        let (buffer, length) = operation.buffer();
        assert_eq!(length, 9);
        operation.start().unwrap();
        operation.request_cancellation();
        // SAFETY: this test acts as the sole foreign writer into the explicitly
        // mutable allocation. Rust reads it only after terminal completion.
        unsafe { std::ptr::copy_nonoverlapping(b"ok\n".as_ptr(), buffer, 3) };
        operation.complete(IoResult::Transferred(3));
        let retired = operation.into_retired().ok().unwrap();
        assert_eq!(retired.kind, IoKind::Read);
        let IoResult::Transferred(length) = retired.result else {
            panic!("successful race lost");
        };
        assert_eq!(&retired.bytes[..length], b"ok\n");
    }

    #[test]
    fn oversized_completion_is_rejected_and_an_operation_cannot_be_reissued() {
        let mut operation = OwnedIo::new(IoKind::Read, vec![0; 2]);
        operation.start().unwrap();
        assert_eq!(operation.start(), Err(InputError::Device));
        operation.complete(IoResult::Transferred(3));
        assert_eq!(
            operation.into_retired().ok().unwrap().result,
            IoResult::Failed
        );
    }

    #[test]
    fn absolute_wait_budget_expires_and_never_becomes_an_infinite_wait() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(20);
        assert_eq!(remaining_wait_ms(now, deadline), Some(20));
        assert_eq!(
            remaining_wait_ms(now + Duration::from_millis(19), deadline),
            Some(1)
        );
        assert_eq!(remaining_wait_ms(deadline, deadline), None);
        assert_eq!(
            remaining_wait_ms(deadline + Duration::from_secs(1), deadline),
            None
        );
        assert_eq!(
            remaining_wait_ms(now, now + Duration::from_nanos(1)),
            Some(1)
        );
        assert_eq!(
            remaining_wait_ms(now, now + Duration::from_secs(u64::from(u32::MAX))),
            Some(u32::MAX - 1)
        );
    }

    #[test]
    fn stop_poll_slices_do_not_renew_the_absolute_operation_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(25);
        assert_eq!(cancellable_wait_ms(now, deadline), Some(10));
        assert_eq!(
            cancellable_wait_ms(now + Duration::from_millis(10), deadline),
            Some(10)
        );
        assert_eq!(
            cancellable_wait_ms(now + Duration::from_millis(20), deadline),
            Some(5)
        );
        assert_eq!(cancellable_wait_ms(deadline, deadline), None);
    }

    #[test]
    fn cancelled_accept_keeps_ownership_until_the_exact_connect_retires() {
        let mut operation = OwnedIo::new(IoKind::Connect, Vec::new());
        operation.start().unwrap();
        operation.request_cancellation();
        let mut operation = match operation.into_retired() {
            Err(operation) => operation,
            Ok(_) => panic!("an accept cancellation is not retirement"),
        };
        operation.complete(IoResult::Disconnected);
        let retired = operation.into_retired().ok().unwrap();
        assert_eq!(retired.kind, IoKind::Connect);
        assert_eq!(retired.result, IoResult::Disconnected);
        assert!(retired.bytes.is_empty());
    }

    #[test]
    fn frame_limit_applies_even_when_an_oversized_line_arrives_in_one_read() {
        let mut buffered = vec![b'x'; MAX_FRAME];
        buffered.push(b'\n');
        assert_eq!(take_frame(&mut buffered), Err(InputError::InvalidEvent));
        let mut buffered = vec![b'x'; MAX_FRAME - 1];
        buffered.extend_from_slice(b"\nnext\n");
        assert_eq!(
            take_frame(&mut buffered).unwrap().unwrap().len(),
            MAX_FRAME - 1
        );
        assert_eq!(take_frame(&mut buffered).unwrap(), Some(b"next".to_vec()));
    }
}
