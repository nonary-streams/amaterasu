//! Direct selected-owner restart enumeration over the pairing ledger.
use super::Pipeline;
use amaterasu_module_sdk::effect_owner_recovery::Request;
use serde_json::{Value, json};

pub(super) fn is_owner_restart(payload: &Value) -> bool {
    matches!(
        payload["action"].as_str(),
        Some(
            "fence_effect_owner_restart"
                | "read_effect_owner_restart"
                | "confirm_effect_owner_restart"
        )
    )
}

impl Pipeline {
    pub(super) fn apply_owner_restart(
        &mut self,
        caller: &str,
        payload: Value,
    ) -> Result<Value, &'static str> {
        let policy = self
            .effect_authority
            .as_ref()
            .ok_or("authority_unavailable")?;
        if caller != policy.effect_owner {
            return Err("forbidden");
        }
        let request: Request = serde_json::from_value(payload).map_err(|_| "invalid_request")?;
        if !request.valid() {
            return Err("invalid_request");
        }
        let gate = self.effect_gate.as_mut().ok_or("authority_unavailable")?;
        match request {
            Request::FenceEffectOwnerRestart {
                version: 1,
                epoch,
                owner_epoch,
                operation_id,
            } => {
                let (fence_generation, count) =
                    gate.fence_owner_restart(&epoch, &owner_epoch, &operation_id)?;
                self.effect_bindings.clear();
                Ok(
                    json!({"ok":true,"version":1,"epoch":epoch,"owner_epoch":owner_epoch,
                    "operation_id":operation_id,"fence_generation":fence_generation,"count":count}),
                )
            }
            Request::ReadEffectOwnerRestart {
                version: 1,
                epoch,
                owner_epoch,
                operation_id,
                index,
            } => {
                let (record, count) =
                    gate.owner_restart_record(&epoch, &owner_epoch, &operation_id, index)?;
                Ok(
                    json!({"ok":true,"version":1,"epoch":epoch,"owner_epoch":owner_epoch,
                    "operation_id":operation_id,"index":index,"count":count,"record":record}),
                )
            }
            Request::ConfirmEffectOwnerRestart {
                version: 1,
                epoch,
                owner_epoch,
                operation_id,
                index,
                retirement,
            } => {
                gate.confirm_owner_restart_record(
                    &epoch,
                    &owner_epoch,
                    &operation_id,
                    index,
                    &retirement,
                )?;
                Ok(
                    json!({"ok":true,"version":1,"epoch":epoch,"owner_epoch":owner_epoch,
                    "operation_id":operation_id,"index":index}),
                )
            }
            _ => Err("invalid_request"),
        }
    }
}
