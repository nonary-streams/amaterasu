//! A bounded, offline subset of JSON Schema 2020-12 for pinned owner contracts.
//! Unsupported keywords and references are rejected at compilation.
use crate::{SchemaRef, validate_schema_ref};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 1024;
const MAX_INSTANCE_NODES: usize = 4096;
const MAX_EVALUATIONS: usize = 65_536;
const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Locally compiled schemas. References are exact pins and never cause network access.
#[derive(Default)]
pub struct SchemaCatalog {
    entries: BTreeMap<String, (SchemaRef, Schema)>,
}

impl SchemaCatalog {
    /// Check that an exact installed reference was compiled into this catalog.
    pub fn contains(&self, reference: &SchemaRef) -> bool {
        self.entries
            .get(&reference.id)
            .is_some_and(|(pin, _)| pin == reference)
    }

    /// Compile exact UTF-8 documents supplied at startup. Digests include every byte.
    pub fn compile<'a>(
        entries: impl IntoIterator<Item = (SchemaRef, &'a str)>,
    ) -> Result<Self, String> {
        let mut catalog = Self::default();
        for (reference, document) in entries {
            if catalog.entries.len() >= 128 || document.len() > MAX_DOCUMENT_BYTES {
                return Err("schema catalog limit exceeded".into());
            }
            let digest = format!("{:x}", Sha256::digest(document.as_bytes()));
            validate_schema_ref(&reference)?;
            if reference.digest != digest {
                return Err("schema reference digest or version is invalid".into());
            }
            let value: Value = serde_json::from_str(document).map_err(|e| e.to_string())?;
            let object = value
                .as_object()
                .ok_or("schema document must be an object")?;
            if object.get("$id").and_then(Value::as_str) != Some(reference.id.as_str())
                || object.get("$schema").and_then(Value::as_str) != Some(DIALECT)
            {
                return Err("schema identity or dialect mismatch".into());
            }
            let schema = Schema::compile(object, 0, &mut 0, true)?;
            if catalog
                .entries
                .insert(reference.id.clone(), (reference, schema))
                .is_some()
            {
                return Err("duplicate schema id".into());
            }
        }
        Ok(catalog)
    }

    /// Validate against the exact registered reference; absence or mismatch fails closed.
    pub fn validate(&self, reference: &SchemaRef, value: &Value) -> Result<(), String> {
        let (pin, schema) = self.entries.get(&reference.id).ok_or("unknown schema")?;
        if pin != reference {
            return Err("schema pin mismatch".into());
        }
        validate_instance_complexity(value)?;
        if serde_json::to_vec(value).map_err(|e| e.to_string())?.len() > MAX_VALUE_BYTES {
            return Err("instance exceeds size limit".into());
        }
        if schema.matches(value, &mut 0)? {
            Ok(())
        } else {
            Err("instance violates schema".into())
        }
    }
}

