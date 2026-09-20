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
fn preflight_refuses_sensitive_configuration_without_exposing_its_contents() {
    let fixture = TemporaryDirectory::new();
    let predecessor = fixture.path().join("Sunshine");
    let secret = b"pairing-private-key: never print this";
    write(&predecessor.join("sunshine.conf"), b"port = 47990");
    write(&predecessor.join("credentials/client.pem"), secret);
    write(
        &predecessor.join("credentials/nested/token"),
        b"other secret",
    );
    let data_root = fixture.path().join("Amaterasu");

    let plan =
        analyze_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).unwrap();

    assert!(!plan.critical_items.is_empty());
    let encoded = serde_json::to_string(&plan).unwrap();
    assert!(!encoded.contains("pairing-private-key"));
    assert!(
        preflight_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).is_err()
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
        b"pkey = private/key.pem\ncert = private/cert.pem\n",
    );
    write(&predecessor.join("private/key.pem"), b"key");
    write(&predecessor.join("private/cert.pem"), b"cert");
    let legacy_source = source(LegacyProduct::Sunshine, &predecessor);
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
            .join("sunshine/private/key.pem")
            .is_file()
    );

    write(
        &predecessor.join("sunshine.conf"),
        b"pkey = ../outside.pem\ncert = private/cert.pem\n",
    );
    assert!(
        analyze_legacy_configuration(&[source(LegacyProduct::Sunshine, &predecessor)]).is_err()
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
