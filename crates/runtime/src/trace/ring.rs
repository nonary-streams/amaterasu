//! Bounded lock-free queue of trace observations.
//!
//! Producers on any thread claim a slot with one compare-and-swap and never
//! wait for the collector. Every field is an atomic, so the queue needs no
//! unsafe code; a slot's sequence number publishes its contents.
use std::sync::atomic::{AtomicU64, Ordering};

/// One record as stored for the collector, with the endpoint that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    pub flow: u64,
    pub value: u64,
    pub timestamp_us: u64,
    pub track: u16,
    pub stage: u16,
    pub kind: u8,
    pub source: u16,
}

impl Observation {
    fn pack(&self) -> u64 {
        u64::from(self.track)
            | u64::from(self.stage) << 16
            | u64::from(self.source) << 32
            | u64::from(self.kind) << 48
    }

    fn unpack(flow: u64, value: u64, timestamp_us: u64, packed: u64) -> Self {
        Self {
            flow,
            value,
            timestamp_us,
            track: packed as u16,
            stage: (packed >> 16) as u16,
            source: (packed >> 32) as u16,
            kind: (packed >> 48) as u8,
        }
    }
}

struct Slot {
    /// Equals the slot's next write position when free, and that position plus
    /// one once a producer has published its fields.
    sequence: AtomicU64,
    flow: AtomicU64,
    value: AtomicU64,
    timestamp_us: AtomicU64,
    packed: AtomicU64,
}

pub(crate) struct Ring {
    slots: Box<[Slot]>,
    mask: u64,
    write: AtomicU64,
    read: AtomicU64,
    dropped: AtomicU64,
}

impl Ring {
    /// `capacity` is rounded up to a power of two.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        let slots = (0..capacity as u64)
            .map(|position| Slot {
                sequence: AtomicU64::new(position),
                flow: AtomicU64::new(0),
                value: AtomicU64::new(0),
                timestamp_us: AtomicU64::new(0),
                packed: AtomicU64::new(0),
            })
            .collect();
        Self {
            slots,
            mask: capacity as u64 - 1,
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    fn slot(&self, position: u64) -> &Slot {
        &self.slots[(position & self.mask) as usize]
    }

    /// Never blocks. A full queue drops the observation and counts it.
    pub fn push(&self, observation: Observation) -> bool {
        let mut position = self.write.load(Ordering::Relaxed);
        loop {
            let slot = self.slot(position);
            let sequence = slot.sequence.load(Ordering::Acquire);
            if sequence == position {
                match self.write.compare_exchange_weak(
                    position,
                    position + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        slot.flow.store(observation.flow, Ordering::Relaxed);
                        slot.value.store(observation.value, Ordering::Relaxed);
                        slot.timestamp_us
                            .store(observation.timestamp_us, Ordering::Relaxed);
                        slot.packed.store(observation.pack(), Ordering::Relaxed);
                        slot.sequence.store(position + 1, Ordering::Release);
                        return true;
                    }
                    Err(current) => position = current,
                }
            } else if sequence < position {
                // The collector has not released this slot yet: the queue is full.
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return false;
            } else {
                position = self.write.load(Ordering::Relaxed);
            }
        }
    }

    pub fn pop(&self) -> Option<Observation> {
        let mut position = self.read.load(Ordering::Relaxed);
        loop {
            let slot = self.slot(position);
            let sequence = slot.sequence.load(Ordering::Acquire);
            if sequence == position + 1 {
                match self.read.compare_exchange_weak(
                    position,
                    position + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let observation = Observation::unpack(
                            slot.flow.load(Ordering::Relaxed),
                            slot.value.load(Ordering::Relaxed),
                            slot.timestamp_us.load(Ordering::Relaxed),
                            slot.packed.load(Ordering::Relaxed),
                        );
                        slot.sequence
                            .store(position + self.mask + 1, Ordering::Release);
                        return Some(observation);
                    }
                    Err(current) => position = current,
                }
            } else if sequence <= position {
                return None;
            } else {
                position = self.read.load(Ordering::Relaxed);
            }
        }
    }

    /// Observations dropped since the previous call.
    pub fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn observation(flow: u64) -> Observation {
        Observation {
            flow,
            value: flow * 2,
            timestamp_us: flow + 7,
            track: 3,
            stage: 65_535,
            kind: 2,
            source: 40_000,
        }
    }

    #[test]
    fn records_round_trip_in_order_with_every_field() {
        let ring = Ring::new(8);
        for flow in 0..5 {
            assert!(ring.push(observation(flow)));
        }
        for flow in 0..5 {
            assert_eq!(ring.pop(), Some(observation(flow)));
        }
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn a_full_ring_drops_new_records_and_recovers_after_draining() {
        let ring = Ring::new(4);
        for flow in 0..4 {
            assert!(ring.push(observation(flow)));
        }
        assert!(!ring.push(observation(99)));
        assert!(!ring.push(observation(100)));
        assert_eq!(ring.take_dropped(), 2);
        assert_eq!(ring.take_dropped(), 0);
        assert_eq!(ring.pop(), Some(observation(0)));
        assert!(ring.push(observation(4)));
        for flow in 1..5 {
            assert_eq!(ring.pop(), Some(observation(flow)));
        }
    }

    #[test]
    fn concurrent_producers_lose_nothing_while_the_collector_keeps_up() {
        const PRODUCERS: u64 = 4;
        const PER_PRODUCER: u64 = 20_000;
        let ring = Arc::new(Ring::new(1024));
        let workers: Vec<_> = (0..PRODUCERS)
            .map(|producer| {
                let ring = ring.clone();
                std::thread::spawn(move || {
                    for index in 0..PER_PRODUCER {
                        let item = observation(producer * PER_PRODUCER + index);
                        while !ring.push(item) {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();
        let mut seen = vec![false; (PRODUCERS * PER_PRODUCER) as usize];
        let mut last_per_producer = [None::<u64>; PRODUCERS as usize];
        let mut received = 0;
        while received < PRODUCERS * PER_PRODUCER {
            let Some(item) = ring.pop() else {
                std::thread::yield_now();
                continue;
            };
            assert_eq!(item, observation(item.flow));
            assert!(!std::mem::replace(&mut seen[item.flow as usize], true));
            let producer = (item.flow / PER_PRODUCER) as usize;
            assert!(last_per_producer[producer].is_none_or(|last| last < item.flow));
            last_per_producer[producer] = Some(item.flow);
            received += 1;
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(ring.pop(), None);
    }
}
