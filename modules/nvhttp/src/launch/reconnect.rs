//! Reconnects replace their own transport after retirement, rather than borrowing
//! a capture producer that the previous transport is already releasing.
use super::*;

fn same_client(left: &PeerIdentity, right: &PeerIdentity) -> bool {
    left.certificate == right.certificate && left.client_id == right.client_id
}

impl Controller {
    pub(super) fn reconnect_application(&self, app_id: u32) -> Option<u64> {
        self.records
            .values()
            .find(|record| {
                record.application_session.is_none()
                    && record.auxiliary.is_none()
                    && record.lifecycle_required
                    && record.lifecycle_session_target.is_some()
                    && record.request.app_id == app_id
                    && (matches!(record.state, State::Ready | State::Paused)
                        || (record.state.closing() && record.retains_application()))
            })
            .map(|record| record.id)
    }

    pub(super) fn reconnect_ready(&self, application: u64, peer: &PeerIdentity) -> bool {
        let Some(root) = self.records.get(&application) else {
            return true;
        };
        let waiting = self.reconnect_waiting(application, peer);
        !waiting && (root.state == State::Paused || self.application_has_live_stream(application))
    }

    pub(super) fn defer_reconnect_if_needed(
        &mut self,
        application: u64,
        connection: u64,
        request: &LaunchRequest,
        peer: &PeerIdentity,
        response: StreamResponse,
        now: Instant,
    ) -> Result<bool, Reply> {
        let waiting = self.reconnect_waiting(application, peer);
        if !waiting {
            return Ok(false);
        }
        // Validate before any effect on a live predecessor. Admission will check
        // trust, key freshness and policy again after the bounded wait.
        self.validate_reconnect(application, request, peer)?;
        if self.deferred_launch.is_some() || self.pending_resume.is_some() {
            return Err(private_error(
                409,
                "stream admission is already in progress",
            ));
        }
        if let Some(reply) =
            self.defer_launch(connection, request.clone(), peer.clone(), response, now)
        {
            return Err(reply);
        }
        self.deferred_launch
            .as_mut()
            .expect("held reconnect")
            .resume_application = Some(application);
        for record in self.records.values_mut().filter(|record| {
            record.application_root() == application
                && same_client(&record.peer, peer)
                && record.rtsp_requested
                && matches!(record.state, State::Ready)
        }) {
            record.request_transport_cleanup(None, now);
        }
        Ok(true)
    }
    fn reconnect_waiting(&self, application: u64, peer: &PeerIdentity) -> bool {
        self.records.values().any(|record| {
            record.application_root() == application
                && !record.state.terminal()
                && (record.state.closing()
                    || record.rtsp_stream_ending
                    || record.rtsp_closing_deadline.is_some()
                    || (same_client(&record.peer, peer)
                        && record.rtsp_requested
                        && record.state != State::Paused))
        })
    }

    fn validate_reconnect(
        &self,
        application: u64,
        request: &LaunchRequest,
        peer: &PeerIdentity,
    ) -> Result<(), Reply> {
        self.validate_stream_request(request, peer, None)?;
        if self.records.values().any(|record| {
            record.request.remote_input_key == request.remote_input_key
                && record.state != State::Paused
                && !record.state.terminal()
        }) {
            return Err(private_error(
                409,
                "launch key conflicts with an existing request",
            ));
        }
        let root = &self.records[&application];
        let private = shared_session::requested_resolution_only(&root.peer);
        let requested_private = shared_session::requested_resolution_only(peer);
        if private || requested_private {
            let mode = peer.client_display_mode.unwrap_or(PhysicalDisplayMode {
                width: request.width,
                height: request.height,
                refresh_millihz: request.fps.saturating_mul(1000),
            });
            if !private
                || !requested_private
                || !same_client(&root.peer, peer)
                || root.physical_display_mode != mode
            {
                return Err(private_error(
                    409,
                    "the retained private catalog requires its owner and unchanged output mode",
                ));
            }
        }
        let config = self.config.as_ref().expect("validated configuration");
        self.codec_offer(config, request)
            .map_err(|message| private_error(503, message))?;
        Ok(())
    }
}
