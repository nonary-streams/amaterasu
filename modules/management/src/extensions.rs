//! Signed UI extensions and the independent local policy for their API methods.
//! Publishers can declare capabilities, but cannot grant them to themselves.
use amaterasu_module_sdk::Command;
use amaterasu_package::{
    unix_timestamp,
    web::extension::{self, MethodDeclaration, VerifiedExtension},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub root: PathBuf,
    pub trusted_keys: BTreeMap<String, String>,
    pub methods: Vec<MethodDeclaration>,
}

pub struct Extensions {
    entries: Vec<Entry>,
}

impl Extensions {
    pub fn new(settings: Settings) -> Result<Self, String> {
        if settings.entries.len() > 32 {
            return Err("too many extensions".into());
        }
        let mut roots = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for entry in &settings.entries {
            if !entry.root.is_absolute()
                || entry.trusted_keys.len() != 1
                || !roots.insert(entry.root.clone())
                || !valid_id(&entry.id)
                || !ids.insert(entry.id.clone())
            {
                return Err("extension requires a unique absolute root and one pinned key".into());
            }
            for (fingerprint, key) in &entry.trusted_keys {
                let key = amaterasu_package::decode_hex::<32>(key)?;
                if *fingerprint != amaterasu_package::sha256(&key) {
                    return Err("extension publisher fingerprint mismatch".into());
                }
            }
            let mut aliases = BTreeSet::new();
            for method in &entry.methods {
                if !aliases.insert(&method.alias)
                    || role_rank(&method.min_role).is_none()
                    || method.scope.is_empty()
                {
                    return Err("invalid extension approval".into());
                }
            }
        }
        Ok(Self {
            entries: settings.entries,
        })
    }

    fn selected(&self, entry: &Entry) -> Option<VerifiedExtension> {
        let selected =
            extension::load_selected(&entry.root, &entry.trusted_keys, unix_timestamp()).ok()?;
        (entry.id == selected.manifest.id).then_some(selected)
    }

    pub fn list(&self, auth: &Value) -> Value {
        let extensions: Vec<Value> = self.entries.iter().filter_map(|entry| {
            let bundle = self.selected(entry)?;
            let methods: Vec<Value> = bundle.manifest.methods.iter().filter(|method| approved(entry, method) && permits(auth, method)).map(|method| json!({"alias":method.alias,"scope":method.scope,"min_role":method.min_role})).collect();
            Some(json!({"id":bundle.manifest.id,"name":bundle.manifest.name,"digest":bundle.digest,"sdk_version":bundle.manifest.sdk_version,"entrypoint":bundle.manifest.entrypoint,"placements":bundle.manifest.placements,"settings_provider":bundle.manifest.settings_provider,"settings_owner":bundle.manifest.settings_owner,"methods":methods}))
        }).collect();
        json!({"ok":true,"extensions":extensions})
    }

    /// Returns bytes only from the currently selected and freshly verified bundle.
    pub fn asset(&self, id: &str, digest: &str, path: &str) -> Option<(&'static str, Vec<u8>)> {
        self.entries.iter().find_map(|entry| {
            let mut bundle = self.selected(entry)?;
            if bundle.manifest.id != id || bundle.digest != digest {
                return None;
            }
            let content_type = amaterasu_package::web::content_type(path)?;
            Some((content_type, bundle.assets.remove(path)?))
        })
    }

