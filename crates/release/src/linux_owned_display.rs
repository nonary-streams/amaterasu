//! Optional owned-display artifact and KDE registration checks.
use amaterasu_package::{ComponentManifest, PackageResult, read_bounded};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
pub(crate) const DESKTOP_FILE: &str = "org.amaterasu.StreamingHost.desktop";
const DRIVER_MARKER: &[u8] = b"AMATERASU_LINUX_VDD=b2946d02a09bc453d9e6d287623390ca1dedf964\0";
const CAPTURE_MARKER: &[u8] = b"AMATERASU_KWIN_CAPTURE=1\0";
pub(crate) fn enabled(settings: &BTreeMap<String, Value>) -> bool {
    settings
        .get("amaterasu.capture-pipewire")
        .is_some_and(|setting| setting["allow_named_outputs"] == true)
}
pub(crate) fn validate(
    package: &Path,
    components: &[ComponentManifest],
    settings: &BTreeMap<String, Value>,
) -> PackageResult<()> {
    for (component, setting, marker) in [
        (
            "amaterasu.virtual-display",
            settings
                .get("amaterasu.virtual-display")
                .is_some_and(|setting| setting["enabled"] == true),
            DRIVER_MARKER,
        ),
        (
            "amaterasu.capture-pipewire",
            enabled(settings),
            CAPTURE_MARKER,
        ),
    ] {
        if !setting {
            continue;
        }
        let component = components
            .iter()
            .find(|entry| entry.id == component)
            .ok_or("owned display component is absent")?;
        validate_marker(
            &read_bounded(&package.join(&component.file), 256 * 1024 * 1024)?,
            marker,
        )?;
    }
    Ok(())
}
fn validate_marker(artifact: &[u8], marker: &[u8]) -> PackageResult<()> {
    if artifact
        .windows(marker.len())
        .filter(|bytes| *bytes == marker)
        .count()
        != 1
    {
        return Err(
            "owned-display profile requires the linked libvirtualdisplay and KWin capture features"
                .into(),
        );
    }
    Ok(())
}
pub(crate) fn desktop_entry(host: &Path) -> PackageResult<String> {
    let host = host
        .to_str()
        .filter(|host| {
            !host
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\' | '$' | '`' | '%'))
        })
        .ok_or("host path cannot be represented safely in a KDE desktop entry")?;
    if !Path::new(host).is_absolute() {
        return Err("KDE host path must be absolute".into());
    }
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Amaterasu Streaming Host\nNoDisplay=true\nExec=\"{host}\"\nX-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1\n"
    ))
}
pub(crate) fn install_script() -> &'static str {
    r#"#!/bin/sh
set -eu
[ "$(id -u)" -ne 0 ] || { echo "install the scoped KDE declaration as the desktop user" >&2; exit 1; }
[ -n "${HOME:-}" ] || exit 1
bundle=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
source="$bundle/org.amaterasu.StreamingHost.desktop"
[ -f "$source" ] && [ ! -L "$source" ] || exit 1
command -v kbuildsycoca6 >/dev/null 2>&1 || { echo "KDE service cache tool is unavailable" >&2; exit 1; }
data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}
case "$data_home/" in "$HOME/"*) ;; *) echo "KDE data directory must be under HOME" >&2; exit 1;; esac
case "$data_home/" in */../*|*/./*) exit 1;; esac
case "$data_home" in /*) ;; *) exit 1;; esac
current=
remaining=${data_home#/}/applications
while [ -n "$remaining" ]; do
    part=${remaining%%/*}
    if [ "$part" = "$remaining" ]; then remaining=; else remaining=${remaining#*/}; fi
    current="$current/$part"
    [ ! -L "$current" ] || { echo "KDE data path crosses a symbolic link" >&2; exit 1; }
done
mkdir -p "$data_home/applications"
target="$data_home/applications/org.amaterasu.StreamingHost.desktop"
if [ -e "$target" ] || [ -L "$target" ]; then
    [ -f "$target" ] && [ ! -L "$target" ] && cmp -s "$source" "$target" || { echo "existing KDE declaration differs; leaving it untouched" >&2; exit 1; }
else
    temporary=$(mktemp "$data_home/applications/.amaterasu-declaration.XXXXXX")
    trap 'rm -f -- "$temporary"' EXIT HUP INT TERM
    cat "$source" > "$temporary"
    chmod 644 "$temporary"
    ln "$temporary" "$target"
    rm -f -- "$temporary"
    trap - EXIT HUP INT TERM
fi
kbuildsycoca6 --noincremental
"#
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declarations_scope_the_interface_to_the_exact_host_and_reject_exec_metacharacters() {
        let entry = desktop_entry(Path::new("/home/test/host with spaces")).unwrap();
        assert!(entry.contains("Exec=\"/home/test/host with spaces\""));
        assert!(entry.contains("X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1"));
        for path in [
            "relative/host",
            "/tmp/host%u",
            "/tmp/host\"",
            "/tmp/host$evil",
            "/tmp/host\n",
        ] {
            assert!(desktop_entry(Path::new(path)).is_err());
        }
    }
    #[test]
    fn linked_feature_marker_must_be_present_exactly_once() {
        assert!(validate_marker(DRIVER_MARKER, DRIVER_MARKER).is_ok());
        assert!(validate_marker(b"unmarked artifact", DRIVER_MARKER).is_err());
        assert!(validate_marker(&[DRIVER_MARKER, DRIVER_MARKER].concat(), DRIVER_MARKER).is_err());
    }
}
