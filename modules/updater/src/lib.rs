//! Periodic GitHub release checks with durable user preferences.
mod model;
mod product_settings;
mod release;
mod worker;

use amaterasu_module_sdk::network::{Network, abi as network_abi};
use amaterasu_module_sdk::requests::{CallOutcome, OutboundCall, RequestOptions, reply_with};
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
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use worker::ReleaseWorker;

const SERVICE: &str = "amaterasu.updater/1";
const MANAGEMENT: &str = "amaterasu.management";
const STREAM_SESSION: &str = "amaterasu.stream-session";
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
    StreamStarted {
        session_id: String,
    },
    StreamStopped {
        session_id: String,
    },
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

/// A notification handed to the system tray, and what is known of it.
struct TrayCall {
    update: AvailableUpdate,
    /// `None` once the request ended without the tray's reply. Broker expiry
    /// or cancellation does not prove the provider stopped, so the
    /// notification is neither repeated nor reported as settled.
    call: Option<OutboundCall>,
}

impl TrayCall {
    fn new(update: AvailableUpdate, operation_id: &str) -> ModuleResult<Self> {
        let call = OutboundCall::request(
            RequestOptions {
                service: SYSTEM_TRAY,
                timeout_ms: NETWORK_TIMEOUT_MS,
                operation_id,
                parent: None,
            },
            &json!({"action":"notify_update", "version":update.tag, "release_url":update.url}),
        )?;
        Ok(Self {
            update,
            call: Some(call),
        })
    }

    /// Whether the tray may already have been asked to notify.
    fn was_submitted(&self) -> bool {
        self.call.as_ref().is_none_or(OutboundCall::was_submitted)
    }

