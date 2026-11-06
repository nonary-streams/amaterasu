//! Artifact evidence only for the reserved settings-field request. This does
//! not perform native admission or establish actual capture readiness.
use amaterasu_package::{ComponentManifest, PackageResult, read_bounded};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
pub(crate) const MARKER: &[u8] = b"AMATERASU_CAPTURE_AUTHORITY_PRECHECK=1\0";
pub(crate) const COMPONENT: &str = "amaterasu.capture-vkms";
pub(crate) fn requested(settings: &BTreeMap<String, Value>) -> PackageResult<bool> {
    // The field is reserved across all component JSON, matching host adoption.
    // Packages never serialize an assertion of trusted launcher support.
    for value in settings.values() {
        if let Some(marker) = value.get("authority_precheck_supported")
            && marker != false
        {
            return Err("authority_precheck_supported must be a reserved false placeholder".into());
        }
    }
    let capture = settings.get(COMPONENT);
    let requested =
        capture.is_some_and(|capture| capture.get("authority_precheck_supported").is_some());
    if requested && !capture.is_some_and(|capture| capture["authority_channel"] == true) {
        return Err("capture authority precheck requires the package authority channel".into());
    }
    Ok(requested)
}
pub(crate) fn validate_artifact(artifact: &[u8]) -> PackageResult<()> {
    if artifact
        .windows(MARKER.len())
        .filter(|value| *value == MARKER)
        .count()
        != 1
    {
        return Err(
            "capture authority precheck requires a compatible capture artifact marker".into(),
        );
    }
    Ok(())
}
pub(crate) fn validate_package(
    package: &Path,
    components: &[ComponentManifest],
    settings: &BTreeMap<String, Value>,
) -> PackageResult<()> {
    if !requested(settings)? {
        return Ok(());
    }
    let capture = components
        .iter()
        .find(|component| component.id == COMPONENT)
        .ok_or("capture authority precheck component is absent")?;
    validate_artifact(&read_bounded(
        &package.join(&capture.file),
        256 * 1024 * 1024,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn support_request_requires_false_and_exact_unambiguous_artifact_evidence() {
        assert!(!requested(&BTreeMap::new()).unwrap());
        let mut settings = BTreeMap::from([(COMPONENT.into(), json!({"authority_channel":true}))]);
        assert!(!requested(&settings).unwrap());
        settings.get_mut(COMPONENT).unwrap()["authority_precheck_supported"] = json!(false);
        assert!(requested(&settings).unwrap());
        for invalid in [json!(true), json!(null), json!(0)] {
            settings.get_mut(COMPONENT).unwrap()["authority_precheck_supported"] = invalid;
            assert!(requested(&settings).is_err());
        }
        assert!(validate_artifact(MARKER).is_ok());
        for bytes in [
            b"old component".as_slice(),
            &MARKER[..MARKER.len() - 1],
            &[MARKER, MARKER].concat(),
        ] {
            assert!(validate_artifact(bytes).is_err());
        }
    }
}
