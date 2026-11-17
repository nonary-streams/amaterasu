//! Stream lifecycle state, in the shape of a Vuex store.
//!
//! A stream launch crosses many modules. When one of them refuses, fails or
//! falls back, the launch owner needs to know which module it was, what it
//! was doing and whether the stream could continue. This module gives every
//! layer one way to say so and the launch owner one place to keep it.
//!
//! - **Mutations** are [`StreamIssue`]s. A layer builds one with
//!   [`StreamIssue::critical`], [`StreamIssue::demoted`] or
//!   [`StreamIssue::advisory`]; the constructor records the calling `file:line`.
//! - **Commits** travel on the [`STREAM_REPORTS`] event topic. The runtime
//!   stamps each event with the publishing module's authenticated identity, so
//!   the store records who reported an issue without trusting the payload.
//!   A layer that cannot publish at the point of failure queues reports in a
//!   [`StreamIssueOutbox`] and flushes it from its tick.
//! - **Bindings** let every provider report first-hand under the operation ID
//!   it was called with. A consumer that calls providers for a stream
//!   publishes a [`StreamBinding`] from its own operation prefix to the stream,
//!   and the store files the providers' reports, earlier or later, under it.
//! - **State** is the [`StreamLifecycleStore`] the launch owner keeps: one
//!   [`StreamLifecycle`] per stream, with the first critical failure kept as
//!   the root cause and every demoted fallback kept as a warning.
//! - **Getters** are [`StreamLifecycleStore::snapshot`], which the owner serves
//!   to management.
//!
//! An event is a notification, not a delivery guarantee. Each issue is also
//! written to the reporter's log when it is queued, and a subscriber that
//! falls behind marks the affected streams `audit_gap` rather than presenting
//! an incomplete record as complete.
mod feed;
mod store;
mod termination;
pub use termination::{
    StreamFailure, StreamFailureDecision, StreamFailureHandler, StreamTermination,
};

pub use feed::StreamIssueFeed;
pub use store::{
    LifecycleEntry, MAX_TRACKED_STREAMS, MAX_UNATTRIBUTED_ENTRIES, StreamLifecycle,
    StreamLifecycleSnapshot, StreamLifecycleStore,
};

use crate::control_error::{ControlError, ControlResult};
use crate::events::{Topic, publish_with};
use crate::stream_warnings::{StreamWarning, WarningValue};
use crate::{Command, LogEmitter, LogLevel, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::VecDeque;
use std::panic::Location;

/// The topic every layer reports stream issues and bindings on. Publishing
/// needs the topic's `publish` grant; the launch owner holds its `subscribe`
/// grant.
pub const STREAM_REPORTS: Topic<StreamReport> = Topic::new("amaterasu.stream.report/1");
/// Phase for a confirmed fallback whose operation names the exact failed attempt.
/// Emit only after that attempt is retired or definitively held no resources.
pub const PROVIDER_FALLBACK_CONFIRMED: &str = "provider_fallback_confirmed";

/// One event on [`STREAM_REPORTS`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamReport {
    Issue(StreamIssue),
    Binding(StreamBinding),
}

impl StreamReport {
    pub fn valid(&self) -> bool {
        match self {
            Self::Issue(issue) => issue.valid(),
            Self::Binding(binding) => binding.valid(),
        }
    }

    /// Publish this report on [`STREAM_REPORTS`] through `send`.
    pub fn report_with(
        &self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ControlResult<()> {
        if !self.valid() {
            return Err(ControlError::Payload("invalid stream report".to_owned()));
        }
        publish_with(send, &STREAM_REPORTS, self)
    }
}

/// Operations named `alias`, or derived from it (`<alias>-…`, `<alias>.…`,
/// `<alias>:…`), belong to `stream`. `stream` is the launch owner's stream
/// key or any operation that already resolves to it.
///
/// A module publishes one before, or soon after, it calls providers for a
/// stream under its own operation IDs, so those providers can report
/// first-hand with the IDs they were given.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamBinding {
    pub stream: String,
    pub alias: String,
}

