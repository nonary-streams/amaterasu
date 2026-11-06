//! Application ownership remains with a retained root. Joining clients own only
//! their encrypted transport, input binding and history activity.
use super::{
    Controller, LaunchRequest, MAX_RECORDS, PeerIdentity, PendingResume, Reply, StreamResponse,
    operation_id, private_error, random_nonzero_u32,
    record::{Record, State},
    resume_snapshot::ResumeSnapshot,
    valid_device_name,
};
use sha2::{Digest, Sha256};
use std::time::Instant;

const MAX_CONCURRENT_CLIENTS: usize = 2;
const MAX_RETIRED_STREAM_KEYS: usize = 32_768;

impl Record {
    pub(super) fn application_root(&self) -> u64 {
        self.application_session.unwrap_or(self.id)
    }

    pub(super) fn lifecycle_intent(&self) -> &'static str {
        if self.application_session.is_some() {
            "join"
        } else if self.resumed {
            "resume"
        } else {
            "initial"
        }
    }

    pub(super) fn owns_application_cleanup(&self) -> bool {
        self.application_session.is_none() && !self.transport_only_cleanup
    }
}

impl Controller {
    pub(super) fn application_status(&self, record: &Record) -> State {
        if record.application_session.is_some() || record.state != State::Paused {
            return record.state;
        }
        let members = || {
            self.records
                .values()
                .filter(|member| member.application_session == Some(record.id))
        };
        if members().any(|member| member.state == State::Ready) {
            State::Ready
        } else if members()
            .any(|member| matches!(member.state, State::Starting | State::AwaitingDelivery))
        {
            State::Starting
        } else {
            record.state
        }
    }
    pub(super) fn retired_stream_key(&self, request: &LaunchRequest) -> bool {
        self.retired_stream_keys
            .contains(&<[u8; 32]>::from(Sha256::digest(request.remote_input_key)))
    }

    pub(super) fn retire_stream_key(&mut self, key: [u8; 16]) -> bool {
        let fingerprint = Sha256::digest(key).into();
        if self.retired_stream_keys.contains(&fingerprint) {
            return true;
        }
        if self.retired_stream_keys.len() >= MAX_RETIRED_STREAM_KEYS {
            return false;
        }
        self.retired_stream_keys.insert(fingerprint);
        true
    }

    /// Retire only fully settled client metadata. Keep exact key fingerprints
    /// so eviction cannot turn a replay into a fresh transport admission.
    pub(super) fn prune_finished_transports(&mut self) {
        while self.records.len() >= MAX_RECORDS
            && self.retired_stream_keys.len() < MAX_RETIRED_STREAM_KEYS
        {
            let Some(id) = self
                .records
                .values()
                .find(|record| {
                    (record.application_session.is_some() || record.auxiliary.is_some())
                        && record.state.terminal()
                        && record.source_observations_emitted()
                        && record.uncertain.is_empty()
                        && !record.rtsp_requested
                        && !record.history_release_required
                        && self.job.as_ref().is_none_or(|job| job.record != record.id)
                })
                .map(|record| record.id)
            else {
                break;
            };
            let retired = self.records.remove(&id).expect("settled client record");
            self.retired_stream_keys
                .insert(Sha256::digest(retired.request.remote_input_key).into());
        }
    }

    pub(super) fn shared_application(&self, app_id: u32) -> Option<u64> {
        self.records
            .values()
            .find(|record| {
                record.application_session.is_none()
                    && record.lifecycle_required
                    && record.lifecycle_session_target.is_some()
                    && record.request.app_id == app_id
                    && matches!(record.state, State::Ready | State::Paused)
            })
            .map(|record| record.id)
    }

    pub(super) fn application_has_live_stream(&self, application: u64) -> bool {
        self.records.values().any(|record| {
            record.application_root() == application
                && (matches!(record.state, State::Starting | State::AwaitingDelivery)
                    || (record.state == State::Ready && record.rtsp_requested))
        })
    }

