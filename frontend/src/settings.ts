namespace Amaterasu {
  const maxConfiguredAuthTtlSeconds = 0x7fff_ffff;
  const maxInputKeyRepeatDelayMs = 0x7fff_ffff;
  const minInputBackButtonTimeoutMs = -0x8000_0000;
  const maxInputBackButtonTimeoutMs = 0x7fff_ffff;
  const maxHistoryLimit = 0x7fff_ffff;

  export function parseVirtualSinkSelection(
    enabled: boolean,
    selector: string,
  ): string | null {
    if (typeof enabled !== "boolean" || typeof selector !== "string")
      throw new Error("Virtual sink selection is invalid.");
    if (new TextEncoder().encode(selector).length > 4096 || /[\u0000-\u001f\u007f-\u009f]/u.test(selector))
      throw new Error("Virtual sink selector must be at most 4096 UTF-8 bytes without control characters.");
    return enabled ? selector : null;
  }

  export function parseHistorySettings(
    enabled: boolean,
    ttlDays: number,
    sizeLimitMb: number,
  ): HistorySettingsReply["settings"] {
    if (
      typeof enabled !== "boolean" ||
      !Number.isSafeInteger(ttlDays) ||
      ttlDays < 0 ||
      ttlDays > maxHistoryLimit ||
      !Number.isSafeInteger(sizeLimitMb) ||
      sizeLimitMb < 0 ||
      sizeLimitMb > maxHistoryLimit
    )
      throw new Error("History limits must be whole numbers from 0 to 2147483647.");
    return { enabled, ttl_days: ttlDays, db_size_limit_mb: sizeLimitMb };
  }

  export function parseAuthTokenLifetimes(
    sessionTokenTtlSeconds: number,
    rememberMeRefreshTokenTtlSeconds: number,
  ): AuthTokenSettings["values"] {
    if (
      !Number.isSafeInteger(sessionTokenTtlSeconds) ||
      sessionTokenTtlSeconds < 1 ||
      sessionTokenTtlSeconds > maxConfiguredAuthTtlSeconds ||
      !Number.isSafeInteger(rememberMeRefreshTokenTtlSeconds) ||
      rememberMeRefreshTokenTtlSeconds < 1 ||
      rememberMeRefreshTokenTtlSeconds > maxConfiguredAuthTtlSeconds
    )
      throw new Error("Authentication lifetimes must be whole seconds from 1 to 2147483647.");
    return {
      session_token_ttl_seconds: sessionTokenTtlSeconds,
      remember_me_refresh_token_ttl_seconds: rememberMeRefreshTokenTtlSeconds,
    };
  }

  export function parseInputPolicy(
    keyboard: boolean,
    mouse: boolean,
    highResolutionScrolling: boolean,
    alwaysSendScancodes: boolean,
    keyRightAltToKeyWin: boolean,
    keyRepeatDelay: number,
    keyRepeatFrequency: number,
    backButtonTimeout: number = -1,
    gamepadProfile: string = "auto",
    selectableGamepadProfiles: readonly string[] = ["auto"],
    controllerEnabled: boolean = true,
  ): InputPolicySettings["values"] {
    if (
      typeof keyboard !== "boolean" ||
      typeof mouse !== "boolean" ||
      typeof highResolutionScrolling !== "boolean" ||
      typeof alwaysSendScancodes !== "boolean" ||
      typeof keyRightAltToKeyWin !== "boolean" ||
      !Number.isSafeInteger(keyRepeatDelay) ||
      keyRepeatDelay < 0 ||
      keyRepeatDelay > maxInputKeyRepeatDelayMs ||
      !Number.isFinite(keyRepeatFrequency) ||
      keyRepeatFrequency < 0 ||
      !Number.isSafeInteger(backButtonTimeout) ||
      backButtonTimeout < minInputBackButtonTimeoutMs ||
      backButtonTimeout > maxInputBackButtonTimeoutMs ||
      typeof gamepadProfile !== "string" ||
      !Array.isArray(selectableGamepadProfiles) ||
      !selectableGamepadProfiles.includes(gamepadProfile) ||
      typeof controllerEnabled !== "boolean"
    )
      throw new Error("Input policy values, key repeat settings, or Back timeout are invalid.");
    return {
      keyboard,
      mouse,
      high_resolution_scrolling: highResolutionScrolling,
      always_send_scancodes: alwaysSendScancodes,
      key_rightalt_to_key_win: keyRightAltToKeyWin,
      key_repeat_delay: keyRepeatDelay,
      key_repeat_frequency: keyRepeatFrequency,
      back_button_timeout: backButtonTimeout,
      gamepad: gamepadProfile,
      controller: controllerEnabled,
    };
  }

  export interface GamepadProfileChoice {
    value: string;
    label: string;
    available: boolean;
  }

  export function inputGamepadProfileChoices(
    selectedProfile: string,
    supportedProfiles: readonly string[],
  ): GamepadProfileChoice[] {
    if (
      typeof selectedProfile !== "string" ||
      !Array.isArray(supportedProfiles) ||
      !supportedProfiles.every((profile) => typeof profile === "string")
    )
      throw new Error("Gamepad profile status is invalid.");
    const availableProfiles = [...new Set(supportedProfiles)];
    if (availableProfiles.length === 0) availableProfiles.push("auto");
    const choices = availableProfiles.map((value) => ({
      value,
      label: value,
      available: true,
    }));
    if (selectedProfile !== "" && !availableProfiles.includes(selectedProfile)) {
      choices.push({
        value: selectedProfile,
        label: `${selectedProfile} (recognized, unavailable on this backend)`,
        available: false,
      });
    }
    return choices;
  }

  export function inputGamepadStatusDescription(
    selectedProfile: string,
    status: InputRuntimeStatus,
  ): string {
    if (!status || !Array.isArray(status.supported_profiles))
      return "Current gamepad backend status is unavailable.";
    if (!status.controller_enabled)
      return status.controller_cleanup_required
        ? "Controller input is disabled; virtual gamepad removal is retrying. Keyboard and mouse input remain available."
        : "Controller input is disabled by policy. Keyboard and mouse input remain available.";
    const isSupported = status.supported_profiles.includes(selectedProfile);
    if (!isSupported) {
      const reason =
        status.gamepad === selectedProfile &&
        typeof status.gamepad_unavailable_reason === "string" &&
        status.gamepad_unavailable_reason !== ""
          ? status.gamepad_unavailable_reason
          : null;
      return reason
        ? `The selected ${selectedProfile} profile is recognized but unavailable: ${reason}`
        : `The selected ${selectedProfile} profile is recognized but unavailable on this backend.`;
    }
    if (status.lease === null)
      return `${selectedProfile} is supported by this backend; device availability is checked when a stream starts.`;
    const reason =
      status.gamepad === selectedProfile &&
      typeof status.gamepad_unavailable_reason === "string" &&
      status.gamepad_unavailable_reason !== ""
        ? status.gamepad_unavailable_reason
        : null;
    if (status.gamepad === selectedProfile && !status.controller_input)
      return reason
        ? `The ${selectedProfile} profile is selected, but controller input is currently unavailable: ${reason}`
        : `The ${selectedProfile} profile is selected, but controller input is currently unavailable.`;
    return `${selectedProfile} is supported by the current input backend.`;
  }

  export function parseClientFecOverride(value: string): {
    fec_percentage: number | null;
  } {
    const trimmed = value.trim();
    if (trimmed === "") return { fec_percentage: null };
    if (!/^(?:[1-9][0-9]{0,2})$/.test(trimmed))
      throw new Error("FEC percentage must be a whole number from 1 to 255, or blank to inherit.");
    const percentage = Number(trimmed);
    if (percentage > 255)
      throw new Error("FEC percentage must be a whole number from 1 to 255, or blank to inherit.");
    return { fec_percentage: percentage };
  }

  export function inputPolicyStatusDescription(
    settings: InputPolicySettings,
  ): string {
    if (!settings.persisted)
      return "The saved input policy has not been confirmed yet.";
    if (!settings.applied || settings.cleanup_required)
      return "The saved policy is active for new events, but cleanup of held native input is still pending.";
    return "The saved keyboard, mouse, and gamepad policy is active.";
  }

  function safeNvhttpBindFamily(value: string): "ipv4" | "ipv6" | null {
    if (value === "") return null;
    const octets = value.split(".");
    if (octets.length === 4) {
      const parsed = octets.map((octet) => Number(octet));
      if (
        octets.every((octet, index) => {
          const number = parsed[index];
          return (
            number !== undefined &&
            /^(0|[1-9][0-9]{0,2})$/.test(octet) &&
            number <= 255 &&
            String(number) === octet
          );
        })
      ) {
        const firstOctet = parsed[0];
        if (firstOctet !== undefined && firstOctet >= 224 && firstOctet <= 239)
          throw new Error("NVHTTP bind address must not be multicast or broadcast.");
        if (value === "255.255.255.255")
          throw new Error("NVHTTP bind address must not be multicast or broadcast.");
        return "ipv4";
      }
    }
    if (!value.includes(":") || /[%\[\]\s]/.test(value)) return null;
    try {
      const hostname = new URL(`http://[${value}]/`).hostname.toLowerCase();
      if (!hostname.startsWith("[") || !hostname.endsWith("]")) return null;
      if (hostname.startsWith("[ff") || hostname.startsWith("[::ffff:"))
        throw new Error("NVHTTP bind address must not be multicast or IPv4-mapped IPv6.");
      return "ipv6";
    } catch (error) {
      if (error instanceof Error && error.message.startsWith("NVHTTP bind address"))
        throw error;
      return null;
    }
  }

  export function parseNvhttpNetworkOverrides(
    addressFamilySelection: string,
    useStartupBindAddress: boolean,
    bindAddress: string,
  ): {
    address_family: "ipv4" | "both" | null;
    bind_address: string | null;
  } {
    const addressFamily =
      addressFamilySelection === "startup"
        ? null
        : addressFamilySelection === "ipv4" || addressFamilySelection === "both"
          ? addressFamilySelection
          : (() => {
              throw new Error("Choose IPv4, both address families, or startup policy.");
            })();
    let configuredBindAddress: string | null = null;
    if (!useStartupBindAddress) {
      const bindFamily = safeNvhttpBindFamily(bindAddress);
      if (bindAddress !== "" && bindFamily === null)
        throw new Error("Enter a valid IPv4 or IPv6 address literal, or leave it blank.");
      if (addressFamily === "ipv4" && bindFamily === "ipv6")
        throw new Error("An IPv6 bind address requires both address families.");
      configuredBindAddress = bindAddress;
    }
    return {
      address_family: addressFamily,
      bind_address: configuredBindAddress,
    };
  }

  export function parseOutputNameSelection(value: string): string {
    if (
      new TextEncoder().encode(value).length > 512 ||
      value.trim() !== value ||
      /[\u0000-\u001f\u007f-\u009f]/.test(value)
    )
      throw new Error("The capture output ID must have at most 512 UTF-8 bytes without surrounding whitespace or control characters.");
    return value;
  }

  export function parseCaptureSelection(value: string): NvhttpSettings["configured_capture"] {
    if (value === "auto" || value === "ddx" || value === "wgc" || value === "wgcc")
      return value;
    throw new Error("Choose Automatic, Desktop Duplication, or a Windows Graphics Capture mode.");
  }

  export function parseVideoMaxBatchSize(value: string): number | null {
    if (value.trim() === "") return null;
    if (!/^(0|[1-9][0-9]*)$/.test(value))
      throw new Error("Video batch size must be a whole number from 0 to 64 KiB.");
    const size = Number(value);
    if (!Number.isSafeInteger(size) || size > 64)
      throw new Error("Video batch size must be a whole number from 0 to 64 KiB.");
    return size;
  }

  export function parseMaxBitrate(value: string): number | null {
    if (value.trim() === "") return null;
    if (!/^(0|-?[1-9][0-9]*)$/.test(value))
      throw new Error("Maximum bitrate must be a signed 32-bit whole number in kbps.");
    const bitrate = Number(value);
    if (!Number.isInteger(bitrate) || bitrate < -2_147_483_648 || bitrate > 2_147_483_647)
      throw new Error("Maximum bitrate must be a signed 32-bit whole number in kbps.");
    return bitrate;
  }

  export function parseMinimumFpsTarget(value: string): number | null {
    if (value.trim() === "") return null;
    if (!/^(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(value))
      throw new Error("Minimum frame rate must be a finite number from 0 to 1000, or blank to use 20.");
    const target = Number(value);
    if (!Number.isFinite(target) || target < 0 || target > 1000)
      throw new Error("Minimum frame rate must be a finite number from 0 to 1000, or blank to use 20.");
    return target;
  }

  export function parsePingTimeout(value: string): number {
    if (value.trim() === "") return -1;
    if (!/^(0|[1-9][0-9]*)$/.test(value))
      throw new Error("Ping timeout must be a whole number from 0 to 2147483647 milliseconds, or blank to reset.");
    const timeout = Number(value);
    if (!Number.isSafeInteger(timeout) || timeout > 2_147_483_647)
      throw new Error("Ping timeout must be a whole number from 0 to 2147483647 milliseconds, or blank to reset.");
    return timeout;
  }

  export function parseAudioSinkSelection(value: string): string | null {
    if (value === "") return null;
    if (new TextEncoder().encode(value).length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(value))
      throw new Error("The audio endpoint ID must contain at most 4096 UTF-8 bytes and no control characters.");
    return value;
  }

  export function parseExternalIp(value: string): string | null {
    if (value === "") return null;
    if (new TextEncoder().encode(value).length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(value))
      throw new Error("External IP text must contain at most 4096 UTF-8 bytes and no control characters.");
    return value;
  }

  export function parseWgcPacingSmoothing(value: string): boolean | null {
    if (value === "") return null;
    if (value === "true") return true;
    if (value === "false") return false;
    throw new Error("Choose the default, enabled, or disabled WGC pacing policy.");
  }

  export function parseSteamExclusions(value: string): SteamExclusion[] {
    let parsed: unknown;
    try {
      parsed = JSON.parse(value) as unknown;
    } catch {
      throw new Error("Excluded games must be valid JSON.");
    }
    if (!Array.isArray(parsed) || parsed.length > 256)
      throw new Error("Excluded games must be a JSON array with at most 256 entries.");

    const ids = new Set<string>();
    const names = new Set<string>();
    for (const item of parsed) {
      if (typeof item !== "object" || item === null || Array.isArray(item))
        throw new Error("Each excluded game must be an object.");
      const exclusion = item as Record<string, unknown>;
      if (Object.keys(exclusion).some((key) => key !== "id" && key !== "name"))
        throw new Error("Excluded games may only contain id and name fields.");
      const id = exclusion.id;
      const name = exclusion.name;
      if (id === undefined && name === undefined)
        throw new Error("Each excluded game needs an app ID or name.");

      if (id !== undefined) {
        if (
          typeof id !== "string" ||
          !/^[1-9][0-9]*$/.test(id) ||
          !Number.isSafeInteger(Number(id)) ||
          Number(id) > 0xffff_ffff ||
          String(Number(id)) !== id
        )
          throw new Error("Steam app IDs must be canonical nonzero decimal strings.");
        if (ids.has(id)) throw new Error("Excluded Steam app IDs must be unique.");
        ids.add(id);
      }

      if (name !== undefined) {
        if (
          typeof name !== "string" ||
          name.trim().length === 0 ||
          new TextEncoder().encode(name).length > 512 ||
          /[\u0000-\u001f\u007f-\u009f]/.test(name)
        )
          throw new Error("Exclusion names must contain 1–512 printable UTF-8 bytes.");
        if (id === undefined) {
          const normalized = name.toLowerCase();
          if (names.has(normalized))
            throw new Error("Name-only exclusions must be unique, ignoring case.");
          names.add(normalized);
        }
      }
    }
    return parsed as SteamExclusion[];
  }

  export function parseLutrisExclusions(value: string): LutrisExclusion[] {
    let parsed: unknown;
    try {
      parsed = JSON.parse(value) as unknown;
    } catch {
      throw new Error("Lutris exclusions must be valid JSON.");
    }
    if (!Array.isArray(parsed) || parsed.length > 256)
      throw new Error("Lutris exclusions must be a JSON array with at most 256 entries.");

    const identities = new Set<string>();
    const validateId = (id: string): void => {
      if (
        id.length === 0 ||
        new TextEncoder().encode(id).length > 512 ||
        !/^[0-9]+$/.test(id) ||
        BigInt(id) === 0n ||
        BigInt(id) > 18_446_744_073_709_551_615n
      )
        throw new Error("Lutris exclusion IDs must be positive decimal integers.");
    };
    const validateName = (name: string): void => {
      if (
        name.trim().length === 0 ||
        new TextEncoder().encode(name).length > 512 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(name)
      )
        throw new Error("Lutris exclusion names must be printable text of at most 512 UTF-8 bytes.");
    };
    const unique = (identity: string): void => {
      if (identities.has(identity))
        throw new Error("Lutris exclusion IDs and name-only entries must be unique.");
      identities.add(identity);
    };

    for (const item of parsed) {
      if (typeof item === "string") {
        validateId(item);
        unique(`id:${item}`);
        continue;
      }
      if (typeof item !== "object" || item === null || Array.isArray(item))
        throw new Error("Each Lutris exclusion must be an ID string or object.");
      const exclusion = item as Record<string, unknown>;
      if (Object.keys(exclusion).some((key) => key !== "id" && key !== "name"))
        throw new Error("Lutris exclusions may only contain id and name fields.");
      const id = exclusion.id;
      const name = exclusion.name;
      if (id === undefined && name === undefined)
        throw new Error("Each Lutris exclusion needs an ID or name.");
      if (id !== undefined && typeof id !== "string")
        throw new Error("Lutris exclusion IDs must be strings.");
      if (name !== undefined && typeof name !== "string")
        throw new Error("Lutris exclusion names must be strings.");

      if (typeof id === "string" && id !== "") {
        validateId(id);
        if (typeof name === "string" && name !== "") validateName(name);
        unique(`id:${id}`);
      } else if (typeof name === "string") {
        validateName(name);
        unique(`name:${name.toLowerCase()}`);
      } else {
        throw new Error("Each Lutris exclusion needs a positive ID or nonempty name.");
      }
    }

    const maximumSettings = {
      enabled: true,
      auto_sync: true,
      autosync_remove_uninstalled: true,
      include_steam: true,
      exclude_games: parsed,
    };
    const maximumUpdate = {
      ...maximumSettings,
      expected_revision: Number.MAX_SAFE_INTEGER,
    };
    const encoder = new TextEncoder();
    if (
      encoder.encode(JSON.stringify(maximumSettings)).length > 12_000 ||
      encoder.encode(JSON.stringify(maximumUpdate)).length > 12_000
    )
      throw new Error("Lutris exclusions exceed the product settings or management request size limit.");
    return parsed as LutrisExclusion[];
  }

  export function lutrisStatusDescription(status: LutrisStatus): string {
    if (!status.settings_ready)
      return "Lutris settings are still loading; discovery state is not confirmed.";
    if (!status.platform_supported)
      return "Lutris discovery is supported only on Linux and is inactive on this platform.";
    if (!status.enabled)
      return "Lutris discovery is disabled.";
    if (!status.database_available)
      return "The current user's Lutris game database is unavailable.";
    if (!status.executable_available)
      return "The Lutris executable was not found in the current user's PATH.";
    if (status.last_error)
      return `Lutris discovery is available · ${status.game_count} games · Last scan issue: ${status.last_error}`;
    const pending = status.refresh_pending ? "A catalog scan is in progress." : "No catalog scan is pending.";
    return `Lutris discovery is available · ${status.game_count} games · ${pending}`;
  }

  export function parsePlayniteFilterList(source: string, label: string): string[] {
    let parsed: unknown;
    try {
      parsed = JSON.parse(source);
    } catch {
      throw new Error(`${label} must be a JSON array of strings.`);
    }
    return validatePlayniteFilterList(parsed, label);
  }

  export function parsePlayniteSettings(
    values: PlayniteSettings["values"],
  ): PlayniteSettings["values"] {
    if (
      typeof values.enabled !== "boolean" ||
      typeof values.auto_sync !== "boolean" ||
      typeof values.sync_all_installed !== "boolean" ||
      typeof values.autosync_remove_uninstalled !== "boolean" ||
      typeof values.autosync_require_replacement !== "boolean" ||
      typeof values.focus_exit_on_first !== "boolean" ||
      typeof values.fullscreen_entry_enabled !== "boolean" ||
      !Number.isSafeInteger(values.autosync_delete_after_days) ||
      values.autosync_delete_after_days < 0 ||
      values.autosync_delete_after_days > 2_147_483_647 ||
      !Number.isSafeInteger(values.focus_attempts) ||
      values.focus_attempts < 0 ||
      values.focus_attempts > 2_147_483_647 ||
      !Number.isSafeInteger(values.focus_timeout_secs) ||
      values.focus_timeout_secs < 0 ||
      values.focus_timeout_secs > 2_147_483_647 ||
      !Number.isSafeInteger(values.recent_games) ||
      values.recent_games < 0 ||
      values.recent_games > 10_000 ||
      !Number.isSafeInteger(values.recent_max_age_days) ||
      values.recent_max_age_days < 0 ||
      values.recent_max_age_days > 36_500
    )
      throw new Error("Playnite settings contain values outside their supported ranges.");
    const parsed = {
      enabled: values.enabled,
      auto_sync: values.auto_sync,
      sync_all_installed: values.sync_all_installed,
      autosync_remove_uninstalled: values.autosync_remove_uninstalled,
      autosync_delete_after_days: values.autosync_delete_after_days,
      autosync_require_replacement: values.autosync_require_replacement,
      focus_attempts: values.focus_attempts,
      focus_timeout_secs: values.focus_timeout_secs,
      focus_exit_on_first: values.focus_exit_on_first,
      fullscreen_entry_enabled: values.fullscreen_entry_enabled,
      recent_games: values.recent_games,
      recent_max_age_days: values.recent_max_age_days,
      sync_categories: validatePlayniteFilterList(values.sync_categories, "Sync categories"),
      exclude_categories: validatePlayniteFilterList(values.exclude_categories, "Excluded categories"),
      sync_plugins: validatePlayniteFilterList(values.sync_plugins, "Sync plugin IDs"),
      exclude_plugins: validatePlayniteFilterList(values.exclude_plugins, "Excluded plugin IDs"),
      exclude_games: validatePlayniteFilterList(values.exclude_games, "Excluded game IDs"),
    };
    const maximumUpdate = { ...parsed, expected_revision: Number.MAX_SAFE_INTEGER };
    const encoder = new TextEncoder();
    if (
      encoder.encode(JSON.stringify(parsed)).length > 12_000 ||
      encoder.encode(JSON.stringify(maximumUpdate)).length > 12_000
    )
      throw new Error("Playnite settings exceed the 12000 byte document or request size limit.");
    return parsed;
  }

  export function playniteStatusDescription(status: PlayniteStatus): string {
    if (!status.settings_ready)
      return "Playnite settings are still loading; runtime state is not confirmed.";
    if (!status.enabled)
      return "Playnite catalog synchronization is disabled.";
    if (!status.playnite_running)
      return "No Playnite process was detected. Process discovery is currently available on Windows.";
    if (status.last_error)
      return `Playnite is running · ${status.snapshot_game_count} games in the last snapshot · Last issue: ${status.last_error}`;
    const sync = status.snapshot_pending
      ? "A catalog snapshot is in progress."
      : "No catalog snapshot is in progress.";
    return `Playnite is running · ${status.snapshot_game_count} games in the last snapshot · ${sync}`;
  }

  function validatePlayniteFilterList(value: unknown, label: string): string[] {
    if (!Array.isArray(value) || value.length > 256)
      throw new Error(`${label} must contain at most 256 strings.`);
    const encoder = new TextEncoder();
    const unique = new Set<string>();
    for (const item of value) {
      if (
        typeof item !== "string" ||
        item.trim().length === 0 ||
        encoder.encode(item).length > 512 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(item) ||
        unique.has(item.toLowerCase())
      )
        throw new Error(
          `${label} must contain unique, printable strings of at most 512 UTF-8 bytes.`,
        );
      unique.add(item.toLowerCase());
    }
    return [...value];
  }

  export function parseSteamImportSettings(
    includeTools: boolean,
    syncAllInstalled: boolean,
    autosyncRemoveUninstalled: boolean,
    recentGames: number,
    recentMaxAgeDays: number,
  ): Pick<
    SteamSettings["values"],
    | "include_tools"
    | "sync_all_installed"
    | "autosync_remove_uninstalled"
    | "recent_games"
    | "recent_max_age_days"
  > {
    if (
      typeof includeTools !== "boolean" ||
      typeof syncAllInstalled !== "boolean" ||
      typeof autosyncRemoveUninstalled !== "boolean" ||
      !Number.isSafeInteger(recentGames) ||
      recentGames < 0 ||
      recentGames > 10_000 ||
      !Number.isSafeInteger(recentMaxAgeDays) ||
      recentMaxAgeDays < 0 ||
      recentMaxAgeDays > 36_500
    )
      throw new Error("Steam import values are outside their supported ranges.");
    return {
      include_tools: includeTools,
      sync_all_installed: syncAllInstalled,
      autosync_remove_uninstalled: autosyncRemoveUninstalled,
      recent_games: recentGames,
      recent_max_age_days: recentMaxAgeDays,
    };
  }

  export function steamStatusDescription(status: SteamStatus): string {
    if (status.settings_ready === false)
      return "Provider settings are still loading; runtime state is not confirmed yet.";
    if (typeof status.enabled !== "boolean")
      return "The provider did not report whether Steam discovery is enabled.";
    if (!status.enabled)
      return "Steam discovery is disabled; no scan is running.";
    const scanState = status.scanning
      ? "scan in progress"
      : status.complete
        ? "last scan complete"
        : "no complete scan is available";
    const warningCount = `${status.warning_count} ${status.warning_count === 1 ? "warning" : "warnings"}`;
    return `Steam discovery enabled · ${scanState} · ${status.game_count} games · ${warningCount}.`;
  }

  export function renderSettings(context: PageContext): void {
    const area = el("div");
    const authArea = el("div");
    const hostArea = el("div");
    const steamArea = el("div");
    const lutrisArea = el("div");
    const playniteArea = el("div");
    const inputArea = el("div");
    const historyArea = el("div");
    const pairingArea = el("div");
    context.content.append(
      sectionHeading(
        "Settings",
        "Edit host options and values registered for this management console.",
        button("Refresh", () => {
          void load();
          void loadAuth();
          void loadHost();
          void loadSteam();
          void loadLutris();
          void loadPlaynite();
          void loadInput();
          void loadHistorySettings();
          void loadPairing();
        }),
      ),
      area,
      authArea,
      hostArea,
      steamArea,
      lutrisArea,
      playniteArea,
      inputArea,
      historyArea,
      pairingArea,
    );
    async function loadAuth(): Promise<void> {
      authArea.replaceChildren(el("p", "muted", "Loading authentication lifetimes…"));
      try {
        const settings = await context.api.post<AuthTokenSettings>("auth/settings");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const sessionTtl = input(
          "session_token_ttl_seconds",
          String(settings.values.session_token_ttl_seconds),
          "number",
        );
        const refreshTtl = input(
          "remember_me_refresh_token_ttl_seconds",
          String(settings.values.remember_me_refresh_token_ttl_seconds),
          "number",
        );
        for (const control of [sessionTtl, refreshTtl]) {
          control.required = true;
          control.max = String(maxConfiguredAuthTtlSeconds);
        }
        sessionTtl.min = "60";
        sessionTtl.step = "60";
        refreshTtl.min = "3600";
        refreshTtl.step = "3600";
        const status = el("div");
        const save = el("button", "button primary", "Save authentication settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Administrator session lifetime (seconds)",
            sessionTtl,
            "Access sessions default to 7200 seconds. Existing sessions keep their issued expiry; new logins and refreshes use this value.",
          ),
          field(
            "Remembered device lifetime (seconds)",
            refreshTtl,
            "Remember-me refresh tokens default to 604800 seconds. Non-remembered refresh tokens remain fixed at 86400 seconds.",
          ),
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            const values = parseAuthTokenLifetimes(
              sessionTtl.valueAsNumber,
              refreshTtl.valueAsNumber,
            );
            await context.api.post("auth/settings_update", {
              ...values,
              expected_revision: settings.revision,
            });
            context.notify("Authentication lifetimes saved.");
            await loadAuth();
          });
        });
        authArea.replaceChildren(panel("Authentication session lifetimes", form));
      } catch (error) {
        if (context.content.isConnected)
          authArea.replaceChildren(errorPanel(error, loadAuth));
      }
    }
    async function loadSteam(): Promise<void> {
      steamArea.replaceChildren(el("p", "muted", "Loading Steam settings…"));
      try {
        const settings = await context.api.post<SteamSettings>("steam/settings");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const enabled = input("steam_enabled", "", "checkbox");
        enabled.checked = settings.values.enabled;
        const autoSync = input("steam_auto_sync", "", "checkbox");
        autoSync.checked = settings.values.auto_sync;
        const includeTools = input("steam_include_tools", "", "checkbox");
        includeTools.checked = settings.values.include_tools;
        const syncAllInstalled = input(
          "steam_sync_all_installed",
          "",
          "checkbox",
        );
        syncAllInstalled.checked = settings.values.sync_all_installed;
        const autosyncRemoveUninstalled = input(
          "steam_autosync_remove_uninstalled",
          "",
          "checkbox",
        );
        autosyncRemoveUninstalled.checked =
          settings.values.autosync_remove_uninstalled;
        const recentGames = input(
          "steam_recent_games",
          String(settings.values.recent_games),
          "number",
        );
        recentGames.required = true;
        recentGames.min = "0";
        recentGames.max = "10000";
        recentGames.step = "1";
        const recentMaxAgeDays = input(
          "steam_recent_max_age_days",
          String(settings.values.recent_max_age_days),
          "number",
        );
        recentMaxAgeDays.required = true;
        recentMaxAgeDays.min = "0";
        recentMaxAgeDays.max = "36500";
        recentMaxAgeDays.step = "1";
        const exclusions = el("textarea");
        exclusions.name = "steam_exclude_games";
        exclusions.rows = 10;
        exclusions.spellcheck = false;
        exclusions.value = JSON.stringify(settings.values.exclude_games, null, 2);
        const status = el("div");
        const runtimeStatus = el(
          "p",
          "muted",
          "Loading current provider status…",
        );
        const save = el("button", "button primary", "Save Steam settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Enable Steam library discovery",
            enabled,
            "Controls whether the provider exposes discovered Steam games and accepts scans.",
          ),
          field(
            "Automatic synchronization",
            autoSync,
            "Controls automatic scans of the configured Steam library roots.",
          ),
          field(
            "Include tools",
            includeTools,
            "Includes tools alongside games when importing the library.",
          ),
          field(
            "Synchronize all installed games",
            syncAllInstalled,
            "When off, import recently played games instead of every installed game.",
          ),
          field(
            "Remove uninstalled games during synchronization",
            autosyncRemoveUninstalled,
            "Applies when synchronizing all installed games. Recent-game mode always removes stale imported entries.",
          ),
          field(
            "Recent games to import",
            recentGames,
            "Maximum number of recently played games (0–10000); zero imports none in recent-only mode.",
          ),
          field(
            "Recent game age limit (days)",
            recentMaxAgeDays,
            "Ignore games older than this age (0–36500); zero disables the age cutoff.",
          ),
          field(
            "Excluded games (JSON)",
            exclusions,
            'Use objects with "id" (canonical app ID), "name", or both. ID takes precedence; name-only entries match without case sensitivity.',
          ),
          runtimeStatus,
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            await context.api.post("steam/settings_update", {
              enabled: enabled.checked,
              auto_sync: autoSync.checked,
              exclude_games: parseSteamExclusions(exclusions.value),
              ...parseSteamImportSettings(
                includeTools.checked,
                syncAllInstalled.checked,
                autosyncRemoveUninstalled.checked,
                recentGames.valueAsNumber,
                recentMaxAgeDays.valueAsNumber,
              ),
              expected_revision: settings.revision,
            });
            context.notify("Steam discovery setting saved.");
            await loadSteam();
          });
        });
        steamArea.replaceChildren(panel("Steam", form));
        void loadSteamRuntimeStatus(runtimeStatus, context);
      } catch (error) {
        if (context.content.isConnected)
          steamArea.replaceChildren(errorPanel(error, loadSteam));
      }
    }
    async function loadLutris(): Promise<void> {
      lutrisArea.replaceChildren(el("p", "muted", "Loading Lutris settings…"));
      try {
        const [settings, runtime] = await Promise.all([
          loadLutrisSettings(),
          context.api.post<LutrisStatus>("lutris/status"),
        ]);
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const enabled = input("lutris_enabled", "", "checkbox");
        enabled.checked = settings.values.enabled;
        enabled.disabled = !runtime.platform_supported;
        const autoSync = input("lutris_auto_sync", "", "checkbox");
        autoSync.checked = settings.values.auto_sync;
        const removeUninstalled = input(
          "lutris_autosync_remove_uninstalled",
          "",
          "checkbox",
        );
        removeUninstalled.checked = settings.values.autosync_remove_uninstalled;
        const includeSteam = input("lutris_include_steam", "", "checkbox");
        includeSteam.checked = settings.values.include_steam;
        const exclusions = el("textarea");
        exclusions.name = "lutris_exclude_games";
        exclusions.rows = 8;
        exclusions.maxLength = 12_000;
        exclusions.spellcheck = false;
        exclusions.value = JSON.stringify(settings.values.exclude_games, null, 2);
        const runtimeStatus = el(
          "p",
          runtime.last_error ? "error" : "muted",
          lutrisStatusDescription(runtime),
        );
        const saveStatus = el("div");
        const refreshStatus = el("div");
        const save = el("button", "button primary", "Save Lutris settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        const refresh = button(
          "Refresh now",
          () => runAction(refresh, refreshStatus, async () => {
            const updated = await context.api.post<LutrisStatus>("lutris/refresh");
            runtimeStatus.className = updated.last_error ? "error" : "muted";
            runtimeStatus.textContent = lutrisStatusDescription(updated);
            context.notify("Lutris refresh requested.");
          }),
        );
        refresh.disabled =
          !context.api.allows("configuration.write") ||
          !runtime.platform_supported ||
          !settings.values.enabled;
        enabled.addEventListener("change", () => {
          refresh.disabled =
            !context.api.allows("configuration.write") ||
            !runtime.platform_supported ||
            !enabled.checked;
        });
        form.append(
          field(
            "Enable Lutris discovery",
            enabled,
            runtime.platform_supported
              ? "Controls discovery for the current Linux user."
              : "Lutris discovery is available only on Linux; this option cannot be enabled on this platform.",
          ),
          field(
            "Automatic synchronization",
            autoSync,
            "Scan the current user's installed Lutris games automatically.",
          ),
          field(
            "Remove uninstalled games",
            removeUninstalled,
            "After a successful complete scan, remove Lutris-owned games that are no longer installed.",
          ),
          field(
            "Include matching Steam games",
            includeSteam,
            "Read a complete Steam catalog before import; the direct Steam catalog record takes precedence for matching app IDs.",
          ),
          field(
            "Excluded games (JSON)",
            exclusions,
            'Use a positive decimal ID string or an object with "id", "name", or both. ID takes precedence; name-only entries match without case sensitivity.',
          ),
          runtimeStatus,
          saveStatus,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, saveStatus, async () => {
            try {
              await context.api.post("lutris/settings_update", {
                enabled: enabled.checked,
                auto_sync: autoSync.checked,
                autosync_remove_uninstalled: removeUninstalled.checked,
                include_steam: includeSteam.checked,
                exclude_games: parseLutrisExclusions(exclusions.value),
                expected_revision: settings.revision,
              });
              context.notify("Lutris settings saved.");
              await loadLutris();
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                context.notify("Lutris settings changed elsewhere. The latest values have been reloaded.");
                await loadLutris();
                return;
              }
              throw error;
            }
          });
        });
        refreshStatus.className = "muted";
        lutrisArea.replaceChildren(
          panel(
            "Lutris",
            form,
            el("div", "actions", refresh, refreshStatus),
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          lutrisArea.replaceChildren(errorPanel(error, loadLutris));
      }
    }
    async function loadLutrisSettings(): Promise<LutrisSettings> {
      for (let attempt = 0; ; attempt += 1) {
        try {
          return await context.api.post<LutrisSettings>("lutris/settings");
        } catch (error) {
          if (
            !(error instanceof ApiError) ||
            !["settings_loading", "settings_busy"].includes(error.code) ||
            attempt >= 7
          ) {
            throw error;
          }
          await new Promise<void>((resolve) => {
            window.setTimeout(resolve, 50 * (attempt + 1));
          });
        }
      }
    }
    async function loadPlaynite(): Promise<void> {
      playniteArea.replaceChildren(el("p", "muted", "Loading Playnite settings…"));
      try {
        const settings = await loadPlayniteSettings();
        const runtime = await context.api.post<PlayniteStatus>("playnite/status");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const enabled = input("playnite_enabled", "", "checkbox");
        enabled.checked = settings.values.enabled;
        const autoSync = input("playnite_auto_sync", "", "checkbox");
        autoSync.checked = settings.values.auto_sync;
        const syncAllInstalled = input(
          "playnite_sync_all_installed",
          "",
          "checkbox",
        );
        syncAllInstalled.checked = settings.values.sync_all_installed;
        const removeUninstalled = input(
          "playnite_autosync_remove_uninstalled",
          "",
          "checkbox",
        );
        removeUninstalled.checked = settings.values.autosync_remove_uninstalled;
        const deleteAfterDays = input(
          "playnite_autosync_delete_after_days",
          String(settings.values.autosync_delete_after_days),
          "number",
        );
        deleteAfterDays.required = true;
        deleteAfterDays.min = "0";
        deleteAfterDays.max = "2147483647";
        deleteAfterDays.step = "1";
        const requireReplacement = input(
          "playnite_autosync_require_replacement",
          "",
          "checkbox",
        );
        requireReplacement.checked = settings.values.autosync_require_replacement;
        const focusAttempts = input(
          "playnite_focus_attempts",
          String(settings.values.focus_attempts),
          "number",
        );
        focusAttempts.required = true;
        focusAttempts.min = "0";
        focusAttempts.max = "2147483647";
        focusAttempts.step = "1";
        const focusTimeoutSecs = input(
          "playnite_focus_timeout_secs",
          String(settings.values.focus_timeout_secs),
          "number",
        );
        focusTimeoutSecs.required = true;
        focusTimeoutSecs.min = "0";
        focusTimeoutSecs.max = "2147483647";
        focusTimeoutSecs.step = "1";
        const focusExitOnFirst = input("playnite_focus_exit_on_first", "", "checkbox");
        focusExitOnFirst.checked = settings.values.focus_exit_on_first;
        const fullscreenEntryEnabled = input("playnite_fullscreen_entry_enabled", "", "checkbox");
        fullscreenEntryEnabled.checked = settings.values.fullscreen_entry_enabled;
        const recentGames = input(
          "playnite_recent_games",
          String(settings.values.recent_games),
          "number",
        );
        recentGames.required = true;
        recentGames.min = "0";
        recentGames.max = "10000";
        recentGames.step = "1";
        const recentMaxAgeDays = input(
          "playnite_recent_max_age_days",
          String(settings.values.recent_max_age_days),
          "number",
        );
        recentMaxAgeDays.required = true;
        recentMaxAgeDays.min = "0";
        recentMaxAgeDays.max = "36500";
        recentMaxAgeDays.step = "1";
        const filterList = (name: string, values: string[]): HTMLTextAreaElement => {
          const control = el("textarea");
          control.name = name;
          control.rows = 5;
          control.maxLength = 12_000;
          control.spellcheck = false;
          control.value = JSON.stringify(values, null, 2);
          return control;
        };
        const syncCategories = filterList(
          "playnite_sync_categories",
          settings.values.sync_categories,
        );
        const excludeCategories = filterList(
          "playnite_exclude_categories",
          settings.values.exclude_categories,
        );
        const syncPlugins = filterList(
          "playnite_sync_plugins",
          settings.values.sync_plugins,
        );
        const excludePlugins = filterList(
          "playnite_exclude_plugins",
          settings.values.exclude_plugins,
        );
        const excludeGames = filterList(
          "playnite_exclude_games",
          settings.values.exclude_games,
        );
        const runtimeStatus = el(
          "p",
          runtime.last_error ? "error" : "muted",
          playniteStatusDescription(runtime),
        );
        const saveStatus = el("div");
        const refreshStatus = el("div");
        const save = el("button", "button primary", "Save Playnite settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        const refresh = button(
          "Refresh now",
          () => runAction(refresh, refreshStatus, async () => {
            await context.api.post("playnite/refresh");
            context.notify("Playnite refresh requested.");
            await loadPlaynite();
          }),
        );
        const updateRefreshAvailability = (): void => {
          refresh.disabled =
            !context.api.allows("configuration.write") ||
            !enabled.checked ||
            !runtime.playnite_running ||
            runtime.snapshot_pending;
        };
        updateRefreshAvailability();
        enabled.addEventListener("change", updateRefreshAvailability);
        form.append(
          field(
            "Enable Playnite synchronization",
            enabled,
            "Controls whether Playnite games are imported. The setting is preserved on hosts where Playnite process discovery is unavailable.",
          ),
          field(
            "Automatic synchronization",
            autoSync,
            "Periodically scan Playnite while its desktop or fullscreen client is running.",
          ),
          field(
            "Synchronize all installed games",
            syncAllInstalled,
            "When off, import recent games plus installed games selected by category or plugin filters.",
          ),
          field(
            "Remove uninstalled Playnite games",
            removeUninstalled,
            "Remove imported games explicitly reported as uninstalled by a complete Playnite snapshot.",
          ),
          field(
            "Delete expired Playnite games after (days)",
            deleteAfterDays,
            "Remove eligible Playnite imports after this age when they have not been played since import. Zero disables this cleanup.",
          ),
          field(
            "Require a replacement for recent-game cleanup",
            requireReplacement,
            "Limit removal of unselected recent imports to the count of newly selected games.",
          ),
          field(
            "Focus attempts after game start",
            focusAttempts,
            "Try to focus the confirmed Playnite game process this many times. Zero disables focus.",
          ),
          field(
            "Focus timeout (seconds)",
            focusTimeoutSecs,
            "Stop trying after this many seconds. Zero disables focus.",
          ),
          field(
            "Stop focus attempts after first success",
            focusExitOnFirst,
            "When enabled, stop retrying once the game is confirmed in the foreground.",
          ),
          field(
            "Show Playnite Fullscreen in the catalog",
            fullscreenEntryEnabled,
            "Add or remove the provider-managed Playnite Fullscreen launch entry. Manual Fullscreen entries remain available.",
          ),
          field(
            "Recent games to import",
            recentGames,
            "Maximum number of recent games (0–10000); zero disables recent-game selection.",
          ),
          field(
            "Recent game age limit (days)",
            recentMaxAgeDays,
            "Ignore recent games older than this age (0–36500); zero disables the age cutoff.",
          ),
          field(
            "Included categories (JSON)",
            syncCategories,
            "Additional installed games whose category matches any listed value; an empty list adds no category matches.",
          ),
          field(
            "Excluded categories (JSON)",
            excludeCategories,
            "Games in these categories are excluded even if another setting would include them.",
          ),
          field(
            "Included plugin IDs (JSON)",
            syncPlugins,
            "Additional installed games from any listed Playnite plugin; an empty list adds no plugin matches.",
          ),
          field(
            "Excluded plugin IDs (JSON)",
            excludePlugins,
            "Games from these plugin IDs are excluded even if another setting would include them.",
          ),
          field(
            "Excluded game IDs (JSON)",
            excludeGames,
            "Game IDs to omit from every import selection.",
          ),
          runtimeStatus,
          saveStatus,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, saveStatus, async () => {
            try {
              const values = parsePlayniteSettings({
                enabled: enabled.checked,
                auto_sync: autoSync.checked,
                sync_all_installed: syncAllInstalled.checked,
                autosync_remove_uninstalled: removeUninstalled.checked,
                autosync_delete_after_days: deleteAfterDays.valueAsNumber,
                autosync_require_replacement: requireReplacement.checked,
                focus_attempts: focusAttempts.valueAsNumber,
                focus_timeout_secs: focusTimeoutSecs.valueAsNumber,
                focus_exit_on_first: focusExitOnFirst.checked,
                fullscreen_entry_enabled: fullscreenEntryEnabled.checked,
                recent_games: recentGames.valueAsNumber,
                recent_max_age_days: recentMaxAgeDays.valueAsNumber,
                sync_categories: parsePlayniteFilterList(
                  syncCategories.value,
                  "Sync categories",
                ),
                exclude_categories: parsePlayniteFilterList(
                  excludeCategories.value,
                  "Excluded categories",
                ),
                sync_plugins: parsePlayniteFilterList(
                  syncPlugins.value,
                  "Sync plugin IDs",
                ),
                exclude_plugins: parsePlayniteFilterList(
                  excludePlugins.value,
                  "Excluded plugin IDs",
                ),
                exclude_games: parsePlayniteFilterList(
                  excludeGames.value,
                  "Excluded game IDs",
                ),
              });
              await context.api.post("playnite/settings_update", {
                ...values,
                expected_revision: settings.revision,
              });
              context.notify("Playnite settings saved.");
              await loadPlaynite();
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                context.notify("Playnite settings changed elsewhere. The latest values have been reloaded.");
                await loadPlaynite();
                return;
              }
              throw error;
            }
          });
        });
        refreshStatus.className = "muted";
        playniteArea.replaceChildren(
          panel(
            "Playnite",
            form,
            el("div", "actions", refresh, refreshStatus),
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          playniteArea.replaceChildren(errorPanel(error, loadPlaynite));
      }
    }
    async function loadPlayniteSettings(): Promise<PlayniteSettings> {
      for (let attempt = 0; ; attempt += 1) {
        try {
          return await context.api.post<PlayniteSettings>("playnite/settings");
        } catch (error) {
          if (
            !(error instanceof ApiError) ||
            !["settings_loading", "settings_busy"].includes(error.code) ||
            attempt >= 7
          ) {
            throw error;
          }
          await new Promise<void>((resolve) => {
            window.setTimeout(resolve, 50 * (attempt + 1));
          });
        }
      }
    }
    async function loadPairing(): Promise<void> {
      pairingArea.replaceChildren(el("p", "muted", "Loading paired clients…"));
      try {
        const settings = await context.api.post<PairingClients>("pairing/list_clients");
        if (!context.content.isConnected) return;
        if (settings.clients.length === 0) {
          pairingArea.replaceChildren(
            panel(
              "Paired client overrides",
              empty("No paired clients", "Pair a Moonlight client before setting client-specific streaming values."),
              badge(`Revision ${settings.revision}`),
            ),
          );
          return;
        }
        const rows = settings.clients.map((client) => {
          const fec = input(
            `fec_${client.certificate_id}`,
            String(client.config_overrides?.fec_percentage ?? ""),
            "number",
          );
          fec.min = "1";
          fec.max = "255";
          fec.step = "1";
          fec.placeholder = "Inherit";
          fec.setAttribute("aria-label", `FEC percentage override for ${client.name}`);
          const backButtonTimeout = input(
            `back_button_timeout_${client.certificate_id}`,
            client.config_overrides?.back_button_timeout?.toString() ?? "",
            "number",
          );
          backButtonTimeout.min = String(-0x8000_0000);
          backButtonTimeout.max = String(0x7fff_ffff);
          backButtonTimeout.step = "1";
          backButtonTimeout.placeholder = "Inherit";
          backButtonTimeout.setAttribute("aria-label", `Back button timeout override for ${client.name}`);
          const streamAudio = el("select");
          streamAudio.setAttribute("aria-label", `Audio streaming override for ${client.name}`);
          const audioChoices: Array<[string, string]> = [
            ["", "Inherit"],
            ["true", "Enabled"],
            ["false", "Disabled"],
          ];
          for (const [value, label] of audioChoices) {
            const option = el("option", "", label);
            option.value = value;
            streamAudio.append(option);
          }
          streamAudio.value = client.config_overrides?.stream_audio === undefined
            ? ""
            : String(client.config_overrides.stream_audio);
          const audioSinkMode = el("select");
          audioSinkMode.setAttribute("aria-label", `Audio endpoint override for ${client.name}`);
          for (const [value, label] of [
            ["inherit", "Inherit application/host"],
            ["automatic", "Automatic/default endpoint"],
            ["specific", "Specific endpoint ID"],
          ]) {
            const option = el("option", "", label);
            option.value = value ?? "";
            audioSinkMode.append(option);
          }
          const currentAudioSink = client.config_overrides?.audio_sink;
          audioSinkMode.value = currentAudioSink === undefined ? "inherit" : currentAudioSink === "" ? "automatic" : "specific";
          const audioSinkId = input(`audio_sink_${client.certificate_id}`, currentAudioSink ?? "");
          audioSinkId.maxLength = 4096;
          audioSinkId.placeholder = "Exact endpoint ID";
          audioSinkId.setAttribute("aria-label", `Specific audio endpoint ID for ${client.name}`);
          const updateAudioSink = () => { audioSinkId.disabled = audioSinkMode.value !== "specific"; };
          audioSinkMode.addEventListener("change", updateAudioSink);
          updateAudioSink();
          const virtualSinkMode = el("select");
          virtualSinkMode.setAttribute("aria-label", `Virtual sink override for ${client.name}`);
          for (const [value, label] of [
            ["inherit", "Inherit application/host"],
            ["automatic", "Clear inherited virtual sink"],
            ["specific", "Specific virtual sink selector"],
          ]) {
            const option = el("option", "", label);
            option.value = value ?? "";
            virtualSinkMode.append(option);
          }
          const currentVirtualSink = client.config_overrides?.virtual_sink;
          virtualSinkMode.value = currentVirtualSink === undefined ? "inherit" : currentVirtualSink === "" ? "automatic" : "specific";
          const virtualSinkSelector = input(`virtual_sink_${client.certificate_id}`, currentVirtualSink ?? "");
          virtualSinkSelector.maxLength = 4096;
          virtualSinkSelector.placeholder = "Exact ID or device name";
          virtualSinkSelector.setAttribute("aria-label", `Specific virtual sink selector for ${client.name}`);
          const updateVirtualSink = () => { virtualSinkSelector.disabled = virtualSinkMode.value !== "specific"; };
          virtualSinkMode.addEventListener("change", updateVirtualSink);
          updateVirtualSink();
          const captureOnly = el("select");
          captureOnly.setAttribute("aria-label", `Capture-only audio override for ${client.name}`);
          for (const [value, label] of [
            ["", "Inherit"],
            ["true", "Capture without routing"],
            ["false", "Allow default-output routing"],
          ]) {
            const option = el("option", "", label);
            option.value = value ?? "";
            captureOnly.append(option);
          }
          captureOnly.value = client.config_overrides?.audio_sink_capture_only === undefined
            ? ""
            : String(client.config_overrides.audio_sink_capture_only);
          const status = el("div");
          const save = button(
            "Save",
            () => runAction(save, status, async () => {
              try {
                await context.api.post("pairing/update_client_config_overrides", {
                  certificate_id: client.certificate_id,
                  expected_revision: settings.revision,
                  config_overrides: {
                    ...parseClientFecOverride(fec.value),
                    back_button_timeout: parseBackButtonTimeoutOverride(backButtonTimeout.value),
                    stream_audio: parseStreamAudioOverride(streamAudio.value),
                    audio_sink: parseAudioSinkOverride(audioSinkMode.value, audioSinkId.value),
                    virtual_sink: parseVirtualSinkOverride(virtualSinkMode.value, virtualSinkSelector.value),
                    audio_sink_capture_only: parseAudioSinkCaptureOnlyOverride(captureOnly.value),
                  },
                });
                context.notify(`Streaming override saved for ${client.name}.`);
                await loadPairing();
              } catch (error) {
                if (error instanceof ApiError && error.code === "revision_conflict") {
                  context.notify("Paired clients changed elsewhere. The list has been refreshed; review and save again.");
                  await loadPairing();
                  return;
                }
                throw error;
              }
            }),
            "primary",
          );
          save.disabled = !context.api.allows("pairing.admin");
          const identity = el(
            "div",
            "",
            el("strong", "", client.name),
            el("small", "muted", `${client.client_id} · certificate ${client.certificate_id.slice(0, 12)}…`),
          );
          return [identity, fec, backButtonTimeout, streamAudio, el("div", "", audioSinkMode, audioSinkId), el("div", "", virtualSinkMode, virtualSinkSelector), captureOnly, el("div", "", save, status)];
        });
        const tableView = table(["Paired client", "FEC percentage", "Back button timeout (ms)", "Audio streaming", "Audio endpoint", "Virtual sink", "Capture only", ""], rows);
        pairingArea.replaceChildren(
          panel(
            "Paired client overrides",
            el("p", "muted", "Blank inherits the application and host value. Back button timeout accepts signed 32-bit milliseconds, with -1 disabling the timeout. Each save uses the current pairing revision."),
            tableView,
            el("div", "actions", badge(`Revision ${settings.revision}`)),
          ),
        );
      } catch (error) {
        if (context.content.isConnected)
          pairingArea.replaceChildren(errorPanel(error, loadPairing));
      }
    }
    async function loadHistorySettings(): Promise<void> {
      historyArea.replaceChildren(el("p", "muted", "Loading session history settings…"));
      try {
        const reply = await context.api.post<HistorySettingsReply>(
          "history/settings_get",
        );
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const enabled = input("session_history_enabled", "", "checkbox");
        enabled.checked = reply.settings.enabled;
        const ttl = input(
          "session_history_ttl_days",
          String(reply.settings.ttl_days),
          "number",
        );
        const size = input(
          "session_history_db_size_limit_mb",
          String(reply.settings.db_size_limit_mb),
          "number",
        );
        for (const control of [ttl, size]) {
          control.required = true;
          control.min = "0";
          control.max = String(maxHistoryLimit);
          control.step = "1";
        }
        const ttlField = field(
          "Keep ended sessions for (days)",
          ttl,
          "Zero disables age pruning.",
        );
        const sizeField = field(
          "Approximate live database limit (MiB)",
          size,
          "Zero disables size pruning. Active sessions are never deleted to meet this limit.",
        );
        const updateVisibility = (): void => {
          ttlField.hidden = !enabled.checked;
          sizeField.hidden = !enabled.checked;
        };
        enabled.addEventListener("change", updateVisibility);
        updateVisibility();
        const status = el(
          "p",
          "muted",
          reply.effective_enabled === reply.settings.enabled
            ? "The saved history policy is active."
            : "The enable change will apply after active streams end.",
        );
        const save = el("button", "button primary", "Save history settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Record session history",
            enabled,
            "Changes to recording during an active stream take effect after streams end.",
          ),
          ttlField,
          sizeField,
          status,
          el("div", "actions", save, badge(`Revision ${reply.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            const updated = await context.api.post<HistorySettingsReply>(
              "history/settings_update",
              {
                settings: parseHistorySettings(
                  enabled.checked,
                  ttl.valueAsNumber,
                  size.valueAsNumber,
                ),
                expected_revision: reply.revision,
              },
            );
            context.notify(
              updated.effective_enabled === updated.settings.enabled
                ? "Session history settings saved."
                : "Session history settings saved; the recording change is deferred.",
            );
            await loadHistorySettings();
          });
        });
        historyArea.replaceChildren(panel("Session history", form));
      } catch (error) {
        if (context.content.isConnected)
          historyArea.replaceChildren(errorPanel(error, loadHistorySettings));
      }
    }

    async function loadInput(): Promise<void> {
      inputArea.replaceChildren(el("p", "muted", "Loading input policy…"));
      try {
        const [settings, runtimeStatus] = await Promise.all([
          context.api.post<InputPolicySettings>("input/settings_get"),
          context.api.post<InputRuntimeStatus>("input/status"),
        ]);
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const keyboard = input("input_keyboard", "", "checkbox");
        keyboard.checked = settings.values.keyboard;
        const mouse = input("input_mouse", "", "checkbox");
        mouse.checked = settings.values.mouse;
        const controller = input("input_controller", "", "checkbox");
        controller.checked = settings.values.controller;
        const highResolutionScrolling = input(
          "input_high_resolution_scrolling",
          "",
          "checkbox",
        );
        highResolutionScrolling.checked = settings.values.high_resolution_scrolling;
        const alwaysSendScancodes = input(
          "input_always_send_scancodes",
          "",
          "checkbox",
        );
        alwaysSendScancodes.checked = settings.values.always_send_scancodes;
        const keyRightAltToKeyWin = input(
          "input_key_rightalt_to_key_win",
          "",
          "checkbox",
        );
        keyRightAltToKeyWin.checked = settings.values.key_rightalt_to_key_win;
        const keyRepeatDelay = input(
          "input_key_repeat_delay",
          String(settings.values.key_repeat_delay),
          "number",
        );
        keyRepeatDelay.required = true;
        keyRepeatDelay.min = "0";
        keyRepeatDelay.max = String(maxInputKeyRepeatDelayMs);
        keyRepeatDelay.step = "1";
        const keyRepeatFrequency = input(
          "input_key_repeat_frequency",
          String(settings.values.key_repeat_frequency),
          "number",
        );
        keyRepeatFrequency.required = true;
        keyRepeatFrequency.min = "0";
        keyRepeatFrequency.step = "any";
        const backButtonTimeout = input(
          "input_back_button_timeout",
          String(settings.values.back_button_timeout),
          "number",
        );
        backButtonTimeout.required = true;
        backButtonTimeout.min = String(minInputBackButtonTimeoutMs);
        backButtonTimeout.max = String(maxInputBackButtonTimeoutMs);
        backButtonTimeout.step = "1";
        const gamepadProfile = el("select");
        gamepadProfile.name = "input_gamepad_profile";
        const gamepadProfileChoices = inputGamepadProfileChoices(
          settings.values.gamepad,
          runtimeStatus.supported_profiles,
        );
        for (const choice of gamepadProfileChoices) {
          const option = el("option", "", choice.label);
          option.value = choice.value;
          gamepadProfile.append(option);
        }
        gamepadProfile.value = settings.values.gamepad;
        const selectedGamepadUnavailable =
          settings.values.controller &&
          (!runtimeStatus.supported_profiles.includes(settings.values.gamepad) ||
            (runtimeStatus.lease !== null &&
              runtimeStatus.gamepad === settings.values.gamepad &&
              !runtimeStatus.controller_input));
        const gamepadStatus = el(
          "p",
          selectedGamepadUnavailable ? "error" : "muted",
          inputGamepadStatusDescription(settings.values.gamepad, runtimeStatus),
        );
        const status = el(
          "p",
          settings.applied ? "muted" : "error",
          inputPolicyStatusDescription(settings),
        );
        const save = el("button", "button primary", "Save input policy");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Keyboard input",
            keyboard,
            "Disabling keyboard input suppresses key events and releases held keys immediately.",
          ),
          field(
            "Mouse input",
            mouse,
            "Disabling mouse input suppresses pointer, button, and scroll events and releases held buttons.",
          ),
          field(
            "Controller input",
            controller,
            "Disabling controller input suppresses controller activity and removes owned virtual gamepad slots. Keyboard and mouse input continue.",
          ),
          field(
            "High-resolution scrolling",
            highResolutionScrolling,
            "Preserves raw wheel units for supported streaming clients. Disable to convert scroll input to legacy 120-unit detents.",
          ),
          field(
            "Always send scan codes",
            alwaysSendScancodes,
            "On Windows, maps non-normalized Moonlight key packets to scan codes. Linux persists this preference but uses its fixed key mapping.",
          ),
          field(
            "Map Right Alt to Left Windows",
            keyRightAltToKeyWin,
            "Maps Moonlight Right Alt (VK_RMENU) to Left Windows (VK_LWIN) on Windows and Linux; a held key stays paired with its original release if this changes.",
          ),
          field(
            "Key repeat delay (milliseconds)",
            keyRepeatDelay,
            "Default is 500 milliseconds. Set to 0 to disable key repeat.",
          ),
          field(
            "Key repeat frequency (Hz)",
            keyRepeatFrequency,
            "Default is 24.9 Hz. Set to 0 to use the 24.9 Hz default; decimal frequencies are supported.",
          ),
          field(
            "Back button timeout (milliseconds)",
            backButtonTimeout,
            "On Linux and Windows, when controller input is available, holding Back for this many milliseconds pulses Guide for 100 milliseconds. Any negative value disables it; other platforms do not inject gamepads.",
          ),
          field(
            "Gamepad profile",
            gamepadProfile,
            "Profiles listed by input status are supported by this backend. A saved platform-recognized profile outside that list remains visible and is labeled unavailable.",
          ),
          gamepadStatus,
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            try {
              const updated = await context.api.post<InputPolicySettings>(
                "input/settings_update",
                {
                  ...parseInputPolicy(
                    keyboard.checked,
                    mouse.checked,
                    highResolutionScrolling.checked,
                    alwaysSendScancodes.checked,
                    keyRightAltToKeyWin.checked,
                    keyRepeatDelay.valueAsNumber,
                    keyRepeatFrequency.valueAsNumber,
                    backButtonTimeout.valueAsNumber,
                    gamepadProfile.value,
                    gamepadProfileChoices.map((choice) => choice.value),
                    controller.checked,
                  ),
                  expected_revision: settings.revision,
                },
              );
              context.notify(
                updated.applied
                  ? "Input policy saved and applied."
                  : "Input policy saved; native cleanup is pending.",
              );
            } catch (error) {
              if (!(error instanceof ApiError) || error.code !== "cleanup_pending")
                throw error;
              context.notify(
                "Input policy was saved, but releasing held native input is pending.",
              );
            }
            await loadInput();
          });
        });
        inputArea.replaceChildren(panel("Input policy", form));
      } catch (error) {
        if (context.content.isConnected)
          inputArea.replaceChildren(errorPanel(error, loadInput));
      }
    }
    async function loadSteamRuntimeStatus(
      target: HTMLElement,
      context: PageContext,
    ): Promise<void> {
      try {
        const status = await context.api.post<SteamStatus>("steam/status");
        if (!target.isConnected) return;
        target.textContent = steamStatusDescription(status);
      } catch (error) {
        if (!target.isConnected) return;
        target.textContent =
          error instanceof ApiError && error.code === "forbidden"
            ? "Runtime status requires steam.read; settings remain available with configuration access."
            : "Current provider status is unavailable; settings remain editable.";
      }
    }
    async function loadHost(): Promise<void> {
      hostArea.replaceChildren(el("p", "muted", "Loading host name…"));
      try {
        const settings = await context.api.post<NvhttpSettings>(
          "nvhttp/settings",
        );
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const hostname = input("hostname", settings.configured_hostname);
        hostname.maxLength = 128;
        const port = input("port", String(settings.port), "number");
        port.required = true;
        port.min = "1029";
        port.max = "65514";
        port.step = "1";
        const addressFamily = el("select");
        addressFamily.name = "address_family";
        const familyOptions: Array<[string, string]> = [
          ["startup", `Use startup policy (effective: ${settings.address_family})`],
          ["ipv4", "IPv4 only"],
          ["both", "IPv4 and IPv6"],
        ];
        for (const [value, label] of familyOptions) {
          const option = el("option", "", label);
          option.value = value;
          addressFamily.append(option);
        }
        addressFamily.value = settings.configured_address_family ?? "startup";
        const useStartupBindAddress = input(
          "use_startup_bind_address",
          "",
          "checkbox",
        );
        useStartupBindAddress.checked = settings.configured_bind_address === null;
        const bindAddress = input(
          "bind_address",
          settings.configured_bind_address ?? "",
        );
        bindAddress.disabled = useStartupBindAddress.checked;
        useStartupBindAddress.addEventListener("change", () => {
          bindAddress.disabled = useStartupBindAddress.checked;
        });
        const capture = el("select");
        capture.name = "capture";
        const captureChoices: Array<[NvhttpSettings["configured_capture"], string]> = [
          ["auto", "Automatic (startup capture backend)"],
          ["ddx", "Desktop Duplication"],
          ["wgc", "Windows Graphics Capture"],
          ["wgcc", "Windows Graphics Capture, constant rate"],
        ];
        for (const [value, label] of captureChoices) {
          const option = el("option", "", label);
          option.value = value;
          capture.append(option);
        }
        capture.value = settings.configured_capture;
        const outputName = el("select");
        outputName.name = "output_name";
        const automaticOutput = el("option", "", "Automatic (startup output)");
        automaticOutput.value = "";
        outputName.append(automaticOutput);
        let outputDiscoveryIssue: string | null = null;
        if (settings.capture_provider && context.api.allows("media.read")) {
          try {
            const discovered = await context.api.post<{
              outputs: Array<{ id: string; device_name: string; attached: boolean }>;
            }>(`media/${settings.capture_provider}_outputs`);
            if (!context.content.isConnected) return;
            for (const output of discovered.outputs) {
              try {
                if (!output.attached || !output.id || parseOutputNameSelection(output.id) !== output.id)
                  continue;
                const option = el("option", "", `${output.device_name} (${output.id})`);
                option.value = output.id;
                outputName.append(option);
              } catch {
                // A malformed discovery result is not an editable output ID.
              }
            }
          } catch {
            outputDiscoveryIssue = "Capture outputs could not be listed; the saved selection remains available.";
          }
        } else if (!settings.capture_provider) {
          outputDiscoveryIssue = "This host has no supported capture provider for output selection.";
        } else {
          outputDiscoveryIssue = "Listing capture outputs requires media.read.";
        }
        if (settings.configured_output_name && !Array.from(outputName.options).some(
          (option) => option.value === settings.configured_output_name,
        )) {
          const saved = el("option", "", `Saved output: ${settings.configured_output_name}`);
          saved.value = settings.configured_output_name;
          outputName.append(saved);
        }
        outputName.value = settings.configured_output_name ?? "";
        const streamAudio = input("stream_audio", "", "checkbox");
        streamAudio.checked = settings.stream_audio;
        const wgcPacing = el("select");
        wgcPacing.name = "wgc_pacing_smoothing";
        for (const [value, label] of [
          ["", "Default (enabled)"],
          ["true", "Enabled"],
          ["false", "Disabled"],
        ]) {
          const option = el("option", "", label);
          option.value = value ?? "";
          wgcPacing.append(option);
        }
        wgcPacing.value = settings.configured_wgc_pacing_smoothing === null
          ? ""
          : String(settings.configured_wgc_pacing_smoothing);
        const audioSink = el("select");
        audioSink.name = "audio_sink";
        const automaticAudioSink = el("option", "", "Automatic (startup audio endpoint)");
        automaticAudioSink.value = "";
        audioSink.append(automaticAudioSink);
        let audioDiscoveryIssue: string | null = null;
        if (context.api.allows("media.read")) {
          try {
            const discovered = await context.api.post<{
              endpoints: Array<{ id: string; name: string; default_multimedia: boolean }>;
            }>("media/audio_endpoints");
            if (!context.content.isConnected) return;
            for (const endpoint of discovered.endpoints) {
              try {
                if (!endpoint.id || parseAudioSinkSelection(endpoint.id) !== endpoint.id)
                  continue;
                const option = el("option", "", `${endpoint.name}${endpoint.default_multimedia ? " (default)" : ""} (${endpoint.id})`);
                option.value = endpoint.id;
                audioSink.append(option);
              } catch {
                // Invalid endpoint metadata cannot become an editable selection.
              }
            }
          } catch {
            audioDiscoveryIssue = "Audio endpoints could not be listed; the saved selection remains available.";
          }
        } else {
          audioDiscoveryIssue = "Listing audio endpoints requires media.read.";
        }
        if (settings.configured_audio_sink && !Array.from(audioSink.options).some(
          (option) => option.value === settings.configured_audio_sink,
        )) {
          const saved = el("option", "", `Saved endpoint: ${settings.configured_audio_sink}`);
          saved.value = settings.configured_audio_sink;
          audioSink.append(saved);
        }
        audioSink.value = settings.configured_audio_sink ?? "";
        const virtualSinkEnabled = input("virtual_sink_enabled", "", "checkbox");
        virtualSinkEnabled.checked = settings.configured_virtual_sink !== null;
        const virtualSink = input("virtual_sink", settings.configured_virtual_sink ?? "");
        virtualSink.maxLength = 4096;
        virtualSink.placeholder = "Exact device ID or name";
        const updateVirtualSink = () => { virtualSink.disabled = !virtualSinkEnabled.checked; };
        virtualSinkEnabled.addEventListener("change", updateVirtualSink);
        updateVirtualSink();
        const audioSinkCaptureOnly = input("audio_sink_capture_only", "", "checkbox");
        audioSinkCaptureOnly.checked = settings.audio_sink_capture_only;
        const externalIp = input("external_ip", settings.configured_external_ip ?? "");
        externalIp.maxLength = 4096;
        const videoBatchSize = input(
          "video_max_batch_size_kb",
          settings.configured_video_max_batch_size_kb === null
            ? ""
            : String(settings.configured_video_max_batch_size_kb),
          "number",
        );
        videoBatchSize.min = "0";
        videoBatchSize.max = "64";
        videoBatchSize.step = "1";
        videoBatchSize.placeholder = "Default (64)";
        const maxBitrate = input(
          "max_bitrate",
          settings.configured_max_bitrate === null
            ? ""
            : String(settings.configured_max_bitrate),
        );
        maxBitrate.placeholder = "No host cap";
        const minimumFpsTarget = input(
          "minimum_fps_target",
          settings.configured_minimum_fps_target === null
            ? ""
            : String(settings.configured_minimum_fps_target),
          "number",
        );
        minimumFpsTarget.min = "0";
        minimumFpsTarget.max = "1000";
        minimumFpsTarget.step = "any";
        minimumFpsTarget.placeholder = "Default (20)";
        const pingTimeout = input(
          "ping_timeout_ms",
          settings.configured_ping_timeout_ms === null
            ? ""
            : String(settings.configured_ping_timeout_ms),
          "number",
        );
        pingTimeout.min = "0";
        pingTimeout.max = "2147483647";
        pingTimeout.step = "1";
        pingTimeout.placeholder = "Default (10000)";
        const status = el("div");
        const save = el("button", "button primary", "Save host settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field("Name shown to Moonlight", hostname, "Leave blank to use this PC's name."),
          field("Moonlight HTTP port", port, "HTTPS uses the port five below this value."),
          field(
            "Address family",
            addressFamily,
            "Choose IPv4 only, both families, or retain the startup policy.",
          ),
          field(
            "Use startup bind address",
            useStartupBindAddress,
            `The effective bind address is ${settings.bind_address}. Turn this off to choose an explicit address; a blank value is distinct from startup policy.`,
          ),
          field(
            "Bind address override",
            bindAddress,
            "Use a literal IPv4 or IPv6 address. An explicit blank uses the startup policy's wildcard/loopback behavior.",
          ),
          field(
            "Capture backend",
            capture,
            `Automatic uses the startup backend. The current backend is ${settings.capture_provider ?? "unavailable"}; explicit Windows modes fail if unavailable. Save and reopen settings to list outputs for a new backend.`,
          ),
          field(
            "Capture output",
            outputName,
            "Choose a display for future Moonlight streams. Automatic retains the configured startup output.",
          ),
          field("Stream audio", streamAudio, "Enable host audio capture for future streams. Applications and paired clients may override this setting."),
          field(
            "WGC pacing smoothing",
            wgcPacing,
            `Preserve the frame pacing grid after eligible Windows WGC capture misses. Current effective policy: ${settings.effective_wgc_pacing_smoothing ? "enabled" : "disabled"}. Constant-rate WGC currently lacks fresh-frame identity for this policy.`,
          ),
          field("Audio capture endpoint", audioSink, `Choose the output device used for loopback capture in future streams. Automatic uses the startup endpoint. Current effective endpoint: ${settings.effective_audio_sink}.`),
          field("Set a virtual audio sink", virtualSinkEnabled, "Keep the selector unset to use the regular capture endpoint. An explicitly empty selector is preserved as distinct from unset; Amaterasu currently cannot perform Sunshine's host-audio fallback selection for it."),
          field("Virtual sink ID or name", virtualSink, "Match a Windows virtual output by ID, friendly name, device description, or adapter name. The selected virtual sink takes precedence over the regular endpoint for future streams."),
          field("Capture selected audio endpoint without changing default outputs", audioSinkCaptureOnly, "For an explicitly selected Windows endpoint, keep the host's Console, Multimedia, and Communications default outputs. The setting applies to future streams."),
          field("External IP", externalIp, "Stored for configuration parity. This host does not currently advertise or detect an external IP from this setting; blank resets it."),
          field(
            "Video batch size (KiB)",
            videoBatchSize,
            `Use 16, 32, or 64 KiB; blank or 0 selects 64 KiB. Current effective size: ${settings.effective_video_max_batch_size_kb} KiB${settings.video_batch_size_fallback ? " (configured value fell back to 64 KiB)" : ""}.`,
          ),
          field(
            "Maximum bitrate (kbps)",
            maxBitrate,
            `A positive value caps the host's initial stream bitrate. Blank resets to no host cap; zero or negative values also leave it uncapped. Caps below 100 kbps cannot launch with the current encoder. Current cap: ${settings.effective_max_bitrate_kbps === null ? "none" : `${settings.effective_max_bitrate_kbps} kbps`}.`,
          ),
          field(
            "Minimum frame rate (FPS)",
            minimumFpsTarget,
            `On idle capture, repeat the last accepted frame to target this rate. Zero uses the negotiated stream FPS; blank resets to 20 FPS. Current effective target: ${settings.effective_minimum_fps_target} FPS.`,
          ),
          field(
            "Stream ping timeout (ms)",
            pingTimeout,
            `Disconnect a stream after this much time without a ping. Zero expires immediately; blank resets to the default. Current effective timeout: ${settings.effective_ping_timeout_ms} ms.`,
          ),
          ...(outputDiscoveryIssue ? [el("p", "muted", outputDiscoveryIssue)] : []),
          ...(audioDiscoveryIssue ? [el("p", "muted", audioDiscoveryIssue)] : []),
          el(
            "p",
            "muted",
            `Active HTTP addresses: ${settings.http_addresses?.join(", ") || "off"} · HTTPS: ${settings.https_addresses?.join(", ") || "off"} · RTSP: ${settings.rtsp_addresses?.join(", ") || "off"}`,
          ),
          el("p", "muted", `Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}`),
          ...(settings.listener_error ? [el("p", "error", settings.listener_error)] : []),
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            const network = parseNvhttpNetworkOverrides(
              addressFamily.value,
              useStartupBindAddress.checked,
              bindAddress.value,
            );
            await context.api.post("nvhttp/settings_update", {
              hostname: hostname.value,
              port: port.valueAsNumber,
              ...network,
              capture: parseCaptureSelection(capture.value),
              output_name: parseOutputNameSelection(outputName.value),
              stream_audio: streamAudio.checked,
              video_max_batch_size_kb: parseVideoMaxBatchSize(videoBatchSize.value),
              max_bitrate: parseMaxBitrate(maxBitrate.value),
              minimum_fps_target: parseMinimumFpsTarget(minimumFpsTarget.value),
              ping_timeout_ms: parsePingTimeout(pingTimeout.value),
              audio_sink: parseAudioSinkSelection(audioSink.value),
              virtual_sink: parseVirtualSinkSelection(virtualSinkEnabled.checked, virtualSink.value),
              audio_sink_capture_only: audioSinkCaptureOnly.checked,
              external_ip: parseExternalIp(externalIp.value),
              wgc_pacing_smoothing: parseWgcPacingSmoothing(wgcPacing.value),
              expected_revision: settings.revision,
            });
            context.notify("Host settings saved.");
            await loadHost();
          });
        });
        hostArea.replaceChildren(panel("Moonlight host name", form));
      } catch (error) {
        if (context.content.isConnected)
          hostArea.replaceChildren(errorPanel(error, loadHost));
      }
    }
    async function load(): Promise<void> {
      area.replaceChildren(el("p", "muted", "Loading settings…"));
      try {
        const document =
          await context.api.post<Configuration>("configuration/get");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const inputs = new Map<
          string,
          HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement
        >();
        for (const [name, definition] of Object.entries(document.fields)) {
          const value = document.values[name] ?? definition.default;
          let control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
          if (definition.type === "enum") {
            const select = el("select");
            select.name = name;
            for (const optionValue of definition.options) {
              const option = el("option", "", optionValue);
              option.value = optionValue;
              select.append(option);
            }
            select.value = String(value);
            control = select;
          } else if (
            definition.type === "list" ||
            definition.type === "object"
          ) {
            const editor = el("textarea");
            editor.name = name;
            editor.rows = 6;
            editor.value = JSON.stringify(value, null, 2);
            control = editor;
          } else {
            control = input(
              name,
              String(value),
              definition.type === "boolean"
                ? "checkbox"
                : definition.type === "integer" ||
                    definition.type === "decimal"
                  ? "number"
                  : "text",
            );
          }
          if (definition.type === "boolean")
            (control as HTMLInputElement).checked = Boolean(value);
          if (definition.type === "integer" || definition.type === "decimal") {
            const numericControl = control as HTMLInputElement;
            numericControl.step = definition.type === "integer" ? "1" : "any";
            if (definition.minimum != null)
              numericControl.min = String(definition.minimum);
            if (definition.maximum != null)
              numericControl.max = String(definition.maximum);
          }
          if (definition.type === "string") {
            const textControl = control as HTMLInputElement;
            if (definition.min_length != null)
              textControl.minLength = definition.min_length;
            if (definition.max_length != null)
              textControl.maxLength = definition.max_length;
          }
          inputs.set(name, control);
          form.append(field(name, control, `Type: ${definition.type}`));
        }
        const status = el("div");
        const save = el("button", "button primary", "Save settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          status,
          el("div", "actions", save, badge(`Revision ${document.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            const values: Payload = {};
            for (const [name, control] of inputs) {
              const definition = document.fields[name];
              if (definition) values[name] = fieldValue(definition, control);
            }
            await context.api.post("configuration/update", {
              values,
              expected_revision: document.revision,
            });
            context.notify("Settings saved.");
            await load();
          });
        });
        area.replaceChildren(
          Object.keys(document.fields).length
            ? panel("Registered values", form)
            : empty(
                "No settings fields",
                "The registered schema is empty. Schema setup and migration are available through the management API.",
              ),
        );
      } catch (error) {
        if (!context.content.isConnected) return;
        area.replaceChildren(
          error instanceof ApiError && error.code === "not_registered"
            ? empty(
                "No settings schema yet",
                "Register a schema for the management principal through the authenticated configuration API. This screen will then show its editable values.",
              )
            : errorPanel(error, load),
        );
      }
    }
    void load();
    void loadAuth();
    void loadHost();
    void loadSteam();
    void loadLutris();
    void loadPlaynite();
    void loadInput();
    void loadHistorySettings();
    void loadPairing();
  }
}
