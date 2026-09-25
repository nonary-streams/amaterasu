use super::*;
use std::fs;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-migration-test-{}",
            random_identifier().unwrap()
        ));
        create_private_directory(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn source(product: LegacyProduct, root: &Path) -> LegacyConfigSource {
    LegacyConfigSource {
        product,
        root: root.to_path_buf(),
    }
}

#[test]
fn detects_only_known_product_roots_with_supported_configuration() {
    let fixture = TemporaryDirectory::new();
    write(
        &fixture.path().join("Sunshine/sunshine.conf"),
        b"credentials = preserved",
    );
    write(
        &fixture.path().join("Vibeshine/credentials/key.pem"),
        b"private key",
    );
    fs::create_dir_all(fixture.path().join("Apollo")).unwrap();
    write(&fixture.path().join("unknown/apps.json"), b"[]");

    let found = discover_legacy_sources(&LegacySearchPaths {
        profile_data_roots: vec![fixture.path().to_path_buf()],
        installation_roots: vec![],
    })
    .unwrap();

    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .any(|item| item.product == LegacyProduct::Sunshine)
    );
    assert!(
        found
            .iter()
            .any(|item| item.product == LegacyProduct::Vibeshine)
    );
}

#[test]
fn discovery_deduplicates_lexical_aliases_of_existing_roots() {
    let fixture = TemporaryDirectory::new();
    let profile_root = fixture.path().join("profiles");
    fs::create_dir_all(profile_root.join("nested")).unwrap();
    write(
        &profile_root.join("Sunshine/sunshine.conf"),
        b"port = 47990",
    );

    let found = discover_legacy_sources(&LegacySearchPaths {
        profile_data_roots: vec![profile_root.clone(), profile_root.join("nested/..")],
        installation_roots: vec![],
    })
    .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].root, profile_root.join("Sunshine"));
}

#[cfg(unix)]
#[test]
fn discovery_keeps_case_distinct_roots_on_case_sensitive_filesystems() {
    let fixture = TemporaryDirectory::new();
    let lowercase_profile = fixture.path().join("profiles");
    let uppercase_profile = fixture.path().join("Profiles");
    fs::create_dir_all(&lowercase_profile).unwrap();
    match fs::create_dir(&uppercase_profile) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return,
        Err(error) => panic!("failed to create case-distinct profile root: {error}"),
    }
    write(
        &lowercase_profile.join("Sunshine/sunshine.conf"),
        b"port = 47990",
    );
    write(
        &uppercase_profile.join("Sunshine/sunshine.conf"),
        b"port = 47991",
    );

    let found = discover_legacy_sources(&LegacySearchPaths {
        profile_data_roots: vec![lowercase_profile.clone(), uppercase_profile.clone()],
        installation_roots: vec![],
    })
    .unwrap();

    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .any(|source| source.root == lowercase_profile.join("Sunshine"))
    );
    assert!(
        found
            .iter()
            .any(|source| source.root == uppercase_profile.join("Sunshine"))
    );
}

#[cfg(unix)]
#[test]
fn discovery_keeps_distinct_non_unicode_roots() {
    use std::os::unix::ffi::OsStringExt;

    let fixture = TemporaryDirectory::new();
    let first_profile = fixture
        .path()
        .join(std::ffi::OsString::from_vec(b"profile-\xff".to_vec()));
    let second_profile = fixture
        .path()
        .join(std::ffi::OsString::from_vec(b"profile-\xfe".to_vec()));
    write(
        &first_profile.join("Sunshine/sunshine.conf"),
        b"port = 47990",
    );
    write(
        &second_profile.join("Sunshine/sunshine.conf"),
        b"port = 47991",
    );

    let found = discover_legacy_sources(&LegacySearchPaths {
        profile_data_roots: vec![first_profile.clone(), second_profile.clone()],
        installation_roots: vec![],
    })
    .unwrap();

    assert_eq!(found.len(), 2);
    assert!(
        found
            .iter()
            .any(|source| source.root == first_profile.join("Sunshine"))
    );
    assert!(
        found
            .iter()
            .any(|source| source.root == second_profile.join("Sunshine"))
    );
}

