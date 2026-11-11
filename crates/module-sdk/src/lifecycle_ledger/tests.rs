use super::*;

#[derive(Clone, Debug, PartialEq)]
struct Record {
    admission: Admission,
    retirement: Retirement,
}

impl Owner for Record {
    fn admission(&self) -> Admission {
        self.admission
    }

    fn retirement(&self) -> Retirement {
        self.retirement
    }
}

fn open() -> Record {
    Record {
        admission: Admission::Open,
        retirement: Retirement::Pending,
    }
}

fn ended(retirement: Retirement) -> Record {
    Record {
        admission: Admission::Ended,
        retirement,
    }
}

const EVERY_RETIREMENT: [Retirement; 5] = [
    Retirement::Confirmed,
    Retirement::Superseded,
    Retirement::Abandoned,
    Retirement::Unconfirmed,
    Retirement::Pending,
];

/// Deterministic pseudo-random numbers, so a failure reproduces.
struct Sequence(u64);

impl Sequence {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

#[test]
fn ended_owners_never_change_an_admission_decision() {
    let mut sequence = Sequence(0x9e37_79b9_7f4a_7c15);
    for _ in 0..2_000 {
        let open_count = sequence.next(6) as usize;
        let limit = sequence.next(6) as usize;
        let live_only: Vec<Record> = (0..open_count).map(|_| open()).collect();

        // The same open owners, interleaved with ended owners in every
        // retirement state, including cleanup that never finishes.
        let mut mixed = Vec::new();
        for record in &live_only {
            for _ in 0..sequence.next(4) {
                let retirement = EVERY_RETIREMENT[sequence.next(5) as usize];
                mixed.push(ended(retirement));
            }
            mixed.push(record.clone());
        }
        for _ in 0..sequence.next(40) {
            mixed.push(ended(EVERY_RETIREMENT[sequence.next(5) as usize]));
        }

        let alone = Live::of(&live_only);
        let crowded = Live::of(&mixed);
        assert_eq!(alone.len(), crowded.len());
        assert_eq!(alone.admits(limit), crowded.admits(limit));
        assert!(
            crowded
                .iter()
                .all(|record| record.admission == Admission::Open)
        );
    }
}

#[test]
fn room_is_made_from_the_most_settled_ended_owner_first() {
    let mut records = BTreeMap::from([
        (1, ended(Retirement::Pending)),
        (2, ended(Retirement::Unconfirmed)),
        (3, open()),
        (4, ended(Retirement::Confirmed)),
        (5, ended(Retirement::Abandoned)),
        (6, ended(Retirement::Confirmed)),
        (7, ended(Retirement::Superseded)),
    ]);
    let mut dropped = Vec::new();
    for limit in [7, 6, 5, 4, 3, 2] {
        assert!(make_room(
            &mut records,
            limit,
            |_, _| true,
            |key, _| dropped.push(key),
        ));
        assert!(records.len() < limit);
    }
    // Confirmed owners go first, oldest first, and cleanup still in flight
    // is the last thing given up.
    assert_eq!(dropped, [4, 6, 7, 5, 2, 1]);
    assert_eq!(records.keys().copied().collect::<Vec<_>>(), [3]);
}

#[test]
fn room_is_never_made_from_an_open_or_pinned_owner() {
    let mut records = BTreeMap::from([(1, open()), (2, ended(Retirement::Confirmed)), (3, open())]);
    let mut dropped = Vec::new();
    assert!(!make_room(
        &mut records,
        2,
        |key, _| *key != 2,
        |key, _| dropped.push(key),
    ));
    assert!(dropped.is_empty());
    assert_eq!(records.len(), 3);

    assert!(!make_room(
        &mut records,
        2,
        |_, _| true,
        |key, _| dropped.push(key),
    ));
    assert_eq!(dropped, [2]);
    assert_eq!(records.keys().copied().collect::<Vec<_>>(), [1, 3]);
}

#[test]
fn any_number_of_unfinished_cleanups_leaves_room_for_a_new_owner() {
    let mut records: BTreeMap<u64, Record> = (0..128)
        .map(|id| (id, ended(Retirement::Pending)))
        .collect();
    let mut dropped = 0;
    assert!(make_room(
        &mut records,
        128,
        |_, _| true,
        |_, _| dropped += 1
    ));
    assert_eq!(dropped, 1);
    assert!(Live::of(records.values()).admits(1));
}
