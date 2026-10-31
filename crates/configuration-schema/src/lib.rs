//! Shared configuration definitions and validation used by configuration providers.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, BTreeSet};

pub type Fields = BTreeMap<String, Definition>;
pub type Values = BTreeMap<String, Value>;

const MAX_FIELDS: usize = 128;
const MAX_STRING_LENGTH: usize = 4096;
const MAX_COLLECTION_ITEMS: usize = 4096;
const MAX_SCHEMA_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Definition {
    /// An explicit unset value around an independently validated definition.
    Nullable {
        default: Value,
        inner: Box<Definition>,
    },
    Boolean {
        default: bool,
    },
    Integer {
        default: i64,
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    Decimal {
        default: f64,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    String {
        default: String,
        min_length: Option<usize>,
        max_length: Option<usize>,
    },
    Enum {
        default: String,
        options: Vec<String>,
    },
    List {
        default: Vec<Value>,
        item: Box<Definition>,
        min_items: Option<usize>,
        max_items: Option<usize>,
    },
    Object {
        default: Values,
        fields: Fields,
    },
    TaggedUnion {
        default: Values,
        tag: String,
        variants: BTreeMap<String, Fields>,
    },
}

impl Definition {
    fn default_value(&self) -> Value {
        match self {
            Self::Nullable { default, .. } => default.clone(),
            Self::Boolean { default } => (*default).into(),
            Self::Integer { default, .. } => (*default).into(),
            Self::Decimal { default, .. } => Number::from_f64(*default)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Self::String { default, .. } | Self::Enum { default, .. } => default.clone().into(),
            Self::List { default, .. } => Value::Array(default.clone()),
            Self::Object { default, .. } | Self::TaggedUnion { default, .. } => Value::Object(
                default
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Map<_, _>>(),
            ),
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Nullable { inner, .. } => value.is_null() || inner.accepts(value),
            Self::Boolean { .. } => value.is_boolean(),
            Self::Integer {
                minimum, maximum, ..
            } => value.as_i64().is_some_and(|integer| {
                minimum.is_none_or(|minimum| integer >= minimum)
                    && maximum.is_none_or(|maximum| integer <= maximum)
            }),
            Self::Decimal {
                minimum, maximum, ..
            } => value.as_f64().is_some_and(|decimal| {
                decimal.is_finite()
                    && minimum.is_none_or(|minimum| decimal >= minimum)
                    && maximum.is_none_or(|maximum| decimal <= maximum)
            }),
            Self::String {
                min_length,
                max_length,
                ..
            } => value.as_str().is_some_and(|text| {
                let length = text.chars().count();
                length >= min_length.unwrap_or(0)
                    && length <= max_length.unwrap_or(MAX_STRING_LENGTH)
            }),
            Self::Enum { options, .. } => value
                .as_str()
                .is_some_and(|choice| options.iter().any(|option| option == choice)),
            Self::List {
                item,
                min_items,
                max_items,
                ..
            } => value.as_array().is_some_and(|items| {
                items.len() >= min_items.unwrap_or(0)
                    && items.len() <= max_items.unwrap_or(MAX_COLLECTION_ITEMS)
                    && items.iter().all(|item_value| item.accepts(item_value))
            }),
            Self::Object { fields, .. } => value.as_object().is_some_and(|object| {
                object.len() == fields.len()
                    && fields.iter().all(|(name, definition)| {
                        object
                            .get(name)
                            .is_some_and(|field_value| definition.accepts(field_value))
                    })
            }),
            Self::TaggedUnion { tag, variants, .. } => value.as_object().is_some_and(|object| {
                object
                    .get(tag)
                    .and_then(Value::as_str)
                    .and_then(|variant| variants.get(variant))
                    .is_some_and(|fields| {
                        object.len() == fields.len() + 1
                            && fields.iter().all(|(name, definition)| {
                                object
                                    .get(name)
                                    .is_some_and(|value| definition.accepts(value))
                            })
                    })
            }),
        }
    }
}

pub fn defaults(fields: &Fields) -> Result<Values, String> {
    let mut remaining_fields = MAX_FIELDS;
    defaults_for_fields(fields, 0, &mut remaining_fields, "")
}

fn defaults_for_fields(
    fields: &Fields,
    depth: usize,
    remaining_fields: &mut usize,
    parent_path: &str,
) -> Result<Values, String> {
    if fields.len() > *remaining_fields {
        return Err(format!("schema exceeds {MAX_FIELDS} total fields"));
    }
    *remaining_fields -= fields.len();

    let mut values = Values::new();
    for (name, definition) in fields {
        validate_field_name(name)?;
        let path = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}.{name}")
        };
        validate_definition(definition, &path, depth, remaining_fields)?;

        let default = definition.default_value();
        if !definition.accepts(&default) {
            return Err(format!("default violates field bounds: {path}"));
        }
        values.insert(name.clone(), default);
    }
    Ok(values)
}

