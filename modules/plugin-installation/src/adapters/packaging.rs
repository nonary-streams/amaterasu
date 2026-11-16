//! Publisher-side declarations for the selected built-in backend. This only
//! reads build artifacts; it never invokes detection or installs anything.
use super::{Descriptor, Kind};
use amaterasu_module_sdk::drivers::{Artifact, DriverRequirement};
use amaterasu_package::{
    PackageResult,
    plugins::{validate_artifact, validate_requirement},
};
use std::path::Path;
fn feature(
    kind: &Kind,
    selected: &str,
) -> PackageResult<Option<(&'static str, &'static str, &'static str)>> {
    match (kind, selected) {
        (Kind::WindowsVigem, "input.keyboard" | "input.mouse" | "input.controller.auto") => {
            Ok(None)
        }
        (Kind::WindowsVigem, "input.controller.x360" | "input.controller.ds4") => Ok(Some((
            "org.nefarius.vigem",
            "input.controller.vigem",
            "ViGEm controller support",
        ))),
        (Kind::LinuxInput, "input.uinput") => Ok(Some((
            "linux.uinput",
            "input.uinput",
            "Linux input device access",
        ))),
        (Kind::LinuxInput, "input.uhid") => Ok(Some((
            "linux.uhid",
            "input.uhid",
            "Linux virtual controller access",
        ))),
        (Kind::WindowsVirtualDisplay | Kind::LinuxVirtualDisplay, "display.virtual") => Ok(Some((
            "org.nonary.virtual-display",
            "display.virtual",
            "Virtual display support",
        ))),
        _ => Err("driver_backend_feature_unsupported".into()),
    }
}
pub fn requirement(
    selected: &str,
    directory: &Path,
    helper: &Path,
) -> PackageResult<Option<DriverRequirement>> {
    let descriptor: Descriptor =
        amaterasu_package::read_json(&directory.join("driver-adapter.json"))?;
    let Some((id, feature, name)) = feature(&descriptor.kind, selected)? else {
        return Ok(None);
    };
    let installer = artifact(helper)?;
    let mut payloads = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("driver_payload_directory_not_flat".into());
        }
        let file = artifact(&entry.path())?;
        if file != installer {
            payloads.push(file);
        }
        if payloads.len() > 32 {
            return Err("driver_too_many_payloads".into());
        }
    }
    payloads.sort_by(|a, b| a.file.cmp(&b.file));
    let requirement = DriverRequirement {
        schema: 1,
        id: id.into(),
        name: name.into(),
        target: amaterasu_package::current_target(),
        feature: feature.into(),
        reason: format!("Required by the selected {selected} feature"),
        compatibility: amaterasu_package::sha256(
            &serde_json::to_vec(&descriptor).map_err(|e| e.to_string())?,
        ),
        may_require_reboot: !matches!(descriptor.kind, Kind::LinuxInput),
        offered_version: {
            let offered = amaterasu_module_sdk::drivers::OfferedVersion {
                version: descriptor.version.clone(),
                scheme: amaterasu_module_sdk::drivers::VersionScheme::Numeric,
            };
            offered.validate().then_some(offered)
        },
        installer,
        payloads,
    };
    validate_requirement(&requirement)?;
    Ok(Some(requirement))
}
fn artifact(path: &Path) -> PackageResult<Artifact> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("driver_payload_not_regular".into());
    }
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("driver_payload_name_invalid")?
        .into();
    let bytes =
        amaterasu_package::read_bounded(path, amaterasu_package::plugins::MAX_ARTIFACT_BYTES)?;
    let artifact = Artifact {
        file,
        sha256: amaterasu_package::sha256(&bytes),
        size: bytes.len() as u64,
    };
    validate_artifact(&artifact)?;
    Ok(artifact)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_backends_do_not_gate_driver_independent_input() {
        for selected in ["input.keyboard", "input.mouse", "input.controller.auto"] {
            assert!(feature(&Kind::WindowsVigem, selected).unwrap().is_none());
        }
        let xbox = feature(&Kind::WindowsVigem, "input.controller.x360")
            .unwrap()
            .unwrap();
        let ds4 = feature(&Kind::WindowsVigem, "input.controller.ds4")
            .unwrap()
            .unwrap();
        assert_eq!(xbox, ds4);
        assert_ne!(
            feature(&Kind::LinuxInput, "input.uinput").unwrap(),
            feature(&Kind::LinuxInput, "input.uhid").unwrap()
        );
        assert!(feature(&Kind::WindowsVigem, "input.vhf").is_err());
    }
}
