//! Exact, read-only recovery of a coordinator admission before its stream target
//! reached the local consumer journal. Resolution never authorizes an effect.
use crate::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPreparationOperation {
    ResolveStreamPreparation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamPreparationLookup {
    pub operation: StreamPreparationOperation,
    pub session: LifecycleTarget,
    pub preparation_operation_id: String,
    pub preparation_caller: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPreparationResolutionStatus {
    Succeeded,
    Pending,
    Missing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamPreparationResolution {
    pub status: StreamPreparationResolutionStatus,
    pub session: LifecycleTarget,
    pub preparation_operation_id: String,
    pub preparation_caller: String,
    pub target: Option<LifecycleTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation_status: Option<String>,
}
pub fn validate_stream_preparation_lookup(request: &StreamPreparationLookup) -> Result<(), String> {
    let LifecycleTarget::Session {
        host_epoch,
        session_id,
        generation: 1..,
    } = &request.session
    else {
        return Err("session target required".into());
    };
    if ![host_epoch, session_id, &request.preparation_operation_id]
        .into_iter()
        .all(|id| valid_opaque_id(id))
        || !valid_identifier(&request.preparation_caller)
    {
        return Err("invalid preparation identity".into());
    }
    Ok(())
}
pub fn validate_stream_preparation_resolution(
    request: &StreamPreparationLookup,
    response: &StreamPreparationResolution,
) -> Result<(), String> {
    validate_stream_preparation_lookup(request)?;
    if response.session != request.session
        || response.preparation_operation_id != request.preparation_operation_id
        || response.preparation_caller != request.preparation_caller
    {
        return Err("preparation resolution identity mismatch".into());
    }
    match (&response.status, &response.target) {
        (
            StreamPreparationResolutionStatus::Succeeded,
            Some(LifecycleTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                generation: 1..,
            }),
        ) => {
            let LifecycleTarget::Session {
                host_epoch: epoch,
                session_id: session,
                ..
            } = &request.session
            else {
                unreachable!()
            };
            if epoch != host_epoch
                || session != session_id
                || !valid_opaque_id(stream_id)
                || !matches!(
                    response.preparation_status.as_deref(),
                    Some(
                        "succeeded"
                            | "pending"
                            | "recovery_required"
                            | "failed"
                            | "cancelled"
                            | "rejected"
                    )
                )
            {
                return Err("invalid resolved preparation".into());
            }
        }
        (
            StreamPreparationResolutionStatus::Missing | StreamPreparationResolutionStatus::Pending,
            None,
        ) if response.preparation_status.is_none() => {}
        _ => return Err("invalid preparation resolution shape".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unchanged_resolution_consumer_accepts_replacement_and_fences_foreign_targets() {
        let request:StreamPreparationLookup=serde_json::from_value(json!({"operation":"resolve_stream_preparation",
            "session":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},
            "preparation_operation_id":"rtsp-ticket-lifecycle-prepare","preparation_caller":"transport"})).unwrap();
        let consume = |payload: Value| {
            let response: StreamPreparationResolution = serde_json::from_value(payload).unwrap();
            validate_stream_preparation_resolution(&request, &response)
        };
        for stream in ["first-implementation", "replacement-implementation"] {
            let mut response = json!({"status":"succeeded","session":request.session,
                "preparation_operation_id":request.preparation_operation_id,"preparation_caller":request.preparation_caller,
                "preparation_status":"succeeded","target":{"kind":"stream","host_epoch":"epoch",
                "session_id":"session","stream_id":stream,"generation":1}});
            assert!(consume(response.clone()).is_ok());
            response["target"]["session_id"] = json!("foreign");
            assert!(consume(response).is_err());
        }
    }
}
