//! User-session tray icon and desktop notifications.
mod backend;
mod product_settings;
mod protocol;
mod settings;

use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use backend::{Availability, Delivery, DesktopTrayBackend, TrayEvent, TrayRuntime};
use product_settings::{ProductSettings, UpdateCompletion};
use protocol::{Operation, SERVICE};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::Instant;

const MAX_DEFERRED_SETTINGS_REQUESTS: usize = 32;
const MAX_PENDING_TRAY_EVENTS: usize = 32;

struct SystemTrayModule {
    runtime: RuntimeClient,
    product_settings: ProductSettings,
    tray: TrayRuntime<DesktopTrayBackend>,
    deferred_settings: VecDeque<(u64, Operation)>,
    pending_completions: VecDeque<(u64, Value)>,
    tray_events: TrayEventQueue,
    shutdown_announced: bool,
}

impl ManagedModule for SystemTrayModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "system tray startup randomness unavailable")?;
        let startup_id: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            product_settings: ProductSettings::new(&startup_id),
            tray: TrayRuntime::new(DesktopTrayBackend::default()),
            deferred_settings: VecDeque::new(),
            pending_completions: VecDeque::new(),
            tray_events: TrayEventQueue::default(),
            shutdown_announced: false,
        })
    }

    fn prepare_shutdown(&mut self, _remaining_ms: u64) -> ModuleResult<()> {
        self.shutdown_announced = true;
        Ok(())
    }

    fn quiesce(&mut self) -> ModuleResult<bool> {
        self.shutdown_announced = true;
        while let Some((request, _)) = self.deferred_settings.pop_front() {
            self.complete(
                request,
                failure("shutting_down", "system tray is shutting down"),
            )?;
        }
        let runtime = &mut self.runtime;
        let settings_ready = self
            .product_settings
            .quiesce(&mut |command| runtime.send(command))?;
        let tray_ready = self.tray.quiesce();
        self.complete_settings_updates()?;
        let completions_ready = self.flush_completions();
        Ok(settings_ready && tray_ready && completions_ready)
    }

    fn tick(&mut self) -> ModuleResult<()> {
        if self.flush_completions() {
            match self.runtime.send(Command::Poll {})? {
                Response::Request {
                    id,
                    caller,
                    payload,
                    ..
                } => match serde_json::from_value::<Operation>(payload) {
                    Ok(operation) if operation.authorized_for(&caller) => {
                        self.accept_request(id, operation)?;
                    }
                    Ok(_) => self.complete(
                        id,
                        failure(
                            "permission_denied",
                            "caller is not authorized for this system tray operation",
                        ),
                    )?,
                    Err(_) => self.complete(
                        id,
                        failure(
                            "invalid_request",
                            "request does not match the system tray schema",
                        ),
                    )?,
                },
                Response::Pending => {}
                response => {
                    return Err(format!(
                        "unexpected system tray poll response: {response:?}"
                    ));
                }
            }
        }

        let runtime = &mut self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command))?;
        self.apply_settings();
        self.complete_settings_updates()?;
        self.drain_deferred_settings()?;
        self.tray.reconcile(Instant::now());
        self.collect_tray_events();
        self.flush_completions();
        Ok(())
    }
}

impl SystemTrayModule {
    fn accept_request(&mut self, request: u64, operation: Operation) -> ModuleResult<()> {
        if self.shutdown_announced
            && matches!(
                operation,
                Operation::SettingsGet {} | Operation::SettingsUpdate { .. }
            )
        {
            return self.complete(
                request,
                failure("shutting_down", "system tray is shutting down"),
            );
        }
        if matches!(
            operation,
            Operation::SettingsGet {} | Operation::SettingsUpdate { .. }
        ) && (!self.product_settings.is_ready() || self.product_settings.busy())
        {
            if self.deferred_settings.len() >= MAX_DEFERRED_SETTINGS_REQUESTS {
                return self.complete(
                    request,
                    failure(
                        "settings_busy",
                        "too many system tray settings requests are queued",
                    ),
                );
            }
            self.deferred_settings.push_back((request, operation));
            return Ok(());
        }
        self.handle_operation(request, operation)
    }

