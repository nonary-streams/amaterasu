//! Helpers for the existing catalog provider snapshot protocol.
//!
//! Calls use `RuntimeClient::request_serialized` and its broker-derived caller;
//! no provider or catalog-owner identity in a payload is authoritative. Grants
//! route trusted native modules and do not sandbox their code. The catalog's
//! startup import map determines which caller may write which catalog.
//!
//! A batch is persisted incrementally. A failed or incomplete snapshot does not
//! roll back batches already accepted, and only a successful complete finish can
//! request stale-entry cleanup. Keep the same broker operation ID and unchanged
//! payload for admission retries; reconcile an uncertain result before new work.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

/// Startup-selected application-library facade. This contract currently exposes
/// catalog reads; lifecycle operations continue through `amaterasu.launch/1`.
pub const LIBRARY_SERVICE: &str = "amaterasu.library/1";

/// Catalog-issued continuation. Preserve all fields from the previous page;
/// mutations invalidate the collection revision. The NVHTTP compatibility
/// profile requires nonempty continuation pages and ascending canonical IDs.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryCursor {
    pub revision: u64,
    pub after: String,
    pub query: String,
}

/// Read operations on the unified application-library facade. Identity is
/// derived by the broker, not selected by these payloads.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LibraryRequest {
    List {
        cursor: Option<LibraryCursor>,
        limit: Option<usize>,
    },
    /// Complete identity occupancy for an installed session-presentation
    /// consumer. Unlike `List`, this includes hidden applications and aliases.
    IdentityInventory {
        cursor: Option<LibraryCursor>,
        limit: Option<usize>,
    },
    Resolve {
        id: String,
    },
}

/// Public list row and resolve projection; launch metadata is not exposed.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct LibraryEntry {
    pub id: String,
    pub name: String,
}

/// Successful list response. Check `ok` before consuming this response.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct LibraryListResponse {
    pub ok: bool,
    pub revision: u64,
    pub entries: Vec<LibraryEntry>,
    pub next_cursor: Option<LibraryCursor>,
}

/// Presentation identity without executable or preparation metadata. Properties
/// are provider-owned and bounded; monitor profiles retain their approved
/// display overrides here so editing a row cannot retarget an existing lease.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryIdentityEntry {
    pub id: String,
    pub uuid: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub visible: bool,
    pub monitor_profile: bool,
    pub properties: Value,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryIdentityInventoryResponse {
    pub ok: bool,
    pub revision: u64,
    pub entries: Vec<LibraryIdentityEntry>,
    pub next_cursor: Option<LibraryCursor>,
}

impl LibraryIdentityEntry {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_id = |id: &str| {
            id.len() == 20
                && id.starts_with("app-")
                && id[4..]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                && id[4..].bytes().any(|byte| byte != b'0')
        };
        let printable = |text: &str, max| {
            !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
        };
        if !valid_id(&self.id)
            || !printable(&self.uuid, 128)
            || !printable(&self.title, 160)
            || self.title.trim().is_empty()
            || self.aliases.len() > 32
            || self.aliases.iter().any(|id| !valid_id(id))
            || self.aliases.iter().collect::<BTreeSet<_>>().len() != self.aliases.len()
            || !self.properties.is_object()
            || !serde_json::to_vec(&self.properties).is_ok_and(|bytes| bytes.len() <= 4_096)
        {
            return Err("invalid library identity entry");
        }
        Ok(())
    }
}

impl LibraryIdentityInventoryResponse {
    /// Validate the complete page before exposing any occupancy to a consumer.
    pub fn validate(&self, limit: usize) -> Result<(), &'static str> {
        if !self.ok
            || !(1..=32).contains(&limit)
            || self.entries.len() > limit
            || !serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 12_288)
        {
            return Err("invalid library identity inventory page");
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        if self.entries.windows(2).any(|pair| pair[0].id >= pair[1].id) {
            return Err("unordered library identity inventory page");
        }
        if let Some(cursor) = &self.next_cursor
            && (cursor.revision != self.revision
                || !cursor.query.is_empty()
                || self
                    .entries
                    .last()
                    .is_none_or(|entry| entry.id != cursor.after))
        {
            return Err("invalid library identity inventory continuation");
        }
        Ok(())
    }
}

/// Successful resolve response. Error envelopes have a different shape and
/// should be decoded separately.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct LibraryResolveResponse {
    pub ok: bool,
    pub revision: u64,
    pub id: String,
    pub name: String,
}

pub const CATALOG_SERVICE: &str = "amaterasu.catalog/1";

/// Narrow presentation-profile operations in the client-facing catalog selected
/// by trusted startup composition. Payloads cannot choose a catalog owner. The
/// catalog accepts mutations only from an approved profile coordinator with the
/// authenticated management initiator, and only for marked inert profiles.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CatalogProfileRequest {
    ProfileList {
        cursor: Option<LibraryCursor>,
        limit: Option<usize>,
    },
    ProfileUpsert {
        id: String,
        entry: Value,
        expected_revision: u64,
    },
    ProfileDelete {
        id: String,
        expected_revision: u64,
    },
}
pub const MAX_BATCH_INTENTS: usize = 8;
pub const MAX_BATCH_BYTES: usize = 12_000;

