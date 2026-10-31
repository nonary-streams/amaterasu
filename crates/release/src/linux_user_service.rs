//! Reviewable systemd --user service artifacts for an already active, signed
//! LinuxSession package. Generation never installs or starts the unit.

use crate::profiles::PackageProfile;
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, VerificationPolicy, create_private_directory,
    current_target, package_directory, random_identifier, read_bounded, sha256, sha256_reader,
    unix_timestamp, verify_package,
};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const UNIT_FILE: &str = "amaterasu-linux-session.service";

pub fn prepare(
    runtime_root: &Path,
    host_executable: &Path,
    output_directory: &Path,
) -> PackageResult<()> {
    let runtime_root = existing_path(runtime_root, "runtime root", true)?;
    if runtime_root == Path::new("/") {
        return Err("runtime root cannot be the filesystem root".into());
    }
    let host_executable = existing_path(host_executable, "host executable", false)?;
    validate_executable(&host_executable)?;
    let expected_host_digest = host_executable_sha256(&host_executable)?;
    let output_directory = new_output_path(output_directory)?;

    let bootstrap_path = existing_path(&runtime_root.join("bootstrap.json"), "bootstrap", false)?;
    let selection_path = existing_path(&runtime_root.join("selection.json"), "selection", false)?;
    let bootstrap_bytes = read_bounded(&bootstrap_path, 1024 * 1024)?;
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&bootstrap_bytes).map_err(|error| error.to_string())?;
    if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("user service requires a production bootstrap with trusted signers".into());
    }
    let selection_bytes = read_bounded(&selection_path, 1024 * 1024)?;
    let selection: ActivationState =
        serde_json::from_slice(&selection_bytes).map_err(|error| error.to_string())?;
    let active_digest = selection
        .active
        .as_deref()
        .ok_or("runtime root has no active signed package")?;
    if !valid_digest(active_digest)
        || selection.pending.is_some()
        || selection.trial.is_some()
        || selection.highest_sequence == 0
    {
        return Err("runtime root has an invalid or transitional active selection".into());
    }

    let package_path = existing_path(
        &package_directory(&runtime_root, active_digest)?,
        "active package directory",
        true,
    )?;
    if !package_path.starts_with(runtime_root.join("sets")) {
        return Err("active package path escapes the runtime root".into());
    }
    let verified = verify_package(
        &package_path,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: selection.highest_sequence,
            allow_historical: false,
            now: unix_timestamp(),
        },
    )?;
    if !verified.signed
        || verified.digest != active_digest
        || verified.manifest.host_context_schema != 1
        || verified.manifest.target != current_target()
    {
        return Err("active package is not the selected signed LinuxSession package".into());
    }
    validate_linux_session_composition(&verified.manifest.components)?;
    if host_executable_sha256(&host_executable)? != expected_host_digest {
        return Err("host executable changed during service artifact preparation".into());
    }
    if sha256(&read_bounded(&selection_path, 1024 * 1024)?) != sha256(&selection_bytes)
        || sha256(&read_bounded(&bootstrap_path, 1024 * 1024)?) != sha256(&bootstrap_bytes)
    {
        return Err(
            "runtime trust or active selection changed during service artifact preparation".into(),
        );
    }

    let service_unit = render_service_unit(
        &host_executable,
        &runtime_root,
        active_digest,
        &expected_host_digest,
    )?;
    let mut artifacts = vec![
        (UNIT_FILE, service_unit),
        ("install-user-service.sh", install_script().into()),
        ("uninstall-user-service.sh", uninstall_script().into()),
        ("preflight-user-service.sh", preflight_script().into()),
        ("README.md", render_readme(active_digest)),
    ];
    if crate::linux_owned_display::enabled(&verified.runtime_config.settings) {
        crate::linux_owned_display::validate(
            &package_path,
            &verified.manifest.components,
            &verified.runtime_config.settings,
        )?;
        artifacts.push((
            crate::linux_owned_display::DESKTOP_FILE,
            crate::linux_owned_display::desktop_entry(&host_executable)?,
        ));
        artifacts.push((
            "install-kwin-capture.sh",
            crate::linux_owned_display::install_script().into(),
        ));
    }
    write_artifact_directory(&output_directory, &artifacts)?;
    println!(
        "Prepared user service artifact for signed LinuxSession package {} at {}.",
        verified.digest,
        output_directory.display()
    );
    Ok(())
}

/// Read-only qualification for a generated user-service bundle. It verifies the
/// active package again and queries only the current user's systemd/D-Bus state.
pub fn preflight(
    runtime_root: &Path,
    host_executable: &Path,
    bundle_directory: &Path,
) -> PackageResult<()> {
    let uid = current_user_uid()?;
    if uid == 0 {
        return Err("user-service preflight must run as the target non-root user".into());
    }
    let runtime_root = existing_path(runtime_root, "runtime root", true)?;
    if runtime_root == Path::new("/") {
        return Err("runtime root cannot be the filesystem root".into());
    }
    validate_private_user_directory(&runtime_root, uid, "runtime root")?;
    let state_root = existing_path(&runtime_root.join("data/state"), "runtime state root", true)?;
    validate_private_user_directory(&state_root, uid, "runtime state root")?;
    let host_executable = existing_path(host_executable, "host executable", false)?;
    validate_executable(&host_executable)?;
    let expected_host_digest = host_executable_sha256(&host_executable)?;
    let bundle_directory = existing_path(bundle_directory, "service bundle", true)?;
    let unit_path = existing_path(&bundle_directory.join(UNIT_FILE), "service unit", false)?;
    let unit_bytes = read_bounded(&unit_path, 64 * 1024)?;
    let unit = std::str::from_utf8(&unit_bytes).map_err(|_| "service unit is not UTF-8")?;

    let bootstrap_path = existing_path(&runtime_root.join("bootstrap.json"), "bootstrap", false)?;
    let selection_path = existing_path(&runtime_root.join("selection.json"), "selection", false)?;
    let bootstrap_bytes = read_bounded(&bootstrap_path, 1024 * 1024)?;
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&bootstrap_bytes).map_err(|error| error.to_string())?;
    if bootstrap.development || bootstrap.allow_unsigned || bootstrap.trusted_keys.is_empty() {
        return Err("preflight requires a production bootstrap with trusted signers".into());
    }
    let selection_bytes = read_bounded(&selection_path, 1024 * 1024)?;
    let selection: ActivationState =
        serde_json::from_slice(&selection_bytes).map_err(|error| error.to_string())?;
    let active_digest = selection
        .active
        .as_deref()
        .ok_or("runtime root has no active signed package")?;
    if !valid_digest(active_digest)
        || selection.pending.is_some()
        || selection.trial.is_some()
        || selection.highest_sequence == 0
    {
        return Err("runtime root has an invalid or transitional active selection".into());
    }

    let package_path = existing_path(
        &package_directory(&runtime_root, active_digest)?,
        "active package directory",
        true,
    )?;
    if !package_path.starts_with(runtime_root.join("sets")) {
        return Err("active package path escapes the runtime root".into());
    }
    let verified = verify_package(
        &package_path,
        &bootstrap,
        VerificationPolicy {
            minimum_sequence: selection.highest_sequence,
            allow_historical: false,
            now: unix_timestamp(),
        },
    )?;
    if !verified.signed
        || verified.digest != active_digest
        || verified.manifest.host_context_schema != 1
        || verified.manifest.target != current_target()
    {
        return Err("active package is not the selected signed LinuxSession package".into());
    }
    validate_linux_session_composition(&verified.manifest.components)?;
    let expected_unit = render_service_unit(
        &host_executable,
        &runtime_root,
        active_digest,
        &expected_host_digest,
    )?;
    if unit != expected_unit.as_str() {
        return Err(
            "service unit does not match the active signed package, root, and executable".into(),
        );
    }
    if host_executable_sha256(&host_executable)? != expected_host_digest {
        return Err("host executable changed during user-service preflight".into());
    }
    if sha256(&read_bounded(&selection_path, 1024 * 1024)?) != sha256(&selection_bytes)
        || sha256(&read_bounded(&bootstrap_path, 1024 * 1024)?) != sha256(&bootstrap_bytes)
    {
        return Err("runtime trust or active selection changed during preflight".into());
    }
    let manager_environment = read_user_manager_environment()?;
    validate_manager_runtime_environment(&manager_environment, &current_runtime_environment())?;
    println!(
        "[ok] loader and desktop session environment matches the target user's systemd manager"
    );

    let encoder = verified
        .manifest
        .components
        .iter()
        .find(|component| component.id == "amaterasu.encode")
        .ok_or("signed LinuxSession package has no encoder component")?;
    let encoder_bytes = read_bounded(&package_path.join(&encoder.file), 256 * 1024 * 1024)?;
    let x264_build = crate::development::embedded_x264_build(&encoder_bytes)
        .ok_or("active encoder has no embedded software-x264 ABI marker")?;
    if !crate::development::x264_runtime_available(x264_build) {
        return Err(format!(
            "loader cannot find a compatible system x264 runtime for build {x264_build}"
        ));
    }
    println!(
        "[ok] signed active LinuxSession package {active_digest} verified; x264 ABI {x264_build} is loader-visible"
    );
    if let Some((avcodec, avutil)) = crate::development::embedded_ffmpeg_abi(&encoder_bytes)? {
        if !crate::development::ffmpeg_runtime_available(avcodec, avutil) {
            return Err(format!(
                "loader cannot find compatible system FFmpeg runtimes for avcodec {avcodec} and avutil {avutil}"
            ));
        }
        println!(
            "[ok] optional FFmpeg NVENC encoder ABI avcodec {avcodec}/avutil {avutil} is loader-visible"
        );
    }

    if let Some(video) = verified
        .manifest
        .components
        .iter()
        .find(|component| component.id == crate::profiles::linux_session::VIDEO_COMPONENT_ID)
    {
        crate::linux_video_runtime::validate(&read_bounded(
            &package_path.join(&video.file),
            256 * 1024 * 1024,
        )?)?;
    }
    crate::linux_owned_display::validate(
        &package_path,
        &verified.manifest.components,
        &verified.runtime_config.settings,
    )?;
    let kwin_capture = crate::linux_owned_display::enabled(&verified.runtime_config.settings);
    if kwin_capture {
        let declaration = read_bounded(
            &bundle_directory.join(crate::linux_owned_display::DESKTOP_FILE),
            64 * 1024,
        )?;
        if declaration != crate::linux_owned_display::desktop_entry(&host_executable)?.as_bytes() {
            return Err("KDE capture declaration does not match this host".into());
        }
        require_library("libwayland-client.so.0", &["wl_display_connect"])?;
        println!(
            "[ok] linked owned-display features and scoped KDE declaration match the selected host"
        );
    }
    let needs = runtime_needs(&verified.runtime_config.settings)?;
    if needs.pulse {
        require_library("libpulse.so.0", &["pa_context_new", "pa_context_connect"])?;
        println!("[ok] libpulse.so.0 and required audio symbols are loader-visible");
        println!("[info] Pulse server connectivity was not tested; no audio stream was opened");
    } else {
        println!("[skip] PulseAudio runtime check; signed profile audio is disabled");
    }

    if needs.pipewire_portal {
        require_library("libpipewire-0.3.so.0", &["pw_init", "pw_context_new"])?;
        require_library("libgio-2.0.so.0", &["g_bus_get_sync"])?;
        println!("[ok] PipeWire and GIO portal libraries are loader-visible");
        if kwin_capture {
            println!(
                "[info] named KWin capture uses the scoped executable declaration; no portal selection is required"
            );
        } else {
            match require_portal_available()? {
                PortalAvailability::Running => println!(
                    "[ok] current-user ScreenCast portal bus name is owned (no portal method or selection prompt was invoked)"
                ),
                PortalAvailability::Activatable => println!(
                    "[ok] current-user ScreenCast portal is D-Bus activatable (it was not started and no portal method or selection prompt was invoked)"
                ),
            }
        }
    } else {
        println!("[skip] PipeWire/portal runtime checks; signed profile video capture is disabled");
    }

    check_user_manager_and_unit(uid, &unit_bytes)?;
    println!(
        "[ok] current user's systemd manager is available and the unit name resolves without collision"
    );
    println!("Preflight passed. No service was enabled, started, installed, or modified.");
    Ok(())
}

