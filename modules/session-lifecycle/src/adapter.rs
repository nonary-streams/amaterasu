//! Broker adapter for the lifecycle authority. Request payloads do not establish identity.
use crate::prepare_authority::{ControlPolicy, DomainScope, FinalizePolicy};
use amaterasu_extension_api::{ConsumerRequirements, MethodRef, ResourceAccess, SchemaRef};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SERVICE: &str = "amaterasu.session-lifecycle/1";

/// Receives identities assigned by the broker, never identity fields from a request.
pub trait LifecycleHandler: Sized {
    /// Opens and recovers durable authority state before service registration.
    fn from_settings(settings: &LifecycleSettings) -> ModuleResult<Self>;

    fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value;

    /// Advance bounded background work without waiting for a provider. Implementations
    /// must persist an effect's identity and intent before submitting it, and fence
    /// outcomes that cannot be confirmed. Detached broker calls run as this service,
    /// not as the initiator of an earlier request.
    fn advance(
        &mut self,
        _send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        Ok(())
    }
}

/// Installed policy; request payloads cannot supply or amend this selection.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleSettings {
    pub trusted_callers: BTreeSet<String>,
    /// Broker identities granted delegation by installed operator policy.
    pub session_delegates: BTreeSet<String>,
    pub input_actions: BTreeSet<String>,
    pub action_service: String,
    /// Broker identity allowed to attest settings-scope request sources.
    pub settings_registry_caller: String,
    /// Optional registry service used to freeze provider settings at session start.
    pub settings_registry_service: Option<String>,
    pub journal_path: String,
    pub operation_grants: BTreeMap<String, BTreeSet<String>>,
    pub participants: Vec<ParticipantSettings>,
    /// Installed owner-mediated control grants; absent means no control is allowed.
    pub control_policies: Vec<ControlPolicy>,
    /// Exact owner finalization grants, pinned independently from control grants.
    pub finalize_policies: Vec<FinalizePolicy>,
    /// Exact UTF-8 schema documents supplied by installed signed policy.
    pub schema_documents: Vec<SchemaDocument>,
    pub consumers: ConsumerSettings,
    pub consumer_requirements: ConsumerRequirements,
}

