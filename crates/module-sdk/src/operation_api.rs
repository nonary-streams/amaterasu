//! Versioned API adapters for typed operation services. Providers own operation
//! schemas and caller authorization; this adapter never supplies a default.
use crate::{ApiFieldKind, ApiMethod, ApiPrivilege, ApiSchema, ModuleResult};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn method(
    service: &str,
    name: &str,
    required: &[(&str, ApiFieldKind)],
    optional: &[(&str, ApiFieldKind)],
) -> ApiMethod {
    ApiMethod {
        service: service.into(),
        method: name.into(),
        interface_major: 1,
        interface_minor: 0,
        request: ApiSchema {
            required: required
                .iter()
                .map(|(k, v)| ((*k).into(), v.clone()))
                .collect(),
            optional: optional
                .iter()
                .map(|(k, v)| ((*k).into(), v.clone()))
                .collect(),
            allow_extra: false,
        },
        response: ApiSchema {
            required: BTreeMap::from([("ok".into(), ApiFieldKind::Boolean)]),
            optional: BTreeMap::new(),
            allow_extra: true,
        },
        privilege: ApiPrivilege::User,
    }
}

/// Decode a declared API envelope into its provider-owned operation. Legacy
/// requests remain supported by the same provider authorization checks.
pub fn decode(payload: Value, methods: &[ApiMethod]) -> ModuleResult<Value> {
    if payload.get("method").is_none() {
        return Ok(payload);
    }
    let envelope = payload
        .as_object()
        .filter(|o| o.len() == 4)
        .ok_or("invalid API envelope")?;
    let name = envelope
        .get("method")
        .and_then(Value::as_str)
        .ok_or("invalid API method")?;
    let selected = methods
        .iter()
        .find(|m| m.method == name)
        .ok_or("undeclared API method")?;
    if envelope.get("interface_major") != Some(&Value::from(selected.interface_major))
        || envelope.get("interface_minor") != Some(&Value::from(selected.interface_minor))
    {
        return Err("unsupported API version".into());
    }
    let mut arguments = envelope
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .ok_or("invalid API arguments")?;
    if arguments.contains_key("action")
        || arguments.keys().any(|key| {
            !selected.request.required.contains_key(key)
                && !selected.request.optional.contains_key(key)
        })
        || selected
            .request
            .required
            .keys()
            .any(|key| !arguments.contains_key(key))
    {
        return Err("invalid API arguments".into());
    }
    arguments.insert("action".into(), Value::String(name.into()));
    Ok(Value::Object(arguments))
}

/// Legacy operation requests are accepted only on the original service.
/// An API alias cannot be used as an unrestricted raw-operation endpoint.
pub fn decode_delivery(
    payload: Value,
    delivery_service: &str,
    legacy_service: &str,
    methods: &[ApiMethod],
) -> ModuleResult<Value> {
    if delivery_service == legacy_service {
        if payload.get("method").is_some() {
            return Err("API envelope requires its declared service".into());
        }
        return Ok(payload);
    }
    if payload.get("method").is_none() {
        return Err("invalid operation API delivery".into());
    }
    let selected: Vec<_> = methods
        .iter()
        .filter(|method| method.service == delivery_service)
        .cloned()
        .collect();
    if selected.is_empty() {
        return Err("undeclared operation API service".into());
    }
    decode(payload, &selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn aliases_cannot_change_operations_or_smuggle_fields() {
        let methods = [method("example/1", "read", &[], &[])];
        let request =
            json!({"method":"read","interface_major":1,"interface_minor":0,"arguments":{}});
        assert_eq!(
            decode(request.clone(), &methods).unwrap(),
            json!({"action":"read"})
        );
        for arguments in [json!({"action":"write"}), json!({"other":true})] {
            let mut invalid = request.clone();
            invalid["arguments"] = arguments;
            assert!(decode(invalid, &methods).is_err());
        }
        let mut invalid = request;
        invalid["method"] = "write".into();
        assert!(decode(invalid, &methods).is_err());
    }
}