fn setting_enabled(
    settings: &std::collections::BTreeMap<String, serde_json::Value>,
    component: &str,
) -> bool {
    settings
        .get(component)
        .and_then(|value| value.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeNeeds {
    pulse: bool,
    pipewire_portal: bool,
}

fn runtime_needs(
    settings: &std::collections::BTreeMap<String, serde_json::Value>,
) -> PackageResult<RuntimeNeeds> {
    let video_enabled =
        setting_enabled(settings, crate::profiles::linux_session::VIDEO_COMPONENT_ID);
    if video_enabled
        && (!settings
            .get(crate::profiles::linux_session::VIDEO_COMPONENT_ID)
            .and_then(|value| value.get("capture_providers"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|providers| providers.iter().any(|provider| provider == "pipewire"))
            || !setting_enabled(settings, "amaterasu.capture-pipewire"))
    {
        return Err("signed video settings do not enable the PipeWire portal provider".into());
    }
    Ok(RuntimeNeeds {
        pulse: setting_enabled(settings, "amaterasu.audio"),
        pipewire_portal: video_enabled,
    })
}

fn require_library(name: &str, symbols: &[&str]) -> PackageResult<()> {
    // SAFETY: only fixed system sonames are loaded; this checks loader resolution
    // and symbol presence without calling into the library or loading a package.
    let library = unsafe { libloading::Library::new(name) }
        .map_err(|_| format!("loader cannot find required system library {name}"))?;
    for symbol in symbols {
        // SAFETY: lookup only tests that the fixed ABI symbol is exported.
        unsafe { library.get::<*mut std::ffi::c_void>(symbol.as_bytes()) }
            .map_err(|_| format!("system library {name} lacks required symbol {symbol}"))?;
    }
    Ok(())
}

fn current_user_uid() -> PackageResult<u32> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|error| format!("could not determine target user identity: {error}"))?;
    if !output.status.success() {
        return Err("could not determine target user identity".into());
    }
    let uid = String::from_utf8(output.stdout).map_err(|_| "target user identity is not UTF-8")?;
    uid.trim()
        .parse::<u32>()
        .map_err(|_| "target user identity is invalid".into())
}

const SESSION_RUNTIME_ENVIRONMENT: [&str; 9] = [
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "LD_AUDIT",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "WAYLAND_DISPLAY",
    "PULSE_SERVER",
];

fn current_runtime_environment() -> std::collections::BTreeMap<String, String> {
    SESSION_RUNTIME_ENVIRONMENT
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect()
}

fn validate_manager_runtime_environment(
    manager_environment: &str,
    process_environment: &std::collections::BTreeMap<String, String>,
) -> PackageResult<()> {
    let manager_environment = manager_environment
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect::<std::collections::BTreeMap<_, _>>();
    for name in SESSION_RUNTIME_ENVIRONMENT {
        if manager_environment.get(name).copied()
            != process_environment.get(name).map(String::as_str)
        {
            return Err(format!(
                "current process and systemd manager differ in {name}; rerun preflight in the target user's desktop session"
            ));
        }
    }
    Ok(())
}

fn validate_private_user_directory(path: &Path, uid: u32, description: &str) -> PackageResult<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "{description} must be a user-owned directory without group/world write access"
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PortalAvailability {
    Running,
    Activatable,
}

fn require_portal_available() -> PackageResult<PortalAvailability> {
    require_portal_available_with(OsStr::new("busctl"))
}

fn require_portal_available_with(busctl: &OsStr) -> PackageResult<PortalAvailability> {
    let output = Command::new(busctl)
        .args(["--user", "--no-pager", "--no-legend", "list"])
        .output()
        .map_err(|error| format!("could not query the current user's D-Bus name list: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "current user's D-Bus name query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let names = String::from_utf8(output.stdout).map_err(|_| "D-Bus name list is not UTF-8")?;
    if portal_name_owned(&names) {
        return Ok(PortalAvailability::Running);
    }

    // Listing D-Bus activation candidates is a read-only query. It does not
    // request the portal name or trigger its service activation.
    let output = Command::new(busctl)
        .args([
            "--user",
            "--no-pager",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListActivatableNames",
        ])
        .output()
        .map_err(|error| format!("could not query D-Bus activatable names: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "current user's D-Bus activation query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let names = String::from_utf8(output.stdout)
        .map_err(|_| "current user's activatable D-Bus names are not UTF-8")?;
    if activatable_name_list_contains(&names, "org.freedesktop.portal.Desktop") {
        Ok(PortalAvailability::Activatable)
    } else {
        Err("current user's ScreenCast portal is neither running nor D-Bus activatable".into())
    }
}

fn portal_name_owned(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.split_whitespace().next() == Some("org.freedesktop.portal.Desktop"))
}

fn activatable_name_list_contains(output: &str, expected: &str) -> bool {
    let mut fields = output.split_whitespace();
    if fields.next() != Some("as") {
        return false;
    }
    let Some(count) = fields.next().and_then(|value| value.parse::<usize>().ok()) else {
        return false;
    };
    if count > 4096 {
        return false;
    }
    let names: Vec<_> = fields.collect();
    if names.len() != count {
        return false;
    }
    let mut found = false;
    for field in names {
        let Some(name) = field
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
        else {
            return false;
        };
        found |= name == expected;
    }
    found
}

fn check_user_manager_and_unit(uid: u32, bundle_unit: &[u8]) -> PackageResult<()> {
    let target = user_unit_target_path(uid)?;
    let unit_files = query_systemctl(&[
        "--user",
        "list-unit-files",
        "--all",
        "--no-legend",
        "--no-pager",
    ])?;
    let loaded_units = query_systemctl(&[
        "--user",
        "list-units",
        "--all",
        "--plain",
        "--no-legend",
        "--no-pager",
    ])?;
    let unit_is_listed = listing_contains_unit(&unit_files) || listing_contains_unit(&loaded_units);
    let (load_state, fragment) = if unit_is_listed {
        let resolution = query_systemctl(&[
            "--user",
            "show",
            "--no-pager",
            "--property=LoadState",
            "--property=FragmentPath",
            UNIT_FILE,
        ])?;
        parse_fragment_path(&resolution)?
    } else {
        ("not-found".to_owned(), None)
    };
    let installed = match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("user unit target exists but is not a regular managed file".into());
        }
        Ok(_) => Some(read_bounded(&target, 64 * 1024)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if installed.is_some() && !unit_is_listed {
        return Err(
            "unit file exists but the user's systemd manager has not resolved it yet".into(),
        );
    }
    if let Some(installed) = installed.as_deref()
        && installed != bundle_unit
    {
        return Err("existing user unit target differs from the generated bundle".into());
    }
    let expected = if target.exists() {
        target.canonicalize().map_err(|error| error.to_string())?
    } else {
        target.clone()
    };
    let expected = expected.to_string_lossy();
    let resolved = fragment
        .as_deref()
        .map(|path| {
            Path::new(path)
                .canonicalize()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|_| "systemd resolved the unit to an unavailable path".to_owned())
        })
        .transpose()?;
    validate_manager_resolution(
        load_state.as_str(),
        resolved.as_deref(),
        &expected,
        installed.is_some(),
    )?;
    println!("[ok] target user UID {uid}; systemd --user manager query succeeded");
    if installed.is_some() {
        println!("[ok] exact generated unit bytes already exist in the user's unit directory");
    } else {
        println!("[ok] no existing unit file or resolved unit-name collision");
    }
    Ok(())
}

fn parse_fragment_path(output: &str) -> PackageResult<(String, Option<String>)> {
    let mut load_state = None;
    let mut fragment = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("LoadState=") {
            if load_state.replace(value).is_some() {
                return Err("systemd returned duplicate LoadState fields".into());
            }
        } else if let Some(value) = line.strip_prefix("FragmentPath=")
            && fragment.replace(value).is_some()
        {
            return Err("systemd returned duplicate FragmentPath fields".into());
        }
    }
    let load_state = load_state
        .ok_or("systemd response omitted LoadState")?
        .to_owned();
    let fragment = fragment.filter(|path| !path.is_empty()).map(str::to_owned);
    Ok((load_state, fragment))
}

fn validate_manager_resolution(
    load_state: &str,
    fragment_path: Option<&str>,
    expected_path: &str,
    target_file_exists: bool,
) -> PackageResult<()> {
    match load_state {
        "not-found" if fragment_path.is_none() && !target_file_exists => Ok(()),
        "not-found" => Err("systemd failed to load an existing unit file".into()),
        "loaded" => {
            let fragment_path =
                fragment_path.ok_or("systemd loaded the unit without a FragmentPath")?;
            if fragment_path != expected_path {
                return Err(format!(
                    "unit name collision: systemd resolves {} from {}, expected {}",
                    UNIT_FILE, fragment_path, expected_path
                ));
            }
            if !target_file_exists {
                return Err(
                    "systemd resolves the user-service name but the expected unit file is absent"
                        .into(),
                );
            }
            Ok(())
        }
        state => Err(format!(
            "user unit has unexpected systemd load state {state}"
        )),
    }
}

fn listing_contains_unit(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.split_whitespace().next() == Some(UNIT_FILE))
}

fn query_systemctl(arguments: &[&str]) -> PackageResult<String> {
    let output = Command::new("systemctl")
        .args(arguments)
        .output()
        .map_err(|error| format!("could not query current user's systemd manager: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "systemd user-manager query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| "systemd user-manager output is not UTF-8".into())
}

fn read_user_manager_environment() -> PackageResult<String> {
    let output = Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .map_err(|error| format!("could not query the current user's systemd manager: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "current user's systemd manager is unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| "current user's systemd environment is not UTF-8".into())
}

fn user_unit_target_path(uid: u32) -> PackageResult<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable for user-service preflight")?;
    let home = existing_path(Path::new(&home), "user home", true)?;
    use std::os::unix::fs::MetadataExt;
    let home_metadata = fs::metadata(&home).map_err(|error| error.to_string())?;
    if home == Path::new("/") || home_metadata.uid() != uid || home_metadata.mode() & 0o022 != 0 {
        return Err("HOME must be a private directory owned by the target user".into());
    }
    let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(config_home) => {
            let config_home = PathBuf::from(config_home);
            if !config_home.is_absolute()
                || config_home
                    .components()
                    .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
                || !config_home.starts_with(&home)
            {
                return Err(
                    "XDG_CONFIG_HOME must be an absolute normalized path under HOME".into(),
                );
            }
            optional_directory_path(&config_home, &home)?
        }
        None => home.join(".config"),
    };
    validate_user_directory_chain(&config_home, &home, uid)?;
    let user_unit_directory = config_home.join("systemd/user");
    check_existing_ancestors_for_symlinks(&user_unit_directory)?;
    validate_user_directory_chain(&user_unit_directory, &home, uid)?;
    let target = user_unit_directory.join(UNIT_FILE);
    Ok(target)
}

fn validate_user_directory_chain(path: &Path, home: &Path, uid: u32) -> PackageResult<()> {
    use std::os::unix::fs::MetadataExt;

    let relative = path
        .strip_prefix(home)
        .map_err(|_| "user-service directory escapes HOME")?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("user-service directory path is not normalized".into());
        };
        current.push(name);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o022 != 0
        {
            return Err(format!(
                "user-service directory {} must be a non-writable directory owned by the target user",
                current.display()
            ));
        }
    }
    Ok(())
}

