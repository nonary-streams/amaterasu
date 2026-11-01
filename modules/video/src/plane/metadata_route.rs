//! Trusted bindings and per-session capability snapshots for AFM1 forwarding.
use super::*;
use amaterasu_module_sdk::{
    encode::FormatId,
    media::metadata::{
        self, CarriageOwner, MetadataCapabilitySnapshot, MetadataPolicy, MetadataStage,
        MetadataTreatment,
    },
};

pub(super) struct BoundRoute {
    pub spec: crate::model::MetadataRoute,
    capture: NativeBinding,
    processor: NativeBinding,
    encoder: NativeBinding,
}

pub(super) struct GenerationPins {
    capture: u64,
    processor: u64,
    encoder: u64,
}

impl BoundRoute {
    pub fn bind(runtime: &RuntimeClient, spec: &crate::model::MetadataRoute) -> ModuleResult<Self> {
        // SAFETY: trusted settings select independently granted typed services;
        // each binding remains on the owning Video module thread.
        unsafe {
            Ok(Self {
                spec: spec.clone(),
                capture: metadata::bind_capabilities_with_pinning(
                    runtime,
                    &spec.capture_capabilities,
                )?,
                processor: metadata::bind_capabilities_with_pinning(
                    runtime,
                    &spec.processor_capabilities,
                )?,
                encoder: metadata::bind_capabilities_with_pinning(
                    runtime,
                    &spec.encoder_capabilities,
                )?,
            })
        }
    }

    pub fn matches(&self, input: &Pump<'_>, encoder_service: &str) -> bool {
        input.configuration.extension_profile.as_ref() == Some(&self.spec.profile)
            && input.provider.service() == self.spec.capture_service
            && encoder_service == self.spec.encoder_control_service
    }

    pub fn schema(&self) -> FormatId {
        FormatId {
            namespace_id: self.spec.schema.namespace_id,
            value: self.spec.schema.value,
            schema: self.spec.schema.schema,
        }
    }

