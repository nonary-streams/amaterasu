use crate::settings::{ActivityState, ActivityUpdate, TraySettings, UpdateNotice};
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::thread;
use std::time::{Duration, Instant};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

const MAX_QUEUED_TRAY_EVENTS: usize = 32;
const OPEN_RELEASE_ACTION: u32 = 103;
const OPEN_CLIENTS_ACTION: u32 = 101;
const OPEN_DASHBOARD_ACTION: u32 = 102;

const START_RETRY_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    NotReady,
    Disabled,
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    Notified,
    Disabled,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartState {
    Ready,
    Pending,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuAction {
    OpenUi,
    CheckUpdates,
    Restart,
    Quit,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrayEvent {
    Menu { action: MenuAction },
    OpenClients,
    OpenDashboard,
    OpenRelease { release_url: String },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrayEventBatch {
    pub events: Vec<TrayEvent>,
    pub dropped: u64,
}

/// Desktop-specific operations are injectable so lifecycle policy can be tested
/// without opening a live session bus or creating an operating-system tray icon.
pub trait TrayBackend {
    fn start(&mut self) -> StartState;
    fn is_running(&self) -> bool;
    fn notify_update(&mut self, notice: &UpdateNotice) -> bool;
    fn set_activity(&mut self, update: &ActivityUpdate) -> bool;
    fn notify_pairing(&mut self) -> bool;
    fn notify_vigem_missing(&mut self) -> bool;
    fn take_events(&mut self) -> TrayEventBatch {
        TrayEventBatch::default()
    }
    /// Request shutdown and return true only after the native worker retires.
    fn stop(&mut self) -> bool;
}

pub struct TrayRuntime<B: TrayBackend> {
    backend: B,
    settings: Option<TraySettings>,
    availability: Availability,
    retry_at: Option<Instant>,
    shutdown: bool,
}

impl<B: TrayBackend> TrayRuntime<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            settings: None,
            availability: Availability::NotReady,
            retry_at: None,
            shutdown: false,
        }
    }

    pub fn availability(&self) -> Availability {
        self.availability
    }

    pub fn adopt_settings(&mut self, settings: TraySettings, now: Instant) {
        let changed = self.settings.as_ref() != Some(&settings);
        self.settings = Some(settings);
        if changed {
            self.retry_at = None;
        }
        self.reconcile(now);
    }

    pub fn reconcile(&mut self, now: Instant) {
        if self.shutdown {
            return;
        }
        let Some(settings) = self.settings.as_ref() else {
            self.availability = Availability::NotReady;
            return;
        };

        if !settings.system_tray {
            self.backend.stop();
            self.retry_at = None;
            self.availability = Availability::Disabled;
            return;
        }

        if self.backend.is_running() {
            self.retry_at = None;
            self.availability = Availability::Available;
            return;
        }

        if self.availability == Availability::Available {
            // Reap an event loop that ended unexpectedly before retrying it.
            self.backend.stop();
            self.availability = Availability::Unavailable;
            self.retry_at = Some(now + START_RETRY_INTERVAL);
            return;
        }

        if self.retry_at.is_some_and(|retry_at| now < retry_at) {
            self.availability = Availability::Unavailable;
            return;
        }

        match self.backend.start() {
            StartState::Ready => {
                self.retry_at = None;
                self.availability = Availability::Available;
            }
            StartState::Pending => {
                self.retry_at = None;
                self.availability = Availability::Unavailable;
            }
            StartState::Failed => {
                self.retry_at = Some(now + START_RETRY_INTERVAL);
                self.availability = Availability::Unavailable;
            }
        }
    }

    pub fn quiesce(&mut self) -> bool {
        self.shutdown = true;
        self.availability = Availability::Unavailable;
        self.backend.stop()
    }

    pub fn notify_update(&mut self, notice: &UpdateNotice, now: Instant) -> Delivery {
        self.deliver(now, |backend| backend.notify_update(notice))
    }

    pub fn set_activity(&mut self, update: &ActivityUpdate, now: Instant) -> Delivery {
        self.deliver(now, |backend| backend.set_activity(update))
    }

    pub fn notify_pairing(&mut self, now: Instant) -> Delivery {
        self.deliver(now, TrayBackend::notify_pairing)
    }

    pub fn notify_vigem_missing(&mut self, now: Instant) -> Delivery {
        self.deliver(now, TrayBackend::notify_vigem_missing)
    }

    pub fn take_events(&mut self) -> TrayEventBatch {
        self.backend.take_events()
    }

    fn deliver(&mut self, now: Instant, operation: impl FnOnce(&mut B) -> bool) -> Delivery {
        if self.availability == Availability::NotReady {
            return Delivery::Unavailable;
        }
        if self.availability == Availability::Disabled {
            return Delivery::Disabled;
        }
        self.reconcile(now);
        if self.availability != Availability::Available {
            return Delivery::Unavailable;
        }
        if operation(&mut self.backend) {
            Delivery::Notified
        } else {
            self.backend.stop();
            self.availability = Availability::Unavailable;
            self.retry_at = Some(now + START_RETRY_INTERVAL);
            Delivery::Unavailable
        }
    }
}