#[derive(Clone, Debug, Serialize)]
pub struct ProviderEntry {
    pub source_id: String,
    /// Catalog entry JSON. Catalog performs the authoritative schema validation.
    pub entry: Value,
    pub source_flags: Option<u32>,
}

/// An incremental provider batch. Every source ID must occur at most once,
/// including across the three removal categories and the entry list.
#[derive(Clone, Debug, Serialize)]
pub struct SnapshotBatch {
    pub entries: Vec<ProviderEntry>,
    pub remove_source_ids: Vec<String>,
    pub expire_source_ids: Vec<String>,
    pub selection_remove_source_ids: Vec<String>,
}

impl SnapshotBatch {
    /// Check obvious batch bounds before sending. Catalog entry normalization can
    /// increase encoded size, so the catalog remains authoritative for all
    /// bounds, identities, entry semantics, generation, revision and authorization.
    pub fn validate(&self) -> Result<(), &'static str> {
        let mut ids = BTreeSet::new();
        for id in self
            .entries
            .iter()
            .map(|entry| &entry.source_id)
            .chain(&self.remove_source_ids)
            .chain(&self.expire_source_ids)
            .chain(&self.selection_remove_source_ids)
        {
            if id.is_empty()
                || id.len() > 96
                || id.trim().is_empty()
                || id.chars().any(char::is_control)
                || !ids.insert(id)
            {
                return Err("invalid or duplicate source ID");
            }
        }
        if ids.is_empty() || ids.len() > MAX_BATCH_INTENTS {
            return Err("invalid batch intent count");
        }
        let bytes = serde_json::to_vec(self).map_err(|_| "batch serialization failed")?;
        if bytes.len() > MAX_BATCH_BYTES {
            return Err("batch exceeds encoded byte limit");
        }
        Ok(())
    }
}

/// Catalog action. Generation and collection revisions are returned by the
/// catalog and must be carried forward exactly; they are not provider claims.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SnapshotRequest {
    ProviderSnapshotBegin {
        sync_id: String,
        source_revision: u64,
        provider_epoch: u64,
        expected_collection_revision: Option<u64>,
        /// Request staged publication. Require `staged:true` in the begin
        /// response; older catalogs may not implement this mode.
        #[serde(skip_serializing_if = "is_false")]
        staged: bool,
    },
    ProviderSnapshotBatch {
        sync_id: String,
        generation: u64,
        source_revision: u64,
        provider_epoch: u64,
        sequence: u64,
        expected_revision: u64,
        #[serde(flatten)]
        batch: SnapshotBatch,
    },
    ProviderSnapshotFinish {
        sync_id: String,
        generation: u64,
        source_revision: u64,
        provider_epoch: u64,
        expected_revision: u64,
        next_sequence: u64,
        complete: bool,
        remove_stale: bool,
        remove_uninstalled: bool,
        remove_expired: bool,
        remove_selection: bool,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_inventory_sdk_keeps_exact_hidden_occupancy_and_revision_cursor() {
        let request = LibraryRequest::IdentityInventory {
            cursor: None,
            limit: Some(32),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({"action":"identity_inventory","cursor":null,"limit":32})
        );
        let mut page:LibraryIdentityInventoryResponse=serde_json::from_value(json!({"ok":true,"revision":9,"entries":[{"id":"app-0000000000000001","uuid":"stable","title":"Hidden profile","aliases":["app-000000000000002a"],"visible":false,"monitor_profile":true,"properties":{}}],"next_cursor":{"revision":9,"after":"app-0000000000000001","query":""}})).unwrap();
        page.validate(32).unwrap();
        assert!(!page.entries[0].visible);
        page.next_cursor.as_mut().unwrap().revision = 10;
        assert!(page.validate(32).is_err());
        page.next_cursor = None;
        page.entries[0].aliases.push("app-000000000000002a".into());
        assert!(page.validate(32).is_err());
    }

    fn entry(id: &str) -> ProviderEntry {
        ProviderEntry {
            source_id: id.into(),
            entry: json!({"name":"Example"}),
            source_flags: None,
        }
    }

    #[test]
    fn checks_all_intents_and_encoded_limit() {
        let mut batch = SnapshotBatch {
            entries: vec![entry("one")],
            remove_source_ids: vec![],
            expire_source_ids: vec![],
            selection_remove_source_ids: vec![],
        };
        assert!(batch.validate().is_ok());
        batch.remove_source_ids.push("one".into());
        assert!(batch.validate().is_err());
        batch.remove_source_ids.clear();
        batch.entries[0].entry = json!({"name":"x".repeat(MAX_BATCH_BYTES)});
        assert!(batch.validate().is_err());
    }

    #[test]
    fn staged_mode_is_explicit_and_legacy_mode_omits_the_field() {
        let begin = |staged| SnapshotRequest::ProviderSnapshotBegin {
            sync_id: "sync".into(),
            source_revision: 1,
            provider_epoch: 0,
            expected_collection_revision: None,
            staged,
        };
        assert!(
            serde_json::to_value(begin(false))
                .unwrap()
                .get("staged")
                .is_none()
        );
        assert_eq!(serde_json::to_value(begin(true)).unwrap()["staged"], true);
    }

    #[test]
    fn batch_action_matches_catalog_wire_shape() {
        let action = SnapshotRequest::ProviderSnapshotBatch {
            sync_id: "sync".into(),
            generation: 1,
            source_revision: 2,
            provider_epoch: 0,
            sequence: 0,
            expected_revision: 3,
            batch: SnapshotBatch {
                entries: vec![entry("one")],
                remove_source_ids: vec![],
                expire_source_ids: vec![],
                selection_remove_source_ids: vec![],
            },
        };
        let value = serde_json::to_value(action).unwrap();
        assert_eq!(value["action"], "provider_snapshot_batch");
        assert_eq!(value["entries"][0]["source_id"], "one");
        assert!(value.get("batch").is_none());
    }
}