#[cfg(windows)]
#[test]
fn discovery_deduplicates_windows_case_variants_of_existing_roots() {
    let fixture = TemporaryDirectory::new();
    let lowercase_profile = fixture.path().join("profiles");
    write(
        &lowercase_profile.join("Sunshine/sunshine.conf"),
        b"port = 47990",
    );
    let uppercase_profile = fixture.path().join("PROFILES");
    assert!(uppercase_profile.is_dir());

    let found = discover_legacy_sources(&LegacySearchPaths {
        profile_data_roots: vec![lowercase_profile.clone(), uppercase_profile],
        installation_roots: vec![],
    })
    .unwrap();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].root, lowercase_profile.join("Sunshine"));
}

#[test]
fn preflight_refuses_sensitive_configuration_without_exposing_its_contents() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    let secret = b"pairing-private-key: never print this";
    write(&predecessor.join("sunshine.conf"), b"fps = 60");
    write(&predecessor.join("credentials/client.pem"), secret);
    write(
        &predecessor.join("credentials/nested/token"),
        b"other secret",
    );
    let data_root = fixture.path().join("Amaterasu");

    let plan =
        analyze_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).unwrap();

    assert_eq!(plan.critical_items.len(), 1);
    assert_eq!(plan.critical_items[0].item, "pairing_credentials");
    let encoded = serde_json::to_string(&plan).unwrap();
    assert!(!encoded.contains("pairing-private-key"));
    assert!(
        preflight_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).is_err()
    );
    assert!(
        preflight_legacy_configuration_with_sensitive_sources(
            &[source(LegacyProduct::Sunshine, &predecessor)],
            &[ValidatedSensitiveSource {
                product: LegacyProduct::Sunshine,
                pairing: false,
                authentication: true,
            }],
        )
        .is_err()
    );
    assert!(
        import_legacy_configuration(&data_root, &[source(LegacyProduct::Sunshine, &predecessor)])
            .is_err()
    );
    assert!(!data_root.join(LEGACY_IMPORT_DIRECTORY).exists());
}

#[test]
fn approved_pairing_preserves_only_contained_configured_identity_files() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        b"pkey = credentials/key.pem # retained key path\ncert = credentials/cert.pem # retained cert path\n",
    );
    write(&predecessor.join("credentials/key.pem"), b"key");
    write(&predecessor.join("credentials/cert.pem"), b"cert");
    let collected = collect_configuration_files(&predecessor).unwrap().unwrap();
    for configured in [
        PathBuf::from("credentials/key.pem"),
        PathBuf::from("credentials/cert.pem"),
    ] {
        assert_eq!(
            collected
                .files
                .iter()
                .filter(|path| path.as_path() == configured.as_path())
                .count(),
            1
        );
    }
    let legacy_source = source(LegacyProduct::Sunshine, &predecessor);
    assert!(
        preflight_legacy_configuration_with_sensitive_sources(
            std::slice::from_ref(&legacy_source),
            &[ValidatedSensitiveSource {
                product: LegacyProduct::Sunshine,
                pairing: false,
                authentication: true,
            }],
        )
        .is_err()
    );
    let plan = preflight_legacy_configuration_with_sensitive_sources(
        std::slice::from_ref(&legacy_source),
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: true,
            authentication: false,
        }],
    )
    .unwrap();
    assert!(plan.critical_items.is_empty());
    let destination = fixture.path().join("Amaterasu");
    import_legacy_configuration_with_sensitive_sources(
        &destination,
        &[legacy_source],
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: true,
            authentication: false,
        }],
    )
    .unwrap();
    assert!(
        destination
            .join(LEGACY_IMPORT_DIRECTORY)
            .join("sunshine/credentials/key.pem")
            .is_file()
    );

    write(
        &predecessor.join("sunshine.conf"),
        b"pkey = ../outside.pem\ncert = credentials/cert.pem\n",
    );
    assert!(
        analyze_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).is_err()
    );
}

