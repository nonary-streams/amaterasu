//! Public operation schemas, independently selected and approved by composition.
use amaterasu_module_sdk::{ApiMethod, operation_api};
use serde_json::Value;
pub(crate) const SERVICE: &str = "nonary.catalog/1";
pub(crate) fn methods() -> Vec<ApiMethod> {
    let methods: Vec<ApiMethod> = serde_json::from_str(include_str!("../metadata/api.json"))
        .expect("valid module-owned API metadata");
    assert!(methods.iter().all(|method| method.service == SERVICE));
    methods
}
pub(crate) fn decode(payload: Value, service: &str) -> Result<Value, String> {
    operation_api::decode_delivery(payload, service, crate::SERVICE, &methods())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn public_aliases_require_their_exact_envelope_and_service() {
        let name = methods()[0].method.clone();
        let arguments = if name == "get" {
            json!({"id":"app-1"})
        } else {
            json!({})
        };
        let request =
            json!({"method":name,"interface_major":1,"interface_minor":0,"arguments":arguments});
        assert!(decode(request.clone(), SERVICE).is_ok());
        assert!(decode(request.clone(), crate::SERVICE).is_err());
        assert!(decode(json!({"action":name}), SERVICE).is_err());
        assert!(decode(request.clone(), "foreign.service/1").is_err());
        let mut invalid = request;
        invalid["arguments"]["action"] = json!("apply");
        assert!(decode(invalid, SERVICE).is_err());
    }
}
