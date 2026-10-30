//! Deterministic, bounded selection of a mutually understood frame delivery.
//!
//! Offers are ordered: consumer offers express preference first, and provider
//! offers break ties. Requirements are conjunctive and do not change ranking.
//! Schema-specific capability bytes must have a canonical, mutually agreed
//! encoding; this helper compares them exactly and never interprets them.

/// Limits apply before matching, including entries that would not be selected.
pub const MAX_DELIVERY_OFFERS: usize = 64;
pub const MAX_DELIVERY_REQUIREMENTS: usize = 64;
pub const MAX_DELIVERY_CAPABILITY_BYTES: usize = 256;

/// Numeric schema identifiers match the native frame resource descriptors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Schema {
    /// Zero denotes a standard schema; vendors use their nonzero UUID.
    pub namespace: [u8; 16],
    pub kind: u32,
    pub version: u32,
}

/// Resource accessibility, not physical residency or a zero-copy guarantee.
/// Zero means unspecified; flags can be combined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Accessibility(pub u32);

impl Accessibility {
    pub const UNSPECIFIED: Self = Self(0);
    pub const CPU: Self = Self(1);
    pub const GPU: Self = Self(2);
    pub const CPU_AND_GPU: Self = Self(3);

    fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    fn valid(self) -> bool {
        self.0 & !Self::CPU_AND_GPU.0 == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ownership {
    Borrowed,
    Retained,
}

/// A complete delivery contract. Exact matching prevents treating an unknown
/// resource or synchronization schema as a known one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveryFormat {
    pub resource: Schema,
    pub accessibility: Accessibility,
    pub ownership: Ownership,
    pub acquire_sync: Schema,
    pub completion_sync: Schema,
    pub capabilities: Vec<u8>,
}

/// A provider ID is unique and nonzero within its enumeration generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderOffer {
    pub id: u64,
    pub format: DeliveryFormat,
}

/// The provider ID must be paired with its enumeration generation. The consumer
/// index identifies the corresponding input offer; retain that offer's native
/// ID and enumeration generation for consumer configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeliverySelection {
    pub provider_id: u64,
    pub consumer_index: usize,
    /// Actual provider capability; a borrowed consumer may select Retained.
    pub ownership: Ownership,
    pub accessibility: Accessibility,
}

/// Every requirement must match the selected provider capability. Use ordered consumer offers to express
/// preferences among alternatives rather than treating preferences as mandates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryRequirement {
    Resource(Schema),
    Accessibility(Accessibility),
    Ownership(Ownership),
    AcquireSync(Schema),
    CompletionSync(Schema),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegotiationError {
    TooManyOffers,
    TooManyRequirements,
    InvalidOffer,
    InvalidRequirement,
    DuplicateProviderId,
}

/// Selects an intersection using exact capability bytes. Retainable provider
/// offers satisfy borrowed consumers, but borrowed offers cannot satisfy retained
/// consumers. Use the schema-aware hook where byte equality does not prove
/// compatibility. `Ok(None)` means no match; malformed or
/// excessive inputs are errors. The caller must preserve the enumeration
/// generation alongside the returned provider ID for native selection.
pub fn select_delivery(
    provider: &[ProviderOffer],
    consumer: &[DeliveryFormat],
    requirements: &[DeliveryRequirement],
) -> Result<Option<DeliverySelection>, NegotiationError> {
    select_delivery_with_compatibility(provider, consumer, requirements, |offered, wanted| {
        offered.capabilities == wanted.capabilities
    })
}