    pub fn probe(&self, configuration: &Configuration) -> Result<(), &'static str> {
        if configuration.extension_profile.as_ref() != Some(&self.spec.profile)
            || !configuration.validate_submitted_extension()
        {
            return Err("metadata_probe_configuration");
        }
        let id = |format: amaterasu_contracts::media_profile::FormatId| EncoderFormatId {
            namespace_id: format.namespace_id,
            value: format.value,
            schema: format.schema,
        };
        let schema = FrameMetadataSchemaId {
            namespace_id: self.spec.schema.namespace_id,
            value: self.spec.schema.value,
            schema: self.spec.schema.schema,
        };
        for (binding, stage) in [
            (&self.capture, FRAME_METADATA_STAGE_CAPTURE),
            (&self.processor, FRAME_METADATA_STAGE_PROCESSOR),
            (&self.encoder, FRAME_METADATA_STAGE_ENCODER),
        ] {
            // SAFETY: typed binding stays live on this module thread. Both size
            // and minor cover the additive source-independent callback.
            let api = unsafe { binding.table::<FrameMetadataCapabilitiesApiV1_2>() }
                .map_err(|_| "metadata_profile_probe_unavailable")?;
            if api.base.base.base.header.minor < 2 {
                return Err("metadata_profile_probe_unavailable");
            }
            let query = FrameMetadataProfileProbeV1 {
                size: size_of::<FrameMetadataProfileProbeV1>() as u32,
                version: 1,
                stage,
                owner: FRAME_METADATA_OWNER_FRAMER,
                schema,
                codec: id(self.spec.profile.codec),
                framing: id(self.spec.profile.framing),
                width: configuration.width,
                height: configuration.height,
                fps: configuration.fps,
                bit_depth: 8,
                chroma: ENCODER_CHROMA_420,
                reserved: 0,
                bitrate: u64::from(configuration.bitrate),
            };
            if unsafe { (api.base.probe_profile)(binding.context(), &query) } != STATUS_OK {
                return Err("metadata_profile_unsupported");
            }
        }
        Ok(())
    }

    /// Atomically pin each selected generation at its provider. Partial
    /// acquisition is unwound in reverse order before returning BUSY or error.
    pub fn pin(
        &self,
        video: u64,
        capture: u64,
        encoder: u64,
        source_generation: u64,
        versions: MetadataCapabilities,
    ) -> Result<Option<GenerationPins>, &'static str> {
        let schema = FrameMetadataSchemaId {
            namespace_id: self.spec.schema.namespace_id,
            value: self.spec.schema.value,
            schema: self.spec.schema.schema,
        };
        let mut leases = [0u64; 3];
        let selected = [
            (
                &self.capture,
                capture,
                versions.capture,
                FRAME_METADATA_STAGE_CAPTURE,
            ),
            (
                &self.processor,
                video,
                versions.processor,
                FRAME_METADATA_STAGE_PROCESSOR,
            ),
            (
                &self.encoder,
                encoder,
                versions.encoder,
                FRAME_METADATA_STAGE_ENCODER,
            ),
        ];
        let mut tables = Vec::with_capacity(selected.len());
        for (binding, _, _, _) in &selected {
            // SAFETY: binding remains live on this thread throughout pinning.
            let api = unsafe { binding.table::<FrameMetadataCapabilitiesApiV1_2>() }
                .map_err(|_| "metadata_pinning_unavailable")?;
            if api.base.base.base.header.minor < 2 {
                return Err("metadata_pinning_unavailable");
            }
            tables.push(api);
        }
        for (index, (binding, provider_session, generation, stage)) in selected.iter().enumerate() {
            let api = tables[index];
            let request = FrameMetadataGenerationPinV1 {
                size: size_of::<FrameMetadataGenerationPinV1>() as u32,
                version: 1,
                stage: *stage,
                owner: FRAME_METADATA_OWNER_FRAMER,
                provider_session: *provider_session,
                host_session: video,
                source_id: capture,
                source_generation,
                config_generation: 1,
                capability_generation: *generation,
                schema,
            };
            // SAFETY: the bounded request and lease output are borrowed synchronously.
            let status =
                unsafe { (api.pin_generation)(binding.context(), &request, &mut leases[index]) };
            if status != STATUS_OK || leases[index] == 0 {
                for prior in (0..index).rev() {
                    // SAFETY: successful prior leases remain live and unpin is a barrier.
                    unsafe {
                        (tables[prior].unpin_generation)(selected[prior].0.context(), leases[prior])
                    };
                }
                return if matches!(status, MEDIA_BUSY | MEDIA_PENDING) {
                    Ok(None)
                } else {
                    Err("metadata_generation_pin_failed")
                };
            }
        }
        Ok(Some(GenerationPins {
            capture: leases[0],
            processor: leases[1],
            encoder: leases[2],
        }))
    }

    pub fn unpin(&self, pins: GenerationPins) {
        for (binding, lease) in [
            (&self.encoder, pins.encoder),
            (&self.processor, pins.processor),
            (&self.capture, pins.capture),
        ] {
            // SAFETY: all successful leases were obtained from these live typed bindings.
            let api = unsafe { binding.table::<FrameMetadataCapabilitiesApiV1_2>() }
                .expect("validated pin table");
            unsafe { (api.unpin_generation)(binding.context(), lease) };
        }
    }

    fn query(
        binding: &NativeBinding,
        provider_session: u64,
    ) -> Result<MetadataCapabilitySnapshot, &'static str> {
        // SAFETY: these typed bindings remain live and calls occur on the owner thread.
        let api = unsafe { binding.table::<FrameMetadataCapabilitiesApiV1>() }
            .map_err(|_| "metadata_capability_interface")?;
        unsafe { metadata::enumerate_capabilities(api, provider_session) }
            .map_err(|_| "metadata_capability_unavailable")
    }

    pub fn policies(
        &self,
        video: u64,
        capture: u64,
        encoder: u64,
    ) -> Result<
        (
            MetadataPolicy,
            MetadataPolicy,
            MetadataPolicy,
            MetadataCapabilities,
        ),
        &'static str,
    > {
        let capture = Self::query(&self.capture, capture)?;
        let processor = Self::query(&self.processor, video)?;
        let encoder = Self::query(&self.encoder, encoder)?;
        let versions = MetadataCapabilities {
            capture: capture.generation,
            processor: processor.generation,
            encoder: encoder.generation,
        };
        let schema = self.schema();
        let policy = |snapshot: MetadataCapabilitySnapshot, stage| {
            let mut policy = snapshot
                .policy(CarriageOwner::FramerInsert, stage, false)
                .map_err(|_| "metadata_capability_invalid")?;
            // The Video pipeline owns the three provider-local sessions and is
            // the trusted composition authority mapping them to this video ID.
            policy.session_id = video;
            if !policy.capabilities.iter().any(|claim| {
                claim.schema == schema && claim.treatment == MetadataTreatment::Forward
            }) {
                return Err("metadata_schema_unsupported");
            }
            Ok(policy)
        };
        Ok((
            policy(capture, MetadataStage::Capture)?,
            policy(processor, MetadataStage::Processor)?,
            policy(encoder, MetadataStage::Encoder)?,
            versions,
        ))
    }
}
