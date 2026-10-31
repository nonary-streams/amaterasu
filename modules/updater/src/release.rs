use crate::model::Settings;
use amaterasu_module_sdk::{network::Network, network_http};
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::time::Duration;
use ureq::Agent;

const RELEASES_ENDPOINT: &str =
    "https://api.github.com/repos/Nonary/amaterasu/releases?per_page=100";
const RELEASE_URL_PREFIX: &str = "https://github.com/Nonary/amaterasu/releases/tag/";
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RELEASES: usize = 100;
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub url: String,
    pub prerelease: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseSet {
    pub stable: Option<Release>,
    pub prerelease: Option<Release>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvailableUpdate {
    pub tag: String,
    pub url: String,
    pub prerelease: bool,
}

#[derive(Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

pub trait ReleaseSource {
    fn fetch(&mut self) -> Result<ReleaseSet, String>;
}

#[cfg(test)]
pub trait NotificationSink {
    fn notify(&mut self, update: &AvailableUpdate) -> NotificationDelivery;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationDelivery {
    Notified,
    Disabled,
    Unavailable,
}

pub fn record_notification(
    delivered: &mut BTreeSet<(String, bool)>,
    update: &AvailableUpdate,
    outcome: NotificationDelivery,
) -> bool {
    if outcome != NotificationDelivery::Notified {
        return false;
    }
    delivered.insert((update.tag.clone(), update.prerelease))
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckOutcome {
    pub releases: ReleaseSet,
    pub available: Option<AvailableUpdate>,
    pub notification: Option<NotificationDelivery>,
}

#[cfg(test)]
pub fn check_with_providers(
    source: &mut impl ReleaseSource,
    notifications: &mut impl NotificationSink,
    installed_version: &Version,
    settings: &Settings,
) -> Result<CheckOutcome, String> {
    let releases = source.fetch()?;
    let available = select_available_update(&releases, installed_version, settings);
    let notification = available
        .as_ref()
        .map(|update| notifications.notify(update));
    Ok(CheckOutcome {
        releases,
        available,
        notification,
    })
}

pub fn select_available_update(
    releases: &ReleaseSet,
    installed_version: &Version,
    settings: &Settings,
) -> Option<AvailableUpdate> {
    let allow_prereleases = settings.notify_pre_releases || !installed_version.pre.is_empty();
    let stable_is_newer = releases
        .stable
        .as_ref()
        .is_some_and(|stable| stable.version > *installed_version);
    let prerelease_is_newer_than_stable = releases.prerelease.as_ref().is_some_and(|prerelease| {
        releases
            .stable
            .as_ref()
            .is_none_or(|stable| prerelease.version > stable.version)
    });
    let newer_prerelease = allow_prereleases
        .then_some(releases.prerelease.as_ref())
        .flatten()
        .filter(|prerelease| {
            prerelease.version > *installed_version && prerelease_is_newer_than_stable
        });

    newer_prerelease
        .map(|release| AvailableUpdate {
            tag: release.tag.clone(),
            url: release.url.clone(),
            prerelease: true,
        })
        .or_else(|| {
            stable_is_newer.then(|| {
                let release = releases.stable.as_ref().expect("newer stable is present");
                AvailableUpdate {
                    tag: release.tag.clone(),
                    url: release.url.clone(),
                    prerelease: false,
                }
            })
        })
}

pub fn notification_delivery(payload: &serde_json::Value) -> NotificationDelivery {
    match payload
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .filter(|ok| *ok)
        .and_then(|_| payload.get("delivery"))
        .and_then(serde_json::Value::as_str)
    {
        Some("notified") => NotificationDelivery::Notified,
        Some("disabled") => NotificationDelivery::Disabled,
        _ => NotificationDelivery::Unavailable,
    }
}

pub struct GitHubReleaseSource {
    agent: Agent,
}

impl GitHubReleaseSource {
    pub fn new(network: Network) -> Self {
        let configuration = Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .build();
        Self {
            agent: network_http::agent(configuration, network),
        }
    }

    fn fetch_endpoint(&mut self, endpoint: &str) -> Result<ReleaseSet, String> {
        let mut response = self
            .agent
            .get(endpoint)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Amaterasu-Updater/1.0")
            .call()
            .map_err(|_| "release service request failed".to_owned())?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_RESPONSE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| "release service response could not be read".to_owned())?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("release service response exceeds its size limit".into());
        }
        parse_release_feed(&bytes)
    }
}

impl ReleaseSource for GitHubReleaseSource {
    fn fetch(&mut self) -> Result<ReleaseSet, String> {
        self.fetch_endpoint(RELEASES_ENDPOINT)
    }
}

pub fn parse_release_feed(bytes: &[u8]) -> Result<ReleaseSet, String> {
    let api_releases: Vec<ApiRelease> = serde_json::from_slice(bytes)
        .map_err(|_| "release service response is invalid".to_owned())?;
    if api_releases.len() > MAX_RELEASES {
        return Err("release service returned too many releases".into());
    }
    let mut releases = ReleaseSet {
        stable: None,
        prerelease: None,
    };
    for api_release in api_releases {
        if api_release.draft {
            continue;
        }
        let Some(version) = version_from_tag(&api_release.tag_name) else {
            continue;
        };
        if !is_release_url(&api_release.html_url) {
            continue;
        }
        let release = Release {
            tag: api_release.tag_name,
            version,
            url: api_release.html_url,
            prerelease: api_release.prerelease,
        };
        let latest = if release.prerelease {
            &mut releases.prerelease
        } else {
            &mut releases.stable
        };
        if latest
            .as_ref()
            .is_none_or(|current| release.version > current.version)
        {
            *latest = Some(release);
        }
    }
    Ok(releases)
}

fn version_from_tag(tag: &str) -> Option<Version> {
    if tag.is_empty()
        || tag.len() > 128
        || !tag.is_ascii()
        || tag
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return None;
    }
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

fn is_release_url(url: &str) -> bool {
    url.starts_with(RELEASE_URL_PREFIX)
        && url.len() <= 2048
        && url
            .bytes()
            .all(|byte| !byte.is_ascii_control() && !byte.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FakeSource {
        releases: ReleaseSet,
    }

    impl ReleaseSource for FakeSource {
        fn fetch(&mut self) -> Result<ReleaseSet, String> {
            Ok(self.releases.clone())
        }
    }

    struct FakeNotifications {
        delivered: NotificationDelivery,
        calls: Vec<AvailableUpdate>,
    }

    impl NotificationSink for FakeNotifications {
        fn notify(&mut self, update: &AvailableUpdate) -> NotificationDelivery {
            self.calls.push(update.clone());
            self.delivered
        }
    }

    fn release(tag: &str, prerelease: bool) -> Release {
        Release {
            tag: tag.into(),
            version: version_from_tag(tag).unwrap(),
            url: format!("{RELEASE_URL_PREFIX}{tag}"),
            prerelease,
        }
    }

    #[test]
    fn release_feed_http_uses_the_injected_network_provider() {
        use std::io::{Read, Write};

        let network = Network::test_provider();
        let listener = network.bind_tcp("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "HTTP client did not connect"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("HTTP test accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut bytes = [0u8; 1024];
                let length = stream.read(&mut bytes).unwrap();
                assert_ne!(length, 0, "HTTP client closed before headers");
                request.extend_from_slice(&bytes[..length]);
                assert!(request.len() <= 8192, "HTTP request exceeds header bound");
            }
            let request = std::str::from_utf8(&request).unwrap();
            assert!(request.starts_with("GET /releases HTTP/1.1\r\n"));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("user-agent: amaterasu-updater/1.0")
            );
            let body = br#"[{"tag_name":"v2.0.0","html_url":"https://github.com/Nonary/amaterasu/releases/tag/v2.0.0","prerelease":false,"draft":false}]"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let mut source = GitHubReleaseSource::new(network);
        let releases = source
            .fetch_endpoint(&format!("http://{address}/releases"))
            .unwrap();
        assert_eq!(releases.stable.unwrap().tag, "v2.0.0");
        server.join().unwrap();
    }

    #[test]
    fn parses_latest_non_draft_semantic_releases_and_rejects_foreign_links() {
        let feed = br#"[
            {"tag_name":"v2.0.0","html_url":"https://github.com/Nonary/amaterasu/releases/tag/v2.0.0","draft":false,"prerelease":false},
            {"tag_name":"v3.0.0-rc.1","html_url":"https://github.com/Nonary/amaterasu/releases/tag/v3.0.0-rc.1","draft":false,"prerelease":true},
            {"tag_name":"v9.0.0","html_url":"https://attacker.invalid/release","draft":false,"prerelease":false},
            {"tag_name":"v8.0.0","html_url":"https://github.com/Nonary/amaterasu/releases/tag/v8.0.0","draft":true,"prerelease":false},
            {"tag_name":"preview","html_url":"https://github.com/Nonary/amaterasu/releases/tag/preview","draft":false,"prerelease":false}
        ]"#;

        let parsed = parse_release_feed(feed).unwrap();

        assert_eq!(parsed.stable.unwrap().tag, "v2.0.0");
        assert_eq!(parsed.prerelease.unwrap().tag, "v3.0.0-rc.1");
    }

    #[test]
    fn stable_builds_skip_prereleases_unless_enabled_or_already_prerelease() {
        let releases = ReleaseSet {
            stable: Some(release("v2.0.0", false)),
            prerelease: Some(release("v2.1.0-rc.1", true)),
        };
        let stable_settings = Settings::default();
        let prerelease_settings = Settings {
            notify_pre_releases: true,
            ..Settings::default()
        };

        assert_eq!(
            select_available_update(
                &releases,
                &Version::parse("1.9.0").unwrap(),
                &stable_settings
            )
            .unwrap()
            .tag,
            "v2.0.0"
        );
        assert_eq!(
            select_available_update(
                &releases,
                &Version::parse("2.0.0").unwrap(),
                &stable_settings
            ),
            None
        );
        assert_eq!(
            select_available_update(
                &releases,
                &Version::parse("2.0.0").unwrap(),
                &prerelease_settings
            )
            .unwrap()
            .tag,
            "v2.1.0-rc.1"
        );
        assert_eq!(
            select_available_update(
                &releases,
                &Version::parse("2.0.0-beta.1").unwrap(),
                &stable_settings
            )
            .unwrap()
            .tag,
            "v2.1.0-rc.1"
        );
    }

    #[test]
    fn prerelease_must_outrank_stable_and_installed_version() {
        let releases = ReleaseSet {
            stable: Some(release("v3.0.0", false)),
            prerelease: Some(release("v3.0.0-rc.1", true)),
        };
        let settings = Settings {
            notify_pre_releases: true,
            ..Settings::default()
        };

        assert_eq!(
            select_available_update(&releases, &Version::parse("2.0.0").unwrap(), &settings)
                .unwrap()
                .tag,
            "v3.0.0"
        );
        assert!(
            select_available_update(&releases, &Version::parse("3.0.0").unwrap(), &settings)
                .is_none()
        );
    }

    #[test]
    fn injected_release_and_notification_providers_exercise_the_check_path() {
        let mut source = FakeSource {
            releases: ReleaseSet {
                stable: Some(release("v1.2.0", false)),
                prerelease: Some(release("v2.0.0-beta.1", true)),
            },
        };
        let mut notifications = FakeNotifications {
            delivered: NotificationDelivery::Notified,
            calls: Vec::new(),
        };
        let settings = Settings {
            notify_pre_releases: true,
            ..Settings::default()
        };

        let result = check_with_providers(
            &mut source,
            &mut notifications,
            &Version::parse("1.0.0").unwrap(),
            &settings,
        )
        .unwrap();

        assert_eq!(result.available.unwrap().tag, "v2.0.0-beta.1");
        assert_eq!(result.notification, Some(NotificationDelivery::Notified));
        assert_eq!(notifications.calls.len(), 1);
    }

    #[test]
    fn disabled_or_unavailable_notifications_are_not_treated_as_delivered() {
        let update = AvailableUpdate {
            tag: "v1.1.0".into(),
            url: format!("{RELEASE_URL_PREFIX}v1.1.0"),
            prerelease: false,
        };
        let mut delivered = BTreeSet::new();

        for outcome in [
            NotificationDelivery::Disabled,
            NotificationDelivery::Unavailable,
        ] {
            assert!(!super::record_notification(
                &mut delivered,
                &update,
                outcome
            ));
        }
        assert!(super::record_notification(
            &mut delivered,
            &update,
            NotificationDelivery::Notified
        ));
        assert!(!super::record_notification(
            &mut delivered,
            &update,
            NotificationDelivery::Notified
        ));
        assert_eq!(delivered.len(), 1);
    }
}