fn optional_directory_path(path: &Path, home: &Path) -> PackageResult<PathBuf> {
    check_existing_ancestors_for_symlinks(path)?;
    match path.canonicalize() {
        Ok(canonical) if canonical.starts_with(home) => Ok(canonical),
        Ok(_) => Err("XDG_CONFIG_HOME resolves outside HOME".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(error.to_string()),
    }
}

fn check_existing_ancestors_for_symlinks(path: &Path) -> PackageResult<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "user-service path contains symlink {}",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn validate_linux_session_composition(
    components: &[amaterasu_package::ComponentManifest],
) -> PackageResult<()> {
    let actual = components
        .iter()
        .map(|component| component.id.as_str())
        .collect::<BTreeSet<_>>();
    let expected = PackageProfile::LinuxSession
        .components()
        .iter()
        .map(|component| component.id)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err("signed package does not match the LinuxSession component composition".into());
    }
    Ok(())
}

fn existing_path(path: &Path, description: &str, directory: bool) -> PackageResult<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(format!("{description} must be an absolute normalized path"));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("{description} path is unavailable: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("{description} path cannot contain symlinks"));
        }
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("{description} path cannot be canonicalized: {error}"))?;
    let metadata = fs::metadata(&canonical).map_err(|error| error.to_string())?;
    if directory != metadata.is_dir() || (!directory && !metadata.is_file()) {
        return Err(format!("{description} has the wrong filesystem type"));
    }
    Ok(canonical)
}