/// Like `select_delivery`, but delegates schema-specific capability matching to
/// the caller. The hook runs only after resource, accessibility, ownership, and
/// both synchronization schemas match, and ownership is compatible. Retained
/// offers also satisfy a borrowed consumer; the reverse is forbidden. It must reject unknown or
/// incompatible capabilities and may be called multiple times.
pub fn select_delivery_with_compatibility(
    provider: &[ProviderOffer],
    consumer: &[DeliveryFormat],
    requirements: &[DeliveryRequirement],
    compatible: impl Fn(&DeliveryFormat, &DeliveryFormat) -> bool,
) -> Result<Option<DeliverySelection>, NegotiationError> {
    if provider.len() > MAX_DELIVERY_OFFERS || consumer.len() > MAX_DELIVERY_OFFERS {
        return Err(NegotiationError::TooManyOffers);
    }
    if requirements.len() > MAX_DELIVERY_REQUIREMENTS {
        return Err(NegotiationError::TooManyRequirements);
    }
    for (index, offer) in provider.iter().enumerate() {
        if offer.id == 0 || !valid_format(&offer.format) {
            return Err(NegotiationError::InvalidOffer);
        }
        if provider[..index]
            .iter()
            .any(|earlier| earlier.id == offer.id)
        {
            return Err(NegotiationError::DuplicateProviderId);
        }
    }
    if consumer.iter().any(|format| !valid_format(format)) {
        return Err(NegotiationError::InvalidOffer);
    }
    if requirements
        .iter()
        .any(|requirement| !valid_requirement(*requirement))
    {
        return Err(NegotiationError::InvalidRequirement);
    }
    for (consumer_index, wanted) in consumer.iter().enumerate() {
        if let Some(offer) = provider.iter().find(|offer| {
            same_schemas(&offer.format, wanted)
                && requirements
                    .iter()
                    .all(|requirement| matches_requirement(&offer.format, *requirement))
                && compatible(&offer.format, wanted)
        }) {
            return Ok(Some(DeliverySelection {
                provider_id: offer.id,
                consumer_index,
                ownership: offer.format.ownership,
                accessibility: offer.format.accessibility,
            }));
        }
    }
    Ok(None)
}

fn same_schemas(offered: &DeliveryFormat, wanted: &DeliveryFormat) -> bool {
    offered.resource == wanted.resource
        && offered.accessibility.contains(wanted.accessibility)
        && (offered.ownership == wanted.ownership
            || (offered.ownership == Ownership::Retained
                && wanted.ownership == Ownership::Borrowed))
        && offered.acquire_sync == wanted.acquire_sync
        && offered.completion_sync == wanted.completion_sync
}

fn valid_schema(schema: Schema, allow_none: bool) -> bool {
    if schema.kind == 0 {
        allow_none && schema.version == 0 && schema.namespace == [0; 16]
    } else {
        schema.version != 0
            && ((schema.kind < 0x8000_0000 && schema.namespace == [0; 16])
                || (schema.kind >= 0x8000_0000 && schema.namespace != [0; 16]))
    }
}

fn valid_format(format: &DeliveryFormat) -> bool {
    valid_schema(format.resource, false)
        && valid_schema(format.acquire_sync, true)
        && valid_schema(format.completion_sync, true)
        && format.accessibility.valid()
        && format.capabilities.len() <= MAX_DELIVERY_CAPABILITY_BYTES
}

fn valid_requirement(requirement: DeliveryRequirement) -> bool {
    match requirement {
        DeliveryRequirement::Resource(schema) => valid_schema(schema, false),
        DeliveryRequirement::AcquireSync(schema) | DeliveryRequirement::CompletionSync(schema) => {
            valid_schema(schema, true)
        }
        DeliveryRequirement::Accessibility(accessibility) => accessibility.valid(),
        DeliveryRequirement::Ownership(_) => true,
    }
}