fn validate_instance_complexity(value: &Value) -> Result<(), String> {
    let mut pending = vec![(value, 0)];
    let mut nodes = 0;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if depth > MAX_DEPTH || nodes > MAX_INSTANCE_NODES {
            return Err("instance complexity limit exceeded".into());
        }
        let child_count = match value {
            Value::Array(values) => values.len(),
            Value::Object(values) => values.len(),
            _ => 0,
        };
        if child_count > MAX_INSTANCE_NODES.saturating_sub(nodes + pending.len()) {
            return Err("instance complexity limit exceeded".into());
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
            Value::Object(values) => pending.extend(values.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

#[derive(Default)]
struct Schema {
    types: Option<Vec<String>>,
    properties: BTreeMap<String, Schema>,
    required: Vec<String>,
    additional: Option<Additional>,
    minimum: Option<i64>,
    maximum: Option<i64>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    enumeration: Option<Vec<Value>>,
    constant: Option<Value>,
    one_of: Vec<Schema>,
    all_of: Vec<Schema>,
    conditional: Option<(Box<Schema>, Box<Schema>)>,
}
enum Additional {
    Deny,
    Schema(Box<Schema>),
}

impl Schema {
    fn compile(
        object: &Map<String, Value>,
        depth: usize,
        nodes: &mut usize,
        root: bool,
    ) -> Result<Self, String> {
        *nodes += 1;
        if depth > MAX_DEPTH || *nodes > MAX_NODES {
            return Err("schema complexity limit exceeded".into());
        }
        let mut out = Self::default();
        for (key, value) in object {
            match key.as_str() {
                "$id" | "$schema" if root => {}
                "type" => {
                    let values = match value {
                        Value::String(s) => vec![s.clone()],
                        Value::Array(a) => a
                            .iter()
                            .map(|v| v.as_str().map(str::to_owned).ok_or("invalid type"))
                            .collect::<Result<Vec<_>, _>>()?,
                        _ => return Err("invalid type".into()),
                    };
                    let mut seen = BTreeSet::new();
                    if values.is_empty()
                        || values.iter().any(|s| {
                            !matches!(
                                s.as_str(),
                                "object" | "string" | "integer" | "boolean" | "null"
                            ) || !seen.insert(s)
                        })
                    {
                        return Err("unsupported or duplicate type".into());
                    }
                    out.types = Some(values);
                }
                "properties" => {
                    for (name, child) in value.as_object().ok_or("properties must be an object")? {
                        out.properties.insert(
                            name.clone(),
                            Self::compile(
                                child.as_object().ok_or("property must be a schema")?,
                                depth + 1,
                                nodes,
                                false,
                            )?,
                        );
                    }
                }
                "required" => {
                    let mut seen = BTreeSet::new();
                    for name in value.as_array().ok_or("required must be an array")? {
                        let name = name.as_str().ok_or("required name must be a string")?;
                        if !seen.insert(name) {
                            return Err("duplicate required name".into());
                        }
                        out.required.push(name.to_owned());
                    }
                }
                "additionalProperties" => {
                    out.additional = Some(match value {
                        Value::Bool(false) => Additional::Deny,
                        Value::Object(child) => Additional::Schema(Box::new(Self::compile(
                            child,
                            depth + 1,
                            nodes,
                            false,
                        )?)),
                        _ => return Err("unsupported additionalProperties".into()),
                    })
                }
                "minimum" => {
                    out.minimum = Some(value.as_i64().ok_or("minimum must be an integer")?)
                }
                "maximum" => {
                    out.maximum = Some(value.as_i64().ok_or("maximum must be an integer")?)
                }
                "minLength" => {
                    out.min_length = Some(
                        usize::try_from(value.as_u64().ok_or("invalid minLength")?)
                            .map_err(|_| "invalid minLength")?,
                    )
                }
                "maxLength" => {
                    out.max_length = Some(
                        usize::try_from(value.as_u64().ok_or("invalid maxLength")?)
                            .map_err(|_| "invalid maxLength")?,
                    )
                }
                "enum" => {
                    let values = value.as_array().ok_or("enum must be an array")?;
                    if values.is_empty()
                        || values
                            .iter()
                            .enumerate()
                            .any(|(i, v)| values[..i].contains(v))
                    {
                        return Err("empty or duplicate enum".into());
                    }
                    if values.iter().any(contains_number) {
                        return Err("numeric enum values are unsupported".into());
                    }
                    out.enumeration = Some(values.clone());
                }
                "const" => {
                    if contains_number(value) {
                        return Err("numeric const values are unsupported".into());
                    }
                    out.constant = Some(value.clone());
                }
                "oneOf" => out.one_of = Self::compile_array(value, depth, nodes)?,
                "allOf" => out.all_of = Self::compile_array(value, depth, nodes)?,
                "if" | "then" => {}
                _ => return Err(format!("unsupported schema keyword: {key}")),
            }
        }
        match (object.get("if"), object.get("then")) {
            (Some(condition), Some(consequence)) => {
                out.conditional = Some((
                    Box::new(Self::compile(
                        condition.as_object().ok_or("if must be a schema")?,
                        depth + 1,
                        nodes,
                        false,
                    )?),
                    Box::new(Self::compile(
                        consequence.as_object().ok_or("then must be a schema")?,
                        depth + 1,
                        nodes,
                        false,
                    )?),
                ))
            }
            (None, None) => {}
            _ => return Err("if and then must occur together".into()),
        }
        if out.minimum.zip(out.maximum).is_some_and(|(a, b)| a > b)
            || out
                .min_length
                .zip(out.max_length)
                .is_some_and(|(a, b)| a > b)
        {
            return Err("inverted bounds".into());
        }
        Ok(out)
    }

    fn compile_array(value: &Value, depth: usize, nodes: &mut usize) -> Result<Vec<Self>, String> {
        let array = value.as_array().ok_or("composition must be an array")?;
        if array.is_empty() {
            return Err("composition must not be empty".into());
        }
        array
            .iter()
            .map(|v| {
                Self::compile(
                    v.as_object().ok_or("composition entry must be a schema")?,
                    depth + 1,
                    nodes,
                    false,
                )
            })
            .collect()
    }

    fn matches(&self, value: &Value, evaluations: &mut usize) -> Result<bool, String> {
        *evaluations += 1;
        if *evaluations > MAX_EVALUATIONS {
            return Err("schema evaluation limit exceeded".into());
        }
        if self.types.as_ref().is_some_and(|types| {
            !types.iter().any(|t| match t.as_str() {
                "object" => value.is_object(),
                "string" => value.is_string(),
                "integer" => is_integer(value),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                _ => false,
            })
        }) || self
            .enumeration
            .as_ref()
            .is_some_and(|values| !values.contains(value))
            || self
                .constant
                .as_ref()
                .is_some_and(|constant| constant != value)
        {
            return Ok(false);
        }
        if value.is_number() {
            let integer = value
                .as_i64()
                .map(i128::from)
                .or_else(|| value.as_u64().map(i128::from));
            let number = value
                .as_f64()
                .expect("JSON numbers have an f64 representation");
            let integer = integer.or_else(|| (number.fract() == 0.0).then_some(number as i128));
            let outside = if let Some(number) = integer {
                self.minimum.is_some_and(|min| number < i128::from(min))
                    || self.maximum.is_some_and(|max| number > i128::from(max))
            } else {
                self.minimum.is_some_and(|min| number < min as f64)
                    || self.maximum.is_some_and(|max| number > max as f64)
            };
            if outside {
                return Ok(false);
            }
        }
        if let Some(text) = value.as_str() {
            let length = text.chars().count();
            if self.min_length.is_some_and(|min| length < min)
                || self.max_length.is_some_and(|max| length > max)
            {
                return Ok(false);
            }
        }
        if let Some(object) = value.as_object() {
            if self.required.iter().any(|name| !object.contains_key(name)) {
                return Ok(false);
            }
            for (name, child) in object {
                if let Some(schema) = self.properties.get(name) {
                    if !schema.matches(child, evaluations)? {
                        return Ok(false);
                    }
                } else if let Some(additional) = &self.additional {
                    match additional {
                        Additional::Deny => return Ok(false),
                        Additional::Schema(schema) if !schema.matches(child, evaluations)? => {
                            return Ok(false);
                        }
                        _ => {}
                    }
                }
            }
        }
        if !self.one_of.is_empty() {
            let mut count = 0;
            for schema in &self.one_of {
                if schema.matches(value, evaluations)? {
                    count += 1;
                }
            }
            if count != 1 {
                return Ok(false);
            }
        }
        for schema in &self.all_of {
            if !schema.matches(value, evaluations)? {
                return Ok(false);
            }
        }
        if let Some((condition, consequence)) = &self.conditional
            && condition.matches(value, evaluations)?
            && !consequence.matches(value, evaluations)?
        {
            return Ok(false);
        }
        Ok(true)
    }
}

fn is_integer(value: &Value) -> bool {
    value.as_i64().is_some()
        || value.as_u64().is_some()
        || value
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0)
}

// JSON Schema compares numbers mathematically, including inside arrays and objects.
// Until that equality is implemented, reject those constants instead of letting
// serde_json's representation-sensitive equality change conditional behavior.
fn contains_number(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(_) => return true,
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pin(document: &str) -> SchemaRef {
        SchemaRef {
            id: "test/1".into(),
            major: 1,
            minor: 0,
            digest: format!("{:x}", Sha256::digest(document.as_bytes())),
        }
    }
    fn compile(document: &str) -> SchemaCatalog {
        SchemaCatalog::compile([(pin(document), document)]).unwrap()
    }

    #[test]
    fn validates_objects_conditionals_and_exclusive_branches() {
        let document = r#"{"$id":"test/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"state":{"enum":["on","off"]},"id":{"oneOf":[{"type":"null"},{"type":"string","minLength":1,"maxLength":4}]},"counts":{"type":"object","additionalProperties":{"type":"integer","minimum":1}}},"required":["state","id","counts"],"additionalProperties":false,"allOf":[{"if":{"properties":{"state":{"const":"on"}},"required":["state"]},"then":{"properties":{"id":{"type":"string","minLength":1}}}},{"if":{"properties":{"state":{"const":"off"}},"required":["state"]},"then":{"properties":{"id":{"const":null}}}}]}"#;
        let catalog = compile(document);
        for value in [
            json!({"state":"on","id":"é","counts":{"a":1}}),
            json!({"state":"off","id":null,"counts":{}}),
        ] {
            assert!(catalog.validate(&pin(document), &value).is_ok());
        }
        for value in [
            json!({"state":"on","id":null,"counts":{}}),
            json!({"state":"off","id":"x","counts":{}}),
            json!({"state":"on","id":"12345","counts":{}}),
            json!({"state":"on","id":"x","counts":{"a":0}}),
            json!({"state":"on","id":"x","counts":{},"extra":true}),
        ] {
            assert!(catalog.validate(&pin(document), &value).is_err());
        }
        let both = r#"{"$id":"test/1","$schema":"https://json-schema.org/draft/2020-12/schema","oneOf":[{},{}]}"#;
        assert!(compile(both).validate(&pin(both), &json!(1)).is_err());
    }

    #[test]
    fn pins_exact_document_and_reference() {
        let document = "{\"$id\":\"test/1\",\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"integer\"}\n";
        let catalog = compile(document);
        assert!(catalog.validate(&pin(document), &json!(1.0)).is_ok());
        assert!(
            catalog
                .validate(&pin(document.trim_end()), &json!(1))
                .is_err()
        );
        let mut wrong = pin(document);
        wrong.minor = 1;
        assert!(catalog.validate(&wrong, &json!(1)).is_err());
        assert!(SchemaCatalog::compile([(pin(document.trim_end()), document)]).is_err());
        assert!(
            SchemaCatalog::compile([(pin(document), document), (pin(document), document)]).is_err()
        );
    }

    #[test]
    fn rejects_unsupported_keywords_and_malformed_schemas() {
        for extra in [
            json!({"$ref":"https://example.com/schema"}),
            json!({"properties":{"x":{"format":"uri"}}}),
            json!({"type":"array"}),
            json!({"additionalProperties":true}),
            json!({"if":{}}),
            json!({"enum":[]}),
            json!({"minimum":2,"maximum":1}),
        ] {
            let mut value = json!({"$id":"test/1","$schema":DIALECT});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let document = value.to_string();
            assert!(
                SchemaCatalog::compile([(pin(&document), document.as_str())]).is_err(),
                "{document}"
            );
        }
    }

    #[test]
    fn rejects_numeric_constants_before_a_conditional_can_be_bypassed() {
        for condition in [
            json!({"const": 1}),
            json!({"enum": [1.0]}),
            json!({"const": {"nested": [1]}}),
            json!({"enum": [{"nested": [1.0]}]}),
        ] {
            // A representation-sensitive comparison could let 1.0 skip the
            // consequence of an `if` whose const is 1 (or vice versa).
            let document = json!({"$id":"test/1", "$schema":DIALECT,
                "if":condition, "then":{"const":"required"}})
            .to_string();
            assert!(SchemaCatalog::compile([(pin(&document), document.as_str())]).is_err());
        }
    }

    #[test]
    fn enforces_bounds_and_exact_integer_comparison() {
        let document = r#"{"$id":"test/1","$schema":"https://json-schema.org/draft/2020-12/schema","type":"integer","maximum":9223372036854775807}"#;
        let catalog = compile(document);
        assert!(catalog.validate(&pin(document), &json!(i64::MAX)).is_ok());
        assert!(
            catalog
                .validate(&pin(document), &json!(9223372036854775808u64))
                .is_err()
        );
        assert!(catalog.validate(&pin(document), &json!(1.5)).is_err());
        let mut deep = json!(0);
        for _ in 0..=MAX_DEPTH {
            deep = json!([deep]);
        }
        assert!(catalog.validate(&pin(document), &deep).is_err());
        assert!(
            catalog
                .validate(&pin(document), &json!(vec![0; MAX_INSTANCE_NODES]))
                .is_err()
        );
        assert!(
            catalog
                .validate(&pin(document), &json!("x".repeat(MAX_VALUE_BYTES)))
                .is_err()
        );
        let oversized = format!("{document}{}", " ".repeat(MAX_DOCUMENT_BYTES));
        assert!(SchemaCatalog::compile([(pin(&oversized), oversized.as_str())]).is_err());
        let mut nested = json!({});
        for _ in 0..=MAX_DEPTH {
            nested = json!({"allOf":[nested]});
        }
        let mut root = nested.as_object().unwrap().clone();
        root.insert("$id".into(), json!("test/1"));
        root.insert("$schema".into(), json!(DIALECT));
        let document = Value::Object(root).to_string();
        assert!(SchemaCatalog::compile([(pin(&document), document.as_str())]).is_err());
    }
    #[test]
    fn compiles_owner_documents_and_validates_representative_payloads() {
        let documents = [
            r###"{"$id":"amaterasu.display-control/1","$schema":"https://json-schema.org/draft/2020-12/schema","oneOf":[{"additionalProperties":false,"properties":{"hdr":{"enum":["preserve","on","off"]},"height":{"maximum":4320,"minimum":200,"type":"integer"},"layout":{"enum":["preserve","exclusive","extended","extended_isolated","extended_primary_isolated"]},"operation":{"const":"configure"},"refresh_millihz":{"maximum":1000000,"minimum":1000,"type":"integer"},"scaling":{"oneOf":[{"additionalProperties":false,"properties":{"mode":{"const":"preserve"}},"required":["mode"],"type":"object"},{"additionalProperties":false,"properties":{"mode":{"const":"percent"},"percent":{"maximum":500,"minimum":100,"type":"integer"}},"required":["mode","percent"],"type":"object"}]},"width":{"maximum":7680,"minimum":320,"type":"integer"}},"required":["operation","width","height","refresh_millihz","hdr","scaling","layout"],"type":"object"},{"additionalProperties":false,"properties":{"operation":{"const":"restore"},"snapshot_id":{"maxLength":128,"minLength":1,"type":"string"}},"required":["operation","snapshot_id"],"type":"object"}]}
"###,
            r###"{"$id":"amaterasu.display-target/1","$schema":"https://json-schema.org/draft/2020-12/schema","additionalProperties":false,"allOf":[{"if":{"properties":{"control_state":{"enum":["configured","restored"]}},"required":["control_state"]},"then":{"properties":{"snapshot_id":{"maxLength":128,"minLength":1,"type":"string"}}}},{"if":{"properties":{"control_state":{"const":"unconfigured"}},"required":["control_state"]},"then":{"properties":{"snapshot_id":{"type":"null"}}}}],"properties":{"capture_selector":{"additionalProperties":false,"properties":{"provider":{"minLength":1,"type":"string"},"schema":{"minLength":1,"type":"string"},"selector":{"type":"object"}},"required":["provider","schema","selector"],"type":"object"},"control_state":{"enum":["unconfigured","configured","restored","recovery_required"]},"desired_activation":{"enum":["active","inactive"]},"kind":{"enum":["physical","virtual"]},"layout":{"enum":[null,"exclusive","extended","extended_isolated","extended_primary_isolated"]},"mode":{"additionalProperties":false,"properties":{"hdr":{"type":["boolean","null"]},"height":{"maximum":4320,"minimum":200,"type":"integer"},"refresh_millihz":{"maximum":1000000,"minimum":1000,"type":"integer"},"width":{"maximum":7680,"minimum":320,"type":"integer"}},"required":["width","height","refresh_millihz","hdr"],"type":"object"},"native_state":{"enum":["active","inactive","unknown","recovery_required"]},"physical_identity":{"oneOf":[{"type":"null"},{"maxLength":256,"minLength":1,"type":"string"}]},"primary":{"type":["boolean","null"]},"retention_state":{"enum":["active","retained","released","recovery_required"]},"scale_percent":{"oneOf":[{"type":"null"},{"maximum":500,"minimum":100,"type":"integer"}]},"snapshot_id":{"oneOf":[{"type":"null"},{"maxLength":128,"minLength":1,"type":"string"}]}},"required":["desired_activation","native_state","retention_state","mode","capture_selector","control_state","snapshot_id","scale_percent","layout","kind","physical_identity","primary"],"type":"object"}
"###,
        ];
        let references: Vec<SchemaRef> = documents
            .iter()
            .map(|document| {
                let value: Value = serde_json::from_str(document).unwrap();
                SchemaRef {
                    id: value["$id"].as_str().unwrap().into(),
                    major: 1,
                    minor: 0,
                    digest: format!("{:x}", Sha256::digest(document.as_bytes())),
                }
            })
            .collect();
        assert_eq!(
            references[0].digest,
            "c2c3ce866277cb29bbd9fbd947899917538d46b1020df3197fbd264159607d87"
        );
        assert_eq!(
            references[1].digest,
            "8521cdfd92da98b25136bd01720d397f0d7fee14748c08afde76a0379fa18941"
        );
        let catalog = SchemaCatalog::compile(references.iter().cloned().zip(documents)).unwrap();
        assert!(catalog.contains(&references[0]));
        let restore = json!({"operation":"restore","snapshot_id":"snapshot"});
        assert!(catalog.validate(&references[0], &restore).is_ok());
        assert!(
            catalog
                .validate(
                    &references[0],
                    &json!({"operation":"restore","snapshot_id":""})
                )
                .is_err()
        );
        let target = json!({"desired_activation":"active","native_state":"active","retention_state":"active","mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":null},"capture_selector":{"provider":"provider","schema":"schema","selector":{}},"control_state":"unconfigured","snapshot_id":null,"scale_percent":null,"layout":null,"kind":"physical","physical_identity":"display-1","primary":true});
        assert!(catalog.validate(&references[1], &target).is_ok());
        let mut invalid = target;
        invalid["control_state"] = json!("configured");
        assert!(catalog.validate(&references[1], &invalid).is_err());
    }
}
