namespace Amaterasu {
  export type Page =
    | "overview"
    | "host"
    | "catalog"
    | "settings"
    | "updates"
    | "sessions"
    | "applications"
    | "steam"
    | "diagnostics";
  export type Payload = Record<string, unknown>;
  export interface LoginReply {
    ok: boolean;
    bearer: string;
    subject: string;
    scopes: string[];
    expires_at: number;
  }
  export type Launch =
    | { kind: "application"; path: string }
    | {
        kind: "executable";
        program: string;
        arguments?: string[];
        working_directory?: string | null;
        auto_detach?: boolean | null;
        wait_all?: boolean | null;
        exit_timeout_seconds?: number | null;
        detached?: string[];
        preparation?: PreparationStep[];
      }
    | { kind: "playnite"; game_id: string; exit_timeout_seconds: number }
    | { kind: "playnite_fullscreen"; exit_timeout_seconds: number };
  export interface PreparationStep {
    run: PreparationCommand;
    undo?: PreparationCommand | null;
  }
  export interface PreparationCommand {
    executable: string;
    args?: string[];
    environment?: Record<string, string>;
    cwd?: string | null;
    timeout_ms?: number;
  }
  export type CatalogFrameLimiterProvider = string;
  export type CaptureSelection = "auto" | "ddx" | "wgc" | "wgcc" | "pipewire" | "x11" | "kms";
  export type CatalogAutoVirtualFramegenMode = "enabled" | "vrr" | "legacy" | "disabled";
  export type CatalogFrameGenerationProvider =
    | "lossless-scaling"
    | "game-provided"
    | "nvidia-smooth-motion";
  export type CatalogFrameGenerationMode = "off" | CatalogFrameGenerationProvider;
  export type CatalogRtssFrameLimitType = string;
  export interface AdvancedInputOverrides {
    keybindings?: number[] | null;
    high_resolution_scrolling?: boolean | null;
    always_send_scancodes?: boolean | null;
    key_rightalt_to_key_win?: boolean | null;
    key_repeat_delay?: number | null;
    key_repeat_frequency?: number | null;
  }
  export interface Entry {
    name: string;
    description: string;
    tags: string[];
    launch: Launch;
    artwork?:
      | { provider: "steam"; app_id: number; revision: string }
      | { provider: "lutris"; game_id: number; revision: string };
    steam_id?: string;
    steam_install_dir?: string;
    output?: string | null;
    prep_cmd?: RawPreparationCommand[];
    exclude_global_prep_cmd?: boolean;
    elevated?: boolean;
    config_overrides?: AdvancedInputOverrides & SoftwareEncoderOverrides & NvencSettingsOverrides & {
      fec_percentage?: number;
      capture?: CaptureSelection;
      display_policy?: Record<string, unknown> | null;
      stream_audio?: boolean;
      audio_sink?: string;
      virtual_sink?: string;
      audio_sink_capture_only?: boolean;
      back_button_timeout?: number;
      controller?: boolean;
      gamepad?: string;
      keyboard?: boolean;
      mouse?: boolean;
      frame_limiter_enable?: boolean | null;
      frame_limiter_fps_limit?: number | null;
      frame_limiter_provider?: CatalogFrameLimiterProvider | null;
      frame_limiter_disable_vsync?: boolean | null;
      frame_limiter_auto_virtual_framegen?: CatalogAutoVirtualFramegenMode | null;
      lossless_scaling_framegen?: boolean | null;
      lossless_scaling_target_fps?: number | null;
      frame_generation_provider?: CatalogFrameGenerationProvider | null;
      frame_generation_mode?: CatalogFrameGenerationMode | null;
      rtss_frame_limit_type?: CatalogRtssFrameLimitType | null;
      ds4_back_as_touchpad_click?: boolean | null;
      motion_as_ds4?: boolean | null;
      touchpad_as_ds4?: boolean | null;
      ds5_inputtino_randomize_mac?: boolean | null;
      proton_dualsense_compatibility?: boolean | null;
      dd_refresh_rate_option?: RefreshRateOption | null;
      prefer_10bit_sdr?: boolean | null;
    };
  }
  export interface RawPreparationCommand {
    do?: string;
    undo?: string;
    elevated?: boolean;
  }
  export interface SoftwareEncoderOverrides {
    sw_preset?: string | null;
    sw_tune?: string | null;
    min_threads?: number | null;
    qp?: number | null;
    encoder?: "auto" | "software" | null;
  }
  export type RefreshRateOption = "auto" | "disabled" | "manual" | "prefer_highest";
  export type NvencTwoPass = "disabled" | "quarter_res" | "full_res";
  export type NvencSplitEncode = "auto" | "enabled" | "disabled";
  export interface NvencSettingsOverrides {
    nvenc_preset?: number | null;
    nvenc_h264_cavlc?: boolean | null;
    nvenc_twopass?: NvencTwoPass | null;
    nvenc_vbv_increase?: number | null;
    nvenc_split_encode?: NvencSplitEncode | null;
    nvenc_realtime_hags?: boolean | null;
    nvenc_opengl_vulkan_on_dxgi?: boolean | null;
    nvenc_latency_over_power?: boolean | null;
    vaapi_strict_rc_buffer?: boolean | null;
  }
  export interface CatalogRow {
    id: string;
    entry: Entry;
  }
  export interface CatalogConfigOverrideCapabilities {
    frame_limiter_provider: string[];
    rtss_frame_limit_type: string[];
  }
  export interface CatalogReply {
    revision: number;
    entries: CatalogRow[];
    next_cursor: unknown | null;
    config_override_capabilities: CatalogConfigOverrideCapabilities;
  }
  export type Field =
    | { type: "boolean"; default: boolean }
    | { type: "integer" | "decimal"; default: number; minimum?: number; maximum?: number }
    | { type: "string"; default: string; min_length?: number; max_length?: number }
    | { type: "enum"; default: string; options: string[] }
    | { type: "list"; default: unknown[]; item: Field; min_items?: number; max_items?: number }
    | { type: "object"; default: Record<string, unknown>; fields: Record<string, Field> };
  export interface Configuration {
    ok: true;
    revision: number;
    fields: Record<string, Field>;
    values: Record<string, unknown>;
  }
  export interface NvhttpSettings {
    revision: number;
    hostname: string;
    configured_hostname: string;
    port: number;
    https_port: number;
    address_family: "ipv4" | "both";
    bind_address: string;
    configured_address_family: "ipv4" | "both" | null;
    configured_bind_address: string | null;
    upnp: boolean;
    upnp_status: "disabled" | "active" | "waiting_for_ipv4_listener" | "waiting_for_gateway_route" | "waiting_for_listener" | "error";
    upnp_error: string | null;
    lan_encryption_mode: 0 | 1 | 2;
    wan_encryption_mode: 0 | 1 | 2;
    configured_fec_percentage: number | null;
    effective_fec_percentage: number;
    configured_output_name: string | null;
    effective_output_name: string | null;
    configured_capture: CaptureSelection;
    configured_display_policy?: Record<string, unknown>;
    capture_provider: "dxgi" | "wgc" | "pipewire" | "x11" | "kms" | null;
    configured_video_max_batch_size_kb: number | null;
    effective_video_max_batch_size_kb: number;
    video_batch_size_fallback: boolean;
    configured_max_bitrate: number | null;
    effective_max_bitrate_kbps: number | null;
    configured_qp?: number | null;
    effective_qp?: number;
    configured_minimum_fps_target: number | null;
    effective_minimum_fps_target: number;
    configured_ping_timeout_ms: number | null;
    effective_ping_timeout_ms: number;
    configured_audio_sink: string | null;
    configured_virtual_sink: string | null;
    audio_sink_capture_only: boolean;
    install_steam_audio_drivers: boolean;
    configured_nvenc_preset: number;
    configured_nvenc_h264_cavlc: boolean;
    configured_nvenc_twopass: NvencTwoPass;
    configured_nvenc_vbv_increase: number;
    configured_nvenc_split_encode: NvencSplitEncode;
    configured_nvenc_realtime_hags: boolean;
    configured_nvenc_opengl_vulkan_on_dxgi: boolean;
    configured_nvenc_latency_over_power: boolean;
    configured_vaapi_strict_rc_buffer: boolean;
    configured_external_ip: string | null;
    effective_audio_sink: string;
    configured_wgc_pacing_smoothing: boolean | null;
    configured_sw_preset: string | null;
    configured_sw_tune: string | null;
    configured_min_threads?: number | null;
    configured_encoder: "auto" | "software" | null;
    effective_sw_preset: string;
    effective_sw_tune: string;
    effective_min_threads?: number;
    effective_wgc_pacing_smoothing: boolean;
    configured_global_preparation: PreparationStep[];
    stream_audio: boolean;
    http_addresses: string[] | null;
    https_addresses: string[] | null;
    rtsp_addresses: string[] | null;
    active_http_port: number | null;
    active_https_port: number | null;
    active_rtsp_port: number | null;
    listener_error: string | null;
    rtsp_listener_error: string | null;
  }
  export interface SteamAudioDriverInstallStatus {
    enabled: boolean;
    revision: number | null;
    state: "awaiting_configuration" | "disabled" | "checking" | "already_present" |
      "installed" | "installed_with_route_restore_errors" | "installed_endpoint_unavailable" |
      "error" | "unsupported_platform";
    error_code: string | null;
    native_error: number | null;
    endpoint: string | null;
    restored_roles: string[];
    restore_failures: Array<{
      role: string;
      error_code: string;
      native_error: number | null;
    }>;
  }
  export interface SteamSettings {
    ok: true;
    revision: number;
    values: {
      enabled: boolean;
      auto_sync: boolean;
      exclude_games: SteamExclusion[];
      include_tools: boolean;
      sync_all_installed: boolean;
      recent_games: number;
      recent_max_age_days: number;
      autosync_remove_uninstalled: boolean;
    };
  }
  export interface SteamExclusion {
    id?: string;
    name?: string;
  }
  export type LutrisExclusion =
    | string
    | {
        id?: string;
        name?: string;
      };
  export interface LutrisSettings {
    ok: true;
    revision: number;
    values: {
      enabled: boolean;
      auto_sync: boolean;
      autosync_remove_uninstalled: boolean;
      include_steam: boolean;
      exclude_games: LutrisExclusion[];
    };
  }
  export interface LutrisStatus {
    ok: boolean;
    settings_ready: boolean;
    enabled: boolean;
    auto_sync: boolean;
    autosync_remove_uninstalled: boolean;
    include_steam: boolean;
    available: boolean;
    platform_supported: boolean;
    database_available: boolean;
    executable_available: boolean;
    settings_revision: number;
    game_count: number;
    refresh_pending: boolean;
    last_error: string | null;
  }
  export interface PlayniteSettings {
    ok: boolean;
    revision: number;
    values: {
      enabled: boolean;
      auto_sync: boolean;
      sync_all_installed: boolean;
      autosync_remove_uninstalled: boolean;
      autosync_delete_after_days: number;
      autosync_require_replacement: boolean;
      focus_attempts: number;
      focus_timeout_secs: number;
      focus_exit_on_first: boolean;
      fullscreen_entry_enabled: boolean;
      recent_games: number;
      recent_max_age_days: number;
      sync_categories: string[];
      exclude_categories: string[];
      sync_plugins: string[];
      exclude_plugins: string[];
      exclude_games: string[];
    };
  }
  export interface PlayniteStatus {
    ok: boolean;
    settings_ready: boolean;
    enabled: boolean;
    auto_sync: boolean;
    playnite_running: boolean;
    playnite_paths: string[];
    snapshot_pending: boolean;
    snapshot_game_count: number;
    launchable_game_count: number;
    active_provider_launches: number;
    catalog_sync: unknown;
    last_error: string | null;
    revision: number;
  }
  export interface AuthTokenSettings {
    ok: true;
    revision: number;
    values: {
      session_token_ttl_seconds: number;
      remember_me_refresh_token_ttl_seconds: number;
    };
  }
  export type HostLogLevel =
    | "product_default" | "verbose" | "debug" | "info"
    | "warning" | "error" | "fatal" | "none";
  export interface HostConfiguration {
    ok: true;
    ready: true;
    version: 1;
    revision: number;
    values: {
      file_state: string;
      vibeshine_file_state: string;
      credentials_file: string;
      pkey: string;
      cert: string;
      log_path: string;
      min_log_level: HostLogLevel;
    };
    effective: { log_path: string; min_log_level: HostLogLevel };
    logging?: { saved_revision: number; applied_revision: number; status: "applied" };
  }
  export interface DiagnosticsLoggingStatus {
    ok: true;
    ready: boolean;
    logging_configuration_revision: number | null;
  }
  export interface PairingClient {
    certificate_id: string;
    client_id: string;
    name: string;
    paired_at: number;
    config_overrides?: AdvancedInputOverrides & SoftwareEncoderOverrides & NvencSettingsOverrides & {
      fec_percentage?: number;
      capture?: CaptureSelection;
      display_policy?: Record<string, unknown> | null;
      stream_audio?: boolean;
      audio_sink?: string;
      virtual_sink?: string;
      audio_sink_capture_only?: boolean;
      back_button_timeout?: number;
      controller?: boolean;
      gamepad?: string;
      keyboard?: boolean;
      mouse?: boolean;
      native_pen_touch?: boolean;
      ds4_back_as_touchpad_click?: boolean;
      motion_as_ds4?: boolean;
      touchpad_as_ds4?: boolean;
      ds5_inputtino_randomize_mac?: boolean;
      proton_dualsense_compatibility?: boolean;
      [key: string]: unknown;
    };
  }
  export interface PairingClients {
    ok: boolean;
    revision: number;
    clients: PairingClient[];
  }
  export interface InputPolicySettings {
    ok: boolean;
    persisted: boolean;
    applied: boolean;
    cleanup_required: boolean;
    revision: number;
    values: {
      keyboard: boolean;
      mouse: boolean;
      high_resolution_scrolling: boolean;
      always_send_scancodes: boolean;
      key_rightalt_to_key_win: boolean;
      keybindings: number[];
      key_repeat_delay: number;
      key_repeat_frequency: number;
      back_button_timeout: number;
      gamepad: string;
      controller: boolean;
      ds4_back_as_touchpad_click: boolean;
      motion_as_ds4: boolean;
      touchpad_as_ds4: boolean;
      proton_dualsense_compatibility: boolean;
      ds5_inputtino_randomize_mac: boolean;
      native_pen_touch: boolean;
    };
  }
  export interface InputRuntimeStatus {
    ok: boolean;
    gamepad: string;
    supported_profiles: string[];
    recognized_profiles: string[];
    effective_gamepad_profile: string | null;
    gamepad_unavailable_reason: string | null;
    controller_input: boolean;
    controller_enabled: boolean;
    controller_cleanup_required: boolean;
    lease: Record<string, unknown> | null;
  }
  export interface HistorySettingsReply {
    ok: boolean;
    settings: {
      enabled: boolean;
      ttl_days: number;
      db_size_limit_mb: number;
    };
    revision: number;
    effective_enabled: boolean;
  }
  export interface HistorySessionSummary {
    session_id: string;
    protocol: string;
    client_name: string;
    device_name: string;
    app_name: string;
    width: number;
    height: number;
    target_fps: number;
    target_bitrate_kbps: number;
    codec: string;
    started_at_unix: number;
    ended_at_unix: number | null;
    duration_seconds: number | null;
  }
  export interface HistoryListReply {
    ok: boolean;
    sessions: HistorySessionSummary[];
    next_offset: number | null;
  }
  export interface HistoryRuntimeStatus {
    available: boolean;
    configuration_ready: boolean;
    applied_revision: number | null;
    enabled: boolean;
    effective_enabled: boolean;
    degraded: boolean;
    failed_operations: number;
    active_streams: number;
    pending_activities: number;
    sessions: number;
    samples: number;
    events: number;
    live_bytes: number;
    last_prune: {
      sessions_deleted: number;
      live_bytes: number;
      size_limit_bytes: number;
      size_limit_unmet: boolean;
    };
  }
  export interface HistoryDetailReply {
    ok: boolean;
    detail: {
      session: HistorySessionSummary;
      sample_count: number;
      event_count: number;
      samples_truncated: boolean;
      events_truncated: boolean;
      samples: {
        session_id: string;
        recorded_at_unix: number;
        frames_sent: number;
        bytes_sent: number;
        dropped_frames: number;
        actual_fps: number;
        actual_bitrate_kbps: number;
        encode_latency_ms: number | null;
      }[];
      events: {
        session_id: string;
        recorded_at_unix: number;
        event_type: string;
        payload: unknown | null;
      }[];
    } | null;
  }
  export interface Session {
    id: number;
    key: string;
    state: string;
    lease_count?: number;
    leases?: Record<string, string>;
    outcome?: string;
  }
  export interface SessionReply {
    sessions: Session[];
    next_offset: number | null;
  }
  export interface Process {
    process_id: number;
    pid: number | null;
    operation_id: string;
    executable?: string;
    state: string;
    exit_code?: number | null;
    success?: boolean;
  }
  export interface ProcessReply {
    processes: Process[];
    next_offset: number | null;
  }
  export interface Game {
    app_id: number;
    name: string;
    install_directory?: string;
    installed?: boolean;
  }
  export interface SteamReply {
    revision: number;
    games: Game[];
    next_after: number | null;
    scanning: boolean;
    complete: boolean;
  }
  export interface SteamStatus {
    revision: number;
    settings_ready?: boolean;
    enabled?: boolean;
    game_count: number;
    scanning: boolean;
    complete: boolean;
    warnings: string[];
    warning_count: number;
  }
  export interface DiagnosticRecord {
    sequence: number;
    level: string;
    target: string;
    message: string;
    fields: Record<string, string>;
  }
  export interface DiagnosticReply {
    records: DiagnosticRecord[];
    has_more: boolean;
    resync_required: boolean;
    next_cursor: number;
  }
  export interface PageContext {
    api: Api;
    content: HTMLElement;
    notify: (message: TextValue) => void;
    navigate: (page: Page) => void;
  }
}
