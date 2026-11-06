//! Complete, revision-bound catalog projection through the selected public API.
use crate::launch::DESKTOP_APP_ID;
use crate::wire::{Permission, Reply, xml};
use amaterasu_module_sdk::session_presentation::{
    self as session, Application, Context, Presentation, Request,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const PAGE_SIZE: usize = session::MAX_PAGE_ENTRIES;
const MAX_ENTRIES: usize = session::MAX_CATALOG_ENTRIES;
const MAX_PAGES: usize = 1_024;
const MAX_INVENTORY_BYTES: usize = 64 * 1024 * 1024;
const MAX_XML_BYTES: usize = 1024 * 1024 - 1024;
const PREFIX: &str =
    "<?xml version=\"1.0\" encoding=\"utf-8\"?><root status_code=\"200\" status_message=\"OK\">";
const SUFFIX: &str = "</root>";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    revision: u64,
    after: String,
    query: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryPage {
    ok: bool,
    revision: u64,
    entries: Vec<Application>,
    next_cursor: Option<Cursor>,
}

enum Phase {
    Inventory,
    Begin,
    Append { end: usize },
    Read { offset: usize },
    Release,
    Done,
}

pub(super) enum ProjectionCall {
    Library(Value),
    Provider(Box<Request>),
}

/// One caller-authorized catalog read. No partial list is published, and the
/// completed XML remains held until the provider releases its transaction.
pub(super) struct CatalogProjection {
    context: Context,
    desktop_enabled: bool,
    hdr_supported: bool,
    phase: Phase,
    revision: Option<u64>,
    cursor: Option<Cursor>,
    last_id: Option<u32>,
    inventory: Vec<Application>,
    inventory_bytes: usize,
    inventory_pages: usize,
    projection_id: Option<String>,
    upload_offset: usize,
    presentation: Option<Presentation>,
    projected_ids: BTreeSet<u32>,
    projected_pages: usize,
    body: String,
}

impl CatalogProjection {
    pub(super) fn new(context: Context, desktop_enabled: bool, hdr_supported: bool) -> Self {
        Self {
            context,
            desktop_enabled,
            hdr_supported,
            phase: Phase::Inventory,
            revision: None,
            cursor: None,
            last_id: None,
            inventory: Vec::new(),
            inventory_bytes: 0,
            inventory_pages: 0,
            projection_id: None,
            upload_offset: 0,
            presentation: None,
            projected_ids: BTreeSet::new(),
            projected_pages: 0,
            body: PREFIX.into(),
        }
    }

    pub(super) fn next_call(&self) -> ProjectionCall {
        match self.phase {
            Phase::Inventory => {
                let mut payload = json!({"action":"identity_inventory","limit":PAGE_SIZE});
                if let Some(cursor) = &self.cursor {
                    payload["cursor"] = json!(cursor);
                }
                ProjectionCall::Library(payload)
            }
            Phase::Begin => ProjectionCall::Provider(Box::new(Request::BeginProject {
                context: self.context.clone(),
                catalog_revision: self.revision.expect("inventory revision").to_string(),
            })),
            Phase::Append { end } => ProjectionCall::Provider(Box::new(self.append_request(end))),
            Phase::Read { offset } => ProjectionCall::Provider(Box::new(Request::ReadProject {
                client_id: self.context.client.id.clone(),
                projection_id: self.projection_id().to_owned(),
                offset,
                limit: PAGE_SIZE,
            })),
            Phase::Release => ProjectionCall::Provider(Box::new(
                self.release_request()
                    .expect("completed projection has an ID"),
            )),
            Phase::Done => unreachable!("completed catalog projection cannot advance"),
        }
    }

    pub(super) fn context(&self) -> &Context {
        &self.context
    }

    pub(super) fn release_request(&self) -> Option<Request> {
        if matches!(self.phase, Phase::Done) {
            return None;
        }
        self.projection_id
            .as_ref()
            .map(|projection_id| Request::ReleaseProject {
                client_id: self.context.client.id.clone(),
                projection_id: projection_id.clone(),
            })
    }

    pub(super) fn accept(&mut self, payload: Value) -> Result<Option<Reply>, Reply> {
        if matches!(self.phase, Phase::Inventory) {
            self.accept_inventory(payload)?;
            return Ok(None);
        }
        if !serde_json::to_vec(&payload)
            .is_ok_and(|bytes| bytes.len() <= session::MAX_PAYLOAD_BYTES)
        {
            return Err(xml::error(
                502,
                "session presentation reply exceeds its bound",
            ));
        }
        let reply: session::Reply = serde_json::from_value(payload)
            .map_err(|_| xml::error(502, "session presentation reply is malformed"))?;
        match (&self.phase, reply) {
            (Phase::Begin, session::Reply::Started { projection_id }) => {
                if projection_id.is_empty()
                    || projection_id.len() > 128
                    || projection_id.chars().any(char::is_control)
                {
                    return Err(xml::error(502, "session projection identity is invalid"));
                }
                self.projection_id = Some(projection_id);
                self.prepare_append()?;
            }
            (Phase::Append { end }, session::Reply::Appended { complete }) => {
                let end = *end;
                if complete != (end == self.inventory.len()) {
                    return Err(xml::error(
                        502,
                        "session projection completion is inconsistent",
                    ));
                }
                self.upload_offset = end;
                if complete {
                    self.phase = Phase::Read { offset: 0 };
                    self.inventory.clear();
                } else {
                    self.prepare_append()?;
                }
            }
            (
                Phase::Read { offset },
                session::Reply::Projected {
                    presentation,
                    entries,
                    next_offset,
                },
            ) => {
                let offset = *offset;
                self.accept_projected(offset, presentation, entries, next_offset)?;
            }
            (Phase::Release, session::Reply::Acknowledged {}) => {
                self.phase = Phase::Done;
                self.inventory.clear();
                self.body.push_str(SUFFIX);
                return Ok(Some(Reply {
                    status: 200,
                    content_type: "application/xml; charset=utf-8",
                    body: std::mem::take(&mut self.body).into_bytes(),
                    requires_paired_tls: true,
                    permission: Some(Permission::List),
                }));
            }
            (_, session::Reply::Error { .. }) => {
                return Err(xml::error(503, "session presentation is unavailable"));
            }
            _ => return Err(xml::error(502, "unexpected session presentation reply")),
        }
        Ok(None)
    }

    fn accept_inventory(&mut self, payload: Value) -> Result<(), Reply> {
        if payload["ok"] != true {
            let changed = payload["error"]["code"] == "revision_conflict";
            return Err(xml::error(
                if changed { 409 } else { 503 },
                if changed {
                    "catalog revision changed; request the list again"
                } else {
                    "catalog identity inventory is unavailable"
                },
            ));
        }
        if payload.get("next_cursor").is_none() {
            return Err(xml::error(
                502,
                "catalog inventory has no continuation field",
            ));
        }
        let bytes = serde_json::to_vec(&payload)
            .map_err(|_| xml::error(502, "catalog inventory is malformed"))?
            .len();
        self.inventory_bytes = self.inventory_bytes.saturating_add(bytes);
        self.inventory_pages += 1;
        if self.inventory_bytes > MAX_INVENTORY_BYTES || self.inventory_pages > MAX_PAGES {
            return Err(xml::error(
                413,
                "catalog identity inventory exceeds its bound",
            ));
        }
        let page: InventoryPage = serde_json::from_value(payload)
            .map_err(|_| xml::error(502, "catalog identity inventory is malformed"))?;
        if !page.ok
            || self
                .revision
                .is_some_and(|revision| revision != page.revision)
        {
            return Err(xml::error(
                409,
                "catalog revision changed; request the list again",
            ));
        }
        if page.entries.len() > PAGE_SIZE
            || self.inventory.len().saturating_add(page.entries.len()) > MAX_ENTRIES
        {
            return Err(xml::error(
                413,
                "catalog identity inventory exceeds its entry bound",
            ));
        }
        if page.entries.is_empty() && page.next_cursor.is_some() {
            return Err(xml::error(
                502,
                "catalog inventory did not advance its cursor",
            ));
        }
        self.revision = Some(page.revision);
        for entry in page.entries {
            self.append_inventory(entry)?;
        }
        if let Some(cursor) = &page.next_cursor {
            if cursor.revision != page.revision
                || !cursor.query.is_empty()
                || canonical_numeric_id(&cursor.after).ok() != self.last_id
            {
                return Err(xml::error(502, "catalog inventory cursor is inconsistent"));
            }
            if self.inventory_pages == MAX_PAGES || self.inventory.len() == MAX_ENTRIES {
                return Err(xml::error(
                    413,
                    "catalog identity inventory exceeds pagination bounds",
                ));
            }
        }
        self.cursor = page.next_cursor;
        if self.cursor.is_none() {
            self.finish_inventory()?;
            self.phase = Phase::Begin;
        }
        Ok(())
    }

    fn append_inventory(&mut self, mut entry: Application) -> Result<(), Reply> {
        entry
            .validate()
            .map_err(|_| xml::error(502, "catalog identity entry is invalid"))?;
        let id = canonical_numeric_id(&entry.id).map_err(|message| xml::error(502, message))?;
        if self.last_id.is_some_and(|last| id <= last) {
            return Err(xml::error(
                502,
                "catalog identity IDs are duplicated or unordered",
            ));
        }
        entry.id = id.to_string();
        for alias in &mut entry.aliases {
            *alias = canonical_numeric_id(alias)
                .map_err(|message| xml::error(502, message))?
                .to_string();
        }
        if self.desktop_enabled
            && (id == DESKTOP_APP_ID
                || entry
                    .aliases
                    .iter()
                    .any(|alias| alias == &DESKTOP_APP_ID.to_string()))
        {
            return Err(xml::error(
                502,
                "catalog identity conflicts with the Desktop app ID",
            ));
        }
        if let Some(game) = &mut self.context.game
            && game.application.id == entry.id
        {
            game.application = entry.clone();
        }
        self.inventory.push(entry);
        self.last_id = Some(id);
        Ok(())
    }

    fn finish_inventory(&mut self) -> Result<(), Reply> {
        if self.desktop_enabled {
            if self.inventory.len() == MAX_ENTRIES {
                return Err(xml::error(
                    413,
                    "catalog has no capacity for the Desktop app",
                ));
            }
            self.inventory.push(Application {
                id: DESKTOP_APP_ID.to_string(),
                uuid: String::new(),
                title: "Desktop".into(),
                aliases: Vec::new(),
                visible: true,
                monitor_profile: false,
                properties: json!({}),
            });
        }
        Ok(())
    }

    fn projection_id(&self) -> &str {
        self.projection_id.as_deref().expect("projection has begun")
    }

    fn append_request(&self, end: usize) -> Request {
        Request::AppendProject {
            client_id: self.context.client.id.clone(),
            projection_id: self.projection_id().to_owned(),
            entries: self.inventory[self.upload_offset..end].to_vec(),
            final_page: end == self.inventory.len(),
        }
    }

    fn prepare_append(&mut self) -> Result<(), Reply> {
        let mut end = (self.upload_offset + PAGE_SIZE).min(self.inventory.len());
        loop {
            if self.append_request(end).validate().is_ok() {
                self.phase = Phase::Append { end };
                return Ok(());
            }
            if end <= self.upload_offset + 1 {
                return Err(xml::error(
                    413,
                    "catalog entry exceeds the session API bound",
                ));
            }
            end -= 1;
        }
    }

    fn accept_projected(
        &mut self,
        offset: usize,
        presentation: Presentation,
        entries: Vec<Application>,
        next_offset: Option<usize>,
    ) -> Result<(), Reply> {
        self.projected_pages += 1;
        if self.projected_pages > MAX_PAGES || entries.len() > PAGE_SIZE {
            return Err(xml::error(
                413,
                "projected catalog exceeds pagination bounds",
            ));
        }
        validate_presentation(&presentation)?;
        if self
            .presentation
            .as_ref()
            .is_some_and(|previous| previous != &presentation)
        {
            return Err(xml::error(
                409,
                "session presentation changed during catalog export",
            ));
        }
        self.presentation = Some(presentation);
        let expected_next = offset
            .checked_add(entries.len())
            .ok_or_else(|| xml::error(502, "projected catalog offset overflowed"))?;
        if next_offset.is_some_and(|next| entries.is_empty() || next != expected_next)
            || expected_next > MAX_ENTRIES + 32
        {
            return Err(xml::error(502, "projected catalog cursor is inconsistent"));
        }
        for entry in entries {
            self.append_projected(entry)?;
        }
        if let Some(next) = next_offset {
            if self.projected_pages == MAX_PAGES || expected_next == MAX_ENTRIES + 32 {
                return Err(xml::error(
                    413,
                    "projected catalog exceeds pagination bounds",
                ));
            }
            self.phase = Phase::Read { offset: next };
        } else {
            self.phase = Phase::Release;
        }
        Ok(())
    }

    fn append_projected(&mut self, entry: Application) -> Result<(), Reply> {
        entry
            .validate()
            .map_err(|_| xml::error(502, "projected application is invalid"))?;
        if !entry.visible {
            return Err(xml::error(502, "projected catalog contains a hidden entry"));
        }
        let id = decimal_numeric_id(&entry.id).map_err(|message| xml::error(502, message))?;
        if !self.projected_ids.insert(id) {
            return Err(xml::error(502, "projected catalog contains duplicate IDs"));
        }
        // Providers may narrow a row's modes; the selected encoders remain
        // authoritative for the host's ability to stream HDR.
        let row_hdr_supported = match entry.properties.get("hdr_supported") {
            None => self.hdr_supported,
            Some(Value::Bool(supported)) => self.hdr_supported && *supported,
            Some(_) => {
                return Err(xml::error(
                    502,
                    "projected application HDR capability is invalid",
                ));
            }
        };
        let entry_xml = format!(
            "<App><AppTitle>{}</AppTitle><ID>{id}</ID><UUID>{}</UUID><IsHdrSupported>{}</IsHdrSupported></App>",
            xml::escape(&entry.title),
            xml::escape(&entry.uuid),
            u8::from(row_hdr_supported),
        );
        if self
            .body
            .len()
            .saturating_add(entry_xml.len())
            .saturating_add(SUFFIX.len())
            > MAX_XML_BYTES
        {
            return Err(xml::error(
                413,
                "projected catalog exceeds the Moonlight reply limit",
            ));
        }
        self.body.push_str(&entry_xml);
        Ok(())
    }
}

fn validate_presentation(presentation: &Presentation) -> Result<(), Reply> {
    if let Some(id) = &presentation.current_game {
        decimal_numeric_id(id).map_err(|message| xml::error(502, message))?;
    }
    if presentation
        .current_game_uuid
        .as_ref()
        .is_some_and(|uuid| uuid.len() > 128 || uuid.chars().any(char::is_control))
    {
        return Err(xml::error(502, "session presentation game UUID is invalid"));
    }
    Ok(())
}

fn canonical_numeric_id(id: &str) -> Result<u32, &'static str> {
    let digits = id
        .strip_prefix("app-")
        .ok_or("catalog identity ID is not canonical")?;
    if digits.len() != 16
        || !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("catalog identity ID is not canonical");
    }
    let value = u64::from_str_radix(digits, 16).map_err(|_| "catalog identity ID is invalid")?;
    if !(1..=i32::MAX as u64).contains(&value) {
        return Err("catalog identity ID exceeds the Moonlight range");
    }
    Ok(value as u32)
}

fn decimal_numeric_id(id: &str) -> Result<u32, &'static str> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("projected application ID is not a positive decimal integer");
    }
    let value = id
        .parse::<u32>()
        .map_err(|_| "projected application ID is invalid")?;
    if value == 0 || value > i32::MAX as u32 || value.to_string() != id {
        return Err("projected application ID exceeds the canonical Moonlight range");
    }
    Ok(value)
}

#[cfg(test)]
#[path = "applications/tests.rs"]
mod tests;
