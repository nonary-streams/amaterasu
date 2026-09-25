# Configuration parity run coordination

- Run: `f220bcaa-20260924-152010`
- Clean source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b` on `main`
- Main branch is the direct landing and validation destination; inspect its current tip with `git rev-parse HEAD`.
- Main checkout: `C:\Users\Chase\sources\amaterasu`
- No integration branch/worktree is used. The coordinator cherry-picks accepted lane commits directly onto `main`.
- External session logs and mailboxes: `D:\sources\worktrees\amaterasu-config-runs\f220bcaa-20260924-152010`
- Status: see the current session snapshot at the end of this file. The latest process check found all six lane leads live; L03 and L04 were resumed after completing a slice or reaching a gate.

## Lane worktrees and accountable configuration IDs

Each coverage ID is assigned once. Application fields stay with applications; aliases follow their canonical setting; shared override scopes follow the global setting owner. Assignment does not change the current `pending` status.

| Lead | Branch | Worktree | IDs |
| --- | --- | --- | ---: |
| luna-01 | `config-parity/foundation` | `D:/sources/worktrees/amaterasu-config-foundation` | 7 |
| luna-02 | `config-parity/network` | `D:/sources/worktrees/amaterasu-config-network` | 18 |
| luna-03 | `config-parity/video-display` | `D:/sources/worktrees/amaterasu-config-video-display` | 99 |
| luna-04 | `config-parity/audio-input` | `D:/sources/worktrees/amaterasu-config-audio-input` | 23 |
| luna-05 | `config-parity/applications` | `D:/sources/worktrees/amaterasu-config-applications` | 77 |
| luna-06 | `config-parity/management` | `D:/sources/worktrees/amaterasu-config-management` | 18 |

### luna-01

Write ownership: `modules/configuration/`, `modules/host-configuration/`, `modules/authentication/`, `modules/state/`, `crates/runtime/`, `crates/host/`, `crates/package/`, `crates/storage/`, `crates/identity/`, `crates/module-sdk/`, `crates/contracts/`, `crates/abi/`, `sdk/`, `root manifests and lockfiles`, `CI`, `scripts`.

| Stable ID | Status |
| --- | --- |
| `global:cert` | pending |
| `global:credentials_file` | pending |
| `global:file_state` | pending |
| `global:pkey` | pending |
| `global:remember_me_refresh_token_ttl_seconds` | pending |
| `global:session_token_ttl_seconds` | pending |
| `global:vibeshine_file_state` | pending |

### luna-02

Write ownership: `modules/nvhttp/`, `modules/pairing/`, `modules/rtsp/`, `modules/stream-session/`, `modules/media-transport/`, `modules/control-transport/`, `modules/session/`, `crates/gamestream/`.

| Stable ID | Status |
| --- | --- |
| `global:address_family` | pending |
| `global:bind_address` | pending |
| `global:csrf_allowed_origins` | pending |
| `global:external_ip` | pending |
| `global:fec_percentage` | pending |
| `global:lan_encryption_mode` | pending |
| `global:max_bitrate` | pending |
| `global:minimum_fps_target` | pending |
| `global:origin_web_ui_allowed` | pending |
| `global:ping_timeout` | pending |
| `global:port` | pending |
| `global:remote_monitor_confirm_app_replacement` | pending |
| `global:remote_monitor_disconnect_on_client_disconnect` | pending |
| `global:remote_monitor_disconnect_on_stream_end` | pending |
| `global:remote_monitor_terminate_on_first_request` | pending |
| `global:upnp` | pending |
| `global:video_max_batch_size_kb` | pending |
| `global:wan_encryption_mode` | pending |

### luna-03

Write ownership: `modules/video/`, `modules/encode/`, `modules/capture-dxgi/`, `modules/capture-wgc/`, `modules/display/`, `modules/virtual-display/`, `modules/frame-limiter/`, `crates/media/`, `crates/windows-capture-geometry/`.

| Stable ID | Status |
| --- | --- |
| `alias:global:dd_vdd_static_monitor_count` | pending |
| `alias:global:nvenc_force_split_encode` | pending |
| `alias:global:rtss_disable_vsync_ullm` | pending |
| `global:adapter_name` | pending |
| `global:adapter_pnp_id` | pending |
| `global:amd_av1_latency_mode` | pending |
| `global:amd_av1_screen_content` | pending |
| `global:amd_coder` | pending |
| `global:amd_enforce_hrd` | pending |
| `global:amd_high_motion_quality_boost` | pending |
| `global:amd_input_queue_size` | pending |
| `global:amd_lowlatency_mode` | pending |
| `global:amd_ltr_frames` | pending |
| `global:amd_preanalysis` | pending |
| `global:amd_quality` | pending |
| `global:amd_qvbr_quality_level` | pending |
| `global:amd_rc` | pending |
| `global:amd_smart_access_video` | pending |
| `global:amd_usage` | pending |
| `global:amd_vbaq` | pending |
| `global:av1_mode` | pending |
| `global:capture` | pending |
| `global:dd_activate_virtual_display` | pending |
| `global:dd_always_restore_from_golden` | pending |
| `global:dd_config_revert_delay` | pending |
| `global:dd_config_revert_on_disconnect` | pending |
| `global:dd_configuration_option` | pending |
| `global:dd_display_helper_engine` | pending |
| `global:dd_hdr_option` | pending |
| `global:dd_hdr_request_override` | pending |
| `global:dd_manual_refresh_rate` | pending |
| `global:dd_manual_resolution` | pending |
| `global:dd_mode_remapping` | pending |
| `global:dd_paused_virtual_display_timeout_secs` | pending |
| `global:dd_refresh_rate_option` | pending |
| `global:dd_resolution_option` | pending |
| `global:dd_snapshot_exclude_devices` | pending |
| `global:dd_snapshot_restore_hotkey` | pending |
| `global:dd_snapshot_restore_hotkey_modifiers` | pending |
| `global:dd_use_sunshine_virtual_display_driver` | pending |
| `global:dd_vdd_static_monitor_count` | pending |
| `global:dd_virtual_display_permanent_count` | pending |
| `global:dd_virtual_display_scale` | pending |
| `global:dd_wa_dummy_plug_hdr10` | pending |
| `global:dd_wa_hdr_toggle` | pending |
| `global:dd_wa_hdr_toggle_delay` | pending |
| `global:encoder` | pending |
| `global:flags` | pending |
| `global:frame_limiter_auto_virtual_framegen` | pending |
| `global:frame_limiter_disable_vsync` | pending |
| `global:frame_limiter_enable` | pending |
| `global:frame_limiter_fps_limit` | pending |
| `global:frame_limiter_provider` | pending |
| `global:hevc_mode` | pending |
| `global:lossless_scaling_legacy_auto_detect` | pending |
| `global:lossless_scaling_path` | pending |
| `global:mangohud_always_show_graph` | pending |
| `global:mangohud_limiter_method` | pending |
| `global:mangohud_preset` | pending |
| `global:min_threads` | pending |
| `global:nvenc_h264_cavlc` | pending |
| `global:nvenc_latency_over_power` | pending |
| `global:nvenc_opengl_vulkan_on_dxgi` | pending |
| `global:nvenc_preset` | pending |
| `global:nvenc_realtime_hags` | pending |
| `global:nvenc_spatial_aq` | pending |
| `global:nvenc_split_encode` | pending |
| `global:nvenc_twopass` | pending |
| `global:nvenc_vbv_increase` | pending |
| `global:output_name` | pending |
| `global:qp` | pending |
| `global:qsv_coder` | pending |
| `global:qsv_preset` | pending |
| `global:qsv_slow_hevc` | pending |
| `global:rtss_allow_virtual_display_override` | pending |
| `global:rtss_disable_vsync_ullm` | pending |
| `global:rtss_frame_limit_type` | pending |
| `global:rtss_install_path` | pending |
| `global:rtx_hdr` | pending |
| `global:rtx_hdr_contrast` | pending |
| `global:rtx_hdr_force_sdr` | pending |
| `global:rtx_hdr_middle_gray` | pending |
| `global:rtx_hdr_peak_brightness` | pending |
| `global:rtx_hdr_saturation` | pending |
| `global:rtx_hdr_sdr_brightness` | pending |
| `global:sw_preset` | pending |
| `global:sw_tune` | pending |
| `global:vaapi_strict_rc_buffer` | pending |
| `global:virtual_display_layout` | pending |
| `global:virtual_display_mode` | pending |
| `global:virtual_display_outputs` | pending |
| `global:vk_rc_mode` | pending |
| `global:vk_tune` | pending |
| `global:vt_coder` | pending |
| `global:vt_realtime` | pending |
| `global:vt_software` | pending |
| `global:vulkan_hdr_layer` | pending |
| `global:wayland_hdr_compatibility` | pending |
| `global:wgc_pacing_smoothing` | pending |

### luna-04

Write ownership: `modules/audio/`, `modules/audio-encode/`, `modules/input/`.

| Stable ID | Status |
| --- | --- |
| `global:always_send_scancodes` | pending |
| `global:audio_sink` | pending |
| `global:audio_sink_capture_only` | pending |
| `global:back_button_timeout` | pending |
| `global:controller` | pending |
| `global:ds4_back_as_touchpad_click` | pending |
| `global:ds5_inputtino_randomize_mac` | pending |
| `global:gamepad` | pending |
| `global:high_resolution_scrolling` | pending |
| `global:install_steam_audio_drivers` | pending |
| `global:key_repeat_delay` | pending |
| `global:key_repeat_frequency` | pending |
| `global:key_rightalt_to_key_win` | pending |
| `global:keybindings` | pending |
| `global:keyboard` | pending |
| `global:motion_as_ds4` | pending |
| `global:mouse` | pending |
| `global:native_pen_touch` | pending |
| `global:proton_dualsense_compatibility` | pending |
| `global:remote_monitor_mute_audio` | pending |
| `global:stream_audio` | pending |
| `global:touchpad_as_ds4` | pending |
| `global:virtual_sink` | pending |

### luna-05

Write ownership: `modules/catalog/`, `modules/application/`, `modules/launch/`, `modules/steam/`, `modules/playnite/`, `modules/lutris/`.

| Stable ID | Status |
| --- | --- |
| `alias:per_application:dlss-framegen-capture-fix` | pending |
| `global:file_apps` | pending |
| `global:global_prep_cmd` | pending |
| `global:lutris_auto_sync` | pending |
| `global:lutris_autosync_remove_uninstalled` | pending |
| `global:lutris_enabled` | pending |
| `global:lutris_exclude_games` | pending |
| `global:lutris_include_steam` | pending |
| `global:playnite_auto_sync` | pending |
| `global:playnite_autosync_delete_after_days` | pending |
| `global:playnite_autosync_remove_uninstalled` | pending |
| `global:playnite_autosync_require_replacement` | pending |
| `global:playnite_enabled` | pending |
| `global:playnite_exclude_categories` | pending |
| `global:playnite_exclude_games` | pending |
| `global:playnite_exclude_plugins` | pending |
| `global:playnite_extensions_dir` | pending |
| `global:playnite_focus_attempts` | pending |
| `global:playnite_focus_exit_on_first` | pending |
| `global:playnite_focus_timeout_secs` | pending |
| `global:playnite_fullscreen_entry_enabled` | pending |
| `global:playnite_install_dir` | pending |
| `global:playnite_recent_games` | pending |
| `global:playnite_recent_max_age_days` | pending |
| `global:playnite_sync_all_installed` | pending |
| `global:playnite_sync_categories` | pending |
| `global:playnite_sync_plugins` | pending |
| `global:steam_auto_sync` | pending |
| `global:steam_autosync_remove_uninstalled` | pending |
| `global:steam_enabled` | pending |
| `global:steam_exclude_games` | pending |
| `global:steam_include_tools` | pending |
| `global:steam_recent_games` | pending |
| `global:steam_recent_max_age_days` | pending |
| `global:steam_sync_all_installed` | pending |
| `global:sunshine_name` | pending |
| `per_application:auto-detach` | pending |
| `per_application:cmd` | pending |
| `per_application:config-overrides` | pending |
| `per_application:dd-configuration-option` | pending |
| `per_application:detached` | pending |
| `per_application:display-output` | pending |
| `per_application:dlss-framegen-capture-fix` | pending |
| `per_application:elevated` | pending |
| `per_application:exclude-global-prep-cmd` | pending |
| `per_application:exit-timeout` | pending |
| `per_application:frame-gen-limiter-fix` | pending |
| `per_application:frame-generation-mode` | pending |
| `per_application:frame-generation-provider` | pending |
| `per_application:gen1-framegen-fix` | pending |
| `per_application:gen2-framegen-fix` | pending |
| `per_application:image-path` | pending |
| `per_application:lossless-scaling-custom` | pending |
| `per_application:lossless-scaling-enabled` | pending |
| `per_application:lossless-scaling-framegen` | pending |
| `per_application:lossless-scaling-launch-delay` | pending |
| `per_application:lossless-scaling-legacy-auto-detect` | pending |
| `per_application:lossless-scaling-profile` | pending |
| `per_application:lossless-scaling-recommended` | pending |
| `per_application:lossless-scaling-rtss-limit` | pending |
| `per_application:lossless-scaling-target-fps` | pending |
| `per_application:lutris-directory` | pending |
| `per_application:lutris-id` | pending |
| `per_application:name` | pending |
| `per_application:output` | pending |
| `per_application:playnite-fullscreen` | pending |
| `per_application:playnite-id` | pending |
| `per_application:prefer-10bit-sdr` | pending |
| `per_application:prep-cmd` | pending |
| `per_application:steam-id` | pending |
| `per_application:steam-install-dir` | pending |
| `per_application:uuid` | pending |
| `per_application:virtual-display-layout` | pending |
| `per_application:virtual-display-mode` | pending |
| `per_application:virtual-screen` | pending |
| `per_application:wait-all` | pending |
| `per_application:working-dir` | pending |

### luna-06

Write ownership: `modules/management/`, `frontend/`, `modules/diagnostics/`, `modules/session-history/`, `modules/updater/`, `modules/system-tray/`, `modules/host-metrics/`.

| Stable ID | Status |
| --- | --- |
| `global:locale` | pending |
| `global:log_path` | pending |
| `global:min_log_level` | pending |
| `global:notify_pre_releases` | pending |
| `global:realtime_stats_enabled` | pending |
| `global:realtime_stats_history_retention_seconds` | pending |
| `global:realtime_stats_max_history_points` | pending |
| `global:realtime_stats_pause_when_hidden` | pending |
| `global:realtime_stats_poll_interval_ms` | pending |
| `global:realtime_stats_show_active_sessions` | pending |
| `global:realtime_stats_show_host_charts` | pending |
| `global:realtime_stats_show_host_stats` | pending |
| `global:realtime_stats_show_session_history` | pending |
| `global:session_history_db_size_limit_mb` | pending |
| `global:session_history_enabled` | pending |
| `global:session_history_ttl_days` | pending |
| `global:system_tray` | pending |
| `global:update_check_interval` | pending |

## Dependencies and version reservations

Coordinator decisions and dependency acknowledgments are tracked here and in the run inbox/outbox.

| Request | Ownership / contract | Status |
| --- | --- | --- |
| `luna-02-stream-overrides-01` | `luna-02` owns global stream-value resolution and NVHTTP persistence; `luna-05` owns catalog/application launch persistence; `luna-06` owns signed management routes and editor. Preserve absent/null versus explicit zero; paired-certificate client values take precedence over app, global, then reference defaults. | L02 runtime slice committed; L05 catalog and L06 editor work is active. End-to-end persistence and stream tests remain pending. |
| `luna-03-frame-limiter-path-001` | `luna-03` owns `modules/frame-limiter/**`; `luna-02` is the only writer for NVHTTP persistence and launch projection; L05 owns app lifecycle apply/restore; L01 owns release profile registration. | L02 found that the cited L03 SHA changes only its handoff document, not runtime files. The strict Apply/Restore/Status schema is unpinned. L02 requires pre-spawn app overrides from L05; frame-limiter profile registration waits for L03's component/artifact identity and owner acknowledgments. |
| Product-settings migration | Reserve NVHTTP v17 once for the combined v16-to-v17 batch, with `luna-02` as sole writer. It includes L03's global frame-limiter settings, L05's raw `legacy_global_preparation`, LAN/WAN encryption modes, and capture/encoder settings once their canonical values are confirmed. Preserve structured `global_preparation`; do not create a second v17 migration. | Coordinator reconfirmed one v17 batch and rejected an additional v18 request. L02 is correcting FPS persistence to canonical decimal-Hz and resolving the pre-spawn dependency; capture/encoder fields remain pending L03's contract. |
| LAN/WAN encryption | `luna-02` owns global mode persistence and launch policy; `luna-06` owns signed management and editor controls. Modes are 0=never, 1=opportunistic, 2=mandatory; defaults are LAN 0 and WAN 1. Select by normalized peer address. Match pinned behavior: never disables video encryption, opportunistic keeps negotiated media choices, mandatory requires both video and audio encryption. | Policy classification is a partial L02 candidate; persistence and RTSP/media enforcement are not implemented. Settings stay in the existing v17 batch; L06 editor acknowledgment is pending, and no v18 is allocated. |
| Host settings provider | `luna-01` owns `modules/host-configuration/**`, `crates/release/src/profiles.rs`, `crates/release/src/profiles/tests.rs`, `amaterasu.host-configuration/1`, and the revisioned v1 document for host paths/identity plus `log_path` and `min_log_level`. `file_state` and `vibeshine_file_state` select contained legacy import files and do not replace live Amaterasu state storage. `luna-02` consumes read-only identity/import values; `luna-06` owns the signed editor and diagnostics behavior. | Provider is on main; provider tests passed 15/15, host tests passed 19/19, and strict Clippy passed. Canonical state roots now fail closed if native paths cannot be represented exactly in Unicode. L01 owns the profile paths; diagnostics gets read-only access, updates remain management-only, and worker READY does not wait for logging hydration. Unix-specific tests and Windows path qualification remain pending. |
| Session TTL and browser session parity | `luna-01` owns auth persistence and configured TTL behavior; `luna-06` owns management cookies and frontend behavior. Keep management loopback-only and retain Secure, HttpOnly, SameSite and Origin protections. Access cookie uses `Path=/v1/`; refresh cookie uses `Path=/v1/auth`. | Authentication TTL/session code and fixture/Clippy corrections are on main; 37 authentication tests and strict Clippy pass. Luna-06 cookie/editor integration remains active; restart and browser qualification remain pending. |
| DS4 Back mapping and gamepad profile | `luna-04` owns input policy, global `gamepad` and `ds4_back_as_touchpad_click`, and input settings v9. L02 owns paired-client persistence and stream launch projection; L05 owns sparse app overrides; L06 owns signed management/editor changes. Explicit false is preserved; only selected DS4 profiles change. | Initial settings and lease snapshot are on main. The coordinator input suite passes 113/113; Clippy completes with the existing `windows_gamepad.rs::start_with` dead-code warning. Management/app/client projection and ViGEm device qualification remain; both IDs stay pending. |
| Persisted log settings | L01 owns host-configuration v1 and release profile grants; L06 diagnostics reads the persisted values asynchronously. Diagnostics starts with validated signed/bootstrap settings, retains that fallback when the provider is unavailable or revision is zero, and adopts a ready persisted revision without delaying worker READY. | L06 has a reviewed diagnostics code candidate; the read-only profile grant is assigned to L01. Tests/Clippy, profile integration, and platform qualification remain pending. |
| Shared global preference documents | Host paths/identity/log values remain in `amaterasu.host-configuration/1`. Tray, updater, and host-metrics each own a revisioned global v1 document under their module service; UI-only `global:locale` stays in the management configuration namespace. All updates use state CAS and signed management routes; no management /v1 bump. | Coordinator decision relayed to L06; module contracts and provider registration remain pending. |
| Pairing identity comment parsing | L01 owns package migration; L02 owns `modules/pairing/src/migration.rs` and will strip inline `#` comments from pkey/cert path lines while retaining containment checks and original archived bytes. | Dependency identified by L01; bounded L02 correction requested. |
| Capture and encoder selection | L03 owns runtime selection; L02 owns global/client persistence and immutable stream snapshot; L05 owns app storage/lifecycle; L06 owns editor. Preserve empty/auto, `ddx`, `wgc`, and `wgcc` capture values and use stable encoder identity rather than enumeration order. | The capture-mode video adapter is on main and its package tests pass. Product field names/defaults, persistence, availability fallback, encoder identity, and launch projection remain pending; any new persistence uses the existing v17 batch. |
| Playnite path controls | Pinned reference removed both install/path overrides and the current provider discovers its executable without consuming them. Do not invent semantics for `global:playnite_install_dir` or `global:playnite_extensions_dir`. | Classified as removed/inert reference controls; keep stable IDs pending until the coverage inventory supports a retired status. |
| Updater, tray and host metrics | `luna-06` owns `modules/updater/**`, `modules/system-tray/**`, and `modules/host-metrics/**`; `luna-01` owns host startup integration in `crates/host/**`; `luna-02` owns stream-session metric emission. | Path ownership assigned; module contracts and lifecycle integration pending. |

## Mainline cherry-pick queue

| Lane | Source commit(s) | Main commit(s) | Status |
| --- | --- | --- | --- |
| luna-04 DS4 input and lease correction | `50163b7d`, `4af44696`, `431acd70`, `2f8775d2`, `87caa50f` | `627dc591`, `e306b332`, `3d1fa5ac`, `b49fb916`, `ec27b09f` | Cherry-picked directly to main; the handoff conflict was resolved by preserving the coordinator next-slice text and adding the lane recovery record. `cargo test -p amaterasu-input` passed 113/113; Clippy passed with the existing dead-code warning. ViGEm hardware qualification and management/app/stream integration remain pending. |
| luna-01 configured state migration | `9e550087` | `7d5e3379` | Cherry-picked cleanly. `amaterasu-package` passed 44 tests and Clippy at this point. The alias follow-up remains under correction. |
| luna-01 configured path aliases and Sigma regression correction | `3902a21d`, `c8bed167`, `78fae895`, `d6eee5c0` | `d9e7bc22`, `9b0b4e10`, `f816f6e9`, `e7ff0f1` | The resolver correction and test-only correction were cherry-picked directly to main. Windows ordinal and filesystem probes confirmed `Σ.PEM` and `ς.pem` do not match, so the test now checks fail-closed rejection while retaining ASCII and `É/é` acceptance cases. `cargo test -p amaterasu-package` passed 44 library tests and 3 CLI tests; strict Clippy passed. |
| luna-01 authentication fixture and Clippy corrections | `f0a2f23c`, `f3b78b7b` | `ce6a4330`, `65c1110c` | The persistence test now advances through async state loads before checking writes, and the legacy password fixture supplies its persisted revision. The session assertions use Clippy's preferred option helper. `cargo test -p amaterasu-authentication` passed 37/37; formatting and strict Clippy passed. Browser cookie integration and restart qualification remain pending. |
| luna-01 host-provider Clippy correction | `1c1d99cc` | `45aa8882` | Removed the needless return, initialized path fixtures with struct update syntax, and replaced the explicit test counter with a range. Provider tests passed 15/15, host tests passed 17/17, formatting passed, and strict Clippy passed. Platform symlink/reparse-point qualification and host/editor lifecycle integration remain pending. |
| luna-01 host state-root Unicode validation | `e8e8ae70` | `aaf2849e` | Host loading and logging settings now reject lossy native-path conversion before JSON injection. Formatting passed, `cargo test -p amaterasu` passed 19/19 on Windows, and strict Clippy passed. Unix-specific path-byte tests could not run because WSL is not installed; Windows reparse-point and Unix symlink qualification remain pending. |
| luna-03 capture-mode request adapter | `484c779c` | `466bbbb5` | Cherry-picked cleanly. `cargo test -p amaterasu-video` passed 68/68 and strict Clippy passed. Capture persistence, management, launch projection, provider fallback, and Windows qualification remain pending. |
| luna-03 virtual-display settings and permanent count boundary | `87b8e414`, `46bf52e`, `8cd6e30a`, `c9d8379d` | `ba68624f`, `330116ba`, `c06b774c`, `996cbd9` | Cherry-picked directly to main. The handoff retained all four lane slices. `cargo test -p amaterasu-virtual-display` passed 18/18 and strict Clippy passed. Permanent monitor creation, persistence, scoped projection, management editing, and Windows qualification remain pending. |
| luna-03 WGC timestamp pacing | `fd7e0ae8`, `02e4c438`, `690cf089`, `5790c4ab` | `2f97a8ab`, `a33a7ac6`, `8ab68124`, `d3f5fd95` | Independent XHigh review found no findings. Formatting passed; media (22), capture-WGC (19, 1 ignored), and video (73) tests passed; strict Clippy passed. Separate WGC acquisition time now survives controller timestamp rewriting through the additive v1.3 ABI; Windows WGC qualification and settings persistence/launch projection remain pending. |
| luna-05 Lutris paired exclusion | `abb31538`, `e9491cb0` | `a62585a0`, `05b3db3b` | The removal test now checks exact membership without depending on ordering between base and inventory removals. `cargo test -p amaterasu-lutris` passed 23/23, formatting passed, and strict Clippy passed. |
| luna-05 Playnite mixed-source selection cleanup | `1e310501` | `c2461d9c` | Cleanup now removes only rows whose source flags equal installed-only; rows also marked recent remain. `cargo test -p amaterasu-playnite` passed 64/64, formatting passed, and strict Clippy passed. Windows Playnite runtime qualification remains pending. |
| luna-04 Windows audio capture recovery | `0c9e1fb9`, `f23da668`, `ecf2a15c` | `666c4d7f`, `b62a6b41`, `c531b811` | Recovery and the Clippy correction are on main. Windows-target audio tests passed (42 passed, 1 ignored live-endpoint test), formatting passed, and strict Clippy passed. Windows hardware qualification remains pending. |
| luna-04 direct virtual-sink capture-policy normalization | `1d8bb712`, `83ace035` | `3914de70`, `f5425f9a` | Direct audio API opens clear capture-only routing when a virtual-sink selector is present. Formatting passed, Windows-target tests passed (46 passed, 1 ignored live-endpoint test), and strict Clippy passed after the one-file XHigh correction. Both audio setting IDs remain pending for persistence, caller-scoped resolution, management, and device qualification. |
| luna-02 pairing identity inline-comment handling | `a9c75702` | pending | Lane committed the parser correction and focused tests; coordinator package validation is pending. |
| luna-03 manual-resolution parser | `afe11022` | `d66ac2a9` | Cherry-picked directly to main. `cargo fmt --check -p amaterasu-display`, `git diff --check`, 40 display tests, and strict Clippy passed. The parser slice does not add persistence, scoped resolution, UI readback, or runtime effects; `global:dd_manual_resolution` remains pending. |
| luna-01 cross-config credential migration tests | `81ff881e` | `d82277d7`, `dcebf41` | Direct cherry-pick followed by a coordinator fixture/Clippy correction. Formatting, all 52 authentication tests, and strict all-target Clippy passed. Production behavior is unchanged; the Unix symlink case and the remaining credential migration/management/runtime evidence are pending. |
| luna-05 per-app HDR request storage | `c0ff9151` | `200a4b62`, `a141941` | Direct cherry-pick followed by coordinator Cargo-format cleanup of stale assertion reflow. The net change is limited to the three reserved Catalog paths; formatting, all 80 Catalog tests, and strict all-target Clippy passed. L02 projection, L06 readback, runtime effects, and platform evidence remain pending. |

Cherry-pick each new lane candidate directly onto `main`, resolve clear conflicts in the main checkout, and record the resulting main commit here. Lightweight lane worktrees remain source-only; package tests and Clippy run from this coordinator checkout.

## Shared-resource queue

No host-global audio, virtual-display, input-device, port, or installed-service tests scheduled yet.

## Initial Luna Max CLI launch snapshot (2026-09-24)

These rows record the original launch state. See the current process snapshot below for live status.

All six were launched with `gpt-6-luna` at `max`, multi-agent delegation enabled, and three child slots defaulting to `gpt-6-luna` at `xhigh`. Outbox messages verify the model/worker configuration per lane; messages that are still pending are marked `launch-configured` until that lane reports its startup check. Runner PIDs identify the supervising PowerShell process; CLI session IDs are stable resume keys.

| Lead | Session ID | Runner PID | Startup report |
| --- | --- | ---: | --- |
| luna-01 | `01a0d516-858e-7c40-b459-5de0bbf3d0dd` | resumed | Resumed after candidate submission; XHigh correction worker verified; its isolated-worktree startup is being retried |
| luna-02 | `01a0d516-8616-7270-a509-547be018f212` | 42520 | one XHigh worker confirmed; second check pending |
| luna-03 | `01a0d516-85d0-7261-b883-4dddf4473f99` | 21396 | Capture adapter implementation/review used XHigh workers; root is active on additional video settings |
| luna-04 | `01a0d516-8713-7b83-87f1-d35c0828ae10` | resumed | DS4 lease worker verified XHigh and landed; resumed on audio capture recovery |
| luna-05 | `01a0d516-8830-7960-af1e-079a13415f97` | 23228 | Steam and launch workers scoped; settings report pending |
| luna-06 | `01a0d516-8960-7e02-ac81-3eee350f679a` | 43508 | two XHigh workers confirmed (diagnostics, session history) |

## Initial XHigh worker allocation (2026-09-24)

| Lead | Worker | Worker session | Settings | Scope | Status |
| --- | --- | --- | --- | --- | --- |
| luna-02 | `network_policy_scan` | not yet reported | `gpt-6-luna` / `xhigh` confirmed | Network-owned paths | active |
| luna-02 | `stream_policy_scan` | not yet reported | `gpt-6-luna` / `xhigh` pending confirmation | Stream-owned paths | startup confirmation pending |
| luna-05 | `steam_slice` | not yet reported | `gpt-6-luna` / `xhigh` requested | `modules/steam/` | startup confirmation pending |
| luna-05 | `launchers_slice` | not yet reported | `gpt-6-luna` / `xhigh` requested | `modules/playnite/`, `modules/lutris/` | startup confirmation pending |
| luna-06 | `diagnostics` | `01a0d51a-45be-7bd0-b4d8-08923ae89823` | `gpt-6-luna` / `xhigh` confirmed before edits | `modules/diagnostics/` | active |
| luna-06 | `session_history` | `01a0d51a-69e9-7532-873f-55552f306fae` | `gpt-6-luna` / `xhigh` confirmed before edits | `modules/session-history/` | active |

## Current session snapshot (2026-09-25)

This reflects the latest process and worktree check. Lead session IDs remain the stable resume keys.

| Lead | Lead process | Latest work evidence |
| --- | --- | --- |
| luna-01 | PID 47792, live | Cross-config credentials tests landed on main as `d82277d7`; coordinator fixture and Clippy correction `dcebf41` passed 52 authentication tests and strict Clippy. The lead is resumed for the next independent slice. |
| luna-02 | PID 52592, live | Network lane has a 13-file draft across NVHTTP, RTSP, and stream-session. |
| luna-03 | PID 4472, live | Manual-resolution parser landed on main as `d66ac2a9`; coordinator formatting, 40 tests, and strict Clippy passed. The lead is resumed for another unblocked slice; the ID remains pending. |
| luna-04 | PID 56892, live | Native pen/touch worker candidate is reviewed but held pending L02/L05 contract acknowledgements and coordinator release. The lead is resumed for another independent input slice. |
| luna-05 | PID 50380, live | Catalog HDR storage landed as `200a4b62` and formatter cleanup as `a141941`; 80 tests and strict Clippy passed. The next reservation is an application README correction for `per_application:elevated`. |
| luna-06 | PID 33088, live | The locale worker session ended at its clean reserved base without a code candidate. The coordinator corrected the conflicting topology instruction and asked the lead to inspect the session and continue the authorized slice. |

The main checkout remains the direct landing point; do not push.
