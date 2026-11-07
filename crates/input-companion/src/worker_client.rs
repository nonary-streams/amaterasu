//! Ordered worker exchanges for the existing v1 companion wire protocol.
//!
//! Replies have no request identifier. Exact acknowledgement therefore requires
//! one outstanding request per connection: a timeout retains that request, and
//! a subsequent operation must consume its reply before sending another frame.
#[cfg(target_os = "linux")]
use crate::GamepadFeedback;
use crate::{InputError, InputEvent, InputPacket, InputResult, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) trait WorkerTransport {
    fn write_frame(&mut self, frame: &[u8], deadline: Instant) -> InputResult<()>;
    /// A timeout must preserve every partial frame for the next read.
    fn read_frame(&mut self, deadline: Instant) -> InputResult<Option<Vec<u8>>>;
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Request {
    #[serde(rename = "worker_start")]
    Start,
    #[serde(rename = "worker_event")]
    Event { packet: InputPacket },
    #[cfg(target_os = "linux")]
    #[serde(rename = "worker_feedback_poll")]
    FeedbackPoll { generation: u64, token: String },
    #[serde(rename = "worker_close")]
    Close,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Ok,
    Granted {
        generation: u64,
        token: String,
        // Windows v1 grants predate the separate Linux gamepad capability.
        #[serde(default)]
        gamepad: bool,
    },
    Denied,
    #[cfg(target_os = "linux")]
    Feedback {
        event: Option<GamepadFeedback>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Event,
    #[cfg(target_os = "linux")]
    Feedback,
    Close,
}

enum Completion {
    Accepted,
    Denied,
    #[cfg(target_os = "linux")]
    Feedback(Option<GamepadFeedback>),
}

pub(crate) struct WorkerClient<T: WorkerTransport> {
    transport: T,
    token: String,
    generation: u64,
    next_sequence: u64,
    #[cfg(target_os = "linux")]
    gamepad_enabled: bool,
    pending: Option<Operation>,
    closing: bool,
    closed: bool,
    poisoned: bool,
    reply_timeout: Duration,
    shutdown_deadline: Option<Instant>,
}

impl<T: WorkerTransport> WorkerClient<T> {
    pub(crate) fn connect(transport: T, reply_timeout: Duration) -> InputResult<Self> {
        Self::connect_with_deadline(transport, reply_timeout, None)
    }

    pub(crate) fn connect_with_deadline(
        transport: T,
        reply_timeout: Duration,
        shutdown_deadline: Option<Instant>,
    ) -> InputResult<Self> {
        let mut client = Self {
            transport,
            token: String::new(),
            generation: 0,
            next_sequence: 1,
            #[cfg(target_os = "linux")]
            gamepad_enabled: false,
            pending: None,
            closing: false,
            closed: false,
            poisoned: false,
            reply_timeout,
            shutdown_deadline,
        };
        let deadline = client.deadline()?;
        client.write_request(&Request::Start, deadline)?;
        let Reply::Granted {
            generation,
            token,
            gamepad,
        } = client.read_reply(deadline)?
        else {
            return Err(InputError::PermissionDenied);
        };
        if generation == 0
            || token.len() != 32
            || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(InputError::InvalidGrant);
        }
        client.generation = generation;
        client.token = token;
        #[cfg(target_os = "linux")]
        {
            client.gamepad_enabled = gamepad;
        }
        #[cfg(windows)]
        let _ = gamepad;
        Ok(client)
    }

    pub(crate) fn restrict_deadline(&mut self, deadline: Instant) {
        self.shutdown_deadline = Some(
            self.shutdown_deadline
                .map_or(deadline, |old| old.min(deadline)),
        );
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn gamepad_enabled(&self) -> bool {
        self.gamepad_enabled
    }

    pub(crate) fn send(&mut self, event: InputEvent) -> InputResult<()> {
        let deadline = self.deadline()?;
        self.finish_before_input(deadline)?;
        let sequence = self.next_sequence;
        self.next_sequence = sequence.checked_add(1).ok_or(InputError::InvalidSequence)?;
        let request = Request::Event {
            packet: InputPacket {
                version: PROTOCOL_VERSION,
                generation: self.generation,
                sequence,
                lease_token: self.token.clone(),
                event,
            },
        };
        self.begin(Operation::Event, &request, deadline)?;
        match self.finish_pending(deadline)? {
            Completion::Accepted => Ok(()),
            _ => Err(InputError::PermissionDenied),
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn poll_gamepad_feedback(&mut self) -> InputResult<Option<GamepadFeedback>> {
        let deadline = self.deadline()?;
        self.finish_before_input(deadline)?;
        let request = Request::FeedbackPoll {
            generation: self.generation,
            token: self.token.clone(),
        };
        let frame = serde_json::to_vec(&request).map_err(|_| InputError::InvalidEvent)?;
        self.begin_frame(Operation::Feedback, &frame, deadline)?;
        match self.finish_pending(deadline)? {
            Completion::Feedback(event) => Ok(event),
            _ => Err(InputError::PermissionDenied),
        }
    }

    pub(crate) fn close(&mut self) -> InputResult<()> {
        if self.closed {
            return Ok(());
        }
        // Closing admission does not depend on receiving the earlier reply.
        self.closing = true;
        let deadline = self.deadline()?;
        if let Some(operation) = self.pending {
            let completion = self.finish_pending(deadline)?;
            if operation == Operation::Close {
                return close_result(completion);
            }
            // A rejected input operation still owes device cleanup. Its exact
            // reply has now been consumed, so Close cannot observe that reply.
        }
        self.begin(Operation::Close, &Request::Close, deadline)?;
        close_result(self.finish_pending(deadline)?)
    }

    fn finish_before_input(&mut self, deadline: Instant) -> InputResult<()> {
        if self.closing || self.closed {
            return Err(InputError::Closed);
        }
        self.ensure_usable()?;
        if self.pending.is_some() && matches!(self.finish_pending(deadline)?, Completion::Denied) {
            return Err(InputError::PermissionDenied);
        }
        Ok(())
    }

    fn begin(
        &mut self,
        operation: Operation,
        request: &Request,
        deadline: Instant,
    ) -> InputResult<()> {
        let frame = serde_json::to_vec(request).map_err(|_| InputError::InvalidEvent)?;
        self.begin_frame(operation, &frame, deadline)
    }

    fn begin_frame(
        &mut self,
        operation: Operation,
        frame: &[u8],
        deadline: Instant,
    ) -> InputResult<()> {
        self.ensure_usable()?;
        if self.pending.is_some() || Instant::now() >= deadline {
            return Err(InputError::Device);
        }
        self.pending = Some(operation);
        if let Err(error) = self.transport.write_frame(frame, deadline) {
            // A failed write may have delivered a partial request. Reusing its
            // byte stream cannot establish which later reply belongs to Close.
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    fn finish_pending(&mut self, deadline: Instant) -> InputResult<Completion> {
        self.ensure_usable()?;
        let operation = self.pending.ok_or(InputError::Device)?;
        let completion = match (operation, self.read_reply(deadline)?) {
            (Operation::Event | Operation::Close, Reply::Ok) => Completion::Accepted,
            (_, Reply::Denied) => Completion::Denied,
            #[cfg(target_os = "linux")]
            (Operation::Feedback, Reply::Feedback { event }) => Completion::Feedback(event),
            _ => {
                self.poisoned = true;
                return Err(InputError::InvalidEvent);
            }
        };
        self.pending = None;
        if operation == Operation::Close && matches!(completion, Completion::Accepted) {
            self.closed = true;
        }
        Ok(completion)
    }

    fn deadline(&self) -> InputResult<Instant> {
        let local = Instant::now()
            .checked_add(self.reply_timeout)
            .ok_or(InputError::Device)?;
        Ok(self
            .shutdown_deadline
            .map_or(local, |shutdown| local.min(shutdown)))
    }

    fn read_reply(&mut self, deadline: Instant) -> InputResult<Reply> {
        if Instant::now() >= deadline {
            return Err(InputError::Device);
        }
        let frame = match self.transport.read_frame(deadline) {
            Ok(Some(frame)) => frame,
            // Preserve the pending operation across a bounded observation wait.
            Ok(None) => return Err(InputError::Device),
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        serde_json::from_slice(&frame).map_err(|_| {
            self.poisoned = true;
            InputError::InvalidEvent
        })
    }

    fn write_request(&mut self, request: &Request, deadline: Instant) -> InputResult<()> {
        if Instant::now() >= deadline {
            return Err(InputError::Device);
        }
        let frame = serde_json::to_vec(request).map_err(|_| InputError::InvalidEvent)?;
        self.transport.write_frame(&frame, deadline)
    }

    fn ensure_usable(&self) -> InputResult<()> {
        if self.poisoned {
            Err(InputError::Device)
        } else {
            Ok(())
        }
    }
}

fn close_result(completion: Completion) -> InputResult<()> {
    match completion {
        Completion::Accepted => Ok(()),
        _ => Err(InputError::Device),
    }
}

impl<T: WorkerTransport> Drop for WorkerClient<T> {
    fn drop(&mut self) {
        if self.generation != 0
            && !self.closed
            && !self.poisoned
            && self.pending.is_none()
            && let Ok(deadline) = self.deadline()
        {
            let _ = self.write_request(&Request::Close, deadline);
        }
        // Dropping the transport triggers server-owned revocation. It is not a
        // successful Close acknowledgement and must never mark cleanup complete.
    }
}

#[cfg(test)]
mod tests;
