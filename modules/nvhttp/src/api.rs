//! Public operation schemas, independently selected and approved by composition.
use amaterasu_module_sdk::{ApiMethod, operation_api};
use serde_json::Value;
pub(crate) const SERVICE: &str = "nonary.host-settings/1";
pub(crate) fn methods() -> Vec<ApiMethod> {
    let methods: Vec<ApiMethod> = serde_json::from_str(include_str!("../metadata/api.json"))
        .expect("valid module-owned API metadata");
    assert!(methods.iter().all(|method| method.service == SERVICE));
    methods
}
pub(crate) fn decode(payload: Value, service: &str) -> Result<Value, String> {
    let mut decoded = operation_api::decode_delivery(payload, service, crate::SERVICE, &methods())?;
    if service != SERVICE {
        return Ok(decoded);
    }
    match decoded["action"].as_str() {
        Some("read") => Ok(serde_json::json!({"action":"settings_get"})),
        Some("update") => {
            let mut values = decoded["values"]
                .as_object()
                .cloned()
                .ok_or("invalid_settings")?;
            if values.contains_key("action") || values.contains_key("expected_revision") {
                return Err("reserved_settings_field".into());
            }
            values.insert("action".into(), serde_json::json!("settings_update"));
            values.insert(
                "expected_revision".into(),
                decoded["expected_revision"].take(),
            );
            Ok(Value::Object(values))
        }
        _ => Err("unknown_settings_method".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn partial_updates_keep_null_omission_and_revision_distinct() {
        let request = |values| json!({"method":"update","interface_major":1,"interface_minor":0,"arguments":{"values":values,"expected_revision":7}});
        assert_eq!(
            decode(request(json!({"output_name":null})), SERVICE).unwrap(),
            json!({"action":"settings_update","expected_revision":7,"output_name":null})
        );
        assert_eq!(
            decode(
                request(json!({"display_policy":{"dd_hdr_option":"auto"}})),
                SERVICE
            )
            .unwrap(),
            json!({"action":"settings_update","expected_revision":7,"display_policy":{"dd_hdr_option":"auto"}})
        );
        assert!(decode(request(json!({"action":"launch"})), SERVICE).is_err());
        assert!(decode(request(json!({"expected_revision":99})), SERVICE).is_err());
        assert!(decode(json!({"action":"settings_update"}), SERVICE).is_err());
    }
}
