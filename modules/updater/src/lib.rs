//! Periodic GitHub release checks with durable user preferences.
mod model;
mod product_settings;
mod release;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use model::Settings;
use product_settings::{Completion, ProductSettings};
use release::{
    AvailableUpdate, GitHubReleaseSource, NotificationDelivery, Release, ReleaseSet, ReleaseSource,
    notification_delivery, record_notification, select_available_update,
};
use semver::Version;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SERVICE: &str = "amaterasu.updater/1";
const MANAGEMENT: &str = "amaterasu.management";
const SYSTEM_TRAY: &str = "amaterasu.system-tray/1";
const NETWORK_TIMEOUT_MS: u64 = 12_000;
const MAX_DEFERRED_REQUESTS: usize = 32;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    SettingsGet {},
    SettingsUpdate {
        settings: Settings,
        expected_revision: u64,
    },
    Status {},
    Check {},
}

struct NotificationStatus {
    update: AvailableUpdate,
    delivery: &'static str,
}

struct UpdaterStatusData<'a> {
    settings_ready: bool,
    revision: u64,
    check_in_progress: bool,
    last_check_unix_ms: Option<i64>,
    last_error: Option<&'a str>,
    releases: &'a ReleaseSet,
    available_update: Option<&'a AvailableUpdate>,
    notification: Option<&'a NotificationStatus>,
}

enum TrayCallState {
    Submit,
    Waiting(u64),
    Release {
        request: u64,
        outcome: Result<Value, ()>,
    },
}

struct TrayCall {
    update: AvailableUpdate,
    state: TrayCallState,
}

struct UpdaterModule {
    runtime: RuntimeClient,
    product_settings: ProductSettings,
    deferred_requests: VecDeque<(u64, Operation)>,
    installed_version: Version,
    release_sender: Sender<Result<ReleaseSet, String>>,
    release_receiver: Receiver<Result<ReleaseSet, String>>,
    check_in_progress: bool,
    next_check_at: Option<Instant>,
    configured_interval: Option<u32>,
    last_check_unix_ms: Option<i64>,
    last_error: Option<&'static str>,
    releases: ReleaseSet,
    available_update: Option<AvailableUpdate>,
    delivered_notifications: BTreeSet<(String, bool)>,
    queued_notification: Option<AvailableUpdate>,
    tray_call: Option<TrayCall>,
    notification_status: Option<NotificationStatus>,
    operation_sequence: u64,
}

impl ManagedModule for UpdaterModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        let (release_sender, release_receiver) = mpsc::channel();
        let startup_id = format!(
            "{}-{}",
            std::process::id(),
            unix_time_milliseconds().unwrap_or_default()
        );
        let installed_version = Version::parse(env!("CARGO_PKG_VERSION"))
            .map_err(|_| "updater package version is invalid")?;
        Ok(Self {
            runtime,
            product_settings: ProductSettings::new(&startup_id),
            deferred_requests: VecDeque::new(),
            installed_version,
            release_sender,
            release_receiver,
            check_in_progress: false,
            next_check_at: None,
            configured_interval: None,
            last_check_unix_ms: None,
            last_error: None,
            releases: ReleaseSet {
                stable: None,
                prerelease: None,
            },
            available_update: None,
            delivered_notifications: BTreeSet::new(),
            queued_notification: None,
            tray_call: None,
            notification_status: None,
            operation_sequence: 0,
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        match self.runtime.send(Command::Poll {})? {
            Response::Pending => {}
            Response::Request {
                id,
                caller,
                payload,
                ..
            } => match serde_json::from_value::<Operation>(payload) {
                Ok(operation) => self.handle_request(id, &caller, operation)?,
                Err(_) => self.complete(
                    id,
                    failure(
                        "invalid_request",
                        "request does not match the updater schema",
                    ),
                )?,
            },
            response => return Err(format!("unexpected updater poll response: {response:?}")),
        }

        let runtime = &mut self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command))?;
        while let Some(Completion::Update { request, response }) =
            self.product_settings.take_completion()
        {
            self.complete(request, response)?;
        }

        self.reconcile_interval(Instant::now());
        self.drain_deferred_requests()?;
        self.collect_release_result(Instant::now());
        self.start_periodic_check_if_due(Instant::now());
        self.advance_tray_call()?;
        Ok(())
    }
}