    pub(super) fn join_or_resume(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: PeerIdentity,
        response: StreamResponse,
        now: Instant,
    ) -> Option<Reply> {
        let application = self.shared_application(request.app_id)?;
        if self.application_has_live_stream(application) {
            self.join_stream(connection, request, peer, response, now)
        } else {
            self.resume_for_route(connection, request, &peer, response, now)
        }
    }

    pub(super) fn join_stream(
        &mut self,
        connection: u64,
        request: LaunchRequest,
        peer: PeerIdentity,
        response: StreamResponse,
        now: Instant,
    ) -> Option<Reply> {
        if self.package_activation_pending {
            return Some(private_error(503, "plugin activation is pending"));
        }
        if !peer.paired() {
            return Some(private_error(401, "authenticated paired TLS is required"));
        }
        self.prune_finished_transports();
        if self.retired_stream_key(&request) {
            return Some(private_error(409, "launch key was already used"));
        }
        if let Some(record) = self
            .records
            .values_mut()
            .find(|record| record.request.remote_input_key == request.remote_input_key)
        {
            if record.peer == peer && record.request == request && record.state == State::Ready {
                record.launch_connection = Some(connection);
                record.response_kind = response;
                return Some(super::success_reply(record));
            }
            return Some(private_error(
                409,
                "launch key conflicts with an existing request",
            ));
        }
        let Some(config) = &self.config else {
            return Some(private_error(503, "streaming is disabled"));
        };
        if !self.output_settings_ready {
            return Some(private_error(503, "stream settings are loading"));
        }
        if !config.profile.accepts(&request)
            || !matches!(request.audio_channels, 2 | 6 | 8)
            || (request.audio_channels == 6 && config.profile.audio_bitrate < 256_000)
            || (request.audio_channels == 8 && config.profile.audio_bitrate < 512_000)
            || request
                .device_name
                .as_deref()
                .is_some_and(|name| !valid_device_name(name))
        {
            return Some(private_error(400, "unsupported streaming request"));
        }
        let Some(application) = self.shared_application(request.app_id) else {
            return Some(private_error(404, "running application was not found"));
        };
        if self.pending_resume.is_some() {
            return Some(private_error(
                409,
                "stream admission is already in progress",
            ));
        }
        if self.records.len() >= MAX_RECORDS
            || self
                .records
                .values()
                .filter(|record| {
                    record.application_root() == application
                        && matches!(
                            record.state,
                            State::Starting
                                | State::AwaitingDelivery
                                | State::Ready
                                | State::Stopping
                                | State::CleanupFailed
                        )
                })
                .count()
                >= MAX_CONCURRENT_CLIENTS
        {
            return Some(private_error(503, "client stream capacity exhausted"));
        }
        if let Err(message) = self.codec_offer(config, &request) {
            return Some(private_error(503, message));
        }
        let id = match self.allocate_id() {
            Ok(id) => id,
            Err(code) => return Some(private_error(503, code)),
        };
        let connect_data = match random_nonzero_u32() {
            Ok(value) => value,
            Err(code) => return Some(private_error(503, code)),
        };
        let retained = &self.records[&application];
        let Some(proton) = retained.global_proton_dualsense_compatibility else {
            return Some(private_error(503, "stream settings are loading"));
        };
        let candidate = match ResumeSnapshot::build(
            self,
            retained,
            request.clone(),
            peer.clone(),
            connect_data,
            proton,
        ) {
            Ok(candidate) if !candidate.legacy_display_conflict() => candidate,
            _ => {
                return Some(private_error(
                    503,
                    "shared display settings are unavailable",
                ));
            }
        };
        let operation = operation_id(&self.nonce, id, "rtsp");
        let mut stream = candidate.into_join(retained, id, operation, connection, now);
        stream.response_kind = response;
        self.records.insert(id, stream);
        self.pending_resume = Some(PendingResume {
            connection,
            record: id,
            request,
            peer,
            retained: true,
            response_kind: response,
        });
        None
    }