#[test]
fn approved_pairing_preserves_custom_identity_bytes_for_each_product() {
    let products = [
        (LegacyProduct::Apollo, "apollo.conf", "apollo"),
        (LegacyProduct::Sunshine, "sunshine.conf", "sunshine"),
        (LegacyProduct::Vibeshine, "vibeshine.conf", "vibeshine"),
        (LegacyProduct::Vibepollo, "vibepollo.conf", "vibepollo"),
    ];

    for (product, configuration_file, import_directory) in products {
        let fixture = TemporaryDirectory::new();
        let predecessor = fixture.path().join("source");
        let pkey_relative_path = PathBuf::from("credentials/custom-key.pem");
        let cert_relative_path = PathBuf::from("credentials/custom-cert.pem");
        let pkey_bytes: &[u8] = b"synthetic pkey test bytes\x00\xff";
        let cert_bytes: &[u8] = b"synthetic cert test bytes\x01\xfe";

        write(
            &predecessor.join(configuration_file),
            b"pkey = credentials/custom-key.pem\ncert = credentials/custom-cert.pem\n",
        );
        write(&predecessor.join(&pkey_relative_path), pkey_bytes);
        write(&predecessor.join(&cert_relative_path), cert_bytes);

        let legacy_source = source(product, &predecessor);
        let validated_pairing = [ValidatedPairingSource { product }];
        let plan = preflight_legacy_configuration_with_pairing(
            std::slice::from_ref(&legacy_source),
            &validated_pairing,
        )
        .unwrap();
        assert!(
            plan.critical_items.is_empty(),
            "validated pairing should clear identity preflight for {import_directory}"
        );

        let data_root = fixture.path().join("Amaterasu");
        let receipt = import_legacy_configuration_with_pairing(
            &data_root,
            &[legacy_source],
            &validated_pairing,
        )
        .unwrap();

        for (relative_path, expected_bytes, identity_kind) in [
            (&pkey_relative_path, pkey_bytes, "pkey"),
            (&cert_relative_path, cert_bytes, "cert"),
        ] {
            let imported_path = data_root
                .join(LEGACY_IMPORT_DIRECTORY)
                .join(import_directory)
                .join(relative_path);
            assert!(
                receipt
                    .imported_files
                    .iter()
                    .any(|imported| imported.path == imported_path),
                "{identity_kind} was not imported under {import_directory}"
            );
            let imported_bytes = fs::read(imported_path).unwrap();
            assert!(
                imported_bytes.as_slice() == expected_bytes,
                "custom {identity_kind} bytes changed for {import_directory}"
            );
        }
    }
}

#[test]
fn configured_credentials_file_is_authentication_input_not_pairing_credentials() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    let credentials = b"authentication credentials\x00with exact bytes\xff";
    write(
        &predecessor.join("sunshine.conf"),
        b"credentials_file = \"credentials/custom_state.json\"\n",
    );
    write(
        &predecessor.join("credentials/custom_state.json"),
        credentials,
    );

    let legacy_source = source(LegacyProduct::Sunshine, &predecessor);
    let analyzed = analyze_legacy_configuration(std::slice::from_ref(&legacy_source)).unwrap();
    assert_eq!(analyzed.schema, 1);
    assert!(
        analyzed
            .critical_items
            .iter()
            .any(|item| item.item == "credentials_file")
    );
    assert!(
        !analyzed
            .critical_items
            .iter()
            .any(|item| item.item == "pairing_credentials")
    );
    assert!(
        !analyzed
            .critical_items
            .iter()
            .any(|item| item.item == "pairing_identity_state")
    );

    let validated = [ValidatedSensitiveSource {
        product: LegacyProduct::Sunshine,
        pairing: false,
        authentication: true,
    }];
    let plan = preflight_legacy_configuration_with_sensitive_sources(
        std::slice::from_ref(&legacy_source),
        &validated,
    )
    .unwrap();
    assert_eq!(plan.schema, 1);
    assert!(plan.critical_items.is_empty());

    let destination = fixture.path().join("Amaterasu");
    let receipt = import_legacy_configuration_with_sensitive_sources(
        &destination,
        &[legacy_source],
        &validated,
    )
    .unwrap();
    assert!(receipt.imported_files.iter().any(|imported| {
        imported
            .path
            .ends_with("sunshine/credentials/custom_state.json")
    }));
    assert_eq!(
        fs::read(
            destination
                .join(LEGACY_IMPORT_DIRECTORY)
                .join("sunshine/credentials/custom_state.json")
        )
        .unwrap(),
        credentials
    );

    write(
        &predecessor.join("sunshine.conf"),
        b"credentials_file = \"credentials/custom_state.json\"\n\
file_state = credentials/custom_state.json\n",
    );
    let explicit_state_source = source(LegacyProduct::Sunshine, &predecessor);
    let explicit_state_plan =
        analyze_legacy_configuration(std::slice::from_ref(&explicit_state_source)).unwrap();
    assert!(
        explicit_state_plan
            .critical_items
            .iter()
            .any(|item| item.item == "pairing_identity_state")
    );
    assert!(
        preflight_legacy_configuration_with_sensitive_sources(
            &[explicit_state_source],
            &validated,
        )
        .is_err()
    );
}