impl StreamBinding {
    pub fn new(stream: impl Into<String>, alias: impl Into<String>) -> Self {
        Self {
            stream: stream.into(),
            alias: alias.into(),
        }
    }

    pub fn valid(&self) -> bool {
        // Both are operation IDs, validated as an issue's operation is.
        bounded(&self.stream, MAX_IDENTIFIER)
            && bounded(&self.alias, MAX_IDENTIFIER)
            && self.stream != self.alias
    }

    pub fn report(&self, runtime: &RuntimeClient) -> ControlResult<()> {
        StreamReport::Binding(self.clone()).report_with(&mut |command| runtime.send(command))
    }
}

/// Entries kept for one stream before the oldest are dropped.
pub const MAX_LIFECYCLE_ENTRIES: usize = 64;
/// Issues an outbox holds while it cannot publish.
pub const MAX_OUTBOX_ISSUES: usize = 64;

const MAX_IDENTIFIER: usize = 128;
const MAX_LABEL: usize = 64;
const MAX_SITE: usize = 256;

/// The layer an issue belongs to. The publisher identity says which module
/// reported it; the layer says which part of the stream it concerns, which
/// lets a module report a failure it observed in a dependency.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleLayer {
    Nvhttp,
    Presentation,
    Rtsp,
    SessionLifecycle,
    StreamSession,
    Display,
    DisplaySettings,
    VirtualDisplay,
    Capture,
    EncodeVideo,
    Audio,
    AudioEncode,
    Input,
    MediaTransport,
    ControlTransport,
    FrameLimiter,
    RtxHdr,
    Application,
    History,
    /// A layer this SDK version does not name.
    #[serde(other)]
    Other,
}

impl LifecycleLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nvhttp => "nvhttp",
            Self::Presentation => "presentation",
            Self::Rtsp => "rtsp",
            Self::SessionLifecycle => "session_lifecycle",
            Self::StreamSession => "stream_session",
            Self::Display => "display",
            Self::DisplaySettings => "display_settings",
            Self::VirtualDisplay => "virtual_display",
            Self::Capture => "capture",
            Self::EncodeVideo => "encode_video",
            Self::Audio => "audio",
            Self::AudioEncode => "audio_encode",
            Self::Input => "input",
            Self::MediaTransport => "media_transport",
            Self::ControlTransport => "control_transport",
            Self::FrameLimiter => "frame_limiter",
            Self::RtxHdr => "rtx_hdr",
            Self::Application => "application",
            Self::History => "history",
            Self::Other => "other",
        }
    }
}

/// Whether the issue ended the stream or the stream continued without it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueDisposition {
    /// The stream cannot start or continue. The first one is the root cause.
    CriticalFailure,
    /// A failure that was not critical; the stream continued on `fallback`.
    DemotedWarning,
    /// Something the user should know that needed no fallback.
    AdvisoryWarning,
}

/// One reported failure or warning: a mutation of a stream's lifecycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamIssue {
    /// Any operation ID the reporter holds for the stream: the launch owner's
    /// stream key, or an ID the owner derived from it (`<key>-…`, `<key>.…`).
    pub operation: String,
    pub layer: LifecycleLayer,
    /// What the reporter was doing, such as `open_rtsp` or `wait_video_ready`.
    pub phase: String,
    pub code: String,
    /// The provider's own code or another short explanation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub disposition: IssueDisposition,
    /// What the stream did instead. Required for a demoted warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// The `file:line` that reported the issue.
    pub site: String,
    /// Set when this is the reporter's own reaction to a dependency's
    /// failure, such as a caller ending a stream because a provider refused.
    /// The dependency reports its failure itself; this issue never outranks
    /// that report as the root cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<LifecycleLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_error: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested: Option<WarningValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<WarningValue>,
}