impl Default for LifecycleSettings {
    fn default() -> Self {
        Self {
            trusted_callers: BTreeSet::new(),
            session_delegates: BTreeSet::new(),
            input_actions: BTreeSet::new(),
            action_service: "amaterasu.extension-actions".into(),
            settings_registry_caller: String::new(),
            settings_registry_service: None,
            journal_path: String::new(),
            operation_grants: BTreeMap::new(),
            participants: Vec::new(),
            control_policies: Vec::new(),
            finalize_policies: Vec::new(),
            schema_documents: Vec::new(),
            consumers: ConsumerSettings::default(),
            consumer_requirements: ConsumerRequirements {
                execution_domain: String::new(),
                capture_providers: Vec::new(),
                selector_schemas: Vec::new(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SchemaDocument {
    pub reference: SchemaRef,
    pub document: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ParticipantSettings {
    pub provider: String,
    /// Required participants that must prepare before this provider.
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub plan_method: MethodSettings,
    pub callback_method: MethodSettings,
    pub status_method: MethodSettings,
    #[serde(default)]
    pub binding_validate_method: Option<MethodSettings>,
    pub capabilities: BTreeSet<String>,
    pub cleanup_methods: Vec<MethodRef>,
    pub settings_revision: String,
    pub settings: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MethodSettings {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub schema_revision: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ConsumerSettings {
    pub capture: BTreeSet<String>,
    pub input: BTreeSet<String>,
}

fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-' | b'/')
        })
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn valid_schema_reference(schema: &SchemaRef) -> bool {
    valid_opaque_id(&schema.id)
        && schema.major != 0
        && schema.digest.len() == 64
        && schema
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_control_method(method: &MethodRef, service: &str) -> bool {
    method.service == service
        && valid_identifier(&method.method)
        && method.interface_major == 2
        && method.schema_revision != 0
}

fn valid_domain_scope(scope: DomainScope, domains: &BTreeSet<String>) -> bool {
    domains.len() <= 32
        && domains.iter().all(|domain| valid_opaque_id(domain))
        && match scope {
            DomainScope::Static => !domains.is_empty(),
            DomainScope::Resource => domains.is_empty(),
        }
}

impl LifecycleSettings {
    /// Compile only policy-supplied local documents; no runtime fetch or request
    /// payload can add a schema. Every control and finalization pin must resolve exactly.
    pub(crate) fn schema_catalog(
        &self,
    ) -> ModuleResult<amaterasu_extension_api::schemas::SchemaCatalog> {
        if self.schema_documents.len() > 32
            || self
                .schema_documents
                .iter()
                .any(|entry| entry.document.len() > 12_000)
            || self
                .schema_documents
                .iter()
                .map(|entry| entry.document.len())
                .sum::<usize>()
                > 12_000
        {
            return Err("lifecycle schema document limit exceeded".into());
        }
        let catalog = amaterasu_extension_api::schemas::SchemaCatalog::compile(
            self.schema_documents
                .iter()
                .map(|entry| (entry.reference.clone(), entry.document.as_str())),
        )?;
        if self.control_policies.iter().any(|policy| {
            !catalog.contains(&policy.argument_schema) || !catalog.contains(&policy.output_schema)
        }) || self
            .finalize_policies
            .iter()
            .any(|policy| !catalog.contains(&policy.output_schema))
        {
            return Err("lifecycle policy schema unavailable".into());
        }
        Ok(catalog)
    }

    /// Validate installed policy shape and graph binding. Resolving the actual
    /// schema bytes and installed method approvals remains a separate authority check.
    fn validate_control_policies(&self) -> ModuleResult<()> {
        if self.control_policies.len() > 128 {
            return Err("too many lifecycle control policies".into());
        }
        for (index, policy) in self.control_policies.iter().enumerate() {
            let dependent = self
                .participants
                .iter()
                .find(|p| p.provider == policy.dependent_provider);
            let owner = self
                .participants
                .iter()
                .find(|p| p.provider == policy.owner_provider);
            if dependent.is_none_or(|p| !p.depends_on.contains(&policy.owner_provider))
                || owner.is_none()
                || self.control_policies[..index].contains(policy)
                || !valid_identifier(&policy.owner_service)
                || !valid_control_method(&policy.owner_method, &policy.owner_service)
                || !valid_control_method(&policy.status_method, &policy.owner_service)
                || !valid_control_method(&policy.settlement_method, &policy.owner_service)
                || !valid_control_method(&policy.settlement_status_method, &policy.owner_service)
                || !valid_opaque_id(&policy.output_capability)
                || !valid_schema_reference(&policy.argument_schema)
                || !valid_schema_reference(&policy.output_schema)
                || matches!(policy.max_access, ResourceAccess::Read)
                || !valid_domain_scope(policy.domain_scope, &policy.mutation_domains)
            {
                return Err("invalid lifecycle control policy".into());
            }
        }
        Ok(())
    }

    fn validate_finalize_policies(&self) -> ModuleResult<()> {
        if self.finalize_policies.len() > 128 {
            return Err("too many lifecycle finalize policies".into());
        }
        for (index, policy) in self.finalize_policies.iter().enumerate() {
            if !self
                .participants
                .iter()
                .any(|p| p.provider == policy.owner_provider)
                || self.finalize_policies[..index].contains(policy)
                || !valid_identifier(&policy.owner_service)
                || !valid_control_method(&policy.method, &policy.owner_service)
                || !valid_control_method(&policy.status_method, &policy.owner_service)
                || !valid_opaque_id(&policy.output_capability)
                || !valid_schema_reference(&policy.output_schema)
                || !self
                    .consumer_requirements
                    .capture_providers
                    .contains(&policy.capture_provider)
                || !self
                    .consumer_requirements
                    .selector_schemas
                    .contains(&policy.selector_schema)
                || matches!(policy.required_access, ResourceAccess::Read)
                || !valid_domain_scope(policy.domain_scope, &policy.mutation_domains)
            {
                return Err("invalid lifecycle finalize policy".into());
            }
        }
        Ok(())
    }

    /// Resolve required dependencies before admission. Among ready participants,
    /// the earliest configured provider wins, preserving deterministic tie order.
    pub(crate) fn resolved_participants(&self) -> ModuleResult<Vec<String>> {
        if self.participants.len() > 32 {
            return Err("too many lifecycle participants".into());
        }
        let mut names = BTreeSet::new();
        for participant in &self.participants {
            if !valid_identifier(&participant.provider)
                || !names.insert(participant.provider.as_str())
            {
                return Err("invalid lifecycle participant".into());
            }
        }
        for participant in &self.participants {
            let mut seen = BTreeSet::new();
            if participant.depends_on.len() > 32
                || participant.depends_on.iter().any(|dependency| {
                    !valid_identifier(dependency)
                        || dependency == &participant.provider
                        || !names.contains(dependency.as_str())
                        || !seen.insert(dependency)
                })
            {
                return Err("invalid lifecycle participant dependency".into());
            }
        }
        let mut resolved = Vec::with_capacity(self.participants.len());
        let mut visited = BTreeSet::new();
        while resolved.len() < self.participants.len() {
            let Some(participant) = self.participants.iter().find(|participant| {
                !visited.contains(participant.provider.as_str())
                    && participant
                        .depends_on
                        .iter()
                        .all(|dependency| visited.contains(dependency.as_str()))
            }) else {
                return Err("cyclic lifecycle participant dependencies".into());
            };
            visited.insert(participant.provider.as_str());
            resolved.push(participant.provider.clone());
        }
        Ok(resolved)
    }

    pub(crate) fn validate(&self) -> ModuleResult<()> {
        if self.journal_path.is_empty() {
            return Err("lifecycle journal path is required".into());
        }
        let requirements = &self.consumer_requirements;
        if requirements.execution_domain.is_empty()
            || requirements.execution_domain.len() > 256
            || requirements.execution_domain.chars().any(char::is_control)
            || [
                &requirements.capture_providers,
                &requirements.selector_schemas,
            ]
            .into_iter()
            .any(|values| {
                values.is_empty()
                    || values.len() > 32
                    || values.iter().any(|value| {
                        value.is_empty()
                            || value.len() > 128
                            || !value.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/')
                            })
                    })
            })
        {
            return Err("invalid lifecycle consumer requirements".into());
        }
        if self.trusted_callers.len() > 256
            || self
                .trusted_callers
                .iter()
                .any(|caller| !valid_identifier(caller))
        {
            return Err("invalid lifecycle trusted callers".into());
        }
        if self.session_delegates.len() > 128
            || self.session_delegates.iter().any(|delegate| {
                !valid_identifier(delegate) || !self.trusted_callers.contains(delegate)
            })
        {
            return Err("invalid lifecycle session delegates".into());
        }
        if !valid_identifier(&self.action_service)
            || self.input_actions.len() > 32
            || self
                .input_actions
                .iter()
                .any(|action| !valid_identifier(action))
        {
            return Err("invalid lifecycle input action policy".into());
        }
        if self
            .settings_registry_service
            .as_deref()
            .is_some_and(|service| !valid_identifier(service))
        {
            return Err("invalid lifecycle settings registry service".into());
        }
        if !self.settings_registry_caller.is_empty()
            && (!valid_identifier(&self.settings_registry_caller)
                || !self
                    .trusted_callers
                    .contains(&self.settings_registry_caller))
        {
            return Err("invalid lifecycle settings registry caller".into());
        }
        if self.operation_grants.iter().any(|(caller, operations)| {
            !self.trusted_callers.contains(caller)
                || operations.len() > 128
                || operations
                    .iter()
                    .any(|operation| !valid_identifier(operation))
        }) {
            return Err("invalid lifecycle operation grants".into());
        }
        for consumers in [&self.consumers.capture, &self.consumers.input] {
            if consumers.len() > 128
                || consumers.iter().any(|caller| {
                    !valid_identifier(caller) || !self.trusted_callers.contains(caller)
                })
            {
                return Err("invalid lifecycle consumers".into());
            }
        }
        let mut providers = BTreeSet::new();
        if self.participants.len() > 32 {
            return Err("too many lifecycle participants".into());
        }
        for participant in &self.participants {
            if !valid_identifier(&participant.provider)
                || !providers.insert(&participant.provider)
                || participant.capabilities.len() > 32
                || participant
                    .capabilities
                    .iter()
                    .any(|id| !valid_identifier(id))
                || participant.cleanup_methods.len() > 32
                || participant
                    .cleanup_methods
                    .iter()
                    .enumerate()
                    .any(|(index, method)| {
                        !valid_identifier(&method.service)
                            || !valid_identifier(&method.method)
                            || method.interface_major == 0
                            || method.schema_revision == 0
                            || participant.cleanup_methods[..index].contains(method)
                    })
                || !valid_identifier(&participant.settings_revision)
                || !participant.settings.is_object()
                || serde_json::to_vec(&participant.settings)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 8192
            {
                return Err("invalid lifecycle participant".into());
            }
            for method in [
                &participant.plan_method,
                &participant.callback_method,
                &participant.status_method,
            ]
            .into_iter()
            .chain(participant.binding_validate_method.iter())
            {
                if !valid_identifier(&method.service)
                    || !valid_identifier(&method.method)
                    || method.interface_major != 1
                    || method.schema_revision == 0
                {
                    return Err("invalid lifecycle participant method".into());
                }
            }
        }
        self.resolved_participants()?;
        self.validate_control_policies()?;
        self.validate_finalize_policies()?;
        self.schema_catalog()?;
        Ok(())
    }
}

pub struct LifecycleAdapter<H> {
    runtime: RuntimeClient,
    trusted_callers: BTreeSet<String>,
    operation_grants: BTreeMap<String, BTreeSet<String>>,
    handler: H,
}

impl<H: LifecycleHandler> ManagedModule for LifecycleAdapter<H> {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("lifecycle settings unavailable".into());
        };
        let settings: LifecycleSettings =
            serde_json::from_value(payload).map_err(|_| "invalid lifecycle settings".to_owned())?;
        settings.validate()?;
        let handler = H::from_settings(&settings)?;
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            trusted_callers: settings.trusted_callers,
            operation_grants: settings.operation_grants,
            handler,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        self.handler
            .advance(&mut |command| self.runtime.send(command))?;
        let (id, caller, initiator, service, payload) = match self.runtime.send(Command::Poll {})? {
            Response::Pending => return Ok(()),
            Response::Request {
                id,
                caller,
                initiator,
                service,
                payload,
                ..
            } => (id, caller, initiator, service, payload),
            response => return Err(format!("unexpected lifecycle poll response: {response:?}")),
        };
        match self.runtime.send(Command::CheckRequest { request: id })? {
            Response::Ok => {}
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                return Ok(());
            }
            response => {
                return Err(format!(
                    "unexpected lifecycle request check response: {response:?}"
                ));
            }
        }
        let response = if service != SERVICE {
            json!({"ok": false, "code": "invalid_service"})
        } else if !self.trusted_callers.contains(&caller)
            || !payload
                .get("operation")
                .and_then(Value::as_str)
                .is_some_and(|operation| {
                    self.operation_grants
                        .get(&caller)
                        .is_some_and(|grants| grants.contains(operation))
                })
        {
            json!({"ok": false, "code": "permission_denied"})
        } else {
            self.handler.handle(&caller, &initiator, payload)
        };
        match self.runtime.send(Command::Complete {
            request: id,
            payload: response,
        })? {
            Response::Ok => Ok(()),
            // A committed transition survives a request cancelled after dispatch.
            Response::Error { code, .. } if code == "request_ended" || code == "not_found" => {
                Ok(())
            }
            response => Err(format!(
                "unexpected lifecycle completion response: {response:?}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::abi::{self, AbiHeader, BorrowedBytes, HostApi, ReplySink};
    use std::collections::VecDeque;
    use std::ffi::c_void;

    #[derive(Default)]
    struct Handler;

    impl LifecycleHandler for Handler {
        fn from_settings(_settings: &LifecycleSettings) -> ModuleResult<Self> {
            Ok(Self)
        }

        fn handle(&mut self, caller: &str, initiator: &str, payload: Value) -> Value {
            json!({"caller": caller, "initiator": initiator, "payload": payload})
        }
    }

    struct Broker {
        settings: Value,
        requests: VecDeque<Response>,
        completions: Vec<Value>,
        active: bool,
        completion_error: Option<&'static str>,
        advances: usize,
    }

    impl Broker {
        fn dispatch(&mut self, command: Command) -> Response {
            match command {
                Command::GetSettings {} => Response::Result {
                    payload: self.settings.clone(),
                },
                Command::Register { .. } => Response::Ok,
                Command::Log { .. } => {
                    self.advances += 1;
                    Response::Ok
                }
                Command::Poll {} => self.requests.pop_front().unwrap_or(Response::Pending),
                Command::CheckRequest { .. } if self.active => Response::Ok,
                Command::CheckRequest { .. } => Response::error("request_ended", "cancelled"),
                Command::Complete { payload, .. } => {
                    self.completions.push(payload);
                    match self.completion_error {
                        Some(code) => Response::error(code, "completion failed"),
                        None => Response::Ok,
                    }
                }
                _ => panic!("unexpected broker command"),
            }
        }
    }

    unsafe extern "C" fn invoke(
        context: *mut c_void,
        input: BorrowedBytes,
        reply: ReplySink,
    ) -> abi::Status {
        // SAFETY: the test keeps exclusive access to the broker through this endpoint.
        let broker = unsafe { &mut *context.cast::<Broker>() };
        // SAFETY: the SDK supplies bytes valid for the duration of this call.
        let Ok(bytes) = (unsafe { input.read() }) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(command) = serde_json::from_slice(bytes) else {
            return abi::STATUS_INVALID_ARGUMENT;
        };
        let Ok(output) = serde_json::to_vec(&broker.dispatch(command)) else {
            return abi::STATUS_INTERNAL_ERROR;
        };
        // SAFETY: the reply sink copies these bytes synchronously.
        unsafe { reply.send(&output) };
        abi::STATUS_OK
    }

    fn request(caller: &str, initiator: &str) -> Response {
        Response::Request {
            id: 1,
            caller: caller.into(),
            initiator: initiator.into(),
            service: SERVICE.into(),
            payload: json!({"operation":"prepare", "caller":"transport", "initiator":"forged"}),
            remaining_ms: 1000,
        }
    }

    fn run(broker: &mut Broker) -> ModuleResult<()> {
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (broker as *mut Broker).cast(),
            invoke,
        };
        // SAFETY: broker outlives the module and is exclusively accessed through the endpoint.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = LifecycleAdapter::<Handler>::start(runtime)?;
        module.tick()
    }

    fn broker(mut settings: Value, caller: &str) -> Broker {
        if let Some(object) = settings.as_object_mut() {
            object
                .entry("journal_path")
                .or_insert(json!("/test/journal"));
            object.entry("consumer_requirements").or_insert(json!({"execution_domain":"desktop", "capture_providers":["capture"], "selector_schemas":["selector/1"]}));
        }
        Broker {
            settings,
            requests: VecDeque::from([request(caller, "real-initiator")]),
            completions: Vec::new(),
            active: true,
            completion_error: None,
            advances: 0,
        }
    }

    #[test]
    fn broker_identity_is_authority_and_empty_policy_denies() {
        let mut denied = broker(json!({}), "transport");
        run(&mut denied).unwrap();
        assert_eq!(denied.completions[0]["code"], "permission_denied");
        let mut denied = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "plugin",
        );
        run(&mut denied).unwrap();
        assert_eq!(denied.completions[0]["code"], "permission_denied");
        let mut no_grant = broker(json!({"trusted_callers":["transport"]}), "transport");
        run(&mut no_grant).unwrap();
        assert_eq!(no_grant.completions[0]["code"], "permission_denied");
        let mut allowed = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        run(&mut allowed).unwrap();
        assert_eq!(allowed.completions[0]["caller"], "transport");
        assert_eq!(allowed.completions[0]["initiator"], "real-initiator");
    }

    #[test]
    fn cancelled_request_is_not_dispatched_and_late_cancellation_is_tolerated() {
        let mut cancelled = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        cancelled.active = false;
        run(&mut cancelled).unwrap();
        assert!(cancelled.completions.is_empty());
        for code in ["request_ended", "not_found", "permission_denied"] {
            let mut late = broker(
                json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
                "transport",
            );
            late.completion_error = Some(code);
            assert_eq!(run(&mut late).is_ok(), code != "permission_denied");
            assert_eq!(late.completions.len(), 1);
        }
    }

    #[test]
    fn malformed_settings_fail_closed() {
        for settings in [
            json!({"trusted_callers":[""]}),
            json!({"journal_path":""}),
            json!({"trusted_callers":"transport"}),
            json!({"unexpected":true}),
        ] {
            let mut broker = broker(settings, "transport");
            assert!(run(&mut broker).is_err());
            assert!(broker.completions.is_empty());
        }
    }
    #[test]
    fn background_work_advances_on_idle_and_busy_ticks() {
        struct Advancing;
        impl LifecycleHandler for Advancing {
            fn from_settings(_: &LifecycleSettings) -> ModuleResult<Self> {
                Ok(Self)
            }
            fn handle(&mut self, _: &str, _: &str, _: Value) -> Value {
                json!({"ok": true})
            }
            fn advance(
                &mut self,
                send: &mut dyn FnMut(Command) -> ModuleResult<Response>,
            ) -> ModuleResult<()> {
                match send(Command::Log {
                    level: 0,
                    target: "test".into(),
                    message: "advance".into(),
                })? {
                    Response::Ok => Ok(()),
                    _ => Err("advance failed".into()),
                }
            }
        }
        let mut broker = broker(
            json!({"trusted_callers":["transport"], "operation_grants":{"transport":["prepare"]}}),
            "transport",
        );
        let endpoint = HostApi {
            header: AbiHeader::of::<HostApi>(),
            context: (&mut broker as *mut Broker).cast(),
            invoke,
        };
        // SAFETY: broker outlives the module and is exclusively accessed through the endpoint.
        let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
        let mut module = LifecycleAdapter::<Advancing>::start(runtime).unwrap();
        module.tick().unwrap();
        module.tick().unwrap();
        drop(module);
        assert_eq!(broker.advances, 2);
        assert_eq!(broker.completions.len(), 1);
    }
    #[test]
    fn participants_and_consumers_require_bounded_trusted_policy() {
        let method = json!({"service":"provider/1", "method":"prepare", "interface_major":1, "interface_minor":0, "schema_revision":1});
        let participant = json!({"provider":"provider", "plan_method":method, "callback_method":method, "status_method":method, "capabilities":["display/change"], "cleanup_methods":[method], "settings_revision":"rev-1", "settings":{}});
        let policy = json!({"journal_path":"/test/journal", "trusted_callers":["capture", "input"], "consumers":{"capture":["capture"], "input":["input"]}, "participants":[participant], "consumer_requirements":{"execution_domain":"desktop", "capture_providers":["capture"], "selector_schemas":["selector/1"]}});
        let parse = |value| {
            serde_json::from_value::<LifecycleSettings>(value)
                .unwrap()
                .validate()
        };
        assert!(parse(policy.clone()).is_ok());
        let mut invalid_dependency = policy.clone();
        invalid_dependency["participants"][0]["depends_on"] = json!(["absent"]);
        assert!(parse(invalid_dependency).is_err());
        let mut registry_service = policy.clone();
        registry_service["settings_registry_service"] = json!("amaterasu.extension-registry/1");
        assert!(parse(registry_service.clone()).is_ok());
        registry_service["settings_registry_service"] = json!("");
        assert!(parse(registry_service).is_err());
        let mut registry = policy.clone();
        registry["settings_registry_caller"] = json!("input");
        assert!(parse(registry).is_ok());
        let mut untrusted_registry = policy.clone();
        untrusted_registry["settings_registry_caller"] = json!("foreign");
        assert!(parse(untrusted_registry).is_err());
        let mut actions = policy.clone();
        actions["input_actions"] = json!(["display/remove"]);
        actions["action_service"] = json!("amaterasu.extension-actions");
        assert!(parse(actions).is_ok());
        let mut invalid_action = policy.clone();
        invalid_action["input_actions"] = json!(["invalid\naction"]);
        assert!(parse(invalid_action).is_err());
        let mut invalid_service = policy.clone();
        invalid_service["action_service"] = json!("");
        assert!(parse(invalid_service).is_err());
        let mut many_actions = policy.clone();
        many_actions["input_actions"] =
            json!((0..33).map(|n| format!("action-{n}")).collect::<Vec<_>>());
        assert!(parse(many_actions).is_err());
        let mut binding = policy.clone();
        binding["participants"][0]["binding_validate_method"] = method.clone();
        assert!(parse(binding.clone()).is_ok());
        binding["participants"][0]["binding_validate_method"]["interface_major"] = json!(0);
        assert!(parse(binding).is_err());
        let mut binding = policy.clone();
        binding["participants"][0]["binding_validate_method"] = method.clone();
        binding["participants"][0]["binding_validate_method"]["schema_revision"] = json!(0);
        assert!(parse(binding).is_err());
        let mut delegated = policy.clone();
        delegated["session_delegates"] = json!(["input"]);
        assert!(parse(delegated).is_ok());
        let mut untrusted_delegate = policy.clone();
        untrusted_delegate["session_delegates"] = json!(["other"]);
        assert!(parse(untrusted_delegate).is_err());
        let mut too_many_delegates = policy.clone();
        let delegates: Vec<_> = (0..129).map(|index| format!("delegate-{index}")).collect();
        too_many_delegates["trusted_callers"] = json!(delegates);
        too_many_delegates["session_delegates"] = json!(delegates);
        too_many_delegates["consumers"] = json!({});
        assert!(parse(too_many_delegates).is_err());
        for field in ["capabilities", "cleanup_methods"] {
            let mut empty = policy.clone();
            empty["participants"][0][field] = json!([]);
            assert!(parse(empty).is_ok());
            let mut missing = policy.clone();
            missing["participants"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(serde_json::from_value::<LifecycleSettings>(missing).is_err());
        }
        let mut malformed_cleanup = policy.clone();
        malformed_cleanup["participants"][0]["cleanup_methods"][0]["schema_revision"] = json!(0);
        assert!(parse(malformed_cleanup).is_err());
        let mut duplicate_cleanup = policy.clone();
        duplicate_cleanup["participants"][0]["cleanup_methods"] = json!([method, method]);
        assert!(parse(duplicate_cleanup).is_err());
        let mut missing = policy.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("consumer_requirements");
        assert!(parse(missing).is_err());
        for field in ["capture_providers", "selector_schemas"] {
            let mut empty = policy.clone();
            empty["consumer_requirements"][field] = json!([]);
            assert!(parse(empty).is_err());
            let mut many = policy.clone();
            many["consumer_requirements"][field] = json!(vec!["valid"; 33]);
            assert!(parse(many).is_err());
            let mut invalid = policy.clone();
            invalid["consumer_requirements"][field] = json!(["bad\n"]);
            assert!(parse(invalid).is_err());
        }
        let mut invalid_domain = policy.clone();
        invalid_domain["consumer_requirements"]["execution_domain"] = json!("");
        assert!(parse(invalid_domain).is_err());
        let mut untrusted = policy.clone();
        untrusted["consumers"]["capture"] = json!(["other"]);
        assert!(parse(untrusted).is_err());
        let mut duplicate = policy.clone();
        duplicate["participants"] = json!([participant, participant]);
        assert!(parse(duplicate).is_err());
        let mut invalid_major = policy.clone();
        invalid_major["participants"][0]["plan_method"]["interface_major"] = json!(0);
        assert!(parse(invalid_major.clone()).is_err());
        invalid_major["participants"][0]["plan_method"]["interface_major"] = json!(2);
        assert!(parse(invalid_major).is_err());
        let mut invalid_settings = policy.clone();
        invalid_settings["participants"][0]["settings"] = json!([]);
        assert!(parse(invalid_settings).is_err());
        let mut oversize = policy;
        oversize["participants"][0]["settings"] = json!({"data":"a".repeat(8192)});
        assert!(parse(oversize).is_err());
    }
    #[test]
    fn participant_dependencies_resolve_stably_and_reject_invalid_graphs() {
        let method = json!({"service":"provider/1","method":"prepare","interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = |provider: &str, dependencies: Value| json!({"provider":provider,"depends_on":dependencies,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"initial","settings":{}});
        let mut settings: LifecycleSettings = serde_json::from_value(json!({"participants":[participant("c",json!(["b"])),participant("independent",json!([])),participant("b",json!(["a"])),participant("a",json!([]))]})).unwrap();
        assert_eq!(
            settings.resolved_participants().unwrap(),
            ["independent", "a", "b", "c"]
        );
        settings.participants[3].depends_on.push("c".into());
        assert!(settings.resolved_participants().is_err());
        settings.participants[3].depends_on = vec!["missing".into()];
        assert!(settings.resolved_participants().is_err());
        settings.participants[3].depends_on = vec!["a".into()];
        assert!(settings.resolved_participants().is_err());
        settings.participants[3].depends_on.clear();
        settings.participants[2].depends_on = vec!["a".into(), "a".into()];
        assert!(settings.resolved_participants().is_err());
        settings.participants[2].depends_on = vec!["a".into()];
        settings.participants[3].provider = "b".into();
        assert!(settings.resolved_participants().is_err());
    }
    #[test]
    fn installed_control_policy_requires_exact_graph_and_valid_pins() {
        let method = json!({"service":"owner/1","method":"apply","interface_major":1,"interface_minor":0,"schema_revision":1});
        let participant = |provider: &str, depends_on: Value| json!({"provider":provider,"depends_on":depends_on,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"initial","settings":{}});
        use sha2::{Digest, Sha256};
        let document = r#"{"$id":"schema/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#;
        let digest = format!("{:x}", Sha256::digest(document.as_bytes()));
        let schema = json!({"id":"schema/1","major":1,"minor":0,"digest":digest});
        let effect_method = json!({"service":"owner/1","method":"apply","interface_major":2,"interface_minor":0,"schema_revision":1});
        let status_method = json!({"service":"owner/1","method":"status","interface_major":2,"interface_minor":0,"schema_revision":1});
        let policy = json!({"dependent_provider":"dependent","owner_provider":"owner","owner_service":"owner/1","owner_method":effect_method,"status_method":status_method,"settlement_method":effect_method,"settlement_status_method":status_method,"argument_schema":schema,"output_capability":"display/output","output_schema":schema,"max_access":"exclusive","mutation_domains":["seat:0"]});
        let mut settings: LifecycleSettings = serde_json::from_value(json!({"journal_path":"/test", "consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"participants":[participant("dependent",json!(["owner"])),participant("owner",json!([]))],"control_policies":[policy], "schema_documents":[{"reference":schema,"document":document}]})).unwrap();
        assert!(settings.validate().is_ok());
        settings
            .control_policies
            .push(settings.control_policies[0].clone());
        assert!(settings.validate().is_err());
        settings.control_policies.pop();
        settings.participants[0].depends_on.clear();
        assert!(settings.validate().is_err());
        settings.participants[0].depends_on.push("owner".into());
        settings.control_policies[0].argument_schema.digest = "A".repeat(64);
        assert!(settings.validate().is_err());
        settings.control_policies[0].argument_schema.digest = digest;
        settings.control_policies[0].owner_method.service = "other/1".into();
        assert!(settings.validate().is_err());
        settings.control_policies[0].owner_method.service = "owner/1".into();
        settings.control_policies[0].max_access = ResourceAccess::Read;
        assert!(settings.validate().is_err());
        settings.control_policies[0].max_access = ResourceAccess::Exclusive;
        settings.control_policies[0].mutation_domains.clear();
        assert!(settings.validate().is_err());
        settings.control_policies[0].domain_scope = DomainScope::Resource;
        assert!(settings.validate().is_ok());
        settings.control_policies[0]
            .mutation_domains
            .insert("seat:0".into());
        assert!(settings.validate().is_err());
        settings.control_policies[0].mutation_domains.clear();
        settings.control_policies[0].status_method.service = "wrong/1".into();
        assert!(settings.validate().is_err());
        settings.control_policies[0].status_method.service = "owner/1".into();
        settings.control_policies[0]
            .settlement_method
            .interface_major = 1;
        assert!(settings.validate().is_err());
        settings.control_policies[0]
            .settlement_method
            .interface_major = 2;
        settings.control_policies[0]
            .settlement_status_method
            .service = "wrong/1".into();
        assert!(settings.validate().is_err());
        settings.control_policies[0]
            .settlement_status_method
            .service = "owner/1".into();
        settings.control_policies[0]
            .settlement_status_method
            .schema_revision = 0;
        assert!(settings.validate().is_err());
        settings.control_policies.clear();
        assert!(settings.validate().is_ok());
        let finalize = json!({"owner_provider":"owner","owner_service":"owner/1","method":effect_method,"status_method":status_method,"output_capability":"display/output","output_schema":schema,"capture_provider":"capture","selector_schema":"selector/1","required_access":"exclusive","mutation_domains":["seat:0"]});
        settings.finalize_policies = vec![serde_json::from_value(finalize).unwrap()];
        assert!(settings.validate().is_ok());
        settings
            .finalize_policies
            .push(settings.finalize_policies[0].clone());
        assert!(settings.validate().is_err());
        settings.finalize_policies.pop();
        settings.finalize_policies[0].capture_provider = "wrong".into();
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].capture_provider = "capture".into();
        settings.finalize_policies[0].method.service = "wrong/1".into();
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].method.service = "owner/1".into();
        settings.finalize_policies[0].required_access = ResourceAccess::Read;
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].required_access = ResourceAccess::Exclusive;
        settings.finalize_policies[0].mutation_domains.clear();
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].domain_scope = DomainScope::Resource;
        assert!(settings.validate().is_ok());
        settings.finalize_policies[0]
            .mutation_domains
            .insert("seat:0".into());
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].mutation_domains.clear();
        settings.finalize_policies[0].status_method.interface_major = 1;
        assert!(settings.validate().is_err());
        settings.finalize_policies[0].status_method.interface_major = 2;
        settings.finalize_policies[0].output_schema.digest = "f".repeat(64);
        assert!(settings.validate().is_err());
        settings.finalize_policies.clear();
        assert!(settings.validate().is_ok());
        settings.schema_documents[0].document.push(' ');
        assert!(settings.validate().is_err());
    }
}
