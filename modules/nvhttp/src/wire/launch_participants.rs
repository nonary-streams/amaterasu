//! Explicit startup bindings for launch-option participants. Bindings are
//! trusted composition data, not fields selected by the remote client.
use amaterasu_module_sdk::{
    Command,
    extension_api::{SchemaRef, schemas::SchemaCatalog},
    launch_options::{BoundOption, LaunchOption, LaunchOptions, ParticipantBinding},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Clone)]
pub struct Pending {
    pub binding: Binding,
    pub fields: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub entitlement: String,
    pub fields: Vec<String>,
    pub namespace: String,
    pub version: u32,
    pub output_schema: SchemaRef,
    pub consumer_service: String,
    pub consumer_schema: SchemaRef,
    pub schema_document: String,
}

pub fn validate(bindings: &[Binding]) -> Result<(), &'static str> {
    if bindings.len() > 16 {
        return Err("too_many_launch_participants");
    }
    let mut fields = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    for binding in bindings {
        let service_version = binding
            .service
            .rsplit_once('/')
            .and_then(|(name, version)| {
                (name.contains('.')
                    && !name.starts_with("amaterasu.")
                    && amaterasu_contracts::valid_name(name))
                .then(|| version.parse::<u32>().ok())
                .flatten()
            });
        if service_version != Some(binding.interface_major)
            || binding.interface_major == 0
            || !amaterasu_contracts::valid_name(&binding.method)
            || binding.method.contains('/')
            || !amaterasu_identity::valid_entitlement(&binding.entitlement)
            || binding.consumer_service != "amaterasu.rtsp/1"
            || binding.output_schema != binding.consumer_schema
            || binding.fields.is_empty()
            || binding.fields.len() > 24
            || !namespaces.insert(&binding.namespace)
        {
            return Err("invalid_launch_participant");
        }
        for field in &binding.fields {
            if !fields.insert(field) {
                return Err("invalid_launch_participant");
            }
        }
        let dummy = LaunchOption {
            namespace: binding.namespace.clone(),
            version: binding.version,
            value: Value::Null,
        };
        LaunchOptions::validate(vec![dummy], |_, _| true)
            .map_err(|_| "invalid_launch_participant")?;
        SchemaCatalog::compile([(
            binding.output_schema.clone(),
            binding.schema_document.as_str(),
        )])
        .map_err(|_| "invalid_launch_participant")?;
    }
    let claims: Vec<Vec<&str>> = bindings
        .iter()
        .map(|binding| binding.fields.iter().map(String::as_str).collect())
        .collect();
    let claims: Vec<&[&str]> = claims.iter().map(Vec::as_slice).collect();
    super::streaming::validate_claims(&claims)?;
    Ok(())
}

impl Binding {
    pub fn call(
        &self,
        fields: &std::collections::BTreeMap<String, String>,
        resume: bool,
        timeout_ms: u64,
        operation_id: String,
    ) -> Result<Command, &'static str> {
        let arguments = json!({"version":1,"resume":resume,"fields":fields});
        let wrapper = json!({"method":self.method,"interface_major":self.interface_major,"interface_minor":self.interface_minor,"arguments":arguments});
        if !(1..=30_000).contains(&timeout_ms)
            || !amaterasu_contracts::valid_name(&operation_id)
            || serde_json::to_vec(&wrapper).map_or(true, |bytes| bytes.len() > 16_384)
        {
            return Err("invalid_launch_participant_request");
        }
        Ok(Command::CallApi {
            service: self.service.clone(),
            method: self.method.clone(),
            interface_major: self.interface_major,
            interface_minor: self.interface_minor,
            payload: arguments,
            timeout_ms,
            operation_id,
            parent: None,
        })
    }

    pub fn validate_reply(&self, reply: Value) -> Result<LaunchOption, &'static str> {
        let option: LaunchOption =
            serde_json::from_value(reply).map_err(|_| "invalid_launch_participant_reply")?;
        let schemas =
            SchemaCatalog::compile([(self.output_schema.clone(), self.schema_document.as_str())])
                .map_err(|_| "invalid_launch_participant")?;
        let snapshot = LaunchOptions::validate_bound(
            vec![BoundOption {
                binding: ParticipantBinding {
                    namespace: &self.namespace,
                    version: self.version,
                    schema: &self.output_schema,
                },
                option,
            }],
            &schemas,
            |_, _, schema| schema == &self.consumer_schema,
        )
        .map_err(|_| "invalid_launch_participant_reply")?;
        Ok(snapshot.entries()[0].clone())
    }
}
