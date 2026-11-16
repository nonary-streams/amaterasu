//! Real native-loader qualification, isolated from the installed service.
use amaterasu_contracts::{Command, Grants, Response, RuntimeConfig};
use amaterasu_package::{
    plugins::{
        PluginManifest,
        composition::PluginSource,
        installed,
        operation::{Operation, Phase},
    },
    repository::*,
    *,
};
use ed25519_dalek::{Signer, SigningKey};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
struct Fixture {
    root: PathBuf,
    base: String,
    bootstrap: BootstrapConfig,
    signer: SigningKey,
    publisher: ApprovedRepository,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn component(id: &str, file: &str, bytes: &[u8], runtime: bool) -> ComponentManifest {
    ComponentManifest {
        id: id.into(),
        version: "0.1.0".into(),
        file: file.into(),
        sha256: sha256(bytes),
        size: bytes.len() as u64,
        abi_major: 1,
        runtime,
        requires: BTreeMap::new(),
        requested: Grants::default(),
    }
}
fn sign(directory: &Path, manifest: &impl serde::Serialize, signer: &SigningKey, key: &str) {
    let raw = serde_json::to_vec(manifest).unwrap();
    std::fs::write(directory.join("manifest.json"), &raw).unwrap();
    write_json_atomically(
        &directory.join("manifest.sig"),
        &ReleaseSignature {
            key_id: key.into(),
            signature: encode_hex(&signer.sign(&raw).to_bytes()),
        },
    )
    .unwrap();
}
impl Fixture {
    fn new(runtime: &Path) -> Self {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-native-plugins-{}",
            random_identifier().unwrap()
        ));
        create_private_directory(&root).unwrap();
        create_private_directory(&root.join("data/state")).unwrap();
        let base_signer = SigningKey::from_bytes(&[11; 32]);
        let base_public = base_signer.verifying_key().to_bytes();
        let base_key = sha256(&base_public);
        let signer = SigningKey::from_bytes(&[53; 32]);
        let public = signer.verifying_key().to_bytes();
        let key = sha256(&public);
        let bootstrap = BootstrapConfig {
            development: true,
            allow_unsigned: false,
            trusted_keys: [(base_key.clone(), encode_hex(&base_public))].into(),
            policy: RuntimeConfig {
                principals: [(
                    "amaterasu.console".into(),
                    Grants {
                        call: ["example.echo/1".into()].into(),
                        ..Default::default()
                    },
                )]
                .into(),
                ..Default::default()
            },
        };
        write_json_atomically(&root.join("bootstrap.json"), &bootstrap).unwrap();
        let bytes = std::fs::read(runtime).unwrap();
        let manifest = ReleaseManifest {
            schema: 1,
            host_context_schema: 1,
            project: "amaterasu".into(),
            sequence: 1,
            expires_unix: unix_timestamp() + 3600,
            target: current_target(),
            components: vec![component("amaterasu.runtime", "runtime.so", &bytes, true)],
            privileged_requests: vec![],
        };
        let base = sha256(&serde_json::to_vec(&manifest).unwrap());
        let package = package_directory(&root, &base).unwrap();
        create_private_directory(&package).unwrap();
        std::fs::write(package.join("runtime.so"), bytes).unwrap();
        sign(&package, &manifest, &base_signer, &base_key);
        write_json_atomically(
            &root.join("selection.json"),
            &ActivationState {
                active: Some(base.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let publisher = ApprovedRepository::approve(
            "plugins".into(),
            "https://example.test/plugins".into(),
            encode_hex(&public),
            key,
        )
        .unwrap();
        RepositoryStore::new(root.join("repositories"))
            .unwrap()
            .approve(publisher.clone())
            .unwrap();
        Self {
            root,
            base,
            bootstrap,
            signer,
            publisher,
        }
    }
    fn offer(&self, echo: &Path, bad: bool) -> Vec<PluginSource> {
        let store = RepositoryStore::new(self.root.join("repositories")).unwrap();
        let mut manifests = vec![];
        for (id, file, bytes) in [
            ("example.echo", "echo.so", std::fs::read(echo).unwrap()),
            (
                "example.invalid",
                "invalid.so",
                b"not an ELF library".to_vec(),
            ),
        ]
        .into_iter()
        .take(if bad { 2 } else { 1 })
        {
            let mut module = component(id, file, &bytes, false);
            module
                .requires
                .insert("amaterasu.runtime".into(), "0.1.0".into());
            if id == "example.echo" {
                module.requested.provide.insert("example.echo/1".into());
            }
            let manifest = PluginManifest {
                schema: 1,
                host_context_schema: 1,
                project: "amaterasu".into(),
                id: id.into(),
                version: "0.1.0".into(),
                sequence: 1,
                expires_unix: read_json::<ReleaseManifest>(
                    &package_directory(&self.root, &self.base)
                        .unwrap()
                        .join("manifest.json"),
                )
                .unwrap()
                .expires_unix,
                target: current_target(),
                components: vec![module],
                drivers: if id == "example.echo"
                    && self.root.join("fixture-requirement.json").exists()
                {
                    vec![read_json(&self.root.join("fixture-requirement.json")).unwrap()]
                } else {
                    vec![]
                },
                api_methods: vec![],
                settings: BTreeMap::new(),
            };
            let digest = sha256(&serde_json::to_vec(&manifest).unwrap());
            let directory = store
                .native_candidate_cache_path("plugins", &digest)
                .unwrap();
            if !directory.exists() {
                create_private_directory(&directory).unwrap();
                std::fs::write(directory.join(file), bytes).unwrap();
                sign(
                    &directory,
                    &manifest,
                    &self.signer,
                    &self.publisher.fingerprint,
                );
            }
            manifests.push((manifest, digest));
        }
        let catalog = Catalog {
            schema: 1,
            repository: "plugins".into(),
            sequence: if bad { 2 } else { 1 },
            expires_unix: unix_timestamp() + 3600,
            target: current_target(),
            candidates: manifests
                .iter()
                .map(|(manifest, digest)| Candidate {
                    slot: manifest.id.clone(),
                    implementation: manifest.id.clone(),
                    version: manifest.version.clone(),
                    kind: CandidateKind::NativePlugin,
                    manifest_sha256: digest.clone(),
                })
                .collect(),
        };
        let raw = serde_json::to_vec(&catalog).unwrap();
        let mut message = b"amaterasu.repository.catalog.v1\n".to_vec();
        message.extend_from_slice(&raw);
        store
            .accept_catalog(
                "plugins",
                &raw,
                &encode_hex(&self.signer.sign(&message).to_bytes()),
                &current_target(),
                unix_timestamp(),
            )
            .unwrap();
        let digest = store
            .catalog("plugins", &current_target(), unix_timestamp())
            .unwrap()
            .digest()
            .to_owned();
        manifests
            .into_iter()
            .map(|(manifest, hash)| PluginSource {
                repository: "plugins".into(),
                publisher: self.publisher.fingerprint.clone(),
                catalog_digest: digest.clone(),
                manifest_digest: hash,
                plugin: manifest.id,
                version: manifest.version,
                sequence: 1,
            })
            .collect()
    }
    fn stage(&self, sources: Vec<PluginSource>) {
        let mut op = Operation::prepare(
            installed::revision(&self.root, &self.base).unwrap(),
            sources,
            "fixture".into(),
            vec![],
        )
        .unwrap();
        op.phase = Phase::ReadyToActivate;
        installed::stage(&self.root, &self.base, &op).unwrap();
        assert!(installed::begin_trial(&self.root).unwrap());
    }
    fn load(&self) -> PackageResult<crate::loading::LoadedComponents> {
        crate::publisher_selection::with_verified_selected(
            &self.root,
            &self.base,
            &self.bootstrap,
            crate::publisher_selection::SelectionPurpose::Worker,
            &mut |_, _| Ok(()),
            |package| {
                crate::loading::LoadedComponents::load(
                    &self.root,
                    &package_directory(&self.root, &self.base)?,
                    &package,
                    &self.root.join("data/state"),
                    None,
                    &mut |_| Ok(()),
                )
            },
        )
    }
}
#[test]
#[ignore = "requires separately built real runtime and echo native libraries"]
fn native_plugin_add_trial_commit_rollback_and_revocation() {
    let runtime = PathBuf::from(std::env::var("AMATERASU_RUNTIME_PLUGIN").unwrap());
    let echo = PathBuf::from(std::env::var("AMATERASU_ECHO_PLUGIN").unwrap());
    let fixture = Fixture::new(&runtime);
    let sources = fixture.offer(&echo, false);
    fixture.stage(sources);
    let mut loaded = fixture.load().unwrap();
    loaded.tick().unwrap();
    loaded.health().unwrap();
    let endpoint = loaded.console_endpoint().unwrap();
    let request = crate::worker::invoke(
        &endpoint,
        &Command::Request {
            service: "example.echo/1".into(),
            payload: serde_json::json!({"real_plugin":"works"}),
            timeout_ms: 1000,
            operation_id: "native-plugin-qualification".into(),
            parent: None,
        },
    )
    .unwrap();
    let Response::Accepted { id } = request else {
        panic!("native plugin was not routed");
    };
    loaded.tick().unwrap();
    assert!(
        matches!(crate::worker::invoke(&endpoint,&Command::TakeResult{request:id}).unwrap(),Response::Result{payload} if payload["echo"]["real_plugin"]=="works")
    );
    installed::finish_trial(&fixture.root, true).unwrap();
    loaded.shutdown(Duration::from_secs(2)).unwrap();
    assert_eq!(installed::read(&fixture.root).unwrap().active.len(), 1);
    let sources = fixture.offer(&echo, true);
    fixture.stage(sources);
    assert!(fixture.load().is_err());
    crate::loading::require_process_usable().unwrap();
    installed::finish_trial(&fixture.root, false).unwrap();
    let mut restored = fixture.load().unwrap();
    restored.tick().unwrap();
    restored.health().unwrap();
    restored.shutdown(Duration::from_secs(2)).unwrap();
    assert_eq!(installed::read(&fixture.root).unwrap().active.len(), 1);
    RepositoryStore::new(fixture.root.join("repositories"))
        .unwrap()
        .revoke("plugins")
        .unwrap();
    assert!(fixture.load().is_err());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires built native fixtures and passwordless sudo; changes only isolated temporary fixture files"]
fn plugin_setup_through_real_user_service_and_privileged_broker() {
    qualify_plugin_service(false);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires built native fixtures and passwordless sudo; interrupts only its isolated fixture broker"]
fn plugin_setup_reconciles_after_privileged_service_restart() {
    qualify_plugin_service(true);
}

#[cfg(target_os = "linux")]
fn qualify_plugin_service(interrupt: bool) {
    use amaterasu_contracts::{
        ApiMethodApproval, HOST_LIFECYCLE_SERVICE, HOST_PACKAGE_ACTIVATION_SERVICE,
        HOST_PACKAGE_BARRIER_SERVICE, HOST_STATE_CONTEXT_SERVICE,
    };
    use amaterasu_module_sdk::drivers::{
        Artifact, DriverRequirement, INSTALLATION_SERVICE, installation_methods,
    };
    use std::{
        fs,
        os::unix::{
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
        process::{Child, Command as ProcessCommand},
        time::Instant,
    };
    struct Process {
        child: Child,
        privileged: bool,
    }
    impl Drop for Process {
        fn drop(&mut self) {
            if self.privileged {
                let _ = ProcessCommand::new("sudo")
                    .args([
                        "-n",
                        "kill",
                        "-TERM",
                        "--",
                        &format!("-{}", self.child.id()),
                    ])
                    .status();
            } else {
                unsafe {
                    libc::kill(self.child.id() as i32, libc::SIGTERM);
                }
            }
            let _ = self.child.wait();
        }
    }
    let artifact_path = |name: &str| PathBuf::from(std::env::var(name).unwrap());
    let host = artifact_path("AMATERASU_QUALIFICATION_HOST");
    let runtime = artifact_path("AMATERASU_RUNTIME_PLUGIN");
    let echo = artifact_path("AMATERASU_ECHO_PLUGIN");
    let network = artifact_path("AMATERASU_NETWORK_PLUGIN");
    let installer = artifact_path("AMATERASU_INSTALLER_PLUGIN");
    let mut fixture = Fixture::new(&runtime);
    let machine = fixture.root.join("machine");
    fs::create_dir(&machine).unwrap();
    fs::set_permissions(&machine, fs::Permissions::from_mode(0o755)).unwrap();
    struct MachineDirectory(PathBuf);
    impl Drop for MachineDirectory {
        fn drop(&mut self) {
            assert!(self.0.file_name().is_some_and(|name| name == "machine"));
            let _ = ProcessCommand::new("sudo")
                .args(["-n", "rm", "-rf", "--"])
                .arg(&self.0)
                .status();
        }
    }
    let _machine_directory = MachineDirectory(machine.clone());
    assert!(
        ProcessCommand::new("sudo")
            .args(["-n", "chown", "root:root", "--"])
            .arg(&machine)
            .status()
            .unwrap()
            .success()
    );
    let endpoint = machine.join("control.sock");
    let marker = fixture.root.join("driver-fixture-installed");
    let interrupted = fixture.root.join("driver-fixture-interrupted");
    let reconciled = fixture.root.join("driver-fixture-reconciled");
    let helper=br##"#!/usr/bin/python3
import json,os,pathlib,sys
request=json.load(sys.stdin)
config=json.loads(pathlib.Path('fixture-config.json').read_text())
marker=pathlib.Path(config['marker'])
if request['action']=='install':
    assert os.geteuid()==0, 'install must run as root'
    interrupted=pathlib.Path(config['interrupted'])
    if config['interrupt'] and not interrupted.exists():
        interrupted.write_text('fixture broker interrupted')
        interrupted.chmod(0o644)
        # This helper belongs only to the isolated fixture broker. No installed
        # service, driver or stream exists in this test installation.
        os.kill(os.getppid(),9)
        sys.exit(2)
    marker.write_text('isolated fixture installed')
    marker.chmod(0o644)
if request['action']=='reconcile' and pathlib.Path(config['interrupted']).exists():
    pathlib.Path(config['reconciled']).write_text('unfinished fixture reconciled')
if request['action']=='verify':
    assert str(os.geteuid())==request['runtime_identity'], 'verify must run as runtime user'
json.dump({'schema':1,'operation':request['operation'],'driver':request['requirement']['id'],'state':'ready' if marker.exists() else 'missing','installed_version':'1' if marker.exists() else None,'message':'Fixture ready' if marker.exists() else 'Fixture missing','cancellable':True},sys.stdout)
"##;
    let config = serde_json::to_vec(&serde_json::json!({"marker":marker,"interrupt":interrupt,"interrupted":interrupted,"reconciled":reconciled})).unwrap();
    let requirement = DriverRequirement {
        schema: 1,
        id: "example.fixture".into(),
        name: "Isolated fixture".into(),
        target: current_target(),
        feature: "example.fixture".into(),
        reason: "Qualify the real installation boundary without changing a driver".into(),
        compatibility: "1".into(),
        may_require_reboot: false,
        offered_version: None,
        installer: Artifact {
            file: "fixture-helper".into(),
            sha256: sha256(helper),
            size: helper.len() as u64,
        },
        payloads: vec![Artifact {
            file: "fixture-config.json".into(),
            sha256: sha256(&config),
            size: config.len() as u64,
        }],
    };
    write_json_atomically(&fixture.root.join("fixture-requirement.json"), &requirement).unwrap();
    let sources = fixture.offer(&echo, false);
    let plugin = RepositoryStore::new(fixture.root.join("repositories"))
        .unwrap()
        .existing_native_candidate_cache_path("plugins", &sources[0].manifest_digest)
        .unwrap();
    fs::write(plugin.join("fixture-helper"), helper).unwrap();
    fs::write(plugin.join("fixture-config.json"), config).unwrap();
    // The fixture coordinator has the real management identity and API grants,
    // but no listeners or stream-producing modules exist in this installation.
    let manifests = [
        (
            "amaterasu.runtime",
            "runtime.so",
            runtime,
            true,
            Grants::default(),
        ),
        (
            "amaterasu.network",
            "network.so",
            network,
            false,
            Grants {
                native_provide: ["amaterasu.network/1".into()].into(),
                call: [HOST_STATE_CONTEXT_SERVICE.into()].into(),
                ..Default::default()
            },
        ),
        (
            "amaterasu.plugin-installation",
            "installer.so",
            installer,
            false,
            Grants {
                provide: [INSTALLATION_SERVICE.into()].into(),
                native_use: ["amaterasu.network/1".into()].into(),
                call: [
                    HOST_STATE_CONTEXT_SERVICE.into(),
                    HOST_PACKAGE_ACTIVATION_SERVICE.into(),
                    HOST_LIFECYCLE_SERVICE.into(),
                ]
                .into(),
                ..Default::default()
            },
        ),
        (
            "amaterasu.management",
            "management.so",
            echo,
            false,
            Grants {
                call: [
                    INSTALLATION_SERVICE.into(),
                    HOST_STATE_CONTEXT_SERVICE.into(),
                    HOST_PACKAGE_BARRIER_SERVICE.into(),
                    "example.echo/1".into(),
                ]
                .into(),
                ..Default::default()
            },
        ),
    ];
    let mut components = vec![];
    let mut files = vec![];
    for (id, file, path, is_runtime, grants) in manifests {
        let bytes = fs::read(path).unwrap();
        let mut module = component(id, file, &bytes, is_runtime);
        module.requested = grants.clone();
        if !is_runtime {
            module
                .requires
                .insert("amaterasu.runtime".into(), "0.1.0".into());
        }
        if id == "amaterasu.plugin-installation" {
            module
                .requires
                .insert("amaterasu.network".into(), "0.1.0".into());
        }
        if id == "amaterasu.management" {
            module
                .requires
                .insert("amaterasu.plugin-installation".into(), "0.1.0".into());
        }
        fixture
            .bootstrap
            .policy
            .principals
            .insert(id.into(), grants);
        components.push(module);
        files.push((file, bytes));
    }
    fixture.bootstrap.development = false;
    fixture.bootstrap.policy.api_methods = installation_methods()
        .into_iter()
        .map(|method| ApiMethodApproval {
            provider: "amaterasu.plugin-installation".into(),
            callers: ["amaterasu.management".into()].into(),
            method,
        })
        .collect();
    fixture.bootstrap.policy.settings.insert(
        "amaterasu.plugin-installation".into(),
        serde_json::json!({"broker_endpoint":endpoint}),
    );
    fixture.bootstrap.policy.settings.insert(
        "amaterasu.management".into(),
        serde_json::json!({"fixture_plugin_manifest":sources[0].manifest_digest,"fixture_retry_failed":interrupt}),
    );
    write_json_atomically(&fixture.root.join("bootstrap.json"), &fixture.bootstrap).unwrap();
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
    fixture.base = sha256(&serde_json::to_vec(&manifest).unwrap());
    let package = package_directory(&fixture.root, &fixture.base).unwrap();
    create_private_directory(&package).unwrap();
    for (file, bytes) in files {
        fs::write(package.join(file), bytes).unwrap();
    }
    let base_signer = SigningKey::from_bytes(&[11; 32]);
    let base_key = sha256(&base_signer.verifying_key().to_bytes());
    sign(&package, &manifest, &base_signer, &base_key);
    write_json_atomically(
        &fixture.root.join("selection.json"),
        &ActivationState {
            active: Some(fixture.base.clone()),
            highest_sequence: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let policy = amaterasu_plugin_installation::broker::Policy {
        repository_directory: None,
        root: fixture.root.clone(),
        endpoint: endpoint.clone(),
        approvals: machine.join("approvals"),
        host_sha256: sha256(&fs::read(&host).unwrap()),
        runtime_identity: unsafe { libc::geteuid() }.to_string(),
        publishers: vec![fixture.publisher.fingerprint.clone()],
    };
    let policy_file = fixture.root.join("fixture-policy.json");
    write_json_atomically(&policy_file, &policy).unwrap();
    let protected_policy = machine.join("policy.json");
    assert!(
        ProcessCommand::new("sudo")
            .args(["-n", "install", "-o", "root", "-g", "root", "-m", "600"])
            .arg(&policy_file)
            .arg(&protected_policy)
            .status()
            .unwrap()
            .success()
    );
    let start_broker = || {
        let broker_log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(fixture.root.join("broker.log"))
            .unwrap();
        Process {
            child: ProcessCommand::new("sudo")
                .arg("-n")
                .arg(&host)
                .arg("plugin-driver-service")
                .arg(&protected_policy)
                .stdout(broker_log.try_clone().unwrap())
                .stderr(broker_log)
                .process_group(0)
                .spawn()
                .unwrap(),
            privileged: true,
        }
    };
    let mut broker = Some(start_broker());
    let mut restarted = false;
    let deadline = Instant::now() + Duration::from_secs(90);
    while !endpoint.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let service_log = fs::File::create(fixture.root.join("service.log")).unwrap();
    let mut service = Process {
        child: ProcessCommand::new(&host)
            .arg("service-user")
            .arg(&fixture.root)
            .arg(&fixture.base)
            .stdout(service_log.try_clone().unwrap())
            .stderr(service_log)
            .spawn()
            .unwrap(),
        privileged: false,
    };
    let result = fixture
        .root
        .join("data/state/plugin-qualification-result.json");
    while !result.exists()
        && Instant::now() < deadline
        && service.child.try_wait().unwrap().is_none()
    {
        if interrupt && interrupted.exists() && !restarted {
            drop(broker.take());
            broker = Some(start_broker());
            restarted = true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !result.exists() {
        panic!(
            "service qualification failed: {}\nbroker: {}\noperation: {:?}",
            fs::read_to_string(fixture.root.join("service.log")).unwrap(),
            fs::read_to_string(fixture.root.join("broker.log")).unwrap(),
            fs::read_to_string(
                fixture
                    .root
                    .join("data/state/plugin-installation/operation.json")
            )
        );
    }
    assert_eq!(
        read_json::<serde_json::Value>(&result).unwrap()["phase"],
        "ready"
    );
    let selected = installed::read(&fixture.root).unwrap();
    assert_eq!(selected.active, sources);
    assert!(selected.pending.is_none());
    assert!(!selected.trial);
    if interrupt {
        assert!(
            restarted && reconciled.exists(),
            "interrupted installation must reconcile before retrying"
        );
    }
    assert_eq!(
        fs::metadata(&marker).unwrap().uid(),
        0,
        "machine helper actually ran elevated"
    );
    drop(service);
    drop(broker);
    // Only the explicit fixture machine directory and marker need root cleanup.
    assert!(machine.starts_with(&fixture.root));
    assert!(
        ProcessCommand::new("sudo")
            .args(["-n", "rm", "-rf", "--"])
            .arg(machine)
            .status()
            .unwrap()
            .success()
    );
}
