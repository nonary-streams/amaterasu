//! Asynchronous settings validation and state CAS.
use crate::{
    discovery,
    persistence::Store,
    policy::{GrantedScope, Settings},
    settings::{self, Document, Scope},
};
use amaterasu_configuration_schema::{Fields, Values};
use amaterasu_extension_api::{LifecycleTarget, MethodRef, SettingScope};
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 32;
const MAX_VALIDATIONS: usize = 64;
const MAX_RESPONSE_BYTES: usize = 12_000;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum SettingsOperation {
    #[serde(rename = "settings_get")]
    Get {
        provider: String,
        scope: Scope,
        application: Option<String>,
        paired_client: Option<String>,
        session: Option<String>,
        #[serde(default)]
        target: Option<LifecycleTarget>,
        #[serde(default)]
        input_binding: Option<String>,
        #[serde(default)]
        input_boot_epoch: Option<String>,
    },
    #[serde(rename = "settings_migrate")]
    Migrate {
        provider: String,
        expected_revision: u64,
    },
    #[serde(rename = "settings_update")]
    Update {
        provider: String,
        scope: Scope,
        expected_revision: u64,
        values: Values,
    },
}

struct Pending {
    provider: String,
    scope: Scope,
    operation: SettingsOperation,
    deadline: Instant,
    fields: Fields,
    allowed: BTreeMap<String, BTreeSet<String>>,
    validation: MethodRef,
    schema_revision: u64,
    migrations: Vec<crate::policy::MigrationPlan>,
    writable_scopes: [Vec<GrantedScope>; 2],
    source_caller: String,
    source_initiator: String,
    stage: Stage,
}

impl Pending {
    fn can_write(&self, scope: &Scope) -> bool {
        self.writable_scopes
            .iter()
            .all(|grants| grants.iter().any(|grant| grant.matches(scope)))
    }
}

enum Stage {
    Authorizing {
        target: LifecycleTarget,
        input_binding: Option<String>,
        input_boot_epoch: Option<String>,
        scopes: Vec<Scope>,
        index: usize,
        child: Option<u64>,
        outcome: Option<Value>,
    },
    Loading,
    Validating {
        document: Document,
        state_revision: u64,
        snapshots: Vec<Values>,
        index: usize,
        child: Option<u64>,
        outcome: Option<Value>,
    },
    Saving {
        revision: u64,
    },
}

#[derive(Default)]
pub struct SettingsService {
    store: Store,
    pending: BTreeMap<u64, Pending>,
}

