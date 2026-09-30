//! Management owns ingress policy; configuration owns its caller-scoped durable CAS document.
use crate::csrf_policy::CsrfOrigins;
use crate::peer_policy::PeerTier;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const CONFIGURATION: &str = "amaterasu.configuration/1";
const DOCUMENT_VERSION: u32 = 2;
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
const RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Values {
    #[serde(default)]
    origin_web_ui_allowed: Option<PeerTier>,
    #[serde(default)]
    csrf_allowed_origins: Option<CsrfOrigins>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Document {
    revision: u64,
    values: Values,
}

#[derive(Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
enum Operation {
    #[serde(rename = "product_get")]
    Get {},
    #[serde(rename = "product_update")]
    Update {
        expected_revision: u64,
        #[serde(default)]
        origin_web_ui_allowed: SelectionUpdate,
        #[serde(default)]
        csrf_allowed_origins: OriginsUpdate,
    },
}

#[derive(Default)]
enum SelectionUpdate {
    #[default]
    Omitted,
    Selected(Option<PeerTier>),
}

impl<'de> Deserialize<'de> for SelectionUpdate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<PeerTier>::deserialize(deserializer).map(Self::Selected)
    }
}

#[derive(Default)]
enum OriginsUpdate {
    #[default]
    Omitted,
    Selected(Option<CsrfOrigins>),
}

impl<'de> Deserialize<'de> for OriginsUpdate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<CsrfOrigins>::deserialize(deserializer).map(Self::Selected)
    }
}

enum Purpose {
    Load,
    Update { parent: u64, desired: Document },
}

enum Phase {
    Submitting,
    Waiting,
    Cancelling(&'static str),
    Releasing(Response),
}

struct Pending {
    child: Option<u64>,
    command: Command,
    uncertain_admission: bool,
    parent_ended: bool,
    deadline: Instant,
    purpose: Purpose,
    phase: Phase,
}

#[derive(Default)]
pub(crate) struct ProductSettings {
    document: Option<Document>,
    pending: Option<Pending>,
    sequence: u64,
    retry_at: Option<Instant>,
}

impl ProductSettings {
    pub(crate) fn maximum_peer(&self) -> Option<PeerTier> {
        self.document
            .as_ref()
            .map(|document| document.values.origin_web_ui_allowed.unwrap_or_default())
    }

    pub(crate) fn payload(&self) -> Value {
        match &self.document {
            Some(document) => document_payload(document),
            None => failure("settings_unavailable"),
        }
    }

    pub(crate) fn csrf_origins(&self) -> CsrfOrigins {
        self.document
            .as_ref()
            .and_then(|document| document.values.csrf_allowed_origins.clone())
            .unwrap_or_default()
    }

    pub(crate) fn tick(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        if let Some(mut pending) = self.pending.take() {
            let result = self.advance(now, &mut pending, send);
            match result {
                Ok(true) => {}
                Ok(false) => self.pending = Some(pending),
                Err(_) => {
                    if matches!(pending.purpose, Purpose::Update { .. }) {
                        self.document = None;
                    }
                    self.pending = Some(pending);
                }
            }
        }
        if self.pending.is_none()
            && self.document.is_none()
            && self.retry_at.is_none_or(|deadline| now >= deadline)
        {
            self.submit(
                now,
                Purpose::Load,
                CALL_TIMEOUT,
                json!({"action":"product_get"}),
                send,
            )?;
        }
        Ok(())
    }

    pub(crate) fn receive(
        &mut self,
        caller: &str,
        parent: u64,
        remaining_ms: u64,
        payload: Value,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let operation = match serde_json::from_value::<Operation>(payload) {
            Ok(operation) => operation,
            Err(_) => return complete(send, parent, failure("invalid_request")),
        };
        let Operation::Update {
            expected_revision,
            origin_web_ui_allowed,
            csrf_allowed_origins,
        } = operation
        else {
            return complete(send, parent, self.payload());
        };
        if caller != "amaterasu.management" {
            return complete(send, parent, failure("permission_denied"));
        }
        let Some(document) = &self.document else {
            return complete(send, parent, failure("settings_unavailable"));
        };
        if self.pending.is_some() {
            return complete(send, parent, failure("settings_busy"));
        }
        if expected_revision != document.revision {
            return complete(
                send,
                parent,
                json!({"ok":false,"code":"revision_conflict","current_revision":document.revision}),
            );
        }
        let Some(revision) = document.revision.checked_add(1) else {
            return complete(send, parent, failure("resource_exhausted"));
        };
        if remaining_ms <= 100 {
            return complete(send, parent, failure("settings_timeout"));
        }
        let mut desired = document.clone();
        desired.revision = revision;
        if let SelectionUpdate::Selected(selection) = origin_web_ui_allowed {
            desired.values.origin_web_ui_allowed = selection;
        }
        if let OriginsUpdate::Selected(origins) = csrf_allowed_origins {
            desired.values.csrf_allowed_origins = origins;
        }
        let payload = json!({"action":"product_update","expected_revision":expected_revision,"version":DOCUMENT_VERSION,"values":desired.values});
        self.submit(
            Instant::now(),
            Purpose::Update { parent, desired },
            Duration::from_millis(remaining_ms - 100).min(CALL_TIMEOUT),
            payload,
            send,
        )
    }