fn matches_requirement(format: &DeliveryFormat, requirement: DeliveryRequirement) -> bool {
    match requirement {
        DeliveryRequirement::Resource(value) => format.resource == value,
        DeliveryRequirement::Accessibility(value) => format.accessibility.contains(value),
        DeliveryRequirement::Ownership(value) => format.ownership == value,
        DeliveryRequirement::AcquireSync(value) => format.acquire_sync == value,
        DeliveryRequirement::CompletionSync(value) => format.completion_sync == value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(kind: u32, accessibility: Accessibility, ownership: Ownership) -> DeliveryFormat {
        DeliveryFormat {
            resource: Schema {
                namespace: [0; 16],
                kind,
                version: 1,
            },
            accessibility,
            ownership,
            acquire_sync: Schema {
                namespace: [0; 16],
                kind: 0,
                version: 0,
            },
            completion_sync: Schema {
                namespace: [0; 16],
                kind: 0,
                version: 0,
            },
            capabilities: vec![],
        }
    }

    fn selection(
        provider_id: u64,
        consumer_index: usize,
        ownership: Ownership,
        accessibility: Accessibility,
    ) -> DeliverySelection {
        DeliverySelection {
            provider_id,
            consumer_index,
            ownership,
            accessibility,
        }
    }

    #[test]
    fn consumer_preference_then_provider_order_and_requirements() {
        let cpu = format(1, Accessibility::CPU, Ownership::Borrowed);
        let gpu = format(2, Accessibility::GPU, Ownership::Retained);
        let offers = [
            ProviderOffer {
                id: 1,
                format: cpu.clone(),
            },
            ProviderOffer {
                id: 2,
                format: gpu.clone(),
            },
            ProviderOffer {
                id: 3,
                format: gpu.clone(),
            },
        ];
        assert_eq!(
            select_delivery(&offers, &[gpu.clone(), cpu.clone()], &[]),
            Ok(Some(selection(
                2,
                0,
                Ownership::Retained,
                Accessibility::GPU
            )))
        );
        assert_eq!(
            select_delivery(
                &offers,
                &[gpu, cpu],
                &[DeliveryRequirement::Accessibility(Accessibility::CPU)]
            ),
            Ok(Some(selection(
                1,
                1,
                Ownership::Borrowed,
                Accessibility::CPU
            )))
        );
    }

    #[test]
    fn requires_exact_version_sync_ownership_and_capabilities() {
        let base = format(1, Accessibility::GPU, Ownership::Retained);
        let offers = [ProviderOffer {
            id: 4,
            format: base.clone(),
        }];
        for changed in [
            DeliveryFormat {
                resource: Schema {
                    namespace: base.resource.namespace,
                    kind: base.resource.kind,
                    version: 2,
                },
                ..base.clone()
            },
            DeliveryFormat {
                acquire_sync: Schema {
                    namespace: [1; 16],
                    kind: 0x8000_0002,
                    version: 1,
                },
                ..base.clone()
            },
            DeliveryFormat {
                capabilities: vec![1],
                ..base.clone()
            },
        ] {
            assert_eq!(select_delivery(&offers, &[changed], &[]), Ok(None));
        }
        assert_eq!(
            select_delivery(
                &offers,
                &[base],
                &[DeliveryRequirement::Accessibility(Accessibility::CPU)]
            ),
            Ok(None)
        );
    }

    #[test]
    fn borrowed_only_cannot_satisfy_retained_and_retained_can_be_borrowed() {
        let borrowed = format(1, Accessibility::CPU, Ownership::Borrowed);
        let retained = format(1, Accessibility::CPU, Ownership::Retained);
        let borrow_offer = [ProviderOffer {
            id: 1,
            format: borrowed.clone(),
        }];
        let retain_offer = [ProviderOffer {
            id: 2,
            format: retained.clone(),
        }];
        assert_eq!(
            select_delivery(&borrow_offer, std::slice::from_ref(&retained), &[]),
            Ok(None)
        );
        assert_eq!(
            select_delivery(&retain_offer, std::slice::from_ref(&retained), &[]),
            Ok(Some(selection(
                2,
                0,
                Ownership::Retained,
                Accessibility::CPU
            )))
        );
        assert_eq!(
            select_delivery(
                &retain_offer,
                &[borrowed],
                &[DeliveryRequirement::Ownership(Ownership::Retained)]
            ),
            Ok(Some(selection(
                2,
                0,
                Ownership::Retained,
                Accessibility::CPU
            )))
        );
    }

    #[test]
    fn schema_specific_compatibility_cannot_override_schema_mismatch() {
        let offered = format(2, Accessibility::GPU, Ownership::Retained);
        let mut wanted = offered.clone();
        wanted.capabilities = vec![7];
        let offers = [ProviderOffer {
            id: 9,
            format: offered,
        }];
        assert_eq!(
            select_delivery_with_compatibility(&offers, &[wanted.clone()], &[], |_, _| true),
            Ok(Some(selection(
                9,
                0,
                Ownership::Retained,
                Accessibility::GPU
            )))
        );
        wanted.resource.version = 2;
        assert_eq!(
            select_delivery_with_compatibility(&offers, &[wanted], &[], |_, _| true),
            Ok(None)
        );
    }

    #[test]
    fn validates_standard_and_vendor_namespace_rules() {
        let base = format(1, Accessibility::CPU, Ownership::Borrowed);
        let mut vendor = base.clone();
        vendor.resource = Schema {
            namespace: [9; 16],
            kind: 0x8000_0000,
            version: 1,
        };
        let offer = [ProviderOffer {
            id: 1,
            format: vendor.clone(),
        }];
        assert_eq!(
            select_delivery(&offer, &[vendor], &[]),
            Ok(Some(selection(
                1,
                0,
                Ownership::Borrowed,
                Accessibility::CPU
            )))
        );
        let mut invalid = base.clone();
        invalid.resource.namespace = [9; 16];
        assert_eq!(
            select_delivery(&[], &[invalid], &[]),
            Err(NegotiationError::InvalidOffer)
        );
        let mut invalid = base;
        invalid.resource.kind = 0x8000_0000;
        assert_eq!(
            select_delivery(&[], &[invalid], &[]),
            Err(NegotiationError::InvalidOffer)
        );
    }

    #[test]
    fn custom_resource_accessibility_supports_combined_and_unspecified() {
        let mut dual = format(1, Accessibility::CPU_AND_GPU, Ownership::Borrowed);
        dual.resource = Schema {
            namespace: [3; 16],
            kind: 0x8000_0001,
            version: 1,
        };
        let mut wanted = dual.clone();
        wanted.accessibility = Accessibility::UNSPECIFIED;
        let offers = [ProviderOffer {
            id: 8,
            format: dual.clone(),
        }];
        assert_eq!(
            select_delivery(
                &offers,
                std::slice::from_ref(&wanted),
                &[DeliveryRequirement::Accessibility(Accessibility::GPU)]
            ),
            Ok(Some(selection(
                8,
                0,
                Ownership::Borrowed,
                Accessibility::CPU_AND_GPU
            )))
        );
        let mut unknown = dual;
        unknown.accessibility = Accessibility::UNSPECIFIED;
        let offers = [ProviderOffer {
            id: 9,
            format: unknown,
        }];
        assert_eq!(
            select_delivery(
                &offers,
                &[wanted],
                &[DeliveryRequirement::Accessibility(Accessibility::GPU)]
            ),
            Ok(None)
        );
        let mut invalid = format(1, Accessibility(4), Ownership::Borrowed);
        assert_eq!(
            select_delivery(&[], std::slice::from_ref(&invalid), &[]),
            Err(NegotiationError::InvalidOffer)
        );
        invalid.accessibility = Accessibility::GPU;
        assert_eq!(
            select_delivery(
                &[],
                &[invalid],
                &[DeliveryRequirement::Accessibility(Accessibility(4))]
            ),
            Err(NegotiationError::InvalidRequirement)
        );
    }

    #[test]
    fn validates_all_inputs_even_when_first_offer_matches() {
        let cpu = format(1, Accessibility::CPU, Ownership::Borrowed);
        let offer = ProviderOffer {
            id: 1,
            format: cpu.clone(),
        };
        assert_eq!(
            select_delivery(
                &[offer.clone(), offer.clone()],
                std::slice::from_ref(&cpu),
                &[]
            ),
            Err(NegotiationError::DuplicateProviderId)
        );
        let mut malformed = cpu.clone();
        malformed.completion_sync.version = 1;
        assert_eq!(
            select_delivery(std::slice::from_ref(&offer), &[cpu.clone(), malformed], &[]),
            Err(NegotiationError::InvalidOffer)
        );
        assert_eq!(
            select_delivery(&vec![offer; MAX_DELIVERY_OFFERS + 1], &[cpu], &[]),
            Err(NegotiationError::TooManyOffers)
        );
        assert_eq!(
            select_delivery(
                &[],
                &[],
                &vec![
                    DeliveryRequirement::Accessibility(Accessibility::CPU);
                    MAX_DELIVERY_REQUIREMENTS + 1
                ]
            ),
            Err(NegotiationError::TooManyRequirements)
        );
    }
}