    /// The call keeps its operation identity across a lost admission, so a
    /// retry cannot notify twice.
    fn advance(
        &mut self,
        now: Instant,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Option<NotificationDelivery> {
        let call = self.call.as_mut()?;
        match call.poll_at(now, send) {
            Poll::Pending => None,
            Poll::Ready(CallOutcome::Completed(outcome)) => Some(notification_delivery(&outcome)),
            // The broker never admitted the request, so the tray never saw it.
            Poll::Ready(CallOutcome::Refused(_)) => Some(NotificationDelivery::Unavailable),
            Poll::Ready(CallOutcome::Unconfirmed(_)) => {
                self.call = None;
                None
            }
        }
    }
}

struct UpdaterModule {
    runtime: RuntimeClient,
    product_settings: ProductSettings,
    deferred_requests: VecDeque<(u64, Operation)>,
    pending_completions: VecDeque<(u64, Value)>,
    installed_version: Version,
    network: Network,
    release_worker: Option<ReleaseWorker>,
    check_in_progress: bool,
    next_check_at: Option<Instant>,
    configured_interval: Option<u32>,
    last_check_started: Option<Instant>,
    active_streams: BTreeSet<String>,
    stream_start_check_at: Option<Instant>,
    last_check_unix_ms: Option<i64>,
    last_error: Option<&'static str>,
    releases: ReleaseSet,
    available_update: Option<AvailableUpdate>,
    delivered_notifications: BTreeSet<(String, bool)>,
    queued_notification: Option<AvailableUpdate>,
    tray_call: Option<TrayCall>,
    notification_status: Option<NotificationStatus>,
    operation_sequence: u64,
    notification_generation: String,
    shutdown_announced: bool,
}

impl ManagedModule for UpdaterModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings: Value = runtime
            .settings()
            .map_err(|error| format!("updater settings unavailable: {error}"))?;
        let network = Network::from_settings(&runtime, &settings)?;
        if !network.supports(network_abi::TCP | network_abi::DNS) {
            return Err("updater requires network TCP and DNS capabilities".into());
        }
        let mut startup_nonce = [0_u8; 16];
        getrandom::fill(&mut startup_nonce)
            .map_err(|_| "updater startup randomness unavailable")?;
        let startup_id: String = startup_nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let installed_version = Version::parse(env!("CARGO_PKG_VERSION"))
            .map_err(|_| "updater package version is invalid")?;
        runtime.provide(SERVICE)?;
        Ok(Self {
            runtime,
            product_settings: ProductSettings::new(&startup_id),
            deferred_requests: VecDeque::new(),
            pending_completions: VecDeque::new(),
            installed_version,
            network,
            release_worker: None,
            check_in_progress: false,
            next_check_at: None,
            configured_interval: None,
            last_check_started: None,
            active_streams: BTreeSet::new(),
            stream_start_check_at: None,
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
            notification_generation: startup_id,
            shutdown_announced: false,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_announced = true;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutdown_announced = true;
        while let Some((request, _)) = self.deferred_requests.pop_front() {
            self.complete(
                request,
                failure("shutting_down", "updater is shutting down"),
            )?;
        }
        let runtime = &mut self.runtime;
        let settings_ready = self
            .product_settings
            .quiesce(&mut |command| runtime.send(command))?;
        while let Some(Completion::Update { request, response }) =
            self.product_settings.take_completion()
        {
            self.complete(request, response)?;
        }
        let worker_ready = retire_release_worker(&mut self.release_worker);
        let tray_ready = self.quiesce_tray_call()?;
        let completions_ready = self.flush_completions();
        Ok(settings_ready && worker_ready && tray_ready && completions_ready)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        // Keep bounded ownership of a response whose broker acknowledgement
        // was lost; do not admit another request until it has settled.
        if self.flush_completions()
            && let Some(request) = self.runtime.next_request()?
        {
            match request.parse::<Operation>() {
                Ok(request) => self.handle_request(request.id, &request.caller, request.payload)?,
                Err(malformed) => self.complete(
                    malformed.request.id,
                    failure(
                        "invalid_request",
                        "request does not match the updater schema",
                    ),
                )?,
            }
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
        self.start_stream_check_if_due(Instant::now());
        self.advance_tray_call()?;
        self.flush_completions();
        Ok(())
    }
}

impl Drop for UpdaterModule {
    fn drop(&mut self) {
        // Retire worker code and its provider handles before dropping this
        // module's runtime client or the network binding it shares.
        drop(self.release_worker.take());
    }
}

impl UpdaterModule {
    fn handle_request(
        &mut self,
        request: u64,
        caller: &str,
        operation: Operation,
    ) -> ModuleResult<()> {
        if self.shutdown_announced
            && matches!(
                operation,
                Operation::SettingsGet {} | Operation::SettingsUpdate { .. } | Operation::Check {}
            )
        {
            return self.complete(
                request,
                failure("shutting_down", "updater is shutting down"),
            );
        }
        if caller != MANAGEMENT
            && !(caller == STREAM_SESSION
                && matches!(
                    operation,
                    Operation::StreamStarted { .. } | Operation::StreamStopped { .. }
                ))
        {
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
            Operation::StreamStarted { session_id } => {
                if !valid_stream_session_id(&session_id) {
                    return self.complete(
                        request,
                        failure("invalid_request", "stream session identifier is invalid"),
                    );
                }
                if self.active_streams.insert(session_id) && self.active_streams.len() == 1 {
                    self.stream_start_check_at = Instant::now().checked_add(Duration::from_secs(3));
                }
                self.complete(request, json!({"ok":true}))
            }
            Operation::StreamStopped { session_id } => {
                if !valid_stream_session_id(&session_id) {
                    return self.complete(
                        request,
                        failure("invalid_request", "stream session identifier is invalid"),
                    );
                }
                self.active_streams.remove(&session_id);
                self.complete(request, json!({"ok":true}))
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
        let initial = self.configured_interval.is_none();
        if self.configured_interval == Some(interval) {
            return;
        }
        self.configured_interval = Some(interval);
        self.next_check_at = periodic_deadline(now, interval);
        if initial && interval > 0 {
            self.start_check(now);
        }
    }

    fn start_periodic_check_if_due(&mut self, now: Instant) {
        let Some(interval) = self.configured_interval.filter(|interval| *interval > 0) else {
            return;
        };
        if self.next_check_at.is_some_and(|deadline| now >= deadline) {
            self.next_check_at = periodic_deadline(now, interval);
        } else {
            return;
        }
        if self.active_streams.is_empty()
            && !self.check_in_progress
            && periodic_check_eligible(interval, self.last_check_started, now)
        {
            self.start_check(now);
        }
    }

    fn start_stream_check_if_due(&mut self, now: Instant) {
        if self
            .stream_start_check_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.stream_start_check_at = None;
            self.start_check(now);
        }
    }

    fn start_check(&mut self, now: Instant) -> bool {
        if self.shutdown_announced || self.check_in_progress {
            return false;
        }
        self.check_in_progress = true;
        self.last_check_started = Some(now);
        let network = self.network.clone();
        match ReleaseWorker::spawn(move || GitHubReleaseSource::new(network).fetch()) {
            Ok(worker) => self.release_worker = Some(worker),
            Err(_) => {
                self.check_in_progress = false;
                self.last_error = Some("worker_unavailable");
                self.finish_check_time(now);
                return false;
            }
        }
        true
    }

    fn collect_release_result(&mut self, now: Instant) {
        let Some(result) = self.release_worker.as_mut().and_then(ReleaseWorker::poll) else {
            return;
        };
        self.release_worker = None;
        match result {
            Ok(releases) => {
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
            Err(_) => {
                self.last_error = Some("release_service_unavailable");
                self.check_in_progress = false;
                self.finish_check_time(now);
            }
        }
    }

    fn finish_check_time(&mut self, _now: Instant) {
        self.last_check_unix_ms = unix_time_milliseconds();
    }

    fn advance_tray_call(&mut self) -> ModuleResult<()> {
        if self.tray_call.is_none() {
            if self.shutdown_announced {
                self.queued_notification = None;
                return Ok(());
            }
            let Some(update) = self.queued_notification.take() else {
                return Ok(());
            };
            let Some(sequence) = self.operation_sequence.checked_add(1) else {
                self.finish_notification(&update, NotificationDelivery::Unavailable);
                return Ok(());
            };
            self.operation_sequence = sequence;
            let operation_id = format!(
                "updater.notification.{}.{sequence}",
                self.notification_generation
            );
            match TrayCall::new(update.clone(), &operation_id) {
                Ok(call) => self.tray_call = Some(call),
                Err(_) => self.finish_notification(&update, NotificationDelivery::Unavailable),
            }
            return Ok(());
        }
        let mut call = self.tray_call.take().expect("tray call exists");
        if self.shutdown_announced && !call.was_submitted() {
            return Ok(());
        }
        let result = call.advance(Instant::now(), &mut |command| self.runtime.send(command));
        if let Some(delivery) = result {
            self.finish_notification(&call.update, delivery);
        } else {
            self.tray_call = Some(call);
        }
        Ok(())
    }

    /// Settle an admitted notification while its tray provider remains alive.
    fn quiesce_tray_call(&mut self) -> ModuleResult<bool> {
        self.advance_tray_call()?;
        Ok(self.tray_call.is_none())
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

    fn complete(&mut self, request: u64, payload: Value) -> ModuleResult<()> {
        self.pending_completions.push_back((request, payload));
        Ok(())
    }

    fn flush_completions(&mut self) -> bool {
        let runtime = &self.runtime;
        flush_queued_completions(&mut self.pending_completions, &mut |command| {
            runtime.send(command)
        })
    }
}

fn flush_queued_completions(
    completions: &mut VecDeque<(u64, Value)>,
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
) -> bool {
    while let Some((request, payload)) = completions.front() {
        // A reply to a request that already ended is settled. Neither a
        // transport failure nor any other broker response confirms
        // completion; retain the same request and payload.
        if reply_with(send, *request, payload).is_err() {
            return false;
        }
        completions.pop_front();
    }
    true
}

/// The worker owns a network binding and module code. Never join it from a
/// cooperative host tick until it has actually finished.
fn retire_release_worker(worker: &mut Option<ReleaseWorker>) -> bool {
    let Some(active) = worker.as_mut() else {
        return true;
    };
    if active.poll().is_none() {
        return false;
    }
    *worker = None;
    true
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

fn periodic_deadline(now: Instant, interval_seconds: u32) -> Option<Instant> {
    if interval_seconds == 0 {
        return None;
    }
    now.checked_add(Duration::from_secs(u64::from(interval_seconds)))
}

fn periodic_check_eligible(
    interval_seconds: u32,
    last_check_started: Option<Instant>,
    now: Instant,
) -> bool {
    interval_seconds > 0
        && last_check_started.is_none_or(|started| {
            now.saturating_duration_since(started)
                >= Duration::from_secs(u64::from(interval_seconds))
        })
}

fn valid_stream_session_id(session_id: &str) -> bool {
    session_id
        .strip_prefix("stream-session-")
        .is_some_and(|identifier| {
            identifier.len() == 16 && identifier.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
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
    fn completion_acknowledgement_loss_keeps_exact_response_until_settled() {
        let payload = json!({"ok": true, "revision": 2});
        let mut pending = VecDeque::from([(45, payload.clone())]);
        assert!(!flush_queued_completions(&mut pending, &mut |command| {
            assert!(
                matches!(command, Command::Complete { request: 45, payload: ref value } if *value == payload)
            );
            Err("response lost".into())
        }));
        assert_eq!(pending.len(), 1);
        assert!(flush_queued_completions(&mut pending, &mut |command| {
            assert!(
                matches!(command, Command::Complete { request: 45, payload: ref value } if *value == payload)
            );
            Ok(Response::error("request_ended", "already completed"))
        }));
        assert!(pending.is_empty());
    }

    #[test]
    fn unknown_completion_error_does_not_retire_following_requests() {
        let mut pending = VecDeque::from([(1, json!({})), (2, json!({}))]);
        assert!(!flush_queued_completions(&mut pending, &mut |command| {
            assert!(matches!(command, Command::Complete { request: 1, .. }));
            Ok(Response::error("unavailable", "try again"))
        }));
        assert_eq!(pending.len(), 2);
    }

    fn tray_call() -> TrayCall {
        TrayCall::new(
            AvailableUpdate {
                tag: "v2".into(),
                url: "https://example.test/v2".into(),
                prerelease: false,
            },
            "updater.notification.test.1",
        )
        .unwrap()
    }

    #[test]
    fn lost_notification_admission_retries_exact_operation_and_releases_result() {
        let now = Instant::now();
        let mut call = tray_call();
        let mut commands = Vec::new();
        assert!(
            call.advance(now, &mut |command| {
                commands.push(command);
                Err("lost reply".into())
            })
            .is_none()
        );
        assert!(
            call.advance(now, &mut |command| {
                commands.push(command);
                Ok(Response::Accepted { id: 7 })
            })
            .is_none()
        );
        assert!(matches!((&commands[0], &commands[1]),
            (Command::Request { operation_id: first, .. }, Command::Request { operation_id: second, .. }) if first == second));
        let mut settlement = Vec::new();
        assert_eq!(
            call.advance(now, &mut |command| {
                let response = match &command {
                    Command::TakeResult { request: 7 } => Response::Result {
                        payload: json!({"ok":true,"delivery":"notified"}),
                    },
                    Command::ReleaseResult { request: 7 } => Response::Ok,
                    unexpected => panic!("unexpected command {unexpected:?}"),
                };
                settlement.push(command);
                Ok(response)
            }),
            Some(NotificationDelivery::Notified)
        );
        assert!(matches!(
            settlement.as_slice(),
            [Command::TakeResult { .. }, Command::ReleaseResult { .. }]
        ));
    }

    #[test]
    fn a_refused_notification_is_reported_unavailable() {
        let mut call = tray_call();
        assert_eq!(
            call.advance(Instant::now(), &mut |_| Ok(Response::error(
                "unavailable",
                "no tray provider"
            ))),
            Some(NotificationDelivery::Unavailable)
        );
    }

    #[test]
    fn uncertain_notification_does_not_replay_after_broker_retention() {
        let now = Instant::now();
        let mut call = tray_call();
        assert!(
            call.advance(now, &mut |_| Err("lost reply".into()))
                .is_none()
        );
        for elapsed in [300, 301] {
            assert!(
                call.advance(now + Duration::from_secs(elapsed), &mut |_| panic!(
                    "must not replay expired operation"
                ))
                .is_none()
            );
        }
        assert!(call.call.is_none());
        assert!(call.was_submitted());
    }

    #[test]
    fn broker_expiry_does_not_prove_notification_retired() {
        let mut call = tray_call();
        assert!(
            call.advance(Instant::now(), &mut |_| Ok(Response::Accepted { id: 7 }))
                .is_none()
        );
        assert!(
            call.advance(Instant::now(), &mut |_| Ok(Response::error(
                "deadline_exceeded",
                "expired"
            )))
            .is_none()
        );
        assert!(
            call.advance(Instant::now(), &mut |_| panic!(
                "terminal broker status cannot prove retirement"
            ))
            .is_none()
        );
        assert!(call.call.is_none());
        assert!(call.was_submitted());
    }

    #[test]
    fn shutdown_waits_without_blocking_for_active_release_worker() {
        let (started, entered) = std::sync::mpsc::channel();
        let (release, finish) = std::sync::mpsc::channel();
        let mut worker = Some(
            ReleaseWorker::spawn(move || {
                started.send(()).unwrap();
                finish.recv().unwrap();
                Ok(ReleaseSet {
                    stable: None,
                    prerelease: None,
                })
            })
            .unwrap(),
        );
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let started = Instant::now();
        let ready = retire_release_worker(&mut worker);
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        assert!(!ready);
        assert!(elapsed < Duration::from_secs(1));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !retire_release_worker(&mut worker) {
            assert!(Instant::now() < deadline, "release worker did not retire");
            std::thread::yield_now();
        }
        assert!(worker.is_none());
    }

    #[test]
    fn missing_selected_network_provider_fails_before_registering_updater() {
        use amaterasu_module_sdk::abi::{
            AbiHeader, BorrowedBytes, HostApi, ReplySink, STATUS_OK, Status,
        };
        use std::ffi::c_void;

        struct Host {
            settings: Value,
            commands: Vec<Command>,
        }
        unsafe extern "C" fn invoke(
            context: *mut c_void,
            input: BorrowedBytes,
            reply: ReplySink,
        ) -> Status {
            // SAFETY: the boxed test host and synchronous request/reply borrows
            // remain alive until startup has returned.
            let host = unsafe { &mut *context.cast::<Host>() };
            let command: Command =
                serde_json::from_slice(unsafe { input.read().unwrap() }).unwrap();
            let response = match &command {
                Command::GetSettings {} => Response::Result {
                    payload: host.settings.clone(),
                },
                Command::NativeBindTyped { contract, .. } => {
                    assert_eq!(contract, network_abi::CONTRACT);
                    Response::error("permission_denied", "test provider absent")
                }
                Command::NativeBind { .. } => {
                    Response::error("permission_denied", "test provider absent")
                }
                _ => Response::Ok,
            };
            host.commands.push(command);
            let bytes = serde_json::to_vec(&response).unwrap();
            unsafe { reply.send(&bytes) };
            STATUS_OK
        }

        for (settings, expected) in [
            (json!({}), "amaterasu.network/1"),
            (
                json!({"network_provider":"example.network/1"}),
                "example.network/1",
            ),
        ] {
            let mut host = Box::new(Host {
                settings,
                commands: Vec::new(),
            });
            let endpoint = HostApi {
                header: AbiHeader::of::<HostApi>(),
                context: (&mut *host as *mut Host).cast(),
                invoke,
            };
            // SAFETY: host outlives the rejected startup and no client escapes.
            let runtime = unsafe { RuntimeClient::from_endpoint(endpoint) };
            assert!(UpdaterModule::start(runtime).is_err());
            assert!(host.commands.iter().any(|command| matches!(command,
                Command::NativeBind { service, .. } | Command::NativeBindTyped { service, .. }
                if service == expected
            )));
            assert!(
                !host
                    .commands
                    .iter()
                    .any(|command| matches!(command, Command::Register { .. }))
            );
        }
    }

    #[test]
    fn updater_defaults_keep_the_24_hour_seconds_interval_and_zero_disables_checks() {
        let defaults = Settings::default();
        assert_eq!(defaults.update_check_interval, 86_400);
        assert!(!defaults.notify_pre_releases);
        assert_eq!(DEFAULT_UPDATE_CHECK_INTERVAL_SECONDS, 86_400);
        assert_eq!(MAX_UPDATE_CHECK_INTERVAL_SECONDS, 2_147_483_647);

        let now = Instant::now();
        assert_eq!(periodic_deadline(now, 0), None);
        assert_eq!(
            periodic_deadline(now, 60),
            now.checked_add(Duration::from_secs(60))
        );
        assert!(periodic_check_eligible(60, None, now));
        assert!(!periodic_check_eligible(0, None, now));
        assert!(!periodic_check_eligible(
            60,
            Some(now - Duration::from_secs(59)),
            now
        ));
        assert!(periodic_check_eligible(
            60,
            Some(now - Duration::from_secs(60)),
            now
        ));

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

    #[test]
    fn stream_session_ids_are_bounded_and_safe_for_idempotency_keys() {
        assert!(valid_stream_session_id("stream-session-0000000000000001"));
        assert!(!valid_stream_session_id(""));
        assert!(!valid_stream_session_id("../session"));
        assert!(!valid_stream_session_id(&"a".repeat(129)));
        assert!(!valid_stream_session_id("stream-session-not-an-id"));
    }
}
