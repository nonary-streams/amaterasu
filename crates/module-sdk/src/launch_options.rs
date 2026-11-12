//! Bounded, per-stream option snapshots. Selection and authorization belong to
//! trusted composition; this module does not invoke or select a provider.
use crate::extension_api::{SchemaRef, schemas::SchemaCatalog};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::{self, Write},
};

/// Limits apply to JSON-encoded entries, not just their payload values.
pub const MAX_OPTIONS: usize = 16;
pub const MAX_OPTION_BYTES: usize = 4096;
pub const MAX_OPTIONS_BYTES: usize = 8192;
/// Matches the existing SDK schema instance complexity limits.
pub const MAX_VALUE_DEPTH: usize = 32;
pub const MAX_VALUE_NODES: usize = 4096;

/// A selected stream service's declared support, matched against trusted startup policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportedOption {
    pub namespace: String,
    pub version: u32,
    pub schema: SchemaRef,
}

/// Response to the optional `launch_option_capabilities` stream-service action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionCapabilities {
    pub version: u32,
    pub options: Vec<SupportedOption>,
}

impl OptionCapabilities {
    /// Bound the response before decoding and reject ambiguous or malformed pins.
    pub fn parse(value: Value) -> Result<Self, &'static str> {
        if !fits_json(&value, MAX_OPTIONS_BYTES) {
            return Err("option capabilities too large");
        }
        let capabilities: Self =
            serde_json::from_value(value).map_err(|_| "invalid option capabilities")?;
        let mut names = BTreeSet::new();
        if capabilities.version != 1
            || capabilities.options.len() > MAX_OPTIONS
            || capabilities.options.iter().any(|option| {
                !valid_namespace(&option.namespace)
                    || option.version == 0
                    || !names.insert(&option.namespace)
                    || option.schema.id.is_empty()
                    || option.schema.id.len() > 128
                    || option.schema.major == 0
                    || option.schema.digest.len() != 64
                    || !option
                        .schema
                        .digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("invalid option capabilities");
        }
        Ok(capabilities)
    }

    pub fn accepts(&self, namespace: &str, version: u32, schema: &SchemaRef) -> bool {
        self.options.iter().any(|option| {
            option.namespace == namespace && option.version == version && option.schema == *schema
        })
    }
}

/// A participant's candidate. Its schema and semantics belong to the selected
/// consumer; the namespace is an identifier, not a provider or an authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchOption {
    pub namespace: String,
    pub version: u32,
    pub value: Value,
}

/// The trusted call site must retain this binding alongside the corresponding
/// broker request; a participant response cannot provide or alter it.
pub struct ParticipantBinding<'a> {
    pub namespace: &'a str,
    pub version: u32,
    pub schema: &'a SchemaRef,
}

/// A response paired by the caller with its trusted invocation binding.
pub struct BoundOption<'a> {
    pub binding: ParticipantBinding<'a>,
    pub option: LaunchOption,
}

/// A validated immutable snapshot. A new stream generation needs a new snapshot.
/// Do not deserialize this type directly: every entry must pass trusted approval.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchOptions(Vec<LaunchOption>);

