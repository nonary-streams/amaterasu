//! Pure transition rules. Authentication, durable persistence, provider invocation,
//! and native retirement evidence are the caller's responsibility.
use crate::planning::{ConsumerAcknowledgement, ExactBinding, validate_acknowledgements};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionPhase {
    Preparing,
    Retained,
    Ending,
    Retired,
    Ended,
    RecoveryRequired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamPhase {
    Preparing,
    Active,
    Quiescing,
    Retired,
    RecoveryRequired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stream {
    pub generation: u64,
    pub phase: StreamPhase,
    pub candidate: ExactBinding,
    pub required_consumers: BTreeSet<String>,
    pub acknowledgements: BTreeMap<String, ConsumerAcknowledgement>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub epoch: String,
    pub id: String,
    pub generation: u64,
    pub phase: SessionPhase,
    pub application_retired: bool,
    pub streams: BTreeMap<String, Stream>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionError {
    InvalidState,
    StaleGeneration,
    InvalidBinding,
    MissingConsumers,
    DuplicateStream,
    StreamsNotRetired,
    ApplicationNotRetired,
    CleanupPending,
}
impl Session {
    pub fn prepared(&mut self) -> Result<(), TransitionError> {
        if self.phase != SessionPhase::Preparing {
            return Err(TransitionError::InvalidState);
        }
        self.phase = SessionPhase::Retained;
        Ok(())
    }
    /// The candidate has already been checked against the approved provider plan.
    pub fn prepare_stream(
        &mut self,
        id: String,
        candidate: ExactBinding,
        required_consumers: BTreeSet<String>,
    ) -> Result<(), TransitionError> {
        if self.phase != SessionPhase::Retained || id.is_empty() {
            return Err(TransitionError::InvalidState);
        }
        if required_consumers.is_empty() || required_consumers.iter().any(String::is_empty) {
            return Err(TransitionError::MissingConsumers);
        }
        if self.streams.contains_key(&id) {
            return Err(TransitionError::DuplicateStream);
        }
        self.streams.insert(
            id,
            Stream {
                generation: candidate.generation,
                phase: StreamPhase::Preparing,
                candidate,
                required_consumers,
                acknowledgements: BTreeMap::new(),
            },
        );
        Ok(())
    }
    /// Caller authenticates the consumer and obtains its actual observation.
    pub fn acknowledge(
        &mut self,
        id: &str,
        generation: u64,
        ack: ConsumerAcknowledgement,
    ) -> Result<(), TransitionError> {
        let stream = self
            .streams
            .get_mut(id)
            .ok_or(TransitionError::InvalidState)?;
        if stream.generation != generation {
            return Err(TransitionError::StaleGeneration);
        }
        if stream.phase != StreamPhase::Preparing
            || !stream.required_consumers.contains(&ack.consumer_id)
            || ack.binding != stream.candidate
        {
            return Err(TransitionError::InvalidBinding);
        }
        if let Some(existing) = stream.acknowledgements.get(&ack.consumer_id) {
            return if existing == &ack {
                Ok(())
            } else {
                Err(TransitionError::InvalidBinding)
            };
        }
        stream.acknowledgements.insert(ack.consumer_id.clone(), ack);
        Ok(())
    }
    pub fn activate(&mut self, id: &str, generation: u64) -> Result<(), TransitionError> {
        let stream = self
            .streams
            .get_mut(id)
            .ok_or(TransitionError::InvalidState)?;
        if stream.generation != generation {
            return Err(TransitionError::StaleGeneration);
        }
        if stream.phase != StreamPhase::Preparing {
            return Err(TransitionError::InvalidState);
        }
        let required: Vec<_> = stream.required_consumers.iter().cloned().collect();
        let acks: Vec<_> = stream.acknowledgements.values().cloned().collect();
        validate_acknowledgements(&stream.candidate, &required, &acks)
            .map_err(|_| TransitionError::MissingConsumers)?;
        stream.phase = StreamPhase::Active;
        Ok(())
    }
    /// Persist this fence together with permit revocation before stopping consumers.
    pub fn quiesce(&mut self, id: &str, generation: u64) -> Result<(), TransitionError> {
        let stream = self
            .streams
            .get_mut(id)
            .ok_or(TransitionError::InvalidState)?;
        if stream.generation != generation {
            return Err(TransitionError::StaleGeneration);
        }
        if !matches!(stream.phase, StreamPhase::Active | StreamPhase::Preparing) {
            return Err(TransitionError::InvalidState);
        }
        stream.phase = StreamPhase::Quiescing;
        Ok(())
    }
    /// Only invoke after native consumers have actually retired and cleanup completed.
    pub fn retire_stream(&mut self, id: &str, generation: u64) -> Result<(), TransitionError> {
        let stream = self
            .streams
            .get_mut(id)
            .ok_or(TransitionError::InvalidState)?;
        if stream.generation != generation {
            return Err(TransitionError::StaleGeneration);
        }
        if stream.phase != StreamPhase::Quiescing {
            return Err(TransitionError::InvalidState);
        }
        stream.phase = StreamPhase::Retired;
        Ok(())
    }
    pub fn begin_end(&mut self) -> Result<(), TransitionError> {
        if !matches!(self.phase, SessionPhase::Preparing | SessionPhase::Retained) {
            return Err(TransitionError::InvalidState);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(TransitionError::InvalidState)?;
        self.phase = SessionPhase::Ending;
        for stream in self.streams.values_mut() {
            if matches!(stream.phase, StreamPhase::Active | StreamPhase::Preparing) {
                stream.phase = StreamPhase::Quiescing;
            }
        }
        Ok(())
    }
    /// Application retirement is asserted only after trusted native confirmation.
    pub fn application_retired(&mut self) -> Result<(), TransitionError> {
        if self.phase != SessionPhase::Ending {
            return Err(TransitionError::InvalidState);
        }
        self.application_retired = true;
        Ok(())
    }
    pub fn retire_session(&mut self) -> Result<(), TransitionError> {
        if self.phase != SessionPhase::Ending {
            return Err(TransitionError::InvalidState);
        }
        if self
            .streams
            .values()
            .any(|stream| stream.phase != StreamPhase::Retired)
        {
            return Err(TransitionError::StreamsNotRetired);
        }
        if !self.application_retired {
            return Err(TransitionError::ApplicationNotRetired);
        }
        self.phase = SessionPhase::Retired;
        Ok(())
    }
    pub fn complete_restoration(
        &mut self,
        obligations_complete: bool,
    ) -> Result<(), TransitionError> {
        if self.phase != SessionPhase::Retired {
            return Err(TransitionError::InvalidState);
        }
        if !obligations_complete {
            return Err(TransitionError::CleanupPending);
        }
        self.phase = SessionPhase::Ended;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> ExactBinding {
        ExactBinding {
            provider: "capture".into(),
            selector_schema: "v1".into(),
            resource_id: "output".into(),
            incarnation: "one".into(),
            execution_domain: "seat".into(),
            effective_mode: "mode".into(),
            geometry_revision: 1,
            plan_revision: 1,
            generation: 1,
        }
    }
    #[test]
    fn exact_ack_pause_and_end_require_retirement_and_cleanup() {
        let mut session = Session {
            epoch: "epoch".into(),
            id: "session".into(),
            generation: 1,
            phase: SessionPhase::Preparing,
            application_retired: false,
            streams: BTreeMap::new(),
        };
        session.prepared().unwrap();
        let candidate = binding();
        session
            .prepare_stream(
                "s1".into(),
                candidate.clone(),
                ["capture".into(), "input".into()].into(),
            )
            .unwrap();
        assert_eq!(
            session.activate("s1", 1),
            Err(TransitionError::MissingConsumers)
        );
        for id in ["capture", "input"] {
            session
                .acknowledge(
                    "s1",
                    1,
                    ConsumerAcknowledgement {
                        consumer_id: id.into(),
                        binding: candidate.clone(),
                    },
                )
                .unwrap();
        }
        session.activate("s1", 1).unwrap();
        session.quiesce("s1", 1).unwrap();
        assert_eq!(
            session.activate("s1", 1),
            Err(TransitionError::InvalidState)
        );
        session.retire_stream("s1", 1).unwrap();
        assert_eq!(session.phase, SessionPhase::Retained);
        session.begin_end().unwrap();
        assert_eq!(
            session.retire_session(),
            Err(TransitionError::ApplicationNotRetired)
        );
        session.application_retired().unwrap();
        session.retire_session().unwrap();
        assert_eq!(
            session.complete_restoration(false),
            Err(TransitionError::CleanupPending)
        );
        session.complete_restoration(true).unwrap();
    }
    #[test]
    fn stale_generation_and_mismatched_observation_cannot_activate() {
        let mut session = Session {
            epoch: "epoch".into(),
            id: "session".into(),
            generation: 1,
            phase: SessionPhase::Retained,
            application_retired: false,
            streams: BTreeMap::new(),
        };
        let candidate = binding();
        session
            .prepare_stream("s".into(), candidate.clone(), ["capture".into()].into())
            .unwrap();
        let mut wrong = candidate.clone();
        wrong.incarnation = "two".into();
        assert_eq!(
            session.acknowledge(
                "s",
                1,
                ConsumerAcknowledgement {
                    consumer_id: "capture".into(),
                    binding: wrong
                }
            ),
            Err(TransitionError::InvalidBinding)
        );
        assert_eq!(
            session.acknowledge(
                "s",
                2,
                ConsumerAcknowledgement {
                    consumer_id: "capture".into(),
                    binding: candidate
                }
            ),
            Err(TransitionError::StaleGeneration)
        );
        assert_eq!(
            session.activate("s", 1),
            Err(TransitionError::MissingConsumers)
        );
    }
}
