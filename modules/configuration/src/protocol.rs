use amaterasu_configuration_schema::{Fields, Values, defaults, validate_values};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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
    ProductGet {},
    ProductUpdate {
        version: u32,
        values: Map<String, Value>,
        expected_revision: u64,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub revision: u64,
    pub fields: Fields,
    pub values: Values,
}

/// Opaque product-owned settings. The product module validates `values` and
/// interprets `version`; configuration owns persistence and the CAS revision.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductDocument {
    pub version: u32,
    pub revision: u64,
    pub values: Map<String, Value>,
}

pub enum Change {
    Read(Document),
    Write(Document),
}

#[derive(Debug)]
pub enum ProductChange {
    Read(Option<ProductDocument>),
    Write(ProductDocument),
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
        Operation::ProductGet {} | Operation::ProductUpdate { .. } => Err(error(
            "invalid_request",
            "product settings require the product operation handler",
        )),
    }
}

pub fn apply_product(
    operation: Operation,
    existing: Option<ProductDocument>,
) -> Result<ProductChange, Value> {
    match operation {
        Operation::ProductGet {} => Ok(ProductChange::Read(existing)),
        Operation::ProductUpdate {
            version,
            values,
            expected_revision,
        } => {
            if version == 0 {
                return Err(error(
                    "invalid_version",
                    "product settings version must be greater than zero",
                ));
            }
            let current_revision = existing.as_ref().map_or(0, |document| document.revision);
            if expected_revision != current_revision {
                return Err(json!({
                    "ok": false,
                    "error": {
                        "code": "revision_conflict",
                        "message": "product settings revision differs",
                        "current_revision": current_revision,
                    },
                }));
            }
            let revision = current_revision
                .checked_add(1)
                .ok_or_else(|| error("resource_exhausted", "revision exhausted"))?;
            Ok(ProductChange::Write(ProductDocument {
                version,
                revision,
                values,
            }))
        }
        _ => Err(error(
            "invalid_request",
            "generic configuration operation used for product settings",
        )),
    }
}

pub fn success(document: Document) -> Value {
    json!({"ok":true,"revision":document.revision,"fields":document.fields,"values":document.values})
}

pub fn product_success(document: Option<ProductDocument>) -> Value {
    match document {
        Some(document) => json!({
            "ok": true,
            "found": true,
            "version": document.version,
            "revision": document.revision,
            "values": document.values,
        }),
        None => json!({
            "ok": true,
            "found": false,
            "version": null,
            "revision": 0,
            "values": {},
        }),
    }
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

    #[test]
    fn product_settings_are_full_document_cas_updates() {
        let get: Operation = serde_json::from_value(json!({"action":"product_get"})).unwrap();
        assert_eq!(
            product_success(None),
            json!({
                "ok": true,
                "found": false,
                "version": null,
                "revision": 0,
                "values": {},
            })
        );
        assert!(matches!(
            apply_product(get, None),
            Ok(ProductChange::Read(None))
        ));

        let update: Operation = serde_json::from_value(json!({
            "action":"product_update",
            "expected_revision":0,
            "version":1,
            "values":{"enabled":true,"nested":{"mode":"safe"}}
        }))
        .unwrap();
        let ProductChange::Write(document) = apply_product(update, None).unwrap() else {
            panic!("product update must create a document");
        };
        assert_eq!(document.version, 1);
        assert_eq!(document.revision, 1);
        assert_eq!(document.values["nested"]["mode"], "safe");

        let conflict: Operation = serde_json::from_value(json!({
            "action":"product_update",
            "expected_revision":0,
            "version":1,
            "values":{"enabled":false}
        }))
        .unwrap();
        let failure = apply_product(conflict, Some(document)).unwrap_err();
        assert_eq!(failure["error"]["code"], "revision_conflict");
        assert_eq!(failure["error"]["current_revision"], 1);
    }

    #[test]
    fn product_settings_reject_invalid_versions_and_unknown_selectors() {
        for payload in [
            json!({"action":"product_update","expected_revision":0,"version":0,"values":{}}),
            json!({"action":"product_update","expected_revision":0,"version":1,"values":[],"owner":"other"}),
            json!({"action":"product_get","owner":"other"}),
            json!({"action":"product_update","expected_revision":0,"version":1,"values":{},"owner":"other"}),
        ] {
            if let Ok(operation) = serde_json::from_value::<Operation>(payload) {
                assert!(apply_product(operation, None).is_err());
            } else {
                // Strict serde rejects unknown owner selectors and non-object values.
            }
        }
    }
}

#[cfg(test)]
mod sdk_contract_tests {
    use super::*;
    use amaterasu_module_sdk::configuration::{self, ProductSettingsRequest};

    fn exercise(mut call: impl FnMut(ProductSettingsRequest) -> Value) {
        let empty = configuration::decode_response(call(ProductSettingsRequest::ProductGet {}))
            .unwrap()
            .unwrap();
        assert!(!empty.found);
        let update = || ProductSettingsRequest::ProductUpdate {
            version: 1,
            values: [("enabled".into(), json!(true))].into_iter().collect(),
            expected_revision: 0,
        };
        let saved = configuration::decode_response(call(update()))
            .unwrap()
            .unwrap();
        assert!(saved.found);
        assert_eq!(saved.revision, 1);
        assert_eq!(saved.values["enabled"], true);
        let conflict = configuration::decode_response(call(update()))
            .unwrap()
            .unwrap_err();
        assert_eq!(conflict.code, "revision_conflict");
        assert_eq!(conflict.current_revision, Some(1));
    }

    #[test]
    fn same_consumer_accepts_real_and_independent_product_providers() {
        let mut document = None;
        exercise(|request| {
            let operation = serde_json::from_value(request.payload()).unwrap();
            match apply_product(operation, document.clone()) {
                Ok(ProductChange::Read(found)) => product_success(found),
                Ok(ProductChange::Write(saved)) => {
                    document = Some(saved.clone());
                    product_success(Some(saved))
                }
                Err(error) => error,
            }
        });
        let mut revision = 0;
        let mut values = Map::new();
        exercise(|request| match request {
            ProductSettingsRequest::ProductGet {} => {
                json!({"ok":true,"found":revision != 0,"version":if revision == 0 {None} else {Some(1)},"revision":revision,"values":values})
            }
            ProductSettingsRequest::ProductUpdate {
                version,
                values: new_values,
                expected_revision,
            } => {
                if expected_revision != revision {
                    json!({"ok":false,"error":{"code":"revision_conflict","message":"revision differs","current_revision":revision}})
                } else {
                    revision += 1;
                    values = new_values;
                    json!({"ok":true,"found":true,"version":version,"revision":revision,"values":values})
                }
            }
        });
    }

    #[test]
    fn no_owner_selector_or_inconsistent_documents() {
        assert!(
            serde_json::from_value::<ProductSettingsRequest>(
                json!({"action":"product_get","owner":"other"})
            )
            .is_err()
        );
        assert!(
            configuration::decode_response(
                json!({"ok":true,"found":true,"version":null,"revision":1,"values":{}})
            )
            .is_err()
        );
        assert!(configuration::decode_response(json!({"ok":false})).is_err());
    }
}
