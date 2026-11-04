//! Transport cleanup consumes exact coordinator-backed lifecycle evidence. A
//! retired consumer may leave its original producer resources shared and pinned.
use super::*;
use amaterasu_extension_api::{LifecycleTarget, StreamRetirementProof, stream_retirement_proof};

pub(super) fn preparation_source(
    record: &Record,
    target: &Value,
    shared_source: Option<&Value>,
) -> Option<LifecycleTarget> {
    let target: LifecycleTarget = serde_json::from_value(target.clone()).ok()?;
    let source = shared_source
        .filter(|source| !source.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .ok()?
        .unwrap_or_else(|| target.clone());
    let joining = record.config.lifecycle_intent.as_deref() == Some("join");
    let owns_source = source == target;
    if !same_session_source(&target, &source)
        || (joining && owns_source)
        || (!joining && !owns_source)
    {
        return None;
    }
    Some(source)
}

fn same_session_source(target: &LifecycleTarget, source: &LifecycleTarget) -> bool {
    match (target, source) {
        (
            LifecycleTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                generation: 1..,
            },
            LifecycleTarget::Stream {
                host_epoch: source_epoch,
                session_id: source_session,
                stream_id: source_stream,
                generation: 1..,
            },
        ) => {
            host_epoch == source_epoch
                && session_id == source_session
                && [host_epoch, session_id, stream_id, source_stream]
                    .into_iter()
                    .all(|id| crate::model::valid_lifecycle_id(id))
        }
        _ => false,
    }
}

fn lifecycle_proof(record: &Record, payload: &Value) -> Result<Option<StreamRetirementProof>, ()> {
    let Some(target) = &record.lifecycle_target else {
        // A transport without an admitted lifecycle target cannot claim another
        // stream's shared ownership as proof of its own cleanup.
        return if [
            "lifecycle_retired",
            "lifecycle_consumer_retired",
            "lifecycle_resources_released",
            "lifecycle_shared_retained",
        ]
        .into_iter()
        .any(|field| payload[field] == true)
            || !payload["lifecycle_shared_source"].is_null()
        {
            Err(())
        } else {
            Ok(None)
        };
    };
    if payload.get("lifecycle_target") != Some(target) {
        return Err(());
    }
    let target: LifecycleTarget = serde_json::from_value(target.clone()).map_err(|_| ())?;
    let expected_source = record.lifecycle_source_target.as_ref();
    let mut receipt = json!({"status":"succeeded", "target":target});
    let fields = [
        ("lifecycle_consumer_retired", "consumer_retired"),
        ("lifecycle_resources_released", "resources_released"),
        ("lifecycle_shared_retained", "shared_retained"),
        ("lifecycle_shared_source", "shared_source"),
    ];
    let explicit = fields
        .iter()
        .any(|(field, _)| payload.get(*field).is_some());
    if explicit {
        for (field, key) in fields {
            if let Some(value) = payload.get(field) {
                receipt[key] = value.clone();
            }
        }
    } else if payload["lifecycle_retired"] != true {
        return Err(());
    }
    let proof = stream_retirement_proof(&receipt, &target, expected_source).map_err(|_| ())?;
    if payload["lifecycle_retired"].as_bool() != Some(proof.resources_released)
        || proof
            .shared_source
            .as_ref()
            .is_some_and(|source| !same_session_source(&target, source))
    {
        return Err(());
    }
    Ok(Some(proof))
}

fn locally_terminal(record: &Record, payload: &Value) -> bool {
    !record.lifecycle_prepare_outstanding
        && payload["ok"] == true
        && response_session(payload) == record.session
        && matches!(
            stream_state(payload),
            Some("stopped" | "failed" | "cancelled")
        )
        && payload["cleanup_error"].is_null()
}

pub(super) fn clean_terminal(record: &Record, payload: &Value) -> bool {
    locally_terminal(record, payload) && lifecycle_proof(record, payload).is_ok()
}

