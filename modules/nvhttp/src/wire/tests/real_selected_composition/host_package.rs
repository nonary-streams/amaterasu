//! Signed disposable package used to exercise the production host loader.
use super::*;
use amaterasu_contracts::{Grants, RuntimeConfig};
use amaterasu_package::{
    BootstrapConfig, ComponentManifest, ReleaseManifest, ReleaseSignature, ServiceOffer,
    ServiceTransport, VerificationPolicy, create_private_directory, current_target, encode_hex,
    sha256, unix_timestamp, verify_package,
};
use ed25519_dalek::{Signer, SigningKey};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct VerifiedHostPackage {
    pub directory: PathBuf,
    pub bootstrap: BootstrapConfig,
    pub verified: amaterasu_package::VerifiedPackage,
}

pub(super) fn create_signed_package(
    configuration: &Value,
    replacement_selected: bool,
    package_root: &Path,
    artifacts: &Path,
    placeholder: &Path,
) -> VerifiedHostPackage {
    let component_directory = package_root.join("components");
    create_private_directory(&component_directory).expect("create signed component directory");
    let runtime_config: RuntimeConfig =
        serde_json::from_value(configuration.clone()).expect("test RuntimeConfig");
    let grants: BTreeMap<String, Grants> = configuration["principals"]
        .as_object()
        .expect("test principals")
        .iter()
        .map(|(principal, grants)| {
            (
                principal.clone(),
                serde_json::from_value(grants.clone()).expect("test principal grants"),
            )
        })
        .collect();
    let mut ids: Vec<_> = grants
        .keys()
        .filter(|id| {
            !matches!(
                (replacement_selected, id.as_str()),
                (false, "replacement") | (true, "library" | "policy" | "executor")
            )
        })
        .cloned()
        .collect();
    ids.sort_by_key(|id| component_order(id));

    let mut components = vec![component_manifest(
        ComponentInput {
            id: "amaterasu.runtime",
            source: artifact(artifacts, "amaterasu_runtime"),
            requested: Grants::default(),
            runtime: true,
            requires: BTreeMap::new(),
            service_provides: Vec::new(),
        },
        &component_directory,
        0,
    )];
    for (index, id) in ids.iter().enumerate() {
        let selected_artifact = selected_artifact(id, artifacts, placeholder);
        let mut dependencies = BTreeMap::new();
        dependencies.insert("amaterasu.runtime".into(), "0.1.0".into());
        let requested = grants.get(id).cloned().unwrap_or_default();
        let offers = offers_for(id, &runtime_config);
        components.push(component_manifest(
            ComponentInput {
                id,
                source: selected_artifact,
                requested,
                runtime: false,
                requires: dependencies,
                service_provides: offers,
            },
            &component_directory,
            index + 1,
        ));
    }
    let now = unix_timestamp();
    let manifest = ReleaseManifest {
        schema: 1,
        host_context_schema: 1,
        project: "amaterasu".into(),
        sequence: 1,
        expires_unix: now + 3600,
        target: current_target(),
        components,
        privileged_requests: Vec::new(),
    };
    let raw_manifest = serde_json::to_vec(&manifest).expect("serialize signed manifest");
    let signer = SigningKey::from_bytes(&[42; 32]);
    std::fs::write(component_directory.join("manifest.json"), &raw_manifest)
        .expect("write signed manifest");
    let signature = ReleaseSignature {
        key_id: "host-composition-test".into(),
        signature: encode_hex(&signer.sign(&raw_manifest).to_bytes()),
    };
    std::fs::write(
        component_directory.join("manifest.sig"),
        serde_json::to_vec(&signature).expect("serialize manifest signature"),
    )
    .expect("write manifest signature");
    let bootstrap = BootstrapConfig {
        development: true,
        allow_unsigned: false,
        trusted_keys: BTreeMap::from([(
            signature.key_id,
            encode_hex(signer.verifying_key().as_bytes()),
        )]),
        policy: runtime_config,
    };
    let verified = verify_package(
        &component_directory,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: 1,
            allow_historical: false,
            now,
        },
    )
    .expect("verify disposable signed host package");
    VerifiedHostPackage {
        directory: component_directory,
        bootstrap,
        verified,
    }
}

fn component_order(id: &str) -> usize {
    match id {
        "amaterasu.state" => 0,
        "console" | "attacker" | "foreign" | "capture" => 1,
        "fixture-capture" | "fixture-configuration" | "fixture-video" => 2,
        "network" => 3,
        "control" | "media" => 4,
        "application" | "display" | "library" | "policy" | "executor" | "replacement" => 5,
        "session" => 6,
        "amaterasu.pairing" => 7,
        "amaterasu.host-configuration" => 8,
        "amaterasu.launch" => 9,
        "amaterasu.session-lifecycle" => 10,
        "stream" => 11,
        "input" => 12,
        "amaterasu.rtsp" => 13,
        "amaterasu.nvhttp" => 14,
        _ => 20,
    }
}

