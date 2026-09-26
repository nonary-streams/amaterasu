use amaterasu_package::handoff::{
    self, HandoffRecord, LegacyFamily, LegacyService, MsiRegistration, RegistrationReader,
    RegistrationState, ServiceController, ServiceSnapshot, StartMode,
};
use amaterasu_package::importer::{materialize_import_plan, runtime_state_directory};
use amaterasu_package::migration::{
    LegacyConfigSource, LegacyProduct, LegacySearchPaths, MigrationReceipt,
    ValidatedSensitiveSource,
    analyze_legacy_configuration, canonical_existing_source_root, discover_legacy_sources,
    import_legacy_configuration_with_sensitive_sources,
    preflight_legacy_configuration_with_sensitive_sources, rollback_legacy_configuration,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HANDOFF_DIRECTORY: &str = "installer-handoff";
const HANDOFF_FILE: &str = "predecessor-handoff.json";
const BUNDLE_COORDINATION_FILE: &str = "bundle-coordination.json";
const IMPORT_VALIDATION_FILE: &str = "installer-import-validated.json";

fn main() {
    if let Err(error) = run() {
        if error == "predecessor_bundle_required" {
            eprintln!(
                "amaterasu-migration: predecessor_bundle_required: install with Amaterasu Setup.exe to migrate and retire the predecessor safely"
            );
        } else {
            eprintln!("amaterasu-migration: {error}");
        }
        std::process::exit(if error == "predecessor_bundle_required" {
            42
        } else {
            1
        });
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().ok_or_else(|| usage().to_owned())?;

    let mut destination = None;
    let mut output = None;
    let mut runtime = None;
    let mut install_folder = None;
    let mut data_root = None;
    let mut nonce = None;
    let mut program_files = None;
    let mut sources = Vec::new();
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--destination" => {
                let value = arguments.next().ok_or("--destination requires a path")?;
                if destination.replace(PathBuf::from(value)).is_some() {
                    return Err("--destination may be supplied only once".into());
                }
            }
            "--output" => {
                let value = arguments.next().ok_or("--output requires a path")?;
                if output.replace(PathBuf::from(value)).is_some() {
                    return Err("--output may be supplied only once".into());
                }
            }
            "--runtime" => {
                let value = arguments.next().ok_or("--runtime requires a path")?;
                if runtime.replace(PathBuf::from(value)).is_some() {
                    return Err("--runtime may be supplied only once".into());
                }
            }
            "--install-folder" => {
                let value = arguments.next().ok_or("--install-folder requires a path")?;
                if install_folder.replace(PathBuf::from(value)).is_some() {
                    return Err("--install-folder may be supplied only once".into());
                }
            }
            "--data-root" => {
                let value = arguments.next().ok_or("--data-root requires a path")?;
                if data_root.replace(PathBuf::from(value)).is_some() {
                    return Err("--data-root may be supplied only once".into());
                }
            }
            "--nonce" => {
                let value = arguments.next().ok_or("--nonce requires a token")?;
                if nonce.replace(value).is_some() {
                    return Err("--nonce may be supplied only once".into());
                }
            }
            "--program-files" => {
                let value = arguments.next().ok_or("--program-files requires a path")?;
                if program_files.replace(PathBuf::from(value)).is_some() {
                    return Err("--program-files may be supplied only once".into());
                }
            }
            "--source" => {
                let value = arguments.next().ok_or("--source requires PRODUCT=PATH")?;
                sources.push(parse_source(&value)?);
            }
            _ => return Err(usage().into()),
        }
    }

    let discovered_only = sources.is_empty() && program_files.is_some();
    if let Some(program_files) = &program_files {
        sources.extend(discover_program_files_sources(program_files)?);
    }
    deduplicate_sources(&mut sources)?;

    match command.as_str() {
        "preflight" => {
            if destination.is_some()
                || output.is_some()
                || runtime.is_some()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("preflight accepts only --source and --program-files".into());
            }
            if !sources.is_empty() {
                let validation = validate_sensitive_sources(&sources)?;
                preflight_legacy_configuration_with_sensitive_sources(
                    &sources,
                    &validation.approvals,
                )?;
            } else if !discovered_only {
                return Err("preflight requires at least one --source or --program-files".into());
            }
            println!("migration preflight completed");
            Ok(())
        }
        "plan" => {
            if destination.is_some()
                || runtime.is_some()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("plan accepts only --output, --source, and --program-files".into());
            }
            let output = output.ok_or("plan requires --output")?;
            let plan = if sources.is_empty() && discovered_only {
                empty_plan()
            } else {
                analyze_legacy_configuration(&sources)?
            };
            amaterasu_package::write_json_atomically(&output, &plan)?;
            println!("migration plan written");
            Ok(())
        }
        "import" => {
            if output.is_some() || install_folder.is_some() || data_root.is_some() {
                return Err("import does not accept --output".into());
            }
            let destination = destination.ok_or("--destination is required")?;
            // A previous successful attempt cannot authorize handoff after this
            // attempt fails validation, materialization, or sensitive seeding.
            if let Some(runtime) = &runtime {
                invalidate_import_validation(runtime)?;
            }
            if sources.is_empty() && discovered_only {
                println!("migration completed: no predecessor configuration found");
                return Ok(());
            }
            let validation = validate_sensitive_sources(&sources)?;
            if validation
                .approvals
                .iter()
                .any(|source| source.pairing || source.authentication)
                && runtime.is_none()
            {
                return Err("sensitive legacy migration requires --runtime".into());
            }
            let receipt = import_legacy_configuration_with_sensitive_sources(
                &destination,
                &sources,
                &validation.approvals,
            )?;
            if let Some(runtime) = runtime {
                let materialized = finish_import(&receipt, || {
                    let materialized = materialize_import_plan(&runtime, &receipt.plan)?;
                    let state = runtime_state_directory(&runtime)?;
                    seed_sensitive_sources(&destination, &validation, &state)?;
                    write_import_validation(&runtime, &sources)?;
                    Ok(materialized)
                })?;
                println!(
                    "migration runtime state materialized: {} application(s), {} setting(s)",
                    materialized.applications, materialized.settings
                );
            }
            println!(
                "migration completed: {} file(s) imported",
                receipt.imported_files.len()
            );
            Ok(())
        }
        "handoff-predecessor" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("handoff-predecessor accepts only --runtime".into());
            }
            let runtime = runtime.ok_or("handoff-predecessor requires --runtime")?;
            handoff_predecessor(&runtime)?;
            println!("predecessor handoff completed");
            Ok(())
        }
        "rollback-predecessor-handoff" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("rollback-predecessor-handoff accepts only --runtime".into());
            }
            let runtime = runtime.ok_or("rollback-predecessor-handoff requires --runtime")?;
            rollback_predecessor_handoff(&runtime)?;
            println!("predecessor handoff rollback completed");
            Ok(())
        }
        "finalize-predecessor-handoff" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("finalize-predecessor-handoff accepts only --runtime".into());
            }
            let runtime = runtime.ok_or("finalize-predecessor-handoff requires --runtime")?;
            finalize_predecessor_handoff(&runtime)?;
            println!("predecessor handoff finalized");
            Ok(())
        }
        "retire-predecessor" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("retire-predecessor accepts only --runtime".into());
            }
            let runtime = runtime.ok_or("retire-predecessor requires --runtime")?;
            retire_predecessor(&runtime)?;
            println!("predecessor retirement completed");
            Ok(())
        }
        "require-no-predecessor" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("require-no-predecessor accepts only --runtime".into());
            }
            let runtime = runtime.ok_or("require-no-predecessor requires --runtime")?;
            let nonce = nonce.ok_or("require-no-predecessor requires --nonce")?;
            require_no_predecessor(&runtime, &nonce)?;
            println!("no supported predecessor registration found");
            Ok(())
        }
        "prepare-bundle-upgrade" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
                || install_folder.is_some()
                || data_root.is_some()
            {
                return Err("prepare-bundle-upgrade accepts only --runtime".into());
            }
            let runtime = runtime.ok_or("prepare-bundle-upgrade requires --runtime")?;
            let nonce = nonce.ok_or("prepare-bundle-upgrade requires --nonce")?;
            prepare_bundle_upgrade(&runtime, &nonce)?;
            println!("bundle upgrade coordination prepared");
            Ok(())
        }
        "cancel-bundle-upgrade" => {
            if destination.is_some()
                || output.is_some()
                || program_files.is_some()
                || !sources.is_empty()
            {
                return Err("cancel-bundle-upgrade accepts only --runtime and --nonce".into());
            }
            let runtime = runtime.ok_or("cancel-bundle-upgrade requires --runtime")?;
            let nonce = nonce.ok_or("cancel-bundle-upgrade requires --nonce")?;
            cancel_bundle_upgrade(&runtime, &nonce)?;
            println!("bundle upgrade coordination cancelled");
            Ok(())
        }
        "validate-installer-paths" => {
            if destination.is_some()
                || output.is_some()
                || runtime.is_some()
                || program_files.is_some()
                || !sources.is_empty()
            {
                return Err(
                    "validate-installer-paths accepts only --install-folder and --data-root".into(),
                );
            }
            validate_installer_paths(
                &install_folder.ok_or("validate-installer-paths requires --install-folder")?,
                &data_root.ok_or("validate-installer-paths requires --data-root")?,
            )?;
            println!("installer paths validated");
            Ok(())
        }
        _ => Err(usage().into()),
    }
}

