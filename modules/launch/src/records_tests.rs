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

/// Mark a started launch as cleanly finished with its retirement preserved.
fn finish(records: &mut Records, id: u64) {
    let record = records.values.get_mut(&id).unwrap();
    record.launch_started = true;
    record.state = State::Stopped;
    record.native_retirement_confirmed = true;
    records.record_native_retirement(id);
}

#[test]
fn finished_launches_do_not_use_up_launch_capacity() {
    let mut records = Records::new().unwrap();
    let mut operations = Vec::new();
    for index in 0..(MAX_OWNER_LAUNCHES * 3) {
        let operation = format!("ale1:{}:launch-{index}", records.operation_epoch());
        let (id, fresh) = start(&mut records, "owner", &operation)
            .unwrap_or_else(|code| panic!("launch {index} was refused: {code}"));
        assert!(fresh);
        finish(&mut records, id);
        operations.push(operation);
    }
    assert!(records.values.len() <= MAX_OWNER_LAUNCHES);
    // A pruned launch still proves its retirement and cannot be replayed.
    assert!(records.owned_operation("owner", &operations[0]).is_err());
    assert!(records.native_retired("owner", &operations[0]));
    assert_eq!(
        start(&mut records, "owner", &operations[0]),
        Err("operation_retired")
    );
}

#[test]
fn unfinished_or_failed_launches_do_not_use_up_launch_capacity() {
    let mut records = Records::new().unwrap();
    // More launches than the table holds are asked to stop, and none of the
    // stops is ever confirmed: some still running, some failed, some ended
    // without proof.
    for index in 0..(MAX_LAUNCHES * 2) {
        let operation = format!("ale1:{}:launch-{index}", records.operation_epoch());
        let (id, fresh) = start(&mut records, "owner", &operation)
            .unwrap_or_else(|code| panic!("launch {index} was refused: {code}"));
        assert!(fresh);
        let record = records.values.get_mut(&id).unwrap();
        record.launch_started = true;
        record.state = match index % 3 {
            0 => State::Stopping,
            1 => State::CleanupFailed,
            _ => State::Stopped,
        };
        if record.state == State::CleanupFailed {
            record.cleanup_error = Some("termination_unconfirmed");
        }
        assert!(records.values.len() <= MAX_LAUNCHES);
    }
}

#[test]
fn launches_that_are_starting_or_running_still_use_the_limit() {
    let mut records = Records::new().unwrap();
    for index in 0..MAX_OWNER_LAUNCHES {
        let operation = format!("ale1:{}:launch-{index}", records.operation_epoch());
        let (id, _) = start(&mut records, "owner", &operation).unwrap();
        if index % 2 == 0 {
            records.values.get_mut(&id).unwrap().state = State::Active;
        }
    }
    let operation = format!("ale1:{}:one-more", records.operation_epoch());
    assert_eq!(
        start(&mut records, "owner", &operation),
        Err("launch_limit")
    );
    assert_eq!(records.values.len(), MAX_OWNER_LAUNCHES);
    // Another owner has its own allowance.
    assert!(start(&mut records, "other", &operation).is_ok());
}

#[test]
fn a_launch_with_outstanding_broker_work_is_not_dropped_for_capacity() {
    let mut records = Records::new().unwrap();
    for index in 0..MAX_LAUNCHES {
        let owner = format!("owner-{}", index % 4);
        let operation = format!("ale1:{}:launch-{index}", records.operation_epoch());
        let (id, _) = start(&mut records, &owner, &operation).unwrap();
        let record = records.values.get_mut(&id).unwrap();
        record.launch_started = true;
        record.state = State::CleanupFailed;
        record.outstanding_request = Some(900 + id);
    }
    // Every record still owns a broker request, so none can be given up.
    let operation = format!("ale1:{}:one-more", records.operation_epoch());
    assert_eq!(
        start(&mut records, "owner-0", &operation),
        Err("launch_limit")
    );
    records.values.get_mut(&1).unwrap().outstanding_request = None;
    assert!(start(&mut records, "owner-0", &operation).is_ok());
    assert!(!records.values.contains_key(&1));
}
