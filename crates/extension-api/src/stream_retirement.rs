//! Retirement of one consumer is distinct from release of a shared source.
use crate::LifecycleTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamRetirementProof {
    pub target: LifecycleTarget,
    pub consumer_retired: bool,
    pub resources_released: bool,
    pub shared_retained: bool,
    pub shared_source: Option<LifecycleTarget>,
}

pub fn validate_stream_retirement_proof(
    proof: &StreamRetirementProof,
    expected_target: &LifecycleTarget,
    expected_shared_source: Option<&LifecycleTarget>,
) -> Result<(), String> {
    let LifecycleTarget::Stream {
        host_epoch,
        session_id,
        generation: 1..,
        ..
    } = expected_target
    else {
        return Err("stream retirement requires a stream target".into());
    };
    if &proof.target != expected_target || !proof.consumer_retired {
        return Err("stream consumer retirement is unconfirmed".into());
    }
    if proof.resources_released {
        if proof.shared_retained || proof.shared_source.is_some() {
            return Err("resource release contradicts shared retention".into());
        }
        return Ok(());
    }
    let Some(
        source @ LifecycleTarget::Stream {
            host_epoch: source_epoch,
            session_id: source_session,
            generation: 1..,
            ..
        },
    ) = &proof.shared_source
    else {
        return Err("shared source retirement is unconfirmed".into());
    };
    if !proof.shared_retained
        || host_epoch != source_epoch
        || session_id != source_session
        || expected_shared_source.map_or(source != expected_target, |expected| expected != source)
    {
        return Err("shared source identity changed".into());
    }
    Ok(())
}

/// Decode a completed coordinator receipt. Legacy succeeded receipts retain
/// their full-release meaning; a partial new proof never takes that fallback.
pub fn stream_retirement_proof(
    receipt: &Value,
    expected_target: &LifecycleTarget,
    expected_shared_source: Option<&LifecycleTarget>,
) -> Result<StreamRetirementProof, String> {
    if receipt["status"] != "succeeded"
        || receipt.get("target")
            != Some(&serde_json::to_value(expected_target).map_err(|e| e.to_string())?)
    {
        return Err("stream retirement receipt is not complete for this target".into());
    }
    let explicit = [
        "consumer_retired",
        "resources_released",
        "shared_retained",
        "shared_source",
    ]
    .iter()
    .any(|key| receipt.get(*key).is_some());
    let proof = if explicit {
        StreamRetirementProof {
            target: expected_target.clone(),
            consumer_retired: receipt["consumer_retired"]
                .as_bool()
                .ok_or("missing consumer retirement")?,
            resources_released: receipt["resources_released"]
                .as_bool()
                .ok_or("missing resource retirement")?,
            shared_retained: receipt["shared_retained"]
                .as_bool()
                .ok_or("missing shared retention")?,
            shared_source: receipt
                .get("shared_source")
                .filter(|value| !value.is_null())
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "invalid shared source")?,
        }
    } else {
        StreamRetirementProof {
            target: expected_target.clone(),
            consumer_retired: true,
            resources_released: true,
            shared_retained: false,
            shared_source: None,
        }
    };
    validate_stream_retirement_proof(&proof, expected_target, expected_shared_source)?;
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn target(id: &str, generation: u64) -> LifecycleTarget {
        LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: id.into(),
            generation,
        }
    }
    #[test]
    fn accepts_legacy_release_and_exact_shared_subscription() {
        let source = target("source", 3);
        let subscriber = target("subscriber", 2);
        let old = json!({"status":"succeeded","target":subscriber});
        assert!(
            stream_retirement_proof(&old, &subscriber, Some(&source))
                .unwrap()
                .resources_released
        );
        let shared = json!({"status":"succeeded","target":subscriber,"consumer_retired":true,"resources_released":false,"shared_retained":true,"shared_source":source});
        assert!(
            stream_retirement_proof(&shared, &subscriber, Some(&source))
                .unwrap()
                .shared_retained
        );
        assert!(stream_retirement_proof(&shared, &subscriber, None).is_err());
        assert!(stream_retirement_proof(&shared, &subscriber, Some(&target("source", 4))).is_err());
        assert!(stream_retirement_proof(&json!({"status":"succeeded","target":source,"consumer_retired":true,"resources_released":false,"shared_retained":true,"shared_source":source}),&source,None).is_ok());
    }
    #[test]
    fn rejects_partial_contradictory_foreign_and_unretired_proofs() {
        let target = target("source", 1);
        let valid = json!({"status":"succeeded","target":target,"consumer_retired":true,"resources_released":false,"shared_retained":true,"shared_source":target});
        for (path, value) in [
            ("consumer_retired", json!(false)),
            ("resources_released", json!(true)),
            ("shared_retained", json!(false)),
            ("shared_source", Value::Null),
            ("status", json!("pending")),
        ] {
            let mut receipt = valid.clone();
            receipt[path] = value;
            assert!(
                stream_retirement_proof(&receipt, &target, None).is_err(),
                "{receipt}"
            );
        }
        let mut partial = valid.clone();
        partial
            .as_object_mut()
            .unwrap()
            .remove("resources_released");
        assert!(stream_retirement_proof(&partial, &target, None).is_err());
        let mut foreign = valid;
        foreign["shared_source"]["session_id"] = json!("other");
        assert!(stream_retirement_proof(&foreign, &target, None).is_err());
    }
}
