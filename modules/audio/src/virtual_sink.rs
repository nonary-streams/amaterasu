use crate::model::{AudioResult, Endpoint};

pub const DEFAULT_VIRTUAL_SINK_ADAPTER: &str = "Steam Streaming Speakers";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelLayout {
    pub channels: u32,
    pub masks: &'static [u32],
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
}