impl UpdaterModule {
    fn handle_request(
        &mut self,
        request: u64,
        caller: &str,
        operation: Operation,
    ) -> ModuleResult<()> {
        if caller != MANAGEMENT {
            return self.complete(
                request,
                failure(
                    "permission_denied",
                    "caller is not authorized for updater settings",
                ),
            );
        }
        match operation {
            Operation::SettingsGet {} => {
                if !self.product_settings.is_ready() {
                    return self.defer(request, Operation::SettingsGet {});
                }
                let (settings, revision) = self.product_settings.settings();
                self.complete(
                    request,
                    json!({"ok":true,"settings":settings,"revision":revision}),
                )
            }
            Operation::SettingsUpdate {
                settings,
                expected_revision,
            } => {
                if !self.product_settings.is_ready() {
                    return self.defer(
                        request,
                        Operation::SettingsUpdate {
                            settings,
                            expected_revision,
                        },
                    );
                }
                match self
                    .product_settings
                    .begin_update(request, expected_revision, settings)
                {
                    Ok(()) => Ok(()),
                    Err(response) => self.complete(request, response),
                }
            }
            Operation::Status {} => self.complete(request, self.status()),
            Operation::Check {} => {
                if !self.product_settings.is_ready() {
                    return self.defer(request, Operation::Check {});
                }
                let started = self.start_check(Instant::now());
                self.complete(
                    request,
                    json!({
                        "ok":true,
                        "started":started,
                        "in_progress":self.check_in_progress
                    }),
                )
            }
        }
    }

    fn defer(&mut self, request: u64, operation: Operation) -> ModuleResult<()> {
        if self.deferred_requests.len() >= MAX_DEFERRED_REQUESTS {
            return self.complete(
                request,
                failure("settings_busy", "too many updater requests are queued"),
            );
        }
        self.deferred_requests.push_back((request, operation));
        Ok(())
    }

    fn drain_deferred_requests(&mut self) -> ModuleResult<()> {
        if !self.product_settings.is_ready() {
            return Ok(());
        }
        while let Some((request, operation)) = self.deferred_requests.pop_front() {
            self.handle_request(request, MANAGEMENT, operation)?;
        }
        Ok(())
    }

    fn reconcile_interval(&mut self, now: Instant) {
        if !self.product_settings.is_ready() {
            return;
        }
        let interval = self.product_settings.settings().0.update_check_interval;
        if self.configured_interval == Some(interval) {
            return;
        }
        self.configured_interval = Some(interval);
        self.next_check_at = if interval == 0 { None } else { Some(now) };
    }

    fn start_periodic_check_if_due(&mut self, now: Instant) {
        if periodic_check_due(
            self.configured_interval,
            self.next_check_at,
            now,
            self.check_in_progress,
        ) {
            self.start_check(now);
        }
    }

    fn start_check(&mut self, now: Instant) -> bool {
        if self.check_in_progress {
            return false;
        }
        self.check_in_progress = true;
        self.next_check_at = None;
        let sender = self.release_sender.clone();
        let result = thread::Builder::new()
            .name("amaterasu-updater-check".into())
            .spawn(move || {
                let result = GitHubReleaseSource::default().fetch();
                let _ = sender.send(result);
            });
        if result.is_err() {
            self.check_in_progress = false;
            self.last_error = Some("worker_unavailable");
            self.finish_check_time(now);
            return false;
        }
        true
    }

    fn collect_release_result(&mut self, now: Instant) {
        match self.release_receiver.try_recv() {
            Ok(Ok(releases)) => {
                self.releases = releases;
                self.last_error = None;
                self.available_update = select_available_update(
                    &self.releases,
                    &self.installed_version,
                    self.product_settings.settings().0,
                );
                if let Some(update) = self.available_update.clone() {
                    let key = (update.tag.clone(), update.prerelease);
                    if !self.delivered_notifications.contains(&key)
                        && !self
                            .tray_call
                            .as_ref()
                            .is_some_and(|call| call.update == update)
                    {
                        self.queued_notification = Some(update.clone());
                        self.notification_status = Some(NotificationStatus {
                            update,
                            delivery: "pending",
                        });
                    }
                }
                self.check_in_progress = false;
                self.finish_check_time(now);
            }
            Ok(Err(_)) => {
                self.last_error = Some("release_service_unavailable");
                self.check_in_progress = false;
                self.finish_check_time(now);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.last_error = Some("release_worker_stopped");
                self.check_in_progress = false;
                self.finish_check_time(now);
            }
        }
    }

