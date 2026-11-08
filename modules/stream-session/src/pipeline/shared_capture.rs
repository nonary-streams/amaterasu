//! External capture keeps one durable SDK display reference while independent
//! transport pipelines hold owner-bound consumer leases on the source output.
use super::{Record, Resource, State};
use crate::model::{PrepareConfig, StartConfig};
use amaterasu_module_sdk::virtual_display::Operation;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    pub version: u32,
    pub operation_id: String,
    pub session: amaterasu_extension_api::LifecycleTarget,
    pub stream: amaterasu_extension_api::LifecycleTarget,
    pub identity: String,
    pub reference_id: String,
    pub capture_provider: String,
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub hdr_enabled: bool,
    pub output: Option<String>,
}

impl Source {
    pub fn valid(&self) -> bool {
        self.version == 1
            && crate::model::valid_name(&self.operation_id)
            && matches!(&self.session, amaterasu_extension_api::LifecycleTarget::Session {
                host_epoch, session_id, generation:1.. } if crate::model::valid_name(host_epoch) && crate::model::valid_name(session_id))
            && matches!((&self.session, &self.stream), (
                amaterasu_extension_api::LifecycleTarget::Session {host_epoch, session_id, ..},
                amaterasu_extension_api::LifecycleTarget::Stream {host_epoch:source_epoch, session_id:source_session, stream_id, generation:1..})
                if host_epoch == source_epoch && session_id == source_session && crate::model::valid_name(stream_id))
            && self.identity.len() == 36
            && self.identity.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
            && self.reference_id == format!("shared-source-{}", self.identity)
            && crate::model::valid_name(&self.capture_provider)
            && self.width > 0
            && self.width <= 16384
            && self.height > 0
            && self.height <= 16384
            && (1000..=1_000_000).contains(&self.refresh_millihz)
            && self
                .output
                .as_deref()
                .is_none_or(super::virtual_display::is_kwin_virtual_output)
    }

    pub fn create(&self) -> Value {
        serde_json::to_value(Operation::Create {
            identity: self.identity.clone(),
            reference_id: Some(self.reference_id.clone()),
            name: "Amaterasu".into(),
            width: self.width,
            height: self.height,
            refresh_millihz: self.refresh_millihz,
            hdr_enabled: self.hdr_enabled,
            dd_virtual_display_scale: None,
            virtual_display_layout: None,
            expected_topology_revision: None,
            expected_topology_domain_id: None,
        })
        .expect("validated source reference serializes")
    }

    pub fn status(&self) -> Value {
        serde_json::to_value(Operation::Status {
            identity: Some(self.identity.clone()),
            reference_id: Some(self.reference_id.clone()),
        })
        .expect("validated source reference serializes")
    }

    pub fn remove(&self) -> Value {
        serde_json::to_value(Operation::Remove {
            identity: self.identity.clone(),
            reference_id: Some(self.reference_id.clone()),
        })
        .expect("validated source reference serializes")
    }

    fn same_group(&self, other: &Self) -> bool {
        self.version == other.version
            && self.operation_id == other.operation_id
            && self.session == other.session
            && self.stream == other.stream
            && self.identity == other.identity
            && self.reference_id == other.reference_id
            && self.capture_provider == other.capture_provider
            && self.width == other.width
            && self.height == other.height
            && self.refresh_millihz == other.refresh_millihz
            && self.hdr_enabled == other.hdr_enabled
    }
}

pub(super) fn borrow_source(
    records: &BTreeMap<u64, Record>,
    owner: &str,
    config: &PrepareConfig,
) -> Result<Option<Source>, &'static str> {
    let Some(operation) = &config.shared_source_operation_id else {
        return Ok(None);
    };
    if owner != "amaterasu.rtsp" {
        return Err("permission_denied");
    }
    let source = records
        .values()
        .find_map(|record| {
            let source = record.shared_capture.as_ref()?;
            (record.owner == owner
                && record.state == State::Active
                && record.shared_capture_attached
                && record.virtual_display_attempted
                && record.resources.contains_key(&Resource::Video)
                && source.operation_id == *operation
                && config.lifecycle_session_target.as_ref() == Some(&source.session)
                && source.output.is_some()
                && record.virtual_display_output == source.output)
                .then_some(source)
        })
        .ok_or("shared_capture_source_not_ready")?;
    if !source.valid() {
        return Err("invalid_shared_source_reference");
    }
    Ok(Some(source.clone()))
}