/// Domain types for independently implemented library providers. Registration,
/// selection, routing, and lifecycle ownership require host-side configuration
/// and support; these types alone do not activate a provider.
pub mod provider {
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeSet;

    pub const MAX_PAGE_ENTRIES: usize = 32;
    pub const MAX_ARTWORK_BYTES: u64 = 16 * 1024 * 1024;

    fn token(value: &str) -> bool {
        !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
    }

    /// Advertised functionality; the host must validate selection and grants at
    /// startup. This declaration neither grants access nor proves availability.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Capabilities {
        pub discovery: bool,
        pub artwork: bool,
        pub lifecycle: bool,
    }

    /// Provider-local identity and presentation, never a path or command.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Application {
        pub id: String,
        pub name: String,
        pub artwork: Option<ArtworkReference>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct ArtworkReference {
        pub id: String,
        pub revision: String,
    }

    /// All pages in a scan must carry the same provider-issued revision. A
    /// failed scan must not be reported as complete or used to remove rows.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct DiscoveryPage {
        pub revision: String,
        pub entries: Vec<Application>,
        pub next_cursor: Option<String>,
    }

    impl DiscoveryPage {
        pub fn validate(&self) -> Result<(), &'static str> {
            if !token(&self.revision)
                || self.entries.len() > MAX_PAGE_ENTRIES
                || self
                    .next_cursor
                    .as_deref()
                    .is_some_and(|cursor| !token(cursor))
            {
                return Err("invalid discovery page bounds");
            }
            let mut ids = BTreeSet::new();
            for entry in &self.entries {
                if !token(&entry.id)
                    || !token(&entry.name)
                    || !ids.insert(&entry.id)
                    || entry
                        .artwork
                        .as_ref()
                        .is_some_and(|art| !token(&art.id) || !token(&art.revision))
                {
                    return Err("invalid discovery entry");
                }
            }
            Ok(())
        }
    }

    /// Complete provider-owned image. The adapter moves its bytes over the
    /// binary-transfer SDK; the broker receives only an owner-bound offer.
    pub struct ArtworkImage {
        pub reference: ArtworkReference,
        pub mime: String,
        pub bytes: Vec<u8>,
    }
    impl ArtworkImage {
        pub fn validate(&self) -> Result<(), &'static str> {
            if !token(&self.reference.id)
                || !token(&self.reference.revision)
                || !matches!(
                    self.mime.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                )
                || self.bytes.is_empty()
                || self.bytes.len() as u64 > MAX_ARTWORK_BYTES
            {
                return Err("invalid artwork image");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod provider_tests {
    use super::provider::*;

    #[test]
    fn artwork_is_bounded_and_has_an_opaque_revision() {
        let mut image = ArtworkImage {
            reference: ArtworkReference {
                id: "cover".into(),
                revision: "v1".into(),
            },
            mime: "image/png".into(),
            bytes: vec![1, 2],
        };
        assert!(image.validate().is_ok());
        image.bytes = vec![0; MAX_ARTWORK_BYTES as usize + 1];
        assert!(image.validate().is_err());
    }

    #[test]
    fn lifecycle_identity_and_discovery_are_bounded() {
        let mut page = DiscoveryPage {
            revision: "v1".into(),
            entries: vec![],
            next_cursor: None,
        };
        let app = Application {
            id: "game".into(),
            name: "Game".into(),
            artwork: None,
        };
        page.entries = vec![app.clone(), app];
        assert!(page.validate().is_err());
        assert_eq!(page.entries.len(), 2);
    }
}

/// Startup-selected opaque artwork routing and shared wire validation.
pub mod artwork;
/// Adapter for a broker-registered provider service. Host-side routing and
/// lifecycle ownership remain separate responsibilities.
pub mod provider_adapter;

/// Incarnation-bound lifecycle requests and retirement assertions.
pub mod provider_lifecycle;

/// Optional, transport-neutral library reads and opaque execution handoff.
/// This is a separate service; it does not change the legacy `/1` wire format.
pub mod v2;
