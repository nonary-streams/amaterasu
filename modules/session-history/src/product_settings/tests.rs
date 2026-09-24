use super::*;

fn document(settings: &HistorySettings, revision: u64) -> Value {
    json!({
        "ok": true,
        "found": true,
        "version": 1,
        "revision": revision,
        "values": {
            "enabled": settings.enabled,
            "ttl_days": settings.ttl_days,
            "db_size_limit_mb": settings.db_size_limit_mb,
        },
    })
}

fn absent_document() -> Value {
    json!({
        "ok": true,
        "found": false,
        "version": null,
        "revision": 0,
        "values": {},
    })
}

#[test]
fn startup_imports_legacy_values_through_revision_zero_cas() {
    let legacy = HistorySettings {
        enabled: false,
        ttl_days: 13,
        db_size_limit_mb: 24,
    };
    let mut settings = ProductSettings::new(Some(legacy.clone()));
    settings.on_terminal(
        Work::ReconcileStartup {
            candidate: Some(legacy.clone()),
        },
        ChildOutcome::Payload(absent_document()),
    );

    let pending = settings.pending.as_ref().expect("migration is pending");
    assert!(matches!(pending.work, Work::Initialize { .. }));
    let Call::Submit { payload, .. } = &pending.call else {
        panic!("migration should submit a product CAS");
    };
    assert_eq!(payload["action"], "product_update");
    assert_eq!(payload["expected_revision"], 0);
    assert_eq!(payload["version"], PRODUCT_VERSION);
    assert_eq!(payload["values"]["enabled"], false);
    assert_eq!(payload["values"]["ttl_days"], 13);
    assert_eq!(payload["values"]["db_size_limit_mb"], 24);

    settings.pending.take();
    settings.on_terminal(
        Work::Initialize {
            candidate: legacy.clone(),
        },
        ChildOutcome::Payload(document(&legacy, 1)),
    );
    assert!(settings.is_ready());
    assert_eq!(settings.settings(), (&legacy, 1));
}

#[test]
fn existing_central_document_wins_over_a_stale_legacy_row() {
    let legacy = HistorySettings {
        enabled: false,
        ttl_days: 13,
        db_size_limit_mb: 24,
    };
    let central = HistorySettings {
        enabled: true,
        ttl_days: 5,
        db_size_limit_mb: 7,
    };
    let mut settings = ProductSettings::new(Some(legacy.clone()));
    settings.pending.take();
    settings.on_terminal(
        Work::ReconcileStartup {
            candidate: Some(legacy),
        },
        ChildOutcome::Payload(document(&central, 3)),
    );
    assert!(settings.is_ready());
    assert_eq!(settings.settings(), (&central, 3));
    assert!(settings.pending.is_none());
}

#[test]
fn settings_update_uses_cas_and_reports_only_confirmed_revision() {
    let current = HistorySettings::default();
    let desired = HistorySettings {
        enabled: false,
        ttl_days: 4,
        db_size_limit_mb: 15,
    };
    let mut settings = ProductSettings::new(None);
    settings.pending.take();
    settings.adopt(current, 1);
    settings.begin_update(42, 1, desired.clone()).unwrap();

    let pending = settings.pending.as_ref().unwrap();
    let Call::Submit { payload, .. } = &pending.call else {
        panic!("settings update should submit a CAS");
    };
    assert_eq!(payload["expected_revision"], 1);
    assert_eq!(payload["values"]["enabled"], false);

    settings.on_terminal(
        Work::Update {
            management_request: 42,
            expected_revision: 1,
            desired: desired.clone(),
        },
        ChildOutcome::Payload(document(&desired, 2)),
    );
    assert_eq!(settings.settings(), (&desired, 2));
    assert!(matches!(
        settings.take_completion(),
        Some(UpdateCompletion::Applied {
            request: 42,
            revision: 2,
            ..
        })
    ));
}

#[test]
fn uncertain_update_reconciles_durable_configuration_before_completing() {
    let original = HistorySettings::default();
    let desired = HistorySettings {
        enabled: false,
        ttl_days: 6,
        db_size_limit_mb: 0,
    };
    let concurrent = HistorySettings {
        enabled: true,
        ttl_days: 8,
        db_size_limit_mb: 2,
    };
    let mut settings = ProductSettings::new(None);
    settings.pending.take();
    settings.adopt(original, 1);
    let work = Work::Update {
        management_request: 44,
        expected_revision: 1,
        desired: desired.clone(),
    };
    settings.on_terminal(work, ChildOutcome::Payload(document(&desired, 3)));
    assert!(matches!(
        settings.pending.as_ref().map(|pending| &pending.work),
        Some(Work::ReconcileUpdate { .. })
    ));

    settings.on_terminal(
        Work::ReconcileUpdate {
            management_request: 44,
            expected_revision: 1,
            desired,
        },
        ChildOutcome::Payload(document(&concurrent, 2)),
    );
    assert_eq!(settings.settings(), (&concurrent, 2));
    assert!(matches!(
        settings.take_completion(),
        Some(UpdateCompletion::Failed { request: 44, response })
            if response["code"] == "revision_conflict" && response["revision"] == 2
    ));
}

#[test]
fn product_document_parser_rejects_unknown_versions_keys_and_invalid_ranges() {
    let mut unknown_version = document(&HistorySettings::default(), 1);
    unknown_version["version"] = json!(2);
    assert!(read_document(&unknown_version).is_err());

    let mut unknown_key = document(&HistorySettings::default(), 1);
    unknown_key["values"]["unrelated"] = json!(true);
    assert!(read_document(&unknown_key).is_err());

    let mut invalid_range = document(&HistorySettings::default(), 1);
    invalid_range["values"]["ttl_days"] = json!(-1);
    assert!(read_document(&invalid_range).is_err());
    assert!(matches!(
        read_document(&absent_document()),
        Ok(Document::Absent)
    ));
}
