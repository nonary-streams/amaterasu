//! Which admissions belong to owners that have ended.
//!
//! An admission holds its mutation domains until its provider confirms the
//! work it admitted. When that never happens, a later session must not wait
//! on it forever. The authority that hands domains out cannot tell which
//! holders have ended; only the session and stream phases recorded here say
//! so. This module turns them into the one fact the authority needs.
use super::*;
use crate::prepare_authority::{EndedHolder, admitted_this_run};

/// Whether work for this session may still need its domains: it is being
/// prepared, is ready or is paused for resume.
fn session_open(session: &SessionRecord) -> bool {
    matches!(
        session.phase,
        SessionPhase::Preparing | SessionPhase::Ready | SessionPhase::Paused
    )
}

/// Whether work for this stream may still need its domains.
fn stream_open(stream: &StreamRecord) -> bool {
    matches!(
        stream.phase,
        StreamPhase::Preparing | StreamPhase::Ready | StreamPhase::Active
    )
}

/// Every retained admission whose owner has ended, by admission ID.
///
/// An admission this process did not make was made by an earlier host run.
/// Nothing of that run is still executing, so its holder has ended whatever
/// phase the record shows. That is decided per admission, not per session:
/// releasing a session left over from an earlier run makes new admissions in
/// this run, and their work is in flight like any other.
///
/// For an admission of this run, a session has ended once it is past
/// `Paused`, and a stream once it is past `Active` or its record is gone. An
/// admission of this run whose target was never recorded cannot be attributed
/// and is left out.
pub(super) fn ended_holders(journal: &Journal) -> BTreeMap<String, EndedHolder> {
    let mut ended = BTreeMap::new();
    for session in journal.sessions.values() {
        for id in &session.admission_ids {
            let earlier_run = !admitted_this_run(id);
            let target = session.admission_targets.get(id);
            let stream = match target {
                Some(LifecycleTarget::Stream { stream_id, .. }) => Some(stream_id.as_str()),
                _ => None,
            };
            let open = target.is_some()
                && session_open(session)
                && stream.is_none_or(|id| session.streams.get(id).is_some_and(stream_open));
            if earlier_run || (target.is_some() && !open) {
                ended.insert(id.clone(), holder(earlier_run, &session.id, stream));
            }
        }
    }
    ended
}

fn holder(earlier_run: bool, session: &str, stream: Option<&str>) -> EndedHolder {
    let owner = match stream {
        Some(stream) => format!("stream {stream} of session {session}"),
        None => format!("session {session}"),
    };
    EndedHolder {
        earlier_run,
        owner: if earlier_run {
            format!("{owner} from an earlier host run")
        } else {
            owner
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepare_authority::note_admitted_this_run;
    use serde_json::json;

    /// One session with one stream. It holds an admission for the session,
    /// one for the stream and one whose target was never recorded.
    fn journal(prefix: &str, session_phase: &str, stream_phase: &str) -> Journal {
        let id = |name: &str| format!("{prefix}-{name}");
        let mut value = json!({
            "version": 3,
            "host_epoch": "now",
            "receipts": {},
            "sessions": {"s": {
                "host_epoch": "now", "id": "s", "generation": 1, "phase": session_phase,
                "owner": {"caller": "nvhttp", "initiator": "nvhttp"}, "delegates": [],
                "application_ref": {"kind": "desktop"}, "settings_revision": "1",
                "application_retired": false,
                "streams": {"v": {
                    "generation": 1, "phase": stream_phase, "candidate": null,
                    "required": ["capture"], "acknowledgements": [], "retirements": []
                }},
                "admission_ids": [id("session"), id("stream"), id("unattributed")],
                "admission_targets": {}
            }}
        });
        value["sessions"]["s"]["admission_targets"][id("session")] = json!({
            "kind": "session", "host_epoch": "now", "session_id": "s", "generation": 1
        });
        value["sessions"]["s"]["admission_targets"][id("stream")] = json!({
            "kind": "stream", "host_epoch": "now", "session_id": "s", "stream_id": "v",
            "generation": 1
        });
        serde_json::from_value(value).unwrap()
    }

    fn admitted_now(prefix: &str) {
        for name in ["session", "stream", "unattributed"] {
            note_admitted_this_run(&format!("{prefix}-{name}"));
        }
    }

    #[test]
    fn an_open_session_and_stream_hold_their_admissions() {
        admitted_now("open");
        for session in ["preparing", "ready", "paused"] {
            for stream in ["preparing", "ready", "active"] {
                let journal = journal("open", session, stream);
                assert!(ended_holders(&journal).is_empty(), "{session} {stream}");
            }
        }
    }

    #[test]
    fn a_stream_that_was_asked_to_end_no_longer_holds_its_admission() {
        admitted_now("stream-ended");
        for stream in ["quiescing", "retired", "recovery_required"] {
            let ended = ended_holders(&journal("stream-ended", "ready", stream));
            // The session it belongs to is still open and keeps its own.
            assert_eq!(
                ended.keys().collect::<Vec<_>>(),
                ["stream-ended-stream"],
                "{stream}"
            );
            let holder = &ended["stream-ended-stream"];
            assert!(!holder.earlier_run);
            assert_eq!(holder.owner, "stream v of session s");
        }
    }

    #[test]
    fn a_session_that_was_asked_to_end_holds_none_of_its_admissions() {
        admitted_now("session-ended");
        for session in ["ending", "retired", "ended", "recovery_required"] {
            let ended = ended_holders(&journal("session-ended", session, "active"));
            assert_eq!(
                ended.keys().collect::<Vec<_>>(),
                ["session-ended-session", "session-ended-stream"],
                "{session}"
            );
            assert!(ended.values().all(|holder| !holder.earlier_run));
            assert_eq!(ended["session-ended-session"].owner, "session s");
        }
    }

    #[test]
    fn every_admission_of_an_earlier_run_has_ended_whatever_its_record_says() {
        // This process made none of these admissions.
        let ended = ended_holders(&journal("earlier", "ready", "active"));
        assert_eq!(ended.len(), 3);
        assert!(ended.values().all(|holder| holder.earlier_run));
        assert_eq!(
            ended["earlier-session"].owner,
            "session s from an earlier host run"
        );
    }
}
