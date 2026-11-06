//! Replaceable per-client catalog presentation and application-independent streams.
//!
//! The protocol adapter supplies authenticated client facts and application
//! observations. The selected provider owns product policy and synthetic entry
//! identities. Plans describe requirements; they never select native providers
//! or confer authority. Trusted startup composition grants and selects those.
//!
//! Catalogs cross the broker in bounded projection transactions. Providers must
//! bind a transaction to its broker caller, client, complete context and catalog
//! revision, cap retained transactions and expire abandoned work. Consumers must
//! release on completion, cancellation, revoked trust and timeout, and recheck
//! the context before delivery. A projected list is never launch authorization.
use crate::requests::RequestOptions;
use crate::{ApiFieldKind, ApiMethod, ApiSchema, Command, ModuleResult, operation_api};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const SERVICE: &str = "nonary.session-presentation/1";
pub const MAX_PAGE_ENTRIES: usize = 32;
pub const MAX_CATALOG_ENTRIES: usize = 16_384;
pub const MAX_PROJECTIONS: usize = 4;
pub const PROJECTION_TIMEOUT_MS: u64 = 30_000;
pub const MAX_PAYLOAD_BYTES: usize = 12_000;
pub const MAX_ARTWORK_BYTES: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    /// Stable identity derived from authenticated transport, never a query ID.
    pub id: String,
    pub paired: bool,
    pub may_view: bool,
    pub may_launch: bool,
    pub may_terminate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Application {
    /// Opaque presentation identity. Numeric protocol conversion is adapter-owned.
    pub id: String,
    pub uuid: String,
    pub title: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default = "visible_by_default")]
    pub visible: bool,
    #[serde(default)]
    pub monitor_profile: bool,
    /// Bounded opaque catalog metadata; the provider freezes profile policy at
    /// acceptance rather than rereading a possibly edited configured entry.
    #[serde(default = "empty_properties")]
    pub properties: Value,
}