    fn submit(
        &mut self,
        now: Instant,
        purpose: Purpose,
        timeout: Duration,
        payload: Value,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let Some(sequence) = self.sequence.checked_add(1) else {
            if let Purpose::Update { parent, .. } = purpose {
                return complete(send, parent, failure("resource_exhausted"));
            }
            return Ok(());
        };
        self.sequence = sequence;
        let parent = match purpose {
            Purpose::Update { parent, .. } => Some(parent),
            Purpose::Load => None,
        };
        let command = Command::Request {
            service: CONFIGURATION.into(),
            payload,
            timeout_ms: timeout.as_millis() as u64,
            operation_id: format!("management.policy.{sequence}"),
            parent,
        };
        let mut pending = Pending {
            child: None,
            command,
            uncertain_admission: false,
            parent_ended: false,
            deadline: now + timeout,
            purpose,
            phase: Phase::Submitting,
        };
        // Retain the exact operation before touching the transport: an admission
        // reply can be lost after the runtime has queued the write.
        let finished = self.admit(now, &mut pending, send);
        match finished {
            Ok(true) => {}
            Ok(false) => {
                self.pending = Some(pending);
                self.retry_at = None;
            }
            Err(_) => {
                pending.uncertain_admission = true;
                if matches!(pending.purpose, Purpose::Update { .. }) {
                    self.document = None;
                }
                self.pending = Some(pending);
                self.retry_at = None;
            }
        }
        Ok(())
    }

    fn admit(
        &mut self,
        now: Instant,
        pending: &mut Pending,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        if (now >= pending.deadline || pending.uncertain_admission)
            && let Purpose::Update { parent, .. } = pending.purpose
            && !pending.parent_ended
        {
            // Ending the sponsoring parent first makes a later replay incapable
            // of creating a new write. Runtime dedup still returns an earlier
            // admitted child, even after its parent ends.
            complete(send, parent, failure("settings_outcome_unknown"))?;
            pending.parent_ended = true;
        }
        let response = send(pending.command.clone());
        match response {
            Ok(Response::Accepted { id }) => {
                pending.child = Some(id);
                pending.phase = if pending.parent_ended {
                    Phase::Cancelling("settings_outcome_unknown")
                } else {
                    Phase::Waiting
                };
                Ok(false)
            }
            Ok(Response::Error { code, .. }) if explicit_rejection(&code) => {
                self.retry_at = Some(now + RETRY_DELAY);
                if let Purpose::Update { parent, .. } = pending.purpose {
                    if pending.uncertain_admission {
                        self.document = None;
                    }
                    if !pending.parent_ended {
                        complete(send, parent, failure("configuration_unavailable"))?;
                    }
                }
                Ok(true)
            }
            _ => {
                pending.uncertain_admission = true;
                if matches!(pending.purpose, Purpose::Update { .. }) {
                    self.document = None;
                }
                Ok(false)
            }
        }
    }

