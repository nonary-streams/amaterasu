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
struct GeneratedLibrary;
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
        if id != "opaque/game" || revision != "entry:opaque" {
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
                policy_revision: if self.mode == "stale" && self.calls > 1 {
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
struct Module {
    runtime: RuntimeClient,
    library: LibraryAdapter<GeneratedLibrary>,
    policy: Adapter<GeneratedPolicy>,
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
            GeneratedLibrary,
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
        for service in ["fixture.library/2", "fixture.policy/2"] {
            runtime.require_success(Command::Register {
                service: service.into(),
            })?;
        }
        Ok(Self {
            runtime,
            library,
            policy,
        })
    }
    fn tick(&mut self) -> ModuleResult<()> {
        if let Some(request) = self.runtime.poll_serialized()? {
            let payload = if request.service == "fixture.library/2" {
                self.library.dispatch(&request)
            } else {
                self.policy.dispatch(&request)
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