fn visible_by_default() -> bool {
    true
}
fn empty_properties() -> Value {
    json!({})
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub application: Application,
    pub owner_client_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub client: Client,
    pub game: Option<Game>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    pub busy: bool,
    pub current_game: Option<String>,
    pub current_game_uuid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub id: String,
    pub uuid: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureSource {
    /// An explicitly composed synthetic capture provider; never host capture.
    SyntheticBlack,
    /// A logical exact selector resolved and verified by the capture provider.
    /// This is not an OS handle, device path or permission to choose a provider.
    ExactOutput { output: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuxiliaryStreamPlan {
    pub session_id: String,
    pub generation: u64,
    /// Provider-held admission reference, retained before consumer effects.
    pub reservation_id: String,
    pub capture: CaptureSource,
    pub mode: Mode,
    pub stream_audio: bool,
    pub host_audio: bool,
}

/// A normal application transport borrowing an already retained exact source.
/// Its lifecycle must not reconfigure or release that retained source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedCapture {
    pub session_id: String,
    pub generation: u64,
    pub reservation_id: String,
    pub capture: CaptureSource,
    pub mode: Mode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlCompletion {
    /// Protocol-level action completion, emitted only after actual effects.
    pub status_code: u16,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    LaunchApplication {
        application_id: String,
        #[serde(default)]
        source: Option<SharedCapture>,
    },
    ResumeApplication {
        application_id: String,
        #[serde(default)]
        source: Option<SharedCapture>,
    },
    ReplaceApplication {
        application_id: String,
        expected_current_generation: u64,
        #[serde(default)]
        source: Option<SharedCapture>,
    },
    StopApplication {
        application_id: String,
        generation: u64,
        completion: ControlCompletion,
    },
    Auxiliary {
        plan: AuxiliaryStreamPlan,
    },
    StopAuxiliary {
        session_id: String,
        generation: u64,
        completion: ControlCompletion,
    },
    Complete {
        status_code: u16,
        message: String,
    },
    Reject {
        status_code: u16,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchIntent {
    Launch,
    Resume,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEvent {
    StreamStarted,
    /// Send after this exact transport's native resources have retired.
    StreamEnded,
    ClientDisconnected,
    /// Send after all transports borrowing the exact session have retired. The
    /// provider acknowledges only after its retained resources are also gone.
    ExplicitDisconnect,
    Unpaired,
    Shutdown,
    FailedStart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub client_id: String,
    pub session_id: String,
    pub generation: u64,
    pub reservation_id: String,
    /// Distinct RTSP/transport operation identity; reconnects retain session_id
    /// and generation but receive a fresh transport identity.
    pub transport_id: String,
    pub event: SessionEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Artwork {
    /// Re-resolve the original application under current catalog authority.
    Application {
        application_id: String,
    },
    InlinePng {
        bytes: Vec<u8>,
    },
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Describe {},
    BeginProject {
        context: Context,
        catalog_revision: String,
    },
    AppendProject {
        client_id: String,
        projection_id: String,
        entries: Vec<Application>,
        final_page: bool,
    },
    ReadProject {
        client_id: String,
        projection_id: String,
        offset: usize,
        limit: usize,
    },
    ReleaseProject {
        client_id: String,
        projection_id: String,
    },
    Info {
        context: Context,
    },
    Dispatch {
        context: Context,
        /// Stable client-scoped transition identity. Poll with this same value
        /// and unchanged arguments after Pending; never repeat an effect.
        operation_id: String,
        intent: DispatchIntent,
        selection: Option<Selection>,
        mode: Option<Mode>,
    },
    CancelDispatch {
        client_id: String,
        operation_id: String,
    },
    VerifySource {
        client_id: String,
        source: SharedCapture,
    },
    Observe {
        client_id: String,
        session_id: String,
        generation: u64,
        reservation_id: String,
        transport_id: String,
        event: SessionEvent,
    },
    Artwork {
        context: Context,
        selection: Selection,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Described {
        interface_major: u32,
        interface_minor: u32,
    },
    Started {
        projection_id: String,
    },
    Appended {
        complete: bool,
    },
    Projected {
        presentation: Presentation,
        entries: Vec<Application>,
        next_offset: Option<usize>,
    },
    Info {
        presentation: Presentation,
    },
    Dispatched {
        decision: Decision,
    },
    Verified {
        source: SharedCapture,
    },
    Pending {
        retry_after_ms: u64,
    },
    Acknowledged {},
    Artwork {
        artwork: Artwork,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Exact installed API methods. The package approves these schemas for its
/// selected provider and consumer; native table version is not the identity.
pub fn methods() -> Vec<ApiMethod> {
    use ApiFieldKind::{Any, Array, Boolean, Integer, Object, String as Text};
    [
        ("describe", vec![]),
        (
            "begin_project",
            vec![("context", Object), ("catalog_revision", Text)],
        ),
        (
            "append_project",
            vec![
                ("client_id", Text),
                ("projection_id", Text),
                ("entries", Array),
                ("final_page", Boolean),
            ],
        ),
        (
            "read_project",
            vec![
                ("client_id", Text),
                ("projection_id", Text),
                ("offset", Integer),
                ("limit", Integer),
            ],
        ),
        (
            "release_project",
            vec![("client_id", Text), ("projection_id", Text)],
        ),
        ("info", vec![("context", Object)]),
        (
            "dispatch",
            vec![
                ("context", Object),
                ("operation_id", Text),
                ("intent", Text),
                ("selection", Any),
                ("mode", Any),
            ],
        ),
        (
            "cancel_dispatch",
            vec![("client_id", Text), ("operation_id", Text)],
        ),
        (
            "verify_source",
            vec![("client_id", Text), ("source", Object)],
        ),
        (
            "observe",
            vec![
                ("client_id", Text),
                ("session_id", Text),
                ("generation", Integer),
                ("reservation_id", Text),
                ("transport_id", Text),
                ("event", Text),
            ],
        ),
        ("artwork", vec![("context", Object), ("selection", Object)]),
    ]
    .into_iter()
    .map(|(name, required)| {
        let mut method = operation_api::method(SERVICE, name, &required, &[]);
        method.response = ApiSchema {
            required: BTreeMap::from([("result".into(), Text)]),
            optional: BTreeMap::new(),
            allow_extra: true,
        };
        method
    })
    .collect()
}

/// Produce an exact RequestApi invocation. No raw-operation route or built-in
/// fallback is provided when the selected implementation is unavailable.
pub fn command(options: RequestOptions<'_>, request: &Request) -> ModuleResult<Command> {
    if options.service != SERVICE {
        return Err("session_presentation_service_mismatch".into());
    }
    request.validate()?;
    let mut arguments = serde_json::to_value(request).map_err(|e| e.to_string())?;
    let method = arguments
        .as_object_mut()
        .and_then(|value| value.remove("action"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("invalid_session_presentation_request")?;
    Ok(Command::CallApi {
        service: SERVICE.into(),
        method,
        interface_major: 1,
        interface_minor: 0,
        payload: arguments,
        timeout_ms: options.timeout_ms,
        operation_id: options.operation_id.into(),
        parent: options.parent,
    })
}

/// Decode only an API-approved delivery. The runtime supplies the caller and
/// service; providers still authorize callers and authenticated client scope.
pub fn decode(service: &str, payload: Value) -> ModuleResult<Request> {
    if service != SERVICE {
        return Err("session_presentation_service_mismatch".into());
    }
    if payload.get("method").is_none() {
        return Err("session_presentation_requires_api".into());
    }
    let request: Request = serde_json::from_value(operation_api::decode(payload, &methods())?)
        .map_err(|_| "invalid_session_presentation_request")?;
    request.validate()?;
    Ok(request)
}

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}
fn bounded_payload(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_PAYLOAD_BYTES)
}

impl Client {
    pub fn validate(&self) -> Result<(), &'static str> {
        if token(&self.id) && self.paired {
            Ok(())
        } else {
            Err("unauthenticated_client")
        }
    }
}
impl Application {
    pub fn validate(&self) -> Result<(), &'static str> {
        if token(&self.id)
            && self.uuid.len() <= 128
            && !self.uuid.chars().any(char::is_control)
            && bounded_text(&self.title, 160)
            && self.aliases.len() <= 16
            && self.aliases.iter().all(|alias| token(alias))
            && self.properties.is_object()
            && serde_json::to_vec(&self.properties).is_ok_and(|bytes| bytes.len() <= 4096)
        {
            Ok(())
        } else {
            Err("invalid_presentation_application")
        }
    }
}
impl Context {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.client.validate()?;
        if let Some(game) = &self.game {
            game.application.validate()?;
            if game.generation == 0 || !token(&game.owner_client_id) {
                return Err("invalid_game_snapshot");
            }
        }
        Ok(())
    }
}
impl Mode {
    pub fn validate(&self) -> Result<(), &'static str> {
        if (1..=16_384).contains(&self.width)
            && (1..=16_384).contains(&self.height)
            && (10_000..=1_000_000).contains(&self.refresh_millihz)
        {
            Ok(())
        } else {
            Err("invalid_session_mode")
        }
    }
}
impl CaptureSource {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::SyntheticBlack => Ok(()),
            Self::ExactOutput { output } if bounded_text(output, 1024) => Ok(()),
            _ => Err("invalid_capture_source"),
        }
    }
}
impl AuxiliaryStreamPlan {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.capture.validate()?;
        self.mode.validate()?;
        if !token(&self.session_id)
            || self.generation == 0
            || !token(&self.reservation_id)
            || (matches!(self.capture, CaptureSource::SyntheticBlack)
                && (self.stream_audio || self.host_audio))
        {
            Err("invalid_auxiliary_stream_plan")
        } else {
            Ok(())
        }
    }
}
impl SharedCapture {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.capture.validate()?;
        self.mode.validate()?;
        if token(&self.session_id)
            && self.generation > 0
            && token(&self.reservation_id)
            && matches!(self.capture, CaptureSource::ExactOutput { .. })
        {
            Ok(())
        } else {
            Err("invalid_shared_capture")
        }
    }
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !bounded_payload(self) {
            return Err("session_presentation_payload_too_large");
        }
        match self {
            Self::Describe {} => {}
            Self::BeginProject {
                context,
                catalog_revision,
            } => {
                context.validate()?;
                if !token(catalog_revision) {
                    return Err("invalid_catalog_revision");
                }
            }
            Self::AppendProject {
                client_id,
                projection_id,
                entries,
                ..
            } => {
                if !token(client_id) || !token(projection_id) || entries.len() > MAX_PAGE_ENTRIES {
                    return Err("invalid_projection_page");
                }
                for entry in entries {
                    entry.validate()?;
                }
            }
            Self::ReadProject {
                client_id,
                projection_id,
                offset,
                limit,
            } => {
                if !token(client_id)
                    || !token(projection_id)
                    || *offset > MAX_CATALOG_ENTRIES + 32
                    || !(1..=MAX_PAGE_ENTRIES).contains(limit)
                {
                    return Err("invalid_projection_read");
                }
            }
            Self::ReleaseProject {
                client_id,
                projection_id,
            } => {
                if !token(client_id) || !token(projection_id) {
                    return Err("invalid_projection_identity");
                }
            }
            Self::Info { context } => {
                context.validate()?;
            }
            Self::Dispatch {
                context,
                operation_id,
                intent,
                selection,
                mode,
            } => {
                context.validate()?;
                if !token(operation_id) {
                    return Err("invalid_dispatch_operation");
                }
                if let Some(selection) = selection {
                    selection.validate()?;
                }
                if let Some(mode) = mode {
                    mode.validate()?;
                }
                if (*intent == DispatchIntent::Launch && selection.is_none())
                    || (*intent != DispatchIntent::Cancel && mode.is_none())
                {
                    return Err("invalid_session_dispatch");
                }
            }
            Self::CancelDispatch {
                client_id,
                operation_id,
            } => {
                if !token(client_id) || !token(operation_id) {
                    return Err("invalid_dispatch_operation");
                }
            }
            Self::VerifySource { client_id, source } => {
                if !token(client_id) {
                    return Err("invalid_source_owner");
                }
                source.validate()?;
            }
            Self::Observe {
                client_id,
                session_id,
                generation,
                reservation_id,
                transport_id,
                ..
            } => {
                if !token(client_id)
                    || !token(session_id)
                    || *generation == 0
                    || !token(reservation_id)
                    || !token(transport_id)
                {
                    return Err("invalid_session_observation");
                }
            }
            Self::Artwork { context, selection } => {
                context.validate()?;
                selection.validate()?;
            }
        }
        Ok(())
    }
}
impl Selection {
    pub fn validate(&self) -> Result<(), &'static str> {
        if token(&self.id) && self.uuid.as_ref().is_none_or(|uuid| token(uuid)) {
            Ok(())
        } else {
            Err("invalid_presentation_selection")
        }
    }
}

