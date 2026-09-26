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
    enabled: unknown,
    ttlDays: unknown,
    sizeLimitMb: unknown,
  ): HistorySettingsReply["settings"] {
    if (
      typeof enabled !== "boolean" ||
      typeof ttlDays !== "number" ||
      !Number.isSafeInteger(ttlDays) ||
      ttlDays < 0 ||
      ttlDays > maxHistoryLimit ||
      typeof sizeLimitMb !== "number" ||
      !Number.isSafeInteger(sizeLimitMb) ||
      sizeLimitMb < 0 ||
      sizeLimitMb > maxHistoryLimit
    )
      throw new Error("History limits must be whole numbers from 0 to 2147483647.");
    return { enabled, ttl_days: ttlDays, db_size_limit_mb: sizeLimitMb };
  }

  export function parseHistorySettingsReply(value: unknown): HistorySettingsReply {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Session history settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true)
      throw new Error("Session history settings reply must report success.");
    if (reply.settings === null || typeof reply.settings !== "object" || Array.isArray(reply.settings))
      throw new Error("Session history settings reply contains invalid settings.");
    if (
      typeof reply.revision !== "number" ||
      !Number.isSafeInteger(reply.revision) ||
      reply.revision < 0
    )
      throw new Error("Session history settings revision must be a nonnegative safe integer.");
    if (typeof reply.effective_enabled !== "boolean")
      throw new Error("Session history effective enabled state must be a boolean.");

    const settings = reply.settings as Record<string, unknown>;
    return {
      ok: true,
      settings: parseHistorySettings(settings.enabled, settings.ttl_days, settings.db_size_limit_mb),
      revision: reply.revision,
      effective_enabled: reply.effective_enabled,
    };
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

  export async function saveHistorySettings(
    api: Pick<Api, "post">,
    current: HistorySettingsReply,
    enabled: boolean,
    ttlDays: number,
    sizeLimitMb: number,
  ): Promise<HistorySettingsReply> {
    const settings = parseHistorySettings(enabled, ttlDays, sizeLimitMb);
    const updateReply = await api.post<unknown>("history/settings_update", {
      settings,
      expected_revision: current.revision,
    });
    if (updateReply && typeof updateReply === "object" &&
        (updateReply as Record<string, unknown>).code === "revision_conflict")
      throw new Error("Session history settings changed in another editor. Refresh and try again.");
    const updated = parseHistorySettingsReply(updateReply);
    if (updated.revision !== current.revision + 1 ||
        updated.settings.enabled !== settings.enabled ||
        updated.settings.ttl_days !== settings.ttl_days ||
        updated.settings.db_size_limit_mb !== settings.db_size_limit_mb)
      throw new Error("Session history update did not confirm the requested settings. Refresh and check the current values.");
    const saved = parseHistorySettingsReply(await api.post<unknown>("history/settings_get"));
    if (saved.revision !== current.revision + 1 ||
        saved.settings.enabled !== settings.enabled ||
        saved.settings.ttl_days !== settings.ttl_days ||
        saved.settings.db_size_limit_mb !== settings.db_size_limit_mb)
      throw new Error("Session history settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  export async function readHistorySettingsState(
    api: Pick<Api, "post">,
  ): Promise<{ settings: HistorySettingsReply; runtime: HistoryRuntimeStatus | null }> {
    const settings = parseHistorySettingsReply(await api.post<unknown>("history/settings_get"));
    return { settings, runtime: await readHistoryRuntime(api) };
  }

  export class HistorySettingsLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(api: Pick<Api, "post">): Promise<{
      settings: HistorySettingsReply;
      runtime: HistoryRuntimeStatus | null;
    } | null> {
      const generation = ++this.generation;
      try {
        const state = await readHistorySettingsState(api);
        return generation === this.generation ? state : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
  }

  async function readHistoryRuntime(api: Pick<Api, "post">): Promise<HistoryRuntimeStatus | null> {
    try {
      return Amaterasu.parseHistoryRuntimeStatus(await api.post<unknown>("history/history_status"));
    } catch {
      return null;
    }
  }

  export async function saveHistorySettingsState(
    api: Pick<Api, "post">,
    current: HistorySettingsReply,
    enabled: boolean,
    ttlDays: number,
    sizeLimitMb: number,
  ): Promise<{ settings: HistorySettingsReply; runtime: HistoryRuntimeStatus | null }> {
    const settings = await saveHistorySettings(api, current, enabled, ttlDays, sizeLimitMb);
    return { settings, runtime: await readHistoryRuntime(api) };
  }

  export function historySettingsRuntimeDescription(
    settings: HistorySettingsReply,
    runtime: HistoryRuntimeStatus | null,
  ): string {
    if (runtime === null)
      return "Saved history settings confirmed. Runtime status is unavailable; recording is not confirmed.";
    if (!runtime.available)
      return "Saved history settings confirmed. History storage is unavailable; recording is not active.";
    if (!runtime.configuration_ready)
      return "Saved history settings confirmed. History configuration is not ready; recording is not confirmed.";
    if (runtime.enabled !== settings.settings.enabled)
      return "Saved history settings confirmed. Runtime has not yet reported the saved recording policy.";
    const policy = runtime.effective_enabled !== runtime.enabled
      ? runtime.enabled
        ? "Recording enable is deferred until active streams end; new streams still use the disabled policy."
        : "Recording disable is deferred until active streams end; new streams still use the enabled policy."
      : runtime.effective_enabled
        ? "Recording is enabled for new streams."
        : "Recording is disabled for new streams.";
    return runtime.degraded
      ? `Saved history settings confirmed. ${policy} History runtime is degraded; check failed operations and retention status.`
      : `Saved history settings confirmed. ${policy}`;
  }

  export async function saveAuthTokenLifetimes(
    api: Pick<Api, "post">,
    revision: number,
    sessionTokenTtlSeconds: number,
    rememberMeRefreshTokenTtlSeconds: number,
  ): Promise<AuthTokenSettings> {
    if (!Number.isSafeInteger(revision) || revision < 0)
      throw new Error("Authentication settings revision is invalid.");
    const values = parseAuthTokenLifetimes(
      sessionTokenTtlSeconds,
      rememberMeRefreshTokenTtlSeconds,
    );
    const updated = parseAuthTokenSettingsReply(await api.post<unknown>("auth/settings_update", {
      ...values,
      expected_revision: revision,
    }));
    if (updated.revision !== revision + 1 ||
        updated.values.session_token_ttl_seconds !== values.session_token_ttl_seconds ||
        updated.values.remember_me_refresh_token_ttl_seconds !== values.remember_me_refresh_token_ttl_seconds)
      throw new Error("Authentication settings update could not be confirmed. Refresh and check the current values.");
    const saved = parseAuthTokenSettingsReply(await api.post<unknown>("auth/settings"));
    if (
      saved.revision !== updated.revision ||
      saved.values.session_token_ttl_seconds !== values.session_token_ttl_seconds ||
      saved.values.remember_me_refresh_token_ttl_seconds !== values.remember_me_refresh_token_ttl_seconds
    )
      throw new Error("Authentication settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  export function parseAuthTokenSettingsReply(value: unknown): AuthTokenSettings {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Authentication settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        reply.values === null || typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error("Authentication settings reply is invalid.");
    const values = reply.values as Record<string, unknown>;
    return {
      ok: true,
      revision: reply.revision as number,
      values: parseAuthTokenLifetimes(
        values.session_token_ttl_seconds as number,
        values.remember_me_refresh_token_ttl_seconds as number,
      ),
    };
  }

  export class AuthTokenSettingsLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(api: Pick<Api, "post">): Promise<AuthTokenSettings | null> {
      const generation = ++this.generation;
      try {
        const settings = parseAuthTokenSettingsReply(await api.post<unknown>("auth/settings"));
        return generation === this.generation ? settings : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
  }

  export function parseRegisteredConfigurationReply(value: unknown): Configuration {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Registered settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        reply.fields === null || typeof reply.fields !== "object" || Array.isArray(reply.fields) ||
        reply.values === null || typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error("Registered settings reply is invalid.");
    return reply as unknown as Configuration;
  }

  function sameConfigurationValue(left: unknown, right: unknown): boolean {
    if (left === right) return true;
    if (Array.isArray(left) && Array.isArray(right))
      return left.length === right.length && left.every((value, index) => sameConfigurationValue(value, right[index]));
    if (!left || !right || typeof left !== "object" || typeof right !== "object" ||
        Array.isArray(left) || Array.isArray(right)) return false;
    const leftObject = left as Record<string, unknown>;
    const rightObject = right as Record<string, unknown>;
    const keys = Object.keys(leftObject);
    return keys.length === Object.keys(rightObject).length &&
      keys.every((key) => Object.hasOwn(rightObject, key) && sameConfigurationValue(leftObject[key], rightObject[key]));
  }

  export async function saveRegisteredConfiguration(
    api: Pick<Api, "post">,
    current: Configuration,
    values: Payload,
  ): Promise<Configuration> {
    const expectedValues = { ...current.values, ...values };
    const confirms = (reply: Configuration): boolean =>
      reply.revision === current.revision + 1 &&
      sameConfigurationValue(reply.fields, current.fields) &&
      sameConfigurationValue(reply.values, expectedValues);
    const updated = parseRegisteredConfigurationReply(await api.post<unknown>("configuration/update", {
      values,
      expected_revision: current.revision,
    }));
    if (!confirms(updated))
      throw new Error("Registered settings update could not be confirmed. Refresh and check the current values.");
    const saved = parseRegisteredConfigurationReply(await api.post<unknown>("configuration/get"));
    if (!confirms(saved))
      throw new Error("Registered settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  export class RegisteredConfigurationLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(api: Pick<Api, "post">): Promise<Configuration | null> {
      const generation = ++this.generation;
      try {
        const settings = parseRegisteredConfigurationReply(await api.post<unknown>("configuration/get"));
        return generation === this.generation ? settings : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
  }

  export function parseNvhttpSettingsReply(value: unknown): NvhttpSettings {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Moonlight host settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        typeof reply.configured_hostname !== "string" ||
        !Number.isSafeInteger(reply.port) ||
        !Array.isArray(reply.configured_global_preparation))
      throw new Error("Moonlight host settings reply is invalid.");
    return reply as unknown as NvhttpSettings;
  }

  function normalizedPreparationSteps(value: unknown): unknown {
    if (!Array.isArray(value)) return value;
    const command = (raw: Record<string, unknown>): Record<string, unknown> => ({
      executable: raw.executable,
      args: raw.args ?? [],
      environment: raw.environment ?? {},
      cwd: raw.cwd ?? null,
      timeout_ms: raw.timeout_ms ?? 10_000,
    });
    return value.map((step: PreparationStep) => ({
      run: command(step.run as unknown as Record<string, unknown>),
      undo: step.undo ? command(step.undo as unknown as Record<string, unknown>) : null,
    }));
  }

  export async function saveNvhttpHostSettings(
    api: Pick<Api, "post">,
    current: NvhttpSettings,
    values: Payload,
  ): Promise<NvhttpSettings> {
    const preserved = ["port", "stream_audio", "audio_sink_capture_only", "nvenc_spatial_aq",
      "lan_encryption_mode", "wan_encryption_mode"];
    const expected: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(current))
      if (key.startsWith("configured_") || preserved.includes(key)) expected[key] = value;
    Object.assign(expected, {
      configured_hostname: values.hostname,
      port: values.port,
      configured_address_family: values.address_family,
      configured_bind_address: values.bind_address,
      configured_capture: values.capture,
      configured_output_name: values.output_name === "" ? null : values.output_name,
      stream_audio: values.stream_audio,
      configured_video_max_batch_size_kb: values.video_max_batch_size_kb,
      configured_max_bitrate: values.max_bitrate,
      configured_minimum_fps_target: values.minimum_fps_target,
      configured_ping_timeout_ms: values.ping_timeout_ms === -1 ? null : values.ping_timeout_ms,
      configured_audio_sink: values.audio_sink,
      configured_virtual_sink: values.virtual_sink,
      audio_sink_capture_only: values.audio_sink_capture_only,
      configured_external_ip: values.external_ip,
      configured_wgc_pacing_smoothing: values.wgc_pacing_smoothing,
      configured_global_preparation: normalizedPreparationSteps(values.global_preparation),
    });
    const confirms = (reply: NvhttpSettings): boolean =>
      reply.revision === current.revision + 1 &&
      Object.entries(expected).every(([key, value]) =>
        sameConfigurationValue((reply as unknown as Record<string, unknown>)[key], value));
    const updated = parseNvhttpSettingsReply(await api.post<unknown>("nvhttp/settings_update", {
      ...values,
      expected_revision: current.revision,
    }));
    if (!confirms(updated))
      throw new Error("Moonlight host update could not be confirmed. Refresh and check the current values.");
    const saved = parseNvhttpSettingsReply(await api.post<unknown>("nvhttp/settings"));
    if (!confirms(saved))
      throw new Error("Moonlight host settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  export class NvhttpSettingsLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    isCurrent(generation: number): boolean {
      return generation === this.generation;
    }

    async read(api: Pick<Api, "post">): Promise<{ settings: NvhttpSettings; generation: number } | null> {
      const generation = ++this.generation;
      try {
        const settings = parseNvhttpSettingsReply(await api.post<unknown>("nvhttp/settings"));
        return this.isCurrent(generation) ? { settings, generation } : null;
      } catch (error) {
        if (!this.isCurrent(generation)) return null;
        throw error;
      }
    }
  }

  export async function recoverNvhttpSaveFailure(
    code: string,
    reload: () => Promise<void>,
    notify: (message: string) => void,
  ): Promise<boolean> {
    if (code !== "revision_conflict" &&
        !["listener_apply_pending", "listener_apply_failed", "listener_not_active"].includes(code))
      return false;
    await reload();
    notify(code === "revision_conflict"
      ? "Host settings changed elsewhere. Review the latest values before saving."
      : "Host settings were stored, but listener application is not confirmed. Review the active ports and listener error.");
    return true;
  }

  const hostLogLevels: HostLogLevel[] = [
    "product_default", "verbose", "debug", "info", "warning", "error", "fatal", "none",
  ];

  export function parseHostLoggingSettings(level: string, path: string): {
    min_log_level: HostLogLevel;
    log_path: string;
  } {
    if (!hostLogLevels.includes(level as HostLogLevel))
      throw new Error("Choose a supported logging level.");
    const pathComponents = path.split(/[\\/]/u).filter(Boolean);
    if (
      !path ||
      new TextEncoder().encode(path).length > 4096 ||
      /[\u0000-\u001f\u007f-\u009f]/u.test(path) ||
      path.split(/[\\/]/u).includes("..") ||
      (pathComponents.length > 0 && pathComponents.every((component) => component === ".")) ||
      /^[a-zA-Z]:(?![\\/])/u.test(path)
    )
      throw new Error("Enter a log path of at most 4096 UTF-8 bytes without controls or parent traversal.");
    return { min_log_level: level as HostLogLevel, log_path: path };
  }

  export function parseHostConfigurationReply(reply: unknown): HostConfiguration {
    if (!reply || typeof reply !== "object" || Array.isArray(reply))
      throw new Error("Host configuration reply is invalid.");
    const document = reply as Record<string, unknown>;
    if (document.ok !== true || document.ready !== true || document.version !== 1 ||
        !Number.isSafeInteger(document.revision) || (document.revision as number) < 0)
      throw new Error("Host configuration version or revision is invalid.");
    if (!document.values || typeof document.values !== "object" || Array.isArray(document.values) ||
        !document.effective || typeof document.effective !== "object" || Array.isArray(document.effective))
      throw new Error("Host configuration values are invalid.");
    const values = document.values as Record<string, unknown>;
    const effective = document.effective as Record<string, unknown>;
    for (const key of ["file_state", "vibeshine_file_state", "credentials_file", "pkey", "cert", "log_path"])
      if (typeof values[key] !== "string")
        throw new Error(`Host configuration ${key} is invalid.`);
    if (typeof effective.log_path !== "string" ||
        !hostLogLevels.includes(effective.min_log_level as HostLogLevel) ||
        !hostLogLevels.includes(values.min_log_level as HostLogLevel))
      throw new Error("Effective host logging settings are invalid.");
    return document as unknown as HostConfiguration;
  }

  export async function saveHostLoggingSettings(
    api: Pick<Api, "post">,
    current: HostConfiguration,
    level: string,
    path: string,
  ): Promise<HostConfiguration> {
    const logging = parseHostLoggingSettings(level, path);
    const updated = parseHostConfigurationReply(await api.post<unknown>("host-configuration/update", {
      ...current.values,
      ...logging,
      expected_revision: current.revision,
    }));
    if (!confirmsHostLoggingUpdate(updated, current, logging))
      throw new Error("Host logging update could not be confirmed. Refresh and check the current values.");
    const saved = parseHostConfigurationReply(await api.post<unknown>("host-configuration/get"));
    if (!confirmsHostLoggingUpdate(saved, current, logging))
      throw new Error("Host logging settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  function confirmsHostLoggingUpdate(
    reply: HostConfiguration,
    current: HostConfiguration,
    logging: { min_log_level: HostLogLevel; log_path: string },
  ): boolean {
    return reply.revision === current.revision + 1 &&
      reply.values.log_path === logging.log_path &&
      reply.values.min_log_level === logging.min_log_level &&
      ["file_state", "vibeshine_file_state", "credentials_file", "pkey", "cert"].every(
        (key) => reply.values[key as keyof typeof reply.values] === current.values[key as keyof typeof current.values],
      );
  }

  type DiagnosticsSinkStatus = DiagnosticsLoggingStatus & {
    logging_sink?: { state: "disabled" | "healthy" | "failed"; failure: "open" | "write" | null };
  };

  type DiagnosticsStatusRead = {
    status: DiagnosticsSinkStatus | null;
    error: "permission" | "request" | "invalid" | null;
  };

  export function parseDiagnosticsLoggingStatus(value: unknown): DiagnosticsSinkStatus {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Diagnostics status is invalid.");
    const status = value as Record<string, unknown>;
    const revision = status.logging_configuration_revision;
    if (status.ok !== true || typeof status.ready !== "boolean" ||
        (revision !== null && (!Number.isSafeInteger(revision) || (revision as number) < 1)))
      throw new Error("Diagnostics logging status is invalid.");
    if (Object.hasOwn(status, "logging_sink")) {
      const sink = status.logging_sink;
      if (!sink || typeof sink !== "object" || Array.isArray(sink))
        throw new Error("Diagnostics logging sink status is invalid.");
      const { state, failure } = sink as Record<string, unknown>;
      if ((state !== "disabled" && state !== "healthy" && state !== "failed") ||
          (state === "failed" ? failure !== "open" && failure !== "write" : failure !== null))
        throw new Error("Diagnostics logging sink status is invalid.");
    }
    return status as unknown as DiagnosticsSinkStatus;
  }

  export async function readHostLoggingState(
    api: Pick<Api, "post"> & Partial<Pick<Api, "allows">>,
  ): Promise<{ settings: HostConfiguration; diagnostics: DiagnosticsStatusRead }> {
    const settings = parseHostConfigurationReply(await api.post<unknown>("host-configuration/get"));
    return { settings, diagnostics: await readDiagnosticsLoggingStatus(api) };
  }

  export class HostLoggingLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(api: Pick<Api, "post"> & Partial<Pick<Api, "allows">>): Promise<{
      settings: HostConfiguration;
      diagnostics: DiagnosticsStatusRead;
    } | null> {
      const generation = ++this.generation;
      try {
        const state = await readHostLoggingState(api);
        return generation === this.generation ? state : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
  }

  async function readDiagnosticsLoggingStatus(
    api: Pick<Api, "post"> & Partial<Pick<Api, "allows">>,
  ): Promise<DiagnosticsStatusRead> {
    if (api.allows && !api.allows("diagnostics.read"))
      return { status: null, error: "permission" };
    let reply: unknown;
    try {
      reply = await api.post<unknown>("diagnostics/status");
    } catch {
      return { status: null, error: "request" };
    }
    try {
      return { status: parseDiagnosticsLoggingStatus(reply), error: null };
    } catch {
      return { status: null, error: "invalid" };
    }
  }

  export async function saveHostLoggingState(
    api: Pick<Api, "post"> & Partial<Pick<Api, "allows">>,
    current: HostConfiguration,
    level: string,
    path: string,
  ): Promise<{ settings: HostConfiguration; diagnostics: DiagnosticsStatusRead }> {
    const settings = await saveHostLoggingSettings(api, current, level, path);
    return { settings, diagnostics: await readDiagnosticsLoggingStatus(api) };
  }

  export function diagnosticsLoggingDescription(
    savedRevision: number,
    result: DiagnosticsStatusRead,
  ): string {
    if (result.error === "permission")
      return "Saved host logging policy; diagnostics status requires diagnostics.read permission.";
    if (result.error === "request")
      return "Saved host logging policy; diagnostics status request failed. Refresh to retry.";
    if (result.error === "invalid")
      return "Saved host logging policy; diagnostics returned invalid status. Refresh to retry.";
    const status = result.status;
    if (!status)
      return "Saved host logging policy; diagnostics status is unavailable. Refresh to check whether it applied.";
    if (!status.ready)
      return "Saved host logging policy; diagnostics is not ready. Refresh after it starts.";
    if (status.logging_configuration_revision === savedRevision) {
      if (!status.logging_sink)
        return `Diagnostics is using saved host logging revision ${savedRevision}; disk delivery is unverified.`;
      if (status.logging_sink.state === "disabled")
        return `Diagnostics is using saved host logging revision ${savedRevision}; disk logging is disabled.`;
      if (status.logging_sink.state === "failed")
        return status.logging_sink.failure === "open"
          ? `Diagnostics is using saved host logging revision ${savedRevision}, but opening the disk log failed. Disk delivery is not working.`
          : `Diagnostics is using saved host logging revision ${savedRevision}, but writing the disk log failed. Disk delivery is not working.`;
      return `Diagnostics is using saved host logging revision ${savedRevision}; disk logging is healthy.`;
    }
    if (status.logging_configuration_revision === null || status.logging_configuration_revision < savedRevision)
      return `Saved host logging revision ${savedRevision} is pending in diagnostics. Refresh to check again.`;
    return `Diagnostics is using a newer host logging revision ${status.logging_configuration_revision}. Refresh the saved policy.`;
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
    nativePenTouch: boolean = true,
  ): Omit<InputPolicySettings["values"], "ds4_back_as_touchpad_click"> {
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
      typeof controllerEnabled !== "boolean" ||
      typeof nativePenTouch !== "boolean"
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
      native_pen_touch: nativePenTouch,
    };
  }

  const recognizedInputGamepadProfiles = [
    "auto", "x360", "vhf", "vhf_xbox", "vhf_xbox_one", "vhf_ds4", "vhf_ds5", "vhf_switch",
    "ds4", "xone", "ds5", "switch",
  ];

  export function parseInputPolicySettingsReply(value: unknown): InputPolicySettings {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Input policy settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || reply.persisted !== true ||
        typeof reply.applied !== "boolean" || typeof reply.cleanup_required !== "boolean" ||
        !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        !reply.values || typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error("Input policy settings reply is invalid.");
    const values = reply.values as Record<string, unknown>;
    if (typeof values.ds4_back_as_touchpad_click !== "boolean")
      throw new Error("Input policy settings reply omitted the DS4 back button policy.");
    const parsed = {
      ...parseInputPolicy(
        values.keyboard as boolean,
        values.mouse as boolean,
        values.high_resolution_scrolling as boolean,
        values.always_send_scancodes as boolean,
        values.key_rightalt_to_key_win as boolean,
        values.key_repeat_delay as number,
        values.key_repeat_frequency as number,
        values.back_button_timeout as number,
        values.gamepad as string,
        recognizedInputGamepadProfiles,
        values.controller as boolean,
        values.native_pen_touch as boolean,
      ),
      ds4_back_as_touchpad_click: values.ds4_back_as_touchpad_click,
    };
    if (!sameConfigurationValue(values, parsed))
      throw new Error("Input policy settings reply contains unexpected values.");
    return {
      ok: true,
      persisted: true,
      applied: reply.applied as boolean,
      cleanup_required: reply.cleanup_required as boolean,
      revision: reply.revision as number,
      values: parsed,
    };
  }

  export async function saveInputPolicySettings(
    api: Pick<Api, "post">,
    current: InputPolicySettings,
    values: InputPolicySettings["values"],
  ): Promise<InputPolicySettings> {
    if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
        current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error("Input policy settings revision is invalid.");
    const confirms = (reply: InputPolicySettings): boolean =>
      reply.revision === current.revision + 1 && sameConfigurationValue(reply.values, values);
    try {
      const updated = parseInputPolicySettingsReply(await api.post<unknown>("input/settings_update", {
        ...values,
        expected_revision: current.revision,
      }));
      if (!confirms(updated))
        throw new Error("Input policy update could not be confirmed. Refresh and check the current values.");
    } catch (error) {
      if (!(error instanceof ApiError) || error.code !== "cleanup_pending") throw error;
    }
    const saved = parseInputPolicySettingsReply(await api.post<unknown>("input/settings_get"));
    if (!confirms(saved))
      throw new Error("Input policy settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  function parseInputRuntimeStatus(value: unknown): InputRuntimeStatus {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Input runtime status is invalid.");
    const status = value as Record<string, unknown>;
    if (status.ok !== true || typeof status.gamepad !== "string" ||
        !Array.isArray(status.supported_profiles) ||
        !status.supported_profiles.every((profile) => typeof profile === "string") ||
        !Array.isArray(status.recognized_profiles) ||
        !status.recognized_profiles.every((profile) => typeof profile === "string") ||
        typeof status.controller_input !== "boolean" ||
        typeof status.controller_enabled !== "boolean" ||
        typeof status.controller_cleanup_required !== "boolean" ||
        (status.lease !== null && (!status.lease || typeof status.lease !== "object" || Array.isArray(status.lease))) ||
        (status.gamepad_unavailable_reason !== null && typeof status.gamepad_unavailable_reason !== "string"))
      throw new Error("Input runtime status is invalid.");
    return status as unknown as InputRuntimeStatus;
  }

  export class InputPolicyLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(api: Pick<Api, "post">): Promise<{
      settings: InputPolicySettings;
      runtime: InputRuntimeStatus | null;
    } | null> {
      const generation = ++this.generation;
      try {
        const settings = parseInputPolicySettingsReply(await api.post<unknown>("input/settings_get"));
        if (generation !== this.generation) return null;
        let runtime: InputRuntimeStatus | null = null;
        try {
          runtime = parseInputRuntimeStatus(await api.post<unknown>("input/status"));
        } catch {
          // Configuration remains editable when dynamic runtime status is unavailable.
        }
        return generation === this.generation ? { settings, runtime } : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
  }

  export interface GamepadProfileChoice {
    value: string;
    label: string;
    available: boolean;
  }

  export function inputGamepadProfileChoices(
    selectedProfile: string,
    supportedProfiles: readonly string[],
    recognizedProfiles: readonly string[] = supportedProfiles,
  ): GamepadProfileChoice[] {
    if (
      typeof selectedProfile !== "string" ||
      !Array.isArray(supportedProfiles) ||
      !supportedProfiles.every((profile) => typeof profile === "string") ||
      !Array.isArray(recognizedProfiles) ||
      !recognizedProfiles.every((profile) => typeof profile === "string")
    )
      throw new Error("Gamepad profile status is invalid.");
    const availableProfiles = [...new Set(supportedProfiles)];
    if (availableProfiles.length === 0) availableProfiles.push("auto");
    const choices = [...new Set([...recognizedProfiles, ...availableProfiles])].map((value) => ({
      value,
      label: availableProfiles.includes(value)
        ? value
        : `${value} (recognized, unavailable on this backend)`,
      available: availableProfiles.includes(value),
    }));
    if (selectedProfile !== "" && !choices.some((choice) => choice.value === selectedProfile)) {
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
    status: InputRuntimeStatus | null,
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

  const forwardablePairingOverrideFields = [
    "fec_percentage", "stream_audio", "audio_sink", "virtual_sink",
    "audio_sink_capture_only", "back_button_timeout", "controller", "gamepad",
    "keyboard", "mouse", "native_pen_touch",
    "max_bitrate", "minimum_fps_target", "video_max_batch_size_kb",
    "frame_limiter_fps_limit", "frame_limiter_enable", "frame_limiter_provider",
    "frame_limiter_disable_vsync", "frame_limiter_auto_virtual_framegen",
    "rtss_frame_limit_type", "nvenc_spatial_aq", "high_resolution_scrolling",
    "always_send_scancodes", "key_rightalt_to_key_win", "key_repeat_delay",
    "key_repeat_frequency",
  ];

  export function unsupportedPairingOverrideFields(client: PairingClient): string[] {
    const overrides = client.config_overrides ?? {};
    return Object.entries(overrides)
      .filter(([field, value]) => !forwardablePairingOverrideFields.includes(field) ||
        !validStoredPairingOverride(field, value))
      .map(([field]) => field);
  }

  function validStoredPairingOverride(field: string, value: unknown): boolean {
    if (value === null || value === undefined) return false;
    if (["stream_audio", "audio_sink_capture_only", "controller", "keyboard", "mouse",
      "native_pen_touch", "frame_limiter_enable", "frame_limiter_disable_vsync",
      "nvenc_spatial_aq", "high_resolution_scrolling", "always_send_scancodes",
      "key_rightalt_to_key_win"].includes(field)) return typeof value === "boolean";
    if (field === "audio_sink" || field === "virtual_sink")
      return typeof value === "string" && new TextEncoder().encode(value).length <= 4096 &&
        !/[\u0000-\u001f\u007f-\u009f]/.test(value);
    if (field === "gamepad")
      return typeof value === "string" && recognizedInputGamepadProfiles.includes(value);
    if (field === "frame_limiter_provider")
      return typeof value === "string" && ["auto", "rtss", "nvidia-control-panel", "none",
        "mangohud", "proton", "mangohud-proton"].includes(value);
    if (field === "frame_limiter_auto_virtual_framegen")
      return typeof value === "string" && ["enabled", "vrr", "legacy", "disabled"].includes(value);
    if (field === "rtss_frame_limit_type")
      return typeof value === "string" && ["async", "front edge sync", "back edge sync",
        "nvidia reflex"].includes(value);
    if (field === "fec_percentage")
      return Number.isSafeInteger(value) && (value as number) >= 1 && (value as number) <= 255;
    if (field === "video_max_batch_size_kb")
      return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 64;
    if (field === "back_button_timeout" || field === "max_bitrate")
      return Number.isSafeInteger(value) && (value as number) >= -0x8000_0000 &&
        (value as number) <= 0x7fff_ffff;
    if (field === "key_repeat_delay")
      return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 0x7fff_ffff;
    if (field === "frame_limiter_fps_limit")
      return typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= 1000 &&
        Math.abs(value * 1000 - Math.round(value * 1000)) <= 0.000001;
    if (["minimum_fps_target", "key_repeat_frequency"].includes(field))
      return typeof value === "number" && Number.isFinite(value) && value >= 0 &&
        (field === "key_repeat_frequency" || value <= 1000);
    return false;
  }

  export function parsePairingClientsReply(value: unknown): PairingClients {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Paired clients reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) ||
        (reply.revision as number) < 0 || !Array.isArray(reply.clients))
      throw new Error("Paired clients reply is invalid.");
    const seen = new Set<string>();
    for (const value of reply.clients) {
      if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("Paired client metadata is invalid.");
      const client = value as Record<string, unknown>;
      if (typeof client.certificate_id !== "string" ||
          !/^[0-9a-fA-F]{64}$/.test(client.certificate_id) ||
          seen.has(client.certificate_id) ||
          typeof client.client_id !== "string" || typeof client.name !== "string" ||
          !Number.isSafeInteger(client.paired_at) || (client.paired_at as number) < 0 ||
          (client.config_overrides !== undefined &&
            (!client.config_overrides || typeof client.config_overrides !== "object" ||
              Array.isArray(client.config_overrides))))
        throw new Error("Paired client metadata is invalid.");
      seen.add(client.certificate_id);
    }
    return reply as unknown as PairingClients;
  }

  function canonicalPairingOverrides(
    values: Record<string, unknown>,
  ): NonNullable<PairingClient["config_overrides"]> {
    if (Object.keys(values).some((field) => !forwardablePairingOverrideFields.includes(field)))
      throw new Error("The paired client has override fields this editor cannot preserve.");
    return Object.fromEntries(Object.entries(values).filter(([, value]) => value !== null && value !== undefined));
  }

  export async function savePairingClientOverrides(
    api: Pick<Api, "post">,
    current: PairingClients,
    certificateId: string,
    values: Record<string, unknown>,
  ): Promise<PairingClients> {
    const client = current.clients.find((candidate) => candidate.certificate_id === certificateId);
    if (!client || !Number.isSafeInteger(current.revision) || current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error("Paired client or settings revision is invalid.");
    const unsupported = unsupportedPairingOverrideFields(client);
    if (unsupported.length > 0)
      throw new Error(`This client has overrides this editor cannot preserve: ${unsupported.join(", ")}.`);
    const desired = canonicalPairingOverrides({ ...client.config_overrides, ...values });
    const confirms = (reply: PairingClients): boolean => {
      if (reply.revision !== current.revision + 1 || reply.clients.length !== current.clients.length)
        return false;
      return current.clients.every((before) => {
        const after = reply.clients.find((candidate) => candidate.certificate_id === before.certificate_id);
        if (!after || after.client_id !== before.client_id || after.name !== before.name ||
            after.paired_at !== before.paired_at) return false;
        return sameConfigurationValue(after.config_overrides ?? {},
          before.certificate_id === certificateId ? desired : before.config_overrides ?? {});
      });
    };
    const response = await api.post<unknown>("pairing/update_client_config_overrides", {
      certificate_id: certificateId,
      expected_revision: current.revision,
      config_overrides: desired,
    });
    if (!response || typeof response !== "object" || Array.isArray(response) ||
        (response as Record<string, unknown>).ok !== true ||
        (response as Record<string, unknown>).certificate_id !== certificateId)
      throw new Error("Paired client update reply did not confirm the selected certificate.");
    const saved = parsePairingClientsReply(await api.post<unknown>("pairing/list_clients"));
    if (!confirms(saved))
      throw new Error("Paired client overrides could not be confirmed after saving. Refresh and check current values.");
    return saved;
  }

  export class PairingClientsLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(api: Pick<Api, "post">): Promise<{
      settings: PairingClients;
      inputStatus: InputRuntimeStatus | null;
    } | null> {
      const generation = ++this.generation;
      try {
        const settings = parsePairingClientsReply(await api.post<unknown>("pairing/list_clients"));
        if (generation !== this.generation) return null;
        let inputStatus: InputRuntimeStatus | null = null;
        try {
          inputStatus = parseInputRuntimeStatus(await api.post<unknown>("input/status"));
        } catch {
          // Stored paired-client choices remain visible without live input status.
        }
        return generation === this.generation ? { settings, inputStatus } : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
  }

  function sparseInputOverrideSelect(name: string, current: boolean | undefined): HTMLSelectElement {
    const select = el("select");
    select.name = name;
    select.setAttribute("aria-label", `${name} override`);
    const choices: Array<[string, string]> = [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]];
    for (const [value, label] of choices) {
      const option = el("option", "", label);
      option.value = value;
      select.append(option);
    }
    select.value = current === undefined ? "" : String(current);
    return select;
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

  export function inputPolicyRuntimeDescription(
    settings: InputPolicySettings,
    runtime: InputRuntimeStatus | null,
  ): string {
    if (runtime === null)
      return "Saved input policy confirmed. Runtime status is unavailable; application is not confirmed.";
    if (runtime.gamepad !== settings.values.gamepad ||
        runtime.controller_enabled !== settings.values.controller)
      return "Saved input policy confirmed, but runtime has not reported the selected gamepad and controller policy.";
    if (runtime.controller_cleanup_required || settings.cleanup_required || !settings.applied)
      return "Saved input policy confirmed; releasing held native input is still pending.";
    return inputPolicyStatusDescription(settings);
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

  export function parseSteamSettingsReply(value: unknown): SteamSettings {
    const reply = parseImportSettingsEnvelope(value, "Steam");
    const values = reply.values as Record<string, unknown>;
    const imported = parseSteamImportSettings(
      values.include_tools as boolean,
      values.sync_all_installed as boolean,
      values.autosync_remove_uninstalled as boolean,
      values.recent_games as number,
      values.recent_max_age_days as number,
    );
    if (typeof values.enabled !== "boolean" || typeof values.auto_sync !== "boolean")
      throw new Error("Steam settings reply contains invalid switches.");
    const parsed = {
      enabled: values.enabled,
      auto_sync: values.auto_sync,
      exclude_games: parseSteamExclusions(JSON.stringify(values.exclude_games)),
      ...imported,
    };
    if (!sameConfigurationValue(values, parsed))
      throw new Error("Steam settings reply contains unexpected values.");
    return { ok: true, revision: reply.revision, values: parsed };
  }

  export function parseLutrisSettingsReply(value: unknown): LutrisSettings {
    const reply = parseImportSettingsEnvelope(value, "Lutris");
    const values = reply.values as Record<string, unknown>;
    if (typeof values.enabled !== "boolean" || typeof values.auto_sync !== "boolean" ||
        typeof values.autosync_remove_uninstalled !== "boolean" ||
        typeof values.include_steam !== "boolean")
      throw new Error("Lutris settings reply contains invalid switches.");
    const parsed = {
      enabled: values.enabled,
      auto_sync: values.auto_sync,
      autosync_remove_uninstalled: values.autosync_remove_uninstalled,
      include_steam: values.include_steam,
      exclude_games: parseLutrisExclusions(JSON.stringify(values.exclude_games)),
    };
    if (!sameConfigurationValue(values, parsed))
      throw new Error("Lutris settings reply contains unexpected values.");
    return { ok: true, revision: reply.revision, values: parsed };
  }

  function parseImportSettingsEnvelope(
    value: unknown,
    provider: string,
  ): { revision: number; values: Record<string, unknown> } {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error(`${provider} settings reply is invalid.`);
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) ||
        (reply.revision as number) < 0 || !reply.values ||
        typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error(`${provider} settings reply is invalid.`);
    return { revision: reply.revision as number, values: reply.values as Record<string, unknown> };
  }

  async function readImportSettings<T extends SteamSettings | LutrisSettings>(
    api: Pick<Api, "post">,
    provider: "steam" | "lutris",
    parse: (value: unknown) => T,
  ): Promise<T> {
    for (let attempt = 0; ; attempt += 1) {
      try {
        return parse(await api.post<unknown>(`${provider}/settings`));
      } catch (error) {
        if (!(error instanceof ApiError) ||
            !["settings_loading", "settings_busy"].includes(error.code) || attempt >= 7)
          throw error;
        await new Promise<void>((resolve) => window.setTimeout(resolve, 50 * (attempt + 1)));
      }
    }
  }

  async function saveImportSettings<T extends SteamSettings | LutrisSettings>(
    api: Pick<Api, "post">,
    provider: "steam" | "lutris",
    current: T,
    values: T["values"],
    parse: (value: unknown) => T,
  ): Promise<T> {
    if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
        current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error(`${provider} settings revision is invalid.`);
    const confirms = (reply: T): boolean => {
      if (reply.revision !== current.revision + 1) return false;
      if (sameConfigurationValue(reply.values, values)) return true;
      return provider === "steam" && values.enabled === false && reply.values.enabled === true &&
        sameConfigurationValue({ ...reply.values, enabled: false }, values);
    };
    let updated: T;
    try {
      updated = parse(await api.post<unknown>(`${provider}/settings_update`, {
        ...values,
        expected_revision: current.revision,
      }));
    } catch (error) {
      if (!(error instanceof ApiError) ||
          (provider === "steam" ? error.code !== "settings_outcome_unknown" : error.code !== "update_outcome_unknown"))
        throw error;
      const reconciled = await readImportSettings(api, provider, parse);
      if (!confirms(reconciled))
        throw new Error(`${provider} update outcome is uncertain. Refresh and check the current values.`);
      return reconciled;
    }
    if (!confirms(updated))
      throw new Error(`${provider} update could not be confirmed. Refresh and check the current values.`);
    const saved = await readImportSettings(api, provider, parse);
    if (!confirms(saved) || !sameConfigurationValue(saved.values, updated.values))
      throw new Error(`${provider} settings could not be confirmed after saving. Refresh and check the current values.`);
    return saved;
  }

  export function readSteamSettings(api: Pick<Api, "post">): Promise<SteamSettings> {
    return readImportSettings(api, "steam", parseSteamSettingsReply);
  }

  export function readLutrisSettings(api: Pick<Api, "post">): Promise<LutrisSettings> {
    return readImportSettings(api, "lutris", parseLutrisSettingsReply);
  }

  export function saveSteamSettings(
    api: Pick<Api, "post">,
    current: SteamSettings,
    values: SteamSettings["values"],
  ): Promise<SteamSettings> {
    return saveImportSettings(api, "steam", current, values, parseSteamSettingsReply);
  }

  export function saveLutrisSettings(
    api: Pick<Api, "post">,
    current: LutrisSettings,
    values: LutrisSettings["values"],
  ): Promise<LutrisSettings> {
    return saveImportSettings(api, "lutris", current, values, parseLutrisSettingsReply);
  }

  export class ImportSettingsLoadGate<T> {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    isCurrent(generation: number): boolean {
      return generation === this.generation;
    }

    async read(load: () => Promise<T>): Promise<{ value: T; generation: number } | null> {
      const generation = ++this.generation;
      try {
        const value = await load();
        return this.isCurrent(generation) ? { value, generation } : null;
      } catch (error) {
        if (!this.isCurrent(generation)) return null;
        throw error;
      }
    }
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

  export function parsePlayniteSettingsReply(value: unknown): PlayniteSettings {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Playnite settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) ||
        (reply.revision as number) < 0 || !reply.values ||
        typeof reply.values !== "object" || Array.isArray(reply.values))
      throw new Error("Playnite settings reply is invalid.");
    const values = reply.values as PlayniteSettings["values"];
    const parsed = parsePlayniteSettings(values);
    if (!sameConfigurationValue(values, parsed))
      throw new Error("Playnite settings reply contains unexpected values.");
    return { ok: true, revision: reply.revision as number, values: parsed };
  }

  export async function readPlayniteSettings(api: Pick<Api, "post">): Promise<PlayniteSettings> {
    for (let attempt = 0; ; attempt += 1) {
      try {
        return parsePlayniteSettingsReply(await api.post<unknown>("playnite/settings"));
      } catch (error) {
        if (!(error instanceof ApiError) ||
            !["settings_loading", "settings_busy"].includes(error.code) || attempt >= 7)
          throw error;
        await new Promise<void>((resolve) => window.setTimeout(resolve, 50 * (attempt + 1)));
      }
    }
  }

  export async function savePlayniteSettings(
    api: Pick<Api, "post">,
    current: PlayniteSettings,
    values: PlayniteSettings["values"],
  ): Promise<PlayniteSettings> {
    if (!Number.isSafeInteger(current.revision) || current.revision < 0 ||
        current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error("Playnite settings revision is invalid.");
    const requested = parsePlayniteSettings(values);
    const confirms = (reply: PlayniteSettings): boolean =>
      reply.revision === current.revision + 1 &&
      sameConfigurationValue(reply.values, requested);
    let updated: PlayniteSettings;
    try {
      updated = parsePlayniteSettingsReply(await api.post<unknown>("playnite/settings_update", {
        ...requested,
        expected_revision: current.revision,
      }));
    } catch (error) {
      if (!(error instanceof ApiError) || error.code !== "update_outcome_unknown") throw error;
      const reconciled = await readPlayniteSettings(api);
      if (!confirms(reconciled))
        throw new Error("Playnite update outcome is uncertain. Refresh and check the current values.");
      return reconciled;
    }
    if (!confirms(updated))
      throw new Error("Playnite update could not be confirmed. Refresh and check the current values.");
    const saved = await readPlayniteSettings(api);
    if (!confirms(saved))
      throw new Error("Playnite settings could not be confirmed after saving. Refresh and check the current values.");
    return saved;
  }

  export class PlayniteSettingsLoadGate {
    private generation = 0;

    invalidate(): void {
      this.generation += 1;
    }

    async read(
      loadSettings: () => Promise<PlayniteSettings>,
      loadStatus: () => Promise<PlayniteStatus>,
    ): Promise<{ settings: PlayniteSettings; runtime: PlayniteStatus } | null> {
      const generation = ++this.generation;
      try {
        const settings = await loadSettings();
        if (generation !== this.generation) return null;
        const runtime = await loadStatus();
        return generation === this.generation ? { settings, runtime } : null;
      } catch (error) {
        if (generation !== this.generation) return null;
        throw error;
      }
    }
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
    for (const item of value) {
      if (
        typeof item !== "string" ||
        item.trim().length === 0 ||
        encoder.encode(item).length > 512 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(item)
      )
        throw new Error(
          `${label} must contain printable strings of at most 512 UTF-8 bytes.`,
        );
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
    const registeredLoads = new RegisteredConfigurationLoadGate();
    const authArea = el("div");
    const authLoads = new AuthTokenSettingsLoadGate();
    const hostLoggingArea = el("div");
    const hostLoggingLoads = new HostLoggingLoadGate();
    const hostArea = el("div");
    const hostLoads = new NvhttpSettingsLoadGate();
    const steamArea = el("div");
    const steamLoads = new ImportSettingsLoadGate<SteamSettings>();
    const lutrisArea = el("div");
    const lutrisLoads = new ImportSettingsLoadGate<{ settings: LutrisSettings; runtime: LutrisStatus }>();
    const playniteArea = el("div");
    const playniteLoads = new PlayniteSettingsLoadGate();
    const inputArea = el("div");
    const inputLoads = new InputPolicyLoadGate();
    const historyArea = el("div");
    const historyLoads = new HistorySettingsLoadGate();
    const pairingArea = el("div");
    const pairingLoads = new PairingClientsLoadGate();
    context.content.append(
      sectionHeading(
        "Settings",
        "Edit host options and values registered for this management console.",
        button("Refresh", () => {
          void load();
          void loadAuth();
          void loadHostLogging();
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
      hostLoggingArea,
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
        const settings = await authLoads.read(context.api);
        if (settings === null) return;
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
        sessionTtl.min = "1";
        sessionTtl.step = "1";
        refreshTtl.min = "1";
        refreshTtl.step = "1";
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
            authLoads.invalidate();
            try {
              await saveAuthTokenLifetimes(
                context.api,
                settings.revision,
                sessionTtl.valueAsNumber,
                refreshTtl.valueAsNumber,
              );
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                await loadAuth();
                context.notify("Authentication settings changed elsewhere. Review the latest values and save again.");
                return;
              }
              throw error;
            }
            await loadAuth();
            context.notify("Authentication lifetimes saved and confirmed.");
          });
        });
        authArea.replaceChildren(panel("Authentication session lifetimes", form));
      } catch (error) {
        if (context.content.isConnected)
          authArea.replaceChildren(errorPanel(error, loadAuth));
      }
    }
    async function loadHostLogging(): Promise<void> {
      hostLoggingArea.replaceChildren(el("p", "muted", "Loading host logging policy…"));
      try {
        const state = await hostLoggingLoads.read(context.api);
        if (state === null) return;
        const { settings, diagnostics: diagnosticsStatus } = state;
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const level = el("select");
        level.name = "min_log_level";
        for (const optionValue of hostLogLevels) {
          const option = el("option", "", optionValue.replaceAll("_", " "));
          option.value = optionValue;
          level.append(option);
        }
        level.value = settings.values.min_log_level;
        const path = input("log_path", settings.values.log_path);
        path.required = true;
        path.maxLength = 4096;
        const status = el("div");
        const applicationStatus = el("p", "muted", diagnosticsLoggingDescription(settings.revision, diagnosticsStatus));
        const save = el("button", "button primary", "Save host logging settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        const refresh = button("Refresh diagnostics status", () => {
          void runAction(refresh, status, async () => {
            const current = await readDiagnosticsLoggingStatus(context.api);
            applicationStatus.textContent = diagnosticsLoggingDescription(settings.revision, current);
          });
        });
        refresh.disabled = !context.api.allows("diagnostics.read");
        form.append(
          field("Minimum log level", level, "Product default uses the release channel's policy. Diagnostic records below this level are filtered."),
          field("Log path", path, "A relative path stays beneath the private host state root. The setting applies to diagnostic records after the reader refreshes its policy."),
          el("p", "muted", `Effective host policy: ${settings.effective.min_log_level} · ${settings.effective.log_path}`),
          el("p", "muted", "The diagnostics reader applies saved changes asynchronously. Other host and native logs do not use this sink; a host restart may be required for their separate logging policies."),
          applicationStatus,
          refresh,
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          if (!form.reportValidity()) return;
          void runAction(save, status, async () => {
            hostLoggingLoads.invalidate();
            try {
              const saved = await saveHostLoggingState(context.api, settings, level.value, path.value);
              context.notify(diagnosticsLoggingDescription(saved.settings.revision, saved.diagnostics));
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                await loadHostLogging();
                context.notify("Host configuration changed elsewhere. Review the latest logging settings and save again.");
                return;
              }
              throw error;
            }
            await loadHostLogging();
          });
        });
        hostLoggingArea.replaceChildren(panel("Host logging", form));
      } catch (error) {
        if (context.content.isConnected)
          hostLoggingArea.replaceChildren(errorPanel(error, loadHostLogging));
      }
    }
    async function loadSteam(): Promise<void> {
      steamArea.replaceChildren(el("p", "muted", "Loading Steam settings…"));
      try {
        const loaded = await steamLoads.read(() => readSteamSettings(context.api));
        if (loaded === null) return;
        const { value: settings, generation } = loaded;
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
            "Controls whether the provider exposes discovered Steam games and accepts scans. On Linux the provider keeps discovery enabled.",
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
            steamLoads.invalidate();
            let saveStarted = false;
            try {
              const values = {
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
              };
              saveStarted = true;
              const saved = await saveSteamSettings(context.api, settings, values);
              await loadSteam();
              context.notify(!values.enabled && saved.values.enabled
                ? "Steam settings saved and confirmed. Steam discovery remains enabled by Linux host policy; scan status is shown below."
                : "Steam settings saved and confirmed. Scan status is shown below.");
            } catch (error) {
              if (saveStarted) {
                await loadSteam();
                context.notify(error instanceof ApiError && error.code === "revision_conflict"
                  ? "Steam settings changed elsewhere. Review the latest values before saving again."
                  : "Steam save could not be confirmed. Review the latest values before saving again.");
                return;
              }
              throw error;
            }
          });
        });
        steamArea.replaceChildren(panel("Steam", form));
        void loadSteamRuntimeStatus(runtimeStatus, context, settings, generation);
      } catch (error) {
        if (context.content.isConnected)
          steamArea.replaceChildren(errorPanel(error, loadSteam));
      }
    }
    async function loadLutris(): Promise<void> {
      lutrisArea.replaceChildren(el("p", "muted", "Loading Lutris settings…"));
      try {
        const loaded = await lutrisLoads.read(async () => {
          const settings = await readLutrisSettings(context.api);
          const runtime = await context.api.post<LutrisStatus>("lutris/status");
          return { settings, runtime };
        });
        if (loaded === null) return;
        const { settings, runtime } = loaded.value;
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
          runtime.settings_revision !== settings.revision ||
          runtime.enabled !== settings.values.enabled ||
          runtime.auto_sync !== settings.values.auto_sync
            ? "Saved Lutris settings are not yet reflected in provider status. Refresh to check application."
            : lutrisStatusDescription(runtime),
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
            if (!runtimeStatus.isConnected || !lutrisLoads.isCurrent(loaded.generation)) return;
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
            lutrisLoads.invalidate();
            let saveStarted = false;
            try {
              const values = {
                enabled: enabled.checked,
                auto_sync: autoSync.checked,
                autosync_remove_uninstalled: removeUninstalled.checked,
                include_steam: includeSteam.checked,
                exclude_games: parseLutrisExclusions(exclusions.value),
              };
              saveStarted = true;
              await saveLutrisSettings(context.api, settings, values);
              await loadLutris();
              context.notify("Lutris settings saved and confirmed. Scan status is shown below.");
            } catch (error) {
              if (saveStarted) {
                await loadLutris();
                context.notify(error instanceof ApiError && error.code === "revision_conflict"
                  ? "Lutris settings changed elsewhere. Review the latest values before saving again."
                  : "Lutris save could not be confirmed. Review the latest values before saving again.");
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
    async function loadPlaynite(): Promise<void> {
      playniteArea.replaceChildren(el("p", "muted", "Loading Playnite settings…"));
      try {
        const loaded = await playniteLoads.read(
          () => readPlayniteSettings(context.api),
          () => context.api.post<PlayniteStatus>("playnite/status"),
        );
        if (loaded === null) return;
        const { settings, runtime } = loaded;
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
          runtime.revision !== settings.revision || runtime.enabled !== settings.values.enabled ||
          runtime.auto_sync !== settings.values.auto_sync
            ? "Saved Playnite settings are not yet reflected in runtime status. Refresh to check application."
            : playniteStatusDescription(runtime),
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
            playniteLoads.invalidate();
            let saveStarted = false;
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
              saveStarted = true;
              await savePlayniteSettings(context.api, settings, values);
              await loadPlaynite();
              context.notify("Playnite settings saved and confirmed. Runtime status is shown below.");
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                context.notify("Playnite settings changed elsewhere. The latest values have been reloaded.");
                await loadPlaynite();
                return;
              }
              if (saveStarted) {
                await loadPlaynite();
                context.notify("Playnite save could not be confirmed. Review the latest values before saving again.");
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
    async function loadPairing(): Promise<void> {
      pairingArea.replaceChildren(el("p", "muted", "Loading paired clients…"));
      try {
        const loaded = await pairingLoads.read(context.api);
        if (loaded === null) return;
        const { settings, inputStatus } = loaded;
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
          const controller = sparseInputOverrideSelect("controller", client.config_overrides?.controller);
          controller.setAttribute("aria-label", `Controller input override for ${client.name}`);
          const gamepadChoices = inputGamepadProfileChoices(
            client.config_overrides?.gamepad ?? "",
            inputStatus?.supported_profiles ?? ["auto"],
            inputStatus?.recognized_profiles ?? ["auto", ...(client.config_overrides?.gamepad ? [client.config_overrides.gamepad] : [])],
          );
          const gamepad = el("select");
          gamepad.setAttribute("aria-label", `Gamepad profile override for ${client.name}`);
          const inheritGamepad = el("option", "", "Inherit");
          inheritGamepad.value = "";
          gamepad.append(inheritGamepad);
          for (const choice of gamepadChoices) {
            const option = el("option", "", choice.label);
            option.value = choice.value;
            gamepad.append(option);
          }
          gamepad.value = client.config_overrides?.gamepad ?? "";
          const keyboard = sparseInputOverrideSelect("keyboard", client.config_overrides?.keyboard);
          keyboard.setAttribute("aria-label", `Keyboard input override for ${client.name}`);
          const mouse = sparseInputOverrideSelect("mouse", client.config_overrides?.mouse);
          mouse.setAttribute("aria-label", `Mouse input override for ${client.name}`);
          const nativePenTouch = sparseInputOverrideSelect("native_pen_touch", client.config_overrides?.native_pen_touch);
          nativePenTouch.setAttribute("aria-label", `Native pen/touch preference override for ${client.name}`);
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
          const unsupported = unsupportedPairingOverrideFields(client);
          const save = button(
            "Save",
            () => runAction(save, status, async () => {
              pairingLoads.invalidate();
              let saveStarted = false;
              try {
                const values = {
                  ...parseClientFecOverride(fec.value),
                  back_button_timeout: parseBackButtonTimeoutOverride(backButtonTimeout.value),
                  stream_audio: parseStreamAudioOverride(streamAudio.value),
                  audio_sink: parseAudioSinkOverride(audioSinkMode.value, audioSinkId.value),
                  virtual_sink: parseVirtualSinkOverride(virtualSinkMode.value, virtualSinkSelector.value),
                  audio_sink_capture_only: parseAudioSinkCaptureOnlyOverride(captureOnly.value),
                  controller: parseInputBooleanOverride(controller.value, "Controller input"),
                  gamepad: parseGamepadProfileOverride(
                    gamepad.value,
                    inputStatus?.recognized_profiles ?? ["auto", ...(client.config_overrides?.gamepad ? [client.config_overrides.gamepad] : [])],
                  ),
                  keyboard: parseInputBooleanOverride(keyboard.value, "Keyboard input"),
                  mouse: parseInputBooleanOverride(mouse.value, "Mouse input"),
                  native_pen_touch: parseInputBooleanOverride(nativePenTouch.value, "Native pen/touch preference"),
                };
                saveStarted = true;
                await savePairingClientOverrides(context.api, settings, client.certificate_id, values);
                await loadPairing();
                context.notify(`Stored overrides confirmed for ${client.name}. Active stream application is not confirmed.`);
              } catch (error) {
                if (saveStarted) {
                  await loadPairing();
                  context.notify(error instanceof ApiError && error.code === "revision_conflict"
                    ? "Paired clients changed elsewhere. Review the refreshed overrides before saving again."
                    : "Paired client override save could not be confirmed. Review the refreshed values before saving again.");
                  return;
                }
                throw error;
              }
            }),
            "primary",
          );
          save.disabled = !context.api.allows("pairing.admin") || unsupported.length > 0;
          if (unsupported.length > 0)
            status.textContent = `Save unavailable: this client has stored overrides this editor cannot preserve (${unsupported.join(", ")}).`;
          const identity = el(
            "div",
            "",
            el("strong", "", client.name),
            el("small", "muted", `${client.client_id} · certificate ${client.certificate_id.slice(0, 12)}…`),
          );
          return [identity, fec, backButtonTimeout, streamAudio, el("div", "", audioSinkMode, audioSinkId), el("div", "", virtualSinkMode, virtualSinkSelector), captureOnly, controller, gamepad, keyboard, mouse, nativePenTouch, el("div", "", save, status)];
        });
        const tableView = table(["Paired client", "FEC percentage", "Back button timeout (ms)", "Audio streaming", "Audio endpoint", "Virtual sink", "Capture only", "Controller", "Gamepad", "Keyboard", "Mouse", "Native pen/touch", ""], rows);
        pairingArea.replaceChildren(
          panel(
            "Paired client overrides",
            el("p", "muted", "Blank inherits the application and host value. Back button timeout accepts signed 32-bit milliseconds, with -1 disabling the timeout. Recognized unavailable gamepad profiles stay labeled. Each save confirms the stored pairing revision; applying client input preferences during active streams is not yet implemented."),
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
        const state = await historyLoads.read(context.api);
        if (state === null) return;
        const { settings: reply, runtime } = state;
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
        const status = el(
          "p",
          "muted",
          historySettingsRuntimeDescription(reply, runtime),
        );
        const save = el("button", "button primary", "Save history settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Record session history",
            enabled,
            "If streams are active, recording changes wait until all of them end. New streams started while a change is pending use the previous recording policy.",
          ),
          el("p", "muted", "Retention changes apply immediately to stored ended sessions, including while recording is disabled."),
          ttlField,
          sizeField,
          status,
          el("div", "actions", save, badge(`Revision ${reply.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            historyLoads.invalidate();
            const updated = await saveHistorySettingsState(
              context.api,
              reply,
              enabled.checked,
              ttl.valueAsNumber,
              size.valueAsNumber,
            );
            context.notify(historySettingsRuntimeDescription(updated.settings, updated.runtime));
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
        const loaded = await inputLoads.read(context.api);
        if (loaded === null) return;
        const { settings, runtime: runtimeStatus } = loaded;
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const keyboard = input("input_keyboard", "", "checkbox");
        keyboard.checked = settings.values.keyboard;
        const mouse = input("input_mouse", "", "checkbox");
        mouse.checked = settings.values.mouse;
        const controller = input("input_controller", "", "checkbox");
        controller.checked = settings.values.controller;
        const ds4BackAsTouchpadClick = input("input_ds4_back_as_touchpad_click", "", "checkbox");
        ds4BackAsTouchpadClick.checked = settings.values.ds4_back_as_touchpad_click;
        const nativePenTouch = input("input_native_pen_touch", "", "checkbox");
        nativePenTouch.checked = settings.values.native_pen_touch;
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
          runtimeStatus?.supported_profiles ?? ["auto"],
          runtimeStatus?.recognized_profiles ?? [settings.values.gamepad],
        );
        for (const choice of gamepadProfileChoices) {
          const option = el("option", "", choice.label);
          option.value = choice.value;
          gamepadProfile.append(option);
        }
        gamepadProfile.value = settings.values.gamepad;
        const selectedGamepadUnavailable =
          settings.values.controller &&
          (runtimeStatus !== null &&
            (!runtimeStatus.supported_profiles.includes(settings.values.gamepad) ||
              (runtimeStatus.lease !== null &&
                runtimeStatus.gamepad === settings.values.gamepad &&
                !runtimeStatus.controller_input)));
        const gamepadStatus = el(
          "p",
          selectedGamepadUnavailable ? "error" : "muted",
          inputGamepadStatusDescription(settings.values.gamepad, runtimeStatus),
        );
        const status = el(
          "p",
          settings.applied && runtimeStatus !== null && !runtimeStatus.controller_cleanup_required
            ? "muted" : "error",
          inputPolicyRuntimeDescription(settings, runtimeStatus),
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
            "DS4 Back as touchpad click",
            ds4BackAsTouchpadClick,
            "For the DS4 profile, maps Back to a touchpad click when that backend supports it. The preference stays saved if the selected backend cannot apply it.",
          ),
          field(
            "Native pen and touch policy",
            nativePenTouch,
            "Windows-only global preference. Native pen/touch event transport and platform support are not implemented; saving this value does not activate pen or touch events.",
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
            inputLoads.invalidate();
            let saveStarted = false;
            try {
              const values = {
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
                  nativePenTouch.checked,
                ),
                ds4_back_as_touchpad_click: ds4BackAsTouchpadClick.checked,
              };
              saveStarted = true;
              const saved = await saveInputPolicySettings(context.api, settings, values);
              await loadInput();
              context.notify(saved.applied && !saved.cleanup_required
                ? "Input policy saved and confirmed. Runtime status is shown below."
                : "Input policy saved and confirmed; releasing held native input is pending.");
            } catch (error) {
              if (saveStarted) {
                await loadInput();
                context.notify(error instanceof ApiError && error.code === "revision_conflict"
                  ? "Input policy changed elsewhere. Review the latest values before saving again."
                  : "Input policy save could not be confirmed. Review the latest values before saving again.");
                return;
              }
              throw error;
            }
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
      settings: SteamSettings,
      generation: number,
    ): Promise<void> {
      try {
        const status = await context.api.post<SteamStatus>("steam/status");
        if (!target.isConnected || !steamLoads.isCurrent(generation)) return;
        target.textContent = status.settings_ready && status.enabled !== settings.values.enabled
          ? "Saved Steam discovery setting is not yet reflected in provider status. Refresh to check application."
          : steamStatusDescription(status);
      } catch (error) {
        if (!target.isConnected || !steamLoads.isCurrent(generation)) return;
        target.textContent =
          error instanceof ApiError && error.code === "forbidden"
            ? "Runtime status requires steam.read; settings remain available with configuration access."
            : "Current provider status is unavailable; settings remain editable.";
      }
    }
    async function loadHost(): Promise<void> {
      hostArea.replaceChildren(el("p", "muted", "Loading host name…"));
      let generation: number | null = null;
      try {
        const loaded = await hostLoads.read(context.api);
        if (loaded === null) return;
        const { settings } = loaded;
        generation = loaded.generation;
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
            if (!context.content.isConnected || !hostLoads.isCurrent(generation)) return;
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
            if (!context.content.isConnected || !hostLoads.isCurrent(generation)) return;
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
        const globalPreparation = el("textarea");
        globalPreparation.name = "global_preparation";
        globalPreparation.rows = 10;
        globalPreparation.value = JSON.stringify(settings.configured_global_preparation, null, 2);
        const globalPreparationValidation = el("p", "muted");
        const updateGlobalPreparationValidation = () => {
          try {
            const steps = parsePreparationSteps(globalPreparation.value);
            globalPreparation.setCustomValidity("");
            globalPreparationValidation.className = "muted";
            globalPreparationValidation.textContent =
              "Valid preparation list with " +
              steps.length +
              " step" +
              (steps.length === 1 ? "" : "s") +
              ".";
          } catch (error) {
            const message = messageOf(error);
            globalPreparation.setCustomValidity(message);
            globalPreparationValidation.className = "error";
            globalPreparationValidation.textContent = message;
          }
        };
        globalPreparation.addEventListener("input", updateGlobalPreparationValidation);
        updateGlobalPreparationValidation();
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
          field(
            "Global preparation commands (JSON)",
            globalPreparation,
            "Enter an ordered JSON array of up to 8 run commands with optional undo commands. Executables and working directories must be absolute native paths; blank input clears the list.",
          ),
          globalPreparationValidation,
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
          el("p", "muted", `Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}, RTSP ${settings.active_rtsp_port ?? "off"}`),
          ...(settings.listener_error ? [el("p", "error", settings.listener_error)] : []),
          ...(settings.rtsp_listener_error ? [el("p", "error", settings.rtsp_listener_error)] : []),
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          if (!form.reportValidity()) return;
          void runAction(save, status, async () => {
            const network = parseNvhttpNetworkOverrides(
              addressFamily.value,
              useStartupBindAddress.checked,
              bindAddress.value,
            );
            const values: Payload = {
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
              global_preparation: parsePreparationSteps(globalPreparation.value),
            };
            hostLoads.invalidate();
            try {
              await saveNvhttpHostSettings(context.api, settings, values);
              context.notify("Host settings saved and read back. Future-stream options apply to new streams; check active listener status below.");
              await loadHost();
            } catch (error) {
              if (error instanceof ApiError && await recoverNvhttpSaveFailure(
                error.code,
                loadHost,
                (message) => context.notify(message),
              )) {
                return;
              }
              throw error;
            }
          });
        });
        if (hostLoads.isCurrent(generation))
          hostArea.replaceChildren(panel("Moonlight host settings", form));
      } catch (error) {
        if (context.content.isConnected && (generation === null || hostLoads.isCurrent(generation)))
          hostArea.replaceChildren(errorPanel(error, loadHost));
      }
    }
    async function load(): Promise<void> {
      area.replaceChildren(el("p", "muted", "Loading settings…"));
      try {
        const document = await registeredLoads.read(context.api);
        if (document === null) return;
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
          el("p", "muted", "Saving confirms stored values. Runtime use of registered settings is not verified on this page."),
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
            registeredLoads.invalidate();
            try {
              await saveRegisteredConfiguration(context.api, document, values);
              context.notify("Registered settings saved and read back. Runtime application is not verified here.");
              await load();
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                await load();
                context.notify("Registered settings changed elsewhere. Review the latest values and save again.");
                return;
              }
              throw error;
            }
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
    void loadHostLogging();
    void loadHost();
    void loadSteam();
    void loadLutris();
    void loadPlaynite();
    void loadInput();
    void loadHistorySettings();
    void loadPairing();
  }
}