impl SettingsService {
    pub fn accept(
        &mut self,
        runtime: &RuntimeClient,
        policy: &Settings,
        parent: u64,
        identities: (&str, &str),
        remaining_ms: u64,
        mut operation: SettingsOperation,
    ) -> Result<(), Value> {
        let (caller, initiator) = identities;
        if self.pending.len() >= MAX_PENDING {
            return Err(discovery::failure(
                "resource_exhausted",
                "settings queue is full",
            ));
        }
        if let SettingsOperation::Get {
            scope,
            application,
            paired_client,
            session,
            ..
        } = &mut operation
        {
            let selected = match scope {
                Scope::Host => None,
                Scope::Application(id) => Some((application, id)),
                Scope::PairedClient(id) => Some((paired_client, id)),
                Scope::Session(id) => Some((session, id)),
            };
            if let Some((selector, id)) = selected {
                if selector.as_ref().is_some_and(|current| current != id) {
                    return Err(discovery::failure(
                        "invalid_request",
                        "scope and effective selector differ",
                    ));
                }
                *selector = Some(id.clone());
            }
        }
        let host_scope = Scope::Host;
        let (provider, scope, write) = match &operation {
            SettingsOperation::Get {
                provider, scope, ..
            } => (provider, scope, false),
            SettingsOperation::Update {
                provider, scope, ..
            } => (provider, scope, true),
            SettingsOperation::Migrate { provider, .. } => (provider, &host_scope, true),
        };
        if matches!(
            &operation,
            SettingsOperation::Get {
                target: None,
                input_binding: Some(_),
                ..
            }
        ) {
            return Err(discovery::failure(
                "invalid_request",
                "input binding requires target",
            ));
        }
        if let SettingsOperation::Get {
            input_binding,
            input_boot_epoch,
            ..
        } = &operation
        {
            let valid_epoch = input_boot_epoch.as_deref().is_none_or(|epoch| {
                epoch.len() == 32
                    && epoch
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
            if input_binding.is_some() != input_boot_epoch.is_some() || !valid_epoch {
                return Err(discovery::failure(
                    "invalid_request",
                    "input binding requires a valid boot epoch",
                ));
            }
        }
        let dynamic = matches!(
            &operation,
            SettingsOperation::Get {
                target: Some(_),
                ..
            }
        );
        let item = policy
            .descriptors
            .iter()
            .find(|item| {
                item.descriptor.provider == *provider
                    && if dynamic {
                        item.allows(initiator, &Scope::Host, false)
                            && item.allows(caller, &Scope::Host, false)
                    } else {
                        item.allows(initiator, scope, write) && item.allows(caller, scope, write)
                    }
            })
            .ok_or_else(|| discovery::failure("permission_denied", "scope is not authorized"))?;
        let mut dynamic_scopes = BTreeSet::new();
        if let SettingsOperation::Get {
            application,
            paired_client,
            session,
            target,
            ..
        } = &operation
        {
            let selected = application
                .as_ref()
                .map(|id| Scope::Application(id.clone()))
                .into_iter()
                .chain(
                    paired_client
                        .as_ref()
                        .map(|id| Scope::PairedClient(id.clone())),
                )
                .chain(session.as_ref().map(|id| Scope::Session(id.clone())))
                .chain(std::iter::once(scope.clone()));
            for selected in selected {
                if selected == Scope::Host {
                    continue;
                }
                if target.is_some() {
                    if matches!(selected, Scope::PairedClient(_)) {
                        return Err(discovery::failure(
                            "permission_denied",
                            "client scope authority unavailable",
                        ));
                    }
                    dynamic_scopes.insert(selected);
                } else if !item.allows(initiator, &selected, false)
                    || !item.allows(caller, &selected, false)
                {
                    return Err(discovery::failure(
                        "permission_denied",
                        "effective scope is not authorized",
                    ));
                }
            }
            if !item.allows(initiator, &Scope::Host, false)
                || !item.allows(caller, &Scope::Host, false)
            {
                return Err(discovery::failure(
                    "permission_denied",
                    "host scope is not authorized",
                ));
            }
        }
        let (_, approvals) = discovery::approvals(runtime, parent)
            .map_err(|_| discovery::failure("unavailable", "broker approvals unavailable"))?;
        if !crate::policy::approved(item, &item.descriptor.settings_validate, &approvals) {
            return Err(discovery::failure(
                "unavailable",
                "settings validation method unavailable",
            ));
        }
        let fields = item
            .descriptor
            .options
            .iter()
            .map(|option| (option.id.clone(), option.definition.clone()))
            .collect();
        let allowed = item
            .descriptor
            .options
            .iter()
            .map(|option| {
                (
                    option.id.clone(),
                    option
                        .allowed_scopes
                        .iter()
                        .map(|scope| {
                            match scope {
                                SettingScope::Host => "host",
                                SettingScope::Application => "application",
                                SettingScope::PairedClient => "paired_client",
                                SettingScope::Session => "session",
                            }
                            .to_string()
                        })
                        .collect(),
                )
            })
            .collect();
        let deadline = Instant::now() + Duration::from_millis(remaining_ms.min(30_000));
        let stage = match &operation {
            SettingsOperation::Get {
                target: Some(target),
                input_binding,
                input_boot_epoch,
                ..
            } => {
                if dynamic_scopes.is_empty() {
                    return Err(discovery::failure(
                        "invalid_request",
                        "dynamic target requires scoped selector",
                    ));
                }
                Stage::Authorizing {
                    target: target.clone(),
                    input_binding: input_binding.clone(),
                    input_boot_epoch: input_boot_epoch.clone(),
                    scopes: dynamic_scopes.into_iter().collect(),
                    index: 0,
                    child: None,
                    outcome: None,
                }
            }
            _ => {
                self.store
                    .get(parent, format!("extensions/{provider}"), deadline)?;
                Stage::Loading
            }
        };
        self.pending.insert(
            parent,
            Pending {
                provider: provider.clone(),
                scope: scope.clone(),
                operation,
                deadline,
                fields,
                allowed,
                validation: item.descriptor.settings_validate.clone(),
                schema_revision: item.descriptor.revision,
                migrations: item.migrations.clone(),
                writable_scopes: [caller, initiator].map(|identity| {
                    item.grants
                        .iter()
                        .filter(|grant| grant.write && grant.caller == identity)
                        .map(|grant| grant.scope.clone())
                        .collect()
                }),
                source_caller: caller.to_owned(),
                source_initiator: initiator.to_owned(),
                stage,
            },
        );
        Ok(())
    }

    pub fn tick(&mut self, runtime: &RuntimeClient) -> ModuleResult<Vec<(u64, Value)>> {
        let mut completed = Vec::new();
        for (id, payload) in self.store.tick(|command| runtime.send(command))? {
            let Some(mut pending) = self.pending.remove(&id) else {
                continue;
            };
            if payload.get("ok") != Some(&Value::Bool(true)) {
                completed.push((id, payload));
                continue;
            }
            match pending.stage {
                Stage::Loading => {
                    let Some(state_revision) = payload.get("revision").and_then(Value::as_u64)
                    else {
                        completed.push((
                            id,
                            discovery::failure("invalid_state", "state revision missing"),
                        ));
                        continue;
                    };
                    if !matches!(payload.get("found"), Some(Value::Bool(_))) {
                        completed.push((
                            id,
                            discovery::failure("invalid_state", "state found flag missing"),
                        ));
                        continue;
                    }
                    let document = if payload.get("found") == Some(&Value::Bool(false)) {
                        Document {
                            revision: 0,
                            schema_revision: pending.schema_revision,
                            layers: Vec::new(),
                        }
                    } else {
                        match serde_json::from_value::<Document>(
                            payload.get("value").cloned().unwrap_or(Value::Null),
                        ) {
                            Ok(document) => document,
                            Err(_) => {
                                completed.push((
                                    id,
                                    discovery::failure(
                                        "invalid_state",
                                        "stored settings malformed",
                                    ),
                                ));
                                continue;
                            }
                        }
                    };
                    if matches!(&pending.operation, SettingsOperation::Migrate { .. }) {
                        let SettingsOperation::Migrate {
                            expected_revision, ..
                        } = &pending.operation
                        else {
                            unreachable!()
                        };
                        if document
                            .layers
                            .iter()
                            .any(|layer| !pending.can_write(&layer.scope))
                        {
                            completed.push((
                                id,
                                discovery::failure(
                                    "permission_denied",
                                    "migration includes unauthorized scope",
                                ),
                            ));
                            continue;
                        }
                        let Some(plan) = pending
                            .migrations
                            .iter()
                            .find(|plan| plan.from_schema_revision == document.schema_revision)
                        else {
                            completed.push((
                                id,
                                discovery::failure(
                                    "migration_required",
                                    "installed migration plan unavailable",
                                ),
                            ));
                            continue;
                        };
                        let replacements: BTreeMap<_, _> = plan
                            .replacements
                            .iter()
                            .map(|layer| (layer.scope.clone(), layer.values.clone()))
                            .collect();
                        if replacements.len() != plan.replacements.len()
                            || replacements.keys().any(|scope| !pending.can_write(scope))
                        {
                            completed.push((
                                id,
                                discovery::failure(
                                    "permission_denied",
                                    "migration replacement scope is invalid or unauthorized",
                                ),
                            ));
                            continue;
                        }
                        let migration = settings::Migration {
                            old_fields: &plan.old_fields,
                            new_fields: &pending.fields,
                            old_allowed: &plan.old_allowed,
                            new_allowed: &pending.allowed,
                            expected_revision: *expected_revision,
                            new_schema_revision: pending.schema_revision,
                            renames: &plan.renames,
                            removed: &plan.removed,
                            replacements: &replacements,
                        };
                        let updated = match settings::migrate(&document, migration) {
                            Ok(value) => value,
                            Err(error) => {
                                completed.push((
                                    id,
                                    discovery::failure(
                                        if error == "revision_conflict" {
                                            "revision_conflict"
                                        } else {
                                            "invalid_migration"
                                        },
                                        &error,
                                    ),
                                ));
                                continue;
                            }
                        };
                        if !settings::responses_fit(&pending.fields, &updated, MAX_RESPONSE_BYTES) {
                            completed.push((
                                id,
                                discovery::failure(
                                    "resource_exhausted",
                                    "settings response exceeds size limit",
                                ),
                            ));
                            continue;
                        }
                        let snapshots = match settings::validation_snapshots(
                            &pending.fields,
                            &pending.allowed,
                            &updated,
                            MAX_VALIDATIONS,
                        ) {
                            Ok(value) => value,
                            Err(error) => {
                                completed
                                    .push((id, discovery::failure("resource_exhausted", &error)));
                                continue;
                            }
                        };
                        pending.stage = Stage::Validating {
                            document: updated,
                            state_revision,
                            snapshots,
                            index: 0,
                            child: None,
                            outcome: None,
                        };
                        self.pending.insert(id, pending);
                        continue;
                    }
                    if document.schema_revision != pending.schema_revision {
                        completed.push((
                            id,
                            discovery::failure(
                                "migration_required",
                                "stored schema revision differs",
                            ),
                        ));
                        continue;
                    }
                    if let Err(error) =
                        settings::validate_document(&pending.fields, &pending.allowed, &document)
                    {
                        completed.push((id, discovery::failure("invalid_state", &error)));
                        continue;
                    }
                    match &pending.operation {
                        SettingsOperation::Get {
                            application,
                            paired_client,
                            session,
                            ..
                        } => {
                            let effective = settings::resolve(
                                &pending.fields,
                                &pending.allowed,
                                &document,
                                application.as_deref(),
                                paired_client.as_deref(),
                                session.as_deref(),
                            );
                            let values = document
                                .layers
                                .iter()
                                .find(|layer| layer.scope == pending.scope)
                                .map(|layer| layer.values.clone())
                                .unwrap_or_default();
                            match effective { Ok(effective) => completed.push((id, json!({"ok":true,"status":"ok","revision":document.revision,"values":values,"effective":effective}))), Err(error) => completed.push((id, discovery::failure("invalid_state", &error))) }
                            continue;
                        }
                        SettingsOperation::Migrate { .. } => unreachable!(),
                        SettingsOperation::Update {
                            scope,
                            expected_revision,
                            values,
                            ..
                        } => {
                            match settings::update(
                                &pending.fields,
                                &pending.allowed,
                                &document,
                                *expected_revision,
                                scope.clone(),
                                values.clone(),
                            ) {
                                Ok(updated)
                                    if !settings::responses_fit(
                                        &pending.fields,
                                        &updated,
                                        MAX_RESPONSE_BYTES,
                                    ) =>
                                {
                                    completed.push((
                                        id,
                                        discovery::failure(
                                            "resource_exhausted",
                                            "settings response exceeds size limit",
                                        ),
                                    ));
                                    continue;
                                }
                                Ok(updated) => match settings::validation_snapshots(
                                    &pending.fields,
                                    &pending.allowed,
                                    &updated,
                                    MAX_VALIDATIONS,
                                ) {
                                    Ok(snapshots) => {
                                        pending.stage = Stage::Validating {
                                            document: updated,
                                            state_revision,
                                            snapshots,
                                            index: 0,
                                            child: None,
                                            outcome: None,
                                        }
                                    }
                                    Err(error) => {
                                        completed.push((
                                            id,
                                            discovery::failure("resource_exhausted", &error),
                                        ));
                                        continue;
                                    }
                                },
                                Err(error) => {
                                    completed.push((
                                        id,
                                        discovery::failure(&error, "settings update rejected"),
                                    ));
                                    continue;
                                }
                            }
                        }
                    }
                }
                Stage::Saving { revision } => {
                    completed.push((id, json!({"ok":true,"status":"ok","revision":revision})));
                    continue;
                }
                Stage::Authorizing { .. } | Stage::Validating { .. } => {
                    completed.push((
                        id,
                        discovery::failure("invalid_state", "unexpected persistence result"),
                    ));
                    continue;
                }
            }
            self.pending.insert(id, pending);
        }
        for (id, pending) in &mut self.pending {
            let Stage::Authorizing {
                target,
                input_binding,
                input_boot_epoch,
                scopes,
                index,
                child,
                outcome,
            } = &mut pending.stage
            else {
                continue;
            };
            if Instant::now() >= pending.deadline {
                if let Some(handle) = *child {
                    let _ = runtime.send(Command::Cancel { request: handle });
                    let _ = runtime.send(Command::ReleaseResult { request: handle });
                }
                completed.push((
                    *id,
                    discovery::failure("deadline_exceeded", "scope authorization expired"),
                ));
                continue;
            }
            if let Some(handle) = *child {
                if outcome.is_none() {
                    match runtime.send(Command::TakeResult { request: handle }) {
                        Ok(Response::Pending) | Err(_) => continue,
                        Ok(Response::Result { payload }) => *outcome = Some(payload),
                        _ => *outcome = Some(Value::Null),
                    }
                }
                match runtime.send(Command::ReleaseResult { request: handle }) {
                    Ok(Response::Ok) => (),
                    Ok(Response::Error { code, .. }) if code == "not_found" => (),
                    _ => continue,
                }
                let reply = outcome.take().expect("scope reply present");
                *child = None;
                let (kind, scope_id) = match &scopes[*index] {
                    Scope::Application(id) => ("application", id),
                    Scope::Session(id) => ("session", id),
                    _ => unreachable!(),
                };
                if reply.get("status").and_then(Value::as_str) != Some("succeeded")
                    || reply.get("scope").and_then(Value::as_str) != Some(kind)
                    || reply.get("scope_id").and_then(Value::as_str) != Some(scope_id.as_str())
                    || serde_json::to_value(&*target).ok().as_ref() != reply.get("target")
                {
                    completed.push((
                        *id,
                        discovery::failure("permission_denied", "scope authorization failed"),
                    ));
                    continue;
                }
                // Consume the coordinator-returned selector rather than retaining
                // the request field as the source of effective settings identity.
                let canonical = reply["scope_id"]
                    .as_str()
                    .expect("validated scope")
                    .to_owned();
                if let SettingsOperation::Get {
                    scope,
                    application,
                    session,
                    ..
                } = &mut pending.operation
                {
                    match kind {
                        "application" => {
                            if application.is_some() {
                                *application = Some(canonical.clone());
                            }
                            if matches!(scope, Scope::Application(_)) {
                                *scope = Scope::Application(canonical.clone());
                                pending.scope = scope.clone();
                            }
                        }
                        "session" => {
                            if session.is_some() {
                                *session = Some(canonical.clone());
                            }
                            if matches!(scope, Scope::Session(_)) {
                                *scope = Scope::Session(canonical.clone());
                                pending.scope = scope.clone();
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                *index += 1;
            }
            if *index == scopes.len() {
                match self.store.get(
                    *id,
                    format!("extensions/{}", pending.provider),
                    pending.deadline,
                ) {
                    Ok(()) => pending.stage = Stage::Loading,
                    Err(error) => completed.push((*id, error)),
                }
                continue;
            }
            let (kind, scope_id) = match &scopes[*index] {
                Scope::Application(id) => ("application", id),
                Scope::Session(id) => ("session", id),
                _ => unreachable!(),
            };
            let operation_id = format!("registry.scope.{id}.{index}");
            let remaining_ms = pending
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(30_000) as u64;
            let payload = json!({"operation":"resolve_settings_scope","operation_id":operation_id,"target":target,"scope":kind,"scope_id":scope_id,"access":"read","source":{"caller":pending.source_caller,"initiator":pending.source_initiator},"input_binding":input_binding,"input_boot_epoch":input_boot_epoch});
            match runtime.send(Command::Request {
                service: "amaterasu.session-lifecycle/1".into(),
                payload,
                timeout_ms: remaining_ms.max(1),
                operation_id,
                parent: Some(*id),
            }) {
                Ok(Response::Accepted { id }) => *child = Some(id),
                Err(_) => continue,
                _ => completed.push((
                    *id,
                    discovery::failure("permission_denied", "scope authority unavailable"),
                )),
            }
        }
        for (id, pending) in &mut self.pending {
            let Stage::Validating {
                document,
                state_revision,
                snapshots,
                index,
                child,
                outcome,
            } = &mut pending.stage
            else {
                continue;
            };
            if Instant::now() >= pending.deadline {
                if let Some(handle) = *child {
                    let _ = runtime.send(Command::Cancel { request: handle });
                    let _ = runtime.send(Command::ReleaseResult { request: handle });
                }
                completed.push((
                    *id,
                    discovery::failure("deadline_exceeded", "settings validation expired"),
                ));
                continue;
            }
            if let Some(handle) = *child {
                if outcome.is_none() {
                    match runtime.send(Command::TakeResult { request: handle }) {
                        Ok(Response::Pending) | Err(_) => continue,
                        Ok(Response::Result { payload }) => *outcome = Some(payload),
                        Ok(Response::Error { code, message }) => {
                            *outcome = Some(discovery::failure(&code, &message))
                        }
                        _ => {
                            *outcome = Some(discovery::failure(
                                "invalid_response",
                                "invalid validation response",
                            ))
                        }
                    }
                }
                match runtime.send(Command::ReleaseResult { request: handle }) {
                    Ok(Response::Ok) => (),
                    Ok(Response::Error { code, .. }) if code == "not_found" => (),
                    _ => continue,
                }
                let response = outcome.take().expect("result collected");
                *child = None;
                if response.get("ok") != Some(&Value::Bool(true))
                    || response.get("valid") != Some(&Value::Bool(true))
                {
                    completed.push((
                        *id,
                        discovery::failure("invalid_values", "provider rejected settings"),
                    ));
                    continue;
                }
                *index += 1;
            }
            if *index == snapshots.len() {
                match self.store.set(
                    *id,
                    format!("extensions/{}", pending.provider),
                    serde_json::to_value(&*document).map_err(|e| e.to_string())?,
                    *state_revision,
                    pending.deadline,
                ) {
                    Ok(()) => {
                        pending.stage = Stage::Saving {
                            revision: document.revision,
                        }
                    }
                    Err(error) => completed.push((*id, error)),
                }
                continue;
            }
            let remaining_ms = pending
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64;
            match runtime.send(Command::CallApi {
                service: pending.validation.service.clone(),
                method: pending.validation.method.clone(),
                interface_major: pending.validation.interface_major,
                interface_minor: pending.validation.interface_minor,
                payload: json!({"values":snapshots[*index]}),
                timeout_ms: remaining_ms.max(1),
                operation_id: format!("extension-registry.{id}.validate.{index}"),
                parent: Some(*id),
            }) {
                Ok(Response::Accepted { id }) => *child = Some(id),
                Ok(Response::Error { code, message }) => {
                    completed.push((*id, discovery::failure(&code, &message)))
                }
                Err(_) => continue,
                _ => completed.push((
                    *id,
                    discovery::failure("invalid_response", "validation submission failed"),
                )),
            }
        }
        for (id, _) in &completed {
            self.pending.remove(id);
        }
        Ok(completed)
    }
}