/// A failed runtime handoff must leave the copied import retryable. Namespace
/// seeds are idempotent and may have completed before the failure; the receipt
/// removes only files this attempt created and keeps its source backup.
fn finish_import<T>(
    receipt: &MigrationReceipt,
    handoff: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match handoff() {
        Ok(result) => Ok(result),
        Err(error) => {
            rollback_legacy_configuration(receipt).map_err(|rollback| {
                format!("{error}; imported files could not be rolled back: {rollback}")
            })?;
            Err(error)
        }
    }
}

fn validate_sensitive_sources(
    sources: &[LegacyConfigSource],
) -> Result<SensitiveValidation, String> {
    let mut checks = Vec::new();
    for source in sources {
        let (needs_pairing, needs_authentication) = sensitive_needs(source)?;
        checks.push(SourceValidation {
            product: source.product,
            pairing_needed: needs_pairing,
            authentication_needed: needs_authentication,
            pairing_valid: !needs_pairing
                || helper_succeeds(
                    &helper_path("amaterasu-pairing-import")?,
                    "validate",
                    &source.root,
                    None,
                )?,
            authentication_valid: !needs_authentication
                || helper_succeeds(
                    &helper_path("amaterasu-authentication-import")?,
                    "validate",
                    &source.root,
                    None,
                )?,
        });
    }
    let approvals = aggregate_sensitive_approvals(&checks);
    Ok(SensitiveValidation { checks, approvals })
}

struct SensitiveValidation {
    checks: Vec<SourceValidation>,
    approvals: Vec<ValidatedSensitiveSource>,
}

struct SourceValidation {
    product: LegacyProduct,
    pairing_needed: bool,
    authentication_needed: bool,
    pairing_valid: bool,
    authentication_valid: bool,
}

fn aggregate_sensitive_approvals(checks: &[SourceValidation]) -> Vec<ValidatedSensitiveSource> {
    let mut by_product =
        std::collections::BTreeMap::<LegacyProduct, (usize, usize, bool, bool)>::new();
    for check in checks {
        let entry = by_product
            .entry(check.product)
            .or_insert((0, 0, true, true));
        if check.pairing_needed {
            entry.0 += 1;
            entry.2 &= check.pairing_valid;
        }
        if check.authentication_needed {
            entry.1 += 1;
            entry.3 &= check.authentication_valid;
        }
    }
    by_product
        .into_iter()
        .map(
            |(
                product,
                (pairing_count, authentication_count, pairing_valid, authentication_valid),
            )| ValidatedSensitiveSource {
                product,
                // A single destination identity/verifier cannot preserve two distinct
                // sensitive sources, even if both helpers validate independently.
                pairing: pairing_count == 1 && pairing_valid,
                authentication: authentication_count == 1 && authentication_valid,
            },
        )
        .collect()
}

fn sensitive_needs(source: &LegacyConfigSource) -> Result<(bool, bool), String> {
    let plan = analyze_legacy_configuration(std::slice::from_ref(source))?;
    let pairing = plan.critical_items.iter().any(|item| {
        matches!(
            item.item.as_str(),
            "pairing_identity_state" | "pairing_credentials" | "pkey" | "cert"
        )
    });
    let authentication = plan.critical_items.iter().any(|item| {
        matches!(
            item.item.as_str(),
            "credentials_file" | "legacy_administrator_credential"
        )
    });
    Ok((pairing, authentication))
}

fn seed_sensitive_sources(
    destination: &std::path::Path,
    validation: &SensitiveValidation,
    state: &std::path::Path,
) -> Result<(), String> {
    seed_sensitive_sources_with(destination, validation, state, |helper, source, state| {
        helper_succeeds(&helper_path(helper)?, "seed", source, Some(state))
    })
}

fn seed_sensitive_sources_with(
    destination: &std::path::Path,
    validation: &SensitiveValidation,
    state: &std::path::Path,
    mut seed: impl FnMut(&str, &std::path::Path, &std::path::Path) -> Result<bool, String>,
) -> Result<(), String> {
    // The live predecessor can change after the approved copy. Requirements
    // belong to the validation decision, while helpers inspect the copied data.
    for check in &validation.checks {
        let validated = validation
            .approvals
            .iter()
            .find(|item| item.product == check.product)
            .ok_or("migration approval is missing")?;
        let copied = destination
            .join(amaterasu_package::migration::LEGACY_IMPORT_DIRECTORY)
            .join(check.product.directory_name());
        if check.pairing_needed
            && validated.pairing
            && !seed("amaterasu-pairing-import", &copied, state)?
        {
            return Err("validated pairing migration could not be committed".into());
        }
        if check.authentication_needed
            && validated.authentication
            && !seed("amaterasu-authentication-import", &copied, state)?
        {
            return Err("validated authentication migration could not be committed".into());
        }
    }
    Ok(())
}

