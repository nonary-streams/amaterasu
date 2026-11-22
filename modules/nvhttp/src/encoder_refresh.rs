//! Keeps the encoder capability snapshot current with bounded, idle-only
//! catalog reads from the selected video service.
//!
//! Video refreshes its selected encoder while idle. A failed catalog read
//! keeps the last good snapshot and retries with backoff.
use crate::encoder_snapshot::EncoderSnapshot;
use amaterasu_module_sdk::{Command, Response, RuntimeClient};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[cfg(test)]
const VIDEO_SERVICE: &str = amaterasu_module_sdk::video::DEFAULT_CONTROL_SERVICE;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

pub struct EncoderRefresh {
    service: String,
    pending: Option<Pending>,
    sequence: u64,
    next_refresh: Instant,
    retry_delay: Duration,
    update: Option<EncoderSnapshot>,
}

enum Pending {
    Submitting { operation_id: String },
    Waiting { child: u64 },
    Releasing { child: u64, payload: Option<Value> },
}

#[cfg(test)]
impl Default for EncoderRefresh {
    fn default() -> Self {
        Self::new(VIDEO_SERVICE.into())
    }
}

impl EncoderRefresh {
    pub fn new(service: String) -> Self {
        Self {
            service,
            pending: None,
            sequence: 0,
            next_refresh: Instant::now(),
            retry_delay: INITIAL_RETRY_DELAY,
            update: None,
        }
    }

    /// Advance an outstanding call, or start one when due and no stream is
    /// active. Failures never fail the module: they keep the last snapshot
    /// and schedule a retry, or retry a pending release on the next tick.
    pub fn tick(&mut self, runtime: &RuntimeClient, stream_active: bool) {
        let pending = match self.pending.take() {
            Some(pending) => pending,
            None if stream_active || Instant::now() < self.next_refresh => return,
            None => {
                self.sequence = self.sequence.wrapping_add(1);
                Pending::Submitting {
                    operation_id: format!("nvhttp.encoder_capabilities.{}", self.sequence),
                }
            }
        };
        self.pending = match pending {
            Pending::Submitting { operation_id } => self.submit(runtime, operation_id),
            Pending::Waiting { child } => self.wait(runtime, child),
            Pending::Releasing { child, payload } => self.release(runtime, child, payload),
        };
    }

    /// The newest successfully parsed snapshot, once.
    pub fn take_update(&mut self) -> Option<EncoderSnapshot> {
        self.update.take()
    }

    fn submit(&mut self, runtime: &RuntimeClient, operation_id: String) -> Option<Pending> {
        let response = runtime.send(Command::Request {
            service: self.service.clone(),
            payload: json!({"action":amaterasu_module_sdk::encode::SELECTED_ENCODER_CATALOG_ACTION}),
            timeout_ms: REQUEST_TIMEOUT.as_millis() as u64,
            operation_id,
            parent: None,
        });
        if let Ok(Response::Accepted { id }) = response {
            return Some(Pending::Waiting { child: id });
        }
        self.schedule_retry();
        None
    }

    fn wait(&mut self, runtime: &RuntimeClient, child: u64) -> Option<Pending> {
        match runtime.send(Command::TakeResult { request: child }) {
            Ok(Response::Result { payload }) => self.release(runtime, child, Some(payload)),
            Ok(Response::Error { code, .. }) if code == "not_found" => {
                self.schedule_retry();
                None
            }
            Ok(Response::Error { .. }) => self.release(runtime, child, None),
            Ok(_) | Err(_) => Some(Pending::Waiting { child }),
        }
    }

    fn release(
        &mut self,
        runtime: &RuntimeClient,
        child: u64,
        payload: Option<Value>,
    ) -> Option<Pending> {
        match runtime.send(Command::ReleaseResult { request: child }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. }) if code == "not_found" => {}
            _ => return Some(Pending::Releasing { child, payload }),
        }
        match payload.as_ref().map(EncoderSnapshot::parse) {
            Some(Ok(snapshot)) => {
                self.update = Some(snapshot);
                self.retry_delay = INITIAL_RETRY_DELAY;
                self.next_refresh = Instant::now() + REFRESH_INTERVAL;
            }
            Some(Err(code)) => {
                eprintln!("amaterasu nvhttp encoder enumeration rejected reason={code}");
                self.schedule_retry();
            }
            None => self.schedule_retry(),
        }
        None
    }

    fn schedule_retry(&mut self) {
        self.next_refresh = Instant::now() + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(MAX_RETRY_DELAY);
    }
}

#[cfg(test)]
mod tests;