impl StreamIssue {
    /// A failure that ends the stream. Records the caller's `file:line`.
    #[track_caller]
    pub fn critical(
        operation: impl Into<String>,
        layer: LifecycleLayer,
        phase: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self::new(
            operation,
            layer,
            phase,
            code,
            IssueDisposition::CriticalFailure,
            None,
            Location::caller(),
        )
    }

    /// A failure the stream survives by using `fallback`. Records the
    /// caller's `file:line`.
    #[track_caller]
    pub fn demoted(
        operation: impl Into<String>,
        layer: LifecycleLayer,
        phase: impl Into<String>,
        code: impl Into<String>,
        fallback: impl Into<String>,
    ) -> Self {
        Self::new(
            operation,
            layer,
            phase,
            code,
            IssueDisposition::DemotedWarning,
            Some(fallback.into()),
            Location::caller(),
        )
    }

    /// A warning that needed no fallback. Records the caller's `file:line`.
    #[track_caller]
    pub fn advisory(
        operation: impl Into<String>,
        layer: LifecycleLayer,
        phase: impl Into<String>,
        code: impl Into<String>,
    ) -> Self {
        Self::new(
            operation,
            layer,
            phase,
            code,
            IssueDisposition::AdvisoryWarning,
            None,
            Location::caller(),
        )
    }

    fn new(
        operation: impl Into<String>,
        layer: LifecycleLayer,
        phase: impl Into<String>,
        code: impl Into<String>,
        disposition: IssueDisposition,
        fallback: Option<String>,
        site: &Location<'_>,
    ) -> Self {
        let code = code.into();
        let reason = if disposition == IssueDisposition::AdvisoryWarning {
            String::new()
        } else {
            code.clone()
        };
        Self {
            operation: operation.into(),
            layer,
            phase: phase.into(),
            code,
            reason,
            disposition,
            fallback,
            site: format!("{}:{}", site.file(), site.line()),
            caused_by: None,
            os_error: None,
            requested: None,
            effective: None,
        }
    }

    pub fn because(mut self, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        if !reason.is_empty() || self.disposition == IssueDisposition::AdvisoryWarning {
            self.reason = reason;
        }
        self
    }

    /// Mark this as the reporter's reaction to `dependency` failing.
    pub fn caused_by(mut self, dependency: LifecycleLayer) -> Self {
        self.caused_by = Some(dependency);
        self
    }

    /// A failure in the reporter itself rather than a reaction to another
    /// layer's failure.
    pub fn is_origin(&self) -> bool {
        self.caused_by.is_none()
    }

    pub fn with_os_error(mut self, os_error: i32) -> Self {
        self.os_error = Some(os_error);
        self
    }

    pub fn with_values(
        mut self,
        requested: Option<WarningValue>,
        effective: Option<WarningValue>,
    ) -> Self {
        self.requested = requested;
        self.effective = effective;
        self
    }

    pub fn is_critical(&self) -> bool {
        self.disposition == IssueDisposition::CriticalFailure
    }

    /// Fields are bounded identifiers so an issue cannot carry a request body
    /// or other sensitive data into logs and management responses.
    pub fn valid(&self) -> bool {
        bounded(&self.operation, MAX_IDENTIFIER)
            && identifier(&self.phase, MAX_LABEL)
            && identifier(&self.code, MAX_LABEL)
            && (self.reason.is_empty() || identifier(&self.reason, MAX_LABEL))
            && self
                .fallback
                .as_deref()
                .is_none_or(|fallback| identifier(fallback, MAX_IDENTIFIER))
            && (self.disposition != IssueDisposition::DemotedWarning || self.fallback.is_some())
            && bounded(&self.site, MAX_SITE)
            && [&self.requested, &self.effective]
                .into_iter()
                .all(|value| match value {
                    Some(WarningValue::Label(label)) => identifier(label, MAX_LABEL),
                    _ => true,
                })
    }

