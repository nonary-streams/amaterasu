//! Extension points: an owner asks the modules that trusted composition lists
//! for one of its hooks, and decides what to do with their answers.
//!
//! Policy names each hook's owner and its participants, in order. The owner
//! starts a [`HookRun`] and polls it; each participant receives the run as an
//! ordinary request whose [`hook`](crate::requests::IncomingRequest::hook) is
//! set, and replies to it or refuses it.
//!
//! Two choices shape a run:
//!
//! - **Required or best effort.** A required run needs every participant: an
//!   absent, failed or refusing participant stops it, and no later one is
//!   asked. Use it for a decision that must not be made without them. A
//!   [`best_effort`](HookRun::best_effort) run asks every participant and
//!   reports each outcome. Use it for contributions the owner can do without.
//! - **Broadcast or chained.** By default every participant receives the
//!   owner's input, and the owner combines the answers. In a
//!   [`chained`](HookRun::chained) run each participant receives the previous
//!   participant's result and returns the value to pass on, so later
//!   participants build on earlier ones.
//!
//! The broker does not merge or validate answers. The owner checks every
//! contribution, and the final value of a chain, before acting on it.
use crate::control_error::{BrokerError, ControlError, ControlResult, ErrorCode, checked};
use crate::requests::IncomingRequest;
use crate::{Command, ModuleResult, Response, RuntimeClient};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::task::Poll;

/// Consecutive lost exchanges after which a run stops retrying.
const MAX_CONSECUTIVE_FAILURES: u8 = 3;

/// The service prefix under which a participant receives a hook's requests.
const HOOK_SERVICE_PREFIX: &str = "hooks/";

/// How a run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum HookOutcome {
    /// A required run in which every participant answered, or a best-effort
    /// run in which every participant was attempted.
    Completed(HookReport),
    /// The broker did not start the run, so no participant was asked. A
    /// required run is not started while a participant is absent.
    Refused(BrokerError),
    /// A required run started and ended early: a participant refused or
    /// failed, the hook's deadline passed, or the owner cancelled it. The
    /// participants before that one did answer. The message names the
    /// participant that stopped the run.
    Stopped(BrokerError),
    /// The run may have started and its result could not be read.
    Unconfirmed(ControlError),
}

impl HookOutcome {
    /// Policy declares no such hook, so no module is composed into this
    /// extension point. That holds until the runtime restarts, and the owner
    /// carries on with its own behavior.
    pub fn is_undeclared(&self) -> bool {
        matches!(
            self,
            Self::Refused(refusal) if !refusal.is_refusal() && refusal.code == ErrorCode::NotFound
        )
    }
}

/// One participant's part in a run.
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    pub participant: String,
    /// The participant's answer, or why it gave none. An absent participant
    /// reports `unavailable`; one that declined reports its refusal.
    pub result: Result<Value, BrokerError>,
}

/// Every participant's part in a finished run, in policy order.
#[derive(Clone, Debug, PartialEq)]
pub struct HookReport {
    pub contributions: Vec<Contribution>,
    input: Value,
}

impl HookReport {
    /// The answers that were given, in policy order.
    pub fn answers(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.contributions.iter().filter_map(|contribution| {
            let answer = contribution.result.as_ref().ok()?;
            Some((contribution.participant.as_str(), answer))
        })
    }

    /// The participants of a best-effort run that gave no answer, and why.
    pub fn failures(&self) -> impl Iterator<Item = (&str, &BrokerError)> {
        self.contributions.iter().filter_map(|contribution| {
            let failure = contribution.result.as_ref().err()?;
            Some((contribution.participant.as_str(), failure))
        })
    }

    /// Decode every answer as `T`. An answer of another shape is reported
    /// beside its participant, so one bad contribution does not hide the rest.
    pub fn decode<T: DeserializeOwned>(&self) -> Vec<(&str, Result<T, String>)> {
        self.answers()
            .map(|(participant, answer)| {
                let decoded = T::deserialize(answer).map_err(|error| error.to_string());
                (participant, decoded)
            })
            .collect()
    }

    /// The value a chained run ends with: the last answer given, or the
    /// owner's input when no participant answered.
    pub fn value(&self) -> &Value {
        self.answers()
            .last()
            .map_or(&self.input, |(_, answer)| answer)
    }
}

enum Stage {
    /// Nothing has been sent.
    Unsent,
    /// The run was submitted and its reply was lost.
    AdmissionUnknown,
    /// The broker is asking the participants.
    Running { invocation: u64, cancel_sent: bool },
    /// The outcome is known and held until the broker's record is released.
    Releasing {
        invocation: u64,
        outcome: HookOutcome,
    },
    /// The outcome has been returned.
    Settled,
}