fn helper_path(name: &str) -> Result<PathBuf, String> {
    let mut path =
        std::env::current_exe().map_err(|_| "migration helper location is unavailable")?;
    path.set_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        Ok(path)
    } else {
        Err("required migration helper is not installed".into())
    }
}

fn helper_succeeds(
    helper: &std::path::Path,
    command: &str,
    source: &std::path::Path,
    state: Option<&std::path::Path>,
) -> Result<bool, String> {
    let mut process = Command::new(helper);
    process.arg(command).arg("--source").arg(source);
    if let Some(state) = state {
        process.arg("--state").arg(state);
    }
    let status = process
        .status()
        .map_err(|_| "migration helper could not be started")?;
    Ok(status.success())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ImportValidation {
    schema: u32,
    validated: bool,
    families: Vec<LegacyFamily>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HandoffState {
    schema: u32,
    records: Vec<HandoffRecord>,
    registrations: Vec<MsiRegistration>,
    retirement_started: bool,
    retired: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleCoordination {
    schema: u32,
    registrations: Vec<MsiRegistration>,
    nonce_hash: String,
    created_at: u64,
    expires_at: u64,
}

fn write_import_validation(
    runtime: &std::path::Path,
    sources: &[LegacyConfigSource],
) -> Result<(), String> {
    let mut families: Vec<_> = sources
        .iter()
        .map(|source| legacy_family(source.product))
        .collect();
    families.sort();
    families.dedup();
    amaterasu_package::write_json_atomically(
        &runtime.join(IMPORT_VALIDATION_FILE),
        &ImportValidation {
            schema: 2,
            validated: true,
            families,
        },
    )
}

fn invalidate_import_validation(runtime: &std::path::Path) -> Result<(), String> {
    match fs::remove_file(runtime.join(IMPORT_VALIDATION_FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("prior import validation could not be invalidated".into()),
    }
}

fn legacy_family(product: LegacyProduct) -> LegacyFamily {
    match product {
        LegacyProduct::Apollo => LegacyFamily::Apollo,
        LegacyProduct::Sunshine => LegacyFamily::Sunshine,
        LegacyProduct::Vibeshine => LegacyFamily::Vibeshine,
        LegacyProduct::Vibepollo => LegacyFamily::Vibepollo,
    }
}

fn handoff_path(runtime: &std::path::Path) -> Result<PathBuf, String> {
    let root = runtime
        .canonicalize()
        .map_err(|_| "runtime root is unavailable")?;
    let directory = root.join(HANDOFF_DIRECTORY);
    create_system_private_directory(&directory)?;
    let path = directory.join(HANDOFF_FILE);
    if !path.starts_with(&root) {
        return Err("handoff path is invalid".into());
    }
    Ok(path)
}

/// The coordination and recovery records are consumed by elevated package
/// actions only. Keep them separate from user-readable runtime payloads: an
/// interactive worker never needs these records and must not be able to forge
/// a Bundle authorization marker.
fn create_system_private_directory(path: &std::path::Path) -> Result<(), String> {
    amaterasu_package::create_private_directory(path)?;
    #[cfg(windows)]
    {
        if cfg!(test) {
            // Synthetic fixtures run as an unprivileged developer account;
            // production entry points never take this branch.
            return Ok(());
        }
        let output = Command::new(r"C:\Windows\System32\icacls.exe")
            .arg(path)
            .args([
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
            ])
            .output()
            .map_err(|_| "system-private directory ACL tool could not start")?;
        if !output.status.success() {
            return Err("system-private directory ACL could not be applied".into());
        }
        let inspected = Command::new(r"C:\Windows\System32\icacls.exe")
            .arg(path)
            .output()
            .map_err(|_| "system-private directory ACL could not be verified")?;
        let text = String::from_utf8_lossy(&inspected.stdout);
        if !inspected.status.success()
            || !(text.contains("S-1-5-18") || text.contains("SYSTEM"))
            || !(text.contains("S-1-5-32-544") || text.contains("Administrators"))
        {
            return Err("system-private directory ACL verification failed".into());
        }
    }
    Ok(())
}

fn coordination_path(runtime: &std::path::Path) -> Result<PathBuf, String> {
    Ok(handoff_path(runtime)?
        .parent()
        .ok_or("handoff directory is invalid")?
        .join(BUNDLE_COORDINATION_FILE))
}

fn handoff_predecessor(runtime: &std::path::Path) -> Result<(), String> {
    let path = handoff_path(runtime)?;
    if path.try_exists().map_err(|error| error.to_string())? {
        return Err("predecessor handoff already exists; use rollback first".into());
    }
    let mut registry = platform_registration_reader()?;
    let registrations = handoff::capture_msi_registrations(
        &mut registry,
        [
            LegacyFamily::Apollo,
            LegacyFamily::Sunshine,
            LegacyFamily::Vibeshine,
            LegacyFamily::Vibepollo,
        ],
    )?;
    if registrations.is_empty() {
        return Ok(());
    }
    if registrations.len() > 1 {
        return Err(
            "multiple predecessor registrations are unsupported for one retirement transaction"
                .into(),
        );
    }
    require_import_validation(runtime, &registrations)?;
    let mut controller = platform_service_controller()?;
    let records = handoff::preflight(
        &mut controller,
        registrations.iter().map(|registration| registration.family),
    )?;
    let mut state = HandoffState {
        schema: 2,
        records,
        registrations,
        retirement_started: false,
        retired: false,
    };
    // The complete recovery snapshot reaches disk before the first SCM stop.
    // Each stop intent and completion is then atomically recorded by the
    // handoff library, so a crash or a failed write is recoverable.
    persist_handoff_state(&path, &state)?;
    let registrations = state.registrations.clone();
    let mut persist = |records: &[HandoffRecord]| {
        persist_handoff_state(
            &path,
            &HandoffState {
                schema: 2,
                records: records.to_vec(),
                registrations: registrations.clone(),
                retirement_started: false,
                retired: false,
            },
        )
    };
    handoff::stop_with_recovery_state(&mut controller, &mut state.records, true, &mut persist)
}

fn require_import_validation(
    runtime: &std::path::Path,
    registrations: &[MsiRegistration],
) -> Result<(), String> {
    let validation: ImportValidation =
        amaterasu_package::read_json(&runtime.join(IMPORT_VALIDATION_FILE))?;
    if validation.schema != 2
        || !validation.validated
        || validation.families.len() > 4
        || validation.families.iter().any(|family| {
            !registrations
                .iter()
                .any(|registration| registration.family == *family)
        })
        || !registrations
            .iter()
            .all(|registration| validation.families.contains(&registration.family))
    {
        return Err(
            "legacy import validation is missing or does not match the detected predecessor".into(),
        );
    }
    Ok(())
}

fn persist_handoff_state(path: &std::path::Path, state: &HandoffState) -> Result<(), String> {
    amaterasu_package::write_json_atomically(path, state)
}

fn rollback_predecessor_handoff(runtime: &std::path::Path) -> Result<(), String> {
    let path = handoff_path(runtime)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let mut state = read_handoff_state(&path)?;
    let mut controller = platform_service_controller()?;
    handoff::rollback(&mut controller, &mut state.records)?;
    fs::remove_file(path).map_err(|error| error.to_string())
}

fn finalize_predecessor_handoff(runtime: &std::path::Path) -> Result<(), String> {
    let path = handoff_path(runtime)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let _state = read_handoff_state(&path)?;
    fs::remove_file(&path).map_err(|error| error.to_string())?;
    remove_bundle_coordination(runtime)?;
    match fs::remove_dir(path.parent().ok_or("handoff directory is invalid")?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn retire_predecessor(runtime: &std::path::Path) -> Result<(), String> {
    let path = handoff_path(runtime)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        // A coordinated fresh install has no recovery state. Its one-shot
        // authorization marker is still consumed at the bundle's final step.
        remove_bundle_coordination(runtime)?;
        return Ok(());
    }
    let mut state = read_handoff_state(&path)?;
    let mut registry = platform_registration_reader()?;
    let mut controller = platform_service_controller()?;
    if state.retired {
        let _ = finalize_predecessor_handoff(runtime);
        let _ = remove_bundle_coordination(runtime);
        return Ok(());
    }
    let registration = state
        .registrations
        .first()
        .ok_or("handoff state has no predecessor registration")?;
    if state.retirement_started {
        // Once an MSI uninstall has been attempted, do not restart the saved
        // service from a later invocation. Absence proves the irreversible
        // action completed; presence is a failed retirement that still needs
        // recovery.
        if matches!(
            registry.registration(registration.family)?,
            RegistrationState::Absent
        ) {
            state.retired = true;
            let _ = persist_handoff_state(&path, &state);
            let _ = finalize_predecessor_handoff(runtime);
            let _ = remove_bundle_coordination(runtime);
            return Ok(());
        }
        let mut records = state.records.clone();
        let rollback = handoff::rollback(&mut controller, &mut records);
        return match rollback {
            Ok(()) => Err("predecessor retirement did not complete".into()),
            Err(error) => Err(format!(
                "predecessor retirement did not complete; predecessor recovery also failed: {error}"
            )),
        };
    }
    if !matches!(registry.registration(registration.family)?, RegistrationState::Msi(found) if found == *registration)
    {
        let mut records = state.records.clone();
        return match handoff::rollback(&mut controller, &mut records) {
            Ok(()) => Err("captured predecessor MSI registration changed".into()),
            Err(error) => Err(format!(
                "captured predecessor MSI registration changed; predecessor recovery also failed: {error}"
            )),
        };
    }
    state.retirement_started = true;
    if let Err(error) = persist_handoff_state(&path, &state) {
        let mut records = state.records.clone();
        return match handoff::rollback(&mut controller, &mut records) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(format!(
                "{error}; predecessor recovery also failed: {rollback_error}"
            )),
        };
    }
    if let Err(error) = uninstall_product(&registration.product_code) {
        let mut records = state.records.clone();
        return match handoff::rollback(&mut controller, &mut records) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(format!(
                "{error}; predecessor recovery also failed: {rollback_error}"
            )),
        };
    }
    if !matches!(
        registry.registration(registration.family)?,
        RegistrationState::Absent
    ) {
        let mut records = state.records.clone();
        return match handoff::rollback(&mut controller, &mut records) {
            Ok(()) => Err("predecessor MSI registration remains after uninstall".into()),
            Err(rollback_error) => Err(format!(
                "predecessor MSI registration remains after uninstall; predecessor recovery also failed: {rollback_error}"
            )),
        };
    }
    state.retired = true;
    // After a verified uninstall no failure may ask Burn to roll Amaterasu
    // back into the predecessor's listener. Leaving this strict state behind
    // is safe and a later invocation will only remove it.
    let _ = persist_handoff_state(&path, &state);
    let _ = finalize_predecessor_handoff(runtime);
    let _ = remove_bundle_coordination(runtime);
    Ok(())
}

fn prepare_bundle_upgrade(runtime: &std::path::Path, nonce: &str) -> Result<(), String> {
    validate_coordination_nonce(nonce)?;
    let mut registry = platform_registration_reader()?;
    let registrations = handoff::capture_msi_registrations(&mut registry, LegacyFamily::ALL)?;
    if registrations.len() > 1 {
        return Err(
            "multiple predecessor registrations are unsupported for one retirement transaction"
                .into(),
        );
    }
    let path = coordination_path(runtime)?;
    amaterasu_package::write_json_atomically(
        &path,
        &BundleCoordination {
            schema: 1,
            registrations,
            nonce_hash: amaterasu_package::sha256(nonce.as_bytes()),
            created_at: amaterasu_package::unix_timestamp(),
            expires_at: amaterasu_package::unix_timestamp().saturating_add(300),
        },
    )
}

fn require_no_predecessor(runtime: &std::path::Path, nonce: &str) -> Result<(), String> {
    let mut registry = platform_registration_reader()?;
    let registrations = handoff::capture_msi_registrations(&mut registry, LegacyFamily::ALL)?;
    if registrations.is_empty() {
        return Ok(());
    }
    if registrations.len() == 1
        && bundle_coordination_matches(runtime, &registrations, Some(nonce))?
    {
        remove_bundle_coordination(runtime)?;
        return Ok(());
    }
    Err("predecessor_bundle_required".into())
}

fn bundle_coordination_matches(
    runtime: &std::path::Path,
    registrations: &[MsiRegistration],
    supplied_nonce: Option<&str>,
) -> Result<bool, String> {
    let path = coordination_path(runtime)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(false);
    }
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| "bundle coordination marker is unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("bundle coordination marker is unsafe".into());
    }
    let marker: BundleCoordination = amaterasu_package::read_json(&path)?;
    Ok(marker.schema == 1
        && marker.registrations.len() <= 1
        && marker.created_at <= marker.expires_at
        && marker.expires_at >= amaterasu_package::unix_timestamp()
        && supplied_nonce
            .is_none_or(|nonce| amaterasu_package::sha256(nonce.as_bytes()) == marker.nonce_hash)
        && marker.nonce_hash.len() == 64
        && marker.registrations == registrations
        && marker.registrations.iter().all(|registration| {
            LegacyFamily::ALL.contains(&registration.family)
                && handoff::valid_product_code(&registration.product_code)
        }))
}

fn cancel_bundle_upgrade(runtime: &std::path::Path, nonce: &str) -> Result<(), String> {
    let path = coordination_path(runtime)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let marker: BundleCoordination = amaterasu_package::read_json(&path)?;
    if marker.schema != 1
        || marker.nonce_hash != amaterasu_package::sha256(nonce.as_bytes())
        || validate_coordination_nonce(nonce).is_err()
    {
        return Err("bundle coordination marker is invalid".into());
    }
    remove_bundle_coordination(runtime)
}

fn validate_coordination_nonce(nonce: &str) -> Result<(), String> {
    if nonce.is_empty() || nonce.len() > 1024 || nonce.chars().any(char::is_control) {
        return Err("bundle coordination nonce is invalid".into());
    }
    Ok(())
}

fn remove_bundle_coordination(runtime: &std::path::Path) -> Result<(), String> {
    let path = coordination_path(runtime)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| "bundle coordination marker is unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("bundle coordination marker is unsafe".into());
    }
    let marker: BundleCoordination = amaterasu_package::read_json(&path)?;
    if marker.schema != 1
        || marker.registrations.len() > 1
        || marker.created_at > marker.expires_at
        || marker.nonce_hash.len() != 64
        || marker.registrations.iter().any(|registration| {
            !LegacyFamily::ALL.contains(&registration.family)
                || !handoff::valid_product_code(&registration.product_code)
        })
    {
        return Err("bundle coordination marker is invalid".into());
    }
    fs::remove_file(path).map_err(|error| error.to_string())
}

