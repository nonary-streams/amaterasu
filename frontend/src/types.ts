namespace Amaterasu {
  export type Page =
    | "overview"
    | "catalog"
    | "settings"
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
  export interface Entry {
    name: string;
    description: string;
    tags: string[];
    launch: Launch;
    artwork?: { provider: "steam"; app_id: number; revision: string };
    steam_id?: string;
    steam_install_dir?: string;
    config_overrides?: {
      fec_percentage?: number;
      stream_audio?: boolean;
      audio_sink?: string;
      virtual_sink?: string;
      audio_sink_capture_only?: boolean;
      back_button_timeout?: number;
    };
  }
  export interface CatalogRow {
    id: string;
    entry: Entry;
  }
  export interface CatalogReply {
    revision: number;
    entries: CatalogRow[];
    next_cursor: unknown | null;
  }
  export type Field =
    | { type: "boolean"; default: boolean }
    | { type: "integer" | "decimal"; default: number; minimum?: number; maximum?: number }
    | { type: "string"; default: string; min_length?: number; max_length?: number }
    | { type: "enum"; default: string; options: string[] }
    | { type: "list"; default: unknown[]; item: Field; min_items?: number; max_items?: number }
    | { type: "object"; default: Record<string, unknown>; fields: Record<string, Field> };
  export interface Configuration {
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
    configured_output_name: string | null;
    effective_output_name: string | null;
    configured_capture: "auto" | "ddx" | "wgc" | "wgcc";
    capture_provider: "dxgi" | "wgc" | null;
    configured_video_max_batch_size_kb: number | null;
    effective_video_max_batch_size_kb: number;
    video_batch_size_fallback: boolean;
    configured_max_bitrate: number | null;
    effective_max_bitrate_kbps: number | null;
    configured_minimum_fps_target: number | null;
    effective_minimum_fps_target: number;
    configured_ping_timeout_ms: number | null;
    effective_ping_timeout_ms: number;
    configured_audio_sink: string | null;
    configured_virtual_sink: string | null;
    audio_sink_capture_only: boolean;
    configured_external_ip: string | null;
    effective_audio_sink: string;
    configured_wgc_pacing_smoothing: boolean | null;
    effective_wgc_pacing_smoothing: boolean;
    stream_audio: boolean;
    http_addresses: string[] | null;
    https_addresses: string[] | null;
    rtsp_addresses: string[] | null;
    active_http_port: number | null;
    active_https_port: number | null;
    listener_error: string | null;
  }
  export interface SteamSettings {
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
    revision: number;
    values: {
      session_token_ttl_seconds: number;
      remember_me_refresh_token_ttl_seconds: number;
    };
  }
  export interface PairingClient {
    certificate_id: string;
    client_id: string;
    name: string;
    paired_at: number;
    config_overrides?: {
      fec_percentage?: number;
      stream_audio?: boolean;
      audio_sink?: string;
      virtual_sink?: string;
      audio_sink_capture_only?: boolean;
      back_button_timeout?: number;
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
      key_repeat_delay: number;
      key_repeat_frequency: number;
      back_button_timeout: number;
      gamepad: string;
    };
  }
  export interface InputRuntimeStatus {
    ok: boolean;
    gamepad: string;
    supported_profiles: string[];
    effective_gamepad_profile: string | null;
    gamepad_unavailable_reason: string | null;
    controller_input: boolean;
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
        encode_latency_ms: number;
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
    notify: (message: string) => void;
    navigate: (page: Page) => void;
  }
}