    fn finish_check_time(&mut self, now: Instant) {
        self.last_check_unix_ms = unix_time_milliseconds();
        let interval = if self.product_settings.is_ready() {
            self.product_settings.settings().0.update_check_interval
        } else {
            0
        };
        self.next_check_at = (interval > 0)
            .then(|| now.checked_add(Duration::from_secs(u64::from(interval))))
            .flatten();
    }

    fn advance_tray_call(&mut self) -> ModuleResult<()> {
        if self.tray_call.is_none() {
            let Some(update) = self.queued_notification.take() else {
                return Ok(());
            };
            self.operation_sequence = self.operation_sequence.saturating_add(1);
            self.tray_call = Some(TrayCall {
                update,
                state: TrayCallState::Submit,
            });
            return Ok(());
        }
        let Some(mut call) = self.tray_call.take() else {
            unreachable!("tray call was checked before it was taken")
        };

        let mut finished = false;
        let state = std::mem::replace(&mut call.state, TrayCallState::Submit);
        match state {
            TrayCallState::Submit => match self.runtime.send(Command::Request {
                service: SYSTEM_TRAY.into(),
                payload: json!({
                    "action":"notify_update",
                    "version":call.update.tag,
                    "release_url":call.update.url
                }),
                timeout_ms: NETWORK_TIMEOUT_MS,
                operation_id: format!("updater.notification.{}", self.operation_sequence),
                parent: None,
            }) {
                Ok(Response::Accepted { id }) => call.state = TrayCallState::Waiting(id),
                Ok(Response::Error { .. }) | Err(_) => {
                    self.finish_notification(&call.update, NotificationDelivery::Unavailable);
                    finished = true;
                }
                Ok(_) => call.state = TrayCallState::Submit,
            },
            TrayCallState::Waiting(request) => {
                match self.runtime.send(Command::TakeResult { request }) {
                    Ok(Response::Pending) | Err(_) => call.state = TrayCallState::Waiting(request),
                    Ok(Response::Result { payload }) => {
                        call.state = TrayCallState::Release {
                            request,
                            outcome: Ok(payload),
                        };
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.finish_notification(&call.update, NotificationDelivery::Unavailable);
                        finished = true;
                    }
                    Ok(Response::Error { .. }) | Ok(_) => {
                        call.state = TrayCallState::Release {
                            request,
                            outcome: Err(()),
                        };
                    }
                }
            }
            TrayCallState::Release { request, outcome } => {
                match self.runtime.send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => {
                        let delivery = outcome
                            .as_ref()
                            .map(notification_delivery)
                            .unwrap_or(NotificationDelivery::Unavailable);
                        self.finish_notification(&call.update, delivery);
                        finished = true;
                    }
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        let delivery = outcome
                            .as_ref()
                            .map(notification_delivery)
                            .unwrap_or(NotificationDelivery::Unavailable);
                        self.finish_notification(&call.update, delivery);
                        finished = true;
                    }
                    Ok(_) | Err(_) => call.state = TrayCallState::Release { request, outcome },
                }
            }
        }
        if !finished {
            self.tray_call = Some(call);
        }
        Ok(())
    }

    fn finish_notification(&mut self, update: &AvailableUpdate, delivery: NotificationDelivery) {
        if record_notification(&mut self.delivered_notifications, update, delivery) {
            self.notification_status = Some(NotificationStatus {
                update: update.clone(),
                delivery: "notified",
            });
            return;
        }
        self.notification_status = Some(NotificationStatus {
            update: update.clone(),
            delivery: match delivery {
                NotificationDelivery::Notified => "notified",
                NotificationDelivery::Disabled => "disabled",
                NotificationDelivery::Unavailable => "unavailable",
            },
        });
    }

    fn status(&self) -> Value {
        status_payload(UpdaterStatusData {
            settings_ready: self.product_settings.is_ready(),
            revision: self.product_settings.settings().1,
            check_in_progress: self.check_in_progress,
            last_check_unix_ms: self.last_check_unix_ms,
            last_error: self.last_error,
            releases: &self.releases,
            available_update: self.available_update.as_ref(),
            notification: self.notification_status.as_ref(),
        })
    }

    fn complete(&self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            Response::Error { code, .. }
                if matches!(
                    code.as_str(),
                    "cancelled" | "deadline_exceeded" | "request_ended"
                ) =>
            {
                Ok(())
            }
            Response::Error { code, message } => Err(format!("{code}: {message}")),
            response => Err(format!(
                "unexpected updater completion response: {response:?}"
            )),
        }
    }
}