pub(super) fn validate_start(record: &Record, start: &StartConfig) -> Result<(), &'static str> {
    if record.config.shared_source_operation_id.is_none() {
        return Ok(());
    }
    let source = record
        .shared_capture
        .as_ref()
        .ok_or("shared_capture_source_not_ready")?;
    if !record.shared_capture_attached
        || !source.valid()
        || start.output != "amaterasu-owned-display"
        || start.source_id.is_some()
        || json!(start.capture_provider) != source.capture_provider
        || !start.lifecycle_capture_mode.is_external()
        || start.lifecycle_shared_source.as_ref() != Some(&source.stream)
    {
        return Err("shared_capture_configuration_changed");
    }
    Ok(())
}

pub(super) fn initialize_source(record: &mut Record) -> Result<(), &'static str> {
    if record.shared_capture.is_some()
        || !record.config.lifecycle_capture_mode.is_external()
        || !super::virtual_display::requested(record)
    {
        return Ok(());
    }
    let start = record
        .start
        .as_ref()
        .ok_or("shared_capture_start_missing")?;
    let operation_id = record
        .config
        .rtsp_operation_id
        .clone()
        .ok_or("shared_capture_operation_missing")?;
    let identity = super::virtual_display::identity(record);
    let source = Source {
        version: 1,
        operation_id,
        session: record
            .config
            .lifecycle_session_target
            .clone()
            .ok_or("invalid_lifecycle_identity")?,
        stream: start
            .lifecycle_target
            .clone()
            .ok_or("invalid_lifecycle_identity")?,
        reference_id: format!("shared-source-{identity}"),
        identity,
        capture_provider: json!(start.capture_provider)
            .as_str()
            .ok_or("invalid_capture_provider")?
            .into(),
        width: start.configuration.width,
        height: start.configuration.height,
        refresh_millihz: start.configuration.fps * 1000,
        // Freeze the processing input range on the shared source. Transport
        // output HDR belongs to the encoder and must not enable source HDR
        // for an SDR-to-HDR processor or change when another client joins.
        hdr_enabled: record
            .gpu_processing
            .as_ref()
            .and_then(|plan| plan["source_hdr_required"].as_bool())
            .unwrap_or(
                start.configuration.video_stream_format.dynamic_range
                    == amaterasu_module_sdk::DynamicRange::Hdr,
            ),
        output: None,
    };
    if !source.valid() {
        return Err("invalid_shared_source_reference");
    }
    record.shared_capture = Some(source);
    record.shared_capture_attached = true;
    Ok(())
}

pub(super) fn detach_if_borrowed(record: &mut Record, records: &BTreeMap<u64, Record>) {
    let Some(source) = &record.shared_capture else {
        return;
    };
    // All this consumer's native producers and transports must have retired
    // before its durable group claim can be handed to another consumer.
    if !record.shared_capture_attached
        || record.restart_lifecycle_preparation.is_some()
        || record.uncertain.is_some()
        || record.input_attempted
        || record.input_lease.is_some()
        || record
            .attempted
            .iter()
            .any(|resource| *resource != Resource::Session)
        || record
            .resources
            .keys()
            .any(|resource| *resource != Resource::Session)
    {
        return;
    }
    if records.values().any(|other| {
        other.owner == record.owner
            && other.shared_capture_attached
            && other
                .shared_capture
                .as_ref()
                .is_some_and(|other| source.same_group(other))
    }) {
        record.shared_capture_attached = false;
        record.virtual_display_attempted = false;
        record.virtual_display_output = None;
        record.virtual_display_state = "shared_retained".into();
    }
}