impl<B: TrayBackend> Drop for TrayRuntime<B> {
    fn drop(&mut self) {
        self.backend.stop();
    }
}

/// Supported user-session adapter. `ldtray` loads libdbus or Win32 shell APIs
/// at runtime, so a headless or non-interactive host can report unavailable
/// without making the whole component fail to load.
#[derive(Default)]
pub struct DesktopTrayBackend {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    running: Option<RunningTray>,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
struct RunningTray {
    handle: Option<ldtray::TrayHandle>,
    ready: mpsc::Receiver<ldtray::TrayHandle>,
    thread: thread::JoinHandle<ldtray::Result<()>>,
    cancelled: Arc<AtomicBool>,
    quit_sent: bool,
    event_queue: Arc<Mutex<NativeEventQueue>>,
    release_url: Arc<Mutex<Option<String>>>,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[derive(Default)]
struct NativeEventQueue {
    pending: VecDeque<TrayEvent>,
    dropped: u64,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
impl NativeEventQueue {
    fn push(&mut self, event: TrayEvent) {
        if self.pending.len() == MAX_QUEUED_TRAY_EVENTS {
            self.pending.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.pending.push_back(event);
    }

    fn drain(&mut self) -> TrayEventBatch {
        TrayEventBatch {
            events: self.pending.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
        }
    }
}

impl TrayBackend for DesktopTrayBackend {
    fn start(&mut self) -> StartState {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            if let Some(running) = self.running.as_mut() {
                if running.cancelled.load(Ordering::Acquire) {
                    return if self.stop() {
                        StartState::Failed
                    } else {
                        StartState::Pending
                    };
                }
                if running.handle.is_none() {
                    running.handle = running.ready.try_recv().ok();
                }
                if running.thread.is_finished() {
                    let running = self.running.take().expect("finished tray exists");
                    let _ = running.thread.join();
                    return StartState::Failed;
                }
                return if running.handle.is_some() {
                    StartState::Ready
                } else {
                    StartState::Pending
                };
            }
            let event_queue = Arc::new(Mutex::new(NativeEventQueue::default()));
            let thread_event_queue = Arc::clone(&event_queue);
            let release_url = Arc::new(Mutex::new(None));
            let event_release_url = Arc::clone(&release_url);
            let cancelled = Arc::new(AtomicBool::new(false));
            let thread_cancelled = Arc::clone(&cancelled);
            let (ready_tx, ready) = mpsc::sync_channel(1);
            let thread = match thread::Builder::new()
                .name("amaterasu-system-tray".into())
                .spawn(move || {
                    let menu = ldtray::Menu::new()
                        .item(ldtray::MenuItem::button(1, "Open Amaterasu"))
                        .item(ldtray::MenuItem::separator())
                        .item(ldtray::MenuItem::button(2, "Check for Update"))
                        .item(ldtray::MenuItem::button(3, "Restart"))
                        .item(ldtray::MenuItem::button(4, "Quit"));
                    let tray = ldtray::Tray::new(
                        ldtray::TrayConfig::new(tray_icon(ActivityState::Stopped))
                            .tooltip("Amaterasu")
                            .menu(menu),
                    )?;
                    if thread_cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let _ = ready_tx.send(tray.handle());
                    if thread_cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    tray.run(|event| {
                        if let Some(event) = translate_event(event, &event_release_url)
                            && let Ok(mut queue) = thread_event_queue.lock()
                        {
                            queue.push(event);
                        }
                    })
                }) {
                Ok(thread) => thread,
                Err(_) => return StartState::Failed,
            };
            self.running = Some(RunningTray {
                handle: None,
                ready,
                thread,
                cancelled,
                quit_sent: false,
                event_queue,
                release_url,
            });
            StartState::Pending
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            StartState::Failed
        }
    }

