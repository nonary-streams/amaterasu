use amaterasu_module_sdk::{Command, HostLifecycleAction, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};
use url::Url;

const SYSTEM_TRAY_SERVICE: &str = "amaterasu.system-tray/1";
const UPDATER_SERVICE: &str = "amaterasu.updater/1";
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const REQUEST_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MenuAction {
    OpenUi,
    CheckUpdates,
    Restart,
    Quit,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum TrayEvent {
    Menu { action: MenuAction },
    OpenClients,
    OpenDashboard,
    OpenRelease { release_url: String },
}

#[derive(Clone, Copy)]
enum PendingKind {
    Event,
    UpdateCheck,
}

enum PendingStage {
    Waiting,
    Releasing,
}

struct PendingRequest {
    id: u64,
    kind: PendingKind,
    stage: PendingStage,
}

pub(super) struct TrayEventConsumer {
    pending: Option<PendingRequest>,
    queued_event: Option<TrayEvent>,
    next_poll: Instant,
    sequence: u64,
}

impl Default for TrayEventConsumer {
    fn default() -> Self {
        Self {
            pending: None,
            queued_event: None,
            next_poll: Instant::now(),
            sequence: 0,
        }
    }
}

impl TrayEventConsumer {
    pub fn tick(
        &mut self,
        runtime: &RuntimeClient,
        management: Option<&super::server::Server>,
    ) -> ModuleResult<()> {
        if let Some(mut pending) = self.pending.take() {
            match pending.stage {
                PendingStage::Waiting => {
                    match runtime.send(Command::TakeResult {
                        request: pending.id,
                    }) {
                        Ok(Response::Pending) => self.pending = Some(pending),
                        Ok(Response::Result { payload }) => {
                            if matches!(pending.kind, PendingKind::Event) {
                                self.queued_event = decode_event(payload);
                            }
                            pending.stage = PendingStage::Releasing;
                            self.pending = Some(pending);
                        }
                        Ok(Response::Error { code, .. }) if code == "not_found" => {
                            self.next_poll = Instant::now() + EVENT_POLL_INTERVAL;
                        }
                        Ok(_) | Err(_) => {
                            self.next_poll = Instant::now() + EVENT_POLL_INTERVAL;
                            self.pending = Some(pending);
                        }
                    }
                }
                PendingStage::Releasing => {
                    match runtime.send(Command::ReleaseResult {
                        request: pending.id,
                    }) {
                        Ok(Response::Ok) => {
                            self.next_poll = Instant::now() + EVENT_POLL_INTERVAL;
                        }
                        Ok(Response::Error { code, .. }) if code == "not_found" => {
                            self.next_poll = Instant::now() + EVENT_POLL_INTERVAL;
                        }
                        Ok(_) | Err(_) => self.pending = Some(pending),
                    }
                }
            }
        }

        if self.pending.is_some() {
            return Ok(());
        }
        if let Some(event) = self.queued_event.take() {
            self.dispatch(event, runtime, management)?;
            if self.pending.is_some() {
                return Ok(());
            }
        }
        if Instant::now() < self.next_poll {
            return Ok(());
        }
        self.sequence = self.sequence.saturating_add(1);
        match runtime.send(Command::Request {
            service: SYSTEM_TRAY_SERVICE.into(),
            payload: json!({"action":"event_next"}),
            timeout_ms: REQUEST_TIMEOUT_MS,
            operation_id: format!("management.tray-event.{}", self.sequence),
            parent: None,
        }) {
            Ok(Response::Accepted { id }) => {
                self.pending = Some(PendingRequest {
                    id,
                    kind: PendingKind::Event,
                    stage: PendingStage::Waiting,
                });
            }
            Ok(_) | Err(_) => self.next_poll = Instant::now() + EVENT_POLL_INTERVAL,
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        event: TrayEvent,
        runtime: &RuntimeClient,
        management: Option<&super::server::Server>,
    ) -> ModuleResult<()> {
        match event {
            TrayEvent::Menu {
                action: MenuAction::OpenUi,
            }
            | TrayEvent::OpenDashboard => {
                open_management_page(management, "overview");
            }
            TrayEvent::OpenClients => open_management_page(management, "settings"),
            TrayEvent::OpenRelease { release_url } => open_release_page(&release_url),
            TrayEvent::Menu {
                action: MenuAction::CheckUpdates,
            } => self.begin_update_check(runtime)?,
            TrayEvent::Menu {
                action: MenuAction::Restart,
            } => {
                let _ = runtime.request_host_lifecycle(HostLifecycleAction::Restart);
            }
            TrayEvent::Menu {
                action: MenuAction::Quit,
            } => {
                let _ = runtime.request_host_lifecycle(HostLifecycleAction::Quit);
            }
        }
        Ok(())
    }

    fn begin_update_check(&mut self, runtime: &RuntimeClient) -> ModuleResult<()> {
        self.sequence = self.sequence.saturating_add(1);
        if let Ok(Response::Accepted { id }) = runtime.send(Command::Request {
            service: UPDATER_SERVICE.into(),
            payload: json!({"action":"check"}),
            timeout_ms: REQUEST_TIMEOUT_MS,
            operation_id: format!("management.tray-update-check.{}", self.sequence),
            parent: None,
        }) {
            self.pending = Some(PendingRequest {
                id,
                kind: PendingKind::UpdateCheck,
                stage: PendingStage::Waiting,
            });
        }
        Ok(())
    }
}

fn decode_event(response: Value) -> Option<TrayEvent> {
    if response["ok"] != true {
        return None;
    }
    serde_json::from_value(response.get("event")?.clone()).ok()
}

fn open_management_page(server: Option<&super::server::Server>, page: &str) {
    let Some(server) = server else {
        return;
    };
    let Some(url) = management_page_url(server.address(), server.tls_address(), page) else {
        return;
    };
    let _ = open_url(url.as_str());
}

fn management_page_url(
    http_address: &str,
    tls_address: Option<SocketAddr>,
    page: &str,
) -> Option<Url> {
    let http_address = http_address.parse::<SocketAddr>().ok()?;
    let (scheme, address) = tls_address
        .filter(|address| address.ip().is_loopback())
        .map(|address| ("https", address))
        .or_else(|| {
            http_address
                .ip()
                .is_loopback()
                .then_some(("http", http_address))
        })?;
    let mut url = Url::parse(&format!("{scheme}://{address}/")).ok()?;
    url.set_fragment(Some(page));
    Some(url)
}

fn open_release_page(release_url: &str) {
    let Some(url) = validated_release_url(release_url) else {
        return;
    };
    let _ = open_url(url.as_str());
}

fn validated_release_url(release_url: &str) -> Option<Url> {
    if release_url.len() > 2_048 {
        return None;
    }
    let url = Url::parse(release_url).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

#[cfg(target_os = "linux")]
fn open_url(url: &str) -> bool {
    ProcessCommand::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> bool {
    ProcessCommand::new("open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(windows)]
fn open_url(url: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;

    use std::ffi::OsStr;
    let verb: Vec<u16> = OsStr::new("open").encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = OsStr::new(url).encode_wide().chain(Some(0)).collect();
    // SAFETY: both strings are null-terminated UTF-16 buffers that live for
    // the synchronous ShellExecuteW call; the shell receives no borrowed output.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        ) > 32
    }
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        window: *mut std::ffi::c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show_command: i32,
    ) -> isize;
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn open_url(_url: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_events_are_typed_and_unknown_shapes_are_discarded() {
        assert!(matches!(
            decode_event(json!({"ok":true,"event":{"kind":"menu","action":"quit"}})),
            Some(TrayEvent::Menu {
                action: MenuAction::Quit
            })
        ));
        assert!(
            decode_event(
                json!({"ok":true,"event":{"kind":"menu","action":"shell","command":"rm"}})
            )
            .is_none()
        );
        assert!(decode_event(json!({"ok":true,"event":null})).is_none());
    }

    #[test]
    fn release_clickthrough_is_limited_to_https_github_urls() {
        let github = "https://github.com/LizardByte/Sunshine/releases/tag/v1";
        assert!(validated_release_url(github).is_some());
        for rejected in [
            "http://github.com/LizardByte/Sunshine",
            "https://github.com.evil.example/path",
            "https://user:pass@github.com/path",
            "javascript:alert(1)",
        ] {
            assert!(validated_release_url(rejected).is_none());
        }
    }

    #[test]
    fn management_clickthrough_uses_an_actual_loopback_listener_and_route() {
        let http = "127.0.0.1:47990";
        let tls = "127.0.0.1:47991".parse().unwrap();
        assert_eq!(
            management_page_url(http, Some(tls), "settings")
                .unwrap()
                .as_str(),
            "https://127.0.0.1:47991/#settings"
        );
        assert_eq!(
            management_page_url(http, None, "overview")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:47990/#overview"
        );
        assert_eq!(
            management_page_url(http, Some("192.0.2.1:47991".parse().unwrap()), "settings")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:47990/#settings"
        );
        assert!(management_page_url("192.0.2.2:47990", None, "settings").is_none());
    }
}