    pub fn resolve_call(&self, input: &Value, auth: &Value) -> Result<ResolvedCall, &'static str> {
        let object = input.as_object().ok_or("invalid_request")?;
        if object.len() != 4 {
            return Err("invalid_request");
        }
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or("invalid_request")?;
        let digest = object
            .get("digest")
            .and_then(Value::as_str)
            .ok_or("invalid_request")?;
        let alias = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or("invalid_request")?;
        let payload = object.get("payload").ok_or("invalid_request")?.clone();
        for entry in &self.entries {
            let Some(bundle) = self.selected(entry) else {
                continue;
            };
            if bundle.manifest.id != id || bundle.digest != digest {
                continue;
            }
            let method = bundle
                .manifest
                .methods
                .iter()
                .find(|method| method.alias == alias)
                .ok_or("extension_method_denied")?;
            if !approved(entry, method) || !permits(auth, method) {
                return Err("extension_method_denied");
            }
            return Ok(ResolvedCall {
                method: method.clone(),
                payload,
            });
        }
        Err("extension_not_found")
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn approved(entry: &Entry, declaration: &MethodDeclaration) -> bool {
    entry
        .methods
        .iter()
        .any(|policy| same_method(policy, declaration))
}

fn same_method(a: &MethodDeclaration, b: &MethodDeclaration) -> bool {
    a.alias == b.alias
        && a.provider == b.provider
        && a.service == b.service
        && a.method == b.method
        && a.interface_major == b.interface_major
        && a.interface_minor == b.interface_minor
        && a.scope == b.scope
        && a.min_role == b.min_role
}

fn role_rank(role: &str) -> Option<u8> {
    match role {
        "viewer" => Some(0),
        "operator" => Some(1),
        "administrator" => Some(2),
        _ => None,
    }
}

fn permits(auth: &Value, method: &MethodDeclaration) -> bool {
    auth.get("ok").and_then(Value::as_bool) == Some(true)
        && auth.get("kind").and_then(Value::as_str) == Some("administrator_session")
        && auth
            .get("role")
            .and_then(Value::as_str)
            .and_then(role_rank)
            .zip(role_rank(&method.min_role))
            .is_some_and(|(actual, required)| actual >= required)
        && auth
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| {
                scopes.iter().all(Value::is_string)
                    && scopes
                        .iter()
                        .any(|scope| scope.as_str() == Some(&method.scope))
            })
}

pub struct ResolvedCall {
    pub method: MethodDeclaration,
    payload: Value,
}