    fn is_running(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            self.running.as_ref().is_some_and(|running| {
                running.handle.is_some()
                    && !running.cancelled.load(Ordering::Acquire)
                    && !running.thread.is_finished()
            })
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            false
        }
    }

    fn notify_update(&mut self, notice: &UpdateNotice) -> bool {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let Some(running) = self.running.as_ref() else {
                return false;
            };
            if running.thread.is_finished() {
                return false;
            }
            let Some(handle) = running.handle.as_ref() else {
                return false;
            };
            let body = format!(
                "Version {} is available.\n{}",
                notice.version, notice.release_url
            );
            let Ok(mut release_url) = running.release_url.lock() else {
                return false;
            };
            *release_url = Some(notice.release_url.clone());
            handle
                .notify(
                    ldtray::Notification::new("Amaterasu update available", body)
                        .action(OPEN_RELEASE_ACTION, "View release"),
                )
                .is_ok()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = notice;
            false
        }
    }

    fn set_activity(&mut self, update: &ActivityUpdate) -> bool {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let Some(running) = self.running.as_ref() else {
                return false;
            };
            if running.thread.is_finished() {
                return false;
            }
            let Some(handle) = running.handle.as_ref() else {
                return false;
            };
            let (tooltip, title, body) = activity_notice(update);
            let icon = tray_icon(update.state);
            handle.set_icon(icon.clone()).is_ok()
                && handle.set_tooltip(tooltip).is_ok()
                && handle
                    .notify(ldtray::Notification::new(title, body).with_icon(icon))
                    .is_ok()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = update;
            false
        }
    }

    fn notify_pairing(&mut self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let Some(running) = self.running.as_ref() else {
                return false;
            };
            if running.thread.is_finished() {
                return false;
            }
            let Some(handle) = running.handle.as_ref() else {
                return false;
            };
            let icon = tray_icon_with_mark([238, 97, 104, 255]);
            handle.set_icon(icon.clone()).is_ok()
                && handle.set_tooltip("Amaterasu — pairing required").is_ok()
                && handle
                    .notify(
                        ldtray::Notification::new(
                            "Incoming Pairing Request",
                            "Click to complete the pairing process.",
                        )
                        .with_icon(icon)
                        .action(OPEN_CLIENTS_ACTION, "Open clients"),
                    )
                    .is_ok()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            false
        }
    }

    fn notify_vigem_missing(&mut self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            let Some(running) = self.running.as_ref() else {
                return false;
            };
            if running.thread.is_finished() {
                return false;
            }
            let Some(handle) = running.handle.as_ref() else {
                return false;
            };
            let icon = tray_icon(ActivityState::Stopped);
            handle.set_icon(icon.clone()).is_ok()
                && handle
                    .set_tooltip("Amaterasu — gamepad input unavailable")
                    .is_ok()
                && handle
                    .notify(
                        ldtray::Notification::new(
                            "Gamepad Input Unavailable",
                            "ViGEm is not installed. Click for setup information.",
                        )
                        .with_icon(icon)
                        .action(OPEN_DASHBOARD_ACTION, "Open dashboard"),
                    )
                    .is_ok()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            false
        }
    }

    fn take_events(&mut self) -> TrayEventBatch {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            self.running
                .as_ref()
                .and_then(|running| running.event_queue.lock().ok())
                .map_or_else(TrayEventBatch::default, |mut queue| queue.drain())
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            TrayEventBatch::default()
        }
    }

    fn stop(&mut self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some(running) = self.running.as_mut() {
            running.cancelled.store(true, Ordering::Release);
            if running.handle.is_none() {
                running.handle = running.ready.try_recv().ok();
            }
            if !running.quit_sent
                && let Some(handle) = &running.handle
            {
                let _ = handle.quit();
                running.quit_sent = true;
            }
            if !running.thread.is_finished() {
                return false;
            }
            let running = self.running.take().expect("finished tray exists");
            let _ = running.thread.join();
        }
        true
    }
}

