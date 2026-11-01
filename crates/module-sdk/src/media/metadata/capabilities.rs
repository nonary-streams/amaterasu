//! Session-scoped provider capability query and trusted policy construction.
use super::*;

/// Provider claims are negotiated independently of the trusted owner choice.
pub const CAPABILITIES_CONTRACT: &str = "amaterasu.frame-metadata-capabilities.native/1";
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataCapabilityClaim {
    pub schema: FormatId,
    pub operations: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataCapabilitySnapshot {
    pub session_id: u64,
    pub generation: u64,
    pub claims: Vec<MetadataCapabilityClaim>,
}
struct CapabilityCollection {
    claims: Vec<MetadataCapabilityClaim>,
    invalid: bool,
}
unsafe extern "C" fn collect_capability(
    context: *mut std::ffi::c_void,
    capability: *const amaterasu_media::FrameMetadataCapabilityV1,
) -> amaterasu_abi::Status {
    if context.is_null() {
        return amaterasu_abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the collector lives through the synchronous provider callback.
    let collection = unsafe { &mut *context.cast::<CapabilityCollection>() };
    if capability.is_null() || collection.claims.len() >= FRAME_METADATA_MAX_ITEMS {
        collection.invalid = true;
        return amaterasu_abi::STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: the provider promises a live borrowed descriptor for this call.
    let capability = unsafe { &*capability };
    let schema = FormatId {
        namespace_id: capability.schema.namespace_id,
        value: capability.schema.value,
        schema: capability.schema.schema,
    };
    let known =
        amaterasu_media::FRAME_METADATA_CAP_FORWARD | amaterasu_media::FRAME_METADATA_CAP_APPLY;
    if !schema_valid(schema)
        || capability.reserved != 0
        || capability.operations == 0
        || capability.operations & !known != 0
        || collection.claims.iter().any(|c| c.schema == schema)
    {
        collection.invalid = true;
        return amaterasu_abi::STATUS_INVALID_ARGUMENT;
    }
    collection.claims.push(MetadataCapabilityClaim {
        schema,
        operations: capability.operations,
    });
    amaterasu_abi::STATUS_OK
}
/// # Safety
/// Runtime and trusted provider remain live for the binding and its calls.
pub unsafe fn bind_capabilities(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CAPABILITIES_CONTRACT,
            1,
            0,
            amaterasu_media::FRAME_METADATA_CAPABILITIES_API_V1_SIZE,
        )
    }
}

/// # Safety
/// Provider and runtime outlive the profile preflight binding.
pub unsafe fn bind_capabilities_with_probe(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CAPABILITIES_CONTRACT,
            1,
            1,
            std::mem::size_of::<amaterasu_media::FrameMetadataCapabilitiesApiV1_1>() as u32,
        )
    }
}

/// # Safety
/// Provider and runtime outlive all generation leases and their unpin barriers.
pub unsafe fn bind_capabilities_with_pinning(
    runtime: &crate::RuntimeClient,
    service: &str,
) -> crate::ModuleResult<crate::NativeBinding> {
    unsafe {
        runtime.bind_native_typed(
            service,
            CAPABILITIES_CONTRACT,
            1,
            2,
            std::mem::size_of::<amaterasu_media::FrameMetadataCapabilitiesApiV1_2>() as u32,
        )
    }
}
/// # Safety
/// The factory implements the exact capability table and native lifetime rules.
pub unsafe fn register_capabilities(
    runtime: &crate::RuntimeClient,
    service: &str,
    factory: &crate::abi::NativeServiceFactory,
) -> crate::ModuleResult<()> {
    unsafe { runtime.register_native_typed(service, CAPABILITIES_CONTRACT, factory) }
}
/// Query one bound provider's session-specific claims; duplicate, malformed or
/// over-limit descriptors fail even if a provider swallows the sink error.
///
/// # Safety
/// The table and context are live and the provider obeys synchronous callback
/// borrowing and owning-thread requirements throughout this call.
pub unsafe fn enumerate_capabilities(
    api: &amaterasu_media::FrameMetadataCapabilitiesApiV1,
    session: u64,
) -> Result<MetadataCapabilitySnapshot, MetadataError> {
    if session == 0
        || api.base.header.major != 1
        || api.base.header.size < amaterasu_media::FRAME_METADATA_CAPABILITIES_API_V1_SIZE
    {
        return Err(MetadataError::Invalid);
    }
    let mut collection = CapabilityCollection {
        claims: Vec::new(),
        invalid: false,
    };
    let sink = amaterasu_media::FrameMetadataCapabilitySinkV1 {
        context: (&mut collection as *mut CapabilityCollection).cast(),
        write: collect_capability,
    };
    let mut generation = 0;
    // SAFETY: caller guarantees binding and callback state lifetime.
    let status = unsafe { (api.enumerate)(api.base.context, session, sink, &mut generation) };
    if collection.invalid {
        return Err(MetadataError::Invalid);
    }
    if status != amaterasu_abi::STATUS_OK {
        return Err(MetadataError::Provider(status));
    }
    if generation == 0 {
        return Err(MetadataError::Invalid);
    }
    Ok(MetadataCapabilitySnapshot {
        session_id: session,
        generation,
        claims: collection.claims,
    })
}
impl MetadataCapabilitySnapshot {
    /// Combine provider claims with the owner and stage selected by trusted
    /// composition. A claim never selects the owner or proves implementation.
    pub fn policy(
        &self,
        owner: CarriageOwner,
        stage: MetadataStage,
        allow_drop_unknown_optional: bool,
    ) -> Result<MetadataPolicy, MetadataError> {
        if self.session_id == 0
            || self.generation == 0
            || self.claims.len() > FRAME_METADATA_MAX_ITEMS
        {
            return Err(MetadataError::Invalid);
        }
        let treatment = match (stage, owner) {
            (MetadataStage::Encoder, CarriageOwner::EncoderEmbed)
            | (MetadataStage::Framer, CarriageOwner::FramerInsert) => MetadataTreatment::Apply,
            _ => MetadataTreatment::Forward,
        };
        let required = match treatment {
            MetadataTreatment::Forward => amaterasu_media::FRAME_METADATA_CAP_FORWARD,
            MetadataTreatment::Apply => amaterasu_media::FRAME_METADATA_CAP_APPLY,
        };
        let mut capabilities = Vec::new();
        for claim in &self.claims {
            if !schema_valid(claim.schema)
                || claim.operations == 0
                || claim.operations
                    & !(amaterasu_media::FRAME_METADATA_CAP_FORWARD
                        | amaterasu_media::FRAME_METADATA_CAP_APPLY)
                    != 0
                || self
                    .claims
                    .iter()
                    .filter(|other| other.schema == claim.schema)
                    .count()
                    != 1
            {
                return Err(MetadataError::Invalid);
            }
            if claim.operations & required != 0 {
                capabilities.push(MetadataCapability {
                    schema: claim.schema,
                    treatment,
                });
            }
        }
        Ok(MetadataPolicy {
            session_id: self.session_id,
            capability_generation: self.generation,
            owner,
            stage,
            capabilities,
            allow_drop_unknown_optional,
        })
    }
}