fn validate_executable(path: &Path) -> PackageResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)
            .map_err(|error| error.to_string())?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err("host executable has no execute bit".into());
        }
    }
    Ok(())
}

fn new_output_path(path: &Path) -> PackageResult<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("artifact output must be an absolute normalized path".into());
    }
    let name = path
        .file_name()
        .ok_or("artifact output must name a new directory")?;
    if !matches!(path.components().next_back(), Some(Component::Normal(_))) {
        return Err("artifact output must name a new directory".into());
    }
    let parent = path.parent().ok_or("artifact output parent is missing")?;
    let parent = existing_path(parent, "artifact output parent", true)?;
    let output = parent.join(name);
    match fs::symlink_metadata(&output) {
        Ok(_) => return Err("artifact output already exists; refusing to replace it".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("artifact output cannot be inspected: {error}")),
    }
    Ok(output)
}

fn write_artifact_directory(output: &Path, files: &[(&str, String)]) -> PackageResult<()> {
    let parent = output.parent().ok_or("artifact output parent is missing")?;
    let temporary = parent.join(format!(".amaterasu-user-service-{}", random_identifier()?));
    create_private_directory(&temporary)?;
    let result = (|| {
        for (name, contents) in files {
            fs::write(temporary.join(name), contents).map_err(|error| error.to_string())?;
        }
        fs::rename(&temporary, output).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn render_service_unit(
    executable: &Path,
    root: &Path,
    digest: &str,
    host_executable_digest: &str,
) -> PackageResult<String> {
    if !valid_digest(digest) || !valid_digest(host_executable_digest) {
        return Err("service unit digests must be SHA-256 values".into());
    }
    let executable = systemd_quote(executable)?;
    let root = systemd_quote(root)?;
    Ok(format!(
        "[Unit]\nDescription=Amaterasu LinuxSession ({})\n# HostExecutableSha256={host_executable_digest}\nAfter=graphical-session.target\nPartOf=graphical-session.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart={executable} service-user {root} {digest}\nRestart=on-failure\nRestartSec=5s\nKillSignal=SIGTERM\nKillMode=mixed\nTimeoutStopSec=20s\n\n[Install]\nWantedBy=graphical-session.target\n",
        &digest[..12],
    ))
}

/// Hashes the executable bytes without running the selected host program. This
/// keeps preparation usable for cross-built artifacts and pins the bytes into
/// the generated, reviewable unit for later preflight comparisons.
fn host_executable_sha256(path: &Path) -> PackageResult<String> {
    let before = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err("host executable must be a regular non-symlink file".into());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || opened.len() != before.len() {
        return Err("host executable changed while opening for digest".into());
    }
    let digest = sha256_reader(file)?;
    let after = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if after.file_type().is_symlink() || !after.is_file() || after.len() != opened.len() {
        return Err("host executable changed while hashing".into());
    }
    Ok(digest)
}

fn systemd_quote(path: &Path) -> PackageResult<String> {
    let value = path
        .to_str()
        .ok_or("systemd unit paths must be valid UTF-8")?;
    if value.chars().any(char::is_control) {
        return Err("systemd unit paths cannot contain control characters".into());
    }
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            value => quoted.push(value),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

fn install_script() -> &'static str {
    r#"#!/bin/sh
set -eu
unit_name=amaterasu-linux-session.service
[ "$#" -eq 2 ] || { echo "usage: sh install-user-service.sh RUNTIME_ROOT HOST_EXECUTABLE" >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
source_unit="$bundle_dir/$unit_name"
[ -f "$source_unit" ] && [ ! -L "$source_unit" ] || { echo "service artifact is missing or unsafe" >&2; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo "refusing to install a user service as root" >&2; exit 1; }
release_cli=${AMATERASU_RELEASE_BIN:-amaterasu-release}
command -v "$release_cli" >/dev/null 2>&1 || { echo "amaterasu-release must be available, or set AMATERASU_RELEASE_BIN" >&2; exit 1; }
"$release_cli" preflight-linux-user-service "$1" "$2" "$bundle_dir"
command -v systemctl >/dev/null 2>&1 || { echo "systemctl is required for a user unit" >&2; exit 1; }
systemctl --user show-environment >/dev/null 2>&1 || { echo "current user systemd manager is unavailable" >&2; exit 1; }
[ -n "${HOME:-}" ] && [ "${HOME#/}" != "$HOME" ] || { echo "HOME must be an absolute path" >&2; exit 1; }
config_home=${XDG_CONFIG_HOME:-"$HOME/.config"}
case "$config_home/" in "$HOME/"*) ;; *) echo "XDG_CONFIG_HOME must remain under HOME" >&2; exit 1 ;; esac
case "$config_home" in /*) ;; *) echo "XDG_CONFIG_HOME must be absolute" >&2; exit 1 ;; esac
case "$config_home/" in */../*|*/./*) echo "XDG_CONFIG_HOME must be normalized" >&2; exit 1 ;; esac
check_no_symlinks() {
    current=
    rest=${1#/}
    while [ -n "$rest" ]; do
        part=${rest%%/*}
        if [ "$part" = "$rest" ]; then rest=; else rest=${rest#*/}; fi
        current="$current/$part"
        [ ! -L "$current" ] || { echo "refusing symlink path: $current" >&2; return 1; }
    done
}
check_no_symlinks "$config_home"
user_dir="$config_home/systemd/user"
check_no_symlinks "$config_home/systemd"
check_no_symlinks "$user_dir"
mkdir -p "$user_dir"
check_no_symlinks "$user_dir"
target="$user_dir/$unit_name"
selected_fragment=$(systemctl --user show --property=FragmentPath --value "$unit_name" 2>/dev/null || true)
if [ -n "$selected_fragment" ]; then
    [ -f "$target" ] && [ ! -L "$target" ] || {
        echo "refusing to shadow a different user unit named $unit_name: $selected_fragment" >&2; exit 1;
    }
    expected_fragment=$(readlink -f -- "$target")
    [ "$selected_fragment" = "$expected_fragment" ] || {
        echo "refusing to modify a different user unit named $unit_name: $selected_fragment" >&2; exit 1;
    }
fi
wanted_dir="$user_dir/graphical-session.target.wants"
wanted_link="$wanted_dir/$unit_name"
wanted_preexisting=0
if [ -L "$wanted_link" ]; then
    [ "$(readlink -- "$wanted_link")" = "../$unit_name" ] || {
        echo "refusing to overwrite an unmanaged enable link: $wanted_link" >&2; exit 1;
    }
    wanted_preexisting=1
elif [ -e "$wanted_link" ]; then
    echo "refusing to overwrite an unmanaged enable path: $wanted_link" >&2; exit 1
fi
created=0
if [ -e "$target" ] || [ -L "$target" ]; then
    [ ! -L "$target" ] && [ -f "$target" ] && cmp -s "$source_unit" "$target" || {
        echo "refusing to overwrite unmanaged service content: $target" >&2; exit 1;
    }
else
    temporary=$(mktemp "$user_dir/.amaterasu-linux-session.XXXXXX")
    trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
    cp -- "$source_unit" "$temporary"
    cmp -s "$source_unit" "$temporary"
    ln -- "$temporary" "$target"
    rm -- "$temporary"
    trap - EXIT HUP INT TERM
    created=1
fi
rollback_install_changes() {
    changed=0
    if [ "$wanted_preexisting" -eq 0 ] && [ -L "$wanted_link" ] && [ "$(readlink -- "$wanted_link")" = "../$unit_name" ]; then
        rm -- "$wanted_link"
        changed=1
    fi
    if [ "$created" -eq 1 ] && [ -f "$target" ] && [ ! -L "$target" ] && cmp -s "$source_unit" "$target"; then
        rm -- "$target"
        changed=1
    fi
    if [ "$changed" -eq 1 ]; then
        systemctl --user daemon-reload >/dev/null 2>&1 || true
    fi
}
if ! systemctl --user daemon-reload; then
    rollback_install_changes
    echo "systemd user manager could not reload; new unit rolled back" >&2
    exit 1
fi
expected_fragment=$(readlink -f -- "$target")
selected_fragment=$(systemctl --user show --property=FragmentPath --value "$unit_name" 2>/dev/null || true)
if [ "$selected_fragment" != "$expected_fragment" ]; then
    rollback_install_changes
    echo "systemd user manager did not resolve the generated unit; new unit rolled back" >&2
    exit 1
fi
if ! "$release_cli" preflight-linux-user-service "$1" "$2" "$bundle_dir"; then
    rollback_install_changes
    echo "active signed package or desktop prerequisites changed before enable; new unit rolled back" >&2
    exit 1
fi
if ! systemctl --user enable "$unit_name"; then
    rollback_install_changes
    echo "systemd user manager could not enable the unit; new unit rolled back" >&2
    exit 1
fi
if ! "$release_cli" preflight-linux-user-service "$1" "$2" "$bundle_dir"; then
    rollback_install_changes
    echo "active signed package or desktop prerequisites changed during enable; new unit changes rolled back" >&2
    exit 1
fi
echo "Enabled $unit_name for this user; it will start with graphical-session.target."
echo "To start it now, run: systemctl --user start $unit_name"
"#
}

