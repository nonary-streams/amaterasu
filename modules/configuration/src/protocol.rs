use crate::schema::{Fields, Values, defaults, validate_values};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Migrate(crate::migration::Migration),
    Register {
        fields: Fields,
    },
    Get {},
    Update {
        values: Values,
        expected_revision: Option<u64>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub revision: u64,
    pub fields: Fields,
    pub values: Values,
}

pub enum Change {
    Read(Document),
    Write(Document),
}

pub fn apply(operation: Operation, existing: Option<Document>) -> Result<Change, Value> {
    match operation {
        Operation::Migrate(migration) => {
            crate::migration::migrate(migration, existing).map(Change::Write)
        }
        Operation::Register { fields } => {
            let values = defaults(&fields).map_err(|message| error("invalid_schema", message))?;
            if let Some(document) = existing {
                if document.fields != fields {
                    return Err(error(
                        "schema_conflict",
                        "schema is already registered differently",
                    ));
                }
                return Ok(Change::Read(document));
            }
            Ok(Change::Write(Document {
                revision: 1,
                fields,
                values,
            }))
        }
        Operation::Get {} => existing
            .map(Change::Read)
            .ok_or_else(|| error("not_registered", "caller has no configuration schema")),
        Operation::Update {
            values,
            expected_revision,
        } => {
            let mut document = existing
                .ok_or_else(|| error("not_registered", "caller has no configuration schema"))?;
            if expected_revision.is_some_and(|revision| revision != document.revision) {
                return Err(
                    json!({"ok":false,"error":{"code":"revision_conflict","message":"configuration revision differs","current_revision":document.revision}}),
                );
            }
            validate_values(&document.fields, &values)
                .map_err(|message| error("invalid_values", message))?;
            document.values.extend(values);
            document.revision = document
                .revision
                .checked_add(1)
                .ok_or_else(|| error("resource_exhausted", "revision exhausted"))?;
            Ok(Change::Write(document))
        }
    }
}

pub fn success(document: Document) -> Value {
    json!({"ok":true,"revision":document.revision,"fields":document.fields,"values":document.values})
}

pub fn error(code: &str, message: impl Into<String>) -> Value {
    let message: String = message.into().chars().take(512).collect();
    json!({"ok":false,"error":{"code":code,"message":message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_preserve_other_fields_and_advance_document_revision() {
        let operation = serde_json::from_value(json!({"action":"register","fields":{
            "active":{"type":"boolean","default":true},
            "title":{"type":"string","default":"original"}
        }}))
        .unwrap();
        let Change::Write(document) = apply(operation, None).unwrap() else {
            panic!()
        };
        let update = serde_json::from_value(
            json!({"action":"update","values":{"title":"changed"},"expected_revision":1}),
        )
        .unwrap();
        let Change::Write(updated) = apply(update, Some(document)).unwrap() else {
            panic!()
        };
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.values["active"], true);
        assert_eq!(updated.values["title"], "changed");
        let invalid =
            serde_json::from_value(json!({"action":"update","values":{"unknown":true}})).unwrap();
        assert_eq!(
            apply(invalid, Some(updated)).err().unwrap()["error"]["code"],
            "invalid_values"
        );
    }

    #[test]
    fn registration_is_idempotent_and_redefinition_is_rejected() {
        let operation = || {
            serde_json::from_value(
                json!({"action":"register","fields":{"active":{"type":"boolean","default":true}}}),
            )
            .unwrap()
        };
        let Change::Write(document) = apply(operation(), None).unwrap() else {
            panic!()
        };
        assert!(matches!(
            apply(operation(), Some(document.clone())),
            Ok(Change::Read(_))
        ));
        assert!(
            apply(
                Operation::Register {
                    fields: Fields::new()
                },
                Some(document.clone())
            )
            .is_err()
        );
        let failure = apply(
            Operation::Update {
                values: Values::new(),
                expected_revision: Some(0),
            },
            Some(document),
        )
        .err()
        .unwrap();
        assert_eq!(failure["error"]["code"], "revision_conflict");
        assert!(
            serde_json::from_value::<Operation>(json!({"action":"get","caller":"other"})).is_err()
        );
    }
}
