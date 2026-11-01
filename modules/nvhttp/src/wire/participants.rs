//! Startup bindings from exact HTTP operations to already approved broker APIs.
//! These bindings select no provider by themselves; the broker's installed
//! API approval and call grant remain authoritative.
use amaterasu_module_sdk::{Command, protocol_handler};
use serde::Deserialize;
use std::collections::BTreeSet;

const MAX_BINDINGS: usize = 32;
const RESERVED_PATHS: &[&str] = &[
    "/serverinfo",
    "/pair",
    "/applist",
    "/appasset",
    "/launch",
    "/cancel",
    "/resume",
    "/unpair",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub path: String,
    pub http_method: String,
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub schema: String,
    pub entitlement: String,
    #[serde(default)]
    pub upgrade: Option<UpgradeProfile>,
}

/// Trusted startup policy for a byte-stream upgrade; no peer-selected service,
/// schema or protocol name is accepted.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeProfile {
    pub protocol: String,
    pub input_format: amaterasu_module_sdk::data_plane::abi::Schema,
    pub output_format: amaterasu_module_sdk::data_plane::abi::Schema,
    pub metadata_schema: amaterasu_module_sdk::data_plane::abi::Schema,
}

pub fn validate(bindings: &[Binding]) -> Result<(), &'static str> {
    if bindings.len() > MAX_BINDINGS {
        return Err("too_many_protocol_bindings");
    }
    let mut claims = BTreeSet::new();
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
        let valid_path = binding.path.starts_with('/')
            && binding.path.len() <= 128
            && binding.path != "/"
            && binding.path.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'/' | b'-' | b'_')
            })
            && !binding.path.contains("//")
            && !RESERVED_PATHS.contains(&binding.path.as_str());
        if !valid_path
            || !amaterasu_contracts::valid_name(&binding.service)
            || binding.http_method.is_empty()
            || binding.http_method.len() > 16
            || !binding
                .http_method
                .bytes()
                .all(|byte| byte.is_ascii_uppercase())
            || service_version != Some(binding.interface_major)
            || binding.interface_major == 0
            || !amaterasu_contracts::valid_name(&binding.method)
            || binding.method.contains('/')
            || !amaterasu_contracts::valid_name(&binding.schema)
            || !amaterasu_identity::valid_entitlement(&binding.entitlement)
            || binding.upgrade.as_ref().is_some_and(|profile| {
                profile.protocol.is_empty()
                    || profile.protocol.len() > 64
                    || !profile
                        .protocol
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
                    || [
                        profile.input_format,
                        profile.output_format,
                        profile.metadata_schema,
                    ]
                    .iter()
                    .any(|schema| schema.namespace == 0 || schema.value == 0 || schema.version == 0)
            })
            || !claims.insert((&binding.path, &binding.http_method))
        {
            return Err("invalid_protocol_binding");
        }
    }
    Ok(())
}

impl Binding {
    /// Validate the exact JSON payload that `CallApi` enqueues in the broker.
    /// The wrapper must match the broker's method/version/arguments envelope.
    pub fn accepts(&self, request: &protocol_handler::Request) -> bool {
        request.validate()
            && serde_json::to_vec(&serde_json::json!({
                "method": self.method,
                "interface_major": self.interface_major,
                "interface_minor": self.interface_minor,
                "arguments": request,
            }))
            .is_ok_and(|bytes| bytes.len() <= protocol_handler::MAX_MESSAGE_BYTES)
    }

    /// Build an upgrade call from the owner-created offer and this exact
    /// configured profile. The broker still enforces provider and caller grants.
    pub fn call_upgrade(
        &self,
        offer: &protocol_handler::UpgradeOffer,
        timeout_ms: u64,
        operation_id: String,
    ) -> Result<Command, &'static str> {
        let profile = self.upgrade.as_ref().ok_or("upgrade_not_configured")?;
        if !offer.validate()
            || offer.request.schema != self.schema
            || offer.request.method != self.http_method
            || offer.request.target.split('?').next() != Some(self.path.as_str())
            || offer.input.format != profile.input_format
            || offer.output.format != profile.output_format
            || offer.input.metadata_schema != profile.metadata_schema
            || offer.output.metadata_schema != profile.metadata_schema
            || !(1..=30_000).contains(&timeout_ms)
            || !amaterasu_contracts::valid_name(&operation_id)
        {
            return Err("invalid_upgrade_offer");
        }
        let payload = serde_json::to_value(offer).map_err(|_| "invalid_upgrade_offer")?;
        let wrapped = serde_json::json!({"method": self.method, "interface_major": self.interface_major,
            "interface_minor": self.interface_minor, "arguments": payload});
        if serde_json::to_vec(&wrapped).map_or(true, |bytes| {
            bytes.len() > protocol_handler::MAX_MESSAGE_BYTES
        }) {
            return Err("invalid_upgrade_offer");
        }
        Ok(Command::CallApi {
            service: self.service.clone(),
            method: self.method.clone(),
            interface_major: self.interface_major,
            interface_minor: self.interface_minor,
            payload,
            timeout_ms,
            operation_id,
            parent: None,
        })
    }

    /// Construct a broker call only from the trusted binding, never a service
    /// or method supplied by the HTTP client. Authentication and authorization
    /// must be checked by the protocol owner before this method is called.
    pub fn call(
        &self,
        request: &protocol_handler::Request,
        timeout_ms: u64,
        operation_id: String,
    ) -> Result<Command, &'static str> {
        if self.upgrade.is_some()
            || !self.accepts(request)
            || request.schema != self.schema
            || request.method != self.http_method
            || request.target.split('?').next() != Some(self.path.as_str())
            || !(1..=30_000).contains(&timeout_ms)
            || !amaterasu_contracts::valid_name(&operation_id)
        {
            return Err("invalid_protocol_request");
        }
        let payload = serde_json::to_value(request).map_err(|_| "invalid_protocol_request")?;
        Ok(Command::CallApi {
            service: self.service.clone(),
            method: self.method.clone(),
            interface_major: self.interface_major,
            interface_minor: self.interface_minor,
            payload,
            timeout_ms,
            operation_id,
            parent: None,
        })
    }
}

