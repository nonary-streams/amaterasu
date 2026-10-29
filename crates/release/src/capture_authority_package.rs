//! Publisher-side construction of a narrowly scoped capture authority request.
use amaterasu_package::{
    MAX_PRIVILEGED_HELPER_BYTES, PackageResult, PrivilegedHelperArtifact, PrivilegedOperation,
    PrivilegedRequest, PrivilegedResourceScope, current_target, read_bounded, sha256,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SPEC_FILE: &str = "capture-authority-package.json";
const HELPER_FILE: &str = "amaterasu-vkms-frame-helper";

/// Local publisher input only. It is never interpreted as approved authority.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSpec {
    pub schema: u32,
    pub helper_source: PathBuf,
    pub drm_card: u32,
}

pub fn prepare(spec: &PackageSpec, output: &Path) -> PackageResult<PrivilegedRequest> {
    if spec.schema != 1 || spec.drm_card > 63 || !spec.helper_source.is_absolute() {
        return Err("invalid local capture authority package specification".into());
    }
    if !cfg!(target_os = "linux") {
        return Err("capture authority packages are currently supported only on Linux".into());
    }
    let helper = read_bounded(&spec.helper_source, MAX_PRIVILEGED_HELPER_BYTES)?;
    if helper.is_empty() {
        return Err("capture authority helper must not be empty".into());
    }
    let request = PrivilegedRequest {
        schema: 1,
        component: "amaterasu.capture-vkms".into(),
        operation: PrivilegedOperation::VkmsFrameExportV1,
        target: current_target(),
        resource: PrivilegedResourceScope {
            drm_card: spec.drm_card,
        },
        helper: PrivilegedHelperArtifact {
            file: HELPER_FILE.into(),
            sha256: sha256(&helper),
            size: helper.len() as u64,
        },
    };
    // This artifact has no ambient file capability and is not made executable
    // here. Only the protected broker deployment may execute its approved copy.
    std::fs::write(output.join(HELPER_FILE), helper).map_err(|error| error.to_string())?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_publisher_specification_cannot_create_an_artifact() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-authority-spec-{}", std::process::id()));
        let invalid = PackageSpec {
            schema: 2,
            helper_source: PathBuf::from("helper"),
            drm_card: 64,
        };
        assert!(prepare(&invalid, &root).is_err());
        assert!(!root.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn package_helper_request_binds_exact_bytes_and_card() {
        let root =
            std::env::temp_dir().join(format!("amaterasu-authority-helper-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let output = root.join("package");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(&source, b"capture helper fixture").unwrap();
        let spec = PackageSpec {
            schema: 1,
            helper_source: source,
            drm_card: 5,
        };
        let request = prepare(&spec, &output).unwrap();
        assert_eq!(request.resource.drm_card, 5);
        assert_eq!(request.helper.sha256, sha256(b"capture helper fixture"));
        assert_eq!(request.helper.size, 22);
        assert_eq!(
            std::fs::read(output.join(HELPER_FILE)).unwrap(),
            b"capture helper fixture"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
