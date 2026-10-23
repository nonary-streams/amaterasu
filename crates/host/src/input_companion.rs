//! Machine-owned opt-in for the separately installed VHF companion. A signed
//! component set does not gain privileged input solely by selecting a backend.
use amaterasu_package::{PackageResult, read_json};
use serde::Deserialize;
use std::path::Path;

const POLICY_FILE: &str = "input-companion-policy.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompanionPolicy {
    schema: u32,
    component_set_digest: String,
    keyboard: bool,
    mouse: bool,
}

impl CompanionPolicy {
    pub(crate) fn load(root: &Path, selected_digest: &str) -> PackageResult<Option<Self>> {
        let path = root.join(POLICY_FILE);
        if !path.try_exists().map_err(|error| error.to_string())? {
            return Ok(None);
        }
        let policy: Self = read_json(&path)?;
        if policy.schema != 1
            || policy.component_set_digest != selected_digest
            || policy.component_set_digest.len() != 64
            || !policy
                .component_set_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !policy.keyboard && !policy.mouse
        {
            return Err(
                "input companion policy does not authorize this selected component set".into(),
            );
        }
        Ok(Some(policy))
    }

    pub(crate) fn authorize(&self, process_id: u32, creation_time: u64) -> PackageResult<()> {
        amaterasu_input_companion::authorize_worker(
            process_id,
            creation_time,
            &self.component_set_digest,
            self.keyboard,
            self.mouse,
        )
        .map_err(|error| format!("input companion rejected supervised worker: {error:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_only_authorizes_the_selected_component_set() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-companion-policy-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let selected = "a".repeat(64);
        assert!(CompanionPolicy::load(&root, &selected).unwrap().is_none());
        std::fs::write(
            root.join(POLICY_FILE),
            format!(
                "{{\"schema\":1,\"component_set_digest\":\"{selected}\",\"keyboard\":true,\"mouse\":false}}"
            ),
        )
        .unwrap();
        assert!(CompanionPolicy::load(&root, &selected).unwrap().is_some());
        assert!(CompanionPolicy::load(&root, &"b".repeat(64)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