#[test]
fn empty_credentials_file_inherits_configured_file_state() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    let state = b"configured primary state";
    write(
        &predecessor.join("sunshine.conf"),
        b"credentials_file = \"\"\nfile_state = \"private/custom_state.json\"\n",
    );
    write(&predecessor.join("private/custom_state.json"), state);

    let collected = collect_configuration_files(&predecessor).unwrap().unwrap();
    let state_path = PathBuf::from("private/custom_state.json");
    assert!(collected.files.contains(&state_path));
    assert!(collected.configured_state_files.contains(&state_path));
    assert!(
        !collected
            .configured_authentication_files
            .contains(&state_path)
    );

    let destination = fixture.path().join("Amaterasu");
    let receipt = import_legacy_configuration_with_sensitive_sources(
        &destination,
        &[source(LegacyProduct::Sunshine, &predecessor)],
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: true,
            authentication: true,
        }],
    )
    .unwrap();
    let imported_state_path = destination
        .join(LEGACY_IMPORT_DIRECTORY)
        .join("sunshine/private/custom_state.json");
    assert!(
        receipt
            .imported_files
            .iter()
            .any(|imported| imported.path == imported_state_path)
    );
    assert_eq!(fs::read(imported_state_path).unwrap(), state);

    for empty_identity_path in ["pkey = \"\"\n", "cert = \"\"\n"] {
        write(
            &predecessor.join("sunshine.conf"),
            empty_identity_path.as_bytes(),
        );
        assert!(collect_configuration_files(&predecessor).is_err());
    }

    write(
        &predecessor.join("sunshine.conf"),
        b"credentials_file = \"../outside.json\"\n",
    );
    assert!(collect_configuration_files(&predecessor).is_err());
}

#[test]
fn snapshots_configured_paths_with_hashes_inside_quoted_values() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        b"pkey = \"credentials/key#identity.pem\" # identity key\n\
cert = \"credentials/cert#chain.pem\" # identity certificate\n\
credentials_file = \"credentials/auth#store.json\" # authentication file\n\
file_state = \"private/primary#state.json\" # primary state\n\
vibeshine_file_state = \"private/auxiliary#state.json\" # auxiliary state\n",
    );

    let preserved_files = [
        (
            PathBuf::from("credentials/key#identity.pem"),
            b"private key bytes\x00".as_slice(),
        ),
        (
            PathBuf::from("credentials/cert#chain.pem"),
            b"certificate bytes\x01".as_slice(),
        ),
        (
            PathBuf::from("credentials/auth#store.json"),
            b"authentication bytes\x02".as_slice(),
        ),
        (
            PathBuf::from("private/primary#state.json"),
            b"primary state bytes\x03".as_slice(),
        ),
        (
            PathBuf::from("private/auxiliary#state.json"),
            b"auxiliary state bytes\x04".as_slice(),
        ),
    ];
    for (relative, bytes) in &preserved_files {
        write(&predecessor.join(relative), bytes);
    }

    let collected = collect_configuration_files(&predecessor).unwrap().unwrap();
    for (relative, _) in &preserved_files {
        assert!(collected.files.contains(relative));
    }
    for relative in [
        PathBuf::from("private/primary#state.json"),
        PathBuf::from("private/auxiliary#state.json"),
    ] {
        assert!(collected.configured_state_files.contains(&relative));
    }

    let destination = fixture.path().join("Amaterasu");
    import_legacy_configuration_with_sensitive_sources(
        &destination,
        &[source(LegacyProduct::Sunshine, &predecessor)],
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: true,
            authentication: true,
        }],
    )
    .unwrap();

    for (relative, expected_bytes) in preserved_files {
        let imported = destination
            .join(LEGACY_IMPORT_DIRECTORY)
            .join("sunshine")
            .join(&relative);
        assert_eq!(
            fs::read(imported).unwrap().as_slice(),
            expected_bytes,
            "imported bytes changed for {}",
            relative.display()
        );
    }
}

