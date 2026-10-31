//! Asynchronous settings validation and state CAS.
use crate::{
    discovery,
    persistence::Store,
    policy::Settings,
    settings::{self, Document, Scope},
};
use amaterasu_configuration_schema::{Fields, Values};
use amaterasu_extension_api::{MethodRef, SettingScope};
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
    writable_scopes: BTreeSet<Scope>,
    stage: Stage,
}

enum Stage {
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
        operation: SettingsOperation,
    ) -> Result<(), Value> {
        let (caller, initiator) = identities;
        if self.pending.len() >= MAX_PENDING {
            return Err(discovery::failure(
                "resource_exhausted",
                "settings queue is full",
            ));
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
        let item = policy
            .descriptors
            .iter()
            .find(|item| {
                item.descriptor.provider == *provider
                    && item.allows(initiator, scope, write)
                    && item.allows(caller, scope, write)
            })
            .ok_or_else(|| discovery::failure("permission_denied", "scope is not authorized"))?;
        if let SettingsOperation::Get {
            application,
            paired_client,
            session,
            ..
        } = &operation
        {
            for selected in application
                .as_ref()
                .map(|id| Scope::Application(id.clone()))
                .into_iter()
                .chain(
                    paired_client
                        .as_ref()
                        .map(|id| Scope::PairedClient(id.clone())),
                )
                .chain(session.as_ref().map(|id| Scope::Session(id.clone())))
            {
                if !item.allows(initiator, &selected, false)
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
        self.store
            .get(parent, format!("extensions/{provider}"), deadline)?;
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
                writable_scopes: item
                    .grants
                    .iter()
                    .filter(|grant| {
                        grant.write
                            && grant.caller == initiator
                            && item.allows(caller, &grant.scope, true)
                    })
                    .map(|grant| grant.scope.clone())
                    .collect(),
                stage: Stage::Loading,
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
                            .any(|layer| !pending.writable_scopes.contains(&layer.scope))
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
                            || replacements
                                .keys()
                                .any(|scope| !pending.writable_scopes.contains(scope))
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
                Stage::Validating { .. } => {
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