/// Validates the two privileged roots before any deferred installer action.
/// Production roots are fixed Windows known locations; callers cannot replace
/// that policy with an environment variable or a legacy configuration value.
fn validate_installer_paths(
    install_folder: &std::path::Path,
    data_root: &std::path::Path,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let program_files = PathBuf::from(
            std::env::var_os("ProgramW6432").ok_or("Program Files root is unavailable")?,
        );
        let program_data = PathBuf::from(
            std::env::var_os("ProgramData").ok_or("ProgramData root is unavailable")?,
        );
        validate_installer_paths_against(install_folder, data_root, &program_files, &program_data)
    }
    #[cfg(not(windows))]
    {
        let _ = (install_folder, data_root);
        Err("installer path validation is supported only on Windows".into())
    }
}

fn validate_installer_paths_against(
    install_folder: &std::path::Path,
    data_root: &std::path::Path,
    program_files: &std::path::Path,
    program_data: &std::path::Path,
) -> Result<(), String> {
    let program_files = canonical_real_directory(program_files)?;
    let program_data = canonical_real_directory(program_data)?;
    validate_contained_install_path(install_folder, &program_files, "install folder")?;
    validate_contained_install_path(data_root, &program_data, "data root")
}

fn canonical_real_directory(path: &std::path::Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "installer root is unavailable")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("installer root is unsafe".into());
    }
    path.canonicalize()
        .map_err(|_| "installer root is unavailable".into())
}