#[cfg(windows)]
#[test]
fn configured_identity_path_case_variants_share_one_snapshot_entry() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        b"pkey = CREDENTIALS/key.pem\n",
    );
    write(&predecessor.join("credentials/Key.PEM"), b"private key");

    let collected = collect_configuration_files(&predecessor).unwrap().unwrap();
    assert_eq!(
        collected
            .files
            .iter()
            .filter(|path| path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("key.pem")))
            .count(),
        1
    );
}

#[cfg(windows)]
#[test]
fn configured_identity_path_unicode_case_variants_share_one_snapshot_entry() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        "pkey = CREDENTIALS/ÉTAT.PEM\n".as_bytes(),
    );
    write(&predecessor.join("credentials/état.pem"), b"private key");

    let collected = collect_configuration_files(&predecessor).unwrap().unwrap();
    assert_eq!(
        collected
            .files
            .iter()
            .filter(|path| path
                .file_name()
                .is_some_and(|name| { name.to_string_lossy().to_lowercase() == "état.pem" }))
            .count(),
        1
    );
}

#[cfg(windows)]
#[test]
fn configured_identity_path_windows_sigma_mismatch_is_rejected() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        "pkey = CREDENTIALS/Σ.PEM\n".as_bytes(),
    );
    write(&predecessor.join("credentials/ς.pem"), b"private key");

    assert!(collect_configuration_files(&predecessor).is_err());
}

#[cfg(unix)]
#[test]
fn snapshots_contained_sensitive_symlink_targets_under_the_configured_alias() {
    use std::os::unix::fs::symlink;

    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        b"pkey = credentials/key-alias.pem # source uses this alias\n",
    );
    write(&predecessor.join("private/key.pem"), b"private key");
    fs::create_dir_all(predecessor.join("credentials")).unwrap();
    symlink(
        "../private/key.pem",
        predecessor.join("credentials/key-alias.pem"),
    )
    .unwrap();

    let destination = fixture.path().join("Amaterasu");
    import_legacy_configuration_with_sensitive_sources(
        &destination,
        &[source(LegacyProduct::Sunshine, &predecessor)],
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: true,
            authentication: false,
        }],
    )
    .unwrap();

    assert_eq!(
        fs::read(
            destination
                .join(LEGACY_IMPORT_DIRECTORY)
                .join("sunshine/credentials/key-alias.pem")
        )
        .unwrap(),
        b"private key"
    );
}

#[test]
fn snapshots_configured_primary_and_auxiliary_state_files_only_when_contained() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibeshine");
    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = private/primary_state.json # custom primary\nvibeshine_file_state = private/auxiliary_state.json\n",
    );
    write(&predecessor.join("private/primary_state.json"), b"primary");
    write(
        &predecessor.join("private/auxiliary_state.json"),
        b"auxiliary",
    );

    let files = collect_configuration_files(&predecessor).unwrap().unwrap();
    assert!(
        files
            .files
            .contains(&PathBuf::from("private/primary_state.json"))
    );
    assert!(
        files
            .files
            .contains(&PathBuf::from("private/auxiliary_state.json"))
    );
    assert!(
        files
            .configured_state_files
            .contains(&PathBuf::from("private/primary_state.json"))
    );
    assert!(
        files
            .configured_state_files
            .contains(&PathBuf::from("private/auxiliary_state.json"))
    );

    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = private/primary_state.json\nvibeshine_file_state = private/not_created_yet.json\n",
    );
    let files = collect_configuration_files(&predecessor).unwrap().unwrap();
    assert!(
        files
            .files
            .contains(&PathBuf::from("private/primary_state.json"))
    );
    assert!(
        !files
            .files
            .contains(&PathBuf::from("private/not_created_yet.json"))
    );

    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = ../outside_state.json\n",
    );
    assert!(collect_configuration_files(&predecessor).is_err());

    let outside = fixture.path().join("outside_state.json");
    write(&outside, b"outside");
    write(
        &predecessor.join("vibeshine.conf"),
        format!("file_state = {}\n", outside.display()).as_bytes(),
    );
    assert!(collect_configuration_files(&predecessor).is_err());

    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = private/primary_state.json\n",
    );
    write(
        &predecessor.join("sunshine.conf"),
        b"file_state = private/auxiliary_state.json\n",
    );
    assert!(collect_configuration_files(&predecessor).is_err());
}