fn uninstall_script() -> &'static str {
    r#"#!/bin/sh
set -eu
unit_name=amaterasu-linux-session.service
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
source_unit="$bundle_dir/$unit_name"
[ -f "$source_unit" ] && [ ! -L "$source_unit" ] || { echo "matching service artifact is required for safe uninstall" >&2; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo "refusing to uninstall a user service as root" >&2; exit 1; }
command -v systemctl >/dev/null 2>&1 || { echo "systemctl is required for a user unit" >&2; exit 1; }
systemctl --user show-environment >/dev/null 2>&1 || { echo "current user systemd manager is unavailable; leaving files untouched" >&2; exit 1; }
[ -n "${HOME:-}" ] && [ "${HOME#/}" != "$HOME" ] || { echo "HOME must be an absolute path" >&2; exit 1; }
config_home=${XDG_CONFIG_HOME:-"$HOME/.config"}
case "$config_home/" in "$HOME/"*) ;; *) echo "XDG_CONFIG_HOME must remain under HOME" >&2; exit 1 ;; esac
case "$config_home" in /*) ;; *) echo "XDG_CONFIG_HOME must be absolute" >&2; exit 1 ;; esac
case "$config_home/" in */../*|*/./*) echo "XDG_CONFIG_HOME must be normalized" >&2; exit 1 ;; esac
check_no_symlinks() {
    current=
    rest=${1#/}
    while [ -n "$rest" ]; do
        part=${rest%%/*}
        if [ "$part" = "$rest" ]; then rest=; else rest=${rest#*/}; fi
        current="$current/$part"
        [ ! -L "$current" ] || { echo "refusing symlink path: $current" >&2; return 1; }
    done
}
user_dir="$config_home/systemd/user"
check_no_symlinks "$user_dir"
target="$user_dir/$unit_name"
[ -f "$target" ] && [ ! -L "$target" ] && cmp -s "$source_unit" "$target" || {
    echo "installed unit is absent or differs from the managed artifact; leaving it untouched" >&2; exit 1;
}
expected_fragment=$(readlink -f -- "$target")
selected_fragment=$(systemctl --user show --property=FragmentPath --value "$unit_name" 2>/dev/null || true)
[ "$selected_fragment" = "$expected_fragment" ] || {
    echo "user manager resolves a different unit named $unit_name; leaving it untouched" >&2; exit 1;
}
unit_status=$(systemctl --user show --no-pager --property=ActiveState --property=UnitFileState "$unit_name" 2>/dev/null) || {
    echo "could not query active and enablement state for $unit_name; leaving files untouched" >&2; exit 1;
}
active_state=
unit_file_state=
active_state_seen=0
unit_file_state_seen=0
while IFS='=' read -r property value; do
    case "$property" in
        ActiveState) [ "$active_state_seen" -eq 0 ] || { echo "systemd returned duplicate ActiveState; leaving files untouched" >&2; exit 1; }; active_state_seen=1; active_state=$value ;;
        UnitFileState) [ "$unit_file_state_seen" -eq 0 ] || { echo "systemd returned duplicate UnitFileState; leaving files untouched" >&2; exit 1; }; unit_file_state_seen=1; unit_file_state=$value ;;
    esac
done <<EOF
$unit_status
EOF
case "$active_state" in
    active) active=1 ;;
    inactive|failed) active=0 ;;
    *) echo "systemd returned uncertain ActiveState '$active_state'; leaving files untouched" >&2; exit 1 ;;
esac
case "$unit_file_state" in
    enabled) enabled=1 ;;
    disabled) enabled=0 ;;
    *) echo "systemd returned uncertain UnitFileState '$unit_file_state'; leaving files untouched" >&2; exit 1 ;;
esac
if [ "$active" -eq 1 ]; then
    systemctl --user stop "$unit_name"
fi
if [ "$enabled" -eq 1 ]; then
    systemctl --user disable "$unit_name"
fi
[ -f "$target" ] && [ ! -L "$target" ] && cmp -s "$source_unit" "$target" || {
    echo "unit changed during uninstall; refusing to remove it" >&2; exit 1;
}
rm -- "$target"
systemctl --user daemon-reload
echo "Removed the verified Amaterasu user unit. Runtime data and signed package were left untouched."
"#
}

fn preflight_script() -> &'static str {
    r#"#!/bin/sh
set -eu
[ "$#" -eq 2 ] || { echo "usage: sh preflight-user-service.sh RUNTIME_ROOT HOST_EXECUTABLE" >&2; exit 2; }
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
release_cli=${AMATERASU_RELEASE_BIN:-amaterasu-release}
command -v "$release_cli" >/dev/null 2>&1 || { echo "amaterasu-release must be available, or set AMATERASU_RELEASE_BIN" >&2; exit 1; }
exec "$release_cli" preflight-linux-user-service "$1" "$2" "$bundle_dir"
"#
}