fn validate_definition(
    definition: &Definition,
    path: &str,
    depth: usize,
    remaining_fields: &mut usize,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("schema exceeds maximum nesting depth: {path}"));
    }

    match definition {
        Definition::Nullable { inner, .. } => {
            if depth == MAX_SCHEMA_DEPTH {
                return Err(format!("schema exceeds maximum nesting depth: {path}"));
            }
            validate_definition(inner, path, depth + 1, remaining_fields)?;
        }
        Definition::Boolean { .. } | Definition::Integer { .. } => {}
        Definition::Decimal {
            minimum, maximum, ..
        } => {
            if minimum.is_some_and(|value| !value.is_finite())
                || maximum.is_some_and(|value| !value.is_finite())
                || minimum
                    .zip(*maximum)
                    .is_some_and(|(minimum, maximum)| minimum > maximum)
            {
                return Err(format!("invalid decimal bounds: {path}"));
            }
        }
        Definition::String {
            min_length,
            max_length,
            ..
        } => {
            if min_length.unwrap_or(0) > max_length.unwrap_or(MAX_STRING_LENGTH)
                || max_length.unwrap_or(MAX_STRING_LENGTH) > MAX_STRING_LENGTH
            {
                return Err(format!("invalid string bounds: {path}"));
            }
        }
        Definition::Enum { options, .. } => {
            if options.is_empty() || options.len() > MAX_COLLECTION_ITEMS {
                return Err(format!(
                    "enum options must contain 1..={MAX_COLLECTION_ITEMS} entries: {path}"
                ));
            }
            let mut unique_options = BTreeSet::new();
            for option in options {
                if option.chars().count() > MAX_STRING_LENGTH || !unique_options.insert(option) {
                    return Err(format!("invalid or duplicate enum option: {path}"));
                }
            }
        }
        Definition::List {
            default,
            item,
            min_items,
            max_items,
        } => {
            let maximum = max_items.unwrap_or(MAX_COLLECTION_ITEMS);
            if min_items.unwrap_or(0) > maximum || maximum > MAX_COLLECTION_ITEMS {
                return Err(format!("invalid list bounds: {path}"));
            }
            if default.len() > MAX_COLLECTION_ITEMS {
                return Err(format!(
                    "list default exceeds {MAX_COLLECTION_ITEMS} items: {path}"
                ));
            }
            validate_definition(item, path, depth + 1, remaining_fields)?;
        }
        Definition::Object { fields, .. } => {
            if depth == MAX_SCHEMA_DEPTH {
                return Err(format!("schema exceeds maximum nesting depth: {path}"));
            }
            defaults_for_fields(fields, depth + 1, remaining_fields, path)?;
        }
        Definition::TaggedUnion { tag, variants, .. } => {
            if depth == MAX_SCHEMA_DEPTH {
                return Err(format!("schema exceeds maximum nesting depth: {path}"));
            }
            validate_field_name(tag)?;
            if variants.is_empty() || variants.len() > MAX_COLLECTION_ITEMS {
                return Err(format!("invalid tagged union variants: {path}"));
            }
            for (variant, fields) in variants {
                if variant.is_empty() || variant.chars().count() > MAX_STRING_LENGTH {
                    return Err(format!("invalid tagged union variant: {path}"));
                }
                if fields.contains_key(tag) {
                    return Err(format!("tagged union tag conflicts with field: {path}"));
                }
                defaults_for_fields(fields, depth + 1, remaining_fields, path)?;
            }
        }
    }

    let default = definition.default_value();
    if !definition.accepts(&default) {
        return Err(format!("default violates field bounds: {path}"));
    }
    Ok(())
}

