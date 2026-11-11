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

type Table = LeaseTable<&'static str, u32, u64>;

fn granted(outcome: Acquire<u32, u64>) -> (FenceToken, Option<Lease<u32, u64>>) {
    match outcome {
        Acquire::Granted { token, superseded } => (token, superseded),
        other => panic!("expected a grant, got {other:?}"),
    }
}

#[test]
fn an_open_holder_is_a_conflict_and_asking_again_is_idempotent() {
    let mut table = Table::new();
    let (token, superseded) = granted(table.acquire(&"port", &1, Takeover::Allowed, 0));
    assert_eq!(superseded, None);
    assert_eq!(
        granted(table.acquire(&"port", &1, Takeover::Allowed, 5)).0,
        token
    );
    assert_eq!(
        table.acquire(&"port", &2, Takeover::Allowed, 1_000_000),
        Acquire::Held { by: 1 }
    );
}

#[test]
fn an_ended_holder_is_waited_for_and_then_superseded() {
    let mut table = Table::new();
    let (first, _) = granted(table.acquire(&"port", &1, Takeover::Allowed, 0));
    table.end(&1, 100);

    assert_eq!(
        table.acquire(&"port", &2, Takeover::Allowed, 99),
        Acquire::Releasing { by: 1, until: 100 }
    );
    let (second, superseded) = granted(table.acquire(&"port", &2, Takeover::Allowed, 100));
    assert!(second > first);
    assert_eq!(
        superseded,
        Some(Lease {
            owner: 1,
            token: first,
            release_by: Some(100),
        })
    );
}

#[test]
fn a_resource_that_cannot_be_taken_over_is_named_as_unavailable() {
    let mut table = Table::new();
    granted(table.acquire(&"display", &1, Takeover::Refused, 0));
    table.end(&1, 100);
    assert_eq!(
        table.acquire(&"display", &2, Takeover::Refused, 50),
        Acquire::Releasing { by: 1, until: 100 }
    );
    assert_eq!(
        table.acquire(&"display", &2, Takeover::Refused, 100),
        Acquire::Unavailable { by: 1 }
    );
    // The same resource is still granted the moment its holder releases.
    table.release_owner(&1);
    granted(table.acquire(&"display", &2, Takeover::Refused, 101));
}

#[test]
fn ending_revokes_mutation_at_once_and_keeps_cleanup_until_superseded() {
    let mut table = Table::new();
    let (first, _) = granted(table.acquire(&"port", &1, Takeover::Allowed, 0));
    assert!(table.may_mutate(&"port", first));
    assert!(table.may_retire(&"port", first));

    table.end(&1, 100);
    assert!(!table.may_mutate(&"port", first));
    assert!(table.may_retire(&"port", first));

    let (second, _) = granted(table.acquire(&"port", &2, Takeover::Allowed, 100));
    assert!(!table.may_retire(&"port", first));
    assert!(table.may_mutate(&"port", second));
}

#[test]
fn late_work_from_a_superseded_owner_cannot_touch_its_successor() {
    let mut table = Table::new();
    let (first, _) = granted(table.acquire(&"port", &1, Takeover::Allowed, 0));
    table.end(&1, 100);
    let (second, _) = granted(table.acquire(&"port", &2, Takeover::Allowed, 100));

    assert!(!table.release(&"port", first));
    table.release_owner(&1);
    table.end(&1, 5_000);
    assert_eq!(table.holder(&"port").map(|lease| lease.owner), Some(2));
    assert!(table.may_mutate(&"port", second));
    assert!(table.release(&"port", second));
    assert_eq!(table.holder(&"port"), None);
}

#[test]
fn ending_again_cannot_extend_a_successors_wait() {
    let mut table = Table::new();
    granted(table.acquire(&"port", &1, Takeover::Allowed, 0));
    table.end(&1, 100);
    table.end(&1, 9_000);
    granted(table.acquire(&"port", &2, Takeover::Allowed, 100));
}

#[test]
fn tokens_are_never_reused_after_a_release() {
    let mut table = Table::new();
    let mut last = FenceToken(0);
    for owner in 0..50 {
        let (token, _) = granted(table.acquire(&"port", &owner, Takeover::Allowed, 0));
        assert!(token > last);
        last = token;
        assert!(table.release(&"port", token));
    }
}

/// Whatever ended owners do afterwards, a request is answered by the instant
/// their wait ends: granted, or one named resource reported unavailable.
#[test]
fn a_request_against_ended_holders_is_answered_by_their_release_instant() {
    const RESOURCES: [&str; 3] = ["port", "display", "sink"];
    let mut sequence = Sequence(0x2545_f491_4f6c_dd1d);
    for _ in 0..500 {
        let mut table = Table::new();
        let mut tokens = Vec::new();
        for (index, resource) in RESOURCES.iter().enumerate() {
            let owner = index as u32;
            let (token, _) = granted(table.acquire(resource, &owner, Takeover::Allowed, 0));
            tokens.push(token);
            table.end(&owner, 100 + sequence.next(400));
        }
        let wait_ends = 500;

        // Old owners keep working: stale releases, repeated ends, some
        // genuine releases, in any order.
        for _ in 0..sequence.next(12) {
            let index = sequence.next(3) as usize;
            match sequence.next(3) {
                0 => {
                    table.release(&RESOURCES[index], tokens[index]);
                }
                1 => table.end(&(index as u32), 1_000_000),
                _ => {
                    table.release(&RESOURCES[index], FenceToken(999));
                }
            }
        }

        let successor = 7;
        for resource in &RESOURCES {
            let takeover = if sequence.next(2) == 0 {
                Takeover::Allowed
            } else {
                Takeover::Refused
            };
            match table.acquire(resource, &successor, takeover, wait_ends) {
                Acquire::Granted { .. } => {
                    assert_eq!(table.holder(resource).map(|lease| lease.owner), Some(7));
                }
                Acquire::Unavailable { by } => {
                    assert_eq!(takeover, Takeover::Refused);
                    assert_ne!(by, successor);
                }
                other => panic!("unbounded answer {other:?}"),
            }
        }
    }
}