fn render_readme(digest: &str) -> String {
    format!(
        r#"# Amaterasu LinuxSession user service

This bundle was generated for signed active package `{digest}`. The unit runs under the current user manager and never requests root or system service privileges. Its `ExecStart` pins this digest; a different active package fails closed until a new bundle is generated and reviewed.

Before installing or starting, run `sh preflight-user-service.sh RUNTIME_ROOT HOST_EXECUTABLE` as the target non-root user. The script calls `amaterasu-release preflight-linux-user-service` (or `$AMATERASU_RELEASE_BIN`) to reverify the active signature and digest, check loader-visible x264 and optional FFmpeg encoder ABI plus Pulse/PipeWire/GIO libraries, verify the current user manager's unit resolution, and confirm the current user's portal name is either owned or D-Bus activatable when capture is enabled. The D-Bus activation query is read-only; it does not start the portal. The check does not enable/start the Amaterasu unit or request portal consent. It requires the release CLI to be available on the target host. It checks library loading, not the NVENC device/driver, Pulse server connectivity, the ScreenCast interface response, user consent, or an actual capture/audio stream.

Review `amaterasu-linux-session.service`, then install it for this user with `sh install-user-service.sh RUNTIME_ROOT HOST_EXECUTABLE`. The installer runs the same read-only preflight before it writes, immediately before enabling, and after enabling. If the active selection or prerequisites change, it removes only the enable link and unit file created by that attempt. Installation enables the unit for `graphical-session.target` but does not start it. For a manual start, run the standalone preflight immediately beforehand, then use `systemctl --user start amaterasu-linux-session.service`.

The host verifies the production bootstrap, trusted package signature, active selection, and pinned digest on every start. A module tick or health failure exits the service; systemd restarts it after a fixed five-second delay and the next start re-verifies the same digest. SIGTERM triggers orderly component teardown and Linux input revocation. A forced kill is recovered by the same fresh verification and load on restart.

The generated unit records the SHA-256 of the exact host executable bytes supplied during preparation. Preflight hashes the supplied executable again and requires the digest and path to match the unit, without executing the binary; replacing the executable or supplying a symlink fails. Review and preserve the unit together with the selected host binary. The bundle itself is not signed, and the digest does not authenticate the bundle or prevent replacement of the host file after preflight; protect both files and rerun preflight before enabling or starting.

Uninstall from this same bundle with `sh uninstall-user-service.sh`. It stops and disables the user unit only after verifying that the installed unit bytes match this bundle and systemd reports known active and enablement states. A failed or unexpected state query leaves files untouched. It rechecks before removing the file and leaves runtime data and the signed package intact. If the unit was edited or replaced, uninstall refuses to remove it.

For a package with owned virtual displays, preparation also emits `org.amaterasu.StreamingHost.desktop` and `install-kwin-capture.sh`. Run the latter as the desktop user before starting the host; it declares only the exact executable's KDE ScreenCast interface and refreshes KDE's cache. It refuses to replace a different declaration. The broker socket must separately grant that user access; connector ownership remains enforced by Unix UID. These capture artifacts are separate from the user service installer.

The generated service scripts affect only the current user service directory selected by `XDG_CONFIG_HOME` (or `$HOME/.config`). They refuse root execution and symlinked service paths, and verify that systemd resolves this exact unit file before enabling or uninstalling it. No service was installed or started when this bundle was generated.
"#
    )
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::Grants;
    use amaterasu_package::ComponentManifest;

    #[test]
    fn unit_pins_verified_digest_and_uses_user_session_restart_semantics() {
        let unit = render_service_unit(
            Path::new("/home/user/bin/amaterasu"),
            Path::new("/home/user/.local/share/amaterasu"),
            &"a".repeat(64),
            &"b".repeat(64),
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"/home/user/bin/amaterasu\" service-user \"/home/user/.local/share/amaterasu\""));
        assert!(unit.contains(&"a".repeat(64)));
        assert!(unit.contains(&format!("HostExecutableSha256={}", "b".repeat(64))));
        assert!(unit.contains("WantedBy=graphical-session.target"));
        assert!(unit.contains("Restart=on-failure\nRestartSec=5s"));
        assert!(unit.contains("KillMode=mixed\nTimeoutStopSec=20s"));
        assert!(!unit.contains("User=root"));
        assert!(!unit.contains("sudo"));
    }

    #[test]
    fn systemd_arguments_escape_specifiers_quotes_and_backslashes() {
        assert_eq!(
            systemd_quote(Path::new("/home/user/100%/a\\b\"c")).unwrap(),
            "\"/home/user/100%%/a\\\\b\\\"c\""
        );
        assert!(systemd_quote(Path::new("/home/user/line\nbreak")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn host_binary_digest_is_stable_and_rejects_replacement_or_symlink() {
        use std::os::unix::fs::symlink;

        let directory = std::env::temp_dir().join(format!(
            "amaterasu-host-binary-digest-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("amaterasu");
        fs::write(&executable, b"reviewed host executable bytes").unwrap();

        let initial_digest = host_executable_sha256(&executable).unwrap();
        assert_eq!(host_executable_sha256(&executable).unwrap(), initial_digest);
        let prepared_unit = render_service_unit(
            Path::new("/home/user/bin/amaterasu"),
            Path::new("/home/user/.local/share/amaterasu"),
            &"a".repeat(64),
            &initial_digest,
        )
        .unwrap();

        let replacement = directory.join("replacement");
        fs::write(&replacement, b"replacement host executable bytes").unwrap();
        fs::rename(replacement, &executable).unwrap();
        let replacement_digest = host_executable_sha256(&executable).unwrap();
        assert_ne!(replacement_digest, initial_digest);
        assert_ne!(
            render_service_unit(
                Path::new("/home/user/bin/amaterasu"),
                Path::new("/home/user/.local/share/amaterasu"),
                &"a".repeat(64),
                &replacement_digest,
            )
            .unwrap(),
            prepared_unit
        );

        let link = directory.join("amaterasu-link");
        symlink(&executable, &link).unwrap();
        assert!(host_executable_sha256(&link).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn installer_and_uninstaller_guard_unmanaged_content_by_exact_bytes() {
        let install = install_script();
        let uninstall = uninstall_script();
        assert!(install.contains("cmp -s \"$source_unit\" \"$target\""));
        assert!(install.contains("refusing to overwrite unmanaged service content"));
        assert!(install.contains("FragmentPath"));
        assert!(install.contains("refusing to shadow a different user unit"));
        assert!(install.contains("preflight-linux-user-service"));
        assert!(
            install.find("preflight-linux-user-service") < install.find("mkdir -p \"$user_dir\"")
        );
        assert!(install.contains("systemctl --user enable"));
        assert_eq!(
            install.matches("preflight-linux-user-service").count(),
            3,
            "installer must qualify before writes, immediately before enable, and after enable"
        );
        assert!(uninstall.contains("UnitFileState"));
        assert!(uninstall.contains("leaving files untouched"));
        assert!(!uninstall.contains("is-active --quiet"));
        assert!(!uninstall.contains("is-enabled --quiet"));
        assert!(
            !install
                .lines()
                .any(|line| line.starts_with("systemctl --user start "))
        );
        assert!(uninstall.contains("cmp -s \"$source_unit\" \"$target\""));
        assert!(uninstall.contains("user manager resolves a different unit"));
        assert!(uninstall.contains("unit changed during uninstall"));
        assert!(uninstall.contains("systemctl --user stop"));
    }

    #[cfg(unix)]
    #[test]
    fn generated_scripts_only_remove_the_exact_managed_unit() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let root = std::env::temp_dir().join(format!(
            "amaterasu-linux-user-service-script-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        let bundle = root.join("bundle");
        let home = root.join("home");
        let bin = root.join("bin");
        fs::create_dir_all(&bundle).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();

        let unit = render_service_unit(
            Path::new("/home/user/bin/amaterasu"),
            Path::new("/home/user/.local/share/amaterasu"),
            &"a".repeat(64),
            &"b".repeat(64),
        )
        .unwrap();
        fs::write(bundle.join(UNIT_FILE), &unit).unwrap();
        fs::write(bundle.join("install-user-service.sh"), install_script()).unwrap();
        fs::write(bundle.join("uninstall-user-service.sh"), uninstall_script()).unwrap();

        let systemctl = bin.join("systemctl");
        fs::write(
            &systemctl,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$SYSTEMCTL_LOG"
if [ "$*" = "--user show --property=FragmentPath --value amaterasu-linux-session.service" ]; then
    if [ "$SYSTEMD_FRAGMENT" = "$SYSTEMD_EXPECTED_FRAGMENT" ] && [ ! -e "$SYSTEMD_UNIT_PATH" ]; then exit 1; fi
    printf '%s\n' "$SYSTEMD_FRAGMENT"
elif [ "$*" = "--user show --no-pager --property=ActiveState --property=UnitFileState amaterasu-linux-session.service" ]; then
    status=$(cat "$QUERY_STATUS_FILE")
    [ "$status" -eq 0 ] || exit "$status"
    [ ! -f "$DUPLICATE_STATUS_FILE" ] || printf 'ActiveState=\n'
    printf 'ActiveState=%s\nUnitFileState=%s\n' "$(cat "$ACTIVE_STATE_FILE")" "$(cat "$IS_ENABLED_STATUS_FILE")"
elif [ "$*" = "--user enable amaterasu-linux-session.service" ]; then
    mkdir -p "$(dirname -- "$SYSTEMD_WANTED_LINK")"
    [ -L "$SYSTEMD_WANTED_LINK" ] || ln -s ../amaterasu-linux-session.service "$SYSTEMD_WANTED_LINK"
    [ "${FAIL_ENABLE:-0}" = 1 ] && exit 1
elif [ "$*" = "--user disable amaterasu-linux-session.service" ]; then
    rm -f -- "$SYSTEMD_WANTED_LINK"
fi
exit 0
"#,
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
        let fake_release = bin.join("amaterasu-release");
        fs::write(
            &fake_release,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$RELEASE_LOG"
count=0
[ ! -f "$PREFLIGHT_COUNT_FILE" ] || count=$(cat "$PREFLIGHT_COUNT_FILE")
count=$((count + 1))
printf '%s\n' "$count" > "$PREFLIGHT_COUNT_FILE"
fail_at=0
[ ! -f "$PREFLIGHT_FAIL_AT_FILE" ] || fail_at=$(cat "$PREFLIGHT_FAIL_AT_FILE")
[ "${FAIL_PREFLIGHT:-0}" = 0 ] && [ "$count" -ne "$fail_at" ]
"#,
        )
        .unwrap();
        fs::set_permissions(&fake_release, fs::Permissions::from_mode(0o700)).unwrap();
        let fake_id = bin.join("id");
        fs::write(
            &fake_id,
            "#!/bin/sh\n[ \"$1\" = -u ] && { echo 1000; exit 0; }\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&fake_id, fs::Permissions::from_mode(0o700)).unwrap();

        let log = root.join("systemctl.log");
        let release_log = root.join("release.log");
        let preflight_count = root.join("preflight-count");
        let preflight_fail_at = root.join("preflight-fail-at");
        let query_status = root.join("query-status");
        let duplicate_status = root.join("duplicate-status");
        let active_state = root.join("active-state");
        let enabled_status = root.join("enabled-status");
        fs::write(&preflight_fail_at, "0").unwrap();
        fs::write(&query_status, "0").unwrap();
        fs::write(&active_state, "inactive").unwrap();
        fs::write(&enabled_status, "enabled").unwrap();
        let user_dir = home.join(".config/systemd/user");
        let installed = user_dir.join(UNIT_FILE);
        let wanted_link = user_dir
            .join("graphical-session.target.wants")
            .join(UNIT_FILE);
        let expected_fragment = home
            .canonicalize()
            .unwrap()
            .join(".config/systemd/user")
            .join(UNIT_FILE)
            .to_string_lossy()
            .into_owned();
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let run_script = |name: &str, fragment: &str, fail_enable: bool| {
            Command::new("sh")
                .arg(bundle.join(name))
                .args([root.join("runtime"), root.join("amaterasu")])
                .env("HOME", &home)
                .env("PATH", &path)
                .env("SYSTEMCTL_LOG", &log)
                .env("RELEASE_LOG", &release_log)
                .env("PREFLIGHT_COUNT_FILE", &preflight_count)
                .env("PREFLIGHT_FAIL_AT_FILE", &preflight_fail_at)
                .env("QUERY_STATUS_FILE", &query_status)
                .env("DUPLICATE_STATUS_FILE", &duplicate_status)
                .env("ACTIVE_STATE_FILE", &active_state)
                .env("IS_ENABLED_STATUS_FILE", &enabled_status)
                .env("SYSTEMD_FRAGMENT", fragment)
                .env("SYSTEMD_EXPECTED_FRAGMENT", &expected_fragment)
                .env("SYSTEMD_UNIT_PATH", &installed)
                .env("SYSTEMD_WANTED_LINK", &wanted_link)
                .env("FAIL_ENABLE", if fail_enable { "1" } else { "0" })
                .status()
                .unwrap()
        };

        assert!(
            !Command::new("sh")
                .arg(bundle.join("install-user-service.sh"))
                .args([root.join("runtime"), root.join("amaterasu")])
                .env("HOME", &home)
                .env("PATH", &path)
                .env("SYSTEMCTL_LOG", &log)
                .env("RELEASE_LOG", &release_log)
                .env("PREFLIGHT_COUNT_FILE", &preflight_count)
                .env("PREFLIGHT_FAIL_AT_FILE", &preflight_fail_at)
                .env("QUERY_STATUS_FILE", &query_status)
                .env("DUPLICATE_STATUS_FILE", &duplicate_status)
                .env("ACTIVE_STATE_FILE", &active_state)
                .env("IS_ENABLED_STATUS_FILE", &enabled_status)
                .env("FAIL_PREFLIGHT", "1")
                .status()
                .unwrap()
                .success()
        );
        assert!(
            !log.exists(),
            "preflight failure must precede systemctl queries"
        );

        let other_unit = root
            .join("other-user-unit.service")
            .to_string_lossy()
            .into_owned();
        assert!(!run_script("install-user-service.sh", &other_unit, false).success());
        assert!(!installed.exists());

        assert!(!run_script("install-user-service.sh", &expected_fragment, true).success());
        assert!(!installed.exists());
        assert!(!wanted_link.exists());

        fs::write(&log, "").unwrap();
        fs::write(&preflight_count, "0").unwrap();
        fs::write(&preflight_fail_at, "2").unwrap();
        assert!(!run_script("install-user-service.sh", &expected_fragment, false).success());
        assert!(!installed.exists());
        assert!(!wanted_link.exists());
        assert!(!fs::read_to_string(&log).unwrap().contains("--user enable "));

        fs::write(&log, "").unwrap();
        fs::write(&preflight_count, "0").unwrap();
        fs::write(&preflight_fail_at, "3").unwrap();
        assert!(!run_script("install-user-service.sh", &expected_fragment, false).success());
        assert!(!installed.exists());
        assert!(!wanted_link.exists());
        fs::write(&preflight_fail_at, "0").unwrap();

        assert!(run_script("install-user-service.sh", &expected_fragment, false).success());
        assert!(run_script("install-user-service.sh", &expected_fragment, false).success());
        assert_eq!(fs::read(&installed).unwrap(), unit.as_bytes());
        assert!(wanted_link.is_symlink());
        assert!(!fs::read_to_string(&log).unwrap().contains("start "));

        let unrelated_unit = user_dir.join("other.service");
        fs::write(&unrelated_unit, "unrelated content").unwrap();
        let unrelated_link = user_dir
            .join("graphical-session.target.wants")
            .join("other.service");
        std::os::unix::fs::symlink("../other.service", &unrelated_link).unwrap();

        fs::write(&query_status, "5").unwrap();
        assert!(!run_script("uninstall-user-service.sh", &expected_fragment, false).success());
        assert!(installed.exists());
        assert!(wanted_link.is_symlink());
        fs::write(&query_status, "0").unwrap();
        fs::write(&active_state, "unknown").unwrap();
        assert!(!run_script("uninstall-user-service.sh", &expected_fragment, false).success());
        assert!(installed.exists());
        assert!(wanted_link.is_symlink());
        fs::write(&active_state, "inactive").unwrap();
        fs::write(&duplicate_status, "1").unwrap();
        assert!(!run_script("uninstall-user-service.sh", &expected_fragment, false).success());
        assert!(installed.exists());
        assert!(wanted_link.is_symlink());
        fs::remove_file(&duplicate_status).unwrap();
        let manager_calls = fs::read_to_string(&log).unwrap();
        assert!(!manager_calls.contains("--user stop "));
        assert!(!manager_calls.contains("--user disable "));

        assert!(!run_script("uninstall-user-service.sh", &other_unit, false).success());
        assert!(installed.exists());
        assert!(wanted_link.is_symlink());
        let manager_calls = fs::read_to_string(&log).unwrap();
        assert!(!manager_calls.contains("--user stop "));
        assert!(!manager_calls.contains("--user disable "));

        fs::write(&installed, "unmanaged replacement").unwrap();
        assert!(!run_script("install-user-service.sh", &expected_fragment, false).success());
        assert_eq!(
            fs::read_to_string(&installed).unwrap(),
            "unmanaged replacement"
        );
        assert!(!run_script("uninstall-user-service.sh", &expected_fragment, false).success());
        assert_eq!(
            fs::read_to_string(&installed).unwrap(),
            "unmanaged replacement"
        );

        fs::write(&installed, &unit).unwrap();
        assert!(run_script("uninstall-user-service.sh", &expected_fragment, false).success());
        assert!(!installed.exists());
        assert!(!wanted_link.exists());
        assert_eq!(
            fs::read_to_string(&unrelated_unit).unwrap(),
            "unrelated content"
        );
        assert!(unrelated_link.is_symlink());
        assert!(!fs::read_to_string(&log).unwrap().contains("start "));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn active_digest_validator_rejects_non_sha256_paths() {
        assert!(valid_digest(&"a".repeat(64)));
        assert!(!valid_digest("../bootstrap.json"));
        assert!(!valid_digest(&"g".repeat(64)));
    }

    #[test]
    fn systemd_resolution_parser_handles_missing_and_loaded_units() {
        assert_eq!(
            parse_fragment_path("LoadState=not-found\nFragmentPath=\n").unwrap(),
            ("not-found".into(), None)
        );
        assert_eq!(
            parse_fragment_path("LoadState=loaded\nFragmentPath=/home/user/.config/systemd/user/amaterasu-linux-session.service\n").unwrap(),
            (
                "loaded".into(),
                Some("/home/user/.config/systemd/user/amaterasu-linux-session.service".into())
            )
        );
        assert!(parse_fragment_path("FragmentPath=/tmp/foreign.service\n").is_err());
        assert!(parse_fragment_path("LoadState=loaded\nLoadState=loaded\n").is_err());
    }

    #[test]
    fn manager_preflight_allows_only_the_bundle_unit_path() {
        let expected = "/home/user/.config/systemd/user/amaterasu-linux-session.service";
        assert!(validate_manager_resolution("not-found", None, expected, false).is_ok());
        assert!(validate_manager_resolution("loaded", Some(expected), expected, true).is_ok());
        assert!(
            validate_manager_resolution(
                "loaded",
                Some("/usr/lib/systemd/user/amaterasu-linux-session.service"),
                expected,
                true
            )
            .is_err()
        );
        assert!(validate_manager_resolution("loaded", None, expected, true).is_err());
        assert!(validate_manager_resolution("not-found", None, expected, true).is_err());
    }

    #[test]
    fn unit_listing_matches_the_exact_name_only() {
        assert!(listing_contains_unit(
            "amaterasu-linux-session.service disabled enabled\n"
        ));
        assert!(listing_contains_unit(
            "amaterasu-linux-session.service loaded active running\n"
        ));
        assert!(!listing_contains_unit(
            "amaterasu-linux-session.service.custom disabled enabled\n"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn user_unit_directory_chain_must_be_owned_and_not_group_or_world_writable() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = std::env::temp_dir().join(format!(
            "amaterasu-user-unit-chain-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        let home = root.join("home");
        let config = home.join(".config");
        fs::create_dir_all(&config).unwrap();
        let uid = fs::metadata(&home).unwrap().uid();
        assert!(validate_user_directory_chain(&config, &home, uid).is_ok());

        fs::set_permissions(&config, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(validate_user_directory_chain(&config, &home, uid).is_err());

        fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
        let link = home.join("linked-config");
        std::os::unix::fs::symlink(&config, &link).unwrap();
        assert!(validate_user_directory_chain(&link, &home, uid).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loader_checks_follow_signed_audio_and_capture_settings() {
        let mut settings = std::collections::BTreeMap::new();
        assert_eq!(
            runtime_needs(&settings).unwrap(),
            RuntimeNeeds {
                pulse: false,
                pipewire_portal: false
            }
        );

        settings.insert(
            "amaterasu.audio".into(),
            serde_json::json!({"enabled":true}),
        );
        assert_eq!(
            runtime_needs(&settings).unwrap(),
            RuntimeNeeds {
                pulse: true,
                pipewire_portal: false
            }
        );

        settings.insert(
            crate::profiles::linux_session::VIDEO_COMPONENT_ID.into(),
            serde_json::json!({"enabled":true,"capture_providers":["pipewire"]}),
        );
        assert!(runtime_needs(&settings).is_err());
        settings.insert(
            "amaterasu.capture-pipewire".into(),
            serde_json::json!({"enabled":true}),
        );
        assert_eq!(
            runtime_needs(&settings).unwrap(),
            RuntimeNeeds {
                pulse: true,
                pipewire_portal: true
            }
        );
    }

    #[test]
    fn portal_preflight_accepts_running_or_dbus_activatable_portals() {
        assert!(portal_name_owned(
            "org.freedesktop.portal.Desktop 1234 xdg-desktop-portal\norg.example.Other 5678 other\n"
        ));
        assert!(!portal_name_owned("org.example.Other 5678 other\n"));

        assert!(activatable_name_list_contains(
            "as 2 \"org.example.Other\" \"org.freedesktop.portal.Desktop\"",
            "org.freedesktop.portal.Desktop"
        ));
        assert!(!activatable_name_list_contains(
            "as 1 \"org.example.Other\"",
            "org.freedesktop.portal.Desktop"
        ));
        assert!(!activatable_name_list_contains(
            "s \"org.freedesktop.portal.Desktop\"",
            "org.freedesktop.portal.Desktop"
        ));
        assert!(!activatable_name_list_contains(
            "as 2 \"org.freedesktop.portal.Desktop\"",
            "org.freedesktop.portal.Desktop"
        ));
        assert!(!activatable_name_list_contains(
            "as 2 \"org.freedesktop.portal.Desktop\" unquoted",
            "org.freedesktop.portal.Desktop"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn portal_preflight_accepts_dbus_activation_without_starting_the_portal() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "amaterasu-portal-preflight-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        let busctl = root.join("busctl");
        fs::write(
            &busctl,
            r##"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname -- "$0")/calls"
case "$*" in
    "--user --no-pager --no-legend list") printf 'org.example.Other 1234 other\n' ;;
    "--user --no-pager call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ListActivatableNames") printf 'as 1 "org.freedesktop.portal.Desktop"\n' ;;
    *) exit 9 ;;
esac
"##,
        )
        .unwrap();
        fs::set_permissions(&busctl, fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(
            require_portal_available_with(busctl.as_os_str()).unwrap(),
            PortalAvailability::Activatable
        );
        let calls = fs::read_to_string(root.join("calls")).unwrap();
        assert_eq!(calls.lines().count(), 2);
        assert!(calls.contains("ListActivatableNames"));
        assert!(!calls.contains("org.freedesktop.portal.Desktop /"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn loader_checks_follow_actual_linux_session_capture_settings() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-linux-session-preflight-settings-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        let disabled_policy = crate::profiles::linux_session::linux_session_policy(
            &root.join("disabled"),
            &crate::profiles::linux_session::options::LinuxSessionOptions::default(),
        )
        .unwrap();
        assert_eq!(
            runtime_needs(&disabled_policy.settings).unwrap(),
            RuntimeNeeds {
                pulse: false,
                pipewire_portal: false
            }
        );

        let enabled_policy = crate::profiles::linux_session::linux_session_policy(
            &root.join("enabled"),
            &crate::profiles::linux_session::options::LinuxSessionOptions {
                enable_capture: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            runtime_needs(&enabled_policy.settings).unwrap(),
            RuntimeNeeds {
                pulse: false,
                pipewire_portal: true
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn preflight_requires_runtime_environment_to_match_the_user_manager() {
        let mut current = std::collections::BTreeMap::new();
        current.insert("XDG_RUNTIME_DIR".into(), "/run/user/1000".into());
        current.insert(
            "PULSE_SERVER".into(),
            "unix:/run/user/1000/pulse/native".into(),
        );
        let manager =
            "XDG_RUNTIME_DIR=/run/user/1000\nPULSE_SERVER=unix:/run/user/1000/pulse/native\n";
        assert!(validate_manager_runtime_environment(manager, &current).is_ok());

        current.insert("PULSE_SERVER".into(), "unix:/tmp/other.sock".into());
        assert!(validate_manager_runtime_environment(manager, &current).is_err());
    }

    #[test]
    fn generated_read_only_preflight_script_does_not_install_or_start() {
        let script = preflight_script();
        assert!(script.contains("preflight-linux-user-service"));
        assert!(!script.contains("systemctl --user enable"));
        assert!(!script.contains("systemctl --user start"));
    }

    #[test]
    fn package_composition_must_match_exact_linux_session_component_set() {
        let manifests = PackageProfile::LinuxSession
            .components()
            .iter()
            .map(|component| ComponentManifest {
                service_provides: Vec::new(),
                service_requires: Vec::new(),
                id: component.id.into(),
                version: "0.1.0".into(),
                file: format!("{}.so", component.library),
                sha256: "0".repeat(64),
                size: 1,
                abi_major: 1,
                runtime: component.id == "amaterasu.runtime",
                requires: Default::default(),
                requested: Grants::default(),
            })
            .collect::<Vec<_>>();
        assert!(validate_linux_session_composition(&manifests).is_ok());
        assert!(validate_linux_session_composition(&manifests[..manifests.len() - 1]).is_err());
    }

    #[test]
    fn path_validation_rejects_relative_and_existing_output_paths() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-linux-user-service-path-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(new_output_path(Path::new("relative/output")).is_err());
        let output = root.join("bundle");
        assert_eq!(new_output_path(&output).unwrap(), output);
        fs::create_dir(&output).unwrap();
        assert!(new_output_path(&output).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn path_validation_rejects_symlinked_runtime_roots() {
        use std::os::unix::fs::symlink;

        let parent = std::env::temp_dir().join(format!(
            "amaterasu-linux-user-service-link-{}-{}",
            std::process::id(),
            random_identifier().unwrap()
        ));
        fs::create_dir_all(&parent).unwrap();
        let target = parent.join("real");
        fs::create_dir(&target).unwrap();
        let link = parent.join("alias");
        symlink(&target, &link).unwrap();
        assert!(existing_path(&link, "runtime root", true).is_err());
        let _ = fs::remove_dir_all(parent);
    }
}