    /// An application end is group-wide. Per-client retirement never selects
    /// this path, and failed sibling cleanup keeps the application fenced.
    pub(super) fn synchronize_application_cleanup(&mut self, now: Instant) {
        let ending: Vec<_> = self
            .records
            .values()
            .filter(|record| {
                record.application_session.is_none()
                    && record.state == State::Stopping
                    && record.owns_application_cleanup()
            })
            .map(|record| record.id)
            .collect();
        for application in ending {
            for record in self.records.values_mut().filter(|record| {
                record.application_session == Some(application) && !record.state.terminal()
            }) {
                if !matches!(record.state, State::Stopping | State::CleanupFailed) {
                    record.request_transport_cleanup(Some("application_ended"), now);
                }
            }
            let waiting = self.records.values().any(|record| {
                record.application_session == Some(application) && !record.state.terminal()
            });
            if waiting
                && self
                    .records
                    .get(&application)
                    .is_some_and(|root| now >= root.deadline)
            {
                let root = self
                    .records
                    .get_mut(&application)
                    .expect("retained application");
                root.fail_cleanup("shared_stream_cleanup_unconfirmed");
                if let Some(connection) = root
                    .cancel_connection
                    .take()
                    .or(root.launch_connection.take())
                {
                    self.management_replies.push((
                        connection,
                        private_error(503, "shared stream cleanup is unconfirmed"),
                    ));
                }
            }
        }
        let complete: Vec<_> = self
            .records
            .values()
            .filter(|record| {
                record.state == State::Stopping
                    && self.job.as_ref().is_none_or(|job| job.record != record.id)
            })
            .map(|record| record.id)
            .collect();
        let mut replies = Vec::new();
        for record in complete {
            self.finish_or_retain(record, now, &mut replies);
        }
        self.management_replies.extend(replies);
    }

    pub(super) fn application_cleanup_waits_for_clients(&self, record: &Record) -> bool {
        record.owns_application_cleanup()
            && record.state == State::Stopping
            && self.records.values().any(|other| {
                other.application_session == Some(record.id) && !other.state.terminal()
            })
    }

    pub(super) fn end_application_group(&mut self, member: u64, now: Instant) -> u64 {
        let application = self.records[&member].application_root();
        if self.pending_resume.as_ref().is_some_and(|pending| {
            self.records
                .get(&pending.record)
                .is_some_and(|record| record.application_root() == application)
        }) {
            self.pending_resume = None;
        }
        let cancel_job = self.job.as_ref().is_some_and(|job| {
            self.records
                .get(&job.record)
                .is_some_and(|record| record.application_root() == application)
        });
        if cancel_job && let Some(job) = self.job.as_mut() {
            job.call.cancel();
        }
        for record in self
            .records
            .values_mut()
            .filter(|record| record.application_root() == application && !record.state.terminal())
        {
            record.transport_only_cleanup = record.application_session.is_some();
            if record.state == State::CleanupFailed {
                if let Some(pending) = record.retry_cleanup(now) {
                    record.uncertain.push_front(pending);
                }
            } else if record.state != State::Stopping {
                record.request_cleanup(None, now);
            }
        }
        self.synchronize_application_cleanup(now);
        application
    }

    pub(super) fn shared_cancel_member(&self, peer: &PeerIdentity) -> Option<u64> {
        self.records
            .values()
            .find(|record| {
                record.peer.certificate == peer.certificate
                    && !record.state.terminal()
                    && (record.application_session.is_some()
                        || self.records.values().any(|other| {
                            other.application_session == Some(record.id) && !other.state.terminal()
                        }))
            })
            .map(|record| record.id)
    }

    pub(super) fn cancel_application_group(
        &mut self,
        member: u64,
        connection: u64,
        now: Instant,
    ) -> Option<Reply> {
        let application = self.records[&member].application_root();
        let root = &self.records[&application];
        if root.state == State::Stopping || root.cancel_connection.is_some() {
            return Some(private_error(
                409,
                "application cleanup is already in progress",
            ));
        }
        self.records
            .get_mut(&application)
            .expect("retained application")
            .cancel_connection = Some(connection);
        self.end_application_group(member, now);
        None
    }
}