#[test]
fn classifies_custom_configured_primary_state_as_pairing_and_authentication_input() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibeshine");
    let private_password = "never expose this legacy verifier";
    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = private/host-record.bin\n",
    );
    write(
        &predecessor.join("private/host-record.bin"),
        serde_json::json!({
            "username": "Admin",
            "password": private_password,
            "salt": "salt",
            "root": { "uniqueid": "host-id" }
        })
        .to_string()
        .as_bytes(),
    );

    let legacy_source = source(LegacyProduct::Vibeshine, &predecessor);
    let plan = analyze_legacy_configuration(std::slice::from_ref(&legacy_source)).unwrap();

    assert!(
        plan.critical_items
            .iter()
            .any(|item| item.item == "pairing_identity_state")
    );
    assert!(
        plan.critical_items
            .iter()
            .any(|item| item.item == "legacy_administrator_credential")
    );
    assert!(
        !plan
            .unsupported_items
            .iter()
            .any(|item| item.item == "host-record.bin")
    );
    assert!(
        !serde_json::to_string(&plan)
            .unwrap()
            .contains(private_password)
    );
    assert!(preflight_legacy_configuration(std::slice::from_ref(&legacy_source)).is_err());

    let approved = preflight_legacy_configuration_with_sensitive_sources(
        &[legacy_source],
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Vibeshine,
            pairing: true,
            authentication: true,
        }],
    )
    .unwrap();
    assert!(approved.critical_items.is_empty());
}

#[test]
fn custom_configured_auxiliary_state_keeps_legacy_api_tokens_blocking() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibeshine");
    write(
        &predecessor.join("vibeshine.conf"),
        b"vibeshine_file_state = private/web-record.bin\n",
    );
    write(
        &predecessor.join("private/web-record.bin"),
        br#"{"root":{"api_tokens":[{"token":"must-not-be-imported"}]}}"#,
    );

    let legacy_source = source(LegacyProduct::Vibeshine, &predecessor);
    let plan = analyze_legacy_configuration(std::slice::from_ref(&legacy_source)).unwrap();

    assert!(
        plan.critical_items
            .iter()
            .any(|item| item.item == "pairing_identity_state")
    );
    assert!(
        plan.critical_items
            .iter()
            .any(|item| item.item == "legacy_api_tokens")
    );
    assert!(
        !plan
            .unsupported_items
            .iter()
            .any(|item| item.item == "web-record.bin")
    );
    assert!(
        preflight_legacy_configuration_with_sensitive_sources(
            &[legacy_source],
            &[ValidatedSensitiveSource {
                product: LegacyProduct::Vibeshine,
                pairing: true,
                authentication: true,
            }],
        )
        .is_err()
    );
}

#[test]
fn configured_conf_target_retains_listener_critical_setting_after_pairing_approval() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibeshine");
    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = private/listener.conf\n",
    );
    write(
        &predecessor.join("private/listener.conf"),
        b"port = 47990\n",
    );

    let plan =
        analyze_legacy_configuration(&[source(LegacyProduct::Vibeshine, &predecessor)]).unwrap();
    assert!(
        plan.critical_items
            .iter()
            .any(|item| item.item == "pairing_identity_state")
    );
    assert!(plan.critical_items.iter().any(|item| item.item == "port"));
    assert!(
        preflight_legacy_configuration_with_sensitive_sources(
            &[source(LegacyProduct::Vibeshine, &predecessor)],
            &[ValidatedSensitiveSource {
                product: LegacyProduct::Vibeshine,
                pairing: true,
                authentication: false,
            }],
        )
        .is_err()
    );
}

