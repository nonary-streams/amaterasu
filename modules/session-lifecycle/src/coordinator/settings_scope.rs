//! Read-only scope resolution for the installed, broker-authenticated registry.
use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    caller: String,
    initiator: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeRequest {
    operation: String,
    operation_id: String,
    target: LifecycleTarget,
    scope: String,
    scope_id: String,
    access: String,
    source: Source,
    #[serde(default)]
    input_binding: Option<String>,
    #[serde(default)]
    input_boot_epoch: Option<String>,
}

impl Coordinator {
    /// Registry attests source using the parent broker request, never end-user
    /// JSON. The registry's broker identity must be selected by installed policy.
    pub(super) fn resolve_settings_scope(&self, caller: &str, payload: Value) -> Value {
        if self.settings.settings_registry_caller.is_empty()
            || caller != self.settings.settings_registry_caller
        {
            return rejection("permission_denied");
        }
        let Ok(request) = serde_json::from_value::<ScopeRequest>(payload) else {
            return rejection("invalid_request");
        };
        if request.operation != "resolve_settings_scope"
            || !valid_id(&request.operation_id)
            || request.access != "read"
            || !valid_id(&request.scope_id)
            || !valid_id(&request.source.caller)
            || !valid_id(&request.source.initiator)
        {
            return rejection("invalid_request");
        }
        // A token is not an independent authority in this resolver. Source must
        // still be the actual owner or a policy-approved session delegate.
        if request
            .input_binding
            .as_ref()
            .is_some_and(|token| !valid_id(token))
        {
            return rejection("invalid_request");
        }
        let shared = self.shared.borrow();
        let journal = &shared.value;
        match (&request.input_binding, &request.input_boot_epoch) {
            (Some(token), Some(epoch))
                if valid_id(epoch) && journal.input_boot_epochs.get(token) == Some(epoch) => {}
            (None, None) => {}
            _ => return rejection("permission_denied"),
        }
        let owner_session = operations::checked_target(
            journal,
            &request.target,
            &request.source.caller,
            &request.source.initiator,
        )
        .ok()
        .map(|(session, _)| session);
        let session = owner_session.or_else(|| {
            let token = request.input_binding.as_deref()?;
            let binding = journal.input_bindings.get(token)?;
            if journal.revoked_inputs.contains(token)
                || !self
                    .settings
                    .consumers
                    .input
                    .contains(&request.source.caller)
                || binding.input_consumer != request.source.caller
                || binding.host_epoch != journal.host_epoch
                || journal
                    .inputs
                    .validate_input_binding(&request.source.caller, token, binding)
                    .is_err()
            {
                return None;
            }
            let LifecycleTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                generation,
            } = &request.target
            else {
                return None;
            };
            if host_epoch != &binding.host_epoch
                || session_id != &binding.session_id
                || stream_id != &binding.stream_id
                || *generation != binding.generation
            {
                return None;
            }
            let session = journal.sessions.get(session_id)?;
            let stream = session.streams.get(stream_id)?;
            if session.host_epoch != *host_epoch
                || stream.generation != *generation
                || stream.phase != StreamPhase::Active
                || stream.candidate.as_ref().is_none_or(|candidate| {
                    candidate.geometry_revision != binding.geometry_revision
                })
            {
                return None;
            }
            Some(session)
        });
        let Some(session) = session else {
            return rejection("permission_denied");
        };
        let expected = match request.scope.as_str() {
            "session" => Some(session.id.as_str()),
            "application"
                if session.application_ref.get("kind").and_then(Value::as_str)
                    == Some("application") =>
            {
                session
                    .application_ref
                    .get("app_id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_id(id))
            }
            _ => None,
        };
        if expected != Some(request.scope_id.as_str()) {
            return rejection("permission_denied");
        }
        json!({"status":"succeeded", "target":request.target, "scope":request.scope, "scope_id":request.scope_id})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn registry_resolves_only_canonical_owner_scope() {
        let root = std::env::temp_dir().join(format!(
            "lifecycle-scope-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
        coordinator.settings.settings_registry_caller = "registry".into();
        let epoch = coordinator.shared.borrow().value.host_epoch.clone();
        coordinator.shared.borrow_mut().value.sessions.insert(
            "session".into(),
            SessionRecord {
                host_epoch: epoch.clone(),
                id: "session".into(),
                generation: 3,
                phase: SessionPhase::Ready,
                owner: crate::action_authority::Principal {
                    caller: "transport".into(),
                    initiator: "client".into(),
                },
                delegates: BTreeSet::from(["delegate".into()]),
                application_ref: json!({"kind":"application","app_id":"app"}),
                settings_revision: "revision".into(),
                application_retired: false,
                streams: BTreeMap::new(),
                obligations: vec![],
                required_consumers: None,
                application: None,
                application_operation_id: None,
                admission_ids: Default::default(),
                provider_settings: Default::default(),
            },
        );
        let request = json!({"operation":"resolve_settings_scope", "operation_id":"request", "target":{"kind":"session","host_epoch":epoch,"session_id":"session","generation":3},"scope":"application","scope_id":"app","access":"read","source":{"caller":"transport","initiator":"client"}});
        assert_eq!(
            coordinator.resolve_settings_scope("registry", request.clone())["status"],
            "succeeded"
        );
        assert_eq!(
            coordinator.resolve_settings_scope("foreign", request.clone())["status"],
            "rejected"
        );
        for (field, value) in [
            ("scope_id", json!("other")),
            ("scope", json!("paired_client")),
            ("access", json!("write")),
        ] {
            let mut forged = request.clone();
            forged[field] = value;
            assert_eq!(
                coordinator.resolve_settings_scope("registry", forged)["status"],
                "rejected"
            );
        }
        let mut stale = request.clone();
        stale["target"]["generation"] = json!(4);
        assert_eq!(
            coordinator.resolve_settings_scope("registry", stale)["status"],
            "rejected"
        );
        let mut wrong = request.clone();
        wrong["source"]["initiator"] = json!("other");
        assert_eq!(
            coordinator.resolve_settings_scope("registry", wrong)["status"],
            "rejected"
        );
        let mut delegate = request;
        delegate["source"] = json!({"caller":"delegate","initiator":"delegate"});
        delegate["scope"] = json!("session");
        delegate["scope_id"] = json!("session");
        assert_eq!(
            coordinator.resolve_settings_scope("registry", delegate)["status"],
            "succeeded"
        );
        coordinator.settings.consumers.input.insert("input".into());
        let binding = crate::input_authority::ActiveBinding {
            host_epoch: epoch.clone(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 4,
            geometry_revision: 7,
            input_consumer: "input".into(),
            allowed_actions: BTreeSet::new(),
        };
        let token = {
            let mut shared = coordinator.shared.borrow_mut();
            let candidate: CaptureBinding = serde_json::from_value(json!({"capture_provider":"capture", "selector_schema":"selector/1", "selector":{}, "resource":{"provider":"display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null},"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":7,"generation":4})).unwrap();
            shared
                .value
                .sessions
                .get_mut("session")
                .unwrap()
                .streams
                .insert(
                    "stream".into(),
                    StreamRecord {
                        generation: 4,
                        phase: StreamPhase::Active,
                        candidate: Some(candidate),
                        required: BTreeSet::new(),
                        acknowledgements: BTreeSet::new(),
                        retirements: BTreeSet::new(),
                        binding_owner: String::new(),
                        preparation_transition: String::new(),
                        provider_settings: Default::default(),
                    },
                );
            let token = shared.value.inputs.issue_input_binding(&binding).unwrap();
            shared.value.input_bindings.insert(token.clone(), binding);
            shared
                .value
                .input_boot_epochs
                .insert(token.clone(), "boot".into());
            token
        };
        let input = json!({"operation":"resolve_settings_scope","operation_id":"input-request","target":{"kind":"stream","host_epoch":epoch,"session_id":"session","stream_id":"stream","generation":4},"scope":"application","scope_id":"app","access":"read","source":{"caller":"input","initiator":"stream-session"},"input_binding":token,"input_boot_epoch":"boot"});
        assert_eq!(
            coordinator.resolve_settings_scope("registry", input.clone())["status"],
            "succeeded"
        );
        let mut foreign = input.clone();
        foreign["source"]["caller"] = json!("other");
        assert_eq!(
            coordinator.resolve_settings_scope("registry", foreign)["status"],
            "rejected"
        );
        let mut stale_boot = input.clone();
        stale_boot["input_boot_epoch"] = json!("stale");
        stale_boot["source"] = json!({"caller":"transport", "initiator":"client"});
        assert_eq!(
            coordinator.resolve_settings_scope("registry", stale_boot)["status"],
            "rejected"
        );
        let mut missing_boot = input.clone();
        missing_boot["input_boot_epoch"] = Value::Null;
        assert_eq!(
            coordinator.resolve_settings_scope("registry", missing_boot)["status"],
            "rejected"
        );
        coordinator
            .shared
            .borrow_mut()
            .value
            .revoked_inputs
            .insert(token);
        assert_eq!(
            coordinator.resolve_settings_scope("registry", input)["status"],
            "rejected"
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
}