fn validate_contained_install_path(
    candidate: &std::path::Path,
    root: &std::path::Path,
    description: &str,
) -> Result<(), String> {
    if !candidate.is_absolute() {
        return Err(format!("{description} must be absolute"));
    }
    let normalized = canonicalize_existing_prefix(&lexical_normalize(candidate)?)?;
    if !normalized.starts_with(root) || normalized == root {
        return Err(format!("{description} escapes its approved installer root"));
    }
    let mut current = root.to_path_buf();
    let suffix = normalized
        .strip_prefix(root)
        .map_err(|_| "installer path is invalid")?;
    for component in suffix.components() {
        current.push(component);
        if !current.try_exists().map_err(|error| error.to_string())? {
            break;
        }
        let metadata = fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "{description} contains a reparse point or non-directory"
            ));
        }
        if current.canonicalize().map_err(|error| error.to_string())? != current {
            return Err(format!("{description} canonicalization changed"));
        }
    }
    Ok(())
}

fn canonicalize_existing_prefix(path: &std::path::Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.try_exists().map_err(|error| error.to_string())? {
        let name = existing
            .file_name()
            .ok_or("installer path has no existing ancestor")?;
        suffix.push(name.to_os_string());
        existing = existing
            .parent()
            .ok_or("installer path has no existing ancestor")?;
    }
    let mut canonical = existing.canonicalize().map_err(|error| error.to_string())?;
    for name in suffix.iter().rev() {
        canonical.push(name);
    }
    Ok(canonical)
}

fn lexical_normalize(path: &std::path::Path) -> Result<PathBuf, String> {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("installer path escapes its root".into());
                }
            }
        }
    }
    Ok(normalized)
}

fn read_handoff_state(path: &std::path::Path) -> Result<HandoffState, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "handoff state is unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("handoff state is unsafe".into());
    }
    let state: HandoffState = amaterasu_package::read_json(path)?;
    if state.schema != 2
        || state.records.len() > 4
        || state.registrations.len() != 1
        || state.records.iter().any(|record| {
            !matches!(
                record.family,
                LegacyFamily::Apollo
                    | LegacyFamily::Sunshine
                    | LegacyFamily::Vibeshine
                    | LegacyFamily::Vibepollo
            )
        })
        || state
            .records
            .iter()
            .any(|record| record.family != state.registrations[0].family)
    {
        return Err("handoff state is invalid".into());
    }
    Ok(state)
}

