//! Freeze a trusted transport's previously reserved external source without
//! granting any lifecycle participant new native mutation authority.
use super::*;
use amaterasu_extension_api::{
    BORROWED_CAPTURE_CONTEXT_KEY, BorrowedCaptureContext, borrowed_capture_from_application,
};

pub(super) fn freeze_application(
    settings: &LifecycleSettings,
    caller: &str,
    initiator: &str,
    payload: &Value,
    journal: &Journal,
) -> ModuleResult<Value> {
    let mut application = payload
        .get("application_ref")
        .cloned()
        .unwrap_or(Value::Null);
    if application.get(BORROWED_CAPTURE_CONTEXT_KEY).is_some() {
        return Err("borrowed_capture_context_is_coordinator_owned".into());
    }
    let Some(value) = payload
        .get("borrowed_capture")
        .filter(|value| !value.is_null())
    else {
        return Ok(application);
    };
    if !settings.capture_binding_mode.is_external()
        || !settings.borrowed_capture_callers.contains(caller)
        || caller != initiator
    {
        return Err("borrowed_capture_not_authorized".into());
    }
    let source: BorrowedCaptureContext =
        serde_json::from_value(value.clone()).map_err(|_| "invalid_borrowed_capture")?;
    source.validate().map_err(|_| "invalid_borrowed_capture")?;
    if journal.sessions.values().any(|session| {
        session.phase != SessionPhase::Ended
            && borrowed_capture_from_application(&session.application_ref)
                .ok()
                .flatten()
                .is_some_and(|existing| {
                    existing.reservation_id == source.reservation_id
                        || existing.transport_id == source.transport_id
                })
    }) {
        return Err("borrowed_capture_reservation_in_use".into());
    }
    let fields = application
        .as_object_mut()
        .ok_or("borrowed_capture_requires_application_context")?;
    fields.insert(
        BORROWED_CAPTURE_CONTEXT_KEY.into(),
        serde_json::to_value(source).map_err(|_| "invalid_borrowed_capture")?,
    );
    Ok(application)
}

pub(super) fn valid_session(session: &SessionRecord) -> bool {
    let Ok(frozen) = borrowed_capture_from_application(&session.application_ref) else {
        return false;
    };
    if frozen.is_some() && !session.capture_binding_mode.is_external() {
        return false;
    }
    session
        .streams
        .values()
        .all(|stream| match (&frozen, &stream.borrowed_capture) {
            (None, None) => true,
            (Some(frozen), Some(source)) => {
                source.validate().is_ok() && same_source(frozen, source)
            }
            _ => false,
        })
}

fn same_source(frozen: &BorrowedCaptureContext, source: &BorrowedCaptureContext) -> bool {
    frozen.session_id == source.session_id
        && frozen.generation == source.generation
        && frozen.output == source.output
        && frozen.mode == source.mode
}

/// Reconnect and Join use a fresh held transport reservation for the same
/// frozen source. The broker-authenticated session delegate supplies the
/// borrower identity; the selected source provider has already authorized it.
pub(super) fn stream_source(
    settings: &LifecycleSettings,
    caller: &str,
    payload: &Value,
    session: &SessionRecord,
) -> ModuleResult<Option<BorrowedCaptureContext>> {
    let frozen = borrowed_capture_from_application(&session.application_ref)
        .map_err(|_| "invalid_frozen_borrowed_capture")?;
    let supplied = payload
        .get("borrowed_capture")
        .filter(|value| !value.is_null());
    let Some(frozen) = frozen else {
        return if supplied.is_none() {
            Ok(None)
        } else {
            Err("borrowed_capture_context_is_frozen".into())
        };
    };
    if !settings.capture_binding_mode.is_external()
        || !session.capture_binding_mode.is_external()
        || !settings.borrowed_capture_callers.contains(caller)
    {
        return Err("borrowed_capture_not_authorized".into());
    }
    let source: BorrowedCaptureContext =
        serde_json::from_value(supplied.cloned().ok_or("borrowed_capture_required")?)
            .map_err(|_| "invalid_borrowed_capture")?;
    source.validate().map_err(|_| "invalid_borrowed_capture")?;
    if !same_source(&frozen, &source) {
        return Err("borrowed_capture_source_changed".into());
    }
    if session
        .streams
        .values()
        .filter(|stream| stream.phase != StreamPhase::Retired)
        .filter_map(|stream| stream.borrowed_capture.as_ref())
        .any(|existing| {
            existing.reservation_id == source.reservation_id
                || existing.transport_id == source.transport_id
        })
    {
        return Err("borrowed_capture_reservation_in_use".into());
    }
    Ok(Some(source))
}