/// One run of a hook this module owns, polled once per tick until it settles.
///
/// ```ignore
/// let mut run = HookRun::new(LAUNCH_ENVIRONMENT, &unique_operation_id("launch"), &environment)?
///     .best_effort()
///     .chained();
/// // each tick:
/// if let Poll::Ready(HookOutcome::Completed(report)) = run.poll(&runtime) {
///     let environment: Environment = serde_json::from_value(report.value().clone())?;
/// }
/// ```
///
/// Give each run its own operation identifier. A run that settles also gives
/// the identifier up, unless it is [`fenced`](Self::fenced).
pub struct HookRun {
    hook: String,
    input: Value,
    operation_id: String,
    best_effort: bool,
    chained: bool,
    fenced: bool,
    cancel_requested: bool,
    stage: Stage,
    consecutive_failures: u8,
}

impl HookRun {
    /// A required, broadcast run of `hook` with `input`.
    pub fn new<T: Serialize>(hook: &str, operation_id: &str, input: &T) -> ControlResult<Self> {
        Ok(Self {
            hook: hook.to_owned(),
            input: serde_json::to_value(input).map_err(ControlError::payload)?,
            operation_id: operation_id.to_owned(),
            best_effort: false,
            chained: false,
            fenced: false,
            cancel_requested: false,
            stage: Stage::Unsent,
            consecutive_failures: 0,
        })
    }

    /// Ask every participant and report each outcome, instead of stopping at
    /// the first one that is absent, fails or refuses.
    pub fn best_effort(mut self) -> Self {
        self.best_effort = true;
        self
    }

    /// Give each participant the previous participant's result. In a
    /// best-effort chain a participant that gives no answer is passed over
    /// and the value it received goes to the next one.
    pub fn chained(mut self) -> Self {
        self.chained = true;
        self
    }

    /// Keep the operation identifier fenced after the run settles: a repeat
    /// of the same identifier then reports `operation_expired` and never asks
    /// the participants again. The broker holds a bounded number of fenced
    /// identifiers for its whole lifetime, so reserve this for effects that
    /// must not repeat.
    pub fn fenced(mut self) -> Self {
        self.fenced = true;
        self
    }

