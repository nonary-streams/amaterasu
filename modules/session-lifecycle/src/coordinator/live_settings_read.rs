//! Scoped live settings reads retain the admitted stream configuration.
use super::*;
use amaterasu_extension_api::LiveProviderSettingsRequest;
use std::time::Duration;

const MAX_LIVE_JOBS: usize = 32;
const LIVE_JOB_TTL: Duration = Duration::from_secs(30);

pub(super) struct LiveSettingsJob {
    caller: String,
    request: Value,
    started: Instant,
    job: settings_snapshot::SettingsSnapshotJob,
    result: Option<Result<(settings_snapshot::FrozenSettings, Vec<String>), ()>>,
}

impl Coordinator {
    pub(super) fn read_live_provider_settings(&mut self, caller: &str, payload: Value) -> Value {
        let validated = self.read_settings_for_operation(
            caller,
            payload.clone(),
            "read_live_provider_settings",
        );
        if validated["status"] != "succeeded" {
            return validated;
        }
        let Ok(request) = serde_json::from_value::<LiveProviderSettingsRequest>(payload.clone())
        else {
            return rejection("invalid_request");
        };
        let Ok((_, session_id, Some(stream_id), _)) = operations::target_parts(&request.target)
        else {
            return rejection("stream_target_required");
        };
        let (scopes, admitted_names) = {
            let shared = self.shared.borrow();
            let Some(stream) = shared
                .value
                .sessions
                .get(session_id)
                .and_then(|s| s.streams.get(stream_id))
            else {
                return rejection("unknown_stream");
            };
            let Some(scopes) = stream.snapshot_scopes.clone() else {
                return rejection("settings_scope_missing");
            };
            if !scopes.valid_for_session(session_id) {
                return rejection("settings_scope_missing");
            }
            let Some(names) = stream
                .admitted_live_options
                .as_ref()
                .and_then(|m| m.get(&request.provider))
                .cloned()
            else {
                return rejection("live_options_missing");
            };
            if names.len() > 128
                || names.iter().any(|name| {
                    name.is_empty() || name.len() > 128 || name.chars().any(char::is_control)
                })
            {
                return rejection("live_options_missing");
            }
            (scopes, names)
        };
        let Some(service) = self.settings.settings_registry_service.as_deref() else {
            return rejection("settings_registry_unavailable");
        };
        let key = match serde_json::to_string(&(caller, &request.target, &request.provider)) {
            Ok(key) => key,
            Err(_) => return rejection("invalid_request"),
        };
        if let Some(entry) = self.live_settings_jobs.get(&key) {
            if let Some(result) = &entry.result {
                let result = result.clone();
                self.live_settings_jobs.remove(&key);
                let Ok((current, current_names)) = result else {
                    return rejection("settings_registry_unavailable");
                };
                let mut frozen = validated["settings"].clone();
                let Some(values) = frozen.as_object_mut() else {
                    return rejection("invalid_settings_snapshot");
                };
                for name in admitted_names
                    .iter()
                    .filter(|name| current_names.contains(name))
                {
                    let Some(value) = current.settings.get(name) else {
                        return rejection("invalid_settings_snapshot");
                    };
                    values.insert(name.clone(), value.clone());
                }
                if serde_json::to_vec(&frozen).map_or(true, |v| v.len() > 8192) {
                    return rejection("invalid_settings_snapshot");
                }
                return json!({"status":"succeeded", "target":request.target, "provider":request.provider, "revision":current.revision, "settings":frozen});
            }
            return json!({"status":"pending"});
        }
        if self.live_settings_jobs.len() >= MAX_LIVE_JOBS {
            return rejection("live_settings_busy");
        }
        let Some(participant) = self
            .settings
            .participants
            .iter()
            .find(|p| p.provider == request.provider)
            .cloned()
        else {
            return rejection("provider_not_selected");
        };
        let Ok(job) =
            settings_snapshot::SettingsSnapshotJob::new(Some(service), &[participant], scopes)
        else {
            return rejection("invalid_settings_snapshot");
        };
        self.live_settings_jobs.insert(
            key,
            LiveSettingsJob {
                caller: caller.into(),
                request: payload,
                started: Instant::now(),
                job,
                result: None,
            },
        );
        json!({"status":"pending"})
    }

