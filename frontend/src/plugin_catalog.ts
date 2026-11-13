namespace Amaterasu {
  // What the management API reports about packages, modules, updates and
  // repositories, and the Modules page's view of it. Nothing here touches the
  // document.

  /// The page's views in tab order. `#plugins` alone opens the first.
  export const PLUGINS_VIEWS: readonly PluginsView[] = ["overview", "modules", "repositories"];
  export const DEFAULT_PLUGINS_VIEW: PluginsView = "overview";

  /// What two of the views were called while the page was named Plugins.
  const FORMER_VIEW_NAMES: Readonly<Record<string, PluginsView>> = {
    core: "overview", publishers: "overview", available: "overview", installed: "modules",
  };

  /// The view a `#plugins/<name>` address opens, or null when it names none.
  /// Addresses saved under the former names keep working.
  export function pluginsViewFromAddress(name: string): PluginsView | null {
    if ((PLUGINS_VIEWS as readonly string[]).includes(name)) return name as PluginsView;
    return Object.hasOwn(FORMER_VIEW_NAMES, name) ? FORMER_VIEW_NAMES[name] ?? null : null;
  }

  export interface RepositoryChoice {
    slot: string;
    implementation: string;
    version: string;
    kind: "native_set" | "web_bundle";
    manifest_sha256: string;
  }

  /// `unknown` is a host that predates catalog status in the listing.
  export type RepositoryCatalog =
    | { state: "current"; sequence: string; expiresUnix: number }
    | { state: "expired"; expiresUnix: number }
    | { state: "missing" | "unavailable" | "unknown" };

  export interface ApprovedRepositoryView {
    id: string;
    location: string;
    fingerprint: string;
    revoked: boolean;
    candidates: RepositoryChoice[];
    catalog: RepositoryCatalog;
  }

  export type RepositoryChangeKind = "add" | "refresh" | "remove";

  export interface RepositoryOperation {
    id: number;
    repository: string;
    kind: RepositoryChangeKind;
    state: "running" | "succeeded" | "failed";
    code: string | null;
  }

  export interface RepositoryPage {
    repositories: ApprovedRepositoryView[];
    nextOffset: number | null;
    /// False on a host that lists repositories but cannot change them.
    manageable: boolean;
    capacity: number | null;
    operation: RepositoryOperation | null;
  }

  export interface RunningModuleIdentity { id: string; version: string; sha256: string; runtime: boolean }

  export interface PackageSource { repository: string; fingerprint: string }

  export interface PackageSelectionStatus {
    revision: string;
    active: string | null;
    previous: string | null;
    pending: string | null;
    trial: string | null;
    highestSequence: string;
    running: {
      available: boolean;
      digest: string | null;
      modules?: RunningModuleIdentity[];
      source?: PackageSource;
    };
  }

  export interface ModuleDescription {
    id: string;
    version: string;
    sha256: string;
    runtime: boolean;
    size: number;
    abiMajor: number;
    requires: { id: string; version: string }[];
    requiredBy: string[];
    provides: string[];
    calls: string[];
    nativeProvides: string[];
    nativeUses: string[];
    publishes: string[];
    subscribes: string[];
    hooks: string[];
  }

  const DIGEST = /^[a-f0-9]{64}$/;
  const IDENTITY = /^[a-z0-9._-]{1,128}$/;

  function record(value: unknown, message: string): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(message);
    return value as Record<string, unknown>;
  }

  function plainText(value: unknown): value is string {
    return typeof value === "string" && value.length > 0 &&
      new TextEncoder().encode(value).length <= 128 && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
  }

  export function parseRunningModules(value: unknown): RunningModuleIdentity[] {
    if (!Array.isArray(value) || value.length < 1 || value.length > 128) throw new Error("Loaded module inventory is invalid.");
    const ids = new Set<string>();
    return value.map((entry: unknown) => {
      const row = record(entry, "Loaded module identity is invalid.");
      if (typeof row.id !== "string" || !IDENTITY.test(row.id) || ids.has(row.id) || !plainText(row.version) ||
          typeof row.sha256 !== "string" || !DIGEST.test(row.sha256) || typeof row.runtime !== "boolean")
        throw new Error("Loaded module identity is invalid.");
      ids.add(row.id);
      return { id: row.id, version: row.version, sha256: row.sha256, runtime: row.runtime };
    });
  }

  function parsePackageSource(value: unknown): PackageSource {
    const source = record(value, "Running package source is invalid.");
    if (typeof source.repository !== "string" || !IDENTITY.test(source.repository) ||
        typeof source.fingerprint !== "string" || !DIGEST.test(source.fingerprint))
      throw new Error("Running package source is invalid.");
    return { repository: source.repository, fingerprint: source.fingerprint };
  }

  export function parsePackageSelectionStatus(value: unknown): PackageSelectionStatus {
    const reply = record(value, "Package selection response is invalid.");
    if (reply.ok !== true)
      throw new Error("Persisted package selection is unavailable.");
    if (typeof reply.revision !== "string" || !DIGEST.test(reply.revision))
      throw new Error("Package selection revision is invalid.");
    const digest = (key: string): string | null => {
      const candidate = reply[key];
      if (candidate === null) return null;
      if (typeof candidate !== "string" || !DIGEST.test(candidate))
        throw new Error("Package selection digest is invalid.");
      return candidate;
    };
    const highestSequence = reply.highest_sequence;
    if (typeof highestSequence !== "string" ||
        !/^(0|[1-9][0-9]{0,19})$/.test(highestSequence) ||
        (highestSequence.length === 20 && highestSequence > "18446744073709551615"))
      throw new Error("Package selection sequence is invalid.");
    const running = record(reply.running, "Running package status is invalid.");
    if (running.available === true) {
      if (typeof running.digest !== "string" || !DIGEST.test(running.digest))
        throw new Error("Running package digest is invalid.");
    } else if (running.available !== false || running.digest !== undefined) {
      throw new Error("Running package status is invalid.");
    }
    const loaded = running.available === true;
    return {
      revision: reply.revision,
      active: digest("active"),
      previous: digest("previous"),
      pending: digest("pending"),
      trial: digest("trial"),
      highestSequence,
      running: {
        available: loaded,
        digest: loaded ? running.digest as string : null,
        ...(loaded && Object.hasOwn(running, "modules") ? { modules: parseRunningModules(running.modules) } : {}),
        ...(loaded && Object.hasOwn(running, "source") ? { source: parsePackageSource(running.source) } : {}),
      },
    };
  }

  // --- Updating the host's own package ---------------------------------------

  export type PackageUpdatePhase =
    | "idle" | "checking" | "current" | "available" | "downloading"
    | "ready" | "waiting_stream" | "activating" | "failed" | "cancelled";

  const UPDATE_PHASES: readonly PackageUpdatePhase[] = [
    "idle", "checking", "current", "available", "downloading",
    "ready", "waiting_stream", "activating", "failed", "cancelled",
  ];

  export type ModuleChangeKind = "updated" | "rebuilt" | "added" | "removed" | "unchanged";

  const MODULE_CHANGE_KINDS: readonly ModuleChangeKind[] = ["updated", "rebuilt", "added", "removed", "unchanged"];

  export interface PackageUpdateModule {
    id: string;
    /// Null for a module the update adds.
    from: string | null;
    /// Null for a module the update removes.
    to: string | null;
    change: ModuleChangeKind;
    size: number;
  }

  export interface PackageUpdate {
    repository: string;
    slot: string;
    implementation: string;
    version: string;
    digest: string;
    sequence: string;
    /// Every module of the new package, and any the update removes.
    modules: PackageUpdateModule[];
    downloadBytes: number;
    totalBytes: number;
  }

  /// `supported: false` is a host that has the routes but cannot install on
  /// its platform; nothing else in its reply is read.
  export type PackageUpdateStatus =
    | { supported: false }
    | {
      supported: true;
      phase: PackageUpdatePhase;
      checkedUnixMs: number | null;
      source: PackageSource | null;
      /// Both null when the host could not say which package it loaded.
      running: { digest: string | null; sequence: string | null };
      update: PackageUpdate | null;
      transferredBytes: number;
      /// The host's code for why the last update stopped.
      error: string | null;
      /// How the host keeps itself up to date; null on a host that only updates when asked.
      automatic: AutomaticUpdates | null;
      /// True when the last check found the repository publishing an older
      /// list of releases than the host already holds, as after a local install.
      repositoryBehind: boolean;
      /// Whether the host lists its earlier packages and can switch to one.
      versions: boolean;
      /// The earlier package the host is switching to, when it is.
      restoring: { digest: string; sequence: string } | null;
    };

  export type AutomaticMode = "off" | "download" | "install";

  export const AUTOMATIC_MODES: readonly AutomaticMode[] = ["off", "download", "install"];

  export interface AutomaticUpdates {
    mode: AutomaticMode;
    /// When the host will next look for an update by itself.
    nextCheckUnixMs: number | null;
  }

  /// The largest time an ECMAScript date can hold.
  const LATEST_UNIX_MS = 8_640_000_000_000_000;

  function releaseSequence(value: unknown): value is string {
    return typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/.test(value) &&
      !(value.length === 20 && value > "18446744073709551615");
  }

  function byteCount(value: unknown): value is number {
    return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
  }

  function parseUpdateModules(value: unknown): PackageUpdateModule[] {
    if (!Array.isArray(value) || value.length < 1 || value.length > 128) throw new Error("Update module list is invalid.");
    const ids = new Set<string>();
    const version = (candidate: unknown): candidate is string | null => candidate === null || plainText(candidate);
    return value.map((entry: unknown) => {
      const row = record(entry, "Update module is invalid.");
      const change = row.change;
      if (typeof row.id !== "string" || !IDENTITY.test(row.id) || ids.has(row.id) ||
          !version(row.from) || !version(row.to) || !byteCount(row.size) ||
          typeof change !== "string" || !(MODULE_CHANGE_KINDS as readonly string[]).includes(change) ||
          (row.from === null) !== (change === "added") || (row.to === null) !== (change === "removed"))
        throw new Error("Update module is invalid.");
      ids.add(row.id);
      return { id: row.id, from: row.from, to: row.to, change: change as ModuleChangeKind, size: row.size };
    });
  }

  function parsePackageUpdate(value: unknown): PackageUpdate {
    const update = record(value, "Package update is invalid.");
    if (typeof update.repository !== "string" || !IDENTITY.test(update.repository) ||
        typeof update.slot !== "string" || !IDENTITY.test(update.slot) ||
        !plainText(update.implementation) || !plainText(update.version) ||
        typeof update.digest !== "string" || !DIGEST.test(update.digest) ||
        !releaseSequence(update.sequence) ||
        !byteCount(update.download_bytes) || !byteCount(update.total_bytes))
      throw new Error("Package update is invalid.");
    return {
      repository: update.repository,
      slot: update.slot,
      implementation: update.implementation,
      version: update.version,
      digest: update.digest,
      sequence: update.sequence,
      modules: parseUpdateModules(update.modules),
      downloadBytes: update.download_bytes,
      totalBytes: update.total_bytes,
    };
  }

  function unixMs(value: unknown): value is number {
    return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= LATEST_UNIX_MS;
  }

  /// Absent on a host that only updates when asked.
  function parseAutomatic(value: unknown): AutomaticUpdates | null {
    if (value === undefined || value === null) return null;
    const automatic = record(value, "Automatic update setting is invalid.");
    const next = automatic.next_check_unix_ms ?? null;
    if (typeof automatic.mode !== "string" || !(AUTOMATIC_MODES as readonly string[]).includes(automatic.mode) ||
        (next !== null && !unixMs(next)))
      throw new Error("Automatic update setting is invalid.");
    return { mode: automatic.mode as AutomaticMode, nextCheckUnixMs: next as number | null };
  }

  function parseRestoring(value: unknown): { digest: string; sequence: string } | null {
    if (value === undefined || value === null) return null;
    const target = record(value, "Version switch is invalid.");
    if (typeof target.digest !== "string" || !DIGEST.test(target.digest) || !releaseSequence(target.sequence))
      throw new Error("Version switch is invalid.");
    return { digest: target.digest, sequence: target.sequence };
  }

  export function parsePackageUpdateStatus(value: unknown): PackageUpdateStatus {
    const reply = record(value, "Package update response is invalid.");
    if (reply.ok !== true) throw new Error("Package update status is unavailable.");
    if (reply.supported === false) return { supported: false };
    if (reply.supported !== true) throw new Error("Package update support is invalid.");
    const phase = reply.phase;
    if (typeof phase !== "string" || !(UPDATE_PHASES as readonly string[]).includes(phase))
      throw new Error("Package update phase is invalid.");
    const checked = reply.checked_unix_ms;
    if (checked !== null && !unixMs(checked)) throw new Error("Package update check time is invalid.");
    const behind = reply.repository_behind ?? false;
    if (typeof behind !== "boolean") throw new Error("Package update status is invalid.");
    const running = record(reply.running, "Running package status is invalid.");
    const unreported = running.digest === null && running.sequence === null;
    if (!unreported &&
        (typeof running.digest !== "string" || !DIGEST.test(running.digest) || !releaseSequence(running.sequence)))
      throw new Error("Running package status is invalid.");
    if (!byteCount(reply.transferred_bytes)) throw new Error("Package update progress is invalid.");
    const error = reply.error;
    if (error !== null && (typeof error !== "string" || !/^[a-z0-9_]{1,64}$/.test(error)))
      throw new Error("Package update error is invalid.");
    return {
      supported: true,
      phase: phase as PackageUpdatePhase,
      checkedUnixMs: checked,
      source: reply.source === null ? null : parsePackageSource(reply.source),
      running: unreported
        ? { digest: null, sequence: null }
        : { digest: running.digest as string, sequence: running.sequence as string },
      update: reply.update === null ? null : parsePackageUpdate(reply.update),
      transferredBytes: reply.transferred_bytes,
      error,
      automatic: parseAutomatic(reply.automatic),
      repositoryBehind: behind,
      // A host that can switch packages always says whether it is switching.
      versions: reply.restoring !== undefined,
      restoring: parseRestoring(reply.restoring),
    };
  }

  // --- The packages a host has kept ---------------------------------------------

  export type PackageVersionState = "running" | "previous" | "retained";

  export interface PackageVersion {
    digest: string;
    sequence: string;
    state: PackageVersionState;
    /// When the host first held the package; null when it cannot say.
    installedUnixMs: number | null;
    modules: number;
    /// How many modules differ from the running package, counting ones only one of them has.
    differs: number;
    restorable: boolean;
    /// The host's code for why a package cannot be switched to.
    reason: string | null;
    /// True when the package has run here with version switching, so the
    /// console can switch again once it is running.
    switchesBack: boolean;
  }

  export interface PackageVersions {
    supported: boolean;
    /// True while the host is still reading its packages; ask again shortly.
    loading: boolean;
    /// Every package the host has kept, which can be more than it lists.
    total: number;
    versions: PackageVersion[];
  }

  const VERSION_STATES: readonly PackageVersionState[] = ["running", "previous", "retained"];

  export function parsePackageVersions(value: unknown): PackageVersions {
    const reply = record(value, "Package version list is invalid.");
    if (reply.ok !== true) throw new Error("Package versions are unavailable.");
    if (reply.supported === false) return { supported: false, loading: false, total: 0, versions: [] };
    const loading = reply.loading ?? false;
    if (typeof loading !== "boolean") throw new Error("Package version list is invalid.");
    if (reply.supported !== true || !Array.isArray(reply.versions) || reply.versions.length > 64 || !byteCount(reply.total))
      throw new Error("Package version list is invalid.");
    const seen = new Set<string>();
    const versions = reply.versions.map((entry: unknown): PackageVersion => {
      const row = record(entry, "Package version is invalid.");
      const installed = row.installed_unix_ms ?? null;
      const reason = row.reason ?? null;
      const switchesBack = row.switches_back ?? false;
      if (typeof switchesBack !== "boolean" || typeof row.digest !== "string" || !DIGEST.test(row.digest) || seen.has(row.digest) ||
          !releaseSequence(row.sequence) ||
          typeof row.state !== "string" || !(VERSION_STATES as readonly string[]).includes(row.state) ||
          (installed !== null && !unixMs(installed)) ||
          !byteCount(row.modules) || !byteCount(row.differs) || typeof row.restorable !== "boolean" ||
          (reason !== null && (typeof reason !== "string" || !/^[a-z0-9_]{1,64}$/.test(reason))))
        throw new Error("Package version is invalid.");
      seen.add(row.digest);
      return {
        digest: row.digest,
        sequence: row.sequence,
        state: row.state as PackageVersionState,
        installedUnixMs: installed as number | null,
        modules: row.modules,
        differs: row.differs,
        restorable: row.restorable,
        reason: reason as string | null,
        switchesBack,
      };
    });
    return { supported: true, loading, total: Math.max(reply.total, versions.length), versions };
  }

  const VERSION_REASONS: Readonly<Record<string, string>> = {
    expired: "Its signature has expired",
    publisher_untrusted: "Its publisher is no longer trusted",
    unavailable: "Its files are no longer on this host",
  };

  /// Why the host will not switch to a package it lists.
  export function packageVersionReason(code: string | null): string {
    return (code !== null && Object.hasOwn(VERSION_REASONS, code) && VERSION_REASONS[code]) || "Cannot be switched to";
  }

  /// How a kept package differs from the running one, in a few words.
  export function packageVersionDifference(version: PackageVersion): string {
    if (version.state === "running") return `${version.modules} ${version.modules === 1 ? "module" : "modules"}`;
    if (version.differs === 0) return "Same modules";
    return `${version.differs} of ${version.modules} ${version.modules === 1 ? "module" : "modules"} ${version.differs === 1 ? "differs" : "differ"}`;
  }

  function parseCatalog(value: unknown): RepositoryCatalog {
    if (value === undefined) return { state: "unknown" };
    const catalog = record(value, "Repository catalog status is invalid.");
    const expires = catalog.expires_unix;
    const validExpiry = typeof expires === "number" && Number.isSafeInteger(expires) && expires >= 0;
    if (catalog.state === "current" && validExpiry &&
        typeof catalog.sequence === "string" && /^(0|[1-9][0-9]{0,19})$/.test(catalog.sequence))
      return { state: "current", sequence: catalog.sequence, expiresUnix: expires };
    if (catalog.state === "expired" && validExpiry) return { state: "expired", expiresUnix: expires };
    if (catalog.state === "missing" || catalog.state === "unavailable") return { state: catalog.state };
    throw new Error("Repository catalog status is invalid.");
  }

  export function parseRepositoryList(value: unknown): ApprovedRepositoryView[] {
    const reply = record(value, "Repository response is invalid.");
    if (reply.ok !== true || !Array.isArray(reply.repositories))
      throw new Error("Repository state is unavailable.");
    return reply.repositories.map((item: unknown) => {
      const repository = record(item, "Repository entry is invalid.");
      if (typeof repository.id !== "string" ||
          typeof repository.location !== "string" ||
          typeof repository.fingerprint !== "string" ||
          typeof repository.revoked !== "boolean" ||
          !Array.isArray(repository.candidates))
        throw new Error("Repository entry is invalid.");
      const candidates = repository.candidates.map((raw: unknown) => {
        const choice = record(raw, "Repository choice is invalid.");
        if (typeof choice.slot !== "string" ||
            typeof choice.implementation !== "string" ||
            typeof choice.version !== "string" ||
            typeof choice.manifest_sha256 !== "string" ||
            (choice.kind !== "native_set" && choice.kind !== "web_bundle"))
          throw new Error("Repository choice is invalid.");
        return {
          slot: choice.slot,
          implementation: choice.implementation,
          version: choice.version,
          kind: choice.kind,
          manifest_sha256: choice.manifest_sha256,
        } satisfies RepositoryChoice;
      });
      return {
        id: repository.id,
        location: repository.location,
        fingerprint: repository.fingerprint,
        revoked: repository.revoked,
        candidates,
        catalog: parseCatalog(repository.catalog),
      };
    });
  }

  function parseOperation(value: unknown): RepositoryOperation | null {
    if (value === undefined || value === null) return null;
    const operation = record(value, "Repository change status is invalid.");
    if (typeof operation.id !== "number" || !Number.isSafeInteger(operation.id) || operation.id < 1 ||
        typeof operation.repository !== "string" ||
        (operation.kind !== "add" && operation.kind !== "refresh" && operation.kind !== "remove") ||
        (operation.state !== "running" && operation.state !== "succeeded" && operation.state !== "failed") ||
        (operation.code !== null && typeof operation.code !== "string"))
      throw new Error("Repository change status is invalid.");
    return {
      id: operation.id,
      repository: operation.repository,
      kind: operation.kind,
      state: operation.state,
      code: operation.code,
    };
  }

  export function parseRepositoryPage(value: unknown): RepositoryPage {
    const repositories = parseRepositoryList(value);
    const reply = value as Record<string, unknown>;
    const nextOffset = reply.next_offset;
    if (nextOffset !== null &&
        (!Number.isSafeInteger(nextOffset) || (nextOffset as number) <= 0 || (nextOffset as number) > 4096))
      throw new Error("Repository pagination is invalid.");
    const capacity = reply.capacity;
    return {
      repositories,
      nextOffset: nextOffset as number | null,
      manageable: reply.manageable === true,
      capacity: typeof capacity === "number" && Number.isSafeInteger(capacity) && capacity > 0 ? capacity : null,
      operation: parseOperation(reply.operation),
    };
  }

  function nameList(value: unknown, pattern?: RegExp): string[] {
    if (!Array.isArray(value) || value.length > 128) throw new Error("Module description is invalid.");
    return value.map((name: unknown) => {
      if (!plainText(name) || (pattern && !pattern.test(name))) throw new Error("Module description is invalid.");
      return name;
    });
  }

  export function parseModuleDescription(value: unknown): ModuleDescription {
    const reply = record(value, "Module description is invalid.");
    if (reply.ok !== true) throw new Error("Module description is unavailable.");
    const described = record(reply.module, "Module description is invalid.");
    if (typeof described.id !== "string" || !IDENTITY.test(described.id) || !plainText(described.version) ||
        typeof described.sha256 !== "string" || !DIGEST.test(described.sha256) ||
        typeof described.runtime !== "boolean" ||
        typeof described.size !== "number" || !Number.isSafeInteger(described.size) || described.size < 0 ||
        typeof described.abi_major !== "number" || !Number.isSafeInteger(described.abi_major) || described.abi_major < 0 ||
        !Array.isArray(described.requires) || described.requires.length > 128)
      throw new Error("Module description is invalid.");
    const requires = described.requires.map((entry: unknown) => {
      const dependency = record(entry, "Module description is invalid.");
      if (typeof dependency.id !== "string" || !IDENTITY.test(dependency.id) || !plainText(dependency.version))
        throw new Error("Module description is invalid.");
      return { id: dependency.id, version: dependency.version };
    });
    return {
      id: described.id,
      version: described.version,
      sha256: described.sha256,
      runtime: described.runtime,
      size: described.size,
      abiMajor: described.abi_major,
      requires,
      requiredBy: nameList(described.required_by, IDENTITY),
      provides: nameList(described.provides),
      calls: nameList(described.calls),
      nativeProvides: nameList(described.native_provides),
      nativeUses: nameList(described.native_uses),
      publishes: nameList(described.publishes),
      subscribes: nameList(described.subscribes),
      hooks: nameList(described.hooks),
    };
  }

  // --- What a plugin is called and where it is filed -----------------------

  export type PluginCategory =
    | "Core" | "Streaming" | "Video" | "Audio & input" | "Library" | "Diagnostics" | "Interface" | "Other";

  export const PLUGIN_CATEGORIES: readonly PluginCategory[] =
    ["Core", "Streaming", "Video", "Audio & input", "Library", "Diagnostics", "Interface", "Other"];

  export interface PluginPresentation { name: string; summary: string; category: PluginCategory }

  /// The module slot a console interface bundle fills in a repository catalog.
  export const CONSOLE_SLOT = "amaterasu.ui";

  const KNOWN_PLUGINS: Readonly<Record<string, readonly [string, string, PluginCategory]>> = {
    "amaterasu.runtime": ["Runtime", "Loads every module and routes the calls between them.", "Core"],
    "amaterasu.network": ["Network", "Opens the sockets and name lookups other modules use.", "Core"],
    "amaterasu.state": ["State store", "Keeps each module's saved data on disk.", "Core"],
    "amaterasu.configuration": ["Configuration", "Stores settings and their schemas for each module.", "Core"],
    "amaterasu.host-configuration": ["Host configuration", "Holds host-wide settings that change while the host runs.", "Core"],
    "amaterasu.management": ["Management console", "Serves this console and the management API.", "Core"],
    "amaterasu.authentication": ["Accounts and sign-in", "Signs in console accounts and issues API tokens.", "Core"],
    "amaterasu.extension-registry": ["Extension registry", "Finds session extensions and keeps their settings.", "Core"],
    "amaterasu.extension-actions": ["Extension actions", "Tracks the actions session extensions run.", "Core"],
    "amaterasu.updater": ["Update checker", "Checks for new Amaterasu releases.", "Core"],
    "amaterasu.system-tray": ["System tray", "Shows the tray icon and desktop notifications.", "Core"],
    "amaterasu.session": ["Session coordination", "Shares session records between capture and protocol modules.", "Streaming"],
    "amaterasu.session-lifecycle": ["Session lifecycle", "Starts, tracks and cleans up each streaming session.", "Streaming"],
    "amaterasu.stream-session": ["Stream session", "Runs each stream from its first frame to its last.", "Streaming"],
    "amaterasu.nvhttp": ["Moonlight server", "Answers Moonlight discovery, pairing and launch requests.", "Streaming"],
    "amaterasu.pairing": ["Device pairing", "Pairs client devices and keeps their certificates.", "Streaming"],
    "amaterasu.rtsp": ["Stream setup", "Agrees the stream's format with a client over RTSP.", "Streaming"],
    "amaterasu.media-transport": ["Media transport", "Sends video and audio packets to clients.", "Streaming"],
    "amaterasu.control-transport": ["Control transport", "Carries the encrypted control channel.", "Streaming"],
    "amaterasu.upnp": ["Port mapping", "Opens router ports for streaming over the internet.", "Streaming"],
    "amaterasu.remote-session": ["Remote session", "Provides the remote monitor and remote input modes.", "Streaming"],
    "amaterasu.video": ["Video pipeline", "Moves captured frames through processing to the encoder.", "Video"],
    "amaterasu.linux-session-video": ["Video pipeline", "Moves captured frames through processing to the encoder.", "Video"],
    "amaterasu.encode": ["Video encoder", "Encodes frames with the host's hardware or software encoders.", "Video"],
    "amaterasu.capture-kms": ["Screen capture (KMS)", "Reads a physical display through kernel mode setting.", "Video"],
    "amaterasu.capture-pipewire": ["Screen capture (PipeWire)", "Captures through the desktop's screen-sharing portal.", "Video"],
    "amaterasu.capture-vkms": ["Virtual display capture", "Captures the stream's virtual display without copying frames.", "Video"],
    "amaterasu.capture-dxgi": ["Screen capture (DXGI)", "Captures a display with Desktop Duplication.", "Video"],
    "amaterasu.capture-wgc": ["Screen capture (WGC)", "Captures a display with Windows Graphics Capture.", "Video"],
    "amaterasu.capture-x11": ["Screen capture (X11)", "Captures an X11 screen.", "Video"],
    "amaterasu.capture-black": ["Blank video source", "Supplies black frames for input-only sessions.", "Video"],
    "amaterasu.display": ["Displays", "Lists displays and applies the stream's display mode.", "Video"],
    "amaterasu.display-settings": ["Display settings", "Reads and changes HDR and other display settings.", "Video"],
    "amaterasu.virtual-display": ["Virtual display", "Creates a display sized for the client while it streams.", "Video"],
    "amaterasu.frame-limiter": ["Frame limiter", "Caps a game's frame rate to match the stream.", "Video"],
    "amaterasu.foreground-context": ["Foreground window", "Reports which window is in front.", "Video"],
    "amaterasu.rtx-hdr": ["RTX HDR", "Converts SDR streams to HDR with NVIDIA RTX.", "Video"],
    "amaterasu.audio": ["Audio capture", "Captures the host's audio for the stream.", "Audio & input"],
    "amaterasu.audio-encode": ["Audio encoder", "Encodes captured audio as Opus.", "Audio & input"],
    "amaterasu.input": ["Input", "Delivers keyboard, mouse and controller input to the host.", "Audio & input"],
    "amaterasu.catalog": ["Application catalog", "Holds the applications clients can launch.", "Library"],
    "amaterasu.library": ["Library", "Presents the catalog to clients and serves cover art.", "Library"],
    "amaterasu.launch": ["Launcher", "Starts catalog applications for a session.", "Library"],
    "amaterasu.application": ["Application processes", "Tracks and stops the processes a launch starts.", "Library"],
    "amaterasu.steam": ["Steam", "Finds games in local Steam libraries.", "Library"],
    "amaterasu.lutris": ["Lutris", "Finds games in the current user's Lutris library.", "Library"],
    "amaterasu.playnite": ["Playnite", "Imports installed games from Playnite.", "Library"],
    "amaterasu.session-history": ["Session history", "Records past sessions and their timelines.", "Diagnostics"],
    "amaterasu.host-metrics": ["Host metrics", "Samples processor, graphics and stream statistics.", "Diagnostics"],
    "amaterasu.diagnostics": ["Diagnostics", "Keeps recent diagnostic records for the console.", "Diagnostics"],
    [CONSOLE_SLOT]: ["Console interface", "A web interface for this console.", "Interface"],
  };

  /// A readable name for an identity nobody described: its last segment.
  function nameFromIdentity(id: string): string {
    const words = (id.split(".").at(-1) || id).split(/[-_]+/).filter(Boolean);
    const name = words.join(" ");
    return name ? name.charAt(0).toUpperCase() + name.slice(1) : id;
  }

  export function pluginPresentation(id: string): PluginPresentation {
    const known = KNOWN_PLUGINS[id];
    if (known) return { name: known[0], summary: known[1], category: known[2] };
    return { name: nameFromIdentity(id), summary: "", category: "Other" };
  }

  // --- Families of core modules and where their source lives -----------------

  export interface SourceRepository {
    /// "owner/name", as its address writes it.
    name: string;
    url: string;
  }

  export interface ModuleFamily {
    id: string;
    name: string;
    summary: string;
    /// Every module of the family, on any platform. A host runs some of them.
    members: readonly string[];
    /// The repositories the family is built from.
    sources: readonly SourceRepository[];
  }

  function sourceRepository(name: string): SourceRepository {
    return { name: `nonary-streams/${name}`, url: `https://github.com/nonary-streams/${name}` };
  }

  /// Where a module without a repository of its own is built.
  export const AMATERASU_SOURCE: SourceRepository = sourceRepository("amaterasu");

  /// How a module of Amaterasu's that belongs to no family is described.
  export const BUILT_IN_LABEL = "Built into Amaterasu";

  // Family, name, summary, each member with the repository it is built from,
  // and any repository the family draws on that builds none of its modules.
  const FAMILY_TABLE: readonly (readonly [
    string, string, string, readonly (readonly [string, string])[], (readonly string[])?,
  ])[] = [
    ["input", "Input", "Delivers keyboard, mouse, touch and controller input from the client to this host.",
      [["amaterasu.input", "amaterasu-input"]]],
    ["audio", "Audio", "Captures this host's sound and encodes it for the stream.",
      [["amaterasu.audio", "amaterasu-audio"], ["amaterasu.audio-encode", "amaterasu-audio-encode"]]],
    ["capture", "Screen capture",
      "Reads the picture to stream from a display, the desktop portal or the stream's own virtual display.",
      [["amaterasu.capture-kms", "amaterasu-capture"], ["amaterasu.capture-pipewire", "amaterasu-capture"],
        ["amaterasu.capture-x11", "amaterasu-capture"], ["amaterasu.capture-dxgi", "amaterasu-capture"],
        ["amaterasu.capture-wgc", "amaterasu-capture"], ["amaterasu.capture-black", "amaterasu-capture"],
        ["amaterasu.capture-vkms", "amaterasu-capture-vkms"]]],
    ["video", "Video encoding",
      "Processes captured frames and encodes them with the host's hardware or software encoders.",
      [["amaterasu.video", "amaterasu-encode-video"], ["amaterasu.linux-session-video", "amaterasu-encode-video"],
        ["amaterasu.encode", "amaterasu-encode-video"]]],
    ["display", "Display", "Lists displays and prepares the right one, at the right mode, for each stream.",
      [["amaterasu.display", "amaterasu-display"], ["amaterasu.display-settings", "amaterasu"],
        ["amaterasu.virtual-display", "amaterasu"]]],
    ["remote-session", "Remote session", "Provides the remote monitor and remote input modes.",
      [["amaterasu.remote-session", "amaterasu-remote-session"]]],
    ["rtx-hdr", "RTX HDR", "Converts SDR streams to HDR with NVIDIA RTX.",
      [["amaterasu.rtx-hdr", "amaterasu-rtx-hdr"]]],
    ["frame-limiter", "Frame limiter", "Caps a game's frame rate to match the stream.",
      [["amaterasu.frame-limiter", "amaterasu-frame-limiter"]]],
    ["statistics", "Statistics", "Samples host and stream statistics and keeps session history.",
      [["amaterasu.host-metrics", "amaterasu-stats"], ["amaterasu.session-history", "amaterasu-stats"]]],
    ["runtime", "Runtime and SDK",
      "Loads every module and carries the calls between them; modules are written against the SDK.",
      [["amaterasu.runtime", "amaterasu"]], ["amaterasu-sdk"]],
  ];

  const MODULE_HOMES = new Map<string, { family: ModuleFamily; source: SourceRepository }>();

  /// The core module families, in the order the page shows them.
  export const MODULE_FAMILIES: readonly ModuleFamily[] = FAMILY_TABLE.map(([id, name, summary, members, also = []]) => {
    const repositories = [...new Set([...members.map(([, repository]) => repository), ...also])];
    const family: ModuleFamily = {
      id, name, summary,
      members: members.map(([module]) => module),
      sources: repositories.map(sourceRepository),
    };
    for (const [module, repository] of members) MODULE_HOMES.set(module, { family, source: sourceRepository(repository) });
    return family;
  });

  export interface ModuleOrigin {
    family: ModuleFamily | null;
    /// The family's name, or how a module of Amaterasu's without one is
    /// described. Null for a module Amaterasu does not publish.
    label: string | null;
    /// Where the module's source is kept, when this console knows.
    source: SourceRepository | null;
  }

  /// The family a module belongs to and the repository it is built from. A
  /// module of Amaterasu's outside every family is built into Amaterasu itself;
  /// nothing is claimed about a module from another publisher.
  export function moduleOrigin(id: string): ModuleOrigin {
    const home = MODULE_HOMES.get(id);
    if (home) return { family: home.family, label: home.family.name, source: home.source };
    if (id.startsWith("amaterasu.")) return { family: null, label: BUILT_IN_LABEL, source: AMATERASU_SOURCE };
    return { family: null, label: null, source: null };
  }

  /// The running modules no family row accounts for: Amaterasu's own, built
  /// into its main repository, and any another developer wrote.
  export function modulesOutsideFamilies(plugins: readonly InstalledPlugin[]): { builtIn: InstalledPlugin[]; foreign: InstalledPlugin[] } {
    const loose = plugins.filter((plugin) => moduleOrigin(plugin.id).family === null);
    return {
      builtIn: loose.filter((plugin) => plugin.id.startsWith("amaterasu.")),
      foreign: loose.filter((plugin) => !plugin.id.startsWith("amaterasu.")),
    };
  }

  /// The version a repository gives the package the host runs, when one lists it.
  export function runningPackageVersion(
    repositories: readonly ApprovedRepositoryView[],
    digest: string | null,
  ): string | null {
    if (!digest) return null;
    for (const repository of repositories) {
      if (repository.revoked) continue;
      const choice = repository.candidates.find((candidate) =>
        candidate.kind === "native_set" && candidate.manifest_sha256 === digest);
      if (choice) return choice.version;
    }
    return null;
  }

  export interface InstalledPlugin extends PluginPresentation {
    id: string;
    version: string;
    sha256: string;
    runtime: boolean;
  }

  export function installedPlugins(selection: PackageSelectionStatus | null): InstalledPlugin[] {
    return (selection?.running.modules ?? [])
      .map((module) => ({ ...pluginPresentation(module.id), ...module }))
      .sort(byCategoryThenName);
  }

  function byCategoryThenName(a: PluginPresentation & { id: string }, b: PluginPresentation & { id: string }): number {
    return PLUGIN_CATEGORIES.indexOf(a.category) - PLUGIN_CATEGORIES.indexOf(b.category) ||
      a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
  }

  /// Orders dotted versions number by number, so 0.10.0 follows 0.9.0. A
  /// version with a trailing label, such as 1.0.0-beta, precedes its release.
  export function compareVersions(a: string, b: string): number {
    const parts = (version: string) => version.split(/[.\-_+]/);
    const left = parts(a);
    const right = parts(b);
    for (let index = 0; index < Math.max(left.length, right.length); index++) {
      const x = left[index];
      const y = right[index];
      if (x === y) continue;
      if (x === undefined || y === undefined) {
        const extra = (x ?? y) as string;
        const labelled = !/^\d+$/.test(extra);
        return (x === undefined) === labelled ? 1 : -1;
      }
      const numeric = /^\d+$/.test(x) && /^\d+$/.test(y);
      const order = numeric ? Number(x) - Number(y) : x.localeCompare(y);
      if (order !== 0) return order;
    }
    return 0;
  }

  export type AvailabilityStatus = "installed" | "update" | "available";

  export interface AvailablePlugin extends PluginPresentation {
    /// Unique among offers: repository, slot and implementation.
    key: string;
    slot: string;
    kind: RepositoryChoice["kind"];
    implementation: string;
    repository: Pick<ApprovedRepositoryView, "id" | "location" | "fingerprint">;
    /// The newest version this repository offers.
    version: string;
    manifest: string;
    otherVersions: string[];
    status: AvailabilityStatus;
    /// The version this host runs, when the slot is filled.
    installedVersion: string | null;
    /// True when the host runs exactly the package this offer names.
    runningThisPackage: boolean;
  }

  /// Every plugin the enrolled repositories offer this host, newest version of
  /// each. `consoleDigest` is the interface bundle this page was loaded from.
  export function availablePlugins(
    repositories: readonly ApprovedRepositoryView[],
    selection: PackageSelectionStatus | null,
    consoleDigest: string | null,
  ): AvailablePlugin[] {
    const running = new Map((selection?.running.modules ?? []).map((module) => [module.id, module.version]));
    const offers = new Map<string, AvailablePlugin>();
    for (const repository of repositories) {
      if (repository.revoked) continue;
      for (const choice of repository.candidates) {
        const key = `${repository.id}\n${choice.slot}\n${choice.implementation}`;
        const known = offers.get(key);
        if (known && compareVersions(choice.version, known.version) <= 0) {
          if (!known.otherVersions.includes(choice.version) && choice.version !== known.version)
            known.otherVersions.push(choice.version);
          continue;
        }
        const console = choice.kind === "web_bundle";
        const installedVersion = console ? null : running.get(choice.slot) ?? null;
        const runningThisPackage = console
          ? consoleDigest !== null && consoleDigest === choice.manifest_sha256
          : selection?.running.digest === choice.manifest_sha256;
        const status: AvailabilityStatus = console
          ? runningThisPackage ? "installed" : "available"
          : installedVersion === null ? "available"
            : installedVersion === choice.version ? "installed" : "update";
        offers.set(key, {
          ...pluginPresentation(choice.slot),
          key,
          slot: choice.slot,
          kind: choice.kind,
          implementation: choice.implementation,
          repository: { id: repository.id, location: repository.location, fingerprint: repository.fingerprint },
          version: choice.version,
          manifest: choice.manifest_sha256,
          otherVersions: known ? [known.version, ...known.otherVersions] : [],
          status,
          installedVersion,
          runningThisPackage,
        });
      }
    }
    return [...offers.values()].sort((a, b) =>
      a.name.localeCompare(b.name) || a.repository.id.localeCompare(b.repository.id) || a.key.localeCompare(b.key));
  }

  export function availabilityLabel(plugin: AvailablePlugin): string {
    if (plugin.status === "installed") return "Installed";
    if (plugin.status === "update") return "Other version installed";
    return "Not installed";
  }

  /// The offers the Updates panel does not already stand for. A module set
  /// that is the running package itself, or that comes from the repository
  /// which supplied it and fills a slot the host already runs, is Amaterasu's
  /// own package: it is updated as a whole, not picked from a list.
  export function otherPublisherOffers(
    offers: readonly AvailablePlugin[],
    selection: PackageSelectionStatus | null,
  ): AvailablePlugin[] {
    const supplier = selection?.running.source?.repository ?? null;
    const running = new Set((selection?.running.modules ?? []).map((module) => module.id));
    return offers.filter((offer) => offer.kind !== "native_set" ||
      !(offer.runningThisPackage || (offer.repository.id === supplier && running.has(offer.slot))));
  }

  export interface PluginFilter { query: string; category: PluginCategory | "" }

  export function matchesPluginFilter(
    plugin: PluginPresentation & { id?: string; slot?: string; repository?: { id: string } },
    filter: PluginFilter,
  ): boolean {
    if (filter.category && plugin.category !== filter.category) return false;
    const terms = filter.query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length) return true;
    const text = [plugin.name, plugin.summary, plugin.category, plugin.id ?? "", plugin.slot ?? "", plugin.repository?.id ?? ""]
      .join(" ").toLowerCase();
    return terms.every((term) => text.includes(term));
  }

  // --- What an update changes -----------------------------------------------

  export interface ModuleChange {
    id: string;
    name: string;
    change: Exclude<ModuleChangeKind, "unchanged">;
    /// What is shown beside the name: the two versions, or the kind of change.
    detail: string;
  }

  const CHANGE_ORDER: readonly ModuleChangeKind[] = ["updated", "rebuilt", "added", "removed"];

  /// The modules an update touches: new versions first, then rebuilds,
  /// additions and removals, each by name.
  export function moduleChanges(update: PackageUpdate): ModuleChange[] {
    return update.modules.flatMap((module): ModuleChange[] => module.change === "unchanged" ? [] : [{
      id: module.id,
      name: pluginPresentation(module.id).name,
      change: module.change,
      detail: module.change === "updated" ? `${module.from ?? ""} → ${module.to ?? ""}` : module.change,
    }]).sort((a, b) => CHANGE_ORDER.indexOf(a.change) - CHANGE_ORDER.indexOf(b.change) ||
      a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
  }

  /// One line for an update: how much of the package changes and how large
  /// the download is.
  export function packageUpdateSummary(update: PackageUpdate, downloaded = false): string {
    const total = update.modules.length;
    const changed = update.modules.filter((module) => module.change !== "unchanged").length;
    return `${changed} of ${total} ${total === 1 ? "module" : "modules"} ${changed === 1 ? "changes" : "change"}` +
      ` · ${formatBytes(update.downloadBytes)} ${downloaded ? "downloaded" : "to download"}`;
  }

  const UPDATE_FAILURES: Readonly<Record<string, string>> = {
    unsupported_platform: "Updates from the console are not available on this platform yet.",
    no_source: "This host's package did not come from a repository, so there is nowhere to look for a newer one. Install Amaterasu from a published release to get updates here.",
    repository_unavailable: "The repository this host's package came from is not available. Open Repositories and check that it is still listed.",
    catalog_download_failed: "The host could not reach the repository to look for updates. Check the host's internet connection and try again.",
    catalog_rejected: "The repository's list of releases did not pass its signature check, so the host ignored it. Try again later, and tell the publisher if it keeps happening.",
    candidate_ambiguous: "The repository lists more than one package that could replace this one, so the host did not choose. Its publisher needs to correct the list; try again later.",
    manifest_download_failed: "The host found an update but could not download its description. Check the host's internet connection and try again.",
    manifest_rejected: "The update's description did not pass its signature check, so the host did not use it. Try again later, and tell the publisher if it keeps happening.",
    download_failed: "The download did not finish. Check the host's internet connection and download the update again.",
    verification_failed: "The downloaded files did not match what the publisher signed, so the host discarded them. Download the update again.",
    activation_unavailable: "The host could not begin the install. Nothing was changed. Try again, and restart Amaterasu if it keeps happening.",
    activation_failed: "The update could not be installed, and the host kept the package it was running. Try again; if it fails again, the host log in Diagnostics says why.",
    activation_interrupted: "The host stopped before the install finished and is running its previous package. Check for updates to start again.",
    shutting_down: "The host was shutting down, so the update stopped. Check for updates when it is running again.",
  };

  /// Why an update stopped, in words someone who is not a developer can act on.
  export function packageUpdateFailureMessage(code: string | null): string {
    return (code !== null && Object.hasOwn(UPDATE_FAILURES, code) && UPDATE_FAILURES[code]) ||
      "The update did not complete. Check for updates to try again.";
  }

  const UPDATE_REFUSALS: Readonly<Record<string, string>> = {
    busy: "The host is already working on an update. Wait for it to finish, or cancel it first.",
    nothing_to_download: "There is no update to download. Check for updates first.",
    not_ready: "The update has not finished downloading, so it cannot be installed yet.",
    activation_in_progress: "The host is already installing this update.",
    shutting_down: "The host is shutting down. Try again when it is running again.",
    unsupported_platform: "Updates from the console are not available on this platform yet.",
    permission_denied: "This account is not allowed to do that. Installing an update needs an administrator who is signed in to the console.",
    not_restorable: "The host can no longer switch to that package. Open the list again to see what it still keeps.",
    automatic_updates_on: "Turn automatic updates off before switching to that package. It is from before automatic updates and cannot read the setting, which would lock this console out.",
    invalid_mode: "The host did not recognise that setting. Reload the page and try again.",
  };

  /// Why the host refused a request, or null for a code this console does not know.
  export function packageUpdateRefusalMessage(code: string | null): string | null {
    return (code !== null && Object.hasOwn(UPDATE_REFUSALS, code) && UPDATE_REFUSALS[code]) || null;
  }

  // --- The Updates panel's rows ---------------------------------------------

  /// The requests a row can send for its source.
  export type UpdateAction = "check" | "download" | "activate" | "cancel";

  /// What the page asks a source to change, beyond the update requests.
  export type UpdateChange =
    | { kind: "automatic"; mode: AutomaticMode }
    | { kind: "restore"; digest: string };

  /// Where a source's update stands. The last three are not phases a host
  /// reports: its answer has not arrived, could not be read, or says updates
  /// cannot be installed from the console.
  export type UpdateSourceState = PackageUpdatePhase | "loading" | "unreadable" | "unavailable";

  export type UpdateTone = "good" | "attention" | "danger" | "neutral";

  export interface UpdateStep { label: string; state: "done" | "current" | "upcoming" }

  /// One row of the Updates panel: something the host can bring up to date,
  /// and everything the row shows about it. Amaterasu Core is the only source
  /// a host reports today; a module from another publisher would be one more.
  export interface UpdateSourceView {
    id: string;
    name: string;
    /// What the source is, for the row's closing sentence.
    about: string;
    /// The repository it is published from, when the host knows.
    repository: string | null;
    /// The digest of what the host runs now.
    running: string | null;
    /// The version its repository gives what the host runs, when it lists it.
    version: string | null;
    checkedUnixMs: number | null;
    state: UpdateSourceState;
    /// The state in words: the row's answer to "is this up to date?".
    headline: string;
    tone: UpdateTone;
    /// A sentence for a state that needs explaining.
    detail: TextValue | null;
    /// The update the row describes, so a redraw can tell a new one from the same one.
    update: string | null;
    /// How much the update changes, and the modules it touches.
    summary: string | null;
    changes: ModuleChange[];
    /// The stages of an update under way; empty when none is.
    steps: UpdateStep[];
    /// Bytes received while downloading; `total` is null when the host gave none.
    transfer: { received: number; total: number | null } | null;
    /// The requests that make sense in this state.
    actions: UpdateAction[];
    /// True while the host is working and the row should be read again.
    busy: boolean;
    /// How the source keeps itself up to date, when its host can.
    automatic: AutomaticUpdates | null;
    /// Whether the host lists the source's earlier packages and can switch to one.
    versions: boolean;
    /// The earlier package being switched to, when one is.
    restoring: string | null;
  }

  /// What the page has heard about a source: nothing yet, the host's answer,
  /// no such routes on this host, or a request that failed.
  export type UpdateReading =
    | { state: "loading" }
    | { state: "absent" }
    | { state: "failed"; message: string }
    | { state: "ready"; status: PackageUpdateStatus };

  export const CORE_UPDATE_SOURCE = "amaterasu-core";

  const CORE_UPDATE_ABOUT = "Every module Amaterasu ships. They update together as one signed package, and the host goes back to the package it had if a new one does not start.";

  const UPDATE_STEP_LABELS = ["Download", "Verify", "Wait for idle", "Restart"] as const;
  const RESTORE_STEP_LABELS = ["Wait for idle", "Restart"] as const;

  const UPDATE_HEADLINES: Readonly<Record<PackageUpdatePhase, string>> = {
    idle: "Not checked yet",
    checking: "Checking for updates",
    current: "Up to date",
    available: "Update available",
    downloading: "Downloading update",
    ready: "Ready to install",
    waiting_stream: "Waiting for the stream to end",
    activating: "Installing",
    failed: "Update failed",
    cancelled: "Update cancelled",
  };

  const UPDATE_DETAILS: Readonly<Partial<Record<PackageUpdatePhase, string>>> = {
    ready: "Downloaded and verified. Once you choose to install it, the host restarts as soon as no stream is running.",
    waiting_stream: "The host installs the update and restarts as soon as the stream that is running ends.",
    activating: "The host is restarting with the new package. This page follows along and reconnects when it is back.",
    cancelled: "Nothing was installed. The host is still running the same package.",
  };

  /// How far an update has come: `done` stages finished, and the one in
  /// progress when there is one. Null outside an update.
  function updateProgress(phase: PackageUpdatePhase, received: number, total: number | null): { done: number; current: number | null } | null {
    switch (phase) {
      case "available": return { done: 0, current: null };
      // The host verifies what it downloaded before it reports the update ready.
      case "downloading": return total !== null && received >= total ? { done: 1, current: 1 } : { done: 0, current: 0 };
      case "ready": return { done: 2, current: null };
      case "waiting_stream": return { done: 2, current: 2 };
      case "activating": return { done: 3, current: 3 };
      default: return null;
    }
  }

  /// The Amaterasu Core row. `running` is what the package selection says the
  /// host runs, used when the host's update status says nothing.
  export function coreUpdateSource(
    reading: UpdateReading,
    running: { digest: string | null; repository: string | null; version?: string | null } | null,
  ): UpdateSourceView {
    const row: UpdateSourceView = {
      id: CORE_UPDATE_SOURCE,
      name: "Amaterasu Core",
      about: CORE_UPDATE_ABOUT,
      repository: running?.repository ?? null,
      running: running?.digest ?? null,
      version: running?.version ?? null,
      checkedUnixMs: null,
      state: "loading",
      headline: "Loading…",
      tone: "neutral",
      detail: null,
      update: null,
      summary: null,
      changes: [],
      steps: [],
      transfer: null,
      actions: [],
      busy: false,
      automatic: null,
      versions: false,
      restoring: null,
    };
    if (reading.state === "loading") return row;
    if (reading.state === "failed")
      return { ...row, state: "unreadable", headline: "Status unavailable", detail: { kind: "literal", value: reading.message } };
    if (reading.state === "absent" || !reading.status.supported)
      return {
        ...row,
        state: "unavailable",
        headline: "Updated outside the console",
        detail: reading.state === "absent"
          ? "This host does not install updates from the console yet."
          : "Updates from the console are not available on this platform yet.",
      };
    const status = reading.status;
    const phase = status.phase;
    const shared = {
      ...row,
      repository: status.source?.repository ?? row.repository,
      running: status.running.digest ?? row.running,
      checkedUnixMs: status.checkedUnixMs,
      state: phase,
      automatic: status.automatic,
      versions: status.versions,
    };
    // Switching to an earlier package borrows the phases of an install.
    if (status.restoring && (phase === "waiting_stream" || phase === "activating")) {
      const current = phase === "waiting_stream" ? 0 : 1;
      return {
        ...shared,
        headline: phase === "waiting_stream" ? "Waiting for the stream to end" : "Switching version",
        tone: "attention",
        detail: phase === "waiting_stream"
          ? "The host switches to the earlier package and restarts as soon as the stream that is running ends."
          : "The host is restarting with the earlier package. This page follows along and reconnects when it is back.",
        restoring: status.restoring.digest,
        steps: RESTORE_STEP_LABELS.map((label, index) => ({
          label,
          state: index === current ? "current" : index < current ? "done" : "upcoming",
        })),
        actions: phase === "waiting_stream" ? ["cancel"] : [],
        busy: true,
      };
    }
    // Before a check has finished, or after one found nothing, there is no update to describe.
    const update = phase === "idle" || phase === "checking" || phase === "current" ? null : status.update;
    const total = update && update.downloadBytes > 0 ? update.downloadBytes : null;
    const received = total === null ? status.transferredBytes : Math.min(status.transferredBytes, total);
    const progress = updateProgress(phase, received, total);
    const settled = phase === "failed" || phase === "cancelled";
    return {
      ...shared,
      headline: UPDATE_HEADLINES[phase],
      tone: phase === "current" ? "good" : phase === "failed" ? "danger"
        : phase === "idle" || phase === "checking" || phase === "cancelled" ? "neutral" : "attention",
      detail: phase === "failed" ? packageUpdateFailureMessage(status.error)
        : phase === "current" && status.repositoryBehind
          ? "Nothing newer is published. This host runs a later package than its repository lists, as it does after a local install."
          : UPDATE_DETAILS[phase] ?? null,
      update: update?.digest ?? null,
      summary: update
        ? packageUpdateSummary(update, phase === "ready" || phase === "waiting_stream" || phase === "activating")
        : null,
      changes: update ? moduleChanges(update) : [],
      steps: progress ? UPDATE_STEP_LABELS.map((label, index) => ({
        label,
        state: index === progress.current ? "current" : index < progress.done ? "done" : "upcoming",
      })) : [],
      transfer: phase === "downloading" ? { received, total } : null,
      actions: phase === "available" || (settled && update) ? ["download", "check"]
        : phase === "ready" ? ["activate", "check"]
        : phase === "downloading" || phase === "waiting_stream" ? ["cancel"]
        : phase === "checking" || phase === "activating" ? []
        : ["check"],
      busy: phase === "checking" || phase === "downloading" || phase === "waiting_stream" || phase === "activating",
    };
  }

  // --- The page's one-line answer ---------------------------------------------

  export interface UpdatesSummary {
    headline: string;
    tone: UpdateTone;
    /// The most recent check among the sources that report one.
    checkedUnixMs: number | null;
  }

  const UPDATE_PENDING: readonly UpdateSourceState[] = ["available", "downloading", "ready", "waiting_stream", "activating"];

  /// What every source adds up to: the answer to "is this host up to date?".
  /// Trouble comes first, then an update under way, then what is still unknown.
  export function updatesSummary(sources: readonly UpdateSourceView[]): UpdatesSummary {
    const checked = sources.flatMap((source) => source.checkedUnixMs === null ? [] : [source.checkedUnixMs]);
    const checkedUnixMs = checked.length ? Math.max(...checked) : null;
    const count = (states: readonly UpdateSourceState[]): number =>
      sources.filter((source) => states.includes(source.state)).length;
    const answer = (headline: string, tone: UpdateTone): UpdatesSummary => ({ headline, tone, checkedUnixMs });
    if (!sources.length || count(["loading"]) === sources.length) return answer("Loading…", "neutral");
    const switching = sources.filter((source) => source.restoring !== null).length;
    const pending = sources.filter((source) =>
      source.restoring === null && (UPDATE_PENDING.includes(source.state) || (source.state === "cancelled" && source.update !== null))).length;
    if (count(["failed"])) return answer(count(["failed"]) === 1 ? "An update needs attention" : "Some updates need attention", "danger");
    if (switching) return answer("Switching to an earlier version", "attention");
    if (pending) return answer(pending === 1 ? "1 update available" : `${pending} updates available`, "attention");
    if (count(["checking"])) return answer("Checking for updates…", "neutral");
    if (count(["unreadable"])) return answer("Update status is unavailable", "neutral");
    if (count(["current"]) === sources.length) return answer("Everything is up to date", "good");
    if (count(["unavailable"]) === sources.length) return answer("This host is updated outside the console", "neutral");
    return answer("Updates have not been checked yet", "neutral");
  }

  /// How long until something happens, in the largest whole unit that fits.
  /// Null once the time has come.
  export function timeUntil(nowMs: number, thenMs: number): { value: number; unit: "minute" | "hour" | "day" } | null {
    const minutes = Math.ceil((thenMs - nowMs) / 60_000);
    if (minutes < 1) return null;
    if (minutes < 60) return { value: minutes, unit: "minute" };
    const hours = Math.round(minutes / 60);
    return hours < 24 ? { value: hours, unit: "hour" } : { value: Math.round(hours / 24), unit: "day" };
  }

  /// How long ago something happened, in the largest whole unit that fits.
  /// Null within the last minute, and for a time the clock has not reached.
  export function elapsedSince(nowMs: number, thenMs: number): { value: number; unit: "minute" | "hour" | "day" } | null {
    const minutes = Math.floor((nowMs - thenMs) / 60_000);
    if (minutes < 1) return null;
    if (minutes < 60) return { value: minutes, unit: "minute" };
    const hours = Math.floor(minutes / 60);
    return hours < 24 ? { value: hours, unit: "hour" } : { value: Math.floor(hours / 24), unit: "day" };
  }

  // --- Small formatters -----------------------------------------------------

  export function shortDigest(digest: string): string {
    return digest.length > 12 ? digest.slice(0, 12) : digest;
  }

  /// A fingerprint in groups of four, which is how people compare them.
  export function groupedFingerprint(fingerprint: string): string {
    return fingerprint.match(/.{1,4}/g)?.join(" ") ?? fingerprint;
  }

  export function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    const units = ["KB", "MB", "GB"];
    let value = bytes / 1024;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
    return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
  }

  export interface RepositorySummary {
    /// One line for the repository's row.
    status: string;
    /// Set when the repository needs the administrator's attention.
    attention: boolean;
  }

  export function summarizeRepository(
    repository: ApprovedRepositoryView,
    formatDate: (unixSeconds: number) => string,
  ): RepositorySummary {
    if (repository.revoked)
      return { status: "Removed. This ID cannot be added again.", attention: false };
    const count = repository.candidates.length;
    const offers = count === 1 ? "1 plugin" : `${count} plugins`;
    const catalog = repository.catalog;
    switch (catalog.state) {
      case "current":
        return { status: `${offers} · catalog valid until ${formatDate(catalog.expiresUnix)}`, attention: false };
      case "expired":
        return { status: `Catalog expired ${formatDate(catalog.expiresUnix)}. Check for updates to list its plugins again.`, attention: true };
      case "missing":
        return { status: "No catalog yet. Check for updates to list its plugins.", attention: true };
      case "unavailable":
        return { status: "The saved catalog no longer verifies. Check for updates to replace it.", attention: true };
      default:
        return { status: count ? offers : "No plugins listed for this host", attention: false };
    }
  }

  const REPOSITORY_MESSAGES: Readonly<Record<string, string>> = {
    repository_busy: "Another repository change is still running. Try again in a moment.",
    shutting_down: "The host is shutting down.",
    repository_management_unavailable: "This host cannot change its repositories.",
    repository_worker_unavailable: "The host could not start the change.",
    repository_state_unavailable: "The host could not read or save its repository list.",
    invalid_request: "The host did not accept that request.",
    invalid_repository_id: "Enter the repository ID exactly as its publisher writes it: lowercase letters, digits, dots, dashes and underscores.",
    invalid_repository_location: "Enter an https:// address with no query or fragment.",
    invalid_publisher_key: "The publisher key is 64 hexadecimal characters.",
    fingerprint_mismatch: "That fingerprint does not belong to this publisher key. Check both against what the publisher gave you.",
    repository_exists: "A repository with this ID was already added. An ID that was removed cannot be added again.",
    repository_limit: "This host cannot hold any more repositories.",
    repository_unknown: "This repository is no longer enrolled.",
    repository_in_use: "The package this host is running came from this repository. Removing it would stop the host from starting again.",
    repository_serves_console: "This console is signed by this repository's publisher. Removing it would lock you out of the console.",
    catalog_download_failed: "The host could not download a catalog from that address.",
    catalog_invalid: "That address did not return a repository catalog.",
    catalog_identity_mismatch: "The catalog at that address belongs to a different repository ID.",
    catalog_wrong_platform: "That repository publishes for a different platform than this host.",
    catalog_expired: "That repository's catalog has expired. Its publisher needs to publish a new one.",
    catalog_rejected: "The catalog is not signed by this publisher key, or it is older than one this host already accepted.",
  };

  export function repositoryChangeMessage(code: string | null): string {
    return (code && REPOSITORY_MESSAGES[code]) || "The repository change did not complete.";
  }

  /// The form field a refused change points at, if any.
  export function repositoryChangeField(code: string | null): "id" | "location" | "publicKey" | "fingerprint" | null {
    switch (code) {
      case "invalid_repository_id": case "repository_exists": case "catalog_identity_mismatch": return "id";
      case "invalid_repository_location": case "catalog_download_failed": case "catalog_invalid": return "location";
      case "invalid_publisher_key": case "catalog_rejected": return "publicKey";
      case "fingerprint_mismatch": return "fingerprint";
      default: return null;
    }
  }

  export interface RepositoryDescriptor { id?: string; location?: string; publicKey?: string }

  /// Reads the details a publisher hands out as one block of JSON. The key
  /// fingerprint is deliberately not taken from it: the administrator confirms
  /// that separately.
  export function parseRepositoryDescriptor(text: string): RepositoryDescriptor | null {
    const trimmed = text.trim();
    if (!trimmed.startsWith("{") || trimmed.length > 4096) return null;
    let parsed: unknown;
    try { parsed = JSON.parse(trimmed); } catch { return null; }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    const source = parsed as Record<string, unknown>;
    const field = (...names: string[]): string | undefined => {
      for (const name of names) {
        const value = source[name];
        if (typeof value === "string" && value.trim() && value.length <= 512) return value.trim();
      }
      return undefined;
    };
    const id = field("id", "repository");
    const location = field("location", "url");
    const publicKey = field("public_key", "publicKey", "key");
    if (!id && !location && !publicKey) return null;
    return { ...(id ? { id } : {}), ...(location ? { location } : {}), ...(publicKey ? { publicKey: publicKey.toLowerCase() } : {}) };
  }

  /// The interface bundle digest a versioned asset address names, if it has one.
  export function consoleDigestFromAssets(addresses: readonly string[]): string | null {
    for (const address of addresses) {
      const digest = /\/web\/([a-f0-9]{64})\//.exec(address)?.[1];
      if (digest) return digest;
    }
    return null;
  }
}