    /// Ask for the run to end. Keep polling for the outcome.
    pub fn cancel(&mut self) {
        self.cancel_requested = true;
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// Whether the outcome has already been returned.
    pub fn is_settled(&self) -> bool {
        matches!(self.stage, Stage::Settled)
    }

    /// Advance the run. Returns the outcome exactly once.
    pub fn poll(&mut self, runtime: &RuntimeClient) -> Poll<HookOutcome> {
        self.poll_with(&mut |command| runtime.send(command))
    }

    /// Advance the run through an injected transport.
    pub fn poll_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<HookOutcome> {
        match std::mem::replace(&mut self.stage, Stage::Settled) {
            Stage::Unsent => self.start(send),
            Stage::AdmissionUnknown => self.resolve_admission(send),
            Stage::Running {
                invocation,
                cancel_sent,
            } => self.collect(invocation, cancel_sent, send),
            Stage::Releasing {
                invocation,
                outcome,
            } => self.release_then(invocation, outcome, send),
            Stage::Settled => Poll::Ready(HookOutcome::Unconfirmed(ControlError::UnexpectedReply(
                "the hook run was polled after it settled".into(),
            ))),
        }
    }

    fn run_command(&self) -> Command {
        Command::RunHookOperation {
            hook: self.hook.clone(),
            payload: self.input.clone(),
            operation_id: self.operation_id.clone(),
            cleanup: self.best_effort,
            chain: self.chained,
        }
    }

    fn start(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<HookOutcome> {
        if self.cancel_requested {
            return Poll::Ready(HookOutcome::Refused(BrokerError::new(
                ErrorCode::Cancelled,
                "cancelled before the run started",
            )));
        }
        match checked(send(self.run_command())) {
            Ok(Response::Accepted { id }) => self.running(id),
            Err(ControlError::Broker(refusal)) => Poll::Ready(HookOutcome::Refused(refusal)),
            Ok(response) => {
                self.exchange_lost(ControlError::unexpected(&response), Stage::AdmissionUnknown)
            }
            Err(error) => self.exchange_lost(error, Stage::AdmissionUnknown),
        }
    }

    /// The first submission may or may not have arrived. The broker finds a
    /// started run by its operation identifier, so repeating the identical
    /// submission never starts a second one. A cancelled run only looks.
    fn resolve_admission(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<HookOutcome> {
        let command = if self.cancel_requested {
            Command::LookupHook {
                operation_id: self.operation_id.clone(),
            }
        } else {
            self.run_command()
        };
        match checked(send(command)) {
            Ok(Response::Accepted { id }) => self.running(id),
            // The run started and its result is no longer retained.
            Err(error) if error.is(&ErrorCode::OperationExpired) => {
                Poll::Ready(HookOutcome::Unconfirmed(error))
            }
            Err(error) if self.cancel_requested && error.is(&ErrorCode::NotFound) => {
                Poll::Ready(HookOutcome::Refused(BrokerError::new(
                    ErrorCode::Cancelled,
                    "cancelled before the run started",
                )))
            }
            Err(ControlError::Broker(refusal)) => Poll::Ready(HookOutcome::Refused(refusal)),
            Ok(response) => {
                self.exchange_lost(ControlError::unexpected(&response), Stage::AdmissionUnknown)
            }
            Err(error) => self.exchange_lost(error, Stage::AdmissionUnknown),
        }
    }

    fn collect(
        &mut self,
        invocation: u64,
        mut cancel_sent: bool,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<HookOutcome> {
        if self.cancel_requested && !cancel_sent {
            match checked(send(Command::CancelHook { invocation })) {
                Ok(Response::Ok) => cancel_sent = true,
                Err(error) if error.is(&ErrorCode::NotFound) => cancel_sent = true,
                Ok(response) => {
                    let unsent = Stage::Running {
                        invocation,
                        cancel_sent,
                    };
                    return self.exchange_lost(ControlError::unexpected(&response), unsent);
                }
                Err(error) => {
                    let unsent = Stage::Running {
                        invocation,
                        cancel_sent,
                    };
                    return self.exchange_lost(error, unsent);
                }
            }
        }
        let waiting = Stage::Running {
            invocation,
            cancel_sent,
        };
        match checked(send(Command::HookResult { invocation })) {
            Ok(Response::Pending) => {
                self.consecutive_failures = 0;
                self.stage = waiting;
                Poll::Pending
            }
            Ok(Response::HookResult { proposals }) => {
                let contributions = proposals
                    .into_iter()
                    .map(|proposal| Contribution {
                        participant: proposal.participant,
                        result: Ok(proposal.payload),
                    })
                    .collect();
                self.finished(invocation, contributions, send)
            }
            Ok(Response::HookCleanupResult { outcomes }) => {
                let contributions = outcomes
                    .into_iter()
                    .map(|outcome| Contribution {
                        participant: outcome.participant,
                        result: participant_result(*outcome.result),
                    })
                    .collect();
                self.finished(invocation, contributions, send)
            }
            // The broker no longer holds the run, so its result cannot be read.
            Err(error) if error.is(&ErrorCode::NotFound) => {
                Poll::Ready(HookOutcome::Unconfirmed(error))
            }
            Err(ControlError::Broker(ending)) => {
                self.release_then(invocation, HookOutcome::Stopped(ending), send)
            }
            Ok(response) => self.exchange_lost(ControlError::unexpected(&response), waiting),
            Err(error) => self.exchange_lost(error, waiting),
        }
    }

    fn finished(
        &mut self,
        invocation: u64,
        contributions: Vec<Contribution>,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<HookOutcome> {
        let report = HookReport {
            contributions,
            input: self.input.clone(),
        };
        self.release_then(invocation, HookOutcome::Completed(report), send)
    }

    /// Release the broker's record of the run, then return its outcome.
    fn release_then(
        &mut self,
        invocation: u64,
        outcome: HookOutcome,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Poll<HookOutcome> {
        let release = Command::ReleaseHook {
            invocation,
            forget: !self.fenced,
        };
        let released = match checked(send(release)) {
            Ok(Response::Ok) => true,
            // A lost reply may still have released it.
            Err(error) => error.is(&ErrorCode::NotFound),
            Ok(_) => false,
        };
        if released {
            return Poll::Ready(outcome);
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            // The outcome is known. The record expires on its own.
            return Poll::Ready(outcome);
        }
        self.stage = Stage::Releasing {
            invocation,
            outcome,
        };
        Poll::Pending
    }

    fn running(&mut self, invocation: u64) -> Poll<HookOutcome> {
        self.consecutive_failures = 0;
        self.stage = Stage::Running {
            invocation,
            cancel_sent: false,
        };
        Poll::Pending
    }

    fn exchange_lost(&mut self, error: ControlError, resume: Stage) -> Poll<HookOutcome> {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
            return Poll::Ready(HookOutcome::Unconfirmed(error));
        }
        self.stage = resume;
        Poll::Pending
    }
}

fn participant_result(result: Response) -> Result<Value, BrokerError> {
    match result {
        Response::Result { payload } => Ok(payload),
        Response::Error { code, message } => Err(BrokerError::from_wire(&code, message)),
        other => Err(BrokerError::new(
            ErrorCode::Other("unexpected_reply".into()),
            format!("unexpected participant result: {other:?}"),
        )),
    }
}

impl RuntimeClient {
    /// Take part in `hook`. Trusted composition must grant this module the
    /// hook and list it among the hook's participants. Each run then arrives
    /// through [`next_request`](Self::next_request) with
    /// [`hook`](IncomingRequest::hook) set, from the hook's owner.
    ///
    /// Reply with this module's contribution, or the value to pass on in a
    /// chained run. [`refuse`](Self::refuse) to decline: that stops a required
    /// run and is recorded against this participant in a best-effort one.
    pub fn join_hook(&self, hook: &str) -> ControlResult<()> {
        match self.send_checked(Command::RegisterHook {
            hook: hook.to_owned(),
        })? {
            Response::Ok => Ok(()),
            response => Err(ControlError::unexpected(&response)),
        }
    }
}

impl<T> IncomingRequest<T> {
    /// The hook this request is one run of, when it is not a call to a
    /// service. Its caller is the hook's owner.
    pub fn hook(&self) -> Option<&str> {
        self.service.strip_prefix(HOOK_SERVICE_PREFIX)
    }
}

#[cfg(test)]
mod tests;
