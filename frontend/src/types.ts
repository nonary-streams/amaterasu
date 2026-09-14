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
  export interface Field {
    type: "boolean" | "integer" | "string";
    default: boolean | number | string;
    minimum?: number;
    maximum?: number;
    min_length?: number;
    max_length?: number;
  }
  export interface Configuration {
    revision: number;
    fields: Record<string, Field>;
    values: Record<string, boolean | number | string>;
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