    /// The same issue in the stream-warning shape history and clients read.
    pub fn to_stream_warning(&self, source: &str) -> StreamWarning {
        StreamWarning {
            code: self.code.clone(),
            source: source.to_owned(),
            reason: if self.reason.is_empty() {
                self.fallback.clone().unwrap_or_else(|| self.phase.clone())
            } else {
                self.reason.clone()
            },
            os_error: self.os_error,
            requested: self.requested.clone(),
            effective: self.effective.clone(),
        }
    }

    /// Publish this issue on [`STREAM_REPORTS`] through `runtime`.
    pub fn report(&self, runtime: &RuntimeClient) -> ControlResult<()> {
        self.report_with(&mut |command| runtime.send(command))
    }

    /// Publish this issue on [`STREAM_REPORTS`] through `send`.
    pub fn report_with(
        &self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ControlResult<()> {
        if !self.valid() {
            return Err(ControlError::Payload("invalid stream issue".to_owned()));
        }
        publish_with(send, &STREAM_REPORTS, &StreamReport::Issue(self.clone()))
    }

    /// Write this issue to the reporter's own log. This is the durable record
    /// when the event cannot be delivered.
    pub fn log(&self, logger: Option<&LogEmitter>) {
        let Some(logger) = logger else {
            return;
        };
        let (level, target) = match self.disposition {
            IssueDisposition::CriticalFailure => (LogLevel::Error, "stream.lifecycle.failure"),
            IssueDisposition::DemotedWarning => (LogLevel::Warning, "stream.lifecycle.demoted"),
            IssueDisposition::AdvisoryWarning => (LogLevel::Warning, "stream.lifecycle.warning"),
        };
        let _ = logger.emit(level, target, &json!(self).to_string());
    }
}

/// Reports a layer recorded where it could not publish, such as inside a
/// record method with no runtime access. The layer flushes it from its tick.
#[derive(Debug, Default)]
pub struct StreamIssueOutbox {
    issues: VecDeque<StreamReport>,
    dropped: u64,
}

impl StreamIssueOutbox {
    /// Queue `issue`. When the outbox is full the oldest report is dropped
    /// and counted; its log line, written by [`Self::push_logged`], remains.
    pub fn push(&mut self, issue: StreamIssue) {
        self.push_report(StreamReport::Issue(issue));
    }

    /// Queue a binding. It is published in order with the issues around it.
    pub fn push_binding(&mut self, binding: StreamBinding) {
        self.push_report(StreamReport::Binding(binding));
    }

    fn push_report(&mut self, report: StreamReport) {
        if self.issues.len() >= MAX_OUTBOX_ISSUES {
            self.issues.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.issues.push_back(report);
    }

    /// Log `issue` now and queue it for publication.
    pub fn push_logged(&mut self, logger: Option<&LogEmitter>, issue: StreamIssue) {
        issue.log(logger);
        self.push(issue);
    }

    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn len(&self) -> usize {
        self.issues.len()
    }

    /// Issues lost because the outbox was full or held an invalid issue.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Hand every queued report to an owner that keeps the store itself.
    pub fn drain(&mut self) -> impl Iterator<Item = StreamReport> + '_ {
        self.issues.drain(..)
    }

    /// Publish queued issues in order. An invalid issue is discarded and
    /// counted; a failed publication stops the flush and keeps the rest for
    /// the next tick. Returns how many were published.
    pub fn flush_with(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ControlResult<usize> {
        let mut published = 0;
        while let Some(report) = self.issues.front() {
            if !report.valid() {
                self.issues.pop_front();
                self.dropped = self.dropped.saturating_add(1);
                continue;
            }
            report.report_with(send)?;
            self.issues.pop_front();
            published += 1;
        }
        Ok(published)
    }

    pub fn flush(&mut self, runtime: &RuntimeClient) -> ControlResult<usize> {
        self.flush_with(&mut |command| runtime.send(command))
    }
}

fn bounded(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

#[cfg(test)]
mod tests;