pub fn select<'a>(bindings: &'a [Binding], method: &str, target: &str) -> Option<&'a Binding> {
    let path = target.split('?').next()?;
    bindings
        .iter()
        .find(|binding| binding.http_method == method && binding.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding(path: &str, http_method: &str) -> Binding {
        Binding {
            path: path.into(),
            http_method: http_method.into(),
            service: "org.example.handler/1".into(),
            method: "handle".into(),
            interface_major: 1,
            interface_minor: 0,
            schema: "org.example.request/1".into(),
            entitlement: "org.example/read".into(),
            upgrade: None,
        }
    }
    #[test]
    fn selects_exact_method_and_path_and_rejects_conflicts() {
        let bindings = vec![binding("/neutral", "GET"), binding("/neutral", "POST")];
        assert!(validate(&bindings).is_ok());
        assert!(validate(&[binding("/neutral", "PUT")]).is_ok());
        assert_eq!(
            select(&bindings, "POST", "/neutral?key=value")
                .unwrap()
                .http_method,
            "POST"
        );
        assert!(select(&bindings, "GET", "/neutral/other").is_none());
        assert!(validate(&[binding("/neutral", "GET"), binding("/neutral", "GET")]).is_err());
        assert!(validate(&[binding("/launch", "POST")]).is_err());
    }
    #[test]
    fn refuses_invalid_authority_or_api_bindings() {
        let mut value = binding("/neutral", "GET");
        value.entitlement = "unscoped".into();
        assert!(validate(&[value.clone()]).is_err());
        value.entitlement = "org.example/read".into();
        value.interface_major = 2;
        assert!(validate(&[value]).is_err());
    }
    #[test]
    fn upgrade_profile_requires_safe_protocol_and_nonzero_schemas() {
        let schema = amaterasu_module_sdk::data_plane::abi::Schema {
            namespace: 1,
            value: 1,
            version: 1,
        };
        let mut value = binding("/neutral", "GET");
        value.upgrade = Some(UpgradeProfile {
            protocol: "neutral".into(),
            input_format: schema,
            output_format: schema,
            metadata_schema: schema,
        });
        assert!(validate(&[value.clone()]).is_ok());
        value.upgrade.as_mut().unwrap().protocol = "neutral\r\nInjected".into();
        assert!(validate(&[value.clone()]).is_err());
        value.upgrade.as_mut().unwrap().protocol = "neutral".into();
        value.upgrade.as_mut().unwrap().metadata_schema.namespace = 0;
        assert!(validate(&[value]).is_err());
    }

    #[test]
    fn call_uses_selected_api_and_rejects_mismatched_input() {
        let binding = binding("/neutral", "POST");
        let mut request = protocol_handler::Request {
            version: 1,
            schema: binding.schema.clone(),
            method: "POST".into(),
            target: "/neutral?value=1".into(),
            body_base64: String::new(),
            peer: protocol_handler::VerifiedPeer {
                remote: "127.0.0.1".parse().unwrap(),
                certificate_der_base64: "AQ==".into(),
            },
        };
        match binding.call(&request, 1000, "test.call.1".into()).unwrap() {
            Command::CallApi {
                service,
                method,
                interface_major,
                interface_minor,
                ..
            } => {
                assert_eq!(service, "org.example.handler/1");
                assert_eq!(method, "handle");
                assert_eq!((interface_major, interface_minor), (1, 0));
            }
            _ => panic!("expected approved API invocation"),
        }
        request.target = "/another".into();
        assert!(binding.call(&request, 1000, "test.call.1".into()).is_err());
    }
}