    fn handle_operation(&mut self, request: u64, operation: Operation) -> ModuleResult<()> {
        match operation {
            Operation::SettingsGet {} => {
                let (settings, revision) = self.product_settings.settings();
                self.complete(
                    request,
                    json!({
                        "ok":true,
                        "settings":settings,
                        "revision":revision,
                        "availability":availability_name(self.tray.availability()),
                    }),
                )
            }
            Operation::SettingsUpdate {
                settings,
                expected_revision,
            } => match self
                .product_settings
                .begin_update(request, expected_revision, settings)
            {
                Ok(()) => Ok(()),
                Err(response) => self.complete(request, response),
            },
            Operation::EventNext {} => {
                let response = self.tray_events.next();
                self.complete(request, response)
            }
            notification @ Operation::NotifyUpdate { .. } => {
                let notice = match notification.notification() {
                    Ok(notice) => notice,
                    Err(code) => {
                        return self
                            .complete(request, failure(code, "update notice failed validation"));
                    }
                };
                let delivery = self.tray.notify_update(&notice, Instant::now());
                self.complete(
                    request,
                    json!({"ok":true,"delivery":delivery_name(delivery)}),
                )
            }
            activity @ Operation::ActivityUpdate { .. } => {
                let update = match activity.activity() {
                    Ok(update) => update,
                    Err(code) => {
                        return self
                            .complete(request, failure(code, "activity update failed validation"));
                    }
                };
                let delivery = self.tray.set_activity(&update, Instant::now());
                self.complete(
                    request,
                    json!({"ok":true,"delivery":delivery_name(delivery)}),
                )
            }
            Operation::NotifyPairing {} => {
                let delivery = self.tray.notify_pairing(Instant::now());
                self.complete(
                    request,
                    json!({"ok":true,"delivery":delivery_name(delivery)}),
                )
            }
            Operation::NotifyVigemMissing {} => {
                let delivery = self.tray.notify_vigem_missing(Instant::now());
                self.complete(
                    request,
                    json!({"ok":true,"delivery":delivery_name(delivery)}),
                )
            }
        }
    }

    fn apply_settings(&mut self) {
        if self.product_settings.is_ready() {
            let (settings, _) = self.product_settings.settings();
            self.tray.adopt_settings(settings.clone(), Instant::now());
        }
    }

    fn complete_settings_updates(&mut self) -> ModuleResult<()> {
        while let Some(completion) = self.product_settings.take_completion() {
            match completion {
                UpdateCompletion::Applied {
                    request,
                    settings,
                    revision,
                } => {
                    self.tray.adopt_settings(settings.clone(), Instant::now());
                    self.complete(
                        request,
                        json!({
                            "ok":true,
                            "settings":settings,
                            "revision":revision,
                            "availability":availability_name(self.tray.availability()),
                        }),
                    )?;
                }
                UpdateCompletion::Failed { request, response } => {
                    self.complete(request, response)?;
                }
            }
        }
        Ok(())
    }

    fn drain_deferred_settings(&mut self) -> ModuleResult<()> {
        while self.product_settings.is_ready() && !self.product_settings.busy() {
            let Some((request, operation)) = self.deferred_settings.pop_front() else {
                break;
            };
            self.handle_operation(request, operation)?;
        }
        Ok(())
    }

    fn collect_tray_events(&mut self) {
        self.tray_events.push_batch(self.tray.take_events());
    }

    fn complete(&mut self, request: u64, payload: Value) -> ModuleResult<()> {
        self.pending_completions.push_back((request, payload));
        Ok(())
    }

    fn flush_completions(&mut self) -> bool {
        let runtime = &self.runtime;
        flush_completions(&mut self.pending_completions, &mut |command| {
            runtime.send(command)
        })
    }
}

