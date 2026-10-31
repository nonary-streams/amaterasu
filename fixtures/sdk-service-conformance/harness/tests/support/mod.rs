use amaterasu_contracts::{Grants, RuntimeConfig, SelectedService};
use amaterasu_package::{
    BootstrapConfig, ComponentManifest, ReleaseManifest, ReleaseSignature, ServiceOffer,
    ServiceRequirement, ServiceTransport, VerificationPolicy, VerifiedPackage, current_target,
    encode_hex, sha256, unix_timestamp, verify_package,
};
use ed25519_dalek::{Signer, SigningKey};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub const STATE: &str = "amaterasu.state/1";
pub const CONTRACT: &str = "amaterasu.state/1";
pub const CONSUMER: &str = "fixture.consumer";
pub const PROVIDER: &str = "amaterasu.state";
pub const CONSUMER_SERVICE: &str = "example.state-consumer/1";
static NEXT: AtomicU64 = AtomicU64::new(0);

fn grants(call: &[&str], provide: &[&str]) -> Grants {
    Grants {
        call: call.iter().map(|s| (*s).into()).collect(),
        provide: provide.iter().map(|s| (*s).into()).collect(),
        ..Default::default()
    }
}
fn component(
    directory: &Path,
    id: &str,
    source: &Path,
    runtime: bool,
    requires: &[&str],
    requested: Grants,
) -> ComponentManifest {
    let bytes = std::fs::read(source).unwrap();
    let filename = format!("{id}.bin");
    std::fs::write(directory.join(&filename), &bytes).unwrap();
    ComponentManifest {
        id: id.into(),
        version: "0.1.0".into(),
        file: filename,
        sha256: sha256(&bytes),
        size: bytes.len() as u64,
        abi_major: amaterasu_abi::ABI_MAJOR,
        runtime,
        requires: requires
            .iter()
            .map(|id| ((*id).into(), "0.1.0".into()))
            .collect(),
        requested,
        service_provides: vec![],
        service_requires: vec![],
    }
}
pub struct Package {
    pub manifest: ReleaseManifest,
    pub bootstrap: BootstrapConfig,
    directory: PathBuf,
    key: SigningKey,
}
impl Package {
    pub fn new(runtime: &Path, provider: &Path, consumer: &Path, allow_delegation: bool) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "sdk-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let mut runtime_component = component(
            &directory,
            "amaterasu.runtime",
            runtime,
            true,
            &[],
            Grants::default(),
        );
        runtime_component.runtime = true;
        let mut state = component(
            &directory,
            PROVIDER,
            provider,
            false,
            &["amaterasu.runtime"],
            grants(&[], &[STATE]),
        );
        state.service_provides.push(ServiceOffer {
            transport: ServiceTransport::Raw,
            service: STATE.into(),
            contract: CONTRACT.into(),
            method: None,
            major: 1,
            minor: 0,
            capabilities: ["get".into(), "set".into(), "delete".into()].into(),
        });
        let mut consumer_component = component(
            &directory,
            CONSUMER,
            consumer,
            false,
            &["amaterasu.runtime", PROVIDER],
            grants(&[STATE], &[CONSUMER_SERVICE]),
        );
        consumer_component
            .service_requires
            .push(ServiceRequirement {
                transport: ServiceTransport::Raw,
                service: STATE.into(),
                contract: CONTRACT.into(),
                method: None,
                major: 1,
                minor: 0,
                capabilities: ["get".into(), "set".into(), "delete".into()].into(),
                optional: false,
            });
        let driver_calls = if allow_delegation {
            vec![CONSUMER_SERVICE, STATE]
        } else {
            vec![CONSUMER_SERVICE]
        };
        let driver = component(
            &directory,
            "fixture.driver",
            consumer,
            false,
            &["amaterasu.runtime"],
            grants(&driver_calls, &[]),
        );
        let other = component(
            &directory,
            "fixture.other",
            consumer,
            false,
            &["amaterasu.runtime"],
            grants(&[STATE], &[STATE]),
        );
        let components = vec![runtime_component, state, consumer_component, driver, other];
        let mut policy = RuntimeConfig::default();
        for item in components.iter().filter(|item| !item.runtime) {
            policy
                .principals
                .insert(item.id.clone(), item.requested.clone());
        }
        policy.service_selections.broker.insert(
            STATE.into(),
            Some(SelectedService {
                provider: PROVIDER.into(),
                contract: CONTRACT.into(),
            }),
        );
        let key = SigningKey::from_bytes(&[42; 32]);
        let bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: BTreeMap::from([(
                "fixture-key".into(),
                encode_hex(key.verifying_key().as_bytes()),
            )]),
            policy,
        };
        let manifest = ReleaseManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence: 1,
            expires_unix: unix_timestamp() + 3600,
            target: current_target(),
            components,
            privileged_requests: vec![],
        };
        Self {
            manifest,
            bootstrap,
            directory,
            key,
        }
    }
    pub fn artifact_path(&self, component: &str) -> PathBuf {
        let artifact = self
            .manifest
            .components
            .iter()
            .find(|item| item.id == component)
            .unwrap();
        self.directory.join(&artifact.file)
    }
    pub fn verify(&self) -> Result<VerifiedPackage, String> {
        let bytes = serde_json::to_vec(&self.manifest).unwrap();
        std::fs::write(self.directory.join("manifest.json"), &bytes).unwrap();
        let signature = ReleaseSignature {
            key_id: "fixture-key".into(),
            signature: encode_hex(&self.key.sign(&bytes).to_bytes()),
        };
        std::fs::write(
            self.directory.join("manifest.sig"),
            serde_json::to_vec(&signature).unwrap(),
        )
        .unwrap();
        verify_package(
            &self.directory,
            &self.bootstrap,
            VerificationPolicy {
                minimum_sequence: 1,
                allow_historical: false,
                now: unix_timestamp(),
            },
        )
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
