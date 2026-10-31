//! Broker-backed shortcut discovery for an already authenticated session target.
//! Browser or input payload fields are never accepted as target authority here.
use amaterasu_extension_api::{
    ActionDescriptor, BindingSource, BindingTarget, ExecutionClass, ExtensionDescriptor,
    Keybinding, LifecycleTarget, OptionPresentation, TargetKind, validate_descriptor,
    validate_keybinding,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const SERVICE: &str = "amaterasu.extension-registry/1";
const MAX_PROVIDERS: usize = 128;
const MAX_BINDINGS: usize = 128;

/// Construct only from lifecycle-authenticated facts, never an input request.
pub(crate) struct CanonicalContext {
    pub session_id: String,
    pub target: LifecycleTarget,
    pub input_binding: String,
    pub application: Option<String>,
    pub paired_client: Option<String>,
    pub context: String,
}

pub(crate) struct ApprovedBinding {
    pub binding: Keybinding,
    pub action: ActionDescriptor,
}

#[derive(Deserialize)]
struct Page {
    ok: bool,
    revision: u64,
    items: Vec<ExtensionDescriptor>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct Settings {
    ok: bool,
    revision: u64,
    effective: std::collections::BTreeMap<String, Effective>,
}
#[derive(Deserialize)]
struct Effective {
    value: Value,
}

enum Stage {
    List {
        after: Option<String>,
        revision: Option<u64>,
    },
    Settings {
        index: usize,
    },
    Done,
}

pub(crate) struct DiscoveryJob {
    parent: u64,
    context: CanonicalContext,
    deadline: Instant,
    instance: String,
    sequence: u64,
    pending: Option<u64>,
    stage: Stage,
    descriptors: Vec<ExtensionDescriptor>,
    providers: BTreeSet<String>,
    cursors: BTreeSet<String>,
    binding_ids: BTreeSet<String>,
    bindings: Vec<ApprovedBinding>,
    approval_revision: Option<u64>,
    settings_revisions: BTreeMap<String, u64>,
}

impl DiscoveryJob {
    pub fn new(parent: u64, context: CanonicalContext, timeout_ms: u64) -> Result<Self, String> {
        let valid = |value: &str| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        };
        if !valid(&context.session_id)
            || !valid(&context.input_binding)
            || !valid(&context.context)
            || context.paired_client.is_some()
            || !matches!(&context.target, LifecycleTarget::Stream { session_id, .. } if session_id == &context.session_id)
            || context.application.as_deref().is_some_and(|id| !valid(id))
            || context
                .paired_client
                .as_deref()
                .is_some_and(|id| !valid(id))
            || timeout_ms == 0
        {
            return Err("invalid authenticated shortcut context".into());
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "shortcut discovery identity unavailable")?;
        Ok(Self {
            parent,
            context,
            deadline: Instant::now() + Duration::from_millis(timeout_ms.min(30_000)),
            instance: random.iter().map(|byte| format!("{byte:02x}")).collect(),
            sequence: 0,
            pending: None,
            stage: Stage::List {
                after: None,
                revision: None,
            },
            descriptors: Vec::new(),
            providers: BTreeSet::new(),
            cursors: BTreeSet::new(),
            binding_ids: BTreeSet::new(),
            bindings: Vec::new(),
            approval_revision: None,
            settings_revisions: BTreeMap::new(),
        })
    }

    pub fn approval_revision(&self) -> Option<u64> {
        self.approval_revision
    }
    pub fn settings_revisions(&self) -> &BTreeMap<String, u64> {
        &self.settings_revisions
    }

    /// Drive outside the input callback. A completed result is consumed once.
    pub fn advance(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<Option<Vec<ApprovedBinding>>, String> {
        if Instant::now() >= self.deadline {
            if let Some(request) = self.pending.take() {
                let _ = send(Command::Cancel { request });
                let _ = send(Command::ReleaseResult { request });
            }
            return Err("shortcut discovery deadline exceeded".into());
        }
        if let Some(request) = self.pending {
            match send(Command::TakeResult { request })? {
                Response::Pending => return Ok(None),
                Response::Result { payload } => {
                    match send(Command::ReleaseResult { request })? {
                        Response::Ok => (),
                        _ => return Err("shortcut discovery result release failed".into()),
                    }
                    self.pending = None;
                    self.accept(payload)?;
                }
                _ => return Err("shortcut discovery request failed".into()),
            }
        }
        let payload = match &self.stage {
            Stage::List { after, revision } => {
                json!({"operation":"list","after":after,"revision":revision,"limit":32})
            }
            Stage::Settings { index } => {
                let Some(descriptor) = self.descriptors.get(*index) else {
                    self.stage = Stage::Done;
                    return Ok(Some(std::mem::take(&mut self.bindings)));
                };
                json!({"operation":"settings_get","provider":descriptor.provider,
                    "scope":{"kind":"session","id":self.context.session_id},
                    "application":self.context.application,"paired_client":null,"session":self.context.session_id,
                    "target":self.context.target,"input_binding":self.context.input_binding})
            }
            Stage::Done => return Err("shortcut discovery already consumed".into()),
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("shortcut discovery sequence exhausted")?;
        let timeout_ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u64;
        match send(Command::Request {
            service: SERVICE.into(),
            payload,
            timeout_ms: timeout_ms.max(1),
            operation_id: format!("shortcut-discovery-{}-{}", self.instance, self.sequence),
            parent: Some(self.parent),
        })? {
            Response::Accepted { id } => self.pending = Some(id),
            _ => return Err("shortcut discovery request unavailable".into()),
        }
        Ok(None)
    }

    fn accept(&mut self, payload: Value) -> Result<(), String> {
        match &self.stage {
            Stage::List { revision, .. } => {
                let page: Page = serde_json::from_value(payload)
                    .map_err(|_| "invalid shortcut registry page")?;
                if !page.ok
                    || revision.is_some_and(|expected| expected != page.revision)
                    || page.items.len() > 32
                {
                    return Err("shortcut registry changed or denied discovery".into());
                }
                self.approval_revision = Some(page.revision);
                for descriptor in page.items {
                    validate_descriptor(&descriptor)?;
                    if !self.providers.insert(descriptor.provider.clone())
                        || self.providers.len() > MAX_PROVIDERS
                    {
                        return Err("duplicate or excessive shortcut providers".into());
                    }
                    if descriptor
                        .options
                        .iter()
                        .any(|option| option.presentation == Some(OptionPresentation::Keybinding))
                    {
                        self.descriptors.push(descriptor);
                    }
                }
                if let Some(after) = page.next {
                    if after.is_empty() || !self.cursors.insert(after.clone()) {
                        return Err("invalid registry cursor".into());
                    }
                    self.stage = Stage::List {
                        after: Some(after),
                        revision: Some(page.revision),
                    };
                } else {
                    self.stage = Stage::Settings { index: 0 };
                }
            }
            Stage::Settings { index } => {
                let settings: Settings =
                    serde_json::from_value(payload).map_err(|_| "invalid shortcut settings")?;
                if !settings.ok {
                    return Err("shortcut settings unavailable".into());
                }
                let descriptor = self
                    .descriptors
                    .get(*index)
                    .ok_or("missing shortcut descriptor")?;
                self.settings_revisions
                    .insert(descriptor.provider.clone(), settings.revision);
                for option in descriptor
                    .options
                    .iter()
                    .filter(|option| option.presentation == Some(OptionPresentation::Keybinding))
                {
                    if option
                        .required_capabilities
                        .iter()
                        .any(|capability| !descriptor.capabilities.contains(capability))
                    {
                        return Err("shortcut capability unavailable".into());
                    }
                    let value = settings
                        .effective
                        .get(&option.id)
                        .ok_or("missing effective shortcut binding")?;
                    let fields = [(option.id.clone(), option.definition.clone())]
                        .into_iter()
                        .collect();
                    let values = [(option.id.clone(), value.value.clone())]
                        .into_iter()
                        .collect();
                    amaterasu_configuration_schema::validate_values(&fields, &values)
                        .map_err(|_| "shortcut value violates declared schema")?;
                    let binding: Keybinding = serde_json::from_value(value.value.clone())
                        .map_err(|_| "invalid shortcut binding")?;
                    if !binding.enabled
                        || binding.source != BindingSource::SessionInput
                        || binding.context != self.context.context
                    {
                        continue;
                    }
                    if binding.target != BindingTarget::CurrentSession {
                        return Err("remote shortcut cannot select host target".into());
                    }
                    let action = descriptor
                        .actions
                        .iter()
                        .find(|action| action.id == binding.action_id)
                        .ok_or("shortcut action unavailable")?;
                    if !action.allowed_targets.contains(&TargetKind::Session)
                        || !matches!(
                            action.execution,
                            ExecutionClass::ReadOnly | ExecutionClass::SessionMutation
                        )
                        || action
                            .required_capabilities
                            .iter()
                            .any(|capability| !descriptor.capabilities.contains(capability))
                    {
                        return Err("shortcut action is not authorized for sessions".into());
                    }
                    validate_keybinding(&binding, action)?;
                    if !self.binding_ids.insert(binding.id.clone())
                        || self.bindings.len() >= MAX_BINDINGS
                    {
                        return Err("duplicate or excessive shortcut bindings".into());
                    }
                    self.bindings.push(ApprovedBinding {
                        binding,
                        action: action.clone(),
                    });
                }
                self.stage = Stage::Settings { index: index + 1 };
            }
            Stage::Done => return Err("unexpected shortcut discovery response".into()),
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    fn schema(value: &Value) -> Value {
        match value {
            Value::Bool(default) => json!({"type":"boolean","default":default}),
            Value::Number(default) => {
                json!({"type":"integer","default":default,"minimum":null,"maximum":null})
            }
            Value::String(default) => {
                json!({"type":"string","default":default,"min_length":null,"max_length":null})
            }
            Value::Array(default) => {
                json!({"type":"list","default":default,"item":{"type":"integer","default":0,"minimum":null,"maximum":null},"min_items":null,"max_items":null})
            }
            Value::Object(default) => {
                json!({"type":"object","default":default,"fields":default.iter().map(|(k,v)| (k.clone(),schema(v))).collect::<serde_json::Map<String,Value>>()})
            }
            Value::Null => panic!("unsupported test default"),
        }
    }
    pub(crate) fn binding() -> Value {
        json!({"id":"binding","enabled":true,"source":"session_input","context":"active","trigger":{"platform":"linux_evdev","code":30},"modifiers":[],"propagation":"consume_trigger","action_id":"provider/action","arguments":{},"target":"current_session"})
    }
    pub(crate) fn descriptor() -> Value {
        let method = json!({"service":"provider.api/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1});
        json!({"provider":"provider","revision":1,"options":[{"id":"shortcut","label":"Shortcut","help":"","definition":schema(&binding()),"presentation":"keybinding","allowed_scopes":["host","session"],"apply_policy":"next_session","required_capabilities":[]}],"actions":[{"id":"provider/action","label":"Action","description":"","execution":"read_only","arguments":{},"allowed_targets":["session"],"required_capabilities":[],"plan":null,"execute":method,"status":null}],"capabilities":[],"settings_validate":method})
    }
    fn new_job() -> DiscoveryJob {
        DiscoveryJob::new(
            10,
            CanonicalContext {
                session_id: "session".into(),
                target: LifecycleTarget::Stream {
                    host_epoch: "host".into(),
                    session_id: "session".into(),
                    stream_id: "stream".into(),
                    generation: 1,
                },
                input_binding: "token".into(),
                application: None,
                paired_client: None,
                context: "active".into(),
            },
            10000,
        )
        .unwrap()
    }
    #[test]
    fn approved_binding_uses_canonical_session_and_parent_requests() {
        let mut job = new_job();
        let mut pending = Value::Null;
        let mut send = |command: Command| -> ModuleResult<Response> {
            match command {
                Command::Request {
                    payload, parent, ..
                } => {
                    assert_eq!(parent, Some(10));
                    pending = payload;
                    Ok(Response::Accepted { id: 1 })
                }
                Command::TakeResult { .. } if pending["operation"] == "list" => {
                    Ok(Response::Result {
                        payload: json!({"ok":true,"revision":2,"items":[descriptor()],"next":null}),
                    })
                }
                Command::TakeResult { .. } => {
                    assert_eq!(pending["scope"], json!({"kind":"session","id":"session"}));
                    assert_eq!(
                        pending["target"],
                        json!({"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":1})
                    );
                    assert_eq!(pending["input_binding"], "token");
                    assert!(pending["paired_client"].is_null());
                    Ok(Response::Result {
                        payload: json!({"ok":true,"revision":1,"effective":{"shortcut":{"value":binding(),"source":null}}}),
                    })
                }
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                _ => panic!("unexpected command"),
            }
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        let found = job.advance(&mut send).unwrap().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].binding.action_id, "provider/action");
        assert_eq!(found[0].action.id, "provider/action");
    }
    #[test]
    fn discovery_rejects_unverified_paired_client_and_wrong_session_target() {
        let context = |target, paired_client| CanonicalContext {
            session_id: "session".into(),
            target,
            input_binding: "token".into(),
            application: None,
            paired_client,
            context: "active".into(),
        };
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        assert!(DiscoveryJob::new(10, context(target, Some("client".into())), 1000).is_err());
        let wrong = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "other".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        assert!(DiscoveryJob::new(10, context(wrong, None), 1000).is_err());
    }

    #[test]
    fn rejects_changed_pages_missing_action_and_host_target() {
        let mut job = new_job();
        job.accept(json!({"ok":true,"revision":1,"items":[],"next":"next"}))
            .unwrap();
        assert!(
            job.accept(json!({"ok":true,"revision":2,"items":[],"next":null}))
                .is_err()
        );
        for bad in ["missing", "host"] {
            let mut job = new_job();
            job.accept(json!({"ok":true,"revision":1,"items":[descriptor()],"next":null}))
                .unwrap();
            let mut value = binding();
            if bad == "missing" {
                value["action_id"] = json!("provider/missing");
            } else {
                value["target"] = json!("host");
            }
            assert!(
                job.accept(
                    json!({"ok":true,"revision":1,"effective":{"shortcut":{"value":value}}})
                )
                .is_err()
            );
        }
    }
}