fn flush_completions(
    pending: &mut VecDeque<(u64, Value)>,
    send: &mut impl FnMut(Command) -> ModuleResult<Response>,
) -> bool {
    while let Some((request, payload)) = pending.front() {
        match send(Command::Complete {
            request: *request,
            payload: payload.clone(),
        }) {
            Ok(Response::Ok) => {}
            Ok(Response::Error { code, .. })
                if matches!(
                    code.as_str(),
                    "cancelled" | "deadline_exceeded" | "request_ended" | "not_found"
                ) => {}
            _ => return false,
        }
        pending.pop_front();
    }
    true
}

#[derive(Default)]
struct TrayEventQueue {
    pending: VecDeque<TrayEvent>,
    dropped: u64,
}

impl TrayEventQueue {
    fn push_batch(&mut self, batch: backend::TrayEventBatch) {
        self.dropped = self.dropped.saturating_add(batch.dropped);
        for event in batch.events {
            self.push(event);
        }
    }

    fn push(&mut self, event: TrayEvent) {
        if self.pending.len() == MAX_PENDING_TRAY_EVENTS {
            self.pending.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.pending.push_back(event);
    }

    fn next(&mut self) -> Value {
        let event = self.pending.pop_front();
        let dropped = std::mem::take(&mut self.dropped);
        json!({"ok":true,"event":event,"dropped":dropped})
    }
}

fn availability_name(availability: Availability) -> &'static str {
    match availability {
        Availability::NotReady => "not_ready",
        Availability::Disabled => "disabled",
        Availability::Available => "available",
        Availability::Unavailable => "unavailable",
    }
}

fn delivery_name(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Notified => "notified",
        Delivery::Disabled => "disabled",
        Delivery::Unavailable => "unavailable",
    }
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}

amaterasu_module_sdk::export_module!(SystemTrayModule);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_completion_acknowledgement_keeps_exact_response_until_retired() {
        let payload = json!({"ok":true,"delivery":"notified"});
        let mut pending = VecDeque::from([(42, payload.clone())]);
        assert!(!flush_completions(&mut pending, &mut |command| {
            assert!(
                matches!(command, Command::Complete { request: 42, payload: ref value } if *value == payload)
            );
            Err("reply lost".into())
        }));
        assert_eq!(pending.len(), 1);
        assert!(flush_completions(&mut pending, &mut |command| {
            assert!(
                matches!(command, Command::Complete { request: 42, payload: ref value } if *value == payload)
            );
            Ok(Response::error("request_ended", "already completed"))
        }));
        assert!(pending.is_empty());
    }

    #[test]
    fn management_and_updater_response_shapes_are_stable() {
        assert_eq!(availability_name(Availability::NotReady), "not_ready");
        assert_eq!(availability_name(Availability::Disabled), "disabled");
        assert_eq!(delivery_name(Delivery::Notified), "notified");
        assert_eq!(delivery_name(Delivery::Disabled), "disabled");
        assert_eq!(delivery_name(Delivery::Unavailable), "unavailable");
        assert_eq!(
            failure("permission_denied", "no"),
            json!({"ok":false,"error":{"code":"permission_denied","message":"no"}})
        );
        assert_eq!(
            TrayEventQueue::default().next(),
            json!({"ok":true,"event":null,"dropped":0})
        );
        assert_eq!(
            serde_json::to_value(TrayEvent::OpenRelease {
                release_url: "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0".into(),
            })
            .unwrap(),
            json!({
                "kind":"open_release",
                "release_url":"https://github.com/Nonary/amaterasu/releases/tag/v2.0.0"
            })
        );
    }

    #[test]
    fn tray_event_queue_is_bounded_and_reports_dropped_actions() {
        let mut queue = TrayEventQueue::default();
        for _ in 0..MAX_PENDING_TRAY_EVENTS {
            queue.push(TrayEvent::OpenClients);
        }
        queue.push(TrayEvent::OpenDashboard);
        assert_eq!(queue.pending.len(), MAX_PENDING_TRAY_EVENTS);
        assert_eq!(queue.pending.front(), Some(&TrayEvent::OpenClients));
        assert_eq!(queue.pending.back(), Some(&TrayEvent::OpenDashboard));
        assert_eq!(queue.dropped, 1);
        assert_eq!(queue.next()["dropped"], 1);
        assert_eq!(queue.dropped, 0);
    }
}
