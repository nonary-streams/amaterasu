//! Read immutable, scoped settings admitted for an exact session or stream.
use super::*;
use amaterasu_extension_api::{FrozenProviderSettingsRequest, FrozenProviderSettingsResponse};

impl Coordinator {
    pub(super) fn read_frozen_provider_settings(&self, caller: &str, payload: Value) -> Value {
        self.read_settings_for_operation(caller, payload, "read_frozen_provider_settings")
    }

    pub(super) fn read_settings_for_operation(
        &self,
        caller: &str,
        payload: Value,
        operation: &str,
    ) -> Value {
        let Ok(request) = serde_json::from_value::<FrozenProviderSettingsRequest>(payload) else {
            return rejection("invalid_request");
        };
        if request.operation != operation
            || !self.settings.trusted_callers.contains(caller)
            || !self
                .settings
                .operation_grants
                .get(caller)
                .is_some_and(|operations| operations.contains(operation))
            || !self
                .settings
                .provider_settings_readers
                .get(caller)
                .is_some_and(|providers| providers.contains(&request.provider))
        {
            return rejection("permission_denied");
        }
        let Ok((epoch, session_id, stream_id, generation)) =
            operations::target_parts(&request.target)
        else {
            return rejection("session_target_required");
        };
        let shared = self.shared.borrow();
        let journal = &shared.value;
        if journal.host_epoch != epoch {
            return rejection("stale_host_epoch");
        }
        if retention::session_expired(journal, &request.target) {
            return rejection("session_expired");
        }
        let Some(session) = journal.sessions.get(session_id) else {
            return rejection("unknown_session");
        };
        if session.host_epoch != epoch
            || session.application_retired
            || !matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused)
        {
            return rejection("session_not_ready");
        }
        let (graph, snapshots) = if let Some(stream_id) = stream_id {
            let Some(stream) = session.streams.get(stream_id) else {
                return rejection("unknown_stream");
            };
            if stream.generation != generation {
                return rejection("stale_generation");
            }
            if !matches!(stream.phase, StreamPhase::Ready | StreamPhase::Active) {
                return rejection("stream_not_ready");
            }
            (&stream.provider_graph, &stream.provider_settings)
        } else {
            if session.generation != generation {
                return rejection("stale_generation");
            }
            (&session.provider_graph, &session.provider_settings)
        };
        if !graph
            .as_ref()
            .is_some_and(|graph| graph.order.contains(&request.provider))
        {
            return rejection("provider_not_selected");
        }
        let Some(snapshot) = snapshots.get(&request.provider) else {
            return rejection("settings_snapshot_missing");
        };
        // Old or malformed journals must not produce an unbounded response.
        if !snapshot.settings.is_object()
            || serde_json::to_vec(&snapshot.settings).map_or(true, |bytes| bytes.len() > 8192)
            || snapshot.revision.is_empty()
            || snapshot.revision.len() > 128
            || snapshot.revision.chars().any(char::is_control)
        {
            return rejection("invalid_settings_snapshot");
        }
        json!(FrozenProviderSettingsResponse {
            status: "succeeded".into(),
            target: request.target,
            provider: request.provider,
            revision: snapshot.revision.clone(),
            settings: snapshot.settings.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_granted_selected_settings_for_exact_live_stream() {
        let dir = std::env::temp_dir().join(format!(
            "amaterasu-frozen-settings-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&dir.join("journal.json")).unwrap();
        coordinator.settings.trusted_callers.insert("video".into());
        coordinator.settings.operation_grants.insert(
            "video".into(),
            ["read_frozen_provider_settings".into()].into(),
        );
        coordinator
            .settings
            .provider_settings_readers
            .insert("video".into(), ["effect".into()].into());
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let target = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":2});
        let session = json!({"host_epoch":epoch,"id":"session","generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":false,"provider_graph":{"order":["effect"],"dependencies":{"effect":[]}},"provider_settings":{"effect":{"revision":"session-revision","settings":{"enabled":false}}},"streams":{"stream":{"generation":2,"phase":"ready","candidate":null,"required":["capture"],"acknowledgements":[],"retirements":[],"provider_graph":{"order":["effect"],"dependencies":{"effect":[]}},"provider_settings":{"effect":{"revision":"revision-2","settings":{"enabled":true}},"other":{"revision":"private","settings":{"secret":true}}}}}});
        let mut journal = coordinator.shared.borrow().value.clone();
        journal
            .sessions
            .insert("session".into(), serde_json::from_value(session).unwrap());
        save(&coordinator.shared, journal).unwrap();
        let request = json!({"operation":"read_frozen_provider_settings","target":target,"provider":"effect"});
        let response = coordinator.handle("video", "video", request.clone());
        assert_eq!(response["status"], "succeeded");
        assert_eq!(response["target"], target);
        assert_eq!(response["revision"], "revision-2");
        assert_eq!(response["settings"], json!({"enabled":true}));
        let mut session_request = request.clone();
        session_request["target"] =
            json!({"kind":"session","host_epoch":epoch,"session_id":"session","generation":1});
        let session_response = coordinator.handle("video", "video", session_request.clone());
        assert_eq!(session_response["status"], "succeeded");
        assert_eq!(session_response["revision"], "session-revision");
        assert_eq!(session_response["settings"], json!({"enabled":false}));
        session_request["target"]["generation"] = json!(2);
        assert_eq!(
            coordinator.handle("video", "video", session_request)["reason"],
            "stale_generation"
        );
        assert_eq!(
            coordinator.handle("other", "other", request.clone())["reason"],
            "permission_denied"
        );
        let mut foreign = request.clone();
        foreign["provider"] = json!("other");
        assert_eq!(
            coordinator.handle("video", "video", foreign)["reason"],
            "permission_denied"
        );
        let mut stale_epoch = request.clone();
        stale_epoch["target"]["host_epoch"] = json!("different-epoch");
        assert_eq!(
            coordinator.handle("video", "video", stale_epoch)["reason"],
            "stale_host_epoch"
        );
        coordinator
            .settings
            .provider_settings_readers
            .get_mut("video")
            .unwrap()
            .insert("other".into());
        let mut not_selected = request.clone();
        not_selected["provider"] = json!("other");
        assert_eq!(
            coordinator.handle("video", "video", not_selected)["reason"],
            "provider_not_selected"
        );
        let mut stale = request.clone();
        stale["target"]["generation"] = json!(1);
        assert_eq!(
            coordinator.handle("video", "video", stale)["reason"],
            "stale_generation"
        );
        let mut journal = coordinator.shared.borrow().value.clone();
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .phase = StreamPhase::Retired;
        save(&coordinator.shared, journal).unwrap();
        assert_eq!(
            coordinator.handle("video", "video", request)["reason"],
            "stream_not_ready"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
