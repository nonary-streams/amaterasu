use super::*;

fn start(records: &mut Records, owner: &str, operation: &str) -> Result<(u64, bool), &'static str> {
    records.start(
        owner,
        owner,
        operation.into(),
        "app".into(),
        CatalogSelection::Initiator,
        GlobalPreparation::default(),
    )
}

#[test]
fn rejected_tracked_admission_can_retry_after_capacity_is_available() {
    let mut records = Records::new().unwrap();
    for index in 0..16 {
        start(&mut records, "owner", &format!("legacy-{index}")).unwrap();
    }
    let operation = format!("ale1:{}:retry", records.operation_epoch());
    assert_eq!(
        start(&mut records, "owner", &operation),
        Err("launch_limit")
    );
    assert!(
        !records
            .admitted_operations
            .contains(&("owner".into(), operation.clone()))
    );
    records.values.remove(&1);
    let (id, fresh) = start(&mut records, "owner", &operation).unwrap();
    assert!(fresh);
    assert_eq!(start(&mut records, "owner", &operation), Ok((id, false)));
}

#[test]
fn failed_id_allocation_does_not_reserve_tracked_admission() {
    let mut records = Records::new().unwrap();
    records.next_id = u64::MAX;
    let operation = format!("ale1:{}:overflow", records.operation_epoch());
    assert_eq!(
        start(&mut records, "owner", &operation),
        Err("launch_limit")
    );
    assert!(
        !records
            .admitted_operations
            .contains(&("owner".into(), operation.clone()))
    );
    records.next_id = 0;
    assert_eq!(start(&mut records, "owner", &operation), Ok((1, true)));
}

#[test]
fn admitted_and_retired_operations_remain_fenced() {
    let mut records = Records::new().unwrap();
    let admitted = format!("ale1:{}:admitted", records.operation_epoch());
    let (id, _) = start(&mut records, "owner", &admitted).unwrap();
    records.values.remove(&id);
    assert_eq!(
        start(&mut records, "owner", &admitted),
        Err("operation_unconfirmed")
    );
    let retired = format!("ale1:{}:retired", records.operation_epoch());
    records.tombstone_absent("owner", &retired).unwrap();
    assert_eq!(
        start(&mut records, "owner", &retired),
        Err("operation_retired")
    );
}

#[test]
fn full_replay_ledger_allows_existing_operation_retry_but_rejects_new_admission() {
    let mut records = Records::new().unwrap();
    let admitted = format!("ale1:{}:existing", records.operation_epoch());
    let (id, _) = start(&mut records, "owner", &admitted).unwrap();
    for index in 0..4095 {
        records
            .retired_operations
            .insert(("owner".into(), format!("retired-{index}")));
    }
    let next = format!("ale1:{}:next", records.operation_epoch());
    assert_eq!(start(&mut records, "owner", &next), Err("operation_limit"));
    assert_eq!(start(&mut records, "owner", &admitted), Ok((id, false)));
    assert!(
        !records
            .admitted_operations
            .contains(&("owner".into(), next))
    );
}
