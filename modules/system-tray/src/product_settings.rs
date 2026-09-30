//! Versioned system-tray settings persisted through configuration product CAS.
use crate::settings::TraySettings;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};
use std::collections::VecDeque;

const CONFIGURATION_SERVICE: &str = "amaterasu.configuration/1";
const PRODUCT_VERSION: u32 = 1;
const CONFIGURATION_TIMEOUT_MS: u64 = 30_000;

type Sender<'a> = &'a mut dyn FnMut(Command) -> ModuleResult<Response>;

enum Work {
    StartupGet,
    Initialize,
    Update {
        management_request: u64,
        expected_revision: u64,
        desired: TraySettings,
    },
    ReconcileUpdate {
        management_request: u64,
        expected_revision: u64,
        desired: TraySettings,
    },
}

enum ChildOutcome {
    Payload(Value),
    Failure,
}

enum Call {
    Submit {
        payload: Value,
        operation_id: String,
        parent: Option<u64>,
        retry_ticks: u8,
    },
    Waiting {
        request: u64,
    },
    Releasing {
        request: u64,
        outcome: ChildOutcome,
    },
}

struct Pending {
    work: Work,
    call: Call,
}

pub enum UpdateCompletion {
    Applied {
        request: u64,
        settings: TraySettings,
        revision: u64,
    },
    Failed {
        request: u64,
        response: Value,
    },
}

pub struct ProductSettings {
    settings: TraySettings,
    revision: u64,
    ready: bool,
    next_operation: u64,
    pending: Option<Pending>,
    completions: VecDeque<UpdateCompletion>,
}

