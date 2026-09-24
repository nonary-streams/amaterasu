# Configuration parity run coordination

- Run: `f220bcaa-20260924-152010`
- Clean source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b` on `main`
- Main branch is the direct landing and validation destination; inspect its current tip with `git rev-parse HEAD`.
- Main checkout: `C:\Users\Chase\sources\amaterasu`
- No integration branch/worktree is used. The coordinator cherry-picks accepted lane commits directly onto `main`.
- External session logs and mailboxes: `D:\sources\worktrees\amaterasu-config-runs\f220bcaa-20260924-152010`
- Status: all six lane CLI sessions are running; XHigh worker startup and work are tracked in the external run directory.

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
| `luna-03-frame-limiter-path-001` | `luna-03` owns `modules/frame-limiter/**`; `luna-02` is the only writer for NVHTTP persistence and launch projection; L05 owns app lifecycle apply/restore. | Provider and launch contract acknowledged; module implementation and host registration remain pending. |
| Product-settings migration | Reserve NVHTTP v17 once for the combined v16-to-v17 batch, with `luna-02` as sole writer. It includes L03's global frame-limiter settings, L05's raw `legacy_global_preparation`, and LAN/WAN encryption modes. Preserve structured `global_preparation`; do not create a second v17 migration. | L02 acknowledged the single batch. L06 confirmed management maps `global_prep_cmd` to `legacy_global_preparation` and reads `configured_legacy_global_preparation`. |
| LAN/WAN video encryption | `luna-02` owns global mode persistence and launch policy; `luna-06` owns signed management and editor controls. Modes are 0=never, 1=opportunistic, 2=mandatory; defaults are LAN 0 and WAN 1. Preserve negotiated audio behavior and choose the mode from the normalized peer address. | Added to the existing v17 batch; code and editor remain in progress. |
| Host settings provider | `luna-01` owns `modules/host-configuration/**`, `amaterasu.host-configuration/1`, and the revisioned v1 document for host paths/identity plus `log_path` and `min_log_level`. `file_state` and `vibeshine_file_state` select contained legacy import files and do not replace live Amaterasu state storage. `luna-02` consumes read-only identity/import values; `luna-06` owns the signed editor and diagnostics behavior. | Contract acknowledged; provider and editor implementations are in progress. |
| Session TTL and browser session parity | `luna-01` owns auth persistence and configured TTL behavior; `luna-06` owns management cookies and frontend behavior. Keep management loopback-only and retain Secure, HttpOnly, SameSite and Origin protections. Access cookie uses `Path=/v1/`; refresh cookie uses `Path=/v1/auth`. | Contract acknowledged; authentication and cookie workers are active. Restart and browser qualification remain pending. |
| DS4 Back mapping and gamepad profile | `luna-04` owns input policy, global `gamepad` and `ds4_back_as_touchpad_click`, and input settings v9. L02 owns the stream launch projection; L05 owns sparse app overrides; L06 owns signed management/editor changes. Explicit false is preserved; only selected DS4 profiles change. | Input slice landed on main; L02 acknowledged projection ownership. L05 app overrides and L06 editor remain pending. |
| Playnite path controls | Pinned reference removed both install/path overrides and the current provider discovers its executable without consuming them. Do not invent semantics for `global:playnite_install_dir` or `global:playnite_extensions_dir`. | Classified as removed/inert reference controls; keep stable IDs pending until the coverage inventory supports a retired status. |
| Updater, tray and host metrics | `luna-06` owns `modules/updater/**`, `modules/system-tray/**`, and `modules/host-metrics/**`; `luna-01` owns host startup integration in `crates/host/**`; `luna-02` owns stream-session metric emission. | Path ownership assigned; module contracts and lifecycle integration pending. |

## Mainline cherry-pick queue

| Lane | Source commit(s) | Main commit(s) | Status |
| --- | --- | --- | --- |
| luna-04 DS4 input slice | `50163b7d`, `4af44696` | `627dc591`, `e306b332` | Cherry-picked cleanly. Rustfmt and diff checks passed in lane; tests were authored but not run. Windows ViGEm device qualification and management/app/stream integration remain pending; both input IDs stay pending. |

Cherry-pick each new lane candidate directly onto `main`, resolve clear conflicts in the main checkout, and record the resulting main commit here. Lightweight lane worktrees remain non-buildable.

## Shared-resource queue

No host-global audio, virtual-display, input-device, port, or installed-service tests scheduled yet.

## Active Luna Max CLI sessions

All six were launched with `gpt-6-luna` at `max`, multi-agent delegation enabled, and three child slots defaulting to `gpt-6-luna` at `xhigh`. Outbox messages verify the model/worker configuration per lane; messages that are still pending are marked `launch-configured` until that lane reports its startup check. Runner PIDs identify the supervising PowerShell process; CLI session IDs are stable resume keys.

| Lead | Session ID | Runner PID | Startup report |
| --- | --- | ---: | --- |
| luna-01 | `01a0d516-858e-7c40-b459-5de0bbf3d0dd` | 24488 | session started; XHigh worker not yet reported |
| luna-02 | `01a0d516-8616-7270-a509-547be018f212` | 42520 | one XHigh worker confirmed; second check pending |
| luna-03 | `01a0d516-85d0-7261-b883-4dddf4473f99` | 21396 | session verified; XHigh worker not yet reported |
| luna-04 | `01a0d516-8713-7b83-87f1-d35c0828ae10` | 23168 | session verified; XHigh worker not yet reported |
| luna-05 | `01a0d516-8830-7960-af1e-079a13415f97` | 23228 | Steam and launch workers scoped; settings report pending |
| luna-06 | `01a0d516-8960-7e02-ac81-3eee350f679a` | 43508 | two XHigh workers confirmed (diagnostics, session history) |

## XHigh workers

| Lead | Worker | Worker session | Settings | Scope | Status |
| --- | --- | --- | --- | --- | --- |
| luna-02 | `network_policy_scan` | not yet reported | `gpt-6-luna` / `xhigh` confirmed | Network-owned paths | active |
| luna-02 | `stream_policy_scan` | not yet reported | `gpt-6-luna` / `xhigh` pending confirmation | Stream-owned paths | startup confirmation pending |
| luna-05 | `steam_slice` | not yet reported | `gpt-6-luna` / `xhigh` requested | `modules/steam/` | startup confirmation pending |
| luna-05 | `launchers_slice` | not yet reported | `gpt-6-luna` / `xhigh` requested | `modules/playnite/`, `modules/lutris/` | startup confirmation pending |
| luna-06 | `diagnostics` | `01a0d51a-45be-7bd0-b4d8-08923ae89823` | `gpt-6-luna` / `xhigh` confirmed before edits | `modules/diagnostics/` | active |
| luna-06 | `session_history` | `01a0d51a-69e9-7532-873f-55552f306fae` | `gpt-6-luna` / `xhigh` confirmed before edits | `modules/session-history/` | active |
