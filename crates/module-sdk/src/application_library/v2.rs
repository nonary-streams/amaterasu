//! Optional interoperability profile for independently selected libraries.
//!
//! Application IDs and revisions are opaque. A protocol plugin owns any numeric
//! aliases. These types contain no transport, packet, path or command settings.
//! Capabilities describe implemented behavior, never authorization. Broker
//! principals are component identities, not authenticated people or devices.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod adapter;

#[cfg(test)]
mod tests;

pub const SERVICE: &str = "amaterasu.library/2";
pub const LAUNCH_SERVICE: &str = "amaterasu.launch/2";
pub const MAX_PAGE_ENTRIES: usize = 32;
pub const MAX_RESPONSE_BYTES: usize = 12_000;
pub const SEARCH: &str = "library.search/1";
pub const RESOLVE_LAUNCH: &str = "library.resolve-launch/1";

pub(crate) fn token(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(crate) fn key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// A logical, startup-selected library key, not a broker service supplied by a
/// client. IDs are unique inside that library, not across all libraries/stores.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationReference {
    pub library: String,
    pub id: String,
}

impl ApplicationReference {
    pub fn validate(&self) -> Result<(), &'static str> {
        if key(&self.library) && token(&self.id) {
            Ok(())
        } else {
            Err("invalid_application_reference")
        }
    }
}

/// Preserve the cursor unchanged. The implementation must bind its token to the
/// exact query and authorized scope; a revision change requires a fresh list.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub revision: String,
    pub token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Describe {},
    List {
        query: Query,
        cursor: Option<Cursor>,
        limit: usize,
    },
    Resolve {
        id: String,
    },
    /// Private handoff for the selected execution coordinator. The library must
    /// authorize and recheck readiness/revision before returning a target.
    ResolveLaunch {
        id: String,
        revision: String,
    },
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid = match self {
            Self::Describe {} => true,
            Self::Resolve { id } => token(id),
            Self::ResolveLaunch { id, revision } => token(id) && token(revision),
            Self::List {
                query,
                cursor,
                limit,
            } => {
                (1..=MAX_PAGE_ENTRIES).contains(limit)
                    && query.text.len() <= 512
                    && !query.text.chars().any(char::is_control)
                    && cursor
                        .as_ref()
                        .is_none_or(|cursor| token(&cursor.revision) && token(&cursor.token))
            }
        };
        if valid {
            Ok(())
        } else {
            Err("invalid_request")
        }
    }
}

/// This first profile defines reads, optional search, and optional execution
/// resolution. Future edit/artwork/source/action services negotiate separately;
/// their existence must not be inferred from a public metadata field.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub version: u32,
    /// Versioned feature identifiers, not service addresses. Consumers may
    /// ignore unknown features; they must never derive routing grants from them.
    pub features: BTreeSet<String>,
}

impl Capabilities {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 2 {
            return Err("unsupported_version");
        }
        if self.features.len() > 32
            || self.features.iter().any(|feature| {
                feature.len() > 128
                    || feature.rsplit_once('/').is_none_or(|(name, version)| {
                        !key(name)
                            || version
                                .parse::<u32>()
                                .ok()
                                .is_none_or(|value| value == 0 || value.to_string() != version)
                    })
            })
        {
            return Err("invalid_capabilities");
        }
        Ok(())
    }

    pub fn supports(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Installed {
    Yes,
    No,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    Unavailable,
    Preparing,
    #[default]
    Unknown,
}

/// Source-local and portable store identities have deliberately separate types.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableIdentity {
    pub namespace: String,
    pub id: String,
}

/// Public projection. Private execution configuration never belongs here.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub source: Option<SourceIdentity>,
    pub portable_ids: Vec<PortableIdentity>,
    pub installed: Installed,
    pub readiness: Readiness,
    pub unavailable_reason: Option<String>,
}

impl Entry {
    pub fn validate(&self) -> Result<(), &'static str> {
        let text = |value: &str, limit| {
            !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
        };
        let mut tags = BTreeSet::new();
        let mut portable_ids = BTreeSet::new();
        if !token(&self.id)
            || !token(&self.revision)
            || !text(&self.name, 512)
            || self
                .description
                .as_deref()
                .is_some_and(|value| !text(value, 2048))
            || self.tags.len() > 32
            || self.tags.iter().any(|tag| !token(tag) || !tags.insert(tag))
            || self
                .source
                .as_ref()
                .is_some_and(|source| !key(&source.provider) || !token(&source.id))
            || self.portable_ids.len() > 16
            || self.portable_ids.iter().any(|id| {
                !key(&id.namespace)
                    || !token(&id.id)
                    || !portable_ids.insert((&id.namespace, &id.id))
            })
            || self
                .unavailable_reason
                .as_deref()
                .is_some_and(|value| !text(value, 512))
        {
            return Err("invalid_library_response");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub revision: String,
    pub entries: Vec<Entry>,
    pub next_cursor: Option<Cursor>,
}

impl Page {
    pub fn validate_for(&self, cursor: Option<&Cursor>, limit: usize) -> Result<(), &'static str> {
        let mut ids = BTreeSet::new();
        if !token(&self.revision)
            || !(1..=MAX_PAGE_ENTRIES).contains(&limit)
            || self.entries.len() > limit
            || cursor.is_some_and(|cursor| cursor.revision != self.revision)
            || self.next_cursor.as_ref().is_some_and(|next| {
                self.entries.is_empty()
                    || next.revision != self.revision
                    || !token(&next.token)
                    || Some(next) == cursor
            })
        {
            return Err("invalid_library_response");
        }
        for entry in &self.entries {
            entry.validate()?;
            if !ids.insert(&entry.id) {
                return Err("invalid_library_response");
            }
        }
        Ok(())
    }
}

/// An execution reference returned only to an authorized coordinator. `executor`
/// selects a fixed route in that library's startup configuration. The client
/// cannot supply a broker service, executable, path, or command through it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LaunchTarget {
    pub id: String,
    pub revision: String,
    pub executor: String,
    pub application_id: String,
}

impl LaunchTarget {
    pub fn validate_for(&self, id: &str, revision: &str) -> Result<(), &'static str> {
        if !token(id)
            || !token(revision)
            || self.id != id
            || self.revision != revision
            || !key(&self.executor)
            || !token(&self.application_id)
            || self.application_id.len() > 96
        {
            return Err("invalid_library_response");
        }
        Ok(())
    }
}

/// Optional execution-coordinator profile. Retry the identical operation and
/// reference after a lost reply; use lookup/stop for reconciliation. A library
/// revision is an equality token, never a numeric GameStream ID.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchRequest {
    Describe {},
    Launch {
        operation_id: String,
        application: ApplicationReference,
        revision: String,
    },
    Lookup {
        operation_id: String,
    },
    Status {
        launch_id: u64,
    },
    Stop {
        launch_id: u64,
    },
}

impl LaunchRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Launch {
                operation_id,
                application,
                revision,
            } => {
                application.validate()?;
                if !token(operation_id)
                    || !operation_id.bytes().all(|byte| byte.is_ascii_graphic())
                    || !token(revision)
                {
                    return Err("invalid_request");
                }
            }
            Self::Lookup { operation_id } => {
                if !token(operation_id) || !operation_id.bytes().all(|byte| byte.is_ascii_graphic())
                {
                    return Err("invalid_request");
                }
            }
            Self::Status { launch_id: 0 } | Self::Stop { launch_id: 0 } => {
                return Err("invalid_request");
            }
            _ => {}
        }
        Ok(())
    }
}
