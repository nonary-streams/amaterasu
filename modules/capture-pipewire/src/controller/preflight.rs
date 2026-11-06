//! Pure source admission: no portal selection, source stream or display changes.
use super::{Controller, direct};
use crate::backend::Backend;
use amaterasu_module_sdk::capture::{
    SourceClass, SourceModeCapabilities, SourcePreflightRequest, SourcePreflightResponse,
};

impl<B: Backend> Controller<B> {
    pub fn preflight(
        &self,
        caller: &str,
        request: &SourcePreflightRequest,
    ) -> SourcePreflightResponse {
        if !self.settings.enabled {
            return SourcePreflightResponse::unavailable(request, "capture_disabled");
        }
        if !self.settings.capture_principals.contains(caller) {
            return SourcePreflightResponse::unavailable(request, "capture_permission_denied");
        }
        let managed = match request.source_class {
            SourceClass::ManagedOutput
                if self.settings.allow_named_outputs || self.settings.allow_desktop_outputs =>
            {
                true
            }
            SourceClass::DesktopOutput if self.settings.allow_desktop_outputs => false,
            SourceClass::ManagedOutput | SourceClass::DesktopOutput => {
                return SourcePreflightResponse::unavailable(request, "source_class_disabled");
            }
            SourceClass::Synthetic => {
                return SourcePreflightResponse::unsupported(request, "source_class_unsupported");
            }
        };
        // The compiled PipeWire adapter delivers BGRA8. It cannot attest a
        // high-depth/HDR source merely because the compositor exposes one.
        if request.mode.hdr || request.mode.bit_depth.is_some_and(|depth| depth != 8) {
            return SourcePreflightResponse::unsupported(request, "source_format_unsupported");
        }
        let output = match request.route.as_deref() {
            Some(route) => match route.strip_prefix("kwin:") {
                Some(name) if direct::valid_direct_name(name, !managed) => Some(name),
                _ => {
                    return SourcePreflightResponse::unsupported(
                        request,
                        "source_route_unsupported",
                    );
                }
            },
            None => None,
        };
        if let Err(reason) = self
            .backend
            .preflight_output(output, managed, &request.mode)
        {
            return SourcePreflightResponse::unavailable(request, reason);
        }
        SourcePreflightResponse::ready(
            request,
            vec![SourceModeCapabilities {
                source_class: request.source_class,
                min_width: 1,
                max_width: 8192,
                min_height: 1,
                max_height: 8192,
                min_refresh_millihz: 1000,
                max_refresh_millihz: 1_000_000,
                sdr: true,
                hdr: false,
                bit_depths: vec![8],
                format_open: false,
            }],
        )
    }
}