impl ResolvedCall {
    pub fn query(&self) -> Command {
        Command::QueryApiApproval {
            service: self.method.service.clone(),
            method: self.method.method.clone(),
            interface_major: self.method.interface_major,
            min_minor: self.method.interface_minor,
        }
    }
    /// Compare the broker-owned approval, including provider and exact version.
    pub fn accepts_approval(&self, approval: &Value) -> bool {
        let method = &approval["method"];
        approval.get("provider").and_then(Value::as_str) == Some(&self.method.provider)
            && method.get("service").and_then(Value::as_str) == Some(&self.method.service)
            && method.get("method").and_then(Value::as_str) == Some(&self.method.method)
            && method.get("interface_major").and_then(Value::as_u64)
                == Some(u64::from(self.method.interface_major))
            && method.get("interface_minor").and_then(Value::as_u64)
                == Some(u64::from(self.method.interface_minor))
            && method.get("privilege").and_then(Value::as_str) == Some("user")
    }
    /// Requires the caller to verify the query result matches provider and exact version first.
    pub fn call(self, timeout_ms: u64, operation_id: String) -> Command {
        Command::CallApi {
            service: self.method.service,
            method: self.method.method,
            interface_major: self.method.interface_major,
            interface_minor: self.method.interface_minor,
            payload: self.payload,
            timeout_ms,
            operation_id,
            parent: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn method() -> MethodDeclaration {
        MethodDeclaration {
            alias: "read".into(),
            provider: "provider".into(),
            service: "example.service/1".into(),
            method: "read".into(),
            interface_major: 1,
            interface_minor: 0,
            scope: "catalog.read".into(),
            min_role: "operator".into(),
        }
    }
    #[test]
    fn approval_requires_every_signed_field_and_exact_scope_and_role() {
        let mut policy = method();
        let entry = Entry {
            id: "hello".into(),
            root: "/tmp/hello".into(),
            trusted_keys: BTreeMap::new(),
            methods: vec![method()],
        };
        assert!(approved(&entry, &policy));
        policy.provider = "other".into();
        assert!(!approved(&entry, &policy));
        let auth = json!({"ok":true,"kind":"administrator_session","role":"operator","scopes":["catalog.read"]});
        assert!(permits(&auth, &method()));
        assert!(!permits(
            &json!({"ok":true,"kind":"administrator_session","role":"viewer","scopes":["catalog.read"]}),
            &method()
        ));
        assert!(!permits(
            &json!({"ok":true,"kind":"administrator_session","role":"administrator","scopes":["other"]}),
            &method()
        ));
        assert!(!permits(
            &json!({"ok":true,"kind":"api_token","scopes":["catalog.read"]}),
            &method()
        ));
    }
    #[test]
    fn signed_selection_limits_assets_and_calls() {
        use amaterasu_package::{ReleaseSignature, encode_hex, sha256};
        use ed25519_dalek::{Signer, SigningKey};
        use std::fs;
        let temp = std::env::temp_dir().join(format!(
            "extension-management-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&temp).unwrap();
        let source = temp.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("main.js"), b"verified").unwrap();
        let output = temp.join("bundle");
        let mut manifest = extension::prepare_extension(
            &source,
            &output,
            "hello",
            "Hello",
            "main.js",
            1,
            unix_timestamp() + 3600,
        )
        .unwrap();
        manifest.methods.push(method());
        manifest.placements = vec!["application".into()];
        manifest.settings_provider = Some("example.provider".into());
        let signer = SigningKey::from_bytes(&[42; 32]);
        let fingerprint = sha256(signer.verifying_key().as_bytes());
        let keys = BTreeMap::from([(
            fingerprint.clone(),
            encode_hex(signer.verifying_key().as_bytes()),
        )]);
        let raw = serde_json::to_vec(&manifest).unwrap();
        fs::write(output.join("manifest.json"), &raw).unwrap();
        let signature = ReleaseSignature {
            key_id: fingerprint.clone(),
            signature: encode_hex(&signer.sign(&raw).to_bytes()),
        };
        fs::write(
            output.join("manifest.sig"),
            serde_json::to_vec(&signature).unwrap(),
        )
        .unwrap();
        let root = temp.join("hello");
        let digest =
            extension::stage_extension(&output, &root, &keys, &fingerprint, unix_timestamp())
                .unwrap();
        let mismatched = Extensions::new(Settings {
            entries: vec![Entry {
                id: "other".into(),
                root: root.clone(),
                trusted_keys: keys.clone(),
                methods: vec![method()],
            }],
        })
        .unwrap();
        let auth = json!({"ok":true,"kind":"administrator_session","role":"operator","scopes":["catalog.read"]});
        assert_eq!(
            mismatched.list(&auth)["extensions"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert!(mismatched.asset("hello", &digest, "main.js").is_none());
        assert!(
            mismatched
                .resolve_call(
                    &json!({"id":"hello","digest":digest,"method":"read","payload":{}}),
                    &auth
                )
                .is_err()
        );
        let duplicate = Extensions::new(Settings {
            entries: vec![
                Entry {
                    id: "hello".into(),
                    root: root.clone(),
                    trusted_keys: keys.clone(),
                    methods: vec![],
                },
                Entry {
                    id: "hello".into(),
                    root: temp.join("different"),
                    trusted_keys: keys.clone(),
                    methods: vec![],
                },
            ],
        });
        assert!(duplicate.is_err());
        let extensions = Extensions::new(Settings {
            entries: vec![Entry {
                id: "hello".into(),
                root: root.clone(),
                trusted_keys: keys,
                methods: vec![method()],
            }],
        })
        .unwrap();
        let auth = json!({"ok":true,"kind":"administrator_session","role":"operator","scopes":["catalog.read"]});
        assert_eq!(extensions.list(&auth)["extensions"][0]["id"], "hello");
        assert_eq!(
            extensions.list(&auth)["extensions"][0]["placements"],
            json!(["application"])
        );
        assert_eq!(
            extensions.list(&auth)["extensions"][0]["settings_provider"],
            "example.provider"
        );
        assert_eq!(
            extensions.asset("hello", &digest, "main.js").unwrap().1,
            b"verified"
        );
        assert!(extensions.asset("hello", "wrong", "main.js").is_none());
        assert!(extensions.asset("hello", &digest, "../main.js").is_none());
        let call = json!({"id":"hello","digest":digest,"method":"read","payload":{}});
        let resolved = extensions.resolve_call(&call, &auth).unwrap();
        assert!(resolved.accepts_approval(&json!({"provider":"provider","method":{"service":"example.service/1","method":"read","interface_major":1,"interface_minor":0,"privilege":"user"}})));
        assert!(!resolved.accepts_approval(&json!({"provider":"other","method":{"service":"example.service/1","method":"read","interface_major":1,"interface_minor":0,"privilege":"user"}})));
        assert!(extensions.resolve_call(&call, &json!({"ok":true,"kind":"administrator_session","role":"viewer","scopes":["catalog.read"]})).is_err());
        fs::write(
            root.join("bundles").join(&digest).join("main.js"),
            b"tampered",
        )
        .unwrap();
        assert!(extensions.asset("hello", &digest, "main.js").is_none());
        assert!(extensions.resolve_call(&call, &auth).is_err());
        fs::remove_dir_all(temp).unwrap();
    }
}
