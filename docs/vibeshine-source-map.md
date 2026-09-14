# Initial Vibeshine source map

Inspected the sibling checkout `../vibeshine` on September 13, 2026. This is an
initial map, not a completed dependency audit. No source files have been copied.

| Area | Reference paths | Reimplementation implication |
| --- | --- | --- |
| Main host target | `CMakeLists.txt`, `cmake/targets/common.cmake` | The common build creates the `sunshine` executable from shared target files. Use this to understand responsibilities without inheriting its build graph. |
| Platform interface | `src/platform/common.h` | Includes host configuration, utility types, C++ standard types, and external declarations. It is a reference for behavior, not the proposed stable plugin ABI. |
| Windows capture | `src/platform/windows/display_wgc.cpp`, `src/platform/windows/display_base.cpp`, `src/platform/windows/display_ram.cpp`, `src/platform/windows/display_vram.cpp` | Audit capture lifecycle and GPU resource ownership before choosing the ABI. |
| WGC helper | `tools/sunshine_wgc_capture.cpp`, `cmake/targets/windows.cmake` | WGC already has a helper executable. Treat helper compatibility and delivery as part of the component. |
| NVIDIA encoding | `src/nvenc/` | Reference for encoder behavior; implement Rust integration with the NVIDIA SDK. |
| Display management | `src/platform/windows/display_helper_v2/`, `src/platform/windows/virtual_display_sunshine.cpp`, `tools/display_settings_helper_v2.cpp` | Display restoration and helpers need an explicit lifecycle; separating a DLL alone is insufficient. |
| Sessions/protocol | `src/stream.cpp`, `src/stream_protocol.cpp`, `src/rtsp.cpp`, `src/webrtc_stream.cpp` | Inventory required client protocols and session behavior before deciding the migration order. |
| Configuration HTTP | `src/confighttp.cpp`, `src/http_auth.cpp` | Define and version the web-facing API independently of the bundle. |
| Frontend | `src_assets/common/assets/web/package.json`, `src_assets/common/assets/web-legacy/package.json` | Two frontend package roots exist; choose the intended interface before rebuilding the frontend. |
| Current updates | `src/update.h`, `src/update.cpp` | The inspected header exposes release discovery and notifications. The proposed transactional component updater is new work. |
| Application model and lifecycle | `src/process.h` | Catalog identity includes UUIDs and legacy aliases; lifecycle includes preparation/undo commands, detached launchers, process groups, exit grace periods, provider tracking, per-app settings and deferred final-session display restoration. Direct child ownership alone is not lifecycle parity. |

Linux implementation details, driver protocols, licensing, and full build
dependencies still need a focused audit before implementing the corresponding Rust modules.
