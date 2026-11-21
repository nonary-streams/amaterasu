//! The native display can outlive its transport when the application is paused.
//! Its exact provider reference remains in the stream cleanup journal until
//! released. Every new transport still creates and retires its own capture.
use super::*;
use amaterasu_extension_api::LifecycleTarget;

#[derive(Clone, Copy)]
pub(super) enum Hold {
    Paused { deadline: u64 },
    Releasing,
}

impl Record {
    pub(super) fn retains_paused_display(&self) -> bool {
        matches!(self.paused_display, Some(Hold::Paused { .. }))
    }

    pub(super) fn retain_disconnected_display(&mut self, now: u64) -> bool {
        let Some(timeout) = self.config.paused_display_timeout_secs else {
            return false;
        };
        if self.paused_display.is_some()
            || !self.control_authenticated
            || (self.authenticated_disconnect_reason.is_none()
                && (self.outcome != State::Stopped || self.error.is_some()))
            || !self.shared_capture_attached
            || !self.virtual_display_attempted
            || self.display_attempted
            || self.display_lease.is_some()
            || self.cleanup_error.is_some()
            || self
                .shared_capture
                .as_ref()
                .is_none_or(|source| source.output.is_none())
        {
            return false;
        }
        let deadline = if timeout == 0 {
            u64::MAX
        } else {
            now.saturating_add(u64::from(timeout) * 1000)
        };
        self.paused_display = Some(Hold::Paused { deadline });
        self.virtual_display_state = "retained_for_application".into();
        true
    }
}

impl Pipeline {
    /// A compatible successor takes a distinct native reference on the same
    /// display. The old reference survives until that successor is active, so
    /// startup failure cannot unplug the display from the still-paused game.
    pub(super) fn select_paused_display(&self, record: &mut Record) -> Option<u64> {
        let application = record.config.lifecycle_session_target.as_ref()?;
        let (previous, retained) = self.records.values().find_map(|previous| {
            let retained = previous.shared_capture.as_ref()?;
            (previous.state.terminal()
                && previous.retains_paused_display()
                && previous.owner == record.owner
                && previous.config.application_owner == record.config.application_owner
                && *application == retained.session)
                .then_some((previous, retained))
        })?;
        if record.paused_display_fallback_reason.is_none()
            && record.config.paused_display_timeout_secs.is_some()
            && let Some(source) = record.shared_capture.as_mut()
            && source.same_display_mode(retained)
        {
            source.identity.clone_from(&retained.identity);
            source.output.clone_from(&retained.output);
            None
        } else {
            // This resume needs an independent display. Retire the previous
            // hold through ordinary cleanup before changing the topology.
            Some(previous.id)
        }
    }

    /// Only retry if the provider refused the reference before any effect.
    /// Unknown results still go through exact reference cleanup.
    pub(super) fn fallback_from_paused_display(record: &mut Record, payload: &Value) -> bool {
        let Some(source) = record.shared_capture.as_mut() else {
            return false;
        };
        let Some(independent_identity) = source.reference_id.strip_prefix("shared-source-") else {
            return false;
        };
        if source.identity == independent_identity {
            return false;
        }
        let code = payload["code"]
            .as_str()
            .or_else(|| payload["error"]["code"].as_str());
        let reason = match code {
            Some("identity_conflict") => "identity_conflict",
            Some("reference_conflict") => "reference_conflict",
            Some("cleanup_pending") => "cleanup_pending",
            _ => return false,
        };
        source.identity = independent_identity.to_owned();
        source.output = None;
        record.shared_capture_attached = false;
        record.virtual_display_attempted = false;
        record.paused_display_fallback_reason = Some(reason);
        true
    }

    pub(super) fn report_paused_display_fallback(record: &mut Record) {
        use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue};
        if let (Some(reason), Some(operation)) = (
            record.paused_display_fallback_reason.take(),
            record.config.rtsp_operation_id.as_deref(),
        ) {
            record.issues.push(
                StreamIssue::demoted(
                    operation,
                    LifecycleLayer::StreamSession,
                    "resume_display",
                    "retained_display_reuse_refused",
                    "allocate_compatible_display",
                )
                .because(reason),
            );
        }
    }

    pub(super) fn paused_display_due(&self, now: u64) -> Option<u64> {
        self.records.values().find_map(|record| {
            let Some(Hold::Paused { deadline }) = record.paused_display else {
                return None;
            };
            if !record.state.terminal() {
                return None;
            }
            let successor_active = record.shared_capture.as_ref().is_some_and(|source| {
                self.records.values().any(|candidate| {
                    candidate.state == State::Active
                        && candidate.shared_capture_attached
                        && candidate
                            .shared_capture
                            .as_ref()
                            .is_some_and(|replacement| {
                                source.session == replacement.session
                                    && source.identity == replacement.identity
                                    && source.reference_id != replacement.reference_id
                            })
                })
            });
            (now >= deadline || successor_active || self.shutdown_admission_closed())
                .then_some(record.id)
        })
    }

    pub(super) fn begin_paused_display_release(&mut self, id: u64, now: u64) {
        let record = self
            .records
            .get_mut(&id)
            .expect("owned application display");
        let previous_outcome = record.outcome;
        record.paused_display = Some(Hold::Releasing);
        self.job = Some(actions::stop_job(
            record,
            None,
            now,
            self.settings.drain_timeout_ms,
            false,
        ));
        record.outcome = previous_outcome;
    }

    pub(super) fn release_paused_display(
        &mut self,
        request: u64,
        caller: &str,
        application: LifecycleTarget,
        now: u64,
        send: &mut Send<'_>,
    ) {
        if !matches!(
            application,
            LifecycleTarget::Session {
                generation: 1..,
                ..
            }
        ) {
            rpc::complete(send, request, error("invalid_application_display_owner"));
            return;
        }
        let held = self.records.values().find(|record| {
            record.paused_display.is_some()
                && record.config.lifecycle_session_target.as_ref() == Some(&application)
        });
        if let Some(record) = held {
            if record.config.application_owner.as_deref() != Some(caller) {
                rpc::complete(send, request, error("permission_denied"));
                return;
            }
            self.begin_paused_display_release(record.id, now);
            rpc::complete(
                send,
                request,
                json!({"ok":true,"application":application,"released":false}),
            );
        } else {
            rpc::complete(
                send,
                request,
                json!({"ok":true,"application":application,"released":true}),
            );
        }
    }
}
