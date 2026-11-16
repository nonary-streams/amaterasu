use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use ed25519_dalek::{Signer, SigningKey};

fn component(id: &str, runtime: bool) -> ComponentManifest {
    ComponentManifest {
        id: id.into(),
        version: "1".into(),
        file: format!("{id}.bin"),
        sha256: sha256(b"payload"),
        size: 7,
        abi_major: amaterasu_abi::ABI_MAJOR,
        runtime,
        requires: BTreeMap::new(),
        requested: Grants::default(),
    }
}
fn manifest() -> PluginManifest {
    PluginManifest {
        schema: 1,
        host_context_schema: 1,
        project: "amaterasu".into(),
        id: "example.plugin".into(),
        version: "1".into(),
        sequence: 2,
        expires_unix: crate::unix_timestamp() + 3600,
        target: crate::current_target(),
        components: vec![component("example.plugin", false)],
        drivers: vec![],
        api_methods: vec![],
        settings: BTreeMap::new(),
    }
}
fn bootstrap() -> BootstrapConfig {
    let mut policy = RuntimeConfig::default();
    policy
        .principals
        .insert("example.plugin".into(), Grants::default());
    BootstrapConfig {
        development: false,
        allow_unsigned: false,
        trusted_keys: BTreeMap::new(),
        policy,
    }
}

