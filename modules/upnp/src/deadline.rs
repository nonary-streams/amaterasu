use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One caller-selected shutdown budget shared by discovery and gateway operations.
///
/// Restricting this handle affects subsequent socket operations and SOAP actions
/// already running on an adapter that holds a clone. An outstanding provider call
/// retains its previously submitted timeout; the handle never renews that timeout.
#[derive(Clone, Debug, Default)]
pub struct OperationDeadline {
    earliest: Arc<Mutex<Option<Instant>>>,
}

impl OperationDeadline {
    /// Installs an absolute deadline or tightens an earlier caller's deadline.
    pub fn restrict(&self, deadline: Instant) {
        let mut earliest = self
            .earliest
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *earliest = Some(earliest.map_or(deadline, |current| current.min(deadline)));
    }

    /// Returns the earliest deadline selected by any holder of this handle.
    pub fn deadline(&self) -> Option<Instant> {
        *self
            .earliest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn is_expired(&self) -> bool {
        self.deadline()
            .is_some_and(|deadline| deadline <= Instant::now())
    }

    pub(crate) fn remaining(&self, operation_deadline: Instant) -> io::Result<Duration> {
        let deadline = self
            .deadline()
            .map_or(operation_deadline, |shared| shared.min(operation_deadline));
        deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "IGD operation deadline exceeded")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_earliest_deadline_without_renewing_it() {
        let deadline = OperationDeadline::default();
        let clone = deadline.clone();
        let earliest = Instant::now() + Duration::from_secs(1);
        clone.restrict(earliest);
        deadline.restrict(earliest + Duration::from_secs(30));
        assert_eq!(clone.deadline(), Some(earliest));
        deadline.restrict(Instant::now());
        assert!(clone.is_expired());
        assert!(clone.remaining(earliest).is_err());
    }
}
