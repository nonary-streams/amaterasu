use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type Fields = BTreeMap<String, Definition>;
pub type Values = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Definition {
    Boolean {
        default: bool,
    },
    Integer {
        default: i64,
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    String {
        default: String,
        min_length: Option<usize>,
        max_length: Option<usize>,
    },
}

impl Definition {
    fn default_value(&self) -> Value {
        match self {
            Self::Boolean { default } => (*default).into(),
            Self::Integer { default, .. } => (*default).into(),
            Self::String { default, .. } => default.clone().into(),
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Boolean { .. } => value.is_boolean(),
            Self::Integer {
                minimum, maximum, ..
            } => value.as_i64().is_some_and(|integer| {
                minimum.is_none_or(|minimum| integer >= minimum)
                    && maximum.is_none_or(|maximum| integer <= maximum)
            }),
            Self::String {
                min_length,
                max_length,
                ..
            } => value.as_str().is_some_and(|text| {
                let length = text.chars().count();
                length >= min_length.unwrap_or(0) && length <= max_length.unwrap_or(4096)
            }),
        }
    }
}

pub fn defaults(fields: &Fields) -> Result<Values, String> {
    if fields.len() > 128 {
        return Err("schema exceeds 128 fields".into());
    }
    let mut values = Values::new();
    for (name, definition) in fields {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(format!("invalid field name: {name}"));
        }
        if let Definition::String {
            min_length,
            max_length,
            ..
        } = definition
            && (min_length.unwrap_or(0) > max_length.unwrap_or(4096)
                || max_length.unwrap_or(4096) > 4096)
        {
            return Err(format!("invalid string bounds: {name}"));
        }
        let default = definition.default_value();
        if !definition.accepts(&default) {
            return Err(format!("default violates field bounds: {name}"));
        }
        values.insert(name.clone(), default);
    }
    Ok(values)
}

pub fn validate_values(fields: &Fields, values: &Values) -> Result<(), String> {
    for (name, value) in values {
        let definition = fields
            .get(name)
            .ok_or_else(|| format!("unknown field: {name}"))?;
        if !definition.accepts(value) {
            return Err(format!("invalid value for field: {name}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_types_bounds_defaults_and_unknown_fields() {
        let fields: Fields = serde_json::from_value(json!({
            "enabled": {"type":"boolean","default":true},
            "count": {"type":"integer","default":2,"minimum":1,"maximum":4},
            "title": {"type":"string","default":"é","max_length":2}
        }))
        .unwrap();
        assert_eq!(defaults(&fields).unwrap()["count"], 2);
        for invalid in [
            json!({"count":2.5}),
            json!({"count":5}),
            json!({"enabled":1}),
            json!({"title":"abc"}),
            json!({"unknown":true}),
        ] {
            assert!(validate_values(&fields, &serde_json::from_value(invalid).unwrap()).is_err());
        }
        assert!(
            validate_values(
                &fields,
                &serde_json::from_value(json!({"title":"éé"})).unwrap()
            )
            .is_ok()
        );
        let invalid: Fields =
            serde_json::from_value(json!({"count":{"type":"integer","default":0,"minimum":1}}))
                .unwrap();
        assert!(defaults(&invalid).is_err());
        assert!(
            serde_json::from_value::<Definition>(
                json!({"type":"boolean","default":true,"extra":1})
            )
            .is_err()
        );
    }
}