pub(super) fn provider_application(
    session: &SessionRecord,
    target: &LifecycleTarget,
) -> ModuleResult<Value> {
    let mut application = session.application_ref.clone();
    if let LifecycleTarget::Stream { stream_id, .. } = target
        && let Some(source) = session
            .streams
            .get(stream_id)
            .and_then(|stream| stream.borrowed_capture.as_ref())
    {
        application
            .as_object_mut()
            .ok_or("invalid_frozen_borrowed_capture")?
            .insert(
                BORROWED_CAPTURE_CONTEXT_KEY.into(),
                serde_json::to_value(source).map_err(|_| "invalid_borrowed_capture")?,
            );
    }
    Ok(application)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "amaterasu-borrowed-source-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path).unwrap();
            Self(path)
        }
        fn journal(&self) -> std::path::PathBuf {
            self.0.join("journal.json")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn source() -> Value {
        json!({"client_id":"authenticated-client","session_id":"retained-source","generation":3,
            "reservation_id":"source-hold","transport_id":"rtsp-operation","transport_epoch":"transport-epoch","output":"exact-target:monitor",
            "mode":{"width":2560,"height":1440,"refresh_millihz":144000,"hdr":true}})
    }

    fn prepare(id: &str) -> Value {
        json!({"operation":"prepare","operation_id":id,"application_ref":{"kind":"application","app_id":"game","properties":{"unrelated":"preserved"}},
            "requested_video":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},
            "borrowed_capture":source()})
    }

    fn external(directory: &Directory) -> Coordinator {
        let mut coordinator = Coordinator::open(&directory.journal()).unwrap();
        coordinator.settings.capture_binding_mode =
            amaterasu_extension_api::CaptureBindingMode::External;
        coordinator
            .settings
            .trusted_callers
            .insert("transport".into());
        coordinator
            .settings
            .borrowed_capture_callers
            .insert("transport".into());
        coordinator
    }

    #[test]
    fn borrowed_source_is_explicitly_granted_bound_and_durably_frozen() {
        let directory = Directory::new();
        let mut coordinator = external(&directory);
        let request = prepare("prepare-borrowed");
        let admitted = coordinator.handle("transport", "transport", request.clone());
        assert_eq!(admitted["status"], "pending");
        assert_eq!(
            coordinator.handle("transport", "transport", request),
            admitted
        );
        let id = admitted["session_id"].as_str().unwrap();
        let shared = coordinator.shared.borrow();
        let session = &shared.value.sessions[id];
        let frozen = borrowed_capture_from_application(&session.application_ref)
            .unwrap()
            .unwrap();
        assert_eq!(serde_json::to_value(frozen).unwrap(), source());
        assert_eq!(
            session.application_ref["properties"]["unrelated"],
            "preserved"
        );
        assert_eq!(
            session.capture_binding_mode,
            amaterasu_extension_api::CaptureBindingMode::External
        );
        drop(shared);
        assert_eq!(
            coordinator.handle("transport", "transport", prepare("duplicate-hold"))["reason"],
            "borrowed_capture_reservation_in_use"
        );
        drop(coordinator);
        let recovered = Coordinator::open(&directory.journal()).unwrap();
        let shared = recovered.shared.borrow();
        assert_eq!(
            shared.value.sessions[id].application_ref[BORROWED_CAPTURE_CONTEXT_KEY],
            source()
        );
    }

    #[test]
    fn ungranted_delegated_and_managed_source_claims_are_rejected_before_admission() {
        let directory = Directory::new();
        let mut coordinator = external(&directory);
        assert_eq!(
            coordinator.handle("other", "other", prepare("foreign"))["reason"],
            "borrowed_capture_not_authorized"
        );
        assert_eq!(
            coordinator.handle("transport", "client", prepare("delegated"))["reason"],
            "borrowed_capture_not_authorized"
        );
        coordinator.settings.capture_binding_mode =
            amaterasu_extension_api::CaptureBindingMode::Managed;
        assert_eq!(
            coordinator.handle("transport", "transport", prepare("managed"))["reason"],
            "borrowed_capture_not_authorized"
        );
        assert!(coordinator.shared.borrow().value.sessions.is_empty());
    }

    #[test]
    fn application_metadata_cannot_forge_coordinator_owned_source_context() {
        let directory = Directory::new();
        let mut coordinator = external(&directory);
        let mut request = prepare("spoof");
        request.as_object_mut().unwrap().remove("borrowed_capture");
        request["application_ref"][BORROWED_CAPTURE_CONTEXT_KEY] = source();
        assert_eq!(
            coordinator.handle("transport", "transport", request)["reason"],
            "borrowed_capture_context_is_coordinator_owned"
        );
        for (id, field, value) in [
            ("zero-generation", "generation", json!(0)),
            (
                "invalid-selector",
                "output",
                json!("portal-selected-session-source"),
            ),
            (
                "invalid-mode",
                "mode",
                json!({"width":0,"height":1080,"refresh_millihz":60000,"hdr":false}),
            ),
        ] {
            let mut request = prepare(id);
            request["borrowed_capture"][field] = value;
            assert_eq!(
                coordinator.handle("transport", "transport", request)["reason"],
                "invalid_borrowed_capture"
            );
        }
        assert!(coordinator.shared.borrow().value.sessions.is_empty());
    }

    #[test]
    fn stream_borrowers_can_refresh_holds_without_retargeting_the_frozen_source() {
        let directory = Directory::new();
        let mut coordinator = external(&directory);
        let admitted = coordinator.handle("transport", "transport", prepare("source-session"));
        let session_id = admitted["session_id"].as_str().unwrap();
        let shared = coordinator.shared.borrow();
        let session = &shared.value.sessions[session_id];
        let mut borrowed = source();
        borrowed["client_id"] = json!("second-authenticated-borrower");
        borrowed["reservation_id"] = json!("fresh-source-hold");
        borrowed["transport_id"] = json!("fresh-rtsp-operation");
        borrowed["transport_epoch"] = json!("fresh-transport-epoch");
        let request = json!({"borrowed_capture":borrowed});
        let refreshed = stream_source(&coordinator.settings, "transport", &request, session)
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.client_id, "second-authenticated-borrower");
        assert_eq!(refreshed.session_id, "retained-source");
        assert_eq!(
            stream_source(&coordinator.settings, "other", &request, session).unwrap_err(),
            "borrowed_capture_not_authorized"
        );
        assert_eq!(
            stream_source(&coordinator.settings, "transport", &json!({}), session).unwrap_err(),
            "borrowed_capture_required"
        );
        for (field, changed) in [
            ("session_id", json!("different-source")),
            ("generation", json!(4)),
            ("output", json!("exact-target:different-monitor")),
            (
                "mode",
                json!({"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false}),
            ),
        ] {
            let mut changed_request = request.clone();
            changed_request["borrowed_capture"][field] = changed;
            assert_eq!(
                stream_source(
                    &coordinator.settings,
                    "transport",
                    &changed_request,
                    session
                )
                .unwrap_err(),
                "borrowed_capture_source_changed"
            );
        }
    }

    #[test]
    fn provider_context_uses_the_exact_stream_hold_and_preserves_the_source_admission() {
        let directory = Directory::new();
        let mut coordinator = external(&directory);
        let admitted = coordinator.handle("transport", "transport", prepare("source-session"));
        let session_id = admitted["session_id"].as_str().unwrap();
        let mut session = coordinator.shared.borrow().value.sessions[session_id].clone();
        let mut stream_source = source();
        stream_source["client_id"] = json!("second-borrower");
        stream_source["reservation_id"] = json!("second-hold");
        stream_source["transport_id"] = json!("second-transport");
        let stream: StreamRecord = serde_json::from_value(json!({
            "generation":1,"phase":"ready","candidate":null,"required":[],
            "acknowledgements":[],"retirements":[],"borrowed_capture":stream_source,
        }))
        .unwrap();
        session.streams.insert("stream".into(), stream);
        let target = LifecycleTarget::Stream {
            host_epoch: session.host_epoch.clone(),
            session_id: session.id.clone(),
            stream_id: "stream".into(),
            generation: 1,
        };
        assert_eq!(
            provider_application(&session, &target).unwrap()[BORROWED_CAPTURE_CONTEXT_KEY],
            stream_source
        );
        assert_eq!(
            session.application_ref[BORROWED_CAPTURE_CONTEXT_KEY],
            source()
        );
        assert!(valid_session(&session));
        session
            .streams
            .get_mut("stream")
            .unwrap()
            .borrowed_capture
            .as_mut()
            .unwrap()
            .generation += 1;
        assert!(
            !valid_session(&session),
            "journal must reject retargeted source context"
        );
        session.streams.get_mut("stream").unwrap().borrowed_capture = None;
        assert!(
            !valid_session(&session),
            "journal must retain each exact stream hold"
        );
    }
}