fn selected_artifact(id: &str, artifacts: &Path, placeholder: &Path) -> PathBuf {
    match id {
        "amaterasu.state" => artifact(artifacts, "amaterasu_state"),
        "amaterasu.host-configuration" => artifact(artifacts, "amaterasu_host_configuration"),
        "amaterasu.pairing" => artifact(artifacts, "amaterasu_pairing"),
        "amaterasu.launch" => artifact(artifacts, "amaterasu_launch"),
        "amaterasu.rtsp" => artifact(artifacts, "amaterasu_rtsp"),
        "amaterasu.session-lifecycle" => artifact(artifacts, "amaterasu_session_lifecycle"),
        "amaterasu.nvhttp" => artifact(artifacts, "amaterasu_nvhttp"),
        "amaterasu.input" | "input" => input_artifact(artifacts),
        "amaterasu.stream-session" | "stream" => artifact(artifacts, "amaterasu_stream_session"),
        "amaterasu.control-transport" | "control" => {
            artifact(artifacts, "amaterasu_control_transport")
        }
        "amaterasu.media-transport" | "media" => artifact(artifacts, "amaterasu_media_transport"),
        "network" => artifact(artifacts, "amaterasu_network"),
        "session" => artifact(artifacts, "amaterasu_session"),
        "library" => PathBuf::from(
            std::env::var_os("AMATERASU_LIBRARY_ARTIFACT").expect("library provider artifact"),
        ),
        "policy" => PathBuf::from(
            std::env::var_os("AMATERASU_STREAMING_POLICY_ARTIFACT")
                .expect("streaming policy artifact"),
        ),
        "executor" => PathBuf::from(
            std::env::var_os("AMATERASU_EXECUTOR_ARTIFACT").expect("executor artifact"),
        ),
        "replacement" => PathBuf::from(
            std::env::var_os("AMATERASU_REPLACEMENT_ARTIFACT").expect("replacement artifact"),
        ),
        _ => placeholder.to_path_buf(),
    }
}

fn input_artifact(artifacts: &Path) -> PathBuf {
    std::env::var_os("AMATERASU_INPUT_QUALIFICATION_ARTIFACT")
        .map(PathBuf::from)
        .unwrap_or_else(|| artifact(artifacts, "amaterasu_input"))
}

struct ComponentInput<'a> {
    id: &'a str,
    source: PathBuf,
    requested: Grants,
    runtime: bool,
    requires: BTreeMap<String, String>,
    service_provides: Vec<ServiceOffer>,
}

fn component_manifest(
    input: ComponentInput<'_>,
    directory: &Path,
    index: usize,
) -> ComponentManifest {
    let ComponentInput {
        id,
        source,
        requested,
        runtime,
        requires,
        service_provides,
    } = input;
    let bytes = std::fs::read(&source)
        .unwrap_or_else(|error| panic!("read signed component {}: {error}", source.display()));
    let file = format!("component-{index:02}.so");
    std::fs::copy(source, directory.join(&file)).expect("copy signed component");
    ComponentManifest {
        id: id.into(),
        version: "0.1.0".into(),
        file,
        sha256: sha256(&bytes),
        size: bytes.len() as u64,
        abi_major: amaterasu_abi::ABI_MAJOR,
        runtime,
        requires,
        service_provides,
        service_requires: Vec::new(),
        requested,
    }
}

fn offers_for(id: &str, config: &RuntimeConfig) -> Vec<ServiceOffer> {
    let mut offers = Vec::new();
    for (transport, selections) in [
        (ServiceTransport::Raw, &config.service_selections.broker),
        (ServiceTransport::Native, &config.service_selections.native),
    ] {
        for (service, selected) in selections {
            let Some(selected) = selected else { continue };
            if selected.provider != id {
                continue;
            }
            offers.push(ServiceOffer {
                transport,
                service: service.clone(),
                contract: selected.contract.clone(),
                method: None,
                major: service
                    .rsplit_once('/')
                    .and_then(|(_, major)| major.parse().ok())
                    .unwrap_or(1),
                minor: 0,
                capabilities: BTreeSet::new(),
            });
        }
    }
    for approval in config
        .api_methods
        .iter()
        .filter(|approval| approval.provider == id)
    {
        offers.push(ServiceOffer {
            transport: ServiceTransport::Api,
            service: approval.method.service.clone(),
            contract: approval.method.service.clone(),
            method: Some(approval.method.method.clone()),
            major: approval.method.interface_major,
            minor: approval.method.interface_minor,
            capabilities: BTreeSet::new(),
        });
    }
    offers
}
