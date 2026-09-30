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
    tray_events: TrayEventQueue,
}

impl ManagedModule for SystemTrayModule {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.require_success(Command::Register {
            service: SERVICE.into(),
        })?;
        Ok(Self {
            runtime,
            product_settings: ProductSettings::new(),
            tray: TrayRuntime::new(DesktopTrayBackend::default()),
            deferred_settings: VecDeque::new(),
            tray_events: TrayEventQueue::default(),
        })
    }

    fn tick(&mut self) -> ModuleResult<()> {
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

        let runtime = &mut self.runtime;
        self.product_settings
            .advance(&mut |command| runtime.send(command))?;
        self.apply_settings();
        self.complete_settings_updates()?;
        self.drain_deferred_settings()?;
        self.tray.reconcile(Instant::now());
        self.collect_tray_events();
        Ok(())
    }
}

impl SystemTrayModule {
    fn accept_request(&mut self, request: u64, operation: Operation) -> ModuleResult<()> {
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

    fn complete(&self, request: u64, payload: Value) -> ModuleResult<()> {
        match self.runtime.send(Command::Complete { request, payload })? {
            Response::Ok => Ok(()),
            response => Err(format!(
                "unexpected system tray completion response: {response:?}"
            )),
        }
    }
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