    fn advance(
        &mut self,
        now: Instant,
        pending: &mut Pending,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<bool> {
        if matches!(pending.phase, Phase::Submitting) {
            if self.admit(now, pending, send)? {
                return Ok(true);
            }
            if matches!(pending.phase, Phase::Submitting) {
                return Ok(false);
            }
        }
        let child = pending
            .child
            .expect("admitted request owns a child identifier");
        if matches!(pending.phase, Phase::Waiting) {
            let parent_active = if let Purpose::Update { parent, .. } = pending.purpose {
                matches!(
                    send(Command::CheckRequest { request: parent })?,
                    Response::Ok
                )
            } else {
                true
            };
            if now >= pending.deadline || !parent_active {
                pending.phase = Phase::Cancelling("settings_timeout");
                // A write may have committed before cancellation. Reload before opening ingress.
                self.document = None;
            } else {
                let result = send(Command::TakeResult { request: child })?;
                if matches!(result, Response::Pending) {
                    return Ok(false);
                }
                pending.phase = Phase::Releasing(result);
            }
        }
        if let Phase::Cancelling(code) = pending.phase {
            let result = send(Command::Cancel { request: child })?;
            if !released(&result) {
                return Ok(false);
            }
            pending.phase = Phase::Releasing(Response::Error {
                code: code.into(),
                message: "management settings child cancelled".into(),
            });
        }
        if !released(&send(Command::ReleaseResult { request: child })?) {
            if matches!(pending.purpose, Purpose::Update { .. }) {
                self.document = None;
            }
            return Ok(false);
        }
        let Phase::Releasing(result) = &pending.phase else {
            unreachable!("terminal child must be released before applying settings")
        };
        self.finish(now, &pending.purpose, result.clone(), send)?;
        Ok(true)
    }

    fn finish(
        &mut self,
        now: Instant,
        purpose: &Purpose,
        result: Response,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        let document = match &result {
            Response::Result { payload } => parse_document(payload),
            _ => None,
        };
        match purpose {
            Purpose::Load => {
                self.document = document;
                self.retry_at = self.document.is_none().then_some(now + RETRY_DELAY);
            }
            Purpose::Update { parent, desired } => {
                let current_format = matches!(&result, Response::Result { payload } if payload["version"].as_u64() == Some(u64::from(DOCUMENT_VERSION)));
                if current_format && document.as_ref() == Some(desired) {
                    self.document = document;
                    complete(send, *parent, self.payload())?;
                } else {
                    self.document = None;
                    self.retry_at = None;
                    let response = match &result {
                        Response::Result { payload }
                            if payload["error"]["code"] == "revision_conflict" =>
                        {
                            json!({"ok":false,"code":"revision_conflict","current_revision":payload["error"]["current_revision"]})
                        }
                        _ => failure("settings_outcome_unknown"),
                    };
                    complete(send, *parent, response)?;
                }
            }
        }
        Ok(())
    }
}

fn parse_document(payload: &Value) -> Option<Document> {
    if !payload.get("ok")?.as_bool()? {
        return None;
    }
    let revision = payload.get("revision")?.as_u64()?;
    let found = payload.get("found")?.as_bool()?;
    if !found {
        return (revision == 0
            && payload.get("version")?.is_null()
            && payload.get("values")?.as_object()?.is_empty())
        .then(Document::default);
    }
    let version = payload.get("version")?.as_u64()?;
    if !matches!(version, 1 | 2) || revision == 0 {
        return None;
    }
    let values = payload.get("values")?;
    // Version 1 never carried CSRF exceptions. Do not activate an unknown field
    // injected into an older document while migrating its peer selection.
    if version == 1 && values.as_object()?.contains_key("csrf_allowed_origins") {
        return None;
    }
    Some(Document {
        revision,
        values: serde_json::from_value(values.clone()).ok()?,
    })
}

fn document_payload(document: &Document) -> Value {
    json!({"ok":true,"revision":document.revision,"values":document.values,"origin_web_ui_allowed":document.values.origin_web_ui_allowed.unwrap_or_default(),"csrf_allowed_origins":document.values.csrf_allowed_origins.as_ref().map(CsrfOrigins::as_slice).unwrap_or_default()})
}

fn failure(code: &str) -> Value {
    json!({"ok":false,"code":code})
}

fn released(response: &Response) -> bool {
    matches!(response, Response::Ok)
        || matches!(response, Response::Error { code, .. } if code == "not_found" || code == "request_ended")
}

fn explicit_rejection(code: &str) -> bool {
    matches!(
        code,
        "permission_denied" | "invalid_request" | "unavailable" | "resource_exhausted"
    )
}

fn complete(
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    parent: u64,
    payload: Value,
) -> ModuleResult<()> {
    if released(&send(Command::Complete {
        request: parent,
        payload,
    })?) {
        Ok(())
    } else {
        Err("management settings completion failed".into())
    }
}

#[cfg(test)]
mod tests;
