//! Storage bounds for options a consumer does not own. These values are inert:
//! only their provider may interpret, validate or migrate their semantics.
use serde_json::{Map, Value};

pub fn validate(values: &Map<String, Value>) -> Result<(), &'static str> {
    if values.len() > 128
        || values
            .keys()
            .any(|key| key.is_empty() || key.len() > 128 || key.chars().any(char::is_control))
        || serde_json::to_vec(values).map_or(true, |bytes| bytes.len() > 65_536)
    {
        return Err("retained_options_exceed_storage_bounds");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_are_opaque_but_storage_is_bounded() {
        let mut values = json!({"vendor_option":{"enabled":false,"rate":0,"mode":null}})
            .as_object()
            .unwrap()
            .clone();
        assert!(validate(&values).is_ok());
        values.insert("large".into(), json!("x".repeat(65_536)));
        assert!(validate(&values).is_err());
        values.clear();
        values.insert("bad\nkey".into(), Value::Null);
        assert!(validate(&values).is_err());
    }
}
