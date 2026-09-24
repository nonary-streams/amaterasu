use crate::model::{AudioResult, Endpoint};

pub const DEFAULT_VIRTUAL_SINK_ADAPTER: &str = "Steam Streaming Speakers";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelLayout {
    pub channels: u32,
    pub masks: &'static [u32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualSampleFormat {
    Float32,
    Signed32,
    Signed24In32,
    Signed24,
    Signed16,
}

impl VirtualSampleFormat {
    pub const fn container_bits(self) -> u16 {
        match self {
            Self::Signed24 => 24,
            Self::Signed16 => 16,
            Self::Float32 | Self::Signed32 | Self::Signed24In32 => 32,
        }
    }

    pub const fn valid_bits(self) -> u16 {
        match self {
            Self::Signed24 => 24,
            Self::Signed16 => 16,
            Self::Float32 | Self::Signed32 => 32,
            Self::Signed24In32 => 24,
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(self, Self::Float32)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualFormatCandidate {
    pub sample_format: VirtualSampleFormat,
    pub channel_mask: u32,
}

const STEREO_MASKS: &[u32] = &[0x0003];
const SURROUND_51_MASKS: &[u32] = &[0x003f, 0x060f];
const SURROUND_71_MASKS: &[u32] = &[0x063f];

pub fn channel_layout(channels: u32) -> AudioResult<ChannelLayout> {
    let masks = match channels {
        2 => STEREO_MASKS,
        6 => SURROUND_51_MASKS,
        8 => SURROUND_71_MASKS,
        _ => return Err("unsupported_channel_layout"),
    };
    Ok(ChannelLayout { channels, masks })
}

/// Builds candidate order from Vibeshine's channel-specific preferred formats.
pub fn format_candidates(channels: u32) -> AudioResult<Vec<VirtualFormatCandidate>> {
    use VirtualSampleFormat::{Float32 as F32, Signed16 as S16, Signed24 as S24};
    use VirtualSampleFormat::{Signed24In32 as S24In32, Signed32 as S32};

    let (formats, layout) = match channels {
        2 => (&[S24In32, S24, S16, F32, S32][..], channel_layout(2)?),
        6 => (&[F32, S32, S24In32, S24, S16][..], channel_layout(6)?),
        8 => (&[F32, S32, S24In32, S24, S16][..], channel_layout(8)?),
        _ => return Err("unsupported_channel_layout"),
    };

    Ok(formats
        .iter()
        .flat_map(|sample_format| {
            layout
                .masks
                .iter()
                .map(|channel_mask| VirtualFormatCandidate {
                    sample_format: *sample_format,
                    channel_mask: *channel_mask,
                })
        })
        .collect())
}

pub fn preferred_format_candidates(
    channels: u32,
    preferred_valid_bits: u16,
) -> AudioResult<Vec<VirtualFormatCandidate>> {
    Ok(format_candidates(channels)?
        .into_iter()
        .filter(|candidate| candidate.sample_format.valid_bits() == preferred_valid_bits)
        .collect())
}

/// Matches the configured text against the endpoint fields in Vibeshine's
/// priority order. Empty selection means the built-in Steam adapter match.
pub fn select_endpoint<'a>(endpoints: &'a [Endpoint], selector: &str) -> Option<&'a Endpoint> {
    if selector.is_empty() {
        return endpoints.iter().find(|endpoint| {
            endpoint.adapter_name.as_deref() == Some(DEFAULT_VIRTUAL_SINK_ADAPTER)
        });
    }

    [
        MatchField::Id,
        MatchField::Name,
        MatchField::Description,
        MatchField::AdapterName,
    ]
    .into_iter()
    .find_map(|field| {
        endpoints.iter().find(|endpoint| match field {
            MatchField::Id => endpoint.id == selector,
            MatchField::Name => endpoint.name == selector,
            MatchField::Description => endpoint.description.as_deref() == Some(selector),
            MatchField::AdapterName => endpoint.adapter_name.as_deref() == Some(selector),
        })
    })
}

#[derive(Clone, Copy)]
enum MatchField {
    Id,
    Name,
    Description,
    AdapterName,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str, name: &str, description: &str, adapter: &str) -> Endpoint {
        Endpoint {
            id: id.into(),
            name: name.into(),
            description: Some(description.into()),
            adapter_name: Some(adapter.into()),
            default_multimedia: false,
        }
    }

