namespace Amaterasu {
  // What the management API reports about packages, modules and repositories,
  // and the plugin manager's view of it. Nothing here touches the document.

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