fn validate_field_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!("invalid field name: {name}"));
    }
    Ok(())
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
    fn nullable_values_are_explicit_and_inner_schema_is_always_validated() {
        let wire = json!({"hotkey":{"type":"nullable","default":null,"inner":{"type":"object","default":{"key":"K"},"fields":{"key":{"type":"string","default":"K","min_length":1,"max_length":8}}}}});
        let fields: Fields = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&fields).unwrap(), wire);
        assert_eq!(defaults(&fields).unwrap()["hotkey"], Value::Null);
        for value in [json!(null), json!({"key":"A"})] {
            assert!(validate_values(&fields, &[("hotkey".into(), value)].into()).is_ok());
        }
        for value in [
            json!("A"),
            json!({"key":""}),
            json!({"key":"A","extra":true}),
        ] {
            assert!(validate_values(&fields, &[("hotkey".into(), value)].into()).is_err());
        }
        let bad: Fields = serde_json::from_value(json!({"hotkey":{"type":"nullable","default":null,"inner":{"type":"string","default":"x","min_length":3,"max_length":1}}})).unwrap();
        assert!(defaults(&bad).is_err());
        let bad_default: Fields = serde_json::from_value(json!({"hotkey":{"type":"nullable","default":2,"inner":{"type":"boolean","default":false}}})).unwrap();
        assert!(defaults(&bad_default).is_err());
        let mut nested = json!({"type":"boolean","default":false});
        for _ in 0..=MAX_SCHEMA_DEPTH {
            nested = json!({"type":"nullable","default":null,"inner":nested});
        }
        let deep: Fields = serde_json::from_value(json!({"hotkey":nested})).unwrap();
        assert!(defaults(&deep).is_err());
        let legacy = json!({"type":"boolean","default":false});
        let legacy_decoded: Definition = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&legacy_decoded).unwrap(), legacy);
        assert!(!legacy_decoded.accepts(&Value::Null));
    }

    #[test]
    fn tagged_union_validates_selected_variant_and_round_trips() {
        let wire = json!({"key": {
            "type":"tagged_union", "tag":"kind",
            "default":{"kind":"physical","code":30},
            "variants":{
                "physical":{"code":{"type":"integer","default":30,"minimum":0,"maximum":255}},
                "named":{"name":{"type":"string","default":"Enter","min_length":1,"max_length":32}}
            }
        }});
        let fields: Fields = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&fields).unwrap(), wire);
        assert_eq!(
            defaults(&fields).unwrap()["key"],
            json!({"kind":"physical","code":30})
        );
        validate_values(
            &fields,
            &serde_json::from_value(json!({"key":{"kind":"named","name":"Escape"}})).unwrap(),
        )
        .unwrap();
        for value in [
            json!({"kind":"physical","code":256}),
            json!({"kind":"physical"}),
            json!({"kind":"named","name":"Enter","code":30}),
            json!({"kind":"other"}),
            json!({"kind":1}),
        ] {
            assert!(validate_values(&fields, &[("key".into(), value)].into()).is_err());
        }
    }

    #[test]
    fn rejects_invalid_tagged_union_schemas_and_nested_limits() {
        for union in [
            json!({"type":"tagged_union","tag":"kind","default":{"kind":"a"},"variants":{}}),
            json!({"type":"tagged_union","tag":"kind","default":{"kind":"missing"},"variants":{"a":{}}}),
            json!({"type":"tagged_union","tag":"kind","default":{"kind":"a"},"variants":{"a":{"kind":{"type":"boolean","default":true}}}}),
            json!({"type":"tagged_union","tag":"kind","default":{"kind":"a"},"variants":{"a":{},"b":{"bad":{"type":"integer","default":0,"minimum":1}}}}),
        ] {
            let fields: Fields = serde_json::from_value(json!({"key": union})).unwrap();
            assert!(defaults(&fields).is_err());
        }
        let mut nested = Definition::Boolean { default: true };
        for _ in 0..=MAX_SCHEMA_DEPTH {
            nested = Definition::TaggedUnion {
                default: [
                    ("kind".into(), json!("a")),
                    ("child".into(), nested.default_value()),
                ]
                .into(),
                tag: "kind".into(),
                variants: [("a".into(), [("child".into(), nested)].into())].into(),
            };
        }
        assert!(defaults(&[("key".into(), nested)].into()).is_err());
    }

    #[test]
    fn preserves_the_schema_wire_format() {
        let wire = json!({
            "enabled": {"type":"boolean","default":true},
            "count": {"type":"integer","default":2,"minimum":1,"maximum":null},
            "scale": {"type":"decimal","default":1.25,"minimum":null,"maximum":2.0},
            "title": {"type":"string","default":"hello","min_length":null,"max_length":10},
            "quality": {"type":"enum","default":"fast","options":["fast","quality"]},
            "tags": {"type":"list","default":["one"],"item":{"type":"string","default":"","min_length":null,"max_length":null},"min_items":null,"max_items":4},
            "display": {"type":"object","default":{"enabled":true},"fields":{"enabled":{"type":"boolean","default":true}}}
        });
        let fields: Fields = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&fields).unwrap(), wire);
        assert!(defaults(&fields).is_ok());
    }

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

    #[test]
    fn validates_decimal_enum_list_and_object_values() {
        let fields: Fields = serde_json::from_value(json!({
            "scale": {"type":"decimal","default":1.25,"minimum":0.5,"maximum":2.0},
            "quality": {"type":"enum","default":"balanced","options":["fast","balanced","quality"]},
            "tags": {
                "type":"list","default":["gaming"],"item":{"type":"string","default":"","max_length":16},
                "min_items":1,"max_items":4
            },
            "display": {
                "type":"object","default":{"enabled":true,"width":1920},
                "fields":{"enabled":{"type":"boolean","default":true},"width":{"type":"integer","default":1280,"minimum":640}}
            }
        }))
        .unwrap();

        assert_eq!(defaults(&fields).unwrap()["scale"], 1.25);
        for valid in [
            json!({"scale":0.5,"quality":"fast","tags":["one","two"],"display":{"enabled":false,"width":640}}),
            json!({"scale":2,"quality":"quality","tags":["x"],"display":{"enabled":true,"width":4096}}),
        ] {
            validate_values(&fields, &serde_json::from_value(valid).unwrap()).unwrap();
        }
        for invalid in [
            json!({"scale":2.01}),
            json!({"quality":"cinema"}),
            json!({"tags":[]}),
            json!({"tags":["longer than sixteen characters"]}),
            json!({"display":{"enabled":true}}),
            json!({"display":{"enabled":true,"width":1280,"extra":1}}),
        ] {
            assert!(validate_values(&fields, &serde_json::from_value(invalid).unwrap()).is_err());
        }
    }

    #[test]
    fn rejects_invalid_nested_schemas_and_collection_bounds() {
        for invalid in [
            json!({"weight":{"type":"decimal","default":2.0,"minimum":3.0,"maximum":1.0}}),
            json!({"quality":{"type":"enum","default":"fast","options":["fast","fast"]}}),
            json!({"values":{"type":"list","default":[],"item":{"type":"integer","default":0},"min_items":3,"max_items":2}}),
            json!({"settings":{"type":"object","default":{"enabled":1},"fields":{"enabled":{"type":"boolean","default":true}}}}),
            json!({"outer":{"type":"object","default":{"bad":true},"fields":{"bad-name":{"type":"boolean","default":true}}}}),
        ] {
            let fields: Fields = serde_json::from_value(invalid).unwrap();
            assert!(defaults(&fields).is_err());
        }
    }

    #[test]
    fn limits_fields_and_schema_nesting() {
        let too_many: Fields = (0..=MAX_FIELDS)
            .map(|index| {
                (
                    format!("field_{index}"),
                    Definition::Boolean { default: false },
                )
            })
            .collect();
        assert!(defaults(&too_many).is_err());

        let mut nested = Definition::Boolean { default: true };
        for _ in 0..MAX_SCHEMA_DEPTH {
            let child = nested;
            nested = Definition::Object {
                default: [("child".into(), child.default_value())].into(),
                fields: [("child".into(), child)].into(),
            };
        }
        assert!(defaults(&[("nested".into(), nested.clone())].into()).is_ok());

        nested = Definition::Object {
            default: [("child".into(), nested.default_value())].into(),
            fields: [("child".into(), nested)].into(),
        };
        assert!(defaults(&[("nested".into(), nested)].into()).is_err());
    }
}