#[test]
fn configured_apps_json_target_still_imports_recognized_applications() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibeshine");
    write(
        &predecessor.join("vibeshine.conf"),
        b"file_state = private/apps.json\n",
    );
    write(
        &predecessor.join("private/apps.json"),
        br#"[{"name":"Example","cmd":"C:\\Games\\Example.exe"}]"#,
    );

    let plan =
        analyze_legacy_configuration(&[source(LegacyProduct::Vibeshine, &predecessor)]).unwrap();

    assert_eq!(plan.applications.len(), 1);
    assert_eq!(plan.applications[0].name, "Example");
    assert_eq!(plan.applications[0].executable, "C:\\Games\\Example.exe");
    assert!(
        plan.critical_items
            .iter()
            .any(|item| item.item == "pairing_identity_state")
    );
}

#[cfg(unix)]
#[test]
fn rejects_configured_state_symlinks_that_escape_the_predecessor_root() {
    use std::os::unix::fs::symlink;

    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    let outside = fixture.path().join("outside_state.json");
    write(&outside, b"outside");
    write(
        &predecessor.join("sunshine.conf"),
        b"file_state = state_link.json\n",
    );
    symlink(&outside, predecessor.join("state_link.json")).unwrap();

    assert!(collect_configuration_files(&predecessor).is_err());

    let outside_directory = fixture.path().join("outside_directory");
    std::fs::create_dir_all(&outside_directory).unwrap();
    symlink(&outside_directory, predecessor.join("external")).unwrap();
    write(
        &predecessor.join("sunshine.conf"),
        b"vibeshine_file_state = external/not_created_yet.json\n",
    );
    assert!(collect_configuration_files(&predecessor).is_err());
}

#[cfg(unix)]
#[test]
fn snapshots_contained_configured_state_symlink_targets_under_the_configured_alias() {
    use std::os::unix::fs::symlink;

    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        b"file_state = credentials/state-alias.json\n",
    );
    write(&predecessor.join("private/state.json"), br#"{"root":{}}"#);
    fs::create_dir_all(predecessor.join("credentials")).unwrap();
    symlink(
        "../private/state.json",
        predecessor.join("credentials/state-alias.json"),
    )
    .unwrap();

    let destination = fixture.path().join("Amaterasu");
    import_legacy_configuration_with_sensitive_sources(
        &destination,
        &[source(LegacyProduct::Sunshine, &predecessor)],
        &[ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: true,
            authentication: false,
        }],
    )
    .unwrap();

    assert_eq!(
        fs::read(
            destination
                .join(LEGACY_IMPORT_DIRECTORY)
                .join("sunshine/credentials/state-alias.json")
        )
        .unwrap(),
        br#"{"root":{}}"#
    );
}

#[test]
fn plan_maps_safe_settings_and_exact_executable_applications() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibeshine");
    write(
        &predecessor.join("vibeshine.conf"),
        b"encoder = nvenc\nkeyboard = enabled\nunknown_setting = ignored\n",
    );
    write(
        &predecessor.join("apps.json"),
        br#"[{"name":"Example","cmd":"C:\\Games\\Example.exe","working-dir":"C:\\Games"},{"name":"Review","cmd":"C:\\Games\\Example.exe --unsafe"}]"#,
    );

    let plan =
        preflight_legacy_configuration(&[source(LegacyProduct::Vibeshine, &predecessor)]).unwrap();

    assert_eq!(plan.settings["video.encoder_preference"], "nvenc");
    assert_eq!(plan.settings["input.keyboard_enabled"], "enabled");
    assert_eq!(plan.applications.len(), 1);
    assert_eq!(plan.applications[0].executable, "C:\\Games\\Example.exe");
    assert!(
        plan.unsupported_items
            .iter()
            .any(|item| item.item == "application_command_requires_review")
    );
}

#[test]
fn plan_strips_unquoted_inline_setting_comments() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    write(
        &predecessor.join("sunshine.conf"),
        b"fps = 60 # the default frame rate\nencoder = \"nvenc#quality\" # retain quoted hash\n",
    );

    let plan =
        analyze_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).unwrap();

    assert_eq!(plan.settings["stream.default_fps"], "60");
    assert_eq!(
        plan.settings["video.encoder_preference"],
        "\"nvenc#quality\""
    );
}

