//! Reviewable systemd --user service artifacts for an already active, signed
//! LinuxSession package. Generation never installs or starts the unit.

use crate::profiles::PackageProfile;
use amaterasu_package::{
    ActivationState, BootstrapConfig, PackageResult, VerificationPolicy, create_private_directory,
    current_target, package_directory, random_identifier, read_bounded, sha256, unix_timestamp,
    verify_package,
};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

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
    if !package_path.starts_with(&runtime_root.join("sets")) {
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
    if sha256(&read_bounded(&selection_path, 1024 * 1024)?) != sha256(&selection_bytes)
        || sha256(&read_bounded(&bootstrap_path, 1024 * 1024)?) != sha256(&bootstrap_bytes)
    {
        return Err(
            "runtime trust or active selection changed during service artifact preparation".into(),
        );
    }

    let service_unit = render_service_unit(&host_executable, &runtime_root, active_digest)?;
    let artifacts = [
        (UNIT_FILE, service_unit),
        ("install-user-service.sh", install_script().into()),
        ("uninstall-user-service.sh", uninstall_script().into()),
        ("README.md", render_readme(active_digest).into()),
    ];
    write_artifact_directory(&output_directory, &artifacts)?;
    println!(
        "Prepared user service artifact for signed LinuxSession package {} at {}.",
        verified.digest,
        output_directory.display()
    );
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

fn render_service_unit(executable: &Path, root: &Path, digest: &str) -> PackageResult<String> {
    let executable = systemd_quote(executable)?;
    let root = systemd_quote(root)?;
    Ok(format!(
        "[Unit]\nDescription=Amaterasu LinuxSession ({})\nAfter=graphical-session.target\nPartOf=graphical-session.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart={executable} service-user {root} {digest}\nRestart=on-failure\nRestartSec=5s\nKillSignal=SIGTERM\nKillMode=mixed\nTimeoutStopSec=20s\n\n[Install]\nWantedBy=graphical-session.target\n",
        &digest[..12]
    ))
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
bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
source_unit="$bundle_dir/$unit_name"
[ -f "$source_unit" ] && [ ! -L "$source_unit" ] || { echo "service artifact is missing or unsafe" >&2; exit 1; }
[ "$(id -u)" -ne 0 ] || { echo "refusing to install a user service as root" >&2; exit 1; }
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
rollback_new_unit() {
    if [ "$created" -eq 1 ] && [ -f "$target" ] && [ ! -L "$target" ] && cmp -s "$source_unit" "$target"; then
        if [ "$wanted_preexisting" -eq 0 ] && [ -L "$wanted_link" ] && [ "$(readlink -- "$wanted_link")" = "../$unit_name" ]; then
            rm -- "$wanted_link"
        fi
        rm -- "$target"
        systemctl --user daemon-reload >/dev/null 2>&1 || true
    fi
}
if ! systemctl --user daemon-reload; then
    rollback_new_unit
    echo "systemd user manager could not reload; new unit rolled back" >&2
    exit 1
fi
if ! systemctl --user enable "$unit_name"; then
    rollback_new_unit
    echo "systemd user manager could not enable the unit; new unit rolled back" >&2
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
if systemctl --user is-active --quiet "$unit_name"; then
    systemctl --user stop "$unit_name"
fi
if systemctl --user is-enabled --quiet "$unit_name"; then
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

fn render_readme(digest: &str) -> String {
    format!(
        "# Amaterasu LinuxSession user service\n\nThis bundle was generated for signed active package `{digest}`. The unit runs under the current user manager and never requests root or system service privileges. Its `ExecStart` pins this digest; a different active package fails closed until a new bundle is generated and reviewed.\n\nReview `amaterasu-linux-session.service`, then install it for this user with `sh install-user-service.sh`. Installation enables the unit for `graphical-session.target` but does not start it. Start it explicitly with `systemctl --user start amaterasu-linux-session.service`, or let the graphical session start it.\n\nThe host verifies the production bootstrap, trusted package signature, active selection, and pinned digest on every start. A module tick or health failure exits the service; systemd restarts it after a fixed five-second delay and the next start re-verifies the same digest. SIGTERM triggers orderly component teardown and Linux input revocation. A forced kill is recovered by the same fresh verification and load on restart.\n\nUninstall from this same bundle with `sh uninstall-user-service.sh`. It stops and disables the user unit only after verifying that the installed unit bytes match this bundle, rechecks before removing the file, and leaves runtime data and the signed package intact. If the unit was edited or replaced, uninstall refuses to remove it.\n\nThe generated scripts affect only the current user service directory selected by `XDG_CONFIG_HOME` (or `$HOME/.config`). They refuse root execution and symlinked service paths. No service was installed or started when this bundle was generated.\n"
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
        )
        .unwrap();
        assert!(unit.contains("ExecStart=\"/home/user/bin/amaterasu\" service-user \"/home/user/.local/share/amaterasu\""));
        assert!(unit.contains(&"a".repeat(64)));
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

    #[test]
    fn installer_and_uninstaller_guard_unmanaged_content_by_exact_bytes() {
        let install = install_script();
        let uninstall = uninstall_script();
        assert!(install.contains("cmp -s \"$source_unit\" \"$target\""));
        assert!(install.contains("refusing to overwrite unmanaged service content"));
        assert!(install.contains("systemctl --user enable"));
        assert!(!install.contains("systemctl --user start"));
        assert!(uninstall.contains("cmp -s \"$source_unit\" \"$target\""));
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
        )
        .unwrap();
        fs::write(bundle.join(UNIT_FILE), &unit).unwrap();
        fs::write(bundle.join("install-user-service.sh"), install_script()).unwrap();
        fs::write(bundle.join("uninstall-user-service.sh"), uninstall_script()).unwrap();

        let systemctl = bin.join("systemctl");
        fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SYSTEMCTL_LOG\"\ncase \"$*\" in 'is-active --quiet '*|'is-enabled --quiet '*) exit 1 ;; esac\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
        let fake_id = bin.join("id");
        fs::write(
            &fake_id,
            "#!/bin/sh\n[ \"$1\" = -u ] && { echo 1000; exit 0; }\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&fake_id, fs::Permissions::from_mode(0o700)).unwrap();

        let log = root.join("systemctl.log");
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let run_script = |name: &str| {
            Command::new("sh")
                .arg(bundle.join(name))
                .env("HOME", &home)
                .env("PATH", &path)
                .env("SYSTEMCTL_LOG", &log)
                .status()
                .unwrap()
        };

        assert!(run_script("install-user-service.sh").success());
        let installed = home.join(".config/systemd/user").join(UNIT_FILE);
        assert_eq!(fs::read(&installed).unwrap(), unit.as_bytes());
        assert!(!fs::read_to_string(&log).unwrap().contains("start "));

        fs::write(&installed, "unmanaged replacement").unwrap();
        assert!(!run_script("install-user-service.sh").success());
        assert_eq!(
            fs::read_to_string(&installed).unwrap(),
            "unmanaged replacement"
        );
        assert!(!run_script("uninstall-user-service.sh").success());
        assert_eq!(
            fs::read_to_string(&installed).unwrap(),
            "unmanaged replacement"
        );

        fs::write(&installed, &unit).unwrap();
        assert!(run_script("uninstall-user-service.sh").success());
        assert!(!installed.exists());
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
    fn package_composition_must_match_exact_linux_session_component_set() {
        let manifests = PackageProfile::LinuxSession
            .components()
            .iter()
            .map(|component| ComponentManifest {
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