#[test]
fn signed_plugin_is_verified_before_loading_and_changed_bytes_are_rejected() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-plugin-test-{}",
        crate::random_identifier().unwrap()
    ));
    std::fs::create_dir(&directory).unwrap();
    let signing_key = SigningKey::from_bytes(&[42; 32]);
    let mut bootstrap = bootstrap();
    bootstrap.trusted_keys.insert(
        "publisher".into(),
        crate::encode_hex(signing_key.verifying_key().as_bytes()),
    );
    let manifest = manifest();
    let raw = serde_json::to_vec(&manifest).unwrap();
    let signature = ReleaseSignature {
        key_id: "publisher".into(),
        signature: crate::encode_hex(&signing_key.sign(&raw).to_bytes()),
    };
    std::fs::write(directory.join("manifest.json"), &raw).unwrap();
    crate::write_json_atomically(&directory.join("manifest.sig"), &signature).unwrap();
    std::fs::write(directory.join("example.plugin.bin"), b"payload").unwrap();
    verify_package(&directory, &bootstrap, crate::unix_timestamp()).unwrap();
    std::fs::write(directory.join("example.plugin.bin"), b"changed").unwrap();
    assert!(verify_package(&directory, &bootstrap, crate::unix_timestamp()).is_err());
    std::fs::write(directory.join("example.plugin.bin"), b"payload").unwrap();
    bootstrap.trusted_keys.clear();
    assert!(verify_package(&directory, &bootstrap, crate::unix_timestamp()).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn plugin_cannot_replace_core_or_bypass_exact_dependencies() {
    let base = vec![component("amaterasu.runtime", true)];
    let bootstrap = bootstrap();
    let mut plugin = manifest();
    plugin.components[0]
        .requires
        .insert("amaterasu.runtime".into(), "1".into());
    let composed = compose(&base, [&plugin], &bootstrap).unwrap();
    assert_eq!(composed.len(), 2);
    plugin.components[0]
        .requires
        .insert("amaterasu.runtime".into(), "2".into());
    assert!(compose(&base, [&plugin], &bootstrap).is_err());
    plugin.components[0] = component("amaterasu.runtime", false);
    assert!(compose(&base, [&plugin], &bootstrap).is_err());
}

#[test]
fn requested_authority_is_never_silently_granted() {
    let mut plugin = manifest();
    plugin.components[0]
        .requested
        .call
        .insert("example.privileged/1".into());
    assert!(
        compose(
            &[component("amaterasu.runtime", true)],
            [&plugin],
            &bootstrap()
        )
        .is_err()
    );
}

#[test]
fn plugin_dependency_cycle_is_rejected() {
    let mut plugin = manifest();
    plugin.components[0]
        .requires
        .insert("example.other".into(), "1".into());
    let mut other = component("example.other", false);
    other.requires.insert("example.plugin".into(), "1".into());
    plugin.components.push(other);
    let mut bootstrap = bootstrap();
    bootstrap
        .policy
        .principals
        .insert("example.other".into(), Grants::default());
    assert!(
        compose(
            &[component("amaterasu.runtime", true)],
            [&plugin],
            &bootstrap
        )
        .is_err()
    );
}

#[test]
fn unsupported_schema_and_target_fail_before_artifact_access() {
    let mut plugin = manifest();
    plugin.schema = 2;
    assert!(plugin.validate(crate::unix_timestamp()).is_err());
    plugin.schema = 1;
    plugin.target = "unknown-cpu".into();
    assert!(plugin.validate(crate::unix_timestamp()).is_err());
}

#[test]
fn path_traversal_and_payload_conflicts_are_rejected() {
    let mut plugin = manifest();
    plugin.components[0].file = "../payload".into();
    assert!(plugin.artifacts().is_err());
    plugin.components[0].file = "manifest.json".into();
    assert!(plugin.artifacts().is_err());
    plugin.components[0].file = "same.bin".into();
    let mut other = component("example.other", false);
    other.file = "same.bin".into();
    other.sha256 = "a".repeat(64);
    plugin.components.push(other);
    assert!(plugin.artifacts().is_err());
}

#[cfg(unix)]
#[test]
fn symlink_artifacts_are_rejected_even_when_their_content_matches() {
    let directory = std::env::temp_dir().join(format!(
        "amaterasu-plugin-symlink-{}",
        crate::random_identifier().unwrap()
    ));
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("source"), b"payload").unwrap();
    std::os::unix::fs::symlink(
        directory.join("source"),
        directory.join("example.plugin.bin"),
    )
    .unwrap();
    let artifact = manifest().artifacts().unwrap().remove(0);
    assert!(verify_artifact(&directory, &artifact).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn declared_user_api_is_validated_before_activation_and_bound_to_management() {
    use amaterasu_contracts::{
        ApiFieldKind, ApiMethod, ApiMethodApproval, ApiPrivilege, ApiSchema,
    };
    let mut plugin = manifest();
    let service = "org.example.query/1";
    plugin.components[0]
        .requested
        .provide
        .insert(service.into());
    plugin.api_methods.push(ApiMethodApproval {
        provider: plugin.components[0].id.clone(),
        callers: ["amaterasu.management".into()].into(),
        method: ApiMethod {
            service: service.into(),
            method: "query".into(),
            interface_major: 1,
            interface_minor: 0,
            request: ApiSchema {
                required: BTreeMap::new(),
                optional: BTreeMap::new(),
                allow_extra: false,
            },
            response: ApiSchema {
                required: [("ok".into(), ApiFieldKind::Boolean)].into(),
                optional: BTreeMap::new(),
                allow_extra: false,
            },
            privilege: ApiPrivilege::User,
        },
    });
    let mut bootstrap = bootstrap();
    bootstrap.policy.principals.remove("example.plugin");
    bootstrap
        .policy
        .principals
        .insert("amaterasu.management".into(), Grants::default());
    let verified = composition::VerifiedPlugin {
        source: composition::PluginSource {
            repository: "example".into(),
            publisher: "a".repeat(64),
            catalog_digest: "b".repeat(64),
            manifest_digest: "c".repeat(64),
            plugin: plugin.id.clone(),
            version: plugin.version.clone(),
            sequence: plugin.sequence,
        },
        manifest: plugin.clone(),
        directory: std::env::temp_dir(),
    };
    let mut approved = installed::policy_for(&bootstrap, &[verified]).unwrap();
    let components = compose(
        &[
            component("amaterasu.runtime", true),
            component("amaterasu.management", false),
        ],
        [&plugin],
        &approved,
    )
    .unwrap();
    assert!(
        runtime_configuration(&approved, &components).principals["amaterasu.management"]
            .call
            .contains(service)
    );
    approved.policy.api_methods[0].method.interface_major = 2;
    assert!(
        compose(
            &[
                component("amaterasu.runtime", true),
                component("amaterasu.management", false)
            ],
            [&plugin],
            &approved
        )
        .is_err()
    );
}

#[test]
fn native_binding_requires_a_signed_exact_provider_lifetime_dependency() {
    let mut plugin = manifest();
    let service = "org.example.frames/1";
    plugin.components[0]
        .requested
        .native_use
        .insert(service.into());
    let mut provider = component("org.example.frames", false);
    provider.requested.native_provide.insert(service.into());
    let base = [component("amaterasu.runtime", true), provider];
    let mut bootstrap = bootstrap();
    bootstrap
        .policy
        .principals
        .insert(base[1].id.clone(), base[1].requested.clone());
    bootstrap.policy.principals.insert(
        plugin.components[0].id.clone(),
        plugin.components[0].requested.clone(),
    );
    assert!(
        compose(&base, [&plugin], &bootstrap)
            .unwrap_err()
            .starts_with("plugin_native_dependency_missing:")
    );
    plugin.components[0]
        .requires
        .insert(base[1].id.clone(), base[1].version.clone());
    assert!(compose(&base, [&plugin], &bootstrap).is_ok());
}
