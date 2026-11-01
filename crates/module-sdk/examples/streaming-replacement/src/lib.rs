//! Independent generated library and certificate-bound policy implementation
//! used to qualify an unchanged streaming consumer. No host grants are installed.
use amaterasu_module_sdk::application_library::{
    policy::{CommandSpec, Identity, PreparationStep, Snapshot},
    streaming_policy::{Adapter, Admission, Provider, Request},
    v2::{
        self, Capabilities, Cursor, Entry, Installed, LaunchTarget, Page, Query, Readiness,
        adapter::{CallerPolicy, Library, LibraryAdapter},
    },
};
use amaterasu_module_sdk::{Command, ManagedModule, ModuleResult, Response, RuntimeClient};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    certificate_id: String,
    #[serde(default)]
    mode: String,
}
struct GeneratedLibrary {
    mode: String,
    resolutions: u32,
}
impl Library for GeneratedLibrary {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            version: 2,
            features: [v2::RESOLVE_LAUNCH.into()].into(),
        }
    }
    fn list(
        &mut self,
        _: &Identity<'_>,
        _: &Query,
        cursor: Option<&Cursor>,
        _: usize,
    ) -> Result<Page, &'static str> {
        if cursor.is_some() {
            return Err("invalid_cursor");
        }
        Ok(Page {
            revision: "generated:one".into(),
            entries: vec![self.entry()],
            next_cursor: None,
        })
    }
    fn resolve(&mut self, _: &Identity<'_>, id: &str) -> Result<Entry, &'static str> {
        if id != "opaque/game" {
            return Err("not_found");
        }
        Ok(self.entry())
    }
    fn resolve_launch(
        &mut self,
        _: &Identity<'_>,
        id: &str,
        revision: &str,
    ) -> Result<LaunchTarget, &'static str> {
        self.resolutions += 1;
        if id != "opaque/game"
            || revision != "entry:opaque"
            || (self.mode.starts_with("resume_") && self.resolutions > 2)
        {
            return Err("revision_conflict");
        }
        Ok(LaunchTarget {
            id: id.into(),
            revision: revision.into(),
            executor: "runner".into(),
            application_id: "fixture-game".into(),
        })
    }
}
impl GeneratedLibrary {
    fn entry(&self) -> Entry {
        Entry {
            id: "opaque/game".into(),
            revision: "entry:opaque".into(),
            name: "Generated game".into(),
            description: None,
            tags: vec![],
            source: None,
            portable_ids: vec![],
            installed: Installed::Unknown,
            readiness: Readiness::Ready,
            unavailable_reason: None,
        }
    }
}
struct GeneratedPolicy {
    certificate_id: String,
    mode: String,
    calls: u64,
}
impl Provider for GeneratedPolicy {
    fn admit(&mut self, _: &Identity<'_>, request: &Request) -> Result<Admission, &'static str> {
        if request.certificate_id != self.certificate_id
            || request.application.library != "games"
            || request.application.id != "opaque/game"
            || request.entry_revision != "entry:opaque"
        {
            return Err("not_authorized");
        }
        self.calls += 1;
        let preparation = if self.mode == "failed_preparation" {
            vec![PreparationStep {
                run: CommandSpec {
                    executable: "/synthetic/prepare".into(),
                    args: vec![],
                    environment: BTreeMap::new(),
                    cwd: None,
                    timeout_ms: 10_000,
                },
                undo: None,
            }]
        } else {
            vec![]
        };
        Ok(Admission {
            certificate_id: request.certificate_id.clone(),
            policy: Snapshot {
                application: request.application.clone(),
                entry_revision: request.entry_revision.clone(),
                policy_revision: if (self.mode == "stale" && self.calls > 1)
                    || (self.mode.starts_with("resume_") && self.calls > 2)
                {
                    "changed-policy"
                } else {
                    "generated-policy:two"
                }
                .into(),
                working_directory: None,
                exit_timeout_seconds: 10,
                preparation,
            },
            config_overrides: json!({}),
            application_display_settings: None,
            exclude_global_preparation: false,
        })
    }
}
// Independently implemented synthetic executor: it never starts an OS process.
struct GeneratedExecutor {
    incarnation: String,
    mode: String,
    operations: BTreeMap<
        (String, String),
        (
            String,
            Option<String>,
            amaterasu_module_sdk::application_library::provider_lifecycle::State,
        ),
    >,
    reservations: BTreeMap<
        (String, String, String),
        amaterasu_module_sdk::application_library::limiter_target::Request,
    >,
}
impl amaterasu_module_sdk::application_library::provider_adapter::Provider for GeneratedExecutor {
    fn capabilities(&self) -> amaterasu_module_sdk::application_library::provider::Capabilities {
        amaterasu_module_sdk::application_library::provider::Capabilities {
            discovery: false,
            artwork: false,
            lifecycle: true,
        }
    }
    fn discover(
        &mut self,
        _: &Identity<'_>,
        _: Option<&str>,
        _: usize,
    ) -> Result<amaterasu_module_sdk::application_library::provider::DiscoveryPage, &'static str>
    {
        Err("unsupported")
    }
    fn artwork(
        &mut self,
        _: &Identity<'_>,
        _: &amaterasu_module_sdk::application_library::provider::ArtworkReference,
        _: u64,
    ) -> Result<amaterasu_module_sdk::application_library::provider::ArtworkChunk, &'static str>
    {
        Err("unsupported")
    }
    fn incarnation(&self) -> &str {
        &self.incarnation
    }
    fn supports_limiter_target_v1(&self) -> bool {
        matches!(
            self.mode.as_str(),
            "limiter_success"
                | "limiter_mismatch"
                | "limiter_stale"
                | "limiter_stale_recheck"
                | "resume_limiter"
        )
    }
    fn attest_limiter_target(
        &mut self,
        identity: &Identity<'_>,
        request: &amaterasu_module_sdk::application_library::limiter_target::Request,
    ) -> Result<amaterasu_module_sdk::application_library::limiter_target::Attestation, &'static str>
    {
        use amaterasu_module_sdk::application_library::{
            limiter_target::{Attestation, Stage},
            provider_lifecycle::State,
        };
        if request.application_id != "fixture-game" {
            return Err("not_found");
        }
        let key = (identity.caller.to_owned(), request.operation_id.clone());
        let reservation_key = (
            identity.caller.to_owned(),
            identity.initiator.to_owned(),
            request.operation_id.clone(),
        );
        match request.stage {
            Stage::Reserve => {
                if self.operations.contains_key(&key)
                    && !self.reservations.contains_key(&reservation_key)
                {
                    return Err("operation_already_started");
                }
                if self.reservations.len() >= 4096
                    && !self.reservations.contains_key(&reservation_key)
                {
                    return Err("capacity_exhausted");
                }
                if self
                    .reservations
                    .get(&reservation_key)
                    .is_some_and(|prior| prior != request)
                {
                    return Err("operation_conflict");
                }
                self.reservations
                    .insert(reservation_key.clone(), request.clone());
            }
            Stage::Running | Stage::Verify => {
                let mut expected = request.clone();
                expected.stage = Stage::Reserve;
                expected.ticket = None;
                let reserved = if request.stage == Stage::Verify {
                    identity.caller == identity.initiator
                        && self
                            .reservations
                            .iter()
                            .filter(|((caller, _, operation), value)| {
                                caller == identity.caller
                                    && operation == &request.operation_id
                                    && *value == &expected
                            })
                            .count()
                            == 1
                } else {
                    self.reservations.get(&reservation_key) == Some(&expected)
                };
                if !reserved
                    || !self.operations.get(&key).is_some_and(|entry| {
                        entry.0 == request.application_id
                            && entry.1 == request.ticket
                            && entry.2 == State::Running
                    })
                {
                    return Err("execution_mismatch");
                }
            }
        }
        let mut result = Attestation {
            request: request.clone(),
            executable_path: "/synthetic/replacement-game".into(),
        };
        if self.mode == "limiter_mismatch" && request.stage == Stage::Running {
            result.executable_path = "/synthetic/other".into();
        }
        if (self.mode == "limiter_stale" && request.stage == Stage::Running)
            || (self.mode == "limiter_stale_recheck" && request.stage == Stage::Verify)
        {
            result.request.provider_incarnation = "stale".into();
        }
        Ok(result)
    }
    fn lifecycle(
        &mut self,
        identity: &Identity<'_>,
        request: amaterasu_module_sdk::application_library::provider_lifecycle::Request,
    ) -> Result<
        amaterasu_module_sdk::application_library::provider_lifecycle::Observation,
        &'static str,
    > {
        use amaterasu_module_sdk::application_library::provider_lifecycle::{
            Action, Observation, ProofKind, RetirementProof, State,
        };
        if request.application_id != "fixture-game" {
            return Err("not_found");
        }
        let key = (identity.caller.to_owned(), request.operation_id.clone());
        if self
            .operations
            .get(&key)
            .is_some_and(|entry| entry.0 != request.application_id)
        {
            return Err("operation_conflict");
        }
        match request.action {
            Action::Launch => {
                if !self.operations.contains_key(&key) {
                    if self.operations.len() >= 4096 {
                        return Err("capacity_exhausted");
                    }
                    self.operations.insert(
                        key.clone(),
                        (
                            request.application_id.clone(),
                            Some(format!("ticket-{}", self.operations.len() + 1)),
                            State::Running,
                        ),
                    );
                }
            }
            Action::RecoverStop => {
                self.operations
                    .entry(key.clone())
                    .or_insert((request.application_id.clone(), None, State::Stopped))
                    .2 = State::Stopped;
            }
            Action::Stop => {
                let entry = self.operations.get_mut(&key).ok_or("not_found")?;
                if entry.1 != request.ticket {
                    return Err("ticket_mismatch");
                }
                entry.2 = State::Stopped;
            }
            Action::Lookup => {}
        }
        let (ticket, state) = self
            .operations
            .get(&key)
            .map_or((None, State::Unknown), |entry| {
                (entry.1.clone(), entry.2.clone())
            });
        let retirement_proof = (state == State::Stopped).then(|| RetirementProof {
            kind: ProofKind::FencedStopped,
            operation_id: request.operation_id.clone(),
            application_id: request.application_id.clone(),
            provider_incarnation: request.provider_incarnation.clone(),
            ticket: ticket.clone(),
        });
        Ok(Observation {
            operation_id: request.operation_id,
            application_id: request.application_id,
            provider_incarnation: request.provider_incarnation,
            ticket,
            state,
            retirement_proof,
        })
    }
}
struct Module {
    runtime: RuntimeClient,
    library: LibraryAdapter<GeneratedLibrary>,
    policy: Adapter<GeneratedPolicy>,
    executor: amaterasu_module_sdk::application_library::provider_adapter::ProviderAdapter<
        GeneratedExecutor,
    >,
}
impl ManagedModule for Module {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let Response::Result { payload } = runtime.send(Command::GetSettings {})? else {
            return Err("settings unavailable".into());
        };
        let settings: Settings = serde_json::from_value(payload).map_err(|_| "invalid settings")?;
        let caller = "amaterasu.launch".to_owned();
        let initiators = BTreeSet::from(["amaterasu.nvhttp".to_owned()]);
        let library = LibraryAdapter::new(
            GeneratedLibrary {
                mode: settings.mode.clone(),
                resolutions: 0,
            },
            "fixture.library/2".into(),
            BTreeMap::from([(
                caller.clone(),
                CallerPolicy {
                    initiators: initiators.clone(),
                    read: false,
                    resolve_launch: true,
                },
            )]),
        )
        .map_err(str::to_owned)?;
        let executor_mode = settings.mode.clone();
        let policy = Adapter::new(
            GeneratedPolicy {
                certificate_id: settings.certificate_id,
                mode: settings.mode,
                calls: 0,
            },
            "fixture.policy/2".into(),
            BTreeMap::from([(caller, initiators)]),
        )
        .map_err(str::to_owned)?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "entropy unavailable")?;
        let incarnation = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let executor=amaterasu_module_sdk::application_library::provider_adapter::ProviderAdapter::with_policies(
            GeneratedExecutor { incarnation,mode:executor_mode,operations:BTreeMap::new(),reservations:BTreeMap::new() },
            "example.library.provider/1".into(), BTreeMap::from([("amaterasu.launch".into(),amaterasu_module_sdk::application_library::provider_adapter::CallerPolicy { initiators:["amaterasu.nvhttp".into(),"amaterasu.launch".into()].into(), lifecycle:true,..Default::default() })])
        ).map_err(str::to_owned)?;
        for service in [
            "fixture.library/2",
            "fixture.policy/2",
            "example.library.provider/1",
        ] {
            runtime.require_success(Command::Register {
                service: service.into(),
            })?;
        }
        Ok(Self {
            runtime,
            library,
            policy,
            executor,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            let payload = if request.service == "fixture.library/2" {
                self.library.dispatch(&request)
            } else if request.service == "fixture.policy/2" {
                self.policy.dispatch(&request)
            } else {
                self.executor.dispatch(&request)
            };
            self.runtime.require_success(Command::Complete {
                request: request.id,
                payload,
            })?;
        }
        Ok(())
    }
}
amaterasu_module_sdk::export_module!(Module);
