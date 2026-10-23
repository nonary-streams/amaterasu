use crate::model::Operation;

pub(super) fn normalize(mut operation: Operation) -> Operation {
    if let Operation::Open {
        virtual_sink: Some(_),
        capture_sink_without_routing,
        ..
    } = &mut operation
    {
        *capture_sink_without_routing = false;
    }

    operation
}

#[cfg(test)]
mod tests {
    use super::normalize;
    use crate::model::Operation;

    fn open(virtual_sink: Option<&str>, capture_sink_without_routing: bool) -> Operation {
        Operation::Open {
            endpoint: "render.configured".into(),
            virtual_sink: virtual_sink.map(str::to_owned),
            channels: Some(2),
            capture_format: None,
            host_audio_enabled: true,
            operation_id: Some("open-operation".into()),
            capture_sink_without_routing,
        }
    }

    #[test]
    fn virtual_sink_selector_disables_capture_only_and_preserves_open_fields() {
        let Operation::Open {
            endpoint,
            virtual_sink,
            channels,
            capture_format,
            host_audio_enabled,
            operation_id,
            capture_sink_without_routing,
        } = normalize(open(Some("Studio Virtual"), true))
        else {
            panic!("open operation must remain open");
        };

        assert_eq!(endpoint, "render.configured");
        assert_eq!(virtual_sink.as_deref(), Some("Studio Virtual"));
        assert_eq!(channels, Some(2));
        assert_eq!(capture_format, None);
        assert!(host_audio_enabled);
        assert_eq!(operation_id.as_deref(), Some("open-operation"));
        assert!(!capture_sink_without_routing);
    }

    #[test]
    fn empty_virtual_sink_selector_disables_capture_only() {
        let Operation::Open {
            virtual_sink,
            capture_sink_without_routing,
            ..
        } = normalize(open(Some(""), true))
        else {
            panic!("open operation must remain open");
        };

        assert_eq!(virtual_sink.as_deref(), Some(""));
        assert!(!capture_sink_without_routing);
    }

    #[test]
    fn open_without_virtual_sink_preserves_capture_only() {
        let Operation::Open {
            endpoint,
            virtual_sink,
            channels,
            capture_format,
            host_audio_enabled,
            operation_id,
            capture_sink_without_routing,
        } = normalize(open(None, true))
        else {
            panic!("open operation must remain open");
        };

        assert_eq!(endpoint, "render.configured");
        assert_eq!(virtual_sink, None);
        assert_eq!(channels, Some(2));
        assert_eq!(capture_format, None);
        assert!(host_audio_enabled);
        assert_eq!(operation_id.as_deref(), Some("open-operation"));
        assert!(capture_sink_without_routing);
    }

    #[test]
    fn non_open_operation_is_preserved() {
        let Operation::Lookup { operation_id } = normalize(Operation::Lookup {
            operation_id: "open-operation".into(),
        }) else {
            panic!("lookup operation must remain a lookup");
        };

        assert_eq!(operation_id, "open-operation");
    }
}
