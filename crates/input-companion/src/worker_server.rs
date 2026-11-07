//! Authenticated v1 worker dispatch and native-session retirement.
//!
//! Platform adapters prove the connected OS identity and own the transport.
//! This module owns input dispatch, heartbeat checks and the exact lease's
//! cleanup. Transport destruction must happen after this owned session returns.
use crate::{
    DeviceSink, InputError, InputPacket, InputResult, InputSession, PermitLedger, ProcessIdentity,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(crate) const IO_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    HostPermit {
        process_id: u32,
        creation_time: u64,
        digest: String,
        keyboard: bool,
        mouse: bool,
    },
    WorkerStart,
    WorkerEvent {
        packet: InputPacket,
    },
    WorkerClose,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Reply {
    Ok,
    Granted { generation: u64, token: String },
    Denied,
}

pub(crate) fn decode_request(frame: &[u8]) -> InputResult<Request> {
    serde_json::from_slice(frame).map_err(|_| InputError::InvalidEvent)
}

pub(crate) fn encode_reply(reply: &Reply) -> InputResult<Vec<u8>> {
    serde_json::to_vec(reply).map_err(|_| InputError::InvalidEvent)
}

pub(crate) trait WorkerConnection {
    fn authenticated_peer(&self) -> InputResult<ProcessIdentity>;
    fn read_frame(&mut self, deadline: Instant, stop: &AtomicBool) -> InputResult<Option<Vec<u8>>>;
    fn write_frame(
        &mut self,
        frame: &[u8],
        deadline: Instant,
        stop: &AtomicBool,
    ) -> InputResult<()>;
}

pub(crate) struct WorkerContext<'a> {
    pub(crate) process: &'a ProcessIdentity,
    pub(crate) ledger: &'a Mutex<PermitLedger>,
    pub(crate) stop: &'a AtomicBool,
    pub(crate) cleanup_failed: &'a AtomicBool,
    pub(crate) generation: u64,
    pub(crate) token: &'a str,
}

impl WorkerContext<'_> {
    fn authorized(&self, ledger: &PermitLedger, now: Instant) -> bool {
        ledger.is_current_lease(self.process, self.generation, self.token, now)
    }

    fn authorized_now(&self) -> InputResult<bool> {
        let ledger = self.ledger.lock().map_err(|_| InputError::Closed)?;
        Ok(self.authorized(&ledger, Instant::now()))
    }

    fn release_lease(&self) -> InputResult<()> {
        self.ledger
            .lock()
            .map_err(|_| InputError::Closed)?
            .release_lease(self.process, self.generation, self.token);
        Ok(())
    }
}

pub(crate) fn serve_worker<S: DeviceSink, T: WorkerConnection>(
    connection: &mut T,
    mut session: InputSession<S>,
    context: WorkerContext<'_>,
    wake_listener: impl FnOnce(),
) -> InputResult<()> {
    let result = run_worker(connection, &mut session, &context);
    let cleanup = session.revoke();
    if !session.cleanup_required() {
        context.release_lease()?;
    } else {
        context.cleanup_failed.store(true, Ordering::Release);
        context.stop.store(true, Ordering::Release);
        wake_listener();
        cleanup?;
        return Err(InputError::Device);
    }
    // The session (including its native sink) is retired before the caller can
    // destroy a transport whose pending I/O may need forced containment.
    drop(session);
    result
}

fn run_worker<S: DeviceSink, T: WorkerConnection>(
    connection: &mut T,
    session: &mut InputSession<S>,
    context: &WorkerContext<'_>,
) -> InputResult<()> {
    if context.stop.load(Ordering::Acquire) {
        return Err(InputError::Closed);
    }
    // Native device creation may outlast the host permit. Do not issue a new
    // grant acknowledgement after that exact lease has expired or been revoked.
    if !context.authorized_now()? {
        write_reply(connection, context, Reply::Denied)?;
        return Err(InputError::PermissionDenied);
    }
    write_reply(
        connection,
        context,
        Reply::Granted {
            generation: context.generation,
            token: context.token.to_owned(),
        },
    )?;
    while !context.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if !context.authorized_now()? {
            break;
        }
        session.refresh_host_authorized_expiry(now)?;
        if session.poll_expiry(now)? {
            break;
        }
        let frame = connection.read_frame(Instant::now() + IO_POLL_INTERVAL, context.stop)?;
        if context.stop.load(Ordering::Acquire) {
            break;
        }
        let Some(frame) = frame else {
            continue;
        };
        let request = match decode_request(&frame) {
            Ok(request) => request,
            Err(_) => {
                let _ = write_reply(connection, context, Reply::Denied);
                break;
            }
        };
        // The adapter re-authenticates each message against the OS. An inherited
        // or passed transport handle cannot assert another worker identity.
        if connection.authenticated_peer()? != *context.process {
            write_reply(connection, context, Reply::Denied)?;
            break;
        }
        match request {
            Request::WorkerEvent { packet } => {
                let permit = context.ledger.lock().map_err(|_| InputError::Closed)?;
                if !context.authorized(&permit, Instant::now()) {
                    drop(permit);
                    write_reply(connection, context, Reply::Denied)?;
                    break;
                }
                // Serialize native device writes with host revocation.
                let accepted = session.submit(&packet, Instant::now()).is_ok();
                drop(permit);
                write_reply(
                    connection,
                    context,
                    if accepted { Reply::Ok } else { Reply::Denied },
                )?;
                if !accepted {
                    break;
                }
            }
            Request::WorkerClose => {
                let _ = session.revoke();
                let removed = !session.cleanup_required();
                if removed {
                    context.release_lease()?;
                }
                write_reply(
                    connection,
                    context,
                    if removed { Reply::Ok } else { Reply::Denied },
                )?;
                break;
            }
            _ => {
                let _ = write_reply(connection, context, Reply::Denied);
                break;
            }
        }
    }
    Ok(())
}

fn write_reply(
    connection: &mut impl WorkerConnection,
    context: &WorkerContext<'_>,
    reply: Reply,
) -> InputResult<()> {
    connection.write_frame(
        &encode_reply(&reply)?,
        Instant::now() + IO_POLL_INTERVAL,
        context.stop,
    )
}

#[cfg(test)]
mod tests;