impl LaunchOptions {
    /// Check structural bounds and ask the trusted caller to approve each exact
    /// namespace/version for its selected recipient. Unknown entries fail closed.
    /// This does not authenticate the caller or validate a participant's schema.
    pub fn validate(
        entries: Vec<LaunchOption>,
        mut approved: impl FnMut(&str, u32) -> bool,
    ) -> Result<Self, &'static str> {
        if entries.len() > MAX_OPTIONS {
            return Err("too many launch options");
        }
        let mut namespaces = BTreeSet::new();
        for entry in &entries {
            if !valid_namespace(&entry.namespace) || entry.version == 0 {
                return Err("invalid launch option identity");
            }
            if !namespaces.insert(&entry.namespace) {
                return Err("duplicate launch option namespace");
            }
            if !valid_complexity(&entry.value, 0, &mut 0) {
                return Err("launch option too complex");
            }
            if !fits_json(entry, MAX_OPTION_BYTES) {
                return Err("launch option too large");
            }
            if !approved(&entry.namespace, entry.version) {
                return Err("unapproved launch option");
            }
        }
        if !fits_json(&entries, MAX_OPTIONS_BYTES) {
            return Err("launch options too large");
        }
        Ok(Self(entries))
    }

    /// Validate participant attribution, the selected consumer's exact support,
    /// and each payload against the already installed, digest-pinned schema.
    /// The caller supplies bindings from trusted composition, not reply data, and
    /// binds `consumer_accepts` to the consumer selected before processing input.
    /// This function performs no broker calls or entitlement checks.
    pub fn validate_bound(
        replies: Vec<BoundOption<'_>>,
        schemas: &SchemaCatalog,
        mut consumer_accepts: impl FnMut(&str, u32, &SchemaRef) -> bool,
    ) -> Result<Self, &'static str> {
        if replies.len() > MAX_OPTIONS {
            return Err("too many launch options");
        }
        let (bindings, entries): (Vec<_>, Vec<_>) = replies
            .into_iter()
            .map(|reply| (reply.binding, reply.option))
            .unzip();
        // Bound size and complexity before asking the schema catalog to inspect
        // a value; the catalog itself serializes instances during validation.
        let snapshot = Self::validate(entries, |_, _| true)?;
        for (binding, entry) in bindings.iter().zip(snapshot.entries()) {
            if entry.namespace != binding.namespace || entry.version != binding.version {
                return Err("participant option binding mismatch");
            }
            if !consumer_accepts(binding.namespace, binding.version, binding.schema) {
                return Err("consumer does not accept launch option");
            }
            schemas
                .validate(binding.schema, &entry.value)
                .map_err(|_| "invalid launch option schema")?;
        }
        Ok(snapshot)
    }

    pub fn entries(&self) -> &[LaunchOption] {
        &self.0
    }
}

// Count without buffering the serialized value; the writer fails as soon as a
// write would cross the limit. No additional allocation scales with input size.
struct LimitedCounter {
    count: usize,
    limit: usize,
}

impl Write for LimitedCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.count {
            return Err(io::Error::other("serialized value exceeds limit"));
        }
        self.count += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fits_json(value: &impl Serialize, limit: usize) -> bool {
    serde_json::to_writer(&mut LimitedCounter { count: 0, limit }, value).is_ok()
}

fn valid_complexity(value: &Value, depth: usize, nodes: &mut usize) -> bool {
    *nodes += 1;
    if depth > MAX_VALUE_DEPTH || *nodes > MAX_VALUE_NODES {
        return false;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .all(|child| valid_complexity(child, depth + 1, nodes)),
        Value::Object(values) => values
            .values()
            .all(|child| valid_complexity(child, depth + 1, nodes)),
        _ => true,
    }
}

