//! Read-only protocol responses backed by authenticated closed-stream evidence.
//!
//! These adapters grant no effect authority. An absent, ambiguous or invalid
//! certificate returns `None`, allowing the caller to reject the query.
use super::{Journal, closed_stream::ClosedStreamBook};
use crate::adapter::LifecycleSettings;
use amaterasu_extension_api::{
    LifecycleTarget, ResourceControlResponse, ResourceControlSettlementRequest,
    ResourceFinalizeRequest,
};
use serde_json::{Value, json};

/// A checkpoint and any original result for the same stream are ambiguous,
/// including when the residual result belongs to the other query kind.
pub(super) fn has_original_result(journal: &Journal, target: &LifecycleTarget) -> bool {
    journal
        .finalize_results
        .records()
        .any(|record| &record.request.target == target)
        || journal
            .control_settlements
            .records()
            .any(|record| &record.settlement.request.target == target)
}

/// Project an exact, authenticated historical finalization into the existing
/// `verify_finalize_result` response. This does not validate an effect permit.
#[allow(dead_code, reason = "coordinator query fallback integration pending")]
pub(super) fn finalize_result(
    book: &ClosedStreamBook,
    journal: &Journal,
    settings: &LifecycleSettings,
    caller: &str,
    request: &ResourceFinalizeRequest,
    response: &ResourceControlResponse,
) -> Option<Value> {
    let proof = book.verify_finalize(journal, settings, caller, request, response)?;
    Some(json!({
        "status": "valid",
        "target": request.target,
        "result_id": request.permit_id,
        "resource_generation": proof.resource_generation,
    }))
}

/// Project an exact, authenticated historical settlement into the existing
/// `verify_control_settlement` response. This does not authorize a control call.
#[allow(dead_code, reason = "coordinator query fallback integration pending")]
pub(super) fn control_settlement(
    book: &ClosedStreamBook,
    journal: &Journal,
    settings: &LifecycleSettings,
    caller: &str,
    request: &ResourceControlSettlementRequest,
) -> Option<Value> {
    let proof = book.verify_settlement(journal, settings, caller, request)?;
    Some(json!({
        "status": "valid",
        "session_target": proof.session_target,
        "target": request.request.target,
        "output": request.output,
        "root_output": proof.root_output,
        "resource_generation": proof.resource_generation,
        "consumers_retired": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_certificate_never_produces_an_authorized_response() {
        let journal: Journal = serde_json::from_value(json!({
            "version": 3, "host_epoch": "epoch", "receipts": {}, "sessions": {}
        }))
        .unwrap();
        let settings = LifecycleSettings::default();
        let book = ClosedStreamBook::default();
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let schema = json!({"id":"display/1","major":1,"minor":0,"digest":"a".repeat(64)});
        let resource = json!({"provider":"owner","resource_id":"screen","incarnation":"inc","revision":1,"execution_domain":"desktop","mutation_domain":null,"mutation_domains":[]});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":resource,"domain_observations":[],"data":{}});
        let finalize: ResourceFinalizeRequest = serde_json::from_value(json!({"version":2,"transition_id":"transition","operation_id":"finalize","provider_sequence":1,"target":target,"output":output,"control_results":[],"admission_id":"admission","permit_id":"permit","remaining_ms":1000})).unwrap();
        let result: ResourceControlResponse =
            serde_json::from_value(json!({"status":"applied","output":output,"candidate":null}))
                .unwrap();
        assert!(finalize_result(&book, &journal, &settings, "owner", &finalize, &result).is_none());
        let control = json!({"version":2,"transition_id":"transition","operation_id":"control","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"owner","owner_service":"owner/2","target":target,"resource":resource,"method":{"service":"owner/2","method":"resource_control","interface_major":2,"interface_minor":0,"schema_revision":1},"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":[]},"admission_id":"admission","permit_id":"permit","remaining_ms":1000});
        let settlement: ResourceControlSettlementRequest = serde_json::from_value(
            json!({"request":control,"output":output,"settlement_id":"settlement"}),
        )
        .unwrap();
        assert!(control_settlement(&book, &journal, &settings, "owner", &settlement).is_none());
    }
}