impl Drop for DesktopTrayBackend {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if let Some(mut running) = self.running.take() {
            running.cancelled.store(true, Ordering::Release);
            // Initialization may still be obtaining native resources. Wait
            // for either its handle or thread exit before joining, so a racing
            // event loop receives quit and cannot outlive this library.
            if running.handle.is_none() {
                running.handle = running.ready.recv().ok();
            }
            if !running.quit_sent
                && let Some(handle) = &running.handle
            {
                let _ = handle.quit();
            }
            let _ = running.thread.join();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn tray_icon(state: ActivityState) -> ldtray::Icon {
    let mark = match state {
        ActivityState::Stopped => [91, 224, 195, 255],
        ActivityState::Playing => [100, 220, 115, 255],
        ActivityState::Pausing => [245, 187, 76, 255],
    };
    tray_icon_with_mark(mark)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn tray_icon_with_mark(mark: [u8; 4]) -> ldtray::Icon {
    const PATTERN: [&str; 8] = [
        "00111100", "01100110", "11000011", "11000011", "11111111", "11000011", "11000011",
        "11000011",
    ];
    const SIDE: usize = 16;
    const BACKGROUND: [u8; 4] = [24, 38, 60, 255];
    let mut rgba = Vec::with_capacity(SIDE * SIDE * 4);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let pattern_row = PATTERN[y / 2].as_bytes();
            let is_mark = pattern_row[x / 2] == b'1';
            rgba.extend_from_slice(if is_mark { &mark } else { &BACKGROUND });
        }
    }
    ldtray::Icon::from_rgba(SIDE as u32, SIDE as u32, rgba)
        .expect("the fixed tray icon has a complete RGBA buffer")
}

fn activity_notice(update: &ActivityUpdate) -> (String, &'static str, String) {
    match update.state {
        ActivityState::Playing => {
            let body = format!("Streaming started for {}", update.app_name);
            (body.clone(), "Stream Started", body)
        }
        ActivityState::Pausing => {
            let body = format!("Streaming paused for {}", update.app_name);
            (body.clone(), "Stream Paused", body)
        }
        ActivityState::Stopped => {
            let body = format!("Application {} successfully stopped", update.app_name);
            ("Amaterasu".into(), "Application Stopped", body)
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn translate_event(event: ldtray::Event, release_url: &Mutex<Option<String>>) -> Option<TrayEvent> {
    match event {
        ldtray::Event::Menu(id) => Some(TrayEvent::Menu {
            action: match id.0 {
                1 => MenuAction::OpenUi,
                2 => MenuAction::CheckUpdates,
                3 => MenuAction::Restart,
                4 => MenuAction::Quit,
                _ => return None,
            },
        }),
        ldtray::Event::NotificationAction(action) if action.0 == OPEN_CLIENTS_ACTION => {
            Some(TrayEvent::OpenClients)
        }
        ldtray::Event::NotificationAction(action) if action.0 == OPEN_DASHBOARD_ACTION => {
            Some(TrayEvent::OpenDashboard)
        }
        ldtray::Event::NotificationAction(action) if action.0 == OPEN_RELEASE_ACTION => release_url
            .lock()
            .ok()
            .and_then(|release_url| release_url.clone())
            .map(|release_url| TrayEvent::OpenRelease { release_url }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeTrayBackend {
        starts: usize,
        stops: usize,
        notifications: Vec<UpdateNotice>,
        activity_updates: Vec<ActivityUpdate>,
        pairing_notifications: usize,
        vigem_notifications: usize,
        events: Vec<TrayEvent>,
        start_succeeds: bool,
        notification_succeeds: bool,
        running: bool,
    }

    impl TrayBackend for FakeTrayBackend {
        fn start(&mut self) -> StartState {
            self.starts += 1;
            self.running = self.start_succeeds;
            if self.running {
                StartState::Ready
            } else {
                StartState::Failed
            }
        }

        fn is_running(&self) -> bool {
            self.running
        }

        fn notify_update(&mut self, notice: &UpdateNotice) -> bool {
            if self.running && self.notification_succeeds {
                self.notifications.push(notice.clone());
                true
            } else {
                false
            }
        }

        fn set_activity(&mut self, update: &ActivityUpdate) -> bool {
            if self.running && self.notification_succeeds {
                self.activity_updates.push(update.clone());
                true
            } else {
                false
            }
        }

        fn notify_pairing(&mut self) -> bool {
            if self.running && self.notification_succeeds {
                self.pairing_notifications += 1;
                true
            } else {
                false
            }
        }

        fn notify_vigem_missing(&mut self) -> bool {
            if self.running && self.notification_succeeds {
                self.vigem_notifications += 1;
                true
            } else {
                false
            }
        }

        fn take_events(&mut self) -> TrayEventBatch {
            TrayEventBatch {
                events: std::mem::take(&mut self.events),
                dropped: 0,
            }
        }

        fn stop(&mut self) -> bool {
            if self.running {
                self.stops += 1;
            }
            self.running = false;
            true
        }
    }

    fn notice() -> UpdateNotice {
        UpdateNotice::parse(
            "v2.0.0".into(),
            "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0".into(),
        )
        .unwrap()
    }

    #[test]
    fn disabled_setting_does_not_start_or_notify() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: true,
            notification_succeeds: true,
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);

        runtime.adopt_settings(TraySettings { system_tray: false }, now);
        assert_eq!(runtime.availability(), Availability::Disabled);
        assert_eq!(runtime.notify_update(&notice(), now), Delivery::Disabled);
        assert_eq!(
            runtime.set_activity(
                &ActivityUpdate {
                    state: ActivityState::Playing,
                    app_name: "Game".into(),
                },
                now
            ),
            Delivery::Disabled
        );
        assert_eq!(runtime.notify_pairing(now), Delivery::Disabled);
        assert_eq!(runtime.notify_vigem_missing(now), Delivery::Disabled);
        assert_eq!(runtime.backend.starts, 0);
        assert!(runtime.backend.notifications.is_empty());
        assert!(runtime.backend.activity_updates.is_empty());
        assert_eq!(runtime.backend.pairing_notifications, 0);
        assert_eq!(runtime.backend.vigem_notifications, 0);
    }

    #[test]
    fn enabled_setting_starts_icon_and_queues_update_notice() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: true,
            notification_succeeds: true,
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);

        runtime.adopt_settings(TraySettings::default(), now);
        assert_eq!(runtime.availability(), Availability::Available);
        assert_eq!(runtime.notify_update(&notice(), now), Delivery::Notified);
        assert_eq!(runtime.backend.notifications, vec![notice()]);
    }

    #[test]
    fn unavailable_backend_is_nonfatal_and_retried_after_backoff() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: false,
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);

        runtime.adopt_settings(TraySettings::default(), now);
        assert_eq!(runtime.availability(), Availability::Unavailable);
        assert_eq!(runtime.notify_update(&notice(), now), Delivery::Unavailable);
        runtime.reconcile(now + START_RETRY_INTERVAL - Duration::from_millis(1));
        assert_eq!(runtime.backend.starts, 1);

        runtime.backend.start_succeeds = true;
        runtime.reconcile(now + START_RETRY_INTERVAL);
        assert_eq!(runtime.availability(), Availability::Available);
        assert_eq!(runtime.backend.starts, 2);
    }

