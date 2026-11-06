//! Catalog references select trusted artwork services; callers never select native handles.
use amaterasu_module_sdk::application_library::{
    artwork::ArtworkRoutes, provider::ArtworkReference,
};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Artwork {
    Steam {
        app_id: u32,
        revision: String,
    },
    Lutris {
        game_id: i64,
        revision: String,
    },
    Playnite {
        game_id: String,
        revision: String,
    },
    External {
        source: String,
        reference: ArtworkReference,
    },
}
impl Artwork {
    pub(super) fn revision(&self) -> &str {
        match self {
            Self::Steam { revision, .. }
            | Self::Lutris { revision, .. }
            | Self::Playnite { revision, .. } => revision,
            Self::External { reference, .. } => &reference.revision,
        }
    }
    pub(super) fn route(&self, routes: &ArtworkRoutes, initiator: &str) -> Option<(String, Value)> {
        let (service, payload) = match self {
            Self::Steam { app_id, revision } if *app_id > 0 => (
                "amaterasu.steam/1",
                json!({"action":"artwork_open","app_id":app_id,"revision":revision}),
            ),
            Self::Lutris { game_id, revision } if *game_id > 0 => (
                "amaterasu.lutris/1",
                json!({"action":"artwork_open","game_id":game_id,"revision":revision}),
            ),
            Self::Playnite { game_id, revision } => (
                "amaterasu.playnite/1",
                json!({"action":"artwork_open","game_id":game_id,"revision":revision}),
            ),
            Self::External { source, reference } => {
                return routes.open(source, reference, initiator).ok();
            }
            _ => return None,
        };
        Some((service.into(), payload))
    }
}
