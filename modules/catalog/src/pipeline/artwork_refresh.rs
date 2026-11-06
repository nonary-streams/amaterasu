//! Importer-owned artwork maintenance, independent of application synchronization.
use super::*;

pub(super) struct Refresh {
    source_id: String,
    artwork: ArtworkReference,
    marker: Marker,
    collection_revision: u64,
    metadata: Value,
}

impl Pipeline {
    pub(super) fn start_artwork_refresh(
        &mut self,
        context: Context,
        source_id: String,
        artwork: ArtworkReference,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if !valid_source_id(&source_id) {
            return complete(
                send,
                context.request_id,
                error("invalid_request", "invalid artwork source ID"),
            );
        }
        let caller = context.importer.as_deref().expect("authorized importer");
        let payload = json!({"action":"collection_get","collection":context.key,"key":entry_key(caller, &source_id)});
        self.submit(
            context,
            payload,
            Stage::ArtworkMarker { source_id, artwork },
            send,
        )
    }

    pub(super) fn read_artwork_entry(
        &mut self,
        context: Context,
        state_revision: u64,
        payload: Value,
        source_id: String,
        artwork: ArtworkReference,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if payload["value"].is_null() {
            return complete(send, context.request_id, json!({"ok":true,"changed":false}));
        }
        let caller = context.importer.as_deref().expect("authorized importer");
        let marker: Marker = match serde_json::from_value(payload["value"].clone()) {
            Ok(marker) if validate_marker(&marker, caller, &source_id) => marker,
            _ => return complete(send, context.request_id, malformed()),
        };
        let request =
            json!({"action":"collection_get","collection":context.key,"key":marker.entry_id});
        self.submit(
            context,
            request,
            Stage::ArtworkEntry(Box::new(Refresh {
                source_id,
                artwork,
                marker,
                collection_revision: state_revision,
                metadata: payload["metadata"].clone(),
            })),
            send,
        )
    }

    pub(super) fn finish_artwork_refresh(
        &mut self,
        context: Context,
        state_revision: u64,
        payload: Value,
        mut update: Refresh,
        send: &mut impl FnMut(Command) -> TransportResult,
    ) -> ModuleResult<()> {
        if state_revision != update.collection_revision || payload["metadata"] != update.metadata {
            return complete(
                send,
                context.request_id,
                error(
                    "revision_conflict",
                    "catalog changed during artwork refresh",
                ),
            );
        }
        let mut entry: Entry = match serde_json::from_value::<Entry>(payload["value"].clone()) {
            Ok(entry) if entry.validate().is_ok() => entry,
            _ => return complete(send, context.request_id, malformed()),
        };
        // The importer may update only the revision of its existing image.
        // A changed application/provider identity requires normal reconciliation.
        if !same_artwork_identity(entry.artwork.as_ref(), &update.artwork) {
            return complete(
                send,
                context.request_id,
                error("provider_artwork_required", "artwork identity differs"),
            );
        }
        let caller = context.importer.as_deref().expect("authorized importer");
        let was_owned = provider_entry_fingerprint(caller, &entry) == update.marker.fingerprint;
        let old = entry.artwork.replace(update.artwork);
        if let Err(failure) = validate_provider_entry(
            caller,
            self.provider_launch_principals
                .get(caller)
                .map(String::as_str),
            &update.source_id,
            &entry,
            false,
        ) {
            return complete(send, context.request_id, failure);
        }
        if old == entry.artwork {
            return complete(send, context.request_id, json!({"ok":true,"changed":false}));
        }
        if was_owned {
            update.marker.fingerprint = provider_entry_fingerprint(caller, &entry);
        }
        let Some(revision) = update.metadata["revision"]
            .as_u64()
            .and_then(|revision| revision.checked_add(1))
        else {
            return complete(send, context.request_id, malformed());
        };
        update.metadata["revision"] = json!(revision);
        let request = json!({"action":"collection_commit","collection":context.key,
            "expected_revision":state_revision,"metadata":update.metadata,
            "edits":[{"key":update.marker.entry_id,"value":entry},
                {"key":entry_key(caller,&update.source_id),"value":marker_value(&update.marker)}]});
        self.provider_sessions
            .retain(|_, session| session.key != context.key);
        self.submit(
            context,
            request,
            Stage::Saving(json!({"ok":true,"changed":true,"revision":revision})),
            send,
        )
    }
}

fn same_artwork_identity(old: Option<&ArtworkReference>, new: &ArtworkReference) -> bool {
    match (old, new) {
        (
            Some(ArtworkReference::Steam { app_id: a, .. }),
            ArtworkReference::Steam { app_id: b, .. },
        ) => a == b,
        (
            Some(ArtworkReference::Lutris { game_id: a, .. }),
            ArtworkReference::Lutris { game_id: b, .. },
        ) => a == b,
        (
            Some(ArtworkReference::Playnite { game_id: a, .. }),
            ArtworkReference::Playnite { game_id: b, .. },
        ) => a == b,
        (
            Some(ArtworkReference::External {
                source: a,
                reference: ar,
            }),
            ArtworkReference::External {
                source: b,
                reference: br,
            },
        ) => a == b && ar.id == br.id,
        _ => false,
    }
}