impl ProductSettings {
    pub fn new() -> Self {
        Self {
            settings: TraySettings::default(),
            revision: 0,
            ready: false,
            next_operation: 1,
            pending: Some(Pending {
                work: Work::StartupGet,
                call: Call::Submit {
                    payload: product_get(),
                    operation_id: "system-tray.configuration.startup".into(),
                    parent: None,
                    retry_ticks: 0,
                },
            }),
            completions: VecDeque::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn settings(&self) -> (&TraySettings, u64) {
        (&self.settings, self.revision)
    }

    pub fn begin_update(
        &mut self,
        request: u64,
        expected_revision: u64,
        desired: TraySettings,
    ) -> Result<(), Value> {
        if !self.ready {
            return Err(failure(
                "configuration_unavailable",
                "system tray settings are not loaded",
            ));
        }
        if self.pending.is_some() {
            return Err(failure(
                "settings_busy",
                "another system tray configuration operation is pending",
            ));
        }
        if expected_revision != self.revision {
            return Err(revision_conflict(self.revision));
        }
        self.pending = Some(Pending {
            work: Work::Update {
                management_request: request,
                expected_revision,
                desired: desired.clone(),
            },
            call: Call::Submit {
                payload: product_update(&desired, expected_revision),
                operation_id: self.next_operation_id("update"),
                parent: Some(request),
                retry_ticks: 0,
            },
        });
        Ok(())
    }

    pub fn take_completion(&mut self) -> Option<UpdateCompletion> {
        self.completions.pop_front()
    }

    pub fn advance(&mut self, send: Sender<'_>) -> ModuleResult<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };

        match pending.call {
            Call::Submit {
                payload,
                operation_id,
                parent,
                retry_ticks,
            } => {
                if retry_ticks > 0 {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Submit {
                            payload,
                            operation_id,
                            parent,
                            retry_ticks: retry_ticks - 1,
                        },
                    });
                    return Ok(());
                }
                match send(Command::Request {
                    service: CONFIGURATION_SERVICE.into(),
                    payload: payload.clone(),
                    timeout_ms: CONFIGURATION_TIMEOUT_MS,
                    operation_id: operation_id.clone(),
                    parent,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Waiting { request: id },
                        });
                    }
                    // A stable ID recovers a child operation accepted before its
                    // acknowledgement was lost by the host transport.
                    Err(_) => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Submit {
                                payload,
                                operation_id,
                                parent,
                                retry_ticks: 1,
                            },
                        });
                    }
                    Ok(Response::Error { .. }) | Ok(_) => {
                        self.on_submit_failure(pending.work);
                    }
                }
            }
            Call::Waiting { request } => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) | Err(_) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Waiting { request },
                    });
                }
                Ok(Response::Error { code, .. }) if code == "not_found" => {
                    self.on_child_missing(pending.work);
                }
                Ok(Response::Result { payload }) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Payload(payload),
                        },
                    });
                }
                Ok(_) => {
                    self.pending = Some(Pending {
                        work: pending.work,
                        call: Call::Releasing {
                            request,
                            outcome: ChildOutcome::Failure,
                        },
                    });
                }
            },
            Call::Releasing { request, outcome } => {
                match send(Command::ReleaseResult { request }) {
                    Ok(Response::Ok) => self.on_terminal(pending.work, outcome),
                    Ok(Response::Error { code, .. }) if code == "not_found" => {
                        self.on_terminal(pending.work, outcome)
                    }
                    Err(_) | Ok(_) => {
                        self.pending = Some(Pending {
                            work: pending.work,
                            call: Call::Releasing { request, outcome },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn on_submit_failure(&mut self, work: Work) {
        match work {
            Work::StartupGet | Work::Initialize => self.retry_startup(20),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            }
            | Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.start_reconciliation(management_request, expected_revision, desired, 20),
        }
    }

    fn on_child_missing(&mut self, work: Work) {
        match work {
            Work::StartupGet | Work::Initialize => self.retry_startup(1),
            Work::Update {
                management_request,
                expected_revision,
                desired,
            }
            | Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => self.start_reconciliation(management_request, expected_revision, desired, 1),
        }
    }

    fn on_terminal(&mut self, work: Work, outcome: ChildOutcome) {
        let ChildOutcome::Payload(payload) = outcome else {
            self.on_submit_failure(work);
            return;
        };

        match work {
            Work::StartupGet => match read_document(&payload) {
                Ok(Document::Found(settings, revision)) => self.adopt(settings, revision),
                Ok(Document::Absent) => self.start_initialization(),
                Err(_) => self.retry_startup(20),
            },
            Work::Initialize => match read_document(&payload) {
                Ok(Document::Found(settings, revision))
                    if revision == 1 && settings == TraySettings::default() =>
                {
                    self.adopt(settings, revision)
                }
                _ => self.retry_startup(1),
            },
            Work::Update {
                management_request,
                expected_revision,
                desired,
            } => match read_document(&payload) {
                Ok(Document::Found(settings, revision))
                    if expected_revision.checked_add(1) == Some(revision)
                        && settings == desired =>
                {
                    self.adopt(settings.clone(), revision);
                    self.complete_applied(management_request, settings, revision);
                }
                _ => self.start_reconciliation(management_request, expected_revision, desired, 0),
            },
            Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            } => match read_document(&payload) {
                Ok(Document::Found(settings, revision)) => {
                    self.adopt(settings.clone(), revision);
                    if expected_revision.checked_add(1) == Some(revision) && settings == desired {
                        self.complete_applied(management_request, settings, revision);
                    } else {
                        let response = if revision != expected_revision {
                            revision_conflict(revision)
                        } else {
                            failure(
                                "write_unresolved",
                                "durable system tray settings do not match the requested update",
                            )
                        };
                        self.complete_failure(management_request, response);
                    }
                }
                Ok(Document::Absent) => {
                    self.complete_failure(management_request, revision_conflict(0));
                    self.retry_startup(0);
                }
                Err(_) => {
                    self.start_reconciliation(management_request, expected_revision, desired, 20)
                }
            },
        }
    }

    fn adopt(&mut self, settings: TraySettings, revision: u64) {
        self.settings = settings;
        self.revision = revision;
        self.ready = true;
    }

    fn start_initialization(&mut self) {
        let settings = TraySettings::default();
        self.pending = Some(Pending {
            work: Work::Initialize,
            call: Call::Submit {
                payload: product_update(&settings, 0),
                operation_id: self.next_operation_id("initialize"),
                parent: None,
                retry_ticks: 0,
            },
        });
    }

    fn retry_startup(&mut self, retry_ticks: u8) {
        self.pending = Some(Pending {
            work: Work::StartupGet,
            call: Call::Submit {
                payload: product_get(),
                operation_id: "system-tray.configuration.startup".into(),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn start_reconciliation(
        &mut self,
        management_request: u64,
        expected_revision: u64,
        desired: TraySettings,
        retry_ticks: u8,
    ) {
        self.pending = Some(Pending {
            work: Work::ReconcileUpdate {
                management_request,
                expected_revision,
                desired,
            },
            call: Call::Submit {
                payload: product_get(),
                operation_id: self.next_operation_id("update-reconcile"),
                parent: None,
                retry_ticks,
            },
        });
    }

    fn complete_applied(&mut self, request: u64, settings: TraySettings, revision: u64) {
        self.completions.push_back(UpdateCompletion::Applied {
            request,
            settings,
            revision,
        });
    }

    fn complete_failure(&mut self, request: u64, response: Value) {
        self.completions
            .push_back(UpdateCompletion::Failed { request, response });
    }

    fn next_operation_id(&mut self, purpose: &str) -> String {
        let operation = self.next_operation;
        self.next_operation = self.next_operation.saturating_add(1);
        format!("system-tray.configuration.{purpose}.{operation}")
    }
}

impl Default for ProductSettings {
    fn default() -> Self {
        Self::new()
    }
}

enum Document {
    Absent,
    Found(TraySettings, u64),
}

fn product_get() -> Value {
    json!({"action":"product_get"})
}

fn product_update(settings: &TraySettings, expected_revision: u64) -> Value {
    json!({
        "action":"product_update",
        "expected_revision":expected_revision,
        "version":PRODUCT_VERSION,
        "values":{"system_tray":settings.system_tray},
    })
}

fn read_document(payload: &Value) -> Result<Document, &'static str> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("configuration request failed");
    }
    let revision = payload
        .get("revision")
        .and_then(Value::as_u64)
        .ok_or("configuration response has no revision")?;
    match payload.get("found").and_then(Value::as_bool) {
        Some(false)
            if revision == 0
                && payload.get("version").is_some_and(Value::is_null)
                && payload.get("values") == Some(&json!({})) =>
        {
            Ok(Document::Absent)
        }
        Some(true)
            if payload.get("version").and_then(Value::as_u64) == Some(PRODUCT_VERSION as u64)
                && revision > 0 =>
        {
            let values = payload
                .get("values")
                .and_then(Value::as_object)
                .ok_or("configuration values are not an object")?;
            if values.keys().any(|key| key != "system_tray") {
                return Err("configuration document has unknown system-tray fields");
            }
            let settings: TraySettings = serde_json::from_value(Value::Object(values.clone()))
                .map_err(|_| "configuration settings are malformed")?;
            Ok(Document::Found(settings, revision))
        }
        _ => Err("configuration product document is malformed or unsupported"),
    }
}

fn failure(code: &str, message: &str) -> Value {
    json!({"ok":false,"error":{"code":code,"message":message}})
}

fn revision_conflict(revision: u64) -> Value {
    json!({
        "ok":false,
        "error":{
            "code":"revision_conflict",
            "message":"system tray settings changed since the supplied revision",
            "revision":revision,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn advance_once(
        settings: &mut ProductSettings,
        expected: impl FnOnce(Command),
        response: Response,
    ) {
        let mut expected = Some(expected);
        settings
            .advance(&mut |command| {
                expected
                    .take()
                    .expect("settings worker sends one command per advance")(
                    command
                );
                Ok(response.clone())
            })
            .unwrap();
        assert!(expected.is_none());
    }

    fn expect_configuration_request(
        action: &'static str,
        validate_payload: impl FnOnce(&Value),
    ) -> impl FnOnce(Command) {
        move |command| match command {
            Command::Request {
                service, payload, ..
            } => {
                assert_eq!(service, CONFIGURATION_SERVICE);
                assert_eq!(payload["action"], action);
                validate_payload(&payload);
            }
            other => panic!("expected a configuration request, got {other:?}"),
        }
    }

    fn expect_take(request: u64) -> impl FnOnce(Command) {
        move |command| {
            assert!(matches!(command, Command::TakeResult { request: actual } if actual == request))
        }
    }

    fn expect_release(request: u64) -> impl FnOnce(Command) {
        move |command| {
            assert!(
                matches!(command, Command::ReleaseResult { request: actual } if actual == request)
            )
        }
    }

    fn found_document(system_tray: bool, revision: u64) -> Value {
        json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":revision,
            "values":{"system_tray":system_tray}
        })
    }

    fn initialize_default(settings: &mut ProductSettings) {
        advance_once(
            settings,
            expect_configuration_request("product_get", |_| {}),
            Response::Accepted { id: 10 },
        );
        advance_once(
            settings,
            expect_take(10),
            Response::Result {
                payload: json!({"ok":true,"found":false,"version":null,"revision":0,"values":{}}),
            },
        );
        advance_once(settings, expect_release(10), Response::Ok);
        advance_once(
            settings,
            expect_configuration_request("product_update", |payload| {
                assert_eq!(payload["expected_revision"], 0);
                assert_eq!(payload["version"], 1);
                assert_eq!(payload["values"], json!({"system_tray":true}));
            }),
            Response::Accepted { id: 11 },
        );
        advance_once(
            settings,
            expect_take(11),
            Response::Result {
                payload: found_document(true, 1),
            },
        );
        advance_once(settings, expect_release(11), Response::Ok);
        assert!(settings.is_ready());
        assert_eq!(settings.settings(), (&TraySettings::default(), 1));
    }

    #[test]
    fn reads_only_the_versioned_product_document_and_known_setting() {
        let found = json!({
            "ok":true,
            "found":true,
            "version":1,
            "revision":2,
            "values":{"system_tray":false}
        });
        assert!(matches!(
            read_document(&found),
            Ok(Document::Found(TraySettings { system_tray: false }, 2))
        ));
        assert!(matches!(
            read_document(
                &json!({"ok":true,"found":false,"version":null,"revision":0,"values":{}})
            ),
            Ok(Document::Absent)
        ));
        for invalid in [
            json!({"ok":true,"found":true,"version":2,"revision":1,"values":{"system_tray":true}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"system_tray":true,"unknown":false}}),
            json!({"ok":true,"found":true,"version":1,"revision":1,"values":{"system_tray":"yes"}}),
        ] {
            assert!(read_document(&invalid).is_err());
        }
    }

    #[test]
    fn settings_update_uses_revision_cas_and_configuration_product_shape() {
        assert_eq!(
            product_update(&TraySettings { system_tray: false }, 12),
            json!({
                "action":"product_update",
                "expected_revision":12,
                "version":1,
                "values":{"system_tray":false}
            })
        );
        assert_eq!(revision_conflict(13)["error"]["code"], "revision_conflict");
    }

    #[test]
    fn absent_document_is_initialized_once_with_the_default_enabled_setting() {
        let mut settings = ProductSettings::new();
        assert!(!settings.is_ready());
        initialize_default(&mut settings);
        assert!(settings.take_completion().is_none());
    }

    #[test]
    fn update_waits_for_durable_commit_before_completing_management_request() {
        let mut settings = ProductSettings::new();
        initialize_default(&mut settings);
        settings
            .begin_update(70, 1, TraySettings { system_tray: false })
            .unwrap();

        advance_once(
            &mut settings,
            expect_configuration_request("product_update", |payload| {
                assert_eq!(payload["expected_revision"], 1);
                assert_eq!(payload["values"], json!({"system_tray":false}));
            }),
            Response::Accepted { id: 20 },
        );
        assert!(settings.take_completion().is_none());
        advance_once(
            &mut settings,
            expect_take(20),
            Response::Result {
                payload: found_document(false, 2),
            },
        );
        advance_once(&mut settings, expect_release(20), Response::Ok);

        assert_eq!(
            settings.settings(),
            (&TraySettings { system_tray: false }, 2)
        );
        assert!(matches!(
            settings.take_completion(),
            Some(UpdateCompletion::Applied {
                request: 70,
                revision: 2,
                settings: TraySettings { system_tray: false }
            })
        ));
    }

    #[test]
    fn compare_and_swap_conflict_reloads_the_durable_setting_and_revision() {
        let mut settings = ProductSettings::new();
        initialize_default(&mut settings);
        settings
            .begin_update(71, 1, TraySettings { system_tray: false })
            .unwrap();

        advance_once(
            &mut settings,
            expect_configuration_request("product_update", |_| {}),
            Response::Accepted { id: 30 },
        );
        advance_once(
            &mut settings,
            expect_take(30),
            Response::Result {
                payload: json!({
                    "ok":false,
                    "error":{"code":"revision_conflict","message":"stale"}
                }),
            },
        );
        advance_once(&mut settings, expect_release(30), Response::Ok);
        advance_once(
            &mut settings,
            expect_configuration_request("product_get", |_| {}),
            Response::Accepted { id: 31 },
        );
        advance_once(
            &mut settings,
            expect_take(31),
            Response::Result {
                payload: found_document(true, 2),
            },
        );
        advance_once(&mut settings, expect_release(31), Response::Ok);

        assert_eq!(settings.settings(), (&TraySettings::default(), 2));
        assert!(matches!(
            settings.take_completion(),
            Some(UpdateCompletion::Failed { request: 71, response })
                if response["error"]["code"] == "revision_conflict"
                    && response["error"]["revision"] == 2
        ));
    }
}
