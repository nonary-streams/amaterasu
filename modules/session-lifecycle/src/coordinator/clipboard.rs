//! Stream-owned clipboard request reservations. No certificate or provider
//! payload is persisted here; NVHTTP owns TLS and pairing authorization.
use super::*;
use amaterasu_module_sdk::effect_authority::ClipboardStream;
use serde::{Deserialize, Serialize};

const MAX_LIVE: usize = 256;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ClipboardReservations {
    highwater: u64,
    live: BTreeMap<String, Reservation>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    source: ClipboardStream,
    owner: crate::action_authority::Principal,
    sequence: u64,
    ticket: String,
    complete: bool,
    uncertain: bool,
}
impl ClipboardReservations {
    pub(super) fn is_empty(&self) -> bool {
        self.highwater == 0 && self.live.is_empty()
    }
    pub(super) fn valid(&self) -> bool {
        self.live
            .values()
            .map(|r| r.sequence)
            .collect::<BTreeSet<_>>()
            .len()
            == self.live.len()
            && self
                .live
                .values()
                .map(|r| &r.ticket)
                .collect::<BTreeSet<_>>()
                .len()
                == self.live.len()
            && self.live.len() <= MAX_LIVE
            && self.live.iter().all(|(id, r)| {
                valid_id(id)
                    && r.source.operation_id(r.sequence).as_deref() == Some(id)
                    && r.source.valid()
                    && valid_id(&r.owner.caller)
                    && valid_id(&r.owner.initiator)
                    && r.sequence > 0
                    && r.sequence <= self.highwater
                    && r.ticket.len() == 32
                    && r.ticket
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    }
    pub(super) fn recover_after_restart(&mut self) {
        for reservation in self.live.values_mut().filter(|r| !r.complete) {
            reservation.uncertain = true;
        }
    }
    pub(super) fn blocks(&self, target: &LifecycleTarget) -> bool {
        let Ok((epoch, session, stream, generation)) = operations::target_parts(target) else {
            return true;
        };
        self.live.values().any(|r| {
            !r.complete
                && r.source.session_id == session
                && (r.uncertain || r.source.host_epoch == epoch)
                && stream.is_none_or(|id| {
                    r.source.stream_id == id && (r.uncertain || r.source.generation == generation)
                })
        })
    }
}

impl Coordinator {
    /// Debug-only fixture that supplies a connected stream to the real
    /// coordinator. The broker, reservation and pause paths remain in use.
    #[cfg(feature = "clipboard-qualification")]
    pub fn qualification_clipboard_stream(&mut self) -> ClipboardStream {
        let session = self.handle(
            "amaterasu.nvhttp",
            "amaterasu.nvhttp",
            json!({
                "operation":"prepare", "operation_id":"clipboard-qualification",
                "source_attestation":{"version":1,"kind":"paired_tls"}
            }),
        );
        let source = ClipboardStream {
            version: 1,
            host_epoch: session["host_epoch"].as_str().unwrap().into(),
            session_id: session["session_id"].as_str().unwrap().into(),
            stream_id: "qualification-stream".into(),
            generation: 1,
        };
        let mut shared = self.shared.borrow_mut();
        let session = shared.value.sessions.get_mut(&source.session_id).unwrap();
        session.phase = SessionPhase::Ready;
        session.streams.insert(
            source.stream_id.clone(),
            StreamRecord {
                provider_graph: None,
                generation: source.generation,
                phase: StreamPhase::Active,
                candidate: None,
                required: BTreeSet::new(),
                acknowledgements: BTreeSet::new(),
                retirements: BTreeSet::new(),
                binding_owner: String::new(),
                preparation_transition: String::new(),
                provider_settings: Default::default(),
            },
        );
        source
    }

    #[cfg(feature = "clipboard-qualification")]
    pub fn qualification_add_clipboard_stream(
        &mut self,
        session_id: &str,
        stream_id: String,
        generation: u64,
    ) -> ClipboardStream {
        let mut shared = self.shared.borrow_mut();
        let session = shared.value.sessions.get_mut(session_id).unwrap();
        let source = ClipboardStream {
            version: 1,
            host_epoch: session.host_epoch.clone(),
            session_id: session.id.clone(),
            stream_id,
            generation,
        };
        session.streams.insert(
            source.stream_id.clone(),
            StreamRecord {
                provider_graph: None,
                generation,
                phase: StreamPhase::Active,
                candidate: None,
                required: BTreeSet::new(),
                acknowledgements: BTreeSet::new(),
                retirements: BTreeSet::new(),
                binding_owner: String::new(),
                preparation_transition: String::new(),
                provider_settings: Default::default(),
            },
        );
        source
    }

    #[cfg(feature = "clipboard-qualification")]
    pub fn qualification_quiesce_clipboard_stream(&mut self, source: &ClipboardStream) {
        let mut shared = self.shared.borrow_mut();
        let session = shared.value.sessions.get_mut(&source.session_id).unwrap();
        let stream = session.streams.get_mut(&source.stream_id).unwrap();
        assert_eq!(stream.generation, source.generation);
        stream.phase = StreamPhase::Quiescing;
    }

    #[cfg(feature = "clipboard-qualification")]
    pub fn qualification_remove_clipboard_stream(&mut self, source: &ClipboardStream) {
        let mut shared = self.shared.borrow_mut();
        let stream = shared
            .value
            .sessions
            .get_mut(&source.session_id)
            .and_then(|session| session.streams.remove(&source.stream_id));
        assert!(
            stream.is_some_and(|stream| stream.phase == StreamPhase::Quiescing
                && stream.generation == source.generation)
        );
    }

    pub(super) fn clipboard_reservation(
        &mut self,
        caller: &str,
        initiator: &str,
        payload: &Value,
    ) -> Value {
        // The provider, NVHTTP and pairing joins are not yet wired end to end.
        if !cfg!(any(test, feature = "clipboard-qualification")) {
            return rejection("clipboard_authority_unavailable");
        }
        let Some(op) = payload["operation"].as_str() else {
            return rejection("invalid_request");
        };
        if op == "clipboard_stream_status" {
            if caller != initiator
                || caller != "amaterasu.nvhttp"
                || payload.as_object().is_none_or(|object| object.len() != 1)
            {
                return rejection("source_denied");
            }
            let journal = &self.shared.borrow().value;
            return json!({"version":1,"status":"ok","highwater":journal.clipboard_reservations.highwater,
                "recovery_required":journal.clipboard_reservations.live.values().any(|record| record.uncertain)});
        }
        if op == "verify_clipboard_stream_closed" {
            // The adapter's trusted-caller operation grant selects the pairing
            // authority. The broker supplies the protocol owner as initiator
            // of its nested request; payload fields cannot name either owner.
            if caller == initiator
                || !valid_id(initiator)
                || payload
                    .as_object()
                    .is_none_or(|object| object.len() != 2 || !object.contains_key("source"))
            {
                return rejection("source_denied");
            }
            let Ok(source) = serde_json::from_value::<ClipboardStream>(payload["source"].clone())
            else {
                return rejection("invalid_source");
            };
            let target = LifecycleTarget::Stream {
                host_epoch: source.host_epoch.clone(),
                session_id: source.session_id.clone(),
                stream_id: source.stream_id.clone(),
                generation: source.generation,
            };
            let journal = &self.shared.borrow().value;
            let Ok((session, Some(stream_id))) =
                operations::checked_target(journal, &target, initiator, initiator)
            else {
                return rejection("source_denied");
            };
            if !source.valid()
                || !session.paired_source_attested
                || session.streams.get(&stream_id).is_none_or(|stream| {
                    !matches!(stream.phase, StreamPhase::Quiescing | StreamPhase::Retired)
                })
                // The cutoff is global and monotonic. Prove that no older
                // reservation on any stream can still arrive at pairing.
                || !journal.clipboard_reservations.live.is_empty()
            {
                return rejection("stream_not_closed");
            }
            return json!({"version":1,"status":"closed","source":source,
                "highwater":journal.clipboard_reservations.highwater});
        }
        let expected: &[&str] = match op {
            "reserve_clipboard_stream" => &["operation", "operation_id", "source", "sequence"],
            "lookup_clipboard_stream" => &["operation", "operation_id"],
            "complete_clipboard_stream" | "acknowledge_clipboard_stream" => {
                &["operation", "operation_id", "ticket"]
            }
            _ => return rejection("invalid_request"),
        };
        if payload.as_object().is_none_or(|object| {
            object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key))
        }) {
            return rejection("invalid_request");
        }
        let Some(id) = payload["operation_id"].as_str().filter(|v| valid_id(v)) else {
            return rejection("invalid_operation_id");
        };
        let mut next = self.shared.borrow().value.clone();
        let owner = crate::action_authority::Principal {
            caller: caller.into(),
            initiator: initiator.into(),
        };
        let result = match op {
            "reserve_clipboard_stream" => {
                let Ok(source) =
                    serde_json::from_value::<ClipboardStream>(payload["source"].clone())
                else {
                    return rejection("invalid_source");
                };
                let Some(sequence) = payload["sequence"].as_u64().filter(|v| *v > 0) else {
                    return rejection("invalid_sequence");
                };
                let target = LifecycleTarget::Stream {
                    host_epoch: source.host_epoch.clone(),
                    session_id: source.session_id.clone(),
                    stream_id: source.stream_id.clone(),
                    generation: source.generation,
                };
                let Ok((session, Some(stream_id))) =
                    operations::checked_target(&next, &target, caller, initiator)
                else {
                    return rejection("source_denied");
                };
                if !source.valid()
                    || !session.paired_source_attested
                    || session.owner != owner
                    || session.phase != SessionPhase::Ready
                    || session
                        .streams
                        .get(&stream_id)
                        .is_none_or(|stream| stream.phase != StreamPhase::Active)
                {
                    return rejection("source_denied");
                }
                if next
                    .clipboard_reservations
                    .live
                    .values()
                    .any(|r| r.uncertain)
                {
                    return rejection("clipboard_recovery_required");
                }
                if source.operation_id(sequence).as_deref() != Some(id) {
                    return rejection("operation_conflict");
                }
                if let Some(existing) = next.clipboard_reservations.live.get(id) {
                    if existing.owner != owner
                        || existing.source != source
                        || existing.sequence != sequence
                        || existing.complete
                    {
                        return rejection("operation_conflict");
                    }
                    return json!({"version":1,"status":"reserved","operation_id":id,"source":source,"ticket":existing.ticket});
                }
                if sequence <= next.clipboard_reservations.highwater {
                    return rejection("operation_conflict");
                }
                if next.clipboard_reservations.live.len() >= MAX_LIVE {
                    return rejection("capacity_exhausted");
                }
                let Ok(ticket) = identifier() else {
                    return rejection("entropy_unavailable");
                };
                next.clipboard_reservations.highwater = sequence;
                next.clipboard_reservations.live.insert(
                    id.into(),
                    Reservation {
                        source: source.clone(),
                        owner,
                        sequence,
                        ticket: ticket.clone(),
                        complete: false,
                        uncertain: false,
                    },
                );
                json!({"version":1,"status":"reserved","operation_id":id,"source":source,"ticket":ticket})
            }
            "lookup_clipboard_stream"
            | "complete_clipboard_stream"
            | "acknowledge_clipboard_stream" => {
                let Some(record) = next.clipboard_reservations.live.get_mut(id) else {
                    return rejection("unknown_reservation");
                };
                if record.owner != owner {
                    return rejection("source_denied");
                }
                if op != "lookup_clipboard_stream" && payload["ticket"] != record.ticket {
                    return rejection("unknown_reservation");
                }
                if record.uncertain {
                    return rejection("clipboard_recovery_required");
                }
                if op == "complete_clipboard_stream" {
                    record.complete = true;
                }
                let source = record.source.clone();
                let ticket = record.ticket.clone();
                let complete = record.complete;
                if op == "acknowledge_clipboard_stream" {
                    if !complete {
                        return rejection("reservation_pending");
                    }
                    next.clipboard_reservations.live.remove(id);
                }
                json!({"version":1,"status":if complete {"completed"} else {"reserved"},"operation_id":id,"source":source,"ticket":ticket})
            }
            _ => return rejection("invalid_request"),
        };
        if save(&self.shared, next).is_err() {
            return rejection("journal_write_failed");
        }
        result
    }
}