#[cfg(windows)]
struct WindowsServiceController;
#[cfg(windows)]
impl ServiceController for WindowsServiceController {
    fn snapshot(&mut self, service: LegacyService) -> Result<ServiceSnapshot, String> {
        let output = sc_output(&["query", service.name()])?;
        if output.contains("FAILED 1060") {
            return Ok(ServiceSnapshot {
                exists: false,
                running: false,
                start_mode: StartMode::Demand,
            });
        }
        if !output.contains("STATE") {
            return Err("supported predecessor service could not be queried".into());
        }
        let config = sc_output(&["qc", service.name()])?;
        let start_mode = if config.contains("AUTO_START") {
            StartMode::Automatic
        } else if config.contains("DISABLED") {
            StartMode::Disabled
        } else {
            StartMode::Demand
        };
        Ok(ServiceSnapshot {
            exists: true,
            running: output.contains("RUNNING"),
            start_mode,
        })
    }
    fn stop(&mut self, service: LegacyService) -> Result<(), String> {
        sc_status(&["stop", service.name()])
    }
    fn start(&mut self, service: LegacyService) -> Result<(), String> {
        sc_status(&["start", service.name()])
    }
    fn set_start_mode(&mut self, service: LegacyService, mode: StartMode) -> Result<(), String> {
        let value = match mode {
            StartMode::Disabled => "disabled",
            StartMode::Demand => "demand",
            StartMode::Automatic => "auto",
        };
        sc_status(&["config", service.name(), "start=", value])
    }
}
#[cfg(windows)]
fn sc_output(arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(r"C:\Windows\System32\sc.exe")
        .args(arguments)
        .output()
        .map_err(|_| "SCM command could not start")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr))
}
#[cfg(windows)]
fn sc_status(arguments: &[&str]) -> Result<(), String> {
    let output = Command::new(r"C:\Windows\System32\sc.exe")
        .args(arguments)
        .output()
        .map_err(|_| "SCM command could not start")?;
    if output.status.success() {
        Ok(())
    } else {
        Err("SCM operation failed".into())
    }
}
#[cfg(windows)]
fn platform_service_controller() -> Result<WindowsServiceController, String> {
    Ok(WindowsServiceController)
}
#[cfg(windows)]
struct WindowsRegistrationReader;
#[cfg(windows)]
impl RegistrationReader for WindowsRegistrationReader {
    fn registration(&mut self, family: LegacyFamily) -> Result<RegistrationState, String> {
        // The fixed product display names are discovery inputs only. Product
        // codes come from Windows Installer's own uninstall registration, not
        // an MSI property or user-supplied path.
        let name = match family {
            LegacyFamily::Apollo => "Apollo",
            LegacyFamily::Sunshine => "Sunshine",
            LegacyFamily::Vibeshine => "Vibeshine",
            LegacyFamily::Vibepollo => "Vibepollo",
        };
        if let Some(product_code) = installed_msi_product_code(name)? {
            return Ok(RegistrationState::Msi(MsiRegistration {
                family,
                product_code,
            }));
        }
        let output = Command::new(r"C:\Windows\System32\reg.exe")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
                "/s",
                "/v",
                "DisplayName",
            ])
            .output()
            .map_err(|_| "uninstall registry query could not start")?;
        let text = String::from_utf8_lossy(&output.stdout);
        // A non-GUID registration is explicitly blocked rather than executing
        // its arbitrary UninstallString. MSI product-code enumeration remains
        // the only retirement path accepted here.
        if text.lines().any(|line| line.contains(name)) {
            return Ok(RegistrationState::ExeOnly);
        }
        Ok(RegistrationState::Absent)
    }
}
#[cfg(windows)]
fn installed_msi_product_code(expected_name: &str) -> Result<Option<String>, String> {
    const SUCCESS: u32 = 0;
    const MORE_DATA: u32 = 234;
    const NO_MORE_ITEMS: u32 = 259;
    #[link(name = "msi")]
    unsafe extern "system" {
        fn MsiEnumProductsW(index: u32, product: *mut u16) -> u32;
        fn MsiGetProductInfoW(
            product: *const u16,
            property: *const u16,
            value: *mut u16,
            value_size: *mut u32,
        ) -> u32;
    }
    let property: Vec<u16> = "InstalledProductName\0".encode_utf16().collect();
    for index in 0..1024 {
        let mut product = [0u16; 39];
        // SAFETY: `product` has the MSI-specified product-code capacity.
        let status = unsafe { MsiEnumProductsW(index, product.as_mut_ptr()) };
        if status == NO_MORE_ITEMS {
            return Ok(None);
        }
        if status != SUCCESS {
            return Err("Windows Installer product enumeration failed".into());
        }
        let mut value = vec![0u16; 256];
        let mut length = value.len() as u32;
        // SAFETY: all UTF-16 inputs are NUL-terminated and `value` is writable.
        let mut status = unsafe {
            MsiGetProductInfoW(
                product.as_ptr(),
                property.as_ptr(),
                value.as_mut_ptr(),
                &mut length,
            )
        };
        if status == MORE_DATA {
            value.resize(length as usize + 1, 0);
            length = value.len() as u32;
            status = unsafe {
                MsiGetProductInfoW(
                    product.as_ptr(),
                    property.as_ptr(),
                    value.as_mut_ptr(),
                    &mut length,
                )
            };
        }
        if status == SUCCESS && String::from_utf16_lossy(&value[..length as usize]) == expected_name
        {
            let end = product
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(product.len());
            return Ok(Some(String::from_utf16_lossy(&product[..end])));
        }
    }
    Err("Windows Installer product enumeration exceeded its bound".into())
}
#[cfg(windows)]
fn platform_registration_reader() -> Result<WindowsRegistrationReader, String> {
    Ok(WindowsRegistrationReader)
}
#[cfg(windows)]
fn uninstall_product(product_code: &str) -> Result<(), String> {
    if !handoff::valid_product_code(product_code) {
        return Err("captured predecessor product code is invalid".into());
    }
    let status = Command::new(r"C:\Windows\System32\msiexec.exe")
        .args(["/x", product_code, "/qn", "/norestart"])
        .status()
        .map_err(|_| "Windows Installer could not start")?;
    if status.code().is_some_and(|code| code == 0 || code == 3010) {
        Ok(())
    } else {
        Err("predecessor MSI uninstall failed".into())
    }
}
#[cfg(not(windows))]
struct UnsupportedServiceController;
#[cfg(not(windows))]
impl ServiceController for UnsupportedServiceController {
    fn snapshot(&mut self, _: LegacyService) -> Result<ServiceSnapshot, String> {
        Err("predecessor service handoff is supported only on Windows".into())
    }
    fn stop(&mut self, _: LegacyService) -> Result<(), String> {
        Err("predecessor service handoff is supported only on Windows".into())
    }
    fn start(&mut self, _: LegacyService) -> Result<(), String> {
        Err("predecessor service handoff is supported only on Windows".into())
    }
    fn set_start_mode(&mut self, _: LegacyService, _: StartMode) -> Result<(), String> {
        Err("predecessor service handoff is supported only on Windows".into())
    }
}
#[cfg(not(windows))]
fn platform_service_controller() -> Result<UnsupportedServiceController, String> {
    Err("predecessor service handoff is supported only on Windows".into())
}
#[cfg(not(windows))]
struct UnsupportedRegistrationReader;
#[cfg(not(windows))]
impl RegistrationReader for UnsupportedRegistrationReader {
    fn registration(&mut self, _: LegacyFamily) -> Result<RegistrationState, String> {
        Err("predecessor retirement is supported only on Windows".into())
    }
}
#[cfg(not(windows))]
fn platform_registration_reader() -> Result<UnsupportedRegistrationReader, String> {
    Err("predecessor retirement is supported only on Windows".into())
}
#[cfg(not(windows))]
fn uninstall_product(_: &str) -> Result<(), String> {
    Err("predecessor retirement is supported only on Windows".into())
}

fn discover_program_files_sources(
    program_files: &std::path::Path,
) -> Result<Vec<LegacyConfigSource>, String> {
    discover_legacy_sources(&LegacySearchPaths {
        profile_data_roots: Vec::new(),
        installation_roots: LegacyProduct::ALL
            .into_iter()
            .map(|product| LegacyConfigSource {
                product,
                root: program_files.join(product_directory(product)),
            })
            .collect(),
    })
}

