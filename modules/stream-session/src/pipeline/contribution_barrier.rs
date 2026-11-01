//! Tracks input-provider invalidations without waiting for unrelated traffic.
//!
//! The queue owns ordering: an invalidation at position N is settled only when
//! its batch and all preceding input events have been accepted. Later ordinary
//! input does not move the barrier. A failed partially applied batch is not proof
//! that a resource remains valid, so it fails contribution admission closed.
#[derive(Default)]
pub(super) struct ContributionBarrier {
    active_mask: u16,
    generation: u64,
    pending_through: usize,
    uncertain: bool,
}

impl ContributionBarrier {
    pub fn arrival(&mut self, queue_len: usize) {
        self.invalidate(queue_len);
    }

    pub fn controller_mask(&mut self, mask: u16, queue_len: usize) {
        if self.active_mask != mask {
            self.active_mask = mask;
            self.invalidate(queue_len);
        }
    }

    fn invalidate(&mut self, queue_len: usize) {
        if let Some(generation) = self.generation.checked_add(1) {
            self.generation = generation;
            self.pending_through = queue_len;
        } else {
            self.uncertain = true;
        }
    }

    pub fn accepted(&mut self, count: usize) {
        self.pending_through = self.pending_through.saturating_sub(count);
    }

    pub fn partial_failure(&mut self, accepted: usize) {
        self.accepted(accepted);
        if self.pending_through != 0 {
            self.uncertain = true;
        }
        // The failed event is removed by the input queue; never treat that as
        // settlement of a possibly affected invalidation.
        self.pending_through = self.pending_through.saturating_sub(1);
    }

    pub fn uncertain(&self) -> bool {
        self.uncertain
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn ready_generation(&self) -> Option<u64> {
        (!self.uncertain && self.pending_through == 0).then_some(self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_unrelated_traffic_does_not_move_the_barrier() {
        let mut barrier = ContributionBarrier::default();
        barrier.arrival(3);
        assert_eq!(barrier.ready_generation(), None);
        // Further traffic was queued after the third event.
        barrier.accepted(2);
        assert_eq!(barrier.ready_generation(), None);
        barrier.accepted(1);
        assert_eq!(barrier.ready_generation(), Some(1));
        for _ in 0..100 {
            barrier.controller_mask(0, 20);
        }
        assert_eq!(barrier.ready_generation(), Some(1));
    }

    #[test]
    fn mask_change_and_partial_failure_fence_admission() {
        let mut barrier = ContributionBarrier::default();
        barrier.controller_mask(1, 1);
        barrier.accepted(1);
        assert_eq!(barrier.ready_generation(), Some(1));
        barrier.controller_mask(1, 2);
        assert_eq!(barrier.ready_generation(), Some(1));
        barrier.controller_mask(0, 3);
        barrier.partial_failure(1);
        barrier.accepted(10);
        assert_eq!(barrier.ready_generation(), None);
    }
}