pub(super) fn confirm_terminal(record: &mut Record, payload: &Value) -> bool {
    if !locally_terminal(record, payload) {
        return false;
    }
    let Ok(proof) = lifecycle_proof(record, payload) else {
        return false;
    };
    record.lifecycle_retained_source = proof.and_then(|proof| proof.shared_source);
    record.cleanup_confirmed = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(stream_id: &str, generation: u64) -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            stream_id: stream_id.into(),
            generation,
        }
    }

    fn retained(subscriber: bool) -> Record {
        let mut record = super::super::tests::record(State::Closing);
        record.config.lifecycle_session_target = Some(LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            generation: 1,
        });
        record.config.lifecycle_intent = Some(if subscriber { "join" } else { "initial" }.into());
        record.lifecycle_target = Some(json!(target(
            if subscriber { "subscriber" } else { "source" },
            1
        )));
        record.lifecycle_source_target = Some(target("source", 1));
        record
    }

    fn shared_receipt(record: &Record) -> Value {
        json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
            "lifecycle_target":record.lifecycle_target,"lifecycle_retired":false,
            "lifecycle_consumer_retired":true,"lifecycle_resources_released":false,
            "lifecycle_shared_retained":true,"lifecycle_shared_source":target("source",1)})
    }

    #[test]
    fn source_and_subscriber_cleanup_preserve_explicit_shared_retention() {
        for subscriber in [false, true] {
            let mut record = retained(subscriber);
            assert!(confirm_terminal(
                &mut record,
                &shared_receipt(&retained(subscriber))
            ));
            finish_record(&mut record);
            assert_eq!(record.state, State::Closed);
            let status = record.response();
            assert_eq!(status["lifecycle_retired"], false);
            assert_eq!(status["lifecycle_consumer_retired"], true);
            assert_eq!(status["lifecycle_resources_released"], false);
            assert_eq!(status["lifecycle_shared_retained"], true);
            assert_eq!(
                status["lifecycle_shared_source"],
                json!(target("source", 1))
            );
            assert_eq!(status["lifecycle_cleanup_state"], "shared_retained");
        }
    }

    #[test]
    fn shared_proof_rejects_forged_source_and_departing_identity_and_incomplete_cleanup() {
        let record = retained(true);
        let receipt = shared_receipt(&record);
        let mut forged = Vec::new();
        for (field, value) in [
            ("host_epoch", json!("foreign")),
            ("session_id", json!("foreign")),
            ("stream_id", json!("foreign")),
            ("generation", json!(2)),
        ] {
            let mut payload = receipt.clone();
            payload["lifecycle_shared_source"][field] = value;
            forged.push(payload);
        }
        let mut departing = receipt.clone();
        departing["lifecycle_target"]["generation"] = json!(2);
        forged.push(departing);
        for (field, value) in [
            ("lifecycle_consumer_retired", json!(false)),
            ("lifecycle_resources_released", json!(true)),
            ("lifecycle_shared_retained", json!(false)),
            ("lifecycle_shared_source", Value::Null),
            ("lifecycle_retired", json!(true)),
            ("state", json!("stopping")),
            ("session", json!(99)),
            ("cleanup_error", json!("control_cleanup_pending")),
        ] {
            let mut payload = receipt.clone();
            payload[field] = value;
            forged.push(payload);
        }
        let mut partial = receipt;
        partial
            .as_object_mut()
            .unwrap()
            .remove("lifecycle_resources_released");
        forged.push(partial);
        for payload in forged {
            assert!(!clean_terminal(&record, &payload), "{payload}");
            let mut record = retained(true);
            assert!(!confirm_terminal(&mut record, &payload));
            assert!(!record.cleanup_confirmed);
            assert!(record.lifecycle_retained_source.is_none());
        }
    }

    #[test]
    fn shared_proof_needs_known_source_and_completed_consumer_preparation() {
        let mut record = retained(true);
        let payload = shared_receipt(&record);
        record.lifecycle_source_target = None;
        assert!(!clean_terminal(&record, &payload));
        record.lifecycle_source_target = Some(target("source", 1));
        record.lifecycle_prepare_outstanding = true;
        assert!(!clean_terminal(&record, &payload));
        record.lifecycle_prepare_outstanding = false;
        record.lifecycle_target = None;
        assert!(!clean_terminal(&record, &payload));
    }

    #[test]
    fn full_release_retains_legacy_receipt_compatibility_without_relabeling_shared_resources() {
        let mut record = retained(true);
        let legacy = json!({"ok":true,"session":23,"state":"stopped","cleanup_error":null,
            "lifecycle_target":record.lifecycle_target,"lifecycle_retired":true});
        assert!(confirm_terminal(&mut record, &legacy));
        assert_eq!(record.response()["lifecycle_resources_released"], true);
        assert_eq!(record.response()["lifecycle_shared_retained"], false);
        let mut explicit = legacy;
        explicit["lifecycle_consumer_retired"] = json!(true);
        explicit["lifecycle_resources_released"] = json!(true);
        explicit["lifecycle_shared_retained"] = json!(false);
        explicit["lifecycle_shared_source"] = Value::Null;
        assert!(clean_terminal(&record, &explicit));
        explicit["lifecycle_shared_retained"] = json!(true);
        assert!(!clean_terminal(&record, &explicit));
    }

    #[test]
    fn preparation_freezes_exact_join_source_before_any_start_effect() {
        let source = target("source", 3);
        let subscriber = json!(target("subscriber", 2));
        let mut record = retained(true);
        assert_eq!(
            preparation_source(&record, &subscriber, Some(&json!(source))),
            Some(source)
        );
        assert!(preparation_source(&record, &subscriber, None).is_none());
        assert!(preparation_source(&record, &subscriber, Some(&subscriber)).is_none());
        let mut foreign = json!(target("source", 3));
        foreign["session_id"] = json!("foreign");
        assert!(preparation_source(&record, &subscriber, Some(&foreign)).is_none());
        record.config.lifecycle_intent = Some("initial".into());
        assert_eq!(
            preparation_source(&record, &subscriber, None),
            Some(target("subscriber", 2))
        );
        assert!(
            preparation_source(&record, &subscriber, Some(&json!(target("source", 3)))).is_none()
        );
    }
}