fn valid_namespace(namespace: &str) -> bool {
    namespace.len() <= 128
        && namespace.split('.').count() >= 2
        && namespace.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn option(namespace: &str, version: u32, value: Value) -> LaunchOption {
        LaunchOption {
            namespace: namespace.into(),
            version,
            value,
        }
    }

    #[test]
    fn validates_two_independent_namespaces_and_empty_snapshot() {
        let candidates = vec![
            option("org.example.alpha", 1, json!({"x": 2})),
            option("net.example.beta", 2, json!(true)),
        ];
        let snapshot = LaunchOptions::validate(candidates.clone(), |name, version| {
            matches!(
                (name, version),
                ("org.example.alpha", 1) | ("net.example.beta", 2)
            )
        })
        .unwrap();
        assert_eq!(snapshot.entries(), candidates);
        assert!(
            LaunchOptions::validate(vec![], |_, _| false)
                .unwrap()
                .entries()
                .is_empty()
        );
    }

    #[test]
    fn rejects_unknown_versions_conflicts_and_invalid_names() {
        let allowed = |name: &str, version| name == "org.example.alpha" && version == 1;
        assert!(
            LaunchOptions::validate(vec![option("org.example.alpha", 2, json!(1))], allowed)
                .is_err()
        );
        assert!(
            LaunchOptions::validate(
                vec![
                    option("org.example.alpha", 1, json!(1)),
                    option("org.example.alpha", 2, json!(2))
                ],
                |_, _| true
            )
            .is_err()
        );
        assert!(LaunchOptions::validate(vec![option("bad", 1, json!(1))], |_, _| true).is_err());
        assert!(
            LaunchOptions::validate(vec![option("org.example.alpha", 0, json!(1))], |_, _| true)
                .is_err()
        );
    }

    #[test]
    fn participant_and_selected_consumer_must_agree_on_schema_and_option() {
        const DOCUMENT: &str = r#"{"$id":"org.example.option","$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"],"additionalProperties":false}"#;
        let schema = SchemaRef {
            id: "org.example.option".into(),
            major: 1,
            minor: 0,
            digest: "e020a99a8ed4ec086459efba9b8f0c73420143d452a894200c54b112560cc84d".into(),
        };
        let schemas = SchemaCatalog::compile([(schema.clone(), DOCUMENT)]).unwrap();
        let reply = |bound: &'static str, output: &'static str, value| BoundOption {
            binding: ParticipantBinding {
                namespace: bound,
                version: 1,
                schema: &schema,
            },
            option: option(output, 1, value),
        };
        let selected = |name: &str, version, pin: &SchemaRef| {
            matches!(name, "org.example.alpha" | "net.example.beta")
                && version == 1
                && pin == &schema
        };
        let accepted = LaunchOptions::validate_bound(
            vec![
                reply(
                    "org.example.alpha",
                    "org.example.alpha",
                    json!({"enabled": true}),
                ),
                reply(
                    "net.example.beta",
                    "net.example.beta",
                    json!({"enabled": false}),
                ),
            ],
            &schemas,
            selected,
        )
        .unwrap();
        assert_eq!(accepted.entries().len(), 2);
        assert!(
            LaunchOptions::validate_bound(
                vec![reply(
                    "org.example.alpha",
                    "net.example.beta",
                    json!({"enabled": true})
                )],
                &schemas,
                selected
            )
            .is_err()
        );
        assert!(
            LaunchOptions::validate_bound(
                vec![reply(
                    "org.example.alpha",
                    "org.example.alpha",
                    json!({"enabled": true})
                )],
                &schemas,
                |_, _, _| false
            )
            .is_err()
        );
        assert!(
            LaunchOptions::validate_bound(
                vec![reply(
                    "org.example.alpha",
                    "org.example.alpha",
                    json!({"unknown": true})
                )],
                &schemas,
                selected
            )
            .is_err()
        );
        assert!(
            LaunchOptions::validate_bound(
                vec![reply(
                    "org.example.alpha",
                    "org.example.alpha",
                    json!({"enabled": true, "unknown": true})
                )],
                &schemas,
                selected
            )
            .is_err()
        );
        assert!(
            LaunchOptions::validate_bound(
                vec![
                    reply(
                        "org.example.alpha",
                        "org.example.alpha",
                        json!({"enabled": true})
                    ),
                    reply(
                        "org.example.alpha",
                        "org.example.alpha",
                        json!({"enabled": false})
                    )
                ],
                &schemas,
                selected
            )
            .is_err()
        );
    }

    #[test]
    fn bounds_nested_values_and_stops_serializing_at_the_byte_limit() {
        let mut nested = json!(null);
        for _ in 0..=MAX_VALUE_DEPTH {
            nested = Value::Array(vec![nested]);
        }
        assert!(
            LaunchOptions::validate(vec![option("org.example.deep", 1, nested)], |_, _| true)
                .is_err()
        );
        let many = Value::Array(vec![Value::Null; MAX_VALUE_NODES]);
        assert!(
            LaunchOptions::validate(vec![option("org.example.many", 1, many)], |_, _| true)
                .is_err()
        );
        let mut writer = LimitedCounter { count: 0, limit: 4 };
        assert!(writer.write_all(b"1234").is_ok());
        assert!(writer.write_all(b"5").is_err());
        assert_eq!(writer.count, 4);
        assert!(
            LaunchOptions::validate(
                vec![option(
                    "org.example.large",
                    1,
                    json!("x".repeat(1024 * 1024))
                )],
                |_, _| true
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_individual_aggregate_and_count_overflow() {
        assert!(
            LaunchOptions::validate(
                vec![option(
                    "org.example.alpha",
                    1,
                    json!("x".repeat(MAX_OPTION_BYTES))
                )],
                |_, _| true
            )
            .is_err()
        );
        let entries: Vec<_> = (0..3)
            .map(|i| option(&format!("org.example.n{i}"), 1, json!("x".repeat(3000))))
            .collect();
        assert!(LaunchOptions::validate(entries, |_, _| true).is_err());
        let entries: Vec<_> = (0..=MAX_OPTIONS)
            .map(|i| option(&format!("org.example.n{i}"), 1, json!(null)))
            .collect();
        assert!(LaunchOptions::validate(entries, |_, _| true).is_err());
    }
}
