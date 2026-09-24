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
  export interface Launch {
    kind: "application" | "executable";
    path?: string;
    program?: string;
    arguments?: string[];
    working_directory?: string | null;
  }
  export interface Entry {
    name: string;
    description: string;
    tags: string[];
    launch: Launch;
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
    };
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
