use serde::{Deserialize, Serialize};
use url::Url;

/// The persisted global preference consumed by the tray runtime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraySettings {
    pub system_tray: bool,
}

impl Default for TraySettings {
    fn default() -> Self {
        Self { system_tray: true }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Stopped,
    Playing,
    Pausing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityUpdate {
    pub state: ActivityState,
    pub app_name: String,
}

impl ActivityUpdate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.app_name.is_empty()
            || self.app_name.len() > 128
            || self.app_name.chars().any(char::is_control)
        {
            return Err("invalid_app_name");
        }
        Ok(())
    }
}

/// A validated update notice safe to pass to a native desktop notification API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateNotice {
    pub version: String,
    pub release_url: String,
}

impl UpdateNotice {
    pub fn parse(version: String, release_url: String) -> Result<Self, &'static str> {
        validate_version(&version)?;
        validate_release_url(&release_url)?;
        Ok(Self {
            version,
            release_url,
        })
    }
}

fn validate_version(version: &str) -> Result<(), &'static str> {
    if version.is_empty()
        || version.len() > 128
        || !version.bytes().all(|byte| (b' '..=b'~').contains(&byte))
    {
        return Err("invalid_version");
    }
    Ok(())
}

fn validate_release_url(release_url: &str) -> Result<(), &'static str> {
    if release_url.is_empty()
        || release_url.len() > 2048
        || release_url.chars().any(char::is_control)
    {
        return Err("invalid_release_url");
    }

    const RELEASE_PREFIX: &str = "https://github.com/Nonary/amaterasu/releases/";
    let Some(release_path) = release_url.strip_prefix(RELEASE_PREFIX) else {
        return Err("invalid_release_url");
    };
    if release_path.is_empty() {
        return Err("invalid_release_url");
    }
    let Ok(url) = Url::parse(release_url) else {
        return Err("invalid_release_url");
    };
    let path_segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();

    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || path_segments.len() < 4
        || path_segments[0] != "Nonary"
        || path_segments[1] != "amaterasu"
        || path_segments[2] != "releases"
        || path_segments[3].is_empty()
    {
        return Err("invalid_release_url");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_defaults_match_the_pinned_global_setting() {
        assert!(TraySettings::default().system_tray);
    }

    #[test]
    fn accepts_the_updaters_canonical_release_page() {
        let notice = UpdateNotice::parse(
            "v2.0.0-beta.1".into(),
            "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0-beta.1".into(),
        )
        .unwrap();

        assert_eq!(notice.version, "v2.0.0-beta.1");
        assert!(
            UpdateNotice::parse(
                "v2.0.0 release".into(),
                "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0".into(),
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_untrusted_or_malformed_release_urls() {
        for url in [
            "http://github.com/Nonary/amaterasu/releases/tag/v2.0.0",
            "https://github.com.evil.test/Nonary/amaterasu/releases/tag/v2.0.0",
            "https://evil.test/Nonary/amaterasu/releases/tag/v2.0.0",
            "https://user@github.com/Nonary/amaterasu/releases/tag/v2.0.0",
            "https://github.com/Other/amaterasu/releases/tag/v2.0.0",
            "https://github.com/Nonary/amaterasu/releases?tag=v2.0.0",
            "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0#notes",
            "https://github.com:443/Nonary/amaterasu/releases/tag/v2.0.0",
            "https://GitHub.com/Nonary/amaterasu/releases/tag/v2.0.0",
            "not a url",
        ] {
            assert_eq!(
                UpdateNotice::parse("v2.0.0".into(), url.into()).unwrap_err(),
                "invalid_release_url",
                "accepted {url}"
            );
        }
    }

    #[test]
    fn rejects_control_characters_and_oversized_update_fields() {
        assert_eq!(
            UpdateNotice::parse(
                "v2.0.0\ntrusted".into(),
                "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0".into(),
            )
            .unwrap_err(),
            "invalid_version"
        );
        assert_eq!(
            UpdateNotice::parse(
                "v2.0.0".into(),
                format!(
                    "https://github.com/Nonary/amaterasu/releases/tag/{}",
                    "x".repeat(2048)
                ),
            )
            .unwrap_err(),
            "invalid_release_url"
        );
    }

    #[test]
    fn tray_settings_reject_unknown_or_missing_fields() {
        for value in [
            serde_json::json!({"system_tray":true,"extra":false}),
            serde_json::json!({}),
            serde_json::json!({"system_tray":"true"}),
        ] {
            assert!(serde_json::from_value::<TraySettings>(value).is_err());
        }
    }
}
