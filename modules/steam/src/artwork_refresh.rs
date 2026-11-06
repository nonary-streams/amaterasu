//! Keep imported artwork references current without enabling catalog auto-sync.
use crate::Provider;
use amaterasu_module_sdk::{Command, ModuleResult, Response, RuntimeClient};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Refresh {
    completed: BTreeMap<u32, String>,
    pending: Option<(u64, u32, String)>,
    sequence: u64,
    retry_at: Option<Instant>,
}

impl Refresh {
    pub(super) fn tick(
        &mut self,
        runtime: &RuntimeClient,
        provider: &Provider,
    ) -> ModuleResult<()> {
        if let Some((request, app_id, revision)) = self.pending.clone() {
            match runtime.send_ephemeral(Command::TakeResult { request })? {
                Response::Pending => return Ok(()),
                Response::Result { payload } => self.complete(app_id, revision, payload),
                Response::Error { .. } => {
                    self.pending = None;
                    self.retry_at = Some(Instant::now() + Duration::from_secs(5));
                }
                _ => return Err("unexpected artwork refresh result".into()),
            }
        }
        if !provider.enabled
            || provider.scan_in_progress()
            || !provider.has_artwork_snapshot()
            || self
                .retry_at
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Ok(());
        }
        self.completed
            .retain(|id, _| provider.games.contains_key(id));
        let next = provider.games.iter().find_map(|(&app_id, game)| {
            let revision = game.artwork_revision.as_ref()?;
            (self.completed.get(&app_id) != Some(revision)).then(|| (app_id, revision.clone()))
        });
        let Some((app_id, revision)) = next else {
            return Ok(());
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("artwork refresh sequence exhausted")?;
        let response = runtime.send(Command::Request {
            service: "amaterasu.catalog/1".into(),
            payload: json!({"action":"provider_artwork_refresh","source_id":format!("steam:{app_id}"),
                "artwork":{"provider":"steam","app_id":app_id,"revision":revision}}),
            timeout_ms: 5_000,
            operation_id: format!("steam.artwork-refresh.{}", self.sequence),
            parent: None,
        })?;
        match response {
            Response::Accepted { id } => self.pending = Some((id, app_id, revision)),
            Response::Result { payload } => self.complete(app_id, revision, payload),
            Response::Error { .. } => self.retry_at = Some(Instant::now() + Duration::from_secs(5)),
            _ => return Err("unexpected artwork refresh admission".into()),
        }
        Ok(())
    }

    fn complete(&mut self, app_id: u32, revision: String, payload: Value) {
        self.pending = None;
        if payload["ok"] == true || payload["error"]["code"] == "provider_artwork_required" {
            self.completed.insert(app_id, revision);
            self.retry_at = None;
        } else {
            self.retry_at = Some(Instant::now() + Duration::from_secs(5));
        }
    }
}