#[test]
fn repeat_import_is_idempotent_and_merges_products() {
    let fixture = TemporaryDirectory::new();
    let sunshine = fixture.path().join("Sunshine");
    let vibeshine = fixture.path().join("Vibeshine");
    write(&sunshine.join("apps.json"), b"[sunshine]");
    write(&vibeshine.join("apps.json"), b"[vibeshine]");
    let data_root = fixture.path().join("Amaterasu");

    let initial = import_legacy_configuration(
        &data_root,
        &[
            source(LegacyProduct::Sunshine, &sunshine),
            source(LegacyProduct::Vibeshine, &vibeshine),
        ],
    )
    .unwrap();
    let repeated =
        import_legacy_configuration(&data_root, &[source(LegacyProduct::Sunshine, &sunshine)])
            .unwrap();

    assert_eq!(initial.imported_files.len(), 2);
    assert!(repeated.imported_files.is_empty());
    assert_eq!(
        fs::read(
            data_root
                .join(LEGACY_IMPORT_DIRECTORY)
                .join("sunshine/apps.json")
        )
        .unwrap(),
        b"[sunshine]"
    );
    assert_eq!(
        fs::read(
            data_root
                .join(LEGACY_IMPORT_DIRECTORY)
                .join("vibeshine/apps.json")
        )
        .unwrap(),
        b"[vibeshine]"
    );
}

#[test]
fn conflicting_existing_import_fails_before_any_target_changes() {
    let fixture = TemporaryDirectory::new();
    let sunshine = fixture.path().join("Sunshine");
    let apollo = fixture.path().join("Apollo");
    write(&sunshine.join("apps.json"), b"new sunshine");
    write(&apollo.join("apps.json"), b"new apollo");
    let data_root = fixture.path().join("Amaterasu");
    write(
        &data_root
            .join(LEGACY_IMPORT_DIRECTORY)
            .join("sunshine/apps.json"),
        b"old sunshine",
    );

    let error = import_legacy_configuration(
        &data_root,
        &[
            source(LegacyProduct::Apollo, &apollo),
            source(LegacyProduct::Sunshine, &sunshine),
        ],
    )
    .unwrap_err();

    assert!(error.contains("conflicts"));
    assert!(
        !data_root
            .join(LEGACY_IMPORT_DIRECTORY)
            .join("apollo/apps.json")
            .exists()
    );
    assert_eq!(
        fs::read(
            data_root
                .join(LEGACY_IMPORT_DIRECTORY)
                .join("sunshine/apps.json")
        )
        .unwrap(),
        b"old sunshine"
    );
}

#[test]
fn rollback_removes_only_files_created_by_its_transaction() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Vibepollo");
    write(&predecessor.join("vibepollo.conf"), b"quality = high");
    let data_root = fixture.path().join("Amaterasu");

    let receipt = import_legacy_configuration(
        &data_root,
        &[source(LegacyProduct::Vibepollo, &predecessor)],
    )
    .unwrap();
    let imported = data_root
        .join(LEGACY_IMPORT_DIRECTORY)
        .join("vibepollo/vibepollo.conf");
    rollback_legacy_configuration(&receipt).unwrap();

    assert!(!imported.exists());
    assert!(
        receipt
            .backup_directory
            .join("source/vibepollo/0/vibepollo.conf")
            .exists()
    );
}

#[test]
fn rollback_preserves_a_file_changed_after_import() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Apollo");
    write(&predecessor.join("apollo.conf"), b"old value");
    let data_root = fixture.path().join("Amaterasu");

    let receipt =
        import_legacy_configuration(&data_root, &[source(LegacyProduct::Apollo, &predecessor)])
            .unwrap();
    let imported = data_root
        .join(LEGACY_IMPORT_DIRECTORY)
        .join("apollo/apollo.conf");
    fs::write(&imported, b"changed after import").unwrap();

    assert!(rollback_legacy_configuration(&receipt).is_err());
    assert_eq!(fs::read(imported).unwrap(), b"changed after import");
}
