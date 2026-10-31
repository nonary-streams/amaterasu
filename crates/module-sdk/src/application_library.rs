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
    pub const MAX_ARTWORK_CHUNK_BYTES: usize = 8 * 1024;
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

    /// Artwork bytes are bounded per chunk and across the complete object. The
    /// consumer must also check exact requested reference, offset, revision,
    /// MIME, and contiguous progress across every chunk.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct ArtworkChunk {
        pub reference: ArtworkReference,
        pub mime: String,
        pub offset: u64,
        pub total_bytes: u64,
        pub bytes: Vec<u8>,
    }

    impl ArtworkChunk {
        /// Validate a chunk against the pinned reference and expected stream position.
        pub fn validate_for(
            &self,
            reference: &ArtworkReference,
            mime: &str,
            offset: u64,
            total_bytes: u64,
        ) -> Result<(), &'static str> {
            self.validate()?;
            if &self.reference != reference
                || self.mime != mime
                || self.offset != offset
                || self.total_bytes != total_bytes
            {
                return Err("artwork stream changed");
            }
            Ok(())
        }

        pub fn validate(&self) -> Result<(), &'static str> {
            let end = self.offset.checked_add(self.bytes.len() as u64);
            if !token(&self.reference.id)
                || !token(&self.reference.revision)
                || !matches!(
                    self.mime.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                )
                || self.total_bytes == 0
                || self.total_bytes > MAX_ARTWORK_BYTES
                || self.bytes.is_empty()
                || self.bytes.len() > MAX_ARTWORK_CHUNK_BYTES
                || end.is_none_or(|end| end > self.total_bytes)
            {
                return Err("invalid artwork chunk");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod provider_tests {
    use super::provider::*;

    #[test]
    fn artwork_is_bounded_and_pinned_to_stream() {
        let reference = ArtworkReference {
            id: "cover".into(),
            revision: "v1".into(),
        };
        let mut chunk = ArtworkChunk {
            reference: reference.clone(),
            mime: "image/png".into(),
            offset: 0,
            total_bytes: 2,
            bytes: vec![1, 2],
        };
        assert!(chunk.validate_for(&reference, "image/png", 0, 2).is_ok());
        assert!(chunk.validate_for(&reference, "image/png", 1, 2).is_err());
        chunk.offset = u64::MAX;
        assert!(chunk.validate().is_err());
        chunk.offset = 0;
        chunk.bytes = vec![0; MAX_ARTWORK_CHUNK_BYTES + 1];
        assert!(chunk.validate().is_err());
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
/// Private, selected application-policy resolver profile.
pub mod policy;
/// Adapter for a broker-registered provider service. Host-side routing and
/// lifecycle ownership remain separate responsibilities.
pub mod provider_adapter;

/// Incarnation-bound lifecycle requests and retirement assertions.
pub mod provider_lifecycle;

/// Optional, transport-neutral library reads and opaque execution handoff.
/// This is a separate service; it does not change the legacy `/1` wire format.
pub mod v2;