fn product_directory(product: LegacyProduct) -> &'static str {
    match product {
        LegacyProduct::Apollo => "Apollo",
        LegacyProduct::Sunshine => "Sunshine",
        LegacyProduct::Vibeshine => "Vibeshine",
        LegacyProduct::Vibepollo => "Vibepollo",
    }
}

fn deduplicate_sources(sources: &mut Vec<LegacyConfigSource>) -> Result<(), String> {
    #[derive(Ord, PartialOrd, Eq, PartialEq)]
    enum SourceRootIdentity {
        Existing(PathBuf),
        Missing(PathBuf),
    }

    let mut seen = BTreeSet::new();
    let mut unique_sources = Vec::with_capacity(sources.len());
    for source in sources.iter() {
        let root_identity = match canonical_existing_source_root(&source.root)? {
            Some(root) => SourceRootIdentity::Existing(root),
            None => SourceRootIdentity::Missing(source.root.clone()),
        };
        if seen.insert((source.product, root_identity)) {
            unique_sources.push(source.clone());
        }
    }
    *sources = unique_sources;
    Ok(())
}

fn empty_plan() -> amaterasu_package::migration::ImportPlan {
    amaterasu_package::migration::ImportPlan {
        schema: 1,
        applications: Vec::new(),
        settings: Default::default(),
        unsupported_items: Vec::new(),
        critical_items: Vec::new(),
    }
}

fn parse_source(value: &str) -> Result<LegacyConfigSource, String> {
    let (product, path) = value
        .split_once('=')
        .ok_or("--source must be PRODUCT=PATH")?;
    if path.is_empty() {
        return Err("--source requires a non-empty path".into());
    }
    let product = LegacyProduct::parse(product)
        .ok_or("--source product must be Apollo, Sunshine, Vibeshine, or Vibepollo")?;
    Ok(LegacyConfigSource {
        product,
        root: PathBuf::from(path),
    })
}

fn usage() -> &'static str {
    "usage:\n  amaterasu-migration validate-installer-paths --install-folder PATH --data-root PATH\n  amaterasu-migration prepare-bundle-upgrade --runtime RUNTIME_ROOT\n  amaterasu-migration require-no-predecessor --runtime RUNTIME_ROOT\n  amaterasu-migration preflight (--source PRODUCT=PATH ... | --program-files PATH)\n  amaterasu-migration plan --output PLAN.json (--source PRODUCT=PATH ... | --program-files PATH)\n  amaterasu-migration import --destination DATA_ROOT [--runtime RUNTIME_ROOT] (--source PRODUCT=PATH ... | --program-files PATH)\n  amaterasu-migration handoff-predecessor --runtime RUNTIME_ROOT\n  amaterasu-migration rollback-predecessor-handoff --runtime RUNTIME_ROOT\n  amaterasu-migration finalize-predecessor-handoff --runtime RUNTIME_ROOT\n  amaterasu-migration retire-predecessor --runtime RUNTIME_ROOT"
}

#[cfg(test)]
mod handoff_cli_tests {
    use super::*;

    #[test]
    fn failed_credential_handoff_can_retry_with_updated_predecessor_file() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-credential-retry-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source_root = root.join("Sunshine");
        let credential_path = source_root.join("credentials/login.json");
        let destination = root.join("Amaterasu");
        fs::create_dir_all(credential_path.parent().unwrap()).unwrap();
        fs::write(
            source_root.join("sunshine.conf"),
            b"credentials_file = credentials/login.json\n",
        )
        .unwrap();
        fs::write(&credential_path, b"first verifier").unwrap();

        let sources = [LegacyConfigSource {
            product: LegacyProduct::Sunshine,
            root: source_root,
        }];
        let validated = [ValidatedSensitiveSource {
            product: LegacyProduct::Sunshine,
            pairing: false,
            authentication: true,
        }];
        let first =
            import_legacy_configuration_with_sensitive_sources(&destination, &sources, &validated)
                .unwrap();
        let imported = destination.join("legacy-imports/sunshine/credentials/login.json");
        assert_eq!(fs::read(&imported).unwrap(), b"first verifier");
        assert_eq!(
            finish_import(&first, || Err::<(), _>("credential seed failed".into())),
            Err("credential seed failed".into())
        );
        assert!(!imported.exists());
        assert!(first.backup_directory.join("receipt.json").is_file());

        fs::write(&credential_path, b"corrected verifier").unwrap();
        let second =
            import_legacy_configuration_with_sensitive_sources(&destination, &sources, &validated)
                .unwrap();
        finish_import(&second, || Ok(())).unwrap();
        assert_eq!(fs::read(&imported).unwrap(), b"corrected verifier");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_credential_import_retry_revokes_previous_handoff_validation() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-credential-retry-handoff-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let runtime = root.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let source = LegacyConfigSource {
            product: LegacyProduct::Sunshine,
            root: root.join("predecessor"),
        };
        let registrations = [MsiRegistration {
            family: LegacyFamily::Sunshine,
            product_code: "{00000000-0000-0000-0000-000000000001}".into(),
        }];
        write_import_validation(&runtime, std::slice::from_ref(&source)).unwrap();
        require_import_validation(&runtime, &registrations).unwrap();

        // The next attempt starts before validation/copy/seed. Its seed failure
        // must leave no retirement gate from the earlier successful attempt.
        invalidate_import_validation(&runtime).unwrap();
        let check = SourceValidation {
            product: LegacyProduct::Sunshine,
            pairing_needed: false,
            authentication_needed: true,
            pairing_valid: true,
            authentication_valid: true,
        };
        let validation = SensitiveValidation {
            approvals: aggregate_sensitive_approvals(std::slice::from_ref(&check)),
            checks: vec![check],
        };
        assert!(
            seed_sensitive_sources_with(&root, &validation, &runtime.join("state"), |_, _, _| {
                Ok(false)
            })
            .is_err()
        );
        assert!(require_import_validation(&runtime, &registrations).is_err());

        // A later successful retry can publish fresh authorization.
        write_import_validation(&runtime, &[source]).unwrap();
        require_import_validation(&runtime, &registrations).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn approved_credential_copy_still_requires_seeding_after_source_disappears() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-credential-copy-lifecycle-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source_root = root.join("predecessor");
        let destination = root.join("data");
        let state = root.join("runtime/state");
        fs::create_dir_all(source_root.join("credentials")).unwrap();
        fs::write(
            source_root.join("sunshine.conf"),
            b"credentials_file = credentials/login.json\n",
        )
        .unwrap();
        let credential = b"{\"username\":\"admin\",\"password\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",\"salt\":\"1234567890abcdef\"}";
        fs::write(source_root.join("credentials/login.json"), credential).unwrap();
        let source = LegacyConfigSource {
            product: LegacyProduct::Sunshine,
            root: source_root.clone(),
        };
        let check = SourceValidation {
            product: LegacyProduct::Sunshine,
            pairing_needed: false,
            authentication_needed: true,
            pairing_valid: true,
            authentication_valid: true,
        };
        let validation = SensitiveValidation {
            approvals: aggregate_sensitive_approvals(std::slice::from_ref(&check)),
            checks: vec![check],
        };
        amaterasu_package::migration::import_legacy_configuration_with_sensitive_sources(
            &destination,
            &[source],
            &validation.approvals,
        )
        .unwrap();
        fs::remove_dir_all(source_root).unwrap();