fn release_summary(release: &Release) -> Value {
    json!({"version":release.tag,"url":release.url})
}

fn available_summary(update: &AvailableUpdate) -> Value {
    json!({
        "version":update.tag,
        "release_url":update.url,
        "prerelease":update.prerelease
    })
}

fn status_payload(data: UpdaterStatusData<'_>) -> Value {
    let notification = data.notification.map(|status| {
        json!({
            "version":status.update.tag,
            "prerelease":status.update.prerelease,
            "release_url":status.update.url,
            "delivery":status.delivery
        })
    });
    json!({
        "ok":true,
        "settings_ready":data.settings_ready,
        "revision":data.revision,
        "check_in_progress":data.check_in_progress,
        "last_check_unix_ms":data.last_check_unix_ms,
        "last_error":data.last_error,
        "latest_stable":data.releases.stable.as_ref().map(release_summary),
        "latest_prerelease":data.releases.prerelease.as_ref().map(release_summary),
        "available_update":data.available_update.map(available_summary),
        "notification":notification
    })
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"code":code,"message":message})
}

fn periodic_check_due(
    interval_seconds: Option<u32>,
    deadline: Option<Instant>,
    now: Instant,
    check_in_progress: bool,
) -> bool {
    interval_seconds.is_some_and(|interval| interval > 0)
        && !check_in_progress
        && deadline.is_some_and(|deadline| now >= deadline)
}

fn unix_time_milliseconds() -> Option<i64> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_millis()).ok()
}

amaterasu_module_sdk::export_module!(UpdaterModule);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DEFAULT_UPDATE_CHECK_INTERVAL_SECONDS, MAX_UPDATE_CHECK_INTERVAL_SECONDS};

    #[test]
    fn updater_defaults_keep_the_24_hour_seconds_interval_and_zero_disables_checks() {
        let defaults = Settings::default();
        assert_eq!(defaults.update_check_interval, 86_400);
        assert!(!defaults.notify_pre_releases);
        assert_eq!(DEFAULT_UPDATE_CHECK_INTERVAL_SECONDS, 86_400);
        assert_eq!(MAX_UPDATE_CHECK_INTERVAL_SECONDS, 2_147_483_647);

        let now = Instant::now();
        assert!(!periodic_check_due(Some(0), Some(now), now, false));
        assert!(!periodic_check_due(None, Some(now), now, false));
        assert!(periodic_check_due(Some(60), Some(now), now, false));
        assert!(!periodic_check_due(Some(60), Some(now), now, true));

        assert!(
            Settings {
                update_check_interval: 0,
                ..defaults
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn updater_status_reports_pending_settings_and_absent_release_data_safely() {
        let releases = ReleaseSet {
            stable: None,
            prerelease: None,
        };
        let status = status_payload(UpdaterStatusData {
            settings_ready: false,
            revision: 0,
            check_in_progress: false,
            last_check_unix_ms: None,
            last_error: None,
            releases: &releases,
            available_update: None,
            notification: None,
        });
        assert_eq!(status["ok"], true);
        assert_eq!(status["settings_ready"], false);
        assert_eq!(status["latest_stable"], Value::Null);
        assert_eq!(status["available_update"], Value::Null);
    }
}
