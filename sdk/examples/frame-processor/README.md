# Independent frame processor

This deliberately small provider forwards retained BGRA8 SDR frames without
modifying pixels. It accepts only retainable CPU resources with the declared
color and synchronization contract; it is not an HDR converter. It uses no
lifecycle, settings-registry, display, or foreground provider.

From a workspace created by `scripts/create-processing-sdk.py`, build it with
`cargo build -p example-frame-processor`. The adjacent component template,
selection, and empty private settings are inputs to the review and signed-package
workflow in `docs/gpu-processing-sdk.md`.