    pub(super) fn advance_live_settings(
        &mut self,
        send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) {
        let keys: Vec<_> = self.live_settings_jobs.keys().cloned().collect();
        for key in keys {
            let Some(entry) = self.live_settings_jobs.get(&key) else {
                continue;
            };
            let valid = self.read_settings_for_operation(
                &entry.caller,
                entry.request.clone(),
                "read_live_provider_settings",
            )["status"]
                == "succeeded";
            if !valid || entry.started.elapsed() >= LIVE_JOB_TTL {
                if let Some(entry) = self.live_settings_jobs.remove(&key) {
                    entry.job.cancel(send);
                }
                continue;
            }
            let entry = self.live_settings_jobs.get_mut(&key).unwrap();
            if entry.result.is_some() {
                continue;
            }
            match entry.job.advance(send) {
                Ok(None) => {}
                Ok(Some(snapshots)) => {
                    let provider = entry.request["provider"].as_str().unwrap_or("");
                    entry.result = Some(
                        match (snapshots.get(provider), entry.job.live_options(provider)) {
                            (Some(snapshot), Some(names)) => match entry
                                .job
                                .descriptor_revision(provider)
                            {
                                Some(descriptor_revision) => {
                                    let mut snapshot = snapshot.clone();
                                    snapshot.revision =
                                        format!("{}-{}", snapshot.revision, descriptor_revision);
                                    Ok((snapshot, names.to_vec()))
                                }
                                None => Err(()),
                            },
                            _ => Err(()),
                        },
                    );
                }
                Err(_) => {
                    entry.job.cancel(send);
                    entry.result = Some(Err(()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn live_reads_overlay_only_admitted_and_current_names_and_recheck_stream() {
        let dir =
            std::env::temp_dir().join(format!("amaterasu-live-settings-{}", identifier().unwrap()));
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
            ["read_live_provider_settings".into()].into(),
        );
        coordinator
            .settings
            .provider_settings_readers
            .insert("video".into(), ["effect".into()].into());
        coordinator.settings.settings_registry_service = Some("registry/1".into());
        let method = json!({"service":"provider/1","method":"prepare","interface_major":1,"interface_minor":0,"schema_revision":1});
        coordinator.settings.participants.push(serde_json::from_value(json!({"provider":"effect","plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"installed","settings":{}})).unwrap());
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        let target = json!({"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":2});
        let session = json!({"host_epoch":epoch,"id":"session","generation":1,"phase":"ready","owner":{"caller":"transport","initiator":"client"},"delegates":[],"application_ref":null,"settings_revision":"revision","application_retired":false,"streams":{"stream":{"generation":2,"phase":"ready","candidate":null,"required":[],"acknowledgements":[],"retirements":[],"provider_graph":{"order":["effect"],"dependencies":{"effect":[]}},"provider_settings":{"effect":{"revision":"registry-1","settings":{"live":1,"new_live":2,"removed_live":3,"frozen":4}}},"snapshot_scopes":{"application":null,"paired_client":"client","session":"session"},"admitted_live_options":{"effect":["live","removed_live"]}}}});
        let mut journal = coordinator.shared.borrow().value.clone();
        journal
            .sessions
            .insert("session".into(), serde_json::from_value(session).unwrap());
        save(&coordinator.shared, journal).unwrap();
        let request =
            json!({"operation":"read_live_provider_settings","target":target,"provider":"effect"});
        assert_eq!(
            coordinator.handle("other", "other", request.clone())["reason"],
            "permission_denied"
        );
        assert_eq!(
            coordinator.handle("video", "video", request.clone())["status"],
            "pending"
        );
        let mut replies = VecDeque::from([
            Response::Accepted { id: 1 },
            Response::Result {
                payload: json!({"ok":true,"status":"ok","revision":7,"descriptor_revision":9,"live_options":["live","new_live"],"effective":{"live":{"value":10},"new_live":{"value":20},"removed_live":{"value":30},"frozen":{"value":40}}}),
            },
            Response::Ok,
        ]);
        let mut send = |_: Command| Ok(replies.pop_front().unwrap());
        for _ in 0..3 {
            coordinator.advance_live_settings(&mut send);
        }
        let result = coordinator.handle("video", "video", request.clone());
        assert_eq!(result["revision"], "registry-7-9");
        assert_eq!(
            result["settings"],
            json!({"live":10,"new_live":2,"removed_live":3,"frozen":4})
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
            .snapshot_scopes = None;
        save(&coordinator.shared, journal).unwrap();
        assert_eq!(
            coordinator.handle("video", "video", request.clone())["reason"],
            "settings_scope_missing"
        );
        let mut journal = coordinator.shared.borrow().value.clone();
        journal
            .sessions
            .get_mut("session")
            .unwrap()
            .streams
            .get_mut("stream")
            .unwrap()
            .snapshot_scopes = Some(
            serde_json::from_value(
                json!({"application":null,"paired_client":"client","session":"session"}),
            )
            .unwrap(),
        );
        save(&coordinator.shared, journal).unwrap();
        assert_eq!(
            coordinator.handle("video", "video", request.clone())["status"],
            "pending"
        );
        coordinator.advance_live_settings(&mut |_| Ok(Response::Accepted { id: 11 }));
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
        let mut cleanup = Vec::new();
        coordinator.advance_live_settings(&mut |command| {
            cleanup.push(command);
            Ok(Response::Ok)
        });
        assert!(matches!(
            cleanup.as_slice(),
            [
                Command::Cancel { request: 11 },
                Command::ReleaseResult { request: 11 }
            ]
        ));
        assert!(coordinator.live_settings_jobs.is_empty());
        drop(coordinator);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