        let mut seeded = false;
        seed_sensitive_sources_with(&destination, &validation, &state, |helper, copied, root| {
            assert_eq!(helper, "amaterasu-authentication-import");
            assert_eq!(root, state);
            assert_eq!(
                fs::read(copied.join("credentials/login.json")).unwrap(),
                credential
            );
            seeded = true;
            Ok(true)
        })
        .unwrap();
        assert!(seeded);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn one_approved_root_cannot_clear_another_roots_credential_gate() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-credential-source-binding-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let sources: Vec<_> = ["valid", "unapproved"]
            .into_iter()
            .map(|name| {
                let source_root = root.join(name);
                fs::create_dir_all(source_root.join("credentials")).unwrap();
                fs::write(
                    source_root.join("sunshine.conf"),
                    b"credentials_file = credentials/login.json\n",
                )
                .unwrap();
                fs::write(
                    source_root.join("credentials/login.json"),
                    b"{\"username\":\"admin\",\"password\":\"hash\",\"salt\":\"salt\"}",
                )
                .unwrap();
                LegacyConfigSource {
                    product: LegacyProduct::Sunshine,
                    root: source_root,
                }
            })
            .collect();
        for source in &sources {
            assert!(sensitive_needs(source).unwrap().1);
        }
        let approvals = aggregate_sensitive_approvals(&[
            SourceValidation {
                product: LegacyProduct::Sunshine,
                pairing_needed: false,
                authentication_needed: true,
                pairing_valid: true,
                authentication_valid: true,
            },
            SourceValidation {
                product: LegacyProduct::Sunshine,
                pairing_needed: false,
                authentication_needed: true,
                pairing_valid: true,
                authentication_valid: false,
            },
        ]);
        assert!(!approvals[0].authentication);
        assert!(
            preflight_legacy_configuration_with_sensitive_sources(&sources, &approvals).is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn source_deduplication_keeps_case_distinct_existing_roots() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-migration-cli-case-roots-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let lowercase_root = root.join("profiles");
        let uppercase_root = root.join("Profiles");
        fs::create_dir_all(&lowercase_root).unwrap();
        match fs::create_dir(&uppercase_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_dir_all(&root);
                return;
            }
            Err(error) => panic!("failed to create case-distinct source root: {error}"),
        }

        let mut sources = vec![
            LegacyConfigSource {
                product: LegacyProduct::Sunshine,
                root: lowercase_root.clone(),
            },
            LegacyConfigSource {
                product: LegacyProduct::Sunshine,
                root: uppercase_root.clone(),
            },
        ];
        deduplicate_sources(&mut sources).unwrap();

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].root, lowercase_root);
        assert_eq!(sources[1].root, uppercase_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_deduplication_keeps_distinct_missing_roots_and_deduplicates_exact_matches() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-migration-cli-missing-roots-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let first_missing_root = root.join("missing-first");
        let second_missing_root = root.join("missing-second");
        let first_source = LegacyConfigSource {
            product: LegacyProduct::Sunshine,
            root: first_missing_root.clone(),
        };
        let mut sources = vec![
            first_source.clone(),
            LegacyConfigSource {
                product: LegacyProduct::Sunshine,
                root: second_missing_root.clone(),
            },
            first_source,
        ];

        deduplicate_sources(&mut sources).unwrap();

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].root, first_missing_root);
        assert_eq!(sources[1].root, second_missing_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn finalization_is_idempotent_and_removes_only_valid_handoff_state() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-handoff-cli-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        finalize_predecessor_handoff(&root).unwrap();
        let path = handoff_path(&root).unwrap();
        amaterasu_package::write_json_atomically(
            &path,
            &HandoffState {
                schema: 2,
                records: Vec::new(),
                registrations: vec![MsiRegistration {
                    family: LegacyFamily::Sunshine,
                    product_code: "{01234567-89AB-CDEF-0123-456789ABCDEF}".into(),
                }],
                retirement_started: false,
                retired: false,
            },
        )
        .unwrap();
        finalize_predecessor_handoff(&root).unwrap();
        assert!(!path.exists());
        assert!(!root.join(HANDOFF_DIRECTORY).exists());
        finalize_predecessor_handoff(&root).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn synthetic_path_policy_rejects_escape_and_reparse_candidates() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-path-policy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let program_files = root.join("program-files");
        let program_data = root.join("program-data");
        fs::create_dir_all(program_files.join("Amaterasu")).unwrap();
        fs::create_dir_all(&program_data).unwrap();
        validate_installer_paths_against(
            &program_files.join("Amaterasu"),
            &program_data.join("Amaterasu"),
            &program_files,
            &program_data,
        )
        .unwrap();
        assert!(
            validate_installer_paths_against(
                &root.join("elsewhere"),
                &program_data.join("Amaterasu"),
                &program_files,
                &program_data,
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn coordination_marker_requires_a_live_process_and_exact_nonce_and_can_be_cancelled() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-coordination-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = coordination_path(&root).unwrap();
        let registration = MsiRegistration {
            family: LegacyFamily::Sunshine,
            product_code: "{01234567-89AB-CDEF-0123-456789ABCDEF}".into(),
        };
        let nonce = "ab".repeat(32);
        amaterasu_package::write_json_atomically(
            &path,
            &BundleCoordination {
                schema: 1,
                registrations: vec![registration.clone()],
                nonce_hash: amaterasu_package::sha256(nonce.as_bytes()),
                created_at: amaterasu_package::unix_timestamp(),
                expires_at: amaterasu_package::unix_timestamp().saturating_add(300),
            },
        )
        .unwrap();
        assert!(
            bundle_coordination_matches(&root, std::slice::from_ref(&registration), Some(&nonce))
                .unwrap()
        );
        assert!(
            !bundle_coordination_matches(
                &root,
                std::slice::from_ref(&registration),
                Some(&"cd".repeat(32))
            )
            .unwrap()
        );
        cancel_bundle_upgrade(&root, &nonce).unwrap();
        assert!(!path.exists());

        amaterasu_package::write_json_atomically(
            &path,
            &BundleCoordination {
                schema: 1,
                registrations: vec![registration.clone()],
                nonce_hash: amaterasu_package::sha256(nonce.as_bytes()),
                created_at: 1,
                expires_at: 1,
            },
        )
        .unwrap();
        assert!(!bundle_coordination_matches(&root, &[registration], None).unwrap());
        let _ = fs::remove_dir_all(root);
    }
}
