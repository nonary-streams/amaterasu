//! Reclaim a clipboard stream fence only after selected lifecycle confirms the
//! exact stream can no longer issue reservations.
use super::*;
use amaterasu_module_sdk::clipboard_authority::{ClosedStream, LifecycleRequest};
use amaterasu_module_sdk::effect_authority::ClipboardStream;

const LIFECYCLE_SERVICE: &str = "amaterasu.session-lifecycle/1";

impl Pipeline {
    pub(super) fn start_clipboard_reclaim(
        &mut self,
        request_id: u64,
        caller: &str,
        initiator: &str,
        epoch: String,
        stream: ClipboardStream,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        if !clipboard_authority_available()
            || caller != initiator
            || self
                .effect_authority
                .as_ref()
                .is_none_or(|policy| policy.protocol_owner != caller)
        {
            return complete(send, request_id, error("forbidden"));
        }
        if !stream.valid()
            || self
                .effect_gate
                .as_ref()
                .is_none_or(|gate| gate.require_epoch(&epoch).is_err())
        {
            return complete(send, request_id, error("invalid_source_or_epoch"));
        }
        if self.clipboard_reclaim.is_some() {
            return complete(send, request_id, error("reclaim_pending"));
        }
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "clipboard reclaim entropy unavailable")?;
        let Response::Accepted { id } = send(Command::Request {
            service: LIFECYCLE_SERVICE.into(),
            payload: serde_json::to_value(LifecycleRequest::VerifyClipboardStreamClosed {
                source: stream.clone(),
            })
            .map_err(|_| "clipboard reclaim serialization failed")?,
            timeout_ms: 30_000,
            operation_id: format!("clipboard.reclaim.{:032x}", u128::from_le_bytes(nonce)),
            parent: Some(request_id),
        })?
        else {
            return complete(send, request_id, error("lifecycle_unavailable"));
        };
        self.clipboard_reclaim = Some(ClipboardReclaim {
            request_id,
            lifecycle_request: id,
            epoch,
            stream,
        });
        Ok(())
    }

    pub(super) fn progress_clipboard_reclaim(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let Some(pending) = self.clipboard_reclaim.take() else {
            return Ok(());
        };
        match send(Command::CheckRequest {
            request: pending.request_id,
        }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. }) if code == "request_ended" || code == "not_found" => {
                let _ = send(Command::Cancel {
                    request: pending.lifecycle_request,
                });
                let _ = send(Command::ReleaseResult {
                    request: pending.lifecycle_request,
                });
                return Ok(());
            }
            _ => {
                self.clipboard_reclaim = Some(pending);
                return Ok(());
            }
        }
        let result = match send(Command::TakeResult {
            request: pending.lifecycle_request,
        }) {
            Ok(Response::Pending) | Err(_) => {
                self.clipboard_reclaim = Some(pending);
                return Ok(());
            }
            Ok(result) => result,
        };
        match send(Command::ReleaseResult {
            request: pending.lifecycle_request,
        }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. }) if code == "not_found" => {}
            _ => {
                self.clipboard_reclaim = Some(pending);
                return Ok(());
            }
        }
        let proof = match result {
            Response::Result { payload } => serde_json::from_value::<ClosedStream>(payload).ok(),
            _ => None,
        };
        let Some(proof) = proof.filter(|proof| proof.matches(&pending.stream)) else {
            return complete(send, pending.request_id, error("stream_not_closed"));
        };
        if !matches!(
            send(Command::CheckRequest {
                request: pending.request_id
            })?,
            Response::Ok
        ) {
            return Ok(());
        }
        let result = self
            .effect_gate
            .as_mut()
            .ok_or("authority_unavailable")?
            .reclaim_clipboard_stream(&pending.epoch, &pending.stream, proof.highwater);
        complete(
            send,
            pending.request_id,
            match result {
                Ok(()) => {
                    json!({"ok":true,"epoch":pending.epoch,"stream":pending.stream,"state":"reclaimed"})
                }
                Err(code) => error(code),
            },
        )
    }
}
