//! Pairing service adapter for bounded per-source retirement tombstones.
use super::Pipeline;
use amaterasu_module_sdk::effect_source_retirement::Request;
use serde_json::{Value, json};

pub(super) fn is_source_retirement(payload: &Value) -> bool {
    matches!(
        payload["action"].as_str(),
        Some(
            "fence_protocol_restart"
                | "fence_source"
                | "complete_source_retirement"
                | "lookup_source_retirement"
                | "lookup_current_source_retirement"
        )
    )
}

impl Pipeline {
    pub(super) fn apply_source_retirement(
        &mut self,
        caller: &str,
        payload: Value,
    ) -> Result<Value, &'static str> {
        let request: Request = serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        let policy = self
            .effect_authority
            .as_ref()
            .ok_or("authority_unavailable")?;
        let authorized = match &request {
            Request::FenceProtocolRestart { .. } => caller == policy.protocol_owner,
            Request::FenceSource { .. } => {
                caller == policy.protocol_owner || caller == policy.effect_owner
            }
            Request::CompleteSourceRetirement { .. } => caller == policy.effect_owner,
            Request::LookupSourceRetirement { .. } => {
                caller == policy.protocol_owner || caller == policy.effect_owner
            }
            Request::LookupCurrentSourceRetirement { .. } => {
                caller == policy.protocol_owner || caller == policy.effect_owner
            }
        };
        if !authorized {
            return Err("forbidden");
        }
        if !request.valid() {
            return Err("invalid_source_retirement");
        }
        let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
        if let Request::FenceProtocolRestart {
            epoch,
            operation_id,
        } = &request
        {
            gate.require_epoch(epoch)?;
            gate.fence();
            self.effect_bindings.clear();
            return Ok(json!({"ok":true,"epoch":epoch,"operation_id":operation_id,
                "state":"fenced","fence_generation":gate.fence_generation()}));
        }
        let (epoch, source, scope, operation_id, complete) = match request {
            Request::FenceProtocolRestart { .. } => unreachable!("handled above"),
            Request::FenceSource {
                epoch,
                source,
                scope,
                operation_id,
            } => {
                let complete = gate.fence_source(&epoch, &source, &scope, &operation_id)?;
                self.effect_bindings.remove(&source);
                (epoch, source, scope, operation_id, complete)
            }
            Request::CompleteSourceRetirement {
                epoch,
                source,
                scope,
                operation_id,
                retirement,
            } => {
                gate.complete_source_retirement(
                    &epoch,
                    &source,
                    &scope,
                    &operation_id,
                    retirement.as_ref(),
                )?;
                (epoch, source, scope, operation_id, true)
            }
            Request::LookupSourceRetirement {
                epoch,
                source,
                scope,
                operation_id,
            } => {
                let complete =
                    gate.lookup_source_retirement(&epoch, &source, &scope, &operation_id)?;
                (epoch, source, scope, operation_id, complete)
            }
            Request::LookupCurrentSourceRetirement {
                epoch,
                source,
                scope,
            } => {
                let (operation_id, complete) =
                    gate.lookup_current_source_retirement(&epoch, &source, &scope)?;
                (epoch, source, scope, operation_id.to_owned(), complete)
            }
        };
        Ok(
            json!({"ok":true,"epoch":epoch,"source":source,"scope":scope,
            "operation_id":operation_id,"state":if complete {"retired"} else {"fenced"}}),
        )
    }
}