    #[test]
    fn disabling_an_active_tray_stops_its_backend() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: true,
            notification_succeeds: true,
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);
        runtime.adopt_settings(TraySettings::default(), now);
        runtime.adopt_settings(TraySettings { system_tray: false }, now);

        assert_eq!(runtime.availability(), Availability::Disabled);
        assert_eq!(runtime.backend.stops, 1);
        assert_eq!(runtime.notify_update(&notice(), now), Delivery::Disabled);
    }

    #[test]
    fn failed_notification_marks_backend_unavailable_and_stops_it() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: true,
            notification_succeeds: false,
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);
        runtime.adopt_settings(TraySettings::default(), now);

        assert_eq!(runtime.notify_update(&notice(), now), Delivery::Unavailable);
        assert_eq!(runtime.availability(), Availability::Unavailable);
        assert_eq!(runtime.backend.stops, 1);
    }

    #[test]
    fn enabled_runtime_dispatches_activity_and_pairing_notices() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: true,
            notification_succeeds: true,
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);
        runtime.adopt_settings(TraySettings::default(), now);
        let update = ActivityUpdate {
            state: ActivityState::Playing,
            app_name: "Game".into(),
        };

        assert_eq!(runtime.set_activity(&update, now), Delivery::Notified);
        assert_eq!(runtime.notify_pairing(now), Delivery::Notified);
        assert_eq!(runtime.notify_vigem_missing(now), Delivery::Notified);
        assert_eq!(runtime.backend.activity_updates, vec![update]);
        assert_eq!(runtime.backend.pairing_notifications, 1);
        assert_eq!(runtime.backend.vigem_notifications, 1);
    }

    #[test]
    fn native_event_queue_is_drained_as_typed_actions() {
        let now = Instant::now();
        let backend = FakeTrayBackend {
            start_succeeds: true,
            events: vec![
                TrayEvent::Menu {
                    action: MenuAction::OpenUi,
                },
                TrayEvent::OpenClients,
            ],
            ..FakeTrayBackend::default()
        };
        let mut runtime = TrayRuntime::new(backend);
        runtime.adopt_settings(TraySettings::default(), now);

        assert_eq!(
            runtime.take_events(),
            TrayEventBatch {
                events: vec![
                    TrayEvent::Menu {
                        action: MenuAction::OpenUi,
                    },
                    TrayEvent::OpenClients,
                ],
                dropped: 0,
            }
        );
        assert_eq!(runtime.take_events(), TrayEventBatch::default());
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn native_event_queue_reports_overflow_and_keeps_bounded_recent_events() {
        let mut queue = NativeEventQueue::default();
        for _ in 0..MAX_QUEUED_TRAY_EVENTS {
            queue.push(TrayEvent::OpenClients);
        }
        queue.push(TrayEvent::OpenDashboard);

        let batch = queue.drain();
        assert_eq!(batch.events.len(), MAX_QUEUED_TRAY_EVENTS);
        assert_eq!(batch.events.first(), Some(&TrayEvent::OpenClients));
        assert_eq!(batch.events.last(), Some(&TrayEvent::OpenDashboard));
        assert_eq!(batch.dropped, 1);
        assert_eq!(queue.drain(), TrayEventBatch::default());
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn native_menu_and_notification_events_translate_to_typed_values() {
        let release_url = Mutex::new(Some(
            "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0".into(),
        ));
        assert_eq!(
            translate_event(ldtray::Event::Menu(ldtray::MenuId(3)), &release_url),
            Some(TrayEvent::Menu {
                action: MenuAction::Restart,
            })
        );
        assert_eq!(
            translate_event(
                ldtray::Event::NotificationAction(ldtray::ActionId(OPEN_CLIENTS_ACTION)),
                &release_url
            ),
            Some(TrayEvent::OpenClients)
        );
        assert_eq!(
            translate_event(
                ldtray::Event::NotificationAction(ldtray::ActionId(OPEN_DASHBOARD_ACTION)),
                &release_url
            ),
            Some(TrayEvent::OpenDashboard)
        );
        assert_eq!(
            translate_event(
                ldtray::Event::NotificationAction(ldtray::ActionId(OPEN_RELEASE_ACTION)),
                &release_url
            ),
            Some(TrayEvent::OpenRelease {
                release_url: release_url.lock().unwrap().clone().unwrap(),
            })
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn shutdown_during_initialization_waits_without_blocking_or_detaching_worker() {
        let (release, hold) = mpsc::channel();
        let (ready, ready_rx) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let thread = thread::spawn(move || {
            let _keep_sender_alive = ready;
            hold.recv().unwrap();
            assert!(worker_cancelled.load(Ordering::Acquire));
            Ok(())
        });
        let backend = DesktopTrayBackend {
            running: Some(RunningTray {
                handle: None,
                ready: ready_rx,
                thread,
                cancelled,
                quit_sent: false,
                event_queue: Arc::new(Mutex::new(NativeEventQueue::default())),
                release_url: Arc::new(Mutex::new(None)),
            }),
        };
        let mut runtime = TrayRuntime::new(backend);
        let start = Instant::now();
        assert!(!runtime.quiesce());
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(runtime.backend.running.is_some());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !runtime.quiesce() {
            assert!(Instant::now() < deadline, "tray worker failed to retire");
            thread::yield_now();
        }
        assert!(runtime.backend.running.is_none());
    }
}