    #[test]
    fn selector_matches_endpoint_fields_in_declared_priority_order() {
        let endpoints = [
            endpoint("first-id", "Shared", "Shared", "first adapter"),
            endpoint(
                "Shared",
                "second name",
                "second description",
                "second adapter",
            ),
        ];

        assert_eq!(select_endpoint(&endpoints, "Shared").unwrap().id, "Shared");
        assert_eq!(
            select_endpoint(&endpoints, "second description")
                .unwrap()
                .id,
            "Shared"
        );
        assert_eq!(
            select_endpoint(&endpoints, "second adapter").unwrap().id,
            "Shared"
        );

        let description_precedes_adapter = [
            endpoint("first-id", "first name", "Shared", "first adapter"),
            endpoint("second-id", "second name", "second description", "Shared"),
        ];
        assert_eq!(
            select_endpoint(&description_precedes_adapter, "Shared")
                .unwrap()
                .id,
            "first-id"
        );
    }

    #[test]
    fn empty_selector_matches_only_the_default_steam_virtual_adapter() {
        let endpoints = [
            endpoint("render.regular", "Speakers", "speakers", "Realtek Audio"),
            endpoint(
                "render.virtual",
                "Steam Streaming Speakers",
                "virtual speaker endpoint",
                DEFAULT_VIRTUAL_SINK_ADAPTER,
            ),
        ];

        assert_eq!(
            select_endpoint(&endpoints, "").unwrap().id,
            "render.virtual"
        );
        assert!(select_endpoint(&endpoints[..1], "").is_none());
    }

    #[test]
    fn unavailable_virtual_selector_never_falls_back_to_another_endpoint() {
        let endpoints = [endpoint(
            "render.default",
            "Default speakers",
            "built-in speakers",
            "Audio adapter",
        )];

        assert!(select_endpoint(&endpoints, "missing virtual sink").is_none());
    }

    #[test]
    fn channel_layout_uses_the_supported_51_mask_before_its_alternate() {
        assert_eq!(channel_layout(2).unwrap().masks, &[0x0003]);
        assert_eq!(channel_layout(6).unwrap().masks, &[0x003f, 0x060f]);
        assert_eq!(channel_layout(8).unwrap().masks, &[0x063f]);
        assert_eq!(channel_layout(4).unwrap_err(), "unsupported_channel_layout");
    }

    #[test]
    fn stereo_candidates_preserve_spatial_audio_preference_order_and_valid_depth() {
        let candidates = format_candidates(2).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.sample_format)
                .collect::<Vec<_>>(),
            [
                VirtualSampleFormat::Signed24In32,
                VirtualSampleFormat::Signed24,
                VirtualSampleFormat::Signed16,
                VirtualSampleFormat::Float32,
                VirtualSampleFormat::Signed32,
            ]
        );
        assert_eq!(
            preferred_format_candidates(2, 24)
                .unwrap()
                .iter()
                .map(|candidate| candidate.sample_format)
                .collect::<Vec<_>>(),
            [
                VirtualSampleFormat::Signed24In32,
                VirtualSampleFormat::Signed24
            ]
        );
        assert_eq!(
            preferred_format_candidates(2, 32)
                .unwrap()
                .iter()
                .map(|candidate| candidate.sample_format)
                .collect::<Vec<_>>(),
            [VirtualSampleFormat::Float32, VirtualSampleFormat::Signed32]
        );
    }

    #[test]
    fn surround_candidates_keep_each_sample_preference_and_mask_fallback_order() {
        let candidates = format_candidates(6).unwrap();
        assert_eq!(
            candidates,
            [
                (VirtualSampleFormat::Float32, 0x003f),
                (VirtualSampleFormat::Float32, 0x060f),
                (VirtualSampleFormat::Signed32, 0x003f),
                (VirtualSampleFormat::Signed32, 0x060f),
                (VirtualSampleFormat::Signed24In32, 0x003f),
                (VirtualSampleFormat::Signed24In32, 0x060f),
                (VirtualSampleFormat::Signed24, 0x003f),
                (VirtualSampleFormat::Signed24, 0x060f),
                (VirtualSampleFormat::Signed16, 0x003f),
                (VirtualSampleFormat::Signed16, 0x060f),
            ]
            .map(|(sample_format, channel_mask)| VirtualFormatCandidate {
                sample_format,
                channel_mask,
            })
        );
        assert_eq!(
            format_candidates(8).unwrap(),
            [
                VirtualSampleFormat::Float32,
                VirtualSampleFormat::Signed32,
                VirtualSampleFormat::Signed24In32,
                VirtualSampleFormat::Signed24,
                VirtualSampleFormat::Signed16,
            ]
            .map(|sample_format| VirtualFormatCandidate {
                sample_format,
                channel_mask: 0x063f,
            })
        );
        assert_eq!(preferred_format_candidates(6, 24).unwrap().len(), 4);
        assert!(preferred_format_candidates(6, 20).unwrap().is_empty());
    }
}
