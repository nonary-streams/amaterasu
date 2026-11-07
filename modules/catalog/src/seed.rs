//! First-run seeding of the process-less `Desktop` catalog entry.
//!
//! The seed is a startup job that runs between ordinary requests, never during
//! one, so it has a single writer. It creates the entry and a hidden marker row
//! in one atomic collection commit. Because the marker is a row of the same
//! collection, every later catalog write preserves it: deleting or renaming the
//! entry can never cause it to be created again.
use crate::STATE_SERVICE;
use crate::model::{ConfigOverrides, Entry, Launch, MAX_ENTRIES};
use crate::pipeline::header;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Hidden collection row. It sorts after every `app-` entry and is skipped by
/// listing, search, export and provider scans like the provider marker rows.
pub(crate) const MARKER_KEY: &str = "~seed.desktop";
pub(crate) const DESKTOP_ENTRY_NAME: &str = "Desktop";

const MAX_ATTEMPTS: u8 = 8;
const RETRY_DELAY: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT_MS: u64 = 10_000;

/// Records which entry the seed created. Its presence, not its content, is
/// what prevents a second seed.
#[derive(Serialize)]
struct DesktopMarker {
    version: u8,
    entry_id: String,
}

/// Where a collection stands before the seed commit.
struct CatalogPosition {
    state_revision: u64,
    catalog_revision: u64,
    next_id: u64,
    hidden_rows: u64,
}

impl CatalogPosition {
    /// A collection that does not exist yet.
    const EMPTY: Self = Self {
        state_revision: 0,
        catalog_revision: 0,
        next_id: 1,
        hidden_rows: 0,
    };
}

