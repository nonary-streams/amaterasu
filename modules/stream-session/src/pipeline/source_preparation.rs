//! Schedule selected plugins against a scoped source. Display settings remain
//! entirely in the plugin/provider contract, never in this coordinator.
use super::*;
use amaterasu_module_sdk::display_settings::{
    self, Operation as DisplayOperation, preparation::Context,
};

pub(super) fn scope(record: &Record) -> &super::shared_capture::SettingsScope {
    record
        .shared_capture
        .as_ref()
        .and_then(|s| s.settings_scope.as_ref())
        .or(record.source_scope.as_ref())
        .expect("source settings scope")
}
fn scope_mut(record: &mut Record) -> &mut super::shared_capture::SettingsScope {
    if record
        .shared_capture
        .as_ref()
        .is_some_and(|s| s.settings_scope.is_some())
    {
        record
            .shared_capture
            .as_mut()
            .unwrap()
            .settings_scope
            .as_mut()
            .unwrap()
    } else {
        record.source_scope.as_mut().expect("source settings scope")
    }
}
pub(super) fn needs_cleanup(record: &Record) -> bool {
    record
        .source_scope
        .as_ref()
        .is_some_and(|scope| !scope.closed)
        || (record.shared_capture_attached
            && record
                .shared_capture
                .as_ref()
                .and_then(|s| s.settings_scope.as_ref())
                .is_some_and(|scope| !scope.closed))
}
pub(super) fn redirect(record: &mut Record, job: &mut Job) -> Result<(), &'static str> {
    if job.step != Step::AcquireOutput || record.source_preparation.is_none() {
        return Ok(());
    }
    let start = record.start.as_ref().ok_or("source_context_unavailable")?;
    if start.lifecycle_target.is_none() {
        return Err("source_lifecycle_required");
    }
    if record
        .shared_capture
        .as_ref()
        .and_then(|s| s.settings_scope.as_ref())
        .is_none()
        && record.source_scope.is_none()
    {
        use sha2::{Digest, Sha256};
        record.source_scope = Some(super::shared_capture::SettingsScope {
            service: record
                .source_preparation
                .as_ref()
                .unwrap()
                .display_settings_service
                .clone(),
            operation_id: format!("source-{:x}", Sha256::digest(&record.config.operation_id)),
            scope: None,
            prepared: false,
            closed: false,
        });
    }
    let scope = scope(record);
    if scope.scope.is_none() {
        job.step = Step::OpenSourceScope;
    } else if !scope.prepared {
        job.step = Step::ReadSourceHookSettings;
    }
    Ok(())
}
pub(super) fn command(record: &Record, step: Step) -> (String, Value) {
    let scope = scope(record);
    let wire = |operation: DisplayOperation| {
        (
            scope.service.clone(),
            operation.payload().expect("validated display operation"),
        )
    };
    match step {
        Step::OpenSourceScope => wire(DisplayOperation::OpenScope {
            operation_id: scope.operation_id.clone(),
            source: record
                .virtual_display_output
                .as_ref()
                .or_else(|| record.start.as_ref().map(|s| &s.output))
                .unwrap()
                .clone(),
            consumers: record
                .source_preparation
                .as_ref()
                .unwrap()
                .hooks
                .iter()
                .map(|h| h.provider.clone())
                .collect(),
        }),
        Step::CloseSourceScope => wire(DisplayOperation::CloseScope {
            operation_id: scope.operation_id.clone(),
        }),
        Step::ReadSourceHookSettings => {
            let hook = &record.source_preparation.as_ref().unwrap().hooks[record.source_hook_index];
            (
                record
                    .providers
                    .lifecycle
                    .clone()
                    .expect("selected lifecycle service"),
                json!({"operation":"read_frozen_provider_settings","target":record.start.as_ref().unwrap().lifecycle_target,"provider":hook.provider}),
            )
        }
        Step::PrepareSourceHook => {
            let selection = record.source_preparation.as_ref().unwrap();
            let hook = &selection.hooks[record.source_hook_index];
            let (revision, settings) = record.source_hook_settings.as_ref().unwrap();
            let start = record.start.as_ref().unwrap();
            let context = Context {
                display_scope: scope.scope.clone().unwrap(),
                settings_service: scope.service.clone(),
                lifecycle_target: start.lifecycle_target.clone().unwrap(),
                output_format: start.configuration.video_stream_format,
                settings_revision: revision.clone(),
                settings: settings.clone(),
            };
            (
                hook.service.clone(),
                display_settings::preparation::payload(&context),
            )
        }
        _ => unreachable!(),
    }
}
pub(super) fn consume(record: &mut Record, job: &mut Job, payload: &Value, now: u64) -> bool {
    match job.step {
        Step::OpenSourceScope => {
            let parsed =
                serde_json::from_value::<display_settings::Scope>(payload["scope"].clone());
            if payload["ok"] != true || !parsed.as_ref().is_ok_and(|scope| scope.validate().is_ok())
            {
                job.fail(record, "display_scope_unavailable", now);
                return true;
            }
            scope_mut(record).scope = parsed.ok();
            job.step = Step::ReadSourceHookSettings;
        }
        Step::ReadSourceHookSettings => {
            let hook = &record.source_preparation.as_ref().unwrap().hooks[record.source_hook_index];
            if payload["status"] != "succeeded"
                || payload["provider"] != hook.provider
                || payload["target"] != json!(record.start.as_ref().unwrap().lifecycle_target)
                || !payload["revision"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty() && r.len() <= 128)
                || !payload["settings"].is_object()
            {
                job.fail(record, "source_plugin_settings_unavailable", now);
                return true;
            }
            record.source_hook_settings = Some((
                payload["revision"].as_str().unwrap().into(),
                payload["settings"].clone(),
            ));
            job.step = Step::PrepareSourceHook;
        }
        Step::PrepareSourceHook => {
            if payload["ok"] != true || payload["scope"] != json!(scope(record).scope) {
                job.fail(record, "source_plugin_preparation_failed", now);
                return true;
            }
            record.source_hook_index += 1;
            record.source_hook_settings = None;
            if record.source_hook_index == record.source_preparation.as_ref().unwrap().hooks.len() {
                scope_mut(record).prepared = true;
                job.step = Step::AcquireOutput;
            } else {
                job.step = Step::ReadSourceHookSettings;
            }
        }
        Step::CloseSourceScope => {
            if payload["ok"] == true && payload["closed"] == true {
                scope_mut(record).closed = true;
                job.source_scope_tried = true;
            } else {
                record.cleanup_error = Some("display_scope_cleanup_failed");
                job.source_scope_tried = true;
            }
        }
        _ => return false,
    }
    true
}