impl ControlCompletion {
    pub fn validate(&self) -> Result<(), &'static str> {
        if (100..=599).contains(&self.status_code) && bounded_text(&self.message, 512) {
            Ok(())
        } else {
            Err("invalid_control_completion")
        }
    }
}
impl Decision {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::LaunchApplication {
                application_id,
                source,
            }
            | Self::ResumeApplication {
                application_id,
                source,
            } => {
                if !token(application_id) {
                    return Err("invalid_application_decision");
                }
                if let Some(source) = source {
                    source.validate()?;
                }
            }
            Self::ReplaceApplication {
                application_id,
                expected_current_generation,
                source,
            } => {
                if !token(application_id) || *expected_current_generation == 0 {
                    return Err("invalid_application_decision");
                }
                if let Some(source) = source {
                    source.validate()?;
                }
            }
            Self::StopApplication {
                application_id,
                generation,
                completion,
            } => {
                if !token(application_id) || *generation == 0 {
                    return Err("invalid_application_decision");
                }
                completion.validate()?;
            }
            Self::Auxiliary { plan } => plan.validate()?,
            Self::StopAuxiliary {
                session_id,
                generation,
                completion,
            } => {
                if !token(session_id) || *generation == 0 {
                    return Err("invalid_auxiliary_decision");
                }
                completion.validate()?;
            }
            Self::Complete {
                status_code,
                message,
            }
            | Self::Reject {
                status_code,
                message,
            } => {
                ControlCompletion {
                    status_code: *status_code,
                    message: message.clone(),
                }
                .validate()?;
            }
        }
        Ok(())
    }
}
impl Presentation {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.current_game.as_ref().is_some_and(|id| !token(id))
            || self
                .current_game_uuid
                .as_ref()
                .is_some_and(|uuid| !token(uuid))
            || self.busy != self.current_game.is_some()
            || (self.current_game.is_none() && self.current_game_uuid.is_some())
        {
            Err("invalid_session_presentation")
        } else {
            Ok(())
        }
    }
}
impl Reply {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !bounded_payload(self) {
            return Err("session_presentation_payload_too_large");
        }
        match self {
            Self::Described {
                interface_major: 1,
                interface_minor: 0,
            } => {}
            Self::Described { .. } => return Err("unsupported_session_presentation_version"),
            Self::Started { projection_id } if !token(projection_id) => {
                return Err("invalid_projection_identity");
            }
            Self::Projected {
                presentation,
                entries,
                next_offset,
            } => {
                presentation.validate()?;
                if entries.len() > MAX_PAGE_ENTRIES
                    || next_offset.is_some_and(|offset| offset > MAX_CATALOG_ENTRIES + 32)
                    || (entries.is_empty() && next_offset.is_some())
                {
                    return Err("invalid_projection_page");
                }
                for entry in entries {
                    entry.validate()?;
                }
            }
            Self::Info { presentation } => presentation.validate()?,
            Self::Dispatched { decision } => decision.validate()?,
            Self::Verified { source } => source.validate()?,
            Self::Pending { retry_after_ms } if !(1..=1000).contains(retry_after_ms) => {
                return Err("invalid_session_retry");
            }
            Self::Artwork { artwork } => match artwork {
                Artwork::Application { application_id } if !token(application_id) => {
                    return Err("invalid_artwork_reference");
                }
                Artwork::InlinePng { bytes }
                    if bytes.len() > MAX_ARTWORK_BYTES
                        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") =>
                {
                    return Err("invalid_inline_artwork");
                }
                _ => {}
            },
            Self::Error { code, message } if !token(code) || !bounded_text(message, 512) => {
                return Err("invalid_session_error");
            }
            _ => {}
        }
        Ok(())
    }
}

/// Validate a selected provider reply before applying any consumer effects.
pub fn decode_reply(payload: Value) -> ModuleResult<Reply> {
    let reply: Reply =
        serde_json::from_value(payload).map_err(|_| "invalid_session_presentation_reply")?;
    reply.validate()?;
    Ok(reply)
}

#[cfg(test)]
mod tests;