pub(crate) fn desktop_entry() -> Entry {
    Entry {
        name: DESKTOP_ENTRY_NAME.into(),
        uuid: None,
        id_aliases: Vec::new(),
        hidden: false,
        remote_monitor_profile: false,
        properties: json!({}),
        description: String::new(),
        tags: Vec::new(),
        artwork: None,
        steam_id: None,
        steam_install_dir: None,
        output: None,
        display_output: None,
        virtual_screen: None,
        virtual_display_mode: None,
        virtual_display_layout: None,
        config_overrides: ConfigOverrides::default(),
        exclude_global_prep_cmd: false,
        prep_cmd: Vec::new(),
        elevated: false,
        launch: Launch::Desktop {
            exit_timeout_seconds: 10,
            working_directory: None,
            preparation: Vec::new(),
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    ReadMarker,
    ReadLegacyDocument,
    Commit,
}

struct InFlight {
    request: u64,
    step: Step,
}

struct ActiveSeed {
    collection: String,
    attempts: u8,
    retry_at: Option<Instant>,
    in_flight: Option<InFlight>,
}

impl ActiveSeed {
    fn new(collection: String) -> Self {
        Self {
            collection,
            attempts: 0,
            retry_at: None,
            in_flight: None,
        }
    }
}

enum Outcome {
    /// The marker exists or the catalog must be migrated first; nothing to do.
    Settled,
    /// Another state request is needed.
    Continue(Value, Step),
    /// A transient failure; read the collection again later.
    Retry,
}

#[derive(Default)]
pub(crate) struct DesktopSeed {
    queue: VecDeque<String>,
    active: Option<ActiveSeed>,
    nonce: u128,
    sequence: u64,
}

impl DesktopSeed {
    /// `collections` are the state collection names of every catalog to seed.
    pub(crate) fn new(collections: impl IntoIterator<Item = String>) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        Self {
            queue: collections.into_iter().collect(),
            nonce,
            ..Self::default()
        }
    }

    /// True while the seed has a state request in flight or is due to start
    /// one. The pipeline keeps ordinary requests queued meanwhile.
    pub(crate) fn needs_turn(&self) -> bool {
        match &self.active {
            Some(active) => {
                active.in_flight.is_some() || active.retry_at.is_none_or(|at| Instant::now() >= at)
            }
            None => !self.queue.is_empty(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_finished(&self) -> bool {
        self.active.is_none() && self.queue.is_empty()
    }

    /// Make a scheduled retry due now, without waiting for the real delay.
    #[cfg(test)]
    pub(crate) fn expedite_retry(&mut self) {
        if let Some(active) = &mut self.active {
            active.retry_at = None;
        }
    }

    pub(crate) fn step(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        if self.active.is_none() {
            self.active = self.queue.pop_front().map(ActiveSeed::new);
        }
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let outcome = match active.in_flight.take() {
            None => {
                let request = read_request(&active.collection, Some(MARKER_KEY));
                Outcome::Continue(request, Step::ReadMarker)
            }
            Some(in_flight) => match send(Command::TakeResult {
                request: in_flight.request,
            }) {
                Ok(Response::Pending) => {
                    active.in_flight = Some(in_flight);
                    self.active = Some(active);
                    return Ok(());
                }
                Ok(Response::Result { payload }) if payload["ok"] == true => {
                    consume(&active.collection, in_flight.step, &payload)
                }
                _ => Outcome::Retry,
            },
        };
        match outcome {
            Outcome::Settled => {}
            Outcome::Retry => self.retry_later(active),
            Outcome::Continue(payload, step) => self.submit(active, payload, step, send)?,
        }
        Ok(())
    }

    fn submit(
        &mut self,
        mut active: ActiveSeed,
        payload: Value,
        step: Step,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<()> {
        self.sequence = self.sequence.wrapping_add(1);
        let response = send(Command::Request {
            service: STATE_SERVICE.into(),
            payload,
            timeout_ms: REQUEST_TIMEOUT_MS,
            operation_id: format!("catalog.seed.{}.{}", self.nonce, self.sequence),
            // The seed is not delegated work: it runs under the catalog's own
            // state grant and cannot be cancelled by a caller.
            parent: None,
        });
        match response {
            Ok(Response::Accepted { id }) => {
                active.in_flight = Some(InFlight { request: id, step });
                self.active = Some(active);
            }
            _ => self.retry_later(active),
        }
        Ok(())
    }

    fn retry_later(&mut self, mut active: ActiveSeed) {
        active.attempts += 1;
        if active.attempts >= MAX_ATTEMPTS {
            // The marker is still absent, so the next start tries again.
            return;
        }
        active.retry_at = Some(Instant::now() + RETRY_DELAY);
        self.active = Some(active);
    }
}

fn read_request(collection: &str, key: Option<&str>) -> Value {
    json!({"action":"collection_get","collection":collection,"key":key})
}

fn consume(collection: &str, step: Step, payload: &Value) -> Outcome {
    match step {
        Step::ReadMarker => consume_marker_read(collection, payload),
        Step::ReadLegacyDocument => match payload["found"].as_bool() {
            // A legacy document migrates on its first ordinary access. Seeding
            // before that would fork the catalog, so wait for the next start.
            Some(true) => Outcome::Settled,
            Some(false) => commit_request(collection, &CatalogPosition::EMPTY),
            None => Outcome::Retry,
        },
        Step::Commit => Outcome::Settled,
    }
}

fn consume_marker_read(collection: &str, payload: &Value) -> Outcome {
    match payload["found"].as_bool() {
        Some(false) => Outcome::Continue(
            json!({"action":"get","key":collection}),
            Step::ReadLegacyDocument,
        ),
        Some(true) => consume_existing_collection(collection, payload),
        None => Outcome::Retry,
    }
}

fn consume_existing_collection(collection: &str, payload: &Value) -> Outcome {
    // A recorded seed is final even when the entry is gone or the marker's
    // content is unexpected: neither is a reason to create a second Desktop.
    if payload.get("value").is_some_and(|marker| !marker.is_null()) {
        return Outcome::Settled;
    }
    let (Some(state_revision), Some(count)) =
        (payload["revision"].as_u64(), payload["count"].as_u64())
    else {
        return Outcome::Retry;
    };
    let hidden_rows = payload["metadata"]["provider_count"].as_u64().unwrap_or(0);
    let Ok(document) = header(&payload["metadata"]) else {
        return Outcome::Retry;
    };
    if count.saturating_sub(hidden_rows) >= MAX_ENTRIES as u64 {
        return Outcome::Settled;
    }
    commit_request(
        collection,
        &CatalogPosition {
            state_revision,
            catalog_revision: document.revision,
            next_id: document.next_id,
            hidden_rows,
        },
    )
}

/// Build the atomic commit that adds the entry and its marker row. The catalog
/// revision, ID counter and hidden-row count advance together with the rows.
fn commit_request(collection: &str, position: &CatalogPosition) -> Outcome {
    let (Some(revision), Some(following_id), Some(hidden_rows)) = (
        position.catalog_revision.checked_add(1),
        position.next_id.checked_add(1),
        position.hidden_rows.checked_add(1),
    ) else {
        return Outcome::Settled;
    };
    let entry = desktop_entry();
    if entry.validate().is_err() {
        return Outcome::Settled;
    }
    let entry_id = format!("app-{:016x}", position.next_id);
    let marker = DesktopMarker {
        version: 1,
        entry_id: entry_id.clone(),
    };
    Outcome::Continue(
        json!({
            "action":"collection_commit",
            "collection":collection,
            "expected_revision":position.state_revision,
            "metadata":{
                "revision":revision,
                "next_id":following_id,
                "provider_count":hidden_rows,
            },
            "edits":[
                {"key":entry_id,"value":entry},
                {"key":MARKER_KEY,"value":marker},
            ],
        }),
        Step::Commit,
    )
}
