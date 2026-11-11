namespace Amaterasu {
  function markHostSettingsOwner(owner: string, node: HTMLElement): HTMLElement {
    node.setAttribute("data-settings-owner",owner); return node;
  }
  function settingsTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function settingsTextMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function" ? Amaterasu.localizedMessage(value) : value;
  }

  function settingsSetLocalizedTextContent(target: HTMLElement, source: string): void {
    if (typeof Amaterasu.setLocalizedTextContent === "function") {
      Amaterasu.setLocalizedTextContent(target, source);
    } else {
      target.textContent = source;
    }
  }

  function settingsComponentField(label: string, control: HTMLElement, hint: string): HTMLElement {
    return el("div", "field",
      el("span", "field-label", settingsTextMessage(label)),
      control,
      el("small", "muted", settingsTextMessage(hint)),
    );
  }

  function settingsDispatchDraftEvent(target: HTMLElement, type: "input" | "change"): void {
    target.dispatchEvent(new Event(type, { bubbles: true }));
  }

  function settingsSetDraftCheckbox(control: HTMLInputElement, value: boolean): void {
    control.checked = value;
    settingsDispatchDraftEvent(control, "change");
  }

  function settingsSetDraftNumber(control: HTMLInputElement, value: number): void {
    control.value = String(value);
    settingsDispatchDraftEvent(control, "input");
  }

  function setSettingsFormDisabled(form: HTMLElement, disabled: boolean): void {
    const visit = (node: Element): void => {
      if (!node || typeof node.tagName !== "string") return;
      if (["INPUT", "BUTTON", "SELECT", "TEXTAREA"].includes(node.tagName))
        (node as Element & { disabled: boolean }).disabled = disabled;
      for (const child of Array.from(node.children)) visit(child);
    };
    visit(form);
  }

  function settingsLocalizedAttributeTemplate(
    target: Element,
    source: string,
    values: Readonly<Record<string, TextValue>>,
  ): void {
    if (typeof Amaterasu.setLocalizedAttributeTemplate === "function") {
      Amaterasu.setLocalizedAttributeTemplate(target, "aria-label", source, values);
      return;
    }
    let rendered = source;
    for (const [name, value] of Object.entries(values)) {
      const literal = typeof value === "string" ? value : value.value;
      rendered = rendered.replaceAll(`{${name}}`, () => literal);
    }
    target.setAttribute("aria-label", rendered);
  }

  function settingsClientOverrideAttribute(
    target: Element,
    field: string,
    clientName: string,
  ): void {
    settingsLocalizedAttributeTemplate(target, "{field} override for {client}", {
      field: settingsTextMessage(field),
      client: settingsTextLiteral(clientName),
    });
  }

  const maxConfiguredAuthTtlSeconds = 0x7fff_ffff;
  const maxInputKeyRepeatDelayMs = 0x7fff_ffff;
  const minInputBackButtonTimeoutMs = -0x8000_0000;
  const maxInputBackButtonTimeoutMs = 0x7fff_ffff;

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

  export function parseSoftwareEncoderAvailability(value: unknown): string {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Invalid codec availability reply.");
    const reply = value as Record<string, unknown>;
    const software = reply.software_encoder;
    if (reply.ok !== true || typeof reply.enabled !== "boolean" || !software ||
        typeof software !== "object" || Array.isArray(software) ||
        typeof (software as Record<string, unknown>).available !== "boolean")
      throw new Error("Invalid codec availability reply.");
    const availability = software as Record<string, unknown>;
    if (!reply.enabled) return "The codec provider is disabled. Software x264 cannot be selected.";
    if (availability.available) {
      const isSoftwareIdentity = (entry: unknown): boolean => {
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) return false;
        const descriptor = entry as Record<string, unknown>;
        const identity = descriptor.identity;
        return descriptor.hardware === false && identity !== null &&
          typeof identity === "object" && !Array.isArray(identity) &&
          (identity as Record<string, unknown>).clsid === "amaterasu.software.x264";
      };
      if (!Array.isArray(reply.encoders) || !reply.encoders.some(isSoftwareIdentity))
        throw new Error("Software availability lacks an actual encoder identity.");
      return "Software x264 is available in the current codec runtime. Capture and client playback still require platform qualification.";
    }
    const reasons: Record<string, string> = {
      x264_feature_disabled: "This build has the software-x264 feature disabled.",
      x264_headers_unavailable: "This build lacked compatible x264 development headers.",
      x264_runtime_unavailable: "A compatible x264 runtime library is unavailable.",
    };
    return `Software x264 is unavailable. ${reasons[String(availability.reason)] ?? "The codec provider did not report a qualified software backend."}`;
  }

  export function parseNvhttpSettingsReply(value: unknown): NvhttpSettings {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Moonlight host settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || !Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        typeof reply.configured_hostname !== "string" ||
        typeof reply.upnp !== "boolean" ||
        typeof reply.install_steam_audio_drivers !== "boolean" ||
        ![0, 1, 2].includes(reply.lan_encryption_mode as number) ||
        ![0, 1, 2].includes(reply.wan_encryption_mode as number) ||
        !["disabled", "active", "waiting_for_ipv4_listener", "waiting_for_gateway_route", "waiting_for_listener", "error"].includes(String(reply.upnp_status)) ||
        !(reply.upnp_error === null || typeof reply.upnp_error === "string") ||
        !(reply.configured_fec_percentage === null ||
          (Number.isSafeInteger(reply.configured_fec_percentage) &&
            (reply.configured_fec_percentage as number) >= 1 &&
            (reply.configured_fec_percentage as number) <= 255)) ||
        !Number.isSafeInteger(reply.effective_fec_percentage) ||
        (reply.effective_fec_percentage as number) < 1 ||
        (reply.effective_fec_percentage as number) > 255 ||
        !Number.isSafeInteger(reply.port) ||
        !Number.isSafeInteger(reply.configured_nvenc_preset) ||
        (reply.configured_nvenc_preset as number) < 1 ||
        (reply.configured_nvenc_preset as number) > 7 ||
        typeof reply.configured_nvenc_h264_cavlc !== "boolean" ||
        !["disabled", "quarter_res", "full_res"].includes(String(reply.configured_nvenc_twopass)) ||
        !Number.isSafeInteger(reply.configured_nvenc_vbv_increase) ||
        (reply.configured_nvenc_vbv_increase as number) < 0 ||
        (reply.configured_nvenc_vbv_increase as number) > 400 ||
        !["auto", "enabled", "disabled"].includes(String(reply.configured_nvenc_split_encode)) ||
        typeof reply.configured_nvenc_realtime_hags !== "boolean" ||
        typeof reply.configured_nvenc_opengl_vulkan_on_dxgi !== "boolean" ||
        typeof reply.configured_nvenc_latency_over_power !== "boolean" ||
        typeof reply.configured_vaapi_strict_rc_buffer !== "boolean" ||
        !Array.isArray(reply.configured_global_preparation) ||
        (reply.configured_display_policy !== undefined &&
          (!reply.configured_display_policy || typeof reply.configured_display_policy !== "object" ||
            Array.isArray(reply.configured_display_policy))))
      throw new Error("Moonlight host settings reply is invalid.");
    if (reply.configured_display_policy !== undefined) {
      try {
        parseDisplayPolicyOverrideMap(JSON.stringify(reply.configured_display_policy), "global");
      } catch {
        throw new Error("Moonlight host display policy reply is invalid.");
      }
    }
    return reply as unknown as NvhttpSettings;
  }

  export function parseSteamAudioDriverInstallStatus(value: unknown): SteamAudioDriverInstallStatus {
    if (value === null || typeof value !== "object" || Array.isArray(value))
      throw new Error("Steam audio driver status reply is invalid.");
    const reply = value as Record<string, unknown>;
    const status = reply.steam_driver_install;
    if (reply.ok !== true || status === null || typeof status !== "object" || Array.isArray(status))
      throw new Error("Steam audio driver status reply is invalid.");
    const fields = status as Record<string, unknown>;
    const states = ["awaiting_configuration", "disabled", "checking", "already_present", "installed",
      "installed_with_route_restore_errors", "installed_endpoint_unavailable", "error", "unsupported_platform"];
    if (typeof fields.enabled !== "boolean" ||
        !(fields.revision === null || Number.isSafeInteger(fields.revision)) ||
        !states.includes(String(fields.state)) ||
        !(fields.error_code === null || typeof fields.error_code === "string") ||
        !(fields.native_error === null || Number.isSafeInteger(fields.native_error)) ||
        !(fields.endpoint === null || typeof fields.endpoint === "string") ||
        !Array.isArray(fields.restored_roles) || fields.restored_roles.some((role) => typeof role !== "string") ||
        !Array.isArray(fields.restore_failures) || fields.restore_failures.some((failure) =>
          !failure || typeof failure !== "object" || Array.isArray(failure) ||
          typeof (failure as Record<string, unknown>).role !== "string" ||
          typeof (failure as Record<string, unknown>).error_code !== "string" ||
          !((failure as Record<string, unknown>).native_error === null ||
            Number.isSafeInteger((failure as Record<string, unknown>).native_error))))
      throw new Error("Steam audio driver status reply is invalid.");
    return fields as unknown as SteamAudioDriverInstallStatus;
  }

  export function steamAudioDriverInstallStatusText(status: SteamAudioDriverInstallStatus): string {
    if (status.state === "unsupported_platform")
      return "Steam Streaming Speakers installation is available on Windows only.";
    if (status.state === "disabled")
      return "Automatic Steam Streaming Speakers driver installation is disabled.";
    if (status.state === "awaiting_configuration" || status.state === "checking")
      return "Steam Streaming Speakers driver status is being checked.";
    if (status.state === "already_present")
      return `Steam Streaming Speakers is already available${status.endpoint ? ` (${status.endpoint})` : ""}.`;
    if (status.state === "installed")
      return `Steam Streaming Speakers driver installed${status.endpoint ? ` (${status.endpoint})` : ""}. Restored defaults: ${status.restored_roles.join(", ") || "none"}.`;
    if (status.state === "installed_with_route_restore_errors")
      return `Steam Streaming Speakers driver installed, but default endpoint restoration reported errors for ${status.restore_failures.map((failure) => failure.role).join(", ")}.`;
    if (status.state === "installed_endpoint_unavailable")
      return `Steam's installer completed, but the Speakers endpoint did not become active before the 30-second wait expired. Restored defaults: ${status.restored_roles.join(", ") || "none"}${status.restore_failures.length ? `; restoration errors: ${status.restore_failures.map((failure) => failure.role).join(", ")}` : ""}.`;
    return `Steam Streaming Speakers driver installation failed${status.error_code ? ` (${status.error_code})` : ""}${status.native_error === null ? "" : `, Windows error ${status.native_error}`}.`;
  }

  function nvencSparseBooleanSelect(
    name: keyof NvencSettingsOverrides,
    value: boolean | null | undefined,
  ): HTMLSelectElement {
    const select = el("select");
    select.name = name;
    for (const [optionValue, label] of [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]] as const) {
      const option = el("option", "", label);
      option.value = optionValue;
      select.append(option);
    }
    select.value = value == null ? "" : String(value);
    return select;
  }

  function nvencSettingsOverrideControls(
    overrides: PairingClient["config_overrides"],
  ): { fields: HTMLElement[]; read: () => NvencSettingsOverrides } {
    const preset = input("nvenc_preset", overrides?.nvenc_preset == null ? "" : String(overrides.nvenc_preset), "number");
    preset.min = "1";
    preset.max = "7";
    preset.step = "1";
    preset.placeholder = "Inherit";
    const cavlc = nvencSparseBooleanSelect("nvenc_h264_cavlc", overrides?.nvenc_h264_cavlc);
    const twopass = el("select");
    twopass.name = "nvenc_twopass";
    for (const [value, label] of [["", "Inherit"], ["disabled", "Disabled"], ["quarter_res", "Quarter resolution"], ["full_res", "Full resolution"]] as const) {
      const option = el("option", "", label);
      option.value = value;
      twopass.append(option);
    }
    twopass.value = overrides?.nvenc_twopass ?? "";
    const vbvIncrease = input("nvenc_vbv_increase", overrides?.nvenc_vbv_increase == null ? "" : String(overrides.nvenc_vbv_increase), "number");
    vbvIncrease.min = "0";
    vbvIncrease.max = "400";
    vbvIncrease.step = "1";
    vbvIncrease.placeholder = "Inherit";
    const splitEncode = el("select");
    splitEncode.name = "nvenc_split_encode";
    for (const [value, label] of [["", "Inherit"], ["auto", "Automatic"], ["enabled", "Enabled"], ["disabled", "Disabled"]] as const) {
      const option = el("option", "", label);
      option.value = value;
      splitEncode.append(option);
    }
    splitEncode.value = overrides?.nvenc_split_encode ?? "";
    const realtimeHags = nvencSparseBooleanSelect("nvenc_realtime_hags", overrides?.nvenc_realtime_hags);
    const openglVulkanOnDxgi = nvencSparseBooleanSelect("nvenc_opengl_vulkan_on_dxgi", overrides?.nvenc_opengl_vulkan_on_dxgi);
    const latencyOverPower = nvencSparseBooleanSelect("nvenc_latency_over_power", overrides?.nvenc_latency_over_power);
    const vaapiStrictRcBuffer = nvencSparseBooleanSelect("vaapi_strict_rc_buffer", overrides?.vaapi_strict_rc_buffer);
    const booleanValue = (select: HTMLSelectElement, name: string): boolean | null => {
      if (select.value === "") return null;
      if (select.value === "true") return true;
      if (select.value === "false") return false;
      throw new Error(`${name} must be enabled, disabled, or inherited.`);
    };
    const integerValue = (control: HTMLInputElement, name: string, minimum: number, maximum: number): number | null => {
      const value = control.value.trim();
      if (value === "") return null;
      if (!/^[0-9]+$/.test(value))
        throw new Error(`${name} must be a whole number from ${minimum} to ${maximum}, or blank to inherit.`);
      const number = Number(value);
      if (!Number.isSafeInteger(number) || number < minimum || number > maximum)
        throw new Error(`${name} must be a whole number from ${minimum} to ${maximum}, or blank to inherit.`);
      return number;
    };
    const enumValue = <T extends string>(select: HTMLSelectElement, choices: readonly T[], name: string): T | null => {
      if (select.value === "") return null;
      if ((choices as readonly string[]).includes(select.value)) return select.value as T;
      throw new Error(`${name} has an unsupported value.`);
    };
    return {
      fields: [
        field("NVENC preset", preset, "Inherit or choose P1 through P7."),
        field("NVENC H.264 CAVLC", cavlc, "Inherit or set CAVLC on or off."),
        field("NVENC two-pass mode", twopass, "Inherit or choose disabled, quarter resolution, or full resolution."),
        field("NVENC VBV increase (%)", vbvIncrease, "Inherit or set an integer from 0 to 400."),
        field("NVENC split encode", splitEncode, "Inherit or choose automatic, enabled, or disabled."),
        field("NVENC real-time HAGS", realtimeHags, "Inherit or set the real-time HAGS option."),
        field("NVENC OpenGL/Vulkan on DXGI", openglVulkanOnDxgi, "Inherit or set this DXGI capture option."),
        field("NVENC latency over power", latencyOverPower, "Inherit or prefer latency over power."),
        field("VAAPI strict rate-control buffer", vaapiStrictRcBuffer, "Inherit or set strict rate-control buffer behavior."),
      ],
      read: () => ({
        nvenc_preset: integerValue(preset, "NVENC preset", 1, 7),
        nvenc_h264_cavlc: booleanValue(cavlc, "NVENC H.264 CAVLC"),
        nvenc_twopass: enumValue<NvencTwoPass>(twopass, ["disabled", "quarter_res", "full_res"], "NVENC two-pass mode"),
        nvenc_vbv_increase: integerValue(vbvIncrease, "NVENC VBV increase", 0, 400),
        nvenc_split_encode: enumValue<NvencSplitEncode>(splitEncode, ["auto", "enabled", "disabled"], "NVENC split encode"),
        nvenc_realtime_hags: booleanValue(realtimeHags, "NVENC real-time HAGS"),
        nvenc_opengl_vulkan_on_dxgi: booleanValue(openglVulkanOnDxgi, "NVENC OpenGL/Vulkan on DXGI"),
        nvenc_latency_over_power: booleanValue(latencyOverPower, "NVENC latency over power"),
        vaapi_strict_rc_buffer: booleanValue(vaapiStrictRcBuffer, "VAAPI strict rate-control buffer"),
      }),
    };
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

  function sparseDisplayPolicy(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value as Record<string, unknown>)
      .filter(([, fieldValue]) => fieldValue !== null && fieldValue !== undefined));
  }

  export async function saveNvhttpHostSettings(
    api: Pick<Api, "post">,
    current: NvhttpSettings,
    values: Payload,
  ): Promise<NvhttpSettings> {
    const preserved = ["port", "stream_audio", "audio_sink_capture_only", "install_steam_audio_drivers", "nvenc_spatial_aq",
      "lan_encryption_mode", "wan_encryption_mode"];
    const expected: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(current))
      if (key.startsWith("configured_") || preserved.includes(key)) expected[key] = value;
    Object.assign(expected, {
      configured_hostname: values.hostname,
      port: values.port,
      upnp: values.upnp,
      lan_encryption_mode: values.lan_encryption_mode,
      wan_encryption_mode: values.wan_encryption_mode,
      configured_fec_percentage: values.fec_percentage,
      effective_fec_percentage: values.fec_percentage ?? current.effective_fec_percentage,
      configured_address_family: values.address_family,
      configured_bind_address: values.bind_address,
      configured_capture: values.capture,
      configured_output_name: values.output_name === "" ? null : values.output_name,
      stream_audio: values.stream_audio,
      configured_video_max_batch_size_kb: values.video_max_batch_size_kb,
      configured_max_bitrate: values.max_bitrate,
      configured_qp: values.qp,
      configured_minimum_fps_target: values.minimum_fps_target,
      configured_ping_timeout_ms: values.ping_timeout_ms === -1 ? null : values.ping_timeout_ms,
      configured_audio_sink: values.audio_sink,
      configured_virtual_sink: values.virtual_sink,
      audio_sink_capture_only: values.audio_sink_capture_only,
      install_steam_audio_drivers: values.install_steam_audio_drivers,
      configured_external_ip: values.external_ip,
      configured_wgc_pacing_smoothing: values.wgc_pacing_smoothing,
      configured_global_preparation: normalizedPreparationSteps(values.global_preparation),
      configured_nvenc_preset: values.nvenc_preset,
      configured_nvenc_h264_cavlc: values.nvenc_h264_cavlc,
      configured_nvenc_twopass: values.nvenc_twopass,
      configured_nvenc_vbv_increase: values.nvenc_vbv_increase,
      configured_nvenc_split_encode: values.nvenc_split_encode,
      configured_nvenc_realtime_hags: values.nvenc_realtime_hags,
      configured_nvenc_opengl_vulkan_on_dxgi: values.nvenc_opengl_vulkan_on_dxgi,
      configured_nvenc_latency_over_power: values.nvenc_latency_over_power,
      configured_vaapi_strict_rc_buffer: values.vaapi_strict_rc_buffer,
    });
    if (Object.hasOwn(current, "configured_display_policy"))
      expected.configured_display_policy = sparseDisplayPolicy(values.display_policy);
    for (const key of ["sw_preset", "sw_tune", "encoder"])
      if (Object.prototype.hasOwnProperty.call(values, key)) expected[`configured_${key}`] = values[key];
    if (Object.prototype.hasOwnProperty.call(current, "configured_min_threads"))
      expected.configured_min_threads = values.min_threads;
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

  export async function saveNvhttpAudioSettings(
    api: Pick<Api, "post">,
    current: NvhttpSettings,
    streamAudio: boolean,
    audioSink: string,
  ): Promise<NvhttpSettings> {
    if (!Number.isSafeInteger(current.revision) || current.revision < 0 || current.revision >= Number.MAX_SAFE_INTEGER ||
        typeof current.configured_hostname !== "string" || current.configured_hostname.length > 128 ||
        /[\u0000-\u001f\u007f-\u009f]/.test(current.configured_hostname) ||
        !Number.isSafeInteger(current.port) || current.port < 1029 || current.port > 65514 ||
        typeof current.stream_audio !== "boolean" ||
        !(current.configured_audio_sink === null || typeof current.configured_audio_sink === "string"))
      throw new Error("Current Moonlight host settings cannot be safely updated.");
    if (current.configured_audio_sink !== null)
      parseAudioSinkSelection(current.configured_audio_sink);
    if (typeof streamAudio !== "boolean")
      throw new Error("Choose whether future streams should include host audio.");
    const configuredAudioSink = parseAudioSinkSelection(audioSink);
    const preservedPolicyFields = [
      "port", "audio_sink_capture_only", "nvenc_spatial_aq", "lan_encryption_mode", "wan_encryption_mode",
    ];
    const persistedHostSettings = (reply: NvhttpSettings): Record<string, unknown> =>
      Object.fromEntries(
        Object.entries(reply as unknown as Record<string, unknown>).filter(([field]) =>
          (field.startsWith("configured_") && field !== "configured_audio_sink") ||
          preservedPolicyFields.includes(field)),
      );
    const confirms = (reply: NvhttpSettings): boolean =>
      reply.revision === current.revision + 1 &&
      reply.stream_audio === streamAudio &&
      reply.configured_audio_sink === configuredAudioSink &&
      sameConfigurationValue(persistedHostSettings(reply), persistedHostSettings(current));
    const updated = parseNvhttpSettingsReply(await api.post<unknown>("nvhttp/settings_update", {
      hostname: current.configured_hostname,
      port: current.port,
      expected_revision: current.revision,
      stream_audio: streamAudio,
      audio_sink: configuredAudioSink,
    }));
    if (!confirms(updated))
      throw new Error("Audio preferences could not be confirmed without persisted host settings changing. Refresh and check the current values.");
    const saved = parseNvhttpSettingsReply(await api.post<unknown>("nvhttp/settings"));
    if (!confirms(saved))
      throw new Error("Audio preferences could not be confirmed after saving without persisted host settings changing. Refresh and check the current values.");
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

  export function renderNvhttpAudioSettings(
    context: PageContext,
    settings: NvhttpSettings,
    endpoints: Array<{ id: string; name: string; default_multimedia: boolean }>,
    discoveryIssue: string | null,
    reload: () => Promise<void>,
  ): HTMLElement {
    const form = el("form", "settings-form");
    const streamAudio = input("stream_audio_preference", "", "checkbox");
    streamAudio.checked = settings.stream_audio;
    const audioSink = el("select");
    audioSink.name = "audio_sink_preference";
    const automatic = el("option", "", "Automatic (host startup endpoint)");
    automatic.value = "";
    audioSink.append(automatic);
    for (const endpoint of endpoints) {
      if (!endpoint.name || endpoint.name.length > 512 || /[\u0000-\u001f\u007f-\u009f]/.test(endpoint.name))
        continue;
      const option = el("option", "", `${endpoint.name}${endpoint.default_multimedia ? " (default)" : ""} (${endpoint.id})`);
      option.value = endpoint.id;
      audioSink.append(option);
    }
    const configuredAudioSink = settings.configured_audio_sink;
    if (configuredAudioSink !== null && !endpoints.some(({ id }) => id === configuredAudioSink)) {
      try {
        if (parseAudioSinkSelection(configuredAudioSink) === configuredAudioSink) {
          const saved = el("option", "", `Saved endpoint (not currently listed): ${configuredAudioSink}`);
          saved.value = configuredAudioSink;
          audioSink.append(saved);
        }
      } catch {
        // Keep an invalid saved ID out of the editable control; it cannot be submitted.
      }
    }
    audioSink.value = configuredAudioSink ?? "";
    const status = el("div");
    const save = el("button", "button primary", "Save audio preferences");
    save.type = "submit";
    save.disabled = !context.api.allows("configuration.write");
    form.append(
      field("Include host audio in future streams", streamAudio,
        "Applications and paired-device overrides may choose a different value."),
      field("Audio capture endpoint", audioSink,
        "Automatic follows the host startup endpoint. A named endpoint applies to future streams."),
      ...(discoveryIssue ? [el("p", "muted", discoveryIssue)] : []),
      ...(!context.api.allows("configuration.write")
        ? [el("p", "muted", "Saving audio preferences requires configuration.write permission.")]
        : []),
      status,
      el("div", "actions", save, badge(`Revision ${settings.revision}`)),
    );
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (!form.reportValidity()) return;
      void runAction(save, status, async () => {
        if (!context.api.allows("configuration.write")) {
          status.replaceChildren(el("p", "error", "Saving audio preferences requires configuration.write permission."));
          return;
        }
        try {
          await saveNvhttpAudioSettings(context.api, settings, streamAudio.checked, audioSink.value);
        } catch (error) {
          if (error instanceof ApiError && error.code === "revision_conflict") {
            await reload();
            context.notify("Host settings changed elsewhere. Review the latest audio preferences before saving again.");
            return;
          }
          throw error;
        }
        await reload();
        context.notify("Audio preferences saved and read back. They apply to future streams.");
      });
    });
    return panel("Future stream audio", form);
  }

  export async function recoverNvhttpSaveFailure(
    code: string,
    reload: () => Promise<void>,
    notify: (message: TextValue) => void,
  ): Promise<boolean> {
    if (code !== "revision_conflict" &&
        !["listener_apply_pending", "listener_apply_failed", "listener_not_active"].includes(code))
      return false;
    await reload();
    notify(code === "revision_conflict"
      ? settingsTextMessage("Host settings changed elsewhere. Review the latest values before saving.")
      : settingsTextMessage("Host settings were stored, but listener application is not confirmed. Review the active ports and listener error."));
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
    if (Object.hasOwn(document, "logging")) {
      const logging = document.logging;
      if (!logging || typeof logging !== "object" || Array.isArray(logging))
        throw new Error("Host logging application confirmation is invalid.");
      const confirmation = logging as Record<string, unknown>;
      if (Object.keys(confirmation).sort().join(",") !== "applied_revision,saved_revision,status" ||
          confirmation.saved_revision !== document.revision ||
          confirmation.applied_revision !== document.revision ||
          confirmation.status !== "applied")
        throw new Error("Host logging application confirmation is invalid.");
    }
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
    if (!confirmsHostLoggingUpdate(updated, current, logging) || !updated.logging)
      throw new Error("Host logging update could not be confirmed. Refresh and check the current values.");
    const saved = parseHostConfigurationReply(await api.post<unknown>("host-configuration/get"));
    if (!confirmsHostLoggingUpdate(saved, current, logging))
      throw new Error("Host logging settings could not be confirmed after saving. Refresh and check the current values.");
    return { ...saved, logging: updated.logging };
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

  export function hostLoggingSaveFailureNotice(code: string | null): string {
    switch (code) {
      case "logging_apply_failed":
        return "Host logging application failed and the previous settings were restored. Review the refreshed policy before saving again.";
      case "save_unconfirmed":
        return "Host logging write outcome is unknown. Review the refreshed policy and diagnostics status before retrying.";
      case "logging_application_unconfirmed":
        return "Host logging settings may be saved, but application is unconfirmed. Review the refreshed policy and diagnostics status before retrying.";
      case "logging_rollback_failed":
        return "Host logging application failed and previous settings could not be restored. Configuration may differ from the active policy; review both statuses before retrying.";
      case "logging_rollback_unconfirmed":
        return "Host logging application failed and restoration is unconfirmed. Configuration may differ from the active policy; review both statuses before retrying.";
      default:
        return "Host logging save could not be confirmed. Review the refreshed policy before saving again.";
    }
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
    keybindings: unknown = [16, 160, 17, 162, 18, 164],
    motionAsDs4: boolean = true,
    touchpadAsDs4: boolean = true,
    protonDualsenseCompatibility: boolean = true,
    ds5InputtinoRandomizeMac: boolean = true,
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
      typeof nativePenTouch !== "boolean" ||
      typeof motionAsDs4 !== "boolean" ||
      typeof touchpadAsDs4 !== "boolean" ||
      typeof protonDualsenseCompatibility !== "boolean"
      || typeof ds5InputtinoRandomizeMac !== "boolean"
    )
      throw new Error("Input policy values, key repeat settings, or Back timeout are invalid.");
    return {
      keyboard,
      mouse,
      high_resolution_scrolling: highResolutionScrolling,
      always_send_scancodes: alwaysSendScancodes,
      key_rightalt_to_key_win: keyRightAltToKeyWin,
      keybindings: validateKeybindings(keybindings),
      key_repeat_delay: keyRepeatDelay,
      key_repeat_frequency: keyRepeatFrequency,
      back_button_timeout: backButtonTimeout,
      gamepad: gamepadProfile,
      controller: controllerEnabled,
      native_pen_touch: nativePenTouch,
      motion_as_ds4: motionAsDs4,
      touchpad_as_ds4: touchpadAsDs4,
      proton_dualsense_compatibility: protonDualsenseCompatibility,
      ds5_inputtino_randomize_mac: ds5InputtinoRandomizeMac,
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
    if (typeof values.motion_as_ds4 !== "boolean" || typeof values.touchpad_as_ds4 !== "boolean" ||
        typeof values.proton_dualsense_compatibility !== "boolean" ||
        typeof values.ds5_inputtino_randomize_mac !== "boolean")
      throw new Error("Input policy settings reply omitted controller or Proton compatibility policies.");
    const keybindings = validateKeybindings(values.keybindings);
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
        keybindings,
        values.motion_as_ds4,
        values.touchpad_as_ds4,
        values.proton_dualsense_compatibility,
        values.ds5_inputtino_randomize_mac,
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

  export function parseGlobalFecPercentage(value: string): number | null {
    return parseClientFecOverride(value).fec_percentage;
  }

  export function parseEncryptionMode(value: string): 0 | 1 | 2 {
    if (value === "0") return 0;
    if (value === "1") return 1;
    if (value === "2") return 2;
    throw new Error("Encryption mode must be disabled, optional, or required.");
  }

  export function parseClientCaptureOverride(value: string): CaptureSelection | null {
    if (value === "") return null;
    return parseCaptureSelection(value);
  }

  const forwardablePairingOverrideFields = [
    "fec_percentage", "stream_audio", "audio_sink", "virtual_sink", "sw_preset", "sw_tune", "min_threads", "qp", "encoder",
    "audio_sink_capture_only", "back_button_timeout", "controller", "gamepad",
    "keyboard", "mouse", "native_pen_touch", "ds4_back_as_touchpad_click",
    "motion_as_ds4", "touchpad_as_ds4", "ds5_inputtino_randomize_mac",
    "proton_dualsense_compatibility",
    "max_bitrate", "minimum_fps_target", "video_max_batch_size_kb",


    "nvenc_spatial_aq", "nvenc_preset", "nvenc_h264_cavlc",
    "nvenc_twopass", "nvenc_vbv_increase", "nvenc_split_encode", "nvenc_realtime_hags",
    "nvenc_opengl_vulkan_on_dxgi", "nvenc_latency_over_power", "vaapi_strict_rc_buffer",
    "high_resolution_scrolling", "capture",
    "always_send_scancodes", "key_rightalt_to_key_win", "key_repeat_delay",
    "key_repeat_frequency", "keybindings", "display_policy", "requested_resolution_only",
  ];

  export function unsupportedPairingOverrideFields(client: PairingClient): string[] {
    const overrides = client.config_overrides ?? {};
    return Object.entries(overrides)
      .filter(([field, value]) => forwardablePairingOverrideFields.includes(field) &&
        !validStoredPairingOverride(field, value))
      .map(([field]) => field);
  }

  function validStoredPairingOverride(field: string, value: unknown): boolean {
    if (field === "display_policy") {
      if (!value || typeof value !== "object" || Array.isArray(value)) return false;
      try { parseDisplayPolicyOverrideMap(JSON.stringify(value), "client"); return true; } catch { return false; }
    }
    if (field === "keybindings") {
      if (value == null) return true;
      try { validateKeybindings(value); return true; } catch { return false; }
    }
    if (value === null)
      return ["motion_as_ds4", "touchpad_as_ds4", "ds5_inputtino_randomize_mac",
        "proton_dualsense_compatibility", "requested_resolution_only"].includes(field);
    if (value === undefined) return false;
    if (["stream_audio", "audio_sink_capture_only", "controller", "keyboard", "mouse",
      "native_pen_touch", "ds4_back_as_touchpad_click",
      "motion_as_ds4", "touchpad_as_ds4", "ds5_inputtino_randomize_mac",
      "proton_dualsense_compatibility",
      "nvenc_spatial_aq", "nvenc_h264_cavlc", "nvenc_realtime_hags",
      "nvenc_opengl_vulkan_on_dxgi", "nvenc_latency_over_power", "vaapi_strict_rc_buffer",
      "high_resolution_scrolling", "always_send_scancodes",
      "key_rightalt_to_key_win", "requested_resolution_only"].includes(field)) return typeof value === "boolean";
    if (field === "audio_sink" || field === "virtual_sink")
      return typeof value === "string" && new TextEncoder().encode(value).length <= 4096 &&
        !/[\u0000-\u001f\u007f-\u009f]/.test(value);
    if (field === "sw_preset" || field === "sw_tune")
      return typeof value === "string" &&
        new TextEncoder().encode(value).length <= (field === "sw_preset" ? 128 : 256) &&
        !/[\u0000-\u001f\u007f-\u009f]/u.test(value);
    if (field === "encoder") return value === "auto" || value === "software";
    if (field === "capture") return ["auto", "ddx", "wgc", "wgcc", "pipewire", "x11", "kms"].includes(value as string);
    if (field === "min_threads")
      return Number.isSafeInteger(value) && (value as number) >= -2147483648 && (value as number) <= 2147483647;
    if (field === "qp")
      return Number.isSafeInteger(value) && (value as number) >= -2147483648 && (value as number) <= 2147483647;
    if (field === "gamepad")
      return typeof value === "string" && recognizedInputGamepadProfiles.includes(value);
    if (field === "nvenc_twopass")
      return value === "disabled" || value === "quarter_res" || value === "full_res";
    if (field === "nvenc_split_encode")
      return value === "auto" || value === "enabled" || value === "disabled";
    if (field === "nvenc_preset")
      return Number.isSafeInteger(value) && (value as number) >= 1 && (value as number) <= 7;
    if (field === "nvenc_vbv_increase")
      return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 400;
    if (field === "fec_percentage")
      return Number.isSafeInteger(value) && (value as number) >= 1 && (value as number) <= 255;
    if (field === "video_max_batch_size_kb")
      return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 64;
    if (field === "back_button_timeout" || field === "max_bitrate")
      return Number.isSafeInteger(value) && (value as number) >= -0x8000_0000 &&
        (value as number) <= 0x7fff_ffff;
    if (field === "key_repeat_delay")
      return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 0x7fff_ffff;
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
    return Object.fromEntries(Object.entries(values).filter(([field, value]) => value !== undefined &&
      (value !== null || !forwardablePairingOverrideFields.includes(field))));
  }

  export interface PairingOverrideMerge {
    values: Record<string, unknown>;
    conflicts: string[];
  }

  // A missing override means inherit; preserve that distinction from false,
  // zero, empty strings, and explicit values while comparing revisions.
  export function mergePairingOverrides(
    original: Record<string, unknown>,
    local: Record<string, unknown>,
    latest: Record<string, unknown>,
  ): PairingOverrideMerge {
    const values: Record<string, unknown> = { ...latest };
    const conflicts: string[] = [];
    const keys = new Set([...Object.keys(original), ...Object.keys(local), ...Object.keys(latest)]);
    const equalAt = (left: Record<string, unknown>, right: Record<string, unknown>, key: string): boolean =>
      Object.hasOwn(left, key) === Object.hasOwn(right, key) &&
      (!Object.hasOwn(left, key) || sameConfigurationValue(left[key], right[key]));
    for (const key of keys) {
      const localChanged = !equalAt(original, local, key);
      const remoteChanged = !equalAt(original, latest, key);
      if (localChanged && remoteChanged && !equalAt(local, latest, key)) {
        conflicts.push(key);
      } else if (localChanged) {
        if (Object.hasOwn(local, key)) values[key] = local[key];
        else delete values[key];
      }
    }
    return { values, conflicts };
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

  export async function removePairedClient(
    api: Pick<Api, "post">,
    current: PairingClients,
    certificateId: string,
  ): Promise<PairingClients> {
    if (!current.clients.some((client) => client.certificate_id === certificateId) ||
        !Number.isSafeInteger(current.revision) || current.revision >= Number.MAX_SAFE_INTEGER)
      throw new Error("Paired client or settings revision is invalid.");
    const response = await api.post<unknown>("pairing/unpair", {
      certificate_id: certificateId,
      expected_revision: current.revision,
    });
    if (!response || typeof response !== "object" || Array.isArray(response) ||
        (response as Record<string, unknown>).ok !== true)
      throw new Error("Device removal response was not confirmed.");
    const saved = parsePairingClientsReply(await api.post<unknown>("pairing/list_clients"));
    const remaining = current.clients.filter((client) => client.certificate_id !== certificateId);
    if (saved.revision !== current.revision + 1 || saved.clients.length !== remaining.length ||
        !remaining.every((before) => {
          const after = saved.clients.find((client) => client.certificate_id === before.certificate_id);
          return after !== undefined && sameConfigurationValue(before, after);
        }))
      throw new Error("Device removal could not be confirmed. Reload paired clients before trying again.");
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

  function sparseInputOverrideSelect(
    name: string,
    current: boolean | undefined,
    accessibleName = name,
  ): HTMLSelectElement {
    const select = el("select");
    select.name = name;
    settingsLocalizedAttributeTemplate(select, "{field} override", {
      field: settingsTextMessage(accessibleName),
    });
    const choices: Array<[string, string]> = [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]];
    for (const [value, label] of choices) {
      const option = el("option", "", label);
      option.value = value;
      select.append(option);
    }
    select.value = current === undefined ? "" : String(current);
    return select;
  }

  function sparseCaptureOverrideSelect(current: CaptureSelection | undefined): HTMLSelectElement {
    const select = el("select");
    select.setAttribute("aria-label", "Capture provider override");
    const choices: Array<[string, string]> = [
      ["", "Inherit"],
      ["auto", "Automatic host provider"],
      ["ddx", "Desktop Duplication"],
      ["wgc", "Windows Graphics Capture"],
      ["wgcc", "Windows Graphics Capture, constant rate"],
      ["pipewire", "PipeWire portal capture"],
      ["x11", "X11 / XWayland root readback"],
      ["kms", "DRM/KMS active primary plane readback"],
    ];
    for (const [value, label] of choices) {
      const option = el("option", "", label);
      option.value = value;
      select.append(option);
    }
    select.value = current ?? "";
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
    if (value === "auto" || value === "ddx" || value === "wgc" || value === "wgcc" || value === "pipewire" || value === "x11" || value === "kms")
      return value;
    throw new Error("Choose Automatic, a supported Windows capture mode, PipeWire, X11, or DRM/KMS.");
  }

  type PipewireSourceType = "monitor" | "window";
  type PipewireSelectionState = "permission_required" | "selecting" | "ready" | "denied" | "failed" | "cancelled";
  type PipewireCaptureReply = {
    availability: "available" | "unavailable";
    selection_state: PipewireSelectionState;
    reason?: string;
    source_type?: PipewireSourceType;
    width?: number;
    height?: number;
  };

  export function parsePipewireCaptureReply(value: unknown): PipewireCaptureReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("PipeWire capture status is invalid.");
    const reply = value as Record<string, unknown>;
    const selectionStates: PipewireSelectionState[] = ["permission_required", "selecting", "ready", "denied", "failed", "cancelled"];
    if ((reply.availability !== "available" && reply.availability !== "unavailable") ||
        !selectionStates.includes(reply.selection_state as PipewireSelectionState))
      throw new Error("PipeWire capture status is invalid.");
    if (reply.reason !== undefined && (typeof reply.reason !== "string" || reply.reason.length > 64 || !/^[a-z0-9_]+$/.test(reply.reason)))
      throw new Error("PipeWire capture status is invalid.");
    if (reply.source_type !== undefined && reply.source_type !== "monitor" && reply.source_type !== "window")
      throw new Error("PipeWire capture status is invalid.");
    for (const dimension of ["width", "height"] as const) {
      if (reply[dimension] !== undefined && (!Number.isSafeInteger(reply[dimension]) || (reply[dimension] as number) < 1 || (reply[dimension] as number) > 32768))
        throw new Error("PipeWire capture status is invalid.");
    }
    return {
      availability: reply.availability,
      selection_state: reply.selection_state as PipewireSelectionState,
      ...(reply.reason === undefined ? {} : { reason: reply.reason }),
      ...(reply.source_type === undefined ? {} : { source_type: reply.source_type }),
      ...(reply.width === undefined ? {} : { width: reply.width as number }),
      ...(reply.height === undefined ? {} : { height: reply.height as number }),
    };
  }

  function pipewireUnavailableDescription(reason?: string): string {
    switch (reason) {
      case "not_linux": return "PipeWire source selection is available on Linux hosts only.";
      case "portal_unavailable": return "The desktop capture portal is unavailable. Start a supported desktop session and refresh.";
      case "capture_unavailable": return "PipeWire capture is unavailable on this host.";
      default: return "PipeWire source selection is unavailable on this host.";
    }
  }

  function pipewireCancellationUnconfirmedDescription(): string {
    return "Cancellation could not be confirmed. The source selection may remain until the configured provider timeout; refresh to check the current status.";
  }

  export async function boundedPipewireRequest<T>(request: Promise<T>, timeoutMs: number): Promise<T> {
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      return await Promise.race([
        request,
        new Promise<never>((_, reject) => {
          timeout = setTimeout(() => reject(new Error("PipeWire capture request timed out.")), timeoutMs);
        }),
      ]);
    } finally {
      if (timeout !== undefined) clearTimeout(timeout);
    }
  }

  export function pipewireSelectionDescription(reply: PipewireCaptureReply): string {
    if (reply.availability === "unavailable") return pipewireUnavailableDescription(reply.reason);
    switch (reply.selection_state) {
      case "ready": {
        const source = reply.source_type === "window" ? "Window" : "Monitor";
        return reply.width !== undefined && reply.height !== undefined
          ? `${source} source selected (${reply.width} × ${reply.height}).`
          : `${source} source selected.`;
      }
      case "permission_required": return "Choose a monitor or window to grant capture permission.";
      case "selecting": return "A source chooser is currently open.";
      case "denied": return "Source selection was denied.";
      case "cancelled": return "Source selection was cancelled.";
      case "failed": return "Source selection failed. Refresh and try again.";
    }
  }

  export async function waitForPipewireSelection(api: Pick<Api, "post">, operationId: string,
    isActive: () => boolean, onState: (reply: PipewireCaptureReply) => void,
    pollIntervalMs = 400, timeoutMs = 15_000,
    cancelSelection: () => Promise<boolean | void> = () => api.post<unknown>("capture/pipewire_selection_cancel", { operation_id: operationId }).then(() => true)):
    Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline && isActive()) {
      await new Promise((resolve) => setTimeout(resolve, pollIntervalMs));
      if (!isActive()) break;
      let timeout: ReturnType<typeof setTimeout> | undefined;
      const poll = api.post<unknown>("capture/pipewire_selection_poll", { operation_id: operationId });
      let rawReply: unknown;
      try {
        rawReply = await Promise.race([
          poll,
          new Promise<never>((_, reject) => {
            timeout = setTimeout(() => reject(new Error("Source selection timed out and was cancelled.")),
              Math.max(1, Math.min(2_000, deadline - Date.now())));
          }),
        ]);
      } catch (error) {
        if (error instanceof Error && error.message === "Source selection timed out and was cancelled.") {
          const confirmed = await cancelSelection();
          throw new Error(confirmed === false
            ? "Source selection timed out; cancellation could not be confirmed."
            : "Source selection timed out and was cancelled.");
        }
        throw error;
      } finally {
        if (timeout !== undefined) clearTimeout(timeout);
      }
      const reply = parsePipewireCaptureReply(rawReply);
      if (!isActive()) {
        await cancelSelection();
        return;
      }
      onState(reply);
      if (reply.selection_state !== "selecting" && reply.selection_state !== "permission_required") return;
    }
    const confirmed = await cancelSelection();
    if (isActive()) throw new Error(confirmed === false
      ? "Source selection timed out; cancellation could not be confirmed."
      : "Source selection timed out and was cancelled.");
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

  type PairingEditorControl = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
  interface PairingControlState {
    name: string;
    tag: string;
    type: string;
    value: string;
    checked: boolean | null;
  }
  interface PairingEditorRow {
    client: PairingClient;
    card: HTMLElement;
    readOverrides: () => Record<string, unknown>;
    baseline: () => PairingControlState[];
    setBaseline: (states: PairingControlState[]) => void;
  }
  interface PairingRebaseSource {
    settings: PairingClients;
    rows: PairingEditorRow[];
    search: HTMLInputElement;
    errorArea: HTMLElement;
  }

  function pairingEditorControls(card: HTMLElement): PairingEditorControl[] {
    if (typeof card.querySelectorAll !== "function") return [];
    return Array.from(card.querySelectorAll<PairingEditorControl>("input, textarea, select"))
      .filter((control) => control.type !== "hidden" && !control.hasAttribute("data-draft-ignore"));
  }

  function pairingControlStates(card: HTMLElement): PairingControlState[] {
    return pairingEditorControls(card).map((control) => {
      if ("validity" in control && control.validity.badInput)
        throw new Error("Correct the incomplete number before reviewing the latest device settings.");
      return {
        name: control.name, tag: control.tagName, type: control.type, value: control.value,
        checked: control.tagName === "INPUT" && (control.type === "checkbox" || control.type === "radio")
          ? (control as HTMLInputElement).checked : null,
      };
    });
  }

  function pairingRowHasDraft(row: PairingEditorRow, tracker: DraftTracker | null): boolean {
    if (tracker?.isDirty(row.card)) return true;
    const current = pairingControlStates(row.card);
    const baseline = row.baseline();
    return current.length !== baseline.length || current.some((state, index) =>
      !pairingStateShapeMatches(state, baseline[index]!) ||
      pairingStateSignature(state) !== pairingStateSignature(baseline[index]!));
  }

  function pairingStateSignature(state: PairingControlState): string {
    return state.checked === null ? state.value : `${state.checked}:${state.value}`;
  }

  function pairingStateShapeMatches(left: PairingControlState, right: PairingControlState): boolean {
    return left.name === right.name && left.tag === right.tag && left.type === right.type &&
      (left.checked === null) === (right.checked === null);
  }

  function pairingRebaseValue(value: unknown, present: boolean): string {
    if (!present) return "inherit";
    const text = JSON.stringify(value) ?? "inherit";
    return text.length > 160 ? `${text.slice(0, 157)}…` : text;
  }

  export function createPairedClientEditor(
    context: PageContext,
    pairingArea: HTMLElement,
    draftTracker: () => DraftTracker | null = () => null,
    onSaveBusy: (busy: boolean) => void = () => {},
  ): () => Promise<void> {
    const pairingLoads = new PairingClientsLoadGate();
    let removeClientFromDirectory: (key: string) => void = () => {};
    async function loadPairing(rebase?: PairingRebaseSource): Promise<void> {
      if (!rebase) pairingArea.replaceChildren(el("p", "muted", "Loading paired clients…"));
      try {
        const loaded = await pairingLoads.read(context.api);
        if (loaded === null) return;
        let { settings } = loaded;
        const { inputStatus } = loaded;
        if (!context.content.isConnected) return;
        let displayedClients = settings.clients;
        if (rebase) {
          if (settings.revision < rebase.settings.revision)
            throw new Error("The host returned an older pairing revision; your drafts remain unchanged.");
          const mergedClients = new Map<string, PairingClient>();
          for (const oldRow of rebase.rows) {
            const original = rebase.settings.clients.find((client) => client.certificate_id === oldRow.client.certificate_id);
            const latest = settings.clients.find((client) => client.certificate_id === oldRow.client.certificate_id);
            if (!original) throw new Error("A device no longer has its original revision; your drafts remain unchanged.");
            if (!latest) {
              if (pairingRowHasDraft(oldRow, draftTracker()))
                throw new Error(`${original.name} was removed elsewhere and has unsaved edits. Copy or review them before reloading.`);
              continue;
            }
            if (latest.client_id !== original.client_id || latest.paired_at !== original.paired_at)
              throw new Error(`${original.name} was replaced elsewhere. Your drafts remain unchanged.`);
            if (unsupportedPairingOverrideFields(original).length || unsupportedPairingOverrideFields(latest).length) {
              if (pairingRowHasDraft(oldRow, draftTracker()))
                throw new Error(`${original.name} has settings this editor cannot safely reconcile. Your drafts remain unchanged.`);
              mergedClients.set(latest.certificate_id, latest);
              continue;
            }
            const base = canonicalPairingOverrides(original.config_overrides ?? {});
            const local = oldRow.readOverrides();
            const remote = canonicalPairingOverrides(latest.config_overrides ?? {});
            const merged = mergePairingOverrides(base, local, remote);
            if (merged.conflicts.length) {
              const details = merged.conflicts.map((key) =>
                `${key}: yours ${pairingRebaseValue(local[key], Object.hasOwn(local, key))}, latest ${pairingRebaseValue(remote[key], Object.hasOwn(remote, key))}`).join("; ");
              throw new Error(`${original.name} has conflicting changes: ${details}. Your drafts remain unchanged. Enter the latest values and review again, then reapply any changes you still want.`);
            }
            mergedClients.set(latest.certificate_id, { ...latest, config_overrides: merged.values });
          }
          displayedClients = settings.clients.map((client) => mergedClients.get(client.certificate_id) ?? client);
        }
        if (settings.clients.length === 0) {
          pairingArea.replaceChildren(
            panel(
              "Paired client overrides",
              empty("No paired clients", "Pair a Moonlight client before setting client-specific streaming values."),
              badge(settingsTextLiteral(`Revision ${settings.revision}`)),
            ),
          );
          return;
        }
        const revisionBadge = badge(settingsTextLiteral(`Revision ${settings.revision}`));
        const clientSaveControls: { control: HTMLButtonElement; allowed: boolean }[] = [];
        let clientSaveInProgress = false;
        let clientReloadRequired = false;
        function reviewLatestAction(): HTMLElement {
          const errors = el("div");
          const review = button("Review latest and keep drafts", () => {
            void runAction(review, errors, async () => {
              if (clientSaveInProgress) return;
              clientSaveInProgress = true;
              onSaveBusy(true);
              try {
                await loadPairing({ settings, rows, search: clientSearch, errorArea: errors });
              } finally {
                clientSaveInProgress = false;
                onSaveBusy(false);
              }
            });
          });
          return el("div", "client-rebase-actions", review, errors);
        }
        const buildClientRow = (client: PairingClient): PairingEditorRow => {
          const fec = input(
            `fec_${client.certificate_id}`,
            String(client.config_overrides?.fec_percentage ?? ""),
            "number",
          );
          fec.min = "1";
          fec.max = "255";
          fec.step = "1";
          fec.placeholder = "Inherit";
          settingsClientOverrideAttribute(fec, "FEC percentage", client.name);
          const backButtonTimeout = input(
            `back_button_timeout_${client.certificate_id}`,
            client.config_overrides?.back_button_timeout?.toString() ?? "",
            "number",
          );
          backButtonTimeout.min = String(-0x8000_0000);
          backButtonTimeout.max = String(0x7fff_ffff);
          backButtonTimeout.step = "1";
          backButtonTimeout.placeholder = "Inherit";
          settingsClientOverrideAttribute(
            backButtonTimeout,
            "Back button timeout (milliseconds)",
            client.name,
          );
          const controller = sparseInputOverrideSelect(
            "controller",
            client.config_overrides?.controller,
            "Controller input",
          );
          const gamepadChoices = inputGamepadProfileChoices(
            client.config_overrides?.gamepad ?? "",
            inputStatus?.supported_profiles ?? ["auto"],
            inputStatus?.recognized_profiles ?? ["auto", ...(client.config_overrides?.gamepad ? [client.config_overrides.gamepad] : [])],
          );
          const gamepad = el("select");
          settingsClientOverrideAttribute(gamepad, "Gamepad profile", client.name);
          const inheritGamepad = el("option", "", "Inherit");
          inheritGamepad.value = "";
          gamepad.append(inheritGamepad);
          for (const choice of gamepadChoices) {
            const option = el("option", "", settingsTextLiteral(choice.label));
            option.value = choice.value;
            gamepad.append(option);
          }
          gamepad.value = client.config_overrides?.gamepad ?? "";
          const keyboard = sparseInputOverrideSelect(
            "keyboard",
            client.config_overrides?.keyboard,
            "Keyboard input",
          );
          settingsClientOverrideAttribute(keyboard, "Keyboard input", client.name);
          const mouse = sparseInputOverrideSelect(
            "mouse",
            client.config_overrides?.mouse,
            "Mouse input",
          );
          settingsClientOverrideAttribute(mouse, "Mouse input", client.name);
          const nativePenTouch = sparseInputOverrideSelect(
            "native_pen_touch",
            client.config_overrides?.native_pen_touch,
            "Native pen and touch policy",
          );
          settingsClientOverrideAttribute(
            nativePenTouch,
            "Native pen and touch policy",
            client.name,
          );
          const ds4BackAsTouchpadClick = sparseInputOverrideSelect(
            "ds4_back_as_touchpad_click", client.config_overrides?.ds4_back_as_touchpad_click,
          );
          settingsClientOverrideAttribute(
            ds4BackAsTouchpadClick,
            "DS4 Back as touchpad click",
            client.name,
          );
          const motionAsDs4 = sparseInputOverrideSelect("motion_as_ds4", client.config_overrides?.motion_as_ds4);
          settingsClientOverrideAttribute(motionAsDs4, "Motion selects DS4", client.name);
          const touchpadAsDs4 = sparseInputOverrideSelect("touchpad_as_ds4", client.config_overrides?.touchpad_as_ds4);
          settingsClientOverrideAttribute(touchpadAsDs4, "Touchpad selects DS4", client.name);
          const ds5InputtinoRandomizeMac = sparseInputOverrideSelect(
            "ds5_inputtino_randomize_mac", client.config_overrides?.ds5_inputtino_randomize_mac,
          );
          settingsClientOverrideAttribute(ds5InputtinoRandomizeMac, "DS5 random MAC", client.name);
          const protonDualsenseCompatibility = sparseInputOverrideSelect(
            "proton_dualsense_compatibility", client.config_overrides?.proton_dualsense_compatibility,
          );
          settingsClientOverrideAttribute(
            protonDualsenseCompatibility,
            "Proton DualSense compatibility",
            client.name,
          );
          const advancedInput = advancedInputOverrideControls(client.config_overrides);
          const softwareEncoder = softwareEncoderOverrideControls(client.config_overrides);
          const nvencSettings = nvencSettingsOverrideControls(client.config_overrides);
          const streamAudio = el("select");
          settingsClientOverrideAttribute(streamAudio, "Audio streaming", client.name);
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
          settingsClientOverrideAttribute(audioSinkMode, "Audio capture endpoint", client.name);
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
          settingsClientOverrideAttribute(audioSinkId, "Specific endpoint ID", client.name);
          const updateAudioSink = () => { audioSinkId.disabled = audioSinkMode.value !== "specific"; };
          audioSinkMode.addEventListener("change", updateAudioSink);
          updateAudioSink();
          const virtualSinkMode = el("select");
          settingsClientOverrideAttribute(virtualSinkMode, "Virtual audio sink", client.name);
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
          settingsClientOverrideAttribute(virtualSinkSelector, "Virtual sink ID or name", client.name);
          const updateVirtualSink = () => { virtualSinkSelector.disabled = virtualSinkMode.value !== "specific"; };
          virtualSinkMode.addEventListener("change", updateVirtualSink);
          updateVirtualSink();
          const captureOnly = el("select");
          settingsClientOverrideAttribute(
            captureOnly,
            "Capture audio without changing default outputs",
            client.name,
          );
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
          const capture = sparseCaptureOverrideSelect(client.config_overrides?.capture);
          const requestedResolutionOnly = sparseInputOverrideSelect(
            "requested_resolution_only", client.config_overrides?.requested_resolution_only,
            "Only advertise requested resolution",
          );
          settingsClientOverrideAttribute(requestedResolutionOnly, "Only advertise requested resolution", client.name);
          const displayPolicy = el("textarea");
          displayPolicy.rows = 12;
          displayPolicy.spellcheck = false;
          displayPolicy.value = JSON.stringify(client.config_overrides?.display_policy ?? {}, null, 2);
          const status = el("div");
          const unsupported = unsupportedPairingOverrideFields(client);
          const readValues = () => ({
            ...advancedInput.read(),
            ...softwareEncoder.read(),
            ...nvencSettings.read(),
            ...parseClientFecOverride(fec.value),
            back_button_timeout: parseBackButtonTimeoutOverride(backButtonTimeout.value),
            stream_audio: parseStreamAudioOverride(streamAudio.value),
            audio_sink: parseAudioSinkOverride(audioSinkMode.value, audioSinkId.value),
            virtual_sink: parseVirtualSinkOverride(virtualSinkMode.value, virtualSinkSelector.value),
            audio_sink_capture_only: parseAudioSinkCaptureOnlyOverride(captureOnly.value),
            capture: parseClientCaptureOverride(capture.value),
            display_policy: parseDisplayPolicyOverrideMap(displayPolicy.value, "client"),
            requested_resolution_only: parseInputBooleanOverride(
              requestedResolutionOnly.value, "Only advertise requested resolution",
            ),
            controller: parseInputBooleanOverride(controller.value, "Controller input"),
            gamepad: parseGamepadProfileOverride(
              gamepad.value,
              inputStatus?.recognized_profiles ?? ["auto", ...(client.config_overrides?.gamepad ? [client.config_overrides.gamepad] : [])],
            ),
            keyboard: parseInputBooleanOverride(keyboard.value, "Keyboard input"),
            mouse: parseInputBooleanOverride(mouse.value, "Mouse input"),
            native_pen_touch: parseInputBooleanOverride(nativePenTouch.value, "Native pen/touch preference"),
            ds4_back_as_touchpad_click: parseInputBooleanOverride(
              ds4BackAsTouchpadClick.value, "DS4 Back as touchpad click preference",
            ),
            motion_as_ds4: parseInputBooleanOverride(motionAsDs4.value, "Motion selects DS4 preference"),
            touchpad_as_ds4: parseInputBooleanOverride(touchpadAsDs4.value, "Touchpad selects DS4 preference"),
            ds5_inputtino_randomize_mac: parseInputBooleanOverride(
              ds5InputtinoRandomizeMac.value, "DS5 random MAC preference",
            ),
            proton_dualsense_compatibility: parseInputBooleanOverride(
              protonDualsenseCompatibility.value, "Proton DualSense compatibility",
            ),
          });
          const save = button(
            "Save",
            () => {
              void runAction(save, status, async () => {
                if (clientSaveInProgress || clientReloadRequired) return;
                clientSaveInProgress = true;
                onSaveBusy(true);
                for (const item of clientSaveControls) item.control.disabled = true;
                pairingLoads.invalidate();
                let saveStarted = false;
                try {
                  const values = readValues();
                  const submittedControls = pairingControlStates(clientCard);
                  const submittedDraft = draftTracker()?.capture(clientCard);
                  saveStarted = true;
                  settings = await savePairingClientOverrides(context.api, settings, client.certificate_id, values);
                  baselineControls = submittedControls;
                  if (submittedDraft) draftTracker()?.markSaved(submittedDraft);
                  revisionBadge.textContent = `Revision ${settings.revision}`;
                  context.notify(`Stored overrides confirmed for ${client.name}. Active stream application is not confirmed.`);
                } catch (error) {
                  if (saveStarted) {
                    clientReloadRequired = true;
                    const reload = button("Reload paired clients", () => {
                      if (draftTracker()?.isDirty(pairingArea) &&
                          !window.confirm("Reload paired clients and discard unsaved client changes?")) return;
                      void loadPairing();
                    });
                    const notice = el("div", "notice error",
                      el("strong", "", error instanceof ApiError && error.code === "revision_conflict"
                        ? "Paired clients changed elsewhere"
                        : "Client save could not be confirmed"),
                      el("p", "", "Your edits are still here. Review the latest settings to keep non-conflicting drafts, or reload and discard unsaved client changes."),
                      reviewLatestAction(), reload);
                    notice.setAttribute("role", "alert");
                    status.replaceChildren(notice);
                    return;
                  }
                  throw error;
                } finally {
                  clientSaveInProgress = false;
                  onSaveBusy(false);
                  for (const item of clientSaveControls) item.control.disabled = !item.allowed || clientReloadRequired;
                }
              }).finally(() => {
                if (clientReloadRequired) save.disabled = true;
              });
            },
            "primary",
          );
          save.disabled = !context.api.allows("pairing.admin") || unsupported.length > 0;
          clientSaveControls.push({ control: save, allowed: !save.disabled });
          if (unsupported.length > 0)
            status.textContent = `Save unavailable: this client has stored overrides this editor cannot preserve (${unsupported.join(", ")}).`;
          const remove = button("Remove device", () => {
            if (remove.disabled || clientSaveInProgress || clientReloadRequired) return;
            if (!window.confirm(`Remove ${client.name}? This device will need to pair again. Unsaved changes for this device will be discarded.`)) return;
            void runAction(remove, status, async () => {
              clientSaveInProgress = true;
              onSaveBusy(true);
              for (const item of clientSaveControls) item.control.disabled = true;
              pairingLoads.invalidate();
              try {
                settings = await removePairedClient(context.api, settings, client.certificate_id);
                if (!context.content.isConnected) return;
                clientCard.remove();
                const index = rows.findIndex((row) => row.client.certificate_id === client.certificate_id);
                if (index !== -1) rows.splice(index, 1);
                revisionBadge.textContent = `Revision ${settings.revision}`;
                removeClientFromDirectory(client.certificate_id);
                context.notify(`${client.name} was removed from paired devices. Existing stream termination is not confirmed.`);
              } catch (error) {
                if (!context.content.isConnected) return;
                clientReloadRequired = true;
                const reload = button("Reload paired clients", () => {
                  if (draftTracker()?.isDirty(pairingArea) &&
                      !window.confirm("Reload paired clients and discard unsaved client changes?")) return;
                  void loadPairing();
                });
                const notice = el("div", "notice error",
                  el("strong", "", error instanceof ApiError && error.code === "revision_conflict"
                    ? "Paired clients changed elsewhere" : "Device removal could not be confirmed"),
                  el("p", "", "Your edits are still here. Review the latest settings to keep non-conflicting drafts, or reload and discard unsaved client changes."),
                  reviewLatestAction(), reload);
                notice.setAttribute("role", "alert");
                status.replaceChildren(notice);
              } finally {
                clientSaveInProgress = false;
                onSaveBusy(false);
                for (const item of clientSaveControls) item.control.disabled = !item.allowed || clientReloadRequired;
              }
            }).finally(() => {
              if (clientReloadRequired) remove.disabled = true;
            });
          }, "danger");
          remove.disabled = !context.api.allows("pairing.admin");
          clientSaveControls.push({ control: remove, allowed: !remove.disabled });
          const pairedDate = client.paired_at > 0
            ? "Paired " + new Date(client.paired_at * 1000).toLocaleDateString()
            : "Pair date unavailable";
          const identity = el(
            "summary", "client-settings-summary",
            el("span", "client-settings-name", settingsTextLiteral(client.name)),
            el("small", "muted", settingsTextLiteral(client.client_id + " · " + pairedDate + " · certificate " + client.certificate_id.slice(0, 12) + "…")),
          );
          const audioSettings = el("details", "client-settings-group",
            el("summary", "", "Audio"),
            el("div", "form-grid",
              field("Audio streaming", streamAudio),
              field("Audio capture endpoint", audioSinkMode),
              field("Specific endpoint ID", audioSinkId),
              field("Virtual audio sink", virtualSinkMode),
              field("Virtual sink ID or name", virtualSinkSelector),
              field("Capture audio without changing default outputs", captureOnly)));
          const inputSettings = el("details", "client-settings-group",
            el("summary", "", "Input"),
            el("div", "form-grid",
              field("Controller input", controller),
              field("Gamepad profile", gamepad),
              field("Keyboard input", keyboard),
              field("Mouse input", mouse),
              field("Native pen and touch", nativePenTouch),
              field("DS4 Back as touchpad click", ds4BackAsTouchpadClick),
              field("Motion selects DS4", motionAsDs4),
              field("Touchpad selects DS4", touchpadAsDs4),
              field("DS5 random MAC", ds5InputtinoRandomizeMac),
              field("Proton DualSense compatibility", protonDualsenseCompatibility)));
          const advancedSettings = el("details", "client-settings-group",
            el("summary", "", "Advanced input and encoding"),
            el("div", "form-grid", ...advancedInput.fields, ...softwareEncoder.fields, ...nvencSettings.fields));
          const clientCard = el("details", "client-settings-card", identity,
            el("div", "client-settings-body",
              el("p", "muted", "Blank values inherit the application or host setting."),
              el("div", "form-grid",
                field("FEC percentage", fec),
                field("Back button timeout (milliseconds)", backButtonTimeout, "Use -1 to disable the timeout."),
                field("Capture provider", capture),
                field("Only advertise requested resolution", requestedResolutionOnly,
                  "Limit this client's private virtual display to the requested resolution. Requires a compatible Windows virtual display provider.")),
              field("Display and adapter policy (JSON)", displayPolicy,
                "Sparse per-client overrides. Use null or empty values to inherit; global-only keys are rejected."),
              audioSettings, inputSettings, advancedSettings,
              el("div", "client-settings-save", save, remove, status)));
          const pluginSettings = button("Plugin settings", () =>
            showSessionExtensionSettings(context, { kind: "paired_client", id: client.client_id }, `${client.name} plugin settings`));
          pluginSettings.disabled = !client.client_id || !context.api.allows("configuration.read");
          clientCard.append(pluginSettings);
          let baselineControls = pairingControlStates(clientCard);
          return {
            client, card: clientCard,
            readOverrides: () => canonicalPairingOverrides({ ...client.config_overrides, ...readValues() }),
            baseline: () => baselineControls,
            setBaseline: (states) => { baselineControls = states; },
          };
        };
        const rows = displayedClients.map(buildClientRow);
        const originalControlCount = clientSaveControls.length;
        const shadowRows = rebase ? settings.clients.map(buildClientRow) : [];
        clientSaveControls.splice(originalControlCount);
        const rebaseBaselines: DraftSnapshot = new Map();
        if (rebase) {
          for (const row of rows) {
            const shadow = shadowRows.find((candidate) => candidate.client.certificate_id === row.client.certificate_id);
            if (!shadow) throw new Error("A device changed while reviewing its settings.");
            const latestStates = pairingControlStates(shadow.card);
            const controls = pairingEditorControls(row.card);
            const currentStates = pairingControlStates(row.card);
            if (latestStates.length !== controls.length || currentStates.length !== controls.length)
              throw new Error("The device editor changed shape; your drafts remain unchanged.");
            const oldRow = rebase.rows.find((candidate) => candidate.client.certificate_id === row.client.certificate_id);
            if (oldRow) {
              const oldStates = pairingControlStates(oldRow.card);
              const oldBaseline = oldRow.baseline();
              if (oldStates.length !== controls.length || oldBaseline.length !== controls.length)
                throw new Error("The device editor changed shape; your drafts remain unchanged.");
              for (let index = 0; index < controls.length; index++) {
                const control = controls[index]!;
                const before = oldBaseline[index]!;
                const local = oldStates[index]!;
                const latest = latestStates[index]!;
                if (!pairingStateShapeMatches(before, local) || !pairingStateShapeMatches(before, latest))
                  throw new Error("The device editor changed shape; your drafts remain unchanged.");
                if (pairingStateSignature(before) !== pairingStateSignature(local)) {
                  control.value = local.value;
                  if (local.checked !== null) (control as HTMLInputElement).checked = local.checked;
                  if (control.value !== local.value)
                    throw new Error("A selected device setting is no longer available; your drafts remain unchanged.");
                  control.dispatchEvent(new Event("change"));
                }
              }
              if (!unsupportedPairingOverrideFields(row.client).length &&
                  !sameConfigurationValue(row.readOverrides(), canonicalPairingOverrides(row.client.config_overrides ?? {})))
                throw new Error(`${row.client.name} has interdependent edits that need review. Your drafts remain unchanged.`);
              (row.card as HTMLDetailsElement).open = (oldRow.card as HTMLDetailsElement).open;
              const oldDetails = oldRow.card.querySelectorAll<HTMLDetailsElement>("details");
              const newDetails = row.card.querySelectorAll<HTMLDetailsElement>("details");
              if (oldDetails.length === newDetails.length)
                oldDetails.forEach((detail, index) => { newDetails[index]!.open = detail.open; });
            }
            row.setBaseline(latestStates);
            controls.forEach((control, index) => rebaseBaselines.set(control, pairingStateSignature(latestStates[index]!)));
          }
        }
        const directory = createClientDirectory(rows.map(({ client, card }) => ({
          key: client.certificate_id,
          name: client.name,
          searchText: client.name + " " + client.client_id + " " + client.certificate_id,
          pairedAt: client.paired_at,
          card,
        })), rebase?.search.value ?? "");
        const clientSearch = directory.search;
        removeClientFromDirectory = directory.remove;
        pairingArea.replaceChildren(
          panel(
            "Paired client overrides",
            el("p", "muted", "Open a client to review its streaming overrides. Saves confirm the stored pairing revision; input preferences apply when the next stream acquires its input lease."),
            directory.element,
            el("div", "actions", revisionBadge),
          ),
        );
        if (rebase) {
          draftTracker()?.markSaved(rebaseBaselines);
          context.notify("Latest paired devices loaded. Your non-conflicting edits are still unsaved.");
        }
      } catch (error) {
        if (context.content.isConnected) {
          if (rebase) rebase.errorArea.replaceChildren(errorPanel(error));
          else pairingArea.replaceChildren(errorPanel(error, loadPairing));
        }
      }
    }
    return () => loadPairing();
  }

  export function settingsSearchMatches(query: string, ...parts: string[]): boolean {
    const needle = query.trim().toLocaleLowerCase();
    return !needle || parts.join(" ").toLocaleLowerCase().includes(needle);
  }

  type PendingProviderDraft = {
    baseline: Record<string, unknown>;
    submitted: Record<string, unknown>;
    latest?: Record<string, unknown>;
    merged?: Record<string, unknown>;
    overlaps: string[];
    reviewed: boolean;
  };

  function rebaseProviderDraft(
    pending: PendingProviderDraft,
    latest: Record<string, unknown>,
  ): { merged: Record<string, unknown>; overlaps: string[] } {
    const merged = { ...latest };
    const overlaps: string[] = [];
    for (const field of Object.keys(pending.submitted)) {
      const baselineValue = pending.baseline[field];
      const submittedValue = pending.submitted[field];
      const latestValue = latest[field];
      if (sameConfigurationValue(submittedValue, baselineValue)) continue;
      if (!sameConfigurationValue(latestValue, baselineValue)) {
        overlaps.push(field);
        continue;
      }
      merged[field] = submittedValue;
    }
    return { merged, overlaps };
  }

  export function renderSettings(context: PageContext): void {
    const area = el("div", "settings-area-registered");
    const sessionExtensionsArea = el("div", "settings-area-session-extensions");
    const registeredLoads = new RegisteredConfigurationLoadGate();
    const authArea = el("div", "settings-area-auth");
    const authLoads = new AuthTokenSettingsLoadGate();
    const hostLoggingArea = el("div", "settings-area-logging");
    const hostLoggingLoads = new HostLoggingLoadGate();
    const hostArea = el("div", "settings-area-host");
    const hostLoads = new NvhttpSettingsLoadGate();
    const pipewireArea = el("div", "settings-area-pipewire");
    const steamArea = el("div", "settings-area-steam");
    const steamLoads = new ImportSettingsLoadGate<SteamSettings>();
    let activeSteamExclusionPicker: SteamExclusionPicker | null = null;
    const lutrisArea = el("div", "settings-area-lutris");
    const lutrisLoads = new ImportSettingsLoadGate<{ settings: LutrisSettings; runtime: LutrisStatus }>();
    let activeLutrisExclusionEditor: AccessibleListEditor<LutrisExclusion> | null = null;
    const playniteArea = el("div", "settings-area-playnite");
    const playniteLoads = new PlayniteSettingsLoadGate();
    let activePlayniteListEditors: AccessibleListEditor<string>[] = [];
    const inputArea = el("div", "settings-area-input");
    const inputLoads = new InputPolicyLoadGate();
    const metricsArea = el("div", "settings-area-metrics");
    const updaterArea = el("div", "settings-area-updater");
    const systemTrayArea = el("div", "settings-area-tray");
    const pairingArea = el("div", "settings-area-pairing");
    let drafts: DraftTracker | null = null;
    const pendingProviderDrafts = new Map<string, PendingProviderDraft>();
    let clientSaveBusy = false;
    const updateDraftStatus = () => {
      const dirty = (drafts?.isDirty() ?? false) || pendingProviderDrafts.size > 0;
      refreshSettings.disabled = dirty || clientSaveBusy;
      dirtyNotice.hidden = !dirty && !clientSaveBusy;
      settingsSetLocalizedTextContent(dirtyNotice, clientSaveBusy ? "Saving changes…" : "Unsaved changes");
    };
    const configureProviderConflictReview = (
      provider: string,
      status: HTMLElement,
      save: HTMLButtonElement,
      applyValues: (values: Record<string, unknown>, fields: readonly string[]) => void,
    ): void => {
      const pending = pendingProviderDrafts.get(provider);
      if (!pending || pending.overlaps.length === 0) return;
      save.disabled = true;
      const message = el("p", "error", `Overlapping fields: ${pending.overlaps.join(", ")}. Latest values are kept until you choose an action.`);
      const review = button("Review and keep latest values", () => {
        pending.reviewed = true;
        if (pending.latest) applyValues(pending.latest, pending.overlaps);
        save.disabled = !context.api.allows("configuration.write");
        message.textContent = `Reviewed overlapping fields: ${pending.overlaps.join(", ")}. Latest values will be saved.`;
        review.disabled = true;
        overwrite.disabled = true;
      }, "secondary");
      const overwrite = button("Overwrite overlapping fields", () => {
        pending.reviewed = true;
        applyValues(pending.submitted, pending.overlaps);
        save.disabled = !context.api.allows("configuration.write");
        message.textContent = `Overwriting overlapping fields: ${pending.overlaps.join(", ")}. Review the draft before saving.`;
        review.disabled = true;
        overwrite.disabled = true;
      }, "secondary");
      status.append(message, review, overwrite);
    };
    const loadPairing = createPairedClientEditor(context, pairingArea, () => drafts, (busy) => {
      clientSaveBusy = busy;
      updateDraftStatus();
    });
    const managementArea = el("div", "settings-area-management");
    const csrfArea = el("div", "settings-area-csrf");
    const categories = [
      { id: "general", label: "General", description: "Host logging, updates, and registered settings.", areas: [area, sessionExtensionsArea, hostLoggingArea, updaterArea, systemTrayArea] },
      { id: "streaming", label: "Streaming", description: "Configure the host, capture, input, and session reporting.", areas: [hostArea, pipewireArea, inputArea, metricsArea] },
      { id: "integrations", label: "Integrations", description: "Manage application providers and library imports.", areas: [steamArea, lutrisArea, playniteArea] },
      { id: "devices", label: "Devices", description: "Manage paired client settings.", areas: [pairingArea] },
      { id: "access", label: "Access & security", description: "Configure session lifetimes and management access.", areas: [authArea, managementArea, csrfArea] },
    ];
    let selectedCategory = context.initialSettingsCategory ?? categories[0]!.id;
    const categoryContent = el("div", "settings-content");
    const categoryHeading = el("h2");
    const categoryDescription = el("p", "muted");
    const categoryStatus = el("div", "settings-category-heading", categoryHeading, categoryDescription);
    categoryStatus.setAttribute("aria-live", "polite");
    const noResults = el("p", "muted settings-no-results", "No settings match your search.");
    noResults.hidden = true;
    const search = input("settings_search", "", "search");
    search.placeholder = "Search settings";
    search.setAttribute("aria-label", "Search settings");
    const categoryPicker = el("select");
    categoryPicker.setAttribute("aria-label", "Settings category");
    const categoryNavigation = el("nav", "settings-nav");
    categoryNavigation.setAttribute("aria-label", "Settings categories");
    const categoryButtons = new Map<string, HTMLButtonElement>();
    const searchOpenedDetails = new Set<HTMLDetailsElement>();
    categoryContent.append(categoryStatus, noResults);
    for (const category of categories) {
      const option = el("option", "", category.label);
      option.value = category.id;
      categoryPicker.append(option);
      const item = button(category.label, () => selectCategory(category.id), "settings-nav-item");
      categoryButtons.set(category.id, item);
      categoryNavigation.append(item);
      categoryContent.append(...category.areas);
    }
    function selectCategory(id: string): void {
      if (!categories.some((category) => category.id === id)) return;
      selectedCategory = id;
      context.replaceSettingsCategory?.(id as SettingsCategory);
      search.value = "";
      updateCategoryDisplay();
    }
    function restoreSearchDisclosures(): void {
      for (const details of searchOpenedDetails) details.open = false;
      searchOpenedDetails.clear();
    }
    function showFieldInSearch(field: HTMLElement): void {
      let parent = field.parentElement;
      while (parent && parent !== categoryContent) {
        if (parent.tagName === "DETAILS") {
          const details = parent as HTMLDetailsElement;
          if (!details.open && !details.hidden) {
            details.open = true;
            searchOpenedDetails.add(details);
          }
        }
        parent = parent.parentElement;
      }
    }
    function updateCategoryDisplay(): void {
      const query = search.value.trim().toLocaleLowerCase();
      const selected = categories.find((category) => category.id === selectedCategory) ?? categories[0]!;
      categoryPicker.value = selected.id;
      restoreSearchDisclosures();
      let matches = 0;
      for (const category of categories) {
        for (const area of category.areas) {
          // Fields stay in their original forms so saves and unsaved drafts retain their semantics.
          const fields = typeof area.querySelectorAll === "function"
            ? Array.from(area.querySelectorAll<HTMLElement>(".field")) : [];
          let fieldMatches = 0;
          for (const wrapper of fields) {
            const controls = Array.from(wrapper.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>("input, select, textarea"));
            if (controls.length && controls.every((control) => control.hasAttribute("data-draft-ignore"))) continue;
            const panelTitle = wrapper.closest(".panel")?.querySelector("h2")?.textContent ?? "";
            const matched = !query || settingsSearchMatches(query, category.label, panelTitle,
              wrapper.textContent ?? "", ...controls.map((control) => control.name));
            if (query && !matched) wrapper.setAttribute("data-settings-search-hidden", "");
            else wrapper.removeAttribute("data-settings-search-hidden");
            if (query && matched) {
              fieldMatches++;
              showFieldInSearch(wrapper);
            }
          }
          const title = typeof area.querySelector === "function" ? area.querySelector(".panel > h2")?.textContent ?? "" : "";
          const visible = query
            ? fieldMatches > 0 || settingsSearchMatches(query, category.label, title) ||
              (!fields.length && settingsSearchMatches(query, area.textContent ?? ""))
            : category.id === selected.id;
          area.hidden = !visible;
          if (visible) matches++;
        }
        const item = categoryButtons.get(category.id)!;
        item.className = `button settings-nav-item${!query && category.id === selected.id ? " selected" : ""}`;
        item.setAttribute("aria-current", !query && category.id === selected.id ? "page" : "false");
      }
      settingsSetLocalizedTextContent(categoryHeading, query ? "Search results" : selected.label);
      settingsSetLocalizedTextContent(categoryDescription, query
        ? "Matching settings are shown across categories. Saving an editor also saves its hidden fields and unsaved changes."
        : selected.description);
      noResults.hidden = !query || matches !== 0;
    }
    // Native validation may find a required field that search has temporarily hidden.
    // Reveal its original category before the browser tries to focus it.
    categoryContent.addEventListener("invalid", (event) => {
      if (!search.value.trim()) return;
      const target = event.target as HTMLElement | null;
      const category = categories.find((candidate) => candidate.areas.some((area) => target && area.contains(target)));
      if (!category || !target) return;
      selectCategory(category.id);
      showFieldInSearch(target);
      searchOpenedDetails.clear();
    }, true);
    search.addEventListener("input", updateCategoryDisplay);
    categoryPicker.addEventListener("change", () => selectCategory(categoryPicker.value));
    const settingsNavigation = el("div", "settings-navigation", field("Category", categoryPicker), categoryNavigation);
    const refreshSettings = button("Refresh", () => {
      void load();
      void loadAuth();
      void loadHostLogging();
      void loadHost();
      void loadPipewireChooser();
      void loadSteam();
      void loadLutris();
      void loadPlaynite();
      void loadInput();
      renderExtensionSlot(context, metricsArea, "settings", owners => {
        if (typeof Amaterasu.renderSessionExtensionSettings === "function")
          Amaterasu.renderSessionExtensionSettings(context, sessionExtensionsArea, {kind:"host"}, undefined, owners);
      });
      renderUpdaterSettings(context, updaterArea);
      renderSystemTraySettings(context, systemTrayArea);
      void loadPairing();
    });
    const dirtyNotice = badge("Unsaved changes", "accent");
    dirtyNotice.hidden = true;
    dirtyNotice.setAttribute("role", "status");
    context.content.append(
      sectionHeading("Settings", "Configure your host, streaming, integrations, and access.", refreshSettings),
      el("div", "settings-tools", search, dirtyNotice),
      el("div", "settings-layout", settingsNavigation, categoryContent),
    );
    if (context.registerDraftGuard) {
      drafts = trackDrafts(categoryContent, updateDraftStatus);
      context.registerDraftGuard(() => clientSaveBusy || pendingProviderDrafts.size > 0 || (drafts?.isDirty() ?? false), () => {
        steamLoads.invalidate();
        lutrisLoads.invalidate();
        playniteLoads.invalidate();
        activeSteamExclusionPicker?.dispose();
        activeSteamExclusionPicker = null;
        activeLutrisExclusionEditor?.dispose();
        activeLutrisExclusionEditor = null;
        for (const editor of activePlayniteListEditors) editor.dispose();
        activePlayniteListEditors = [];
        drafts?.dispose();
      });
    }
    renderManagementPolicy(context, managementArea);
    renderCsrfPolicy(context, csrfArea);
    updateCategoryDisplay();
    if (typeof MutationObserver !== "undefined") {
      const observer = new MutationObserver(() => {
        if (!context.content.isConnected) {
          observer.disconnect();
        } else if (search.value.trim()) {
          updateCategoryDisplay();
        }
      });
      for (const category of categories)
        for (const item of category.areas)
          observer.observe(item, { childList: true, subtree: true, characterData: true });
    }
    let pipewireGeneration = 0;
    let pipewireOperation = 0;
    async function loadPipewireChooser(): Promise<void> {
      const generation = ++pipewireGeneration;
      pipewireOperation += 1;
      const status = el("p", "muted", "Checking PipeWire capture availability…");
      status.setAttribute("role", "status");
      status.setAttribute("aria-live", "polite");
      const monitor = button("Choose a monitor", () => void beginPipewireSelection("monitor"), "secondary");
      const windowSource = button("Choose a window", () => void beginPipewireSelection("window"), "secondary");
      monitor.disabled = true;
      windowSource.disabled = true;
      pipewireArea.replaceChildren(panel("PipeWire source", el("p", "muted", "Choose a source explicitly for the current host session. This does not change the saved capture backend."), el("div", "actions", monitor, windowSource), status));
      if (!context.api.allows("capture.read")) {
        settingsSetLocalizedTextContent(status, "PipeWire source status requires capture.read.");
        return;
      }
      try {
        const reply = parsePipewireCaptureReply(await boundedPipewireRequest(
          context.api.post<unknown>("capture/pipewire_status"), 5_000,
        ));
        if (!pipewireArea.isConnected || generation !== pipewireGeneration) return;
        status.textContent = pipewireSelectionDescription(reply);
        const available = reply.availability === "available";
        monitor.disabled = !available || !context.api.allows("capture.write");
        windowSource.disabled = !available || !context.api.allows("capture.write");
        if (available && !context.api.allows("capture.write"))
          status.textContent += " Starting selection requires capture.write.";
      } catch (error) {
        if (!pipewireArea.isConnected || generation !== pipewireGeneration) return;
        settingsSetLocalizedTextContent(
          status,
          error instanceof ApiError && error.code === "forbidden"
            ? "PipeWire source status requires capture.read."
            : "PipeWire source status is unavailable; refresh to check again.",
        );
      }
    }
    async function beginPipewireSelection(sourceType: PipewireSourceType): Promise<void> {
      if (!context.api.allows("capture.write")) return;
      const operation = ++pipewireOperation;
      const status = pipewireArea.querySelector<HTMLElement>("[role=status]");
      const monitor = Array.from(pipewireArea.querySelectorAll("button")).find((item) => item.textContent === "Choose a monitor");
      const windowSource = Array.from(pipewireArea.querySelectorAll("button")).find((item) => item.textContent === "Choose a window");
      if (!status) return;
      if (monitor) monitor.disabled = true;
      if (windowSource) windowSource.disabled = true;
      let cancelled = false;
      let beginPending = true;
      let operationId: string | null = null;
      let cancellation: Promise<boolean> | null = null;
      const requestCancel = (): Promise<boolean> => {
        if (operationId === null) return Promise.resolve(false);
        if (cancellation === null) {
          cancellation = boundedPipewireRequest(
            context.api.post<unknown>("capture/pipewire_selection_cancel", { operation_id: operationId }),
            3_000,
          ).then(() => true, () => false);
          const request = cancellation;
          void request.then((confirmed) => {
            if (!confirmed && cancellation === request) cancellation = null;
          });
        }
        return cancellation;
      };
      const cancel = button("Cancel selection", () => {
        cancelled = true;
        cancel.disabled = true;
        settingsSetLocalizedTextContent(status, "Cancelling source selection…");
        if (!beginPending) void requestCancel();
      }, "secondary");
      const cancelOperation = el("div", "actions", cancel);
      pipewireArea.append(cancelOperation);
      settingsSetLocalizedTextContent(status, "Opening the desktop source chooser…");
      try {
        const requestedId = crypto.randomUUID();
        operationId = requestedId;
        parsePipewireCaptureReply(await boundedPipewireRequest(context.api.post<unknown>("capture/pipewire_selection_begin", {
          source_type: sourceType,
          operation_id: requestedId,
        }), 10_000));
        beginPending = false;
        const active = () => pipewireArea.isConnected && pipewireOperation === operation && !cancelled;
        if (cancelled) await requestCancel();
        await waitForPipewireSelection(context.api, operationId, active, (reply) => {
          if (active()) status.textContent = pipewireSelectionDescription(reply);
        }, 400, 15_000, requestCancel);
        if (pipewireArea.isConnected && pipewireOperation === operation) {
          const finalStatus = parsePipewireCaptureReply(await boundedPipewireRequest(context.api.post<unknown>("capture/pipewire_selection_poll", { operation_id: operationId }), 5_000));
          if (cancelled || !pipewireArea.isConnected || pipewireOperation !== operation) {
            const confirmed = await requestCancel();
            if (pipewireArea.isConnected && pipewireOperation === operation)
              settingsSetLocalizedTextContent(
                status,
                confirmed
                  ? "Source selection was cancelled."
                  : pipewireCancellationUnconfirmedDescription(),
              );
          } else {
            status.textContent = pipewireSelectionDescription(finalStatus);
          }
        }
      } catch (error) {
        beginPending = false;
        const confirmedTimeout = error instanceof Error && error.message === "Source selection timed out and was cancelled.";
        const unconfirmedTimeout = error instanceof Error && error.message === "Source selection timed out; cancellation could not be confirmed.";
        let cancellationConfirmed: boolean | null = null;
        if (operationId !== null && !confirmedTimeout && !unconfirmedTimeout)
          cancellationConfirmed = await requestCancel();
        if (pipewireArea.isConnected && pipewireOperation === operation)
          settingsSetLocalizedTextContent(
            status,
            error instanceof ApiError && error.code === "forbidden"
              ? "PipeWire source selection requires capture.write."
              : unconfirmedTimeout || cancellationConfirmed === false
                ? pipewireCancellationUnconfirmedDescription()
                : confirmedTimeout
                  ? "Source selection timed out and cancellation was confirmed."
                  : "Source selection failed or timed out. Refresh and check the current status.",
          );
      } finally {
        cancelOperation.remove();
        if (pipewireArea.isConnected && pipewireOperation === operation) {
          if (monitor) monitor.disabled = false;
          if (windowSource) windowSource.disabled = false;
        }
      }
    }
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
          el("div", "actions", save, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
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
                context.notify(settingsTextMessage("Authentication settings changed elsewhere. Review the latest values and save again."));
                return;
              }
              throw error;
            }
            await loadAuth();
            context.notify(settingsTextMessage("Authentication lifetimes saved and confirmed."));
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
          const option = el("option", "", settingsTextLiteral(optionValue.replaceAll("_", " ")));
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
          field("Log path", path, "A relative path stays beneath the private host state root. The setting applies to diagnostic records when diagnostics confirms the policy."),
          el("p", "muted", settingsTextLiteral(`Effective host policy: ${settings.effective.min_log_level} · ${settings.effective.log_path}`)),
          el("p", "muted", "The host confirms logging application during save; diagnostics status checks disk delivery. Other host and native logs do not use this sink; a host restart may be required for their separate logging policies."),
          applicationStatus,
          refresh,
          status,
          el("div", "actions", save, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
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
                context.notify(settingsTextMessage("Host configuration changed elsewhere. Review the latest logging settings and save again."));
                return;
              }
              // A failed update reply or readback can occur after persistence. Reload
              // before another save so the editor never reuses an uncertain revision.
              await loadHostLogging();
              context.notify(hostLoggingSaveFailureNotice(error instanceof ApiError ? error.code : null));
              return;
            }
            await loadHostLogging();
          });
        });
        hostLoggingArea.replaceChildren(panel("Host logging", form));
      } catch {
        if (context.content.isConnected)
          hostLoggingArea.replaceChildren(errorPanel(new Error("Host logging policy could not be loaded. Try again."), loadHostLogging));
      }
    }
    async function loadSteam(
      draftValues?: SteamSettings["values"],
      draftNotice?: string,
    ): Promise<void> {
      activeSteamExclusionPicker?.dispose();
      activeSteamExclusionPicker = null;
      steamArea.replaceChildren(el("p", "muted", "Loading Steam settings…"));
      try {
        const loaded = await steamLoads.read(() => readSteamSettings(context.api));
        if (loaded === null) return;
        const { value: settings, generation } = loaded;
        if (!context.content.isConnected) return;
        const pendingDraft = pendingProviderDrafts.get("steam");
        if (pendingDraft) {
          const rebased = rebaseProviderDraft(pendingDraft, settings.values as unknown as Record<string, unknown>);
          pendingDraft.latest = settings.values as unknown as Record<string, unknown>;
          pendingDraft.merged = rebased.merged;
          pendingDraft.overlaps = rebased.overlaps;
          pendingDraft.reviewed = rebased.overlaps.length === 0;
          draftValues = rebased.merged as SteamSettings["values"];
          draftNotice = rebased.overlaps.length
            ? "Conflicting fields need review. Non-overlapping edits were rebased."
            : "Settings changed elsewhere. Non-overlapping edits were rebased; review before saving again.";
        }
        const valuesForForm = settings.values;
        const form = el("form", "settings-form");
        const enabled = input("steam_enabled", "", "checkbox");
        enabled.checked = valuesForForm.enabled;
        const autoSync = input("steam_auto_sync", "", "checkbox");
        autoSync.checked = valuesForForm.auto_sync;
        const includeTools = input("steam_include_tools", "", "checkbox");
        includeTools.checked = valuesForForm.include_tools;
        const syncAllInstalled = input(
          "steam_sync_all_installed",
          "",
          "checkbox",
        );
        syncAllInstalled.checked = valuesForForm.sync_all_installed;
        const autosyncRemoveUninstalled = input(
          "steam_autosync_remove_uninstalled",
          "",
          "checkbox",
        );
        autosyncRemoveUninstalled.checked =
          valuesForForm.autosync_remove_uninstalled;
        const recentGames = input(
          "steam_recent_games",
          String(valuesForForm.recent_games),
          "number",
        );
        recentGames.required = true;
        recentGames.min = "0";
        recentGames.max = "10000";
        recentGames.step = "1";
        const recentMaxAgeDays = input(
          "steam_recent_max_age_days",
          String(valuesForForm.recent_max_age_days),
          "number",
        );
        recentMaxAgeDays.required = true;
        recentMaxAgeDays.min = "0";
        recentMaxAgeDays.max = "36500";
        recentMaxAgeDays.step = "1";
        const exclusions = steamExclusionPicker(context.api, valuesForForm.exclude_games);
        activeSteamExclusionPicker = exclusions;
        const exclusionsField = el(
          "div",
          "field",
          el("span", "field-label", settingsTextMessage("Excluded games")),
          exclusions.node,
        );
        const status = el("div");
        if (draftNotice) settingsSetLocalizedTextContent(status, draftNotice);
        const runtimeStatus = el(
          "p",
          "muted",
          "Loading current provider status…",
        );
        const save = el("button", "button primary", "Save Steam settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        let actionInProgress = false;
        const cancel = button(settingsTextMessage("Cancel"), () => {
          if (!actionInProgress) {
            pendingProviderDrafts.delete("steam");
            updateDraftStatus();
            void loadSteam();
          }
        }, "secondary");
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
          exclusionsField,
          runtimeStatus,
          status,
          el("div", "actions", save, cancel, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            actionInProgress = true;
            cancel.disabled = true;
            steamLoads.invalidate();
            let saveStarted = false;
            let requestedValues: SteamSettings["values"] | null = null;
            try {
              const values = {
                enabled: enabled.checked,
                auto_sync: autoSync.checked,
                exclude_games: parseSteamExclusions(JSON.stringify(exclusions.value())),
                ...parseSteamImportSettings(
                  includeTools.checked,
                  syncAllInstalled.checked,
                  autosyncRemoveUninstalled.checked,
                  recentGames.valueAsNumber,
                  recentMaxAgeDays.valueAsNumber,
                ),
              };
              requestedValues = values;
              const existingPending = pendingProviderDrafts.get("steam");
              if (existingPending?.overlaps.length && !existingPending.reviewed)
                throw new Error("Review overlapping fields before saving again.");
              pendingProviderDrafts.set("steam", {
                baseline: settings.values as unknown as Record<string, unknown>,
                submitted: values as unknown as Record<string, unknown>,
                overlaps: [],
                reviewed: true,
              });
              updateDraftStatus();
              setSettingsFormDisabled(form, true);
              exclusions.setDisabled(true);
              saveStarted = true;
              const saved = await saveSteamSettings(context.api, settings, values);
              pendingProviderDrafts.delete("steam");
              updateDraftStatus();
              await loadSteam();
              context.notify(!values.enabled && saved.values.enabled
                ? settingsTextMessage("Steam settings saved and confirmed. Steam discovery remains enabled by Linux host policy; scan status is shown below.")
                : settingsTextMessage("Steam settings saved and confirmed. Scan status is shown below."));
            } catch (error) {
              if (saveStarted) {
                const conflict = error instanceof ApiError && error.code === "revision_conflict";
                const nextDraft = requestedValues ?? undefined;
                const notice = conflict
                  ? "Settings changed elsewhere. Your draft is preserved; review it before saving again."
                  : "Steam save could not be confirmed. Your submitted draft is preserved for review against the latest saved values.";
                await loadSteam(nextDraft, notice);
                context.notify(settingsTextMessage(conflict
                  ? "Steam settings changed elsewhere. Your draft is preserved for review."
                  : "Steam save could not be confirmed. Review your preserved draft against the latest saved values before saving again."));
                return;
              }
              throw error;
            } finally {
              actionInProgress = false;
              if (form.isConnected) setSettingsFormDisabled(form, false);
              exclusions.setDisabled(false);
              cancel.disabled = false;
            }
          });
        });
        steamArea.replaceChildren(panel("Steam", form));
        if (draftValues) {
          const baseline = drafts?.capture(steamArea);
          if (baseline) drafts?.markSaved(baseline);
          settingsSetDraftCheckbox(enabled, draftValues.enabled);
          settingsSetDraftCheckbox(autoSync, draftValues.auto_sync);
          settingsSetDraftCheckbox(includeTools, draftValues.include_tools);
          settingsSetDraftCheckbox(syncAllInstalled, draftValues.sync_all_installed);
          settingsSetDraftCheckbox(autosyncRemoveUninstalled, draftValues.autosync_remove_uninstalled);
          settingsSetDraftNumber(recentGames, draftValues.recent_games);
          settingsSetDraftNumber(recentMaxAgeDays, draftValues.recent_max_age_days);
          exclusions.setValue(draftValues.exclude_games);
          configureProviderConflictReview("steam", status, save, (values, fields) => {
            const selected = new Set(fields);
            if (selected.has("enabled")) settingsSetDraftCheckbox(enabled, values.enabled as boolean);
            if (selected.has("auto_sync")) settingsSetDraftCheckbox(autoSync, values.auto_sync as boolean);
            if (selected.has("include_tools")) settingsSetDraftCheckbox(includeTools, values.include_tools as boolean);
            if (selected.has("sync_all_installed")) settingsSetDraftCheckbox(syncAllInstalled, values.sync_all_installed as boolean);
            if (selected.has("autosync_remove_uninstalled")) settingsSetDraftCheckbox(autosyncRemoveUninstalled, values.autosync_remove_uninstalled as boolean);
            if (selected.has("recent_games")) settingsSetDraftNumber(recentGames, values.recent_games as number);
            if (selected.has("recent_max_age_days")) settingsSetDraftNumber(recentMaxAgeDays, values.recent_max_age_days as number);
            if (selected.has("exclude_games")) exclusions.setValue(values.exclude_games as SteamSettings["values"]["exclude_games"]);
          });
        }
        void loadSteamRuntimeStatus(runtimeStatus, context, settings, generation);
      } catch (error) {
        if (context.content.isConnected)
          steamArea.replaceChildren(errorPanel(error, () => { void loadSteam(draftValues, draftNotice); }));
      }
    }
    async function loadLutris(
      draftValues?: LutrisSettings["values"],
      draftNotice?: string,
    ): Promise<void> {
      activeLutrisExclusionEditor?.dispose();
      activeLutrisExclusionEditor = null;
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
        const pendingDraft = pendingProviderDrafts.get("lutris");
        if (pendingDraft) {
          const rebased = rebaseProviderDraft(pendingDraft, settings.values as unknown as Record<string, unknown>);
          pendingDraft.latest = settings.values as unknown as Record<string, unknown>;
          pendingDraft.merged = rebased.merged;
          pendingDraft.overlaps = rebased.overlaps;
          pendingDraft.reviewed = rebased.overlaps.length === 0;
          draftValues = rebased.merged as LutrisSettings["values"];
          draftNotice = rebased.overlaps.length
            ? "Conflicting fields need review. Non-overlapping edits were rebased."
            : "Settings changed elsewhere. Non-overlapping edits were rebased; review before saving again.";
        }
        const valuesForForm = settings.values;
        const form = el("form", "settings-form");
        const enabled = input("lutris_enabled", "", "checkbox");
        enabled.checked = valuesForForm.enabled;
        enabled.disabled = !runtime.platform_supported;
        const autoSync = input("lutris_auto_sync", "", "checkbox");
        autoSync.checked = valuesForForm.auto_sync;
        const removeUninstalled = input(
          "lutris_autosync_remove_uninstalled",
          "",
          "checkbox",
        );
        removeUninstalled.checked = valuesForForm.autosync_remove_uninstalled;
        const includeSteam = input("lutris_include_steam", "", "checkbox");
        includeSteam.checked = valuesForForm.include_steam;
        const exclusions = accessibleListEditor<LutrisExclusion>({
          label: "Excluded Lutris games",
          description: 'Enter a positive ID, or an object such as {"id":"123","name":"Game"}. Existing IDs missing from discovery remain in the list.',
          values: valuesForForm.exclude_games,
          formatValue: (value) => typeof value === "string" ? value : JSON.stringify(value),
          parseValue: (text) => {
            const source = text.trimStart().startsWith("{")
              ? `[${text}]`
              : JSON.stringify([text]);
            const parsed = parseLutrisExclusions(source);
            if (!parsed[0]) throw new Error("Each Lutris exclusion needs a positive ID or a name.");
            return parsed[0];
          },
        });
        activeLutrisExclusionEditor = exclusions;
        const exclusionsField = el("div", "field", exclusions.element);
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
        if (draftNotice) settingsSetLocalizedTextContent(saveStatus, draftNotice);
        const refreshStatus = el("div");
        const save = el("button", "button primary", "Save Lutris settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        let actionInProgress = false;
        const cancel = button(settingsTextMessage("Cancel"), () => {
          if (!actionInProgress) {
            pendingProviderDrafts.delete("lutris");
            updateDraftStatus();
            void loadLutris();
          }
        }, "secondary");
        const refresh = button(
          "Refresh now",
          () => runAction(refresh, refreshStatus, async () => {
            const updated = await refreshLutrisProvider(context.api);
            if (!runtimeStatus.isConnected || !lutrisLoads.isCurrent(loaded.generation)) return;
            runtimeStatus.className = updated.last_error ? "error" : "muted";
            settingsSetLocalizedTextContent(runtimeStatus, lutrisStatusDescription(updated));
            context.notify(settingsTextMessage("Lutris refresh requested."));
          }),
        );
        refresh.disabled =
          !context.api.allows("configuration.write") ||
          !runtime.platform_supported ||
          !valuesForForm.enabled;
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
          exclusionsField,
          runtimeStatus,
          saveStatus,
          el("div", "actions", save, cancel, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, saveStatus, async () => {
            actionInProgress = true;
            cancel.disabled = true;
            lutrisLoads.invalidate();
            let saveStarted = false;
            let requestedValues: LutrisSettings["values"] | null = null;
            try {
              const values = {
                enabled: enabled.checked,
                auto_sync: autoSync.checked,
                autosync_remove_uninstalled: removeUninstalled.checked,
                include_steam: includeSteam.checked,
                exclude_games: parseLutrisExclusions(JSON.stringify(exclusions.values())),
              };
              requestedValues = values;
              const existingPending = pendingProviderDrafts.get("lutris");
              if (existingPending?.overlaps.length && !existingPending.reviewed)
                throw new Error("Review overlapping fields before saving again.");
              pendingProviderDrafts.set("lutris", {
                baseline: settings.values as unknown as Record<string, unknown>,
                submitted: values as unknown as Record<string, unknown>,
                overlaps: [],
                reviewed: true,
              });
              updateDraftStatus();
              setSettingsFormDisabled(form, true);
              saveStarted = true;
              await saveLutrisSettings(context.api, settings, values);
              pendingProviderDrafts.delete("lutris");
              updateDraftStatus();
              await loadLutris();
              context.notify(settingsTextMessage("Lutris settings saved and confirmed. Scan status is shown below."));
            } catch (error) {
              if (saveStarted) {
                const conflict = error instanceof ApiError && error.code === "revision_conflict";
                const nextDraft = requestedValues ?? undefined;
                const notice = conflict
                  ? "Settings changed elsewhere. Your draft is preserved; review it before saving again."
                  : "Lutris save could not be confirmed. Your submitted draft is preserved for review against the latest saved values.";
                await loadLutris(nextDraft, notice);
                context.notify(settingsTextMessage(conflict
                  ? "Lutris settings changed elsewhere. Your draft is preserved for review."
                  : "Lutris save could not be confirmed. Review your preserved draft against the latest saved values before saving again."));
                return;
              }
              throw error;
            } finally {
              actionInProgress = false;
              if (form.isConnected) setSettingsFormDisabled(form, false);
              cancel.disabled = false;
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
        if (draftValues) {
          const baseline = drafts?.capture(lutrisArea);
          if (baseline) drafts?.markSaved(baseline);
          settingsSetDraftCheckbox(enabled, draftValues.enabled);
          settingsSetDraftCheckbox(autoSync, draftValues.auto_sync);
          settingsSetDraftCheckbox(removeUninstalled, draftValues.autosync_remove_uninstalled);
          settingsSetDraftCheckbox(includeSteam, draftValues.include_steam);
          exclusions.setValues(draftValues.exclude_games);
          configureProviderConflictReview("lutris", saveStatus, save, (values, fields) => {
            const selected = new Set(fields);
            if (selected.has("enabled")) settingsSetDraftCheckbox(enabled, values.enabled as boolean);
            if (selected.has("auto_sync")) settingsSetDraftCheckbox(autoSync, values.auto_sync as boolean);
            if (selected.has("autosync_remove_uninstalled")) settingsSetDraftCheckbox(removeUninstalled, values.autosync_remove_uninstalled as boolean);
            if (selected.has("include_steam")) settingsSetDraftCheckbox(includeSteam, values.include_steam as boolean);
            if (selected.has("exclude_games")) exclusions.setValues(values.exclude_games as LutrisSettings["values"]["exclude_games"]);
          });
        }
      } catch (error) {
        if (context.content.isConnected)
          lutrisArea.replaceChildren(errorPanel(error, () => { void loadLutris(draftValues, draftNotice); }));
      }
    }
    async function loadPlaynite(
      draftValues?: PlayniteSettings["values"],
      draftNotice?: string,
    ): Promise<void> {
      for (const editor of activePlayniteListEditors) editor.dispose();
      activePlayniteListEditors = [];
      playniteArea.replaceChildren(el("p", "muted", "Loading Playnite settings…"));
      try {
        const loaded = await playniteLoads.read(
          () => readPlayniteSettings(context.api),
          () => context.api.post<PlayniteStatus>("playnite/status"),
        );
        if (loaded === null) return;
        const { settings, runtime } = loaded;
        if (!context.content.isConnected) return;
        const pendingDraft = pendingProviderDrafts.get("playnite");
        if (pendingDraft) {
          const rebased = rebaseProviderDraft(pendingDraft, settings.values as unknown as Record<string, unknown>);
          pendingDraft.latest = settings.values as unknown as Record<string, unknown>;
          pendingDraft.merged = rebased.merged;
          pendingDraft.overlaps = rebased.overlaps;
          pendingDraft.reviewed = rebased.overlaps.length === 0;
          draftValues = rebased.merged as PlayniteSettings["values"];
          draftNotice = rebased.overlaps.length
            ? "Conflicting fields need review. Non-overlapping edits were rebased."
            : "Settings changed elsewhere. Non-overlapping edits were rebased; review before saving again.";
        }
        const valuesForForm = settings.values;
        const form = el("form", "settings-form");
        const enabled = input("playnite_enabled", "", "checkbox");
        enabled.checked = valuesForForm.enabled;
        const autoSync = input("playnite_auto_sync", "", "checkbox");
        autoSync.checked = valuesForForm.auto_sync;
        const syncAllInstalled = input(
          "playnite_sync_all_installed",
          "",
          "checkbox",
        );
        syncAllInstalled.checked = valuesForForm.sync_all_installed;
        const removeUninstalled = input(
          "playnite_autosync_remove_uninstalled",
          "",
          "checkbox",
        );
        removeUninstalled.checked = valuesForForm.autosync_remove_uninstalled;
        const deleteAfterDays = input(
          "playnite_autosync_delete_after_days",
          String(valuesForForm.autosync_delete_after_days),
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
        requireReplacement.checked = valuesForForm.autosync_require_replacement;
        const focusAttempts = input(
          "playnite_focus_attempts",
          String(valuesForForm.focus_attempts),
          "number",
        );
        focusAttempts.required = true;
        focusAttempts.min = "0";
        focusAttempts.max = "2147483647";
        focusAttempts.step = "1";
        const focusTimeoutSecs = input(
          "playnite_focus_timeout_secs",
          String(valuesForForm.focus_timeout_secs),
          "number",
        );
        focusTimeoutSecs.required = true;
        focusTimeoutSecs.min = "0";
        focusTimeoutSecs.max = "2147483647";
        focusTimeoutSecs.step = "1";
        const focusExitOnFirst = input("playnite_focus_exit_on_first", "", "checkbox");
        focusExitOnFirst.checked = valuesForForm.focus_exit_on_first;
        const fullscreenEntryEnabled = input("playnite_fullscreen_entry_enabled", "", "checkbox");
        fullscreenEntryEnabled.checked = valuesForForm.fullscreen_entry_enabled;
        const recentGames = input(
          "playnite_recent_games",
          String(valuesForForm.recent_games),
          "number",
        );
        recentGames.required = true;
        recentGames.min = "0";
        recentGames.max = "10000";
        recentGames.step = "1";
        const recentMaxAgeDays = input(
          "playnite_recent_max_age_days",
          String(valuesForForm.recent_max_age_days),
          "number",
        );
        recentMaxAgeDays.required = true;
        recentMaxAgeDays.min = "0";
        recentMaxAgeDays.max = "36500";
        recentMaxAgeDays.step = "1";
        const filterList = (
          name: string,
          label: string,
          values: string[],
        ): AccessibleListEditor<string> => {
          const control = accessibleListEditor<string>({
            label,
            description: "Add, edit, or remove list values. Unknown values are kept in their current order.",
            values,
            maxItems: 256,
            formatValue: (value) => value,
            parseValue: (value) => parsePlayniteFilterList(JSON.stringify([value]), label)[0]!,
          });
          control.element.setAttribute("data-provider-field", name);
          activePlayniteListEditors.push(control);
          return control;
        };
        const filterField = (editor: AccessibleListEditor<string>): HTMLElement => editor.element;
        const syncCategories = filterList(
          "playnite_sync_categories", "Included categories", valuesForForm.sync_categories,
        );
        const excludeCategories = filterList(
          "playnite_exclude_categories", "Excluded categories", valuesForForm.exclude_categories,
        );
        const syncPlugins = filterList(
          "playnite_sync_plugins", "Included plugin IDs", valuesForForm.sync_plugins,
        );
        const excludePlugins = filterList(
          "playnite_exclude_plugins", "Excluded plugin IDs", valuesForForm.exclude_plugins,
        );
        const excludeGames = filterList(
          "playnite_exclude_games", "Excluded game IDs", valuesForForm.exclude_games,
        );
        let currentRuntime = runtime;
        const runtimeStatus = el(
          "p",
          runtime.last_error ? "error" : "muted",
          runtime.revision !== settings.revision || runtime.enabled !== settings.values.enabled ||
          runtime.auto_sync !== settings.values.auto_sync
            ? "Saved Playnite settings are not yet reflected in runtime status. Refresh to check application."
            : playniteStatusDescription(runtime),
        );
        const saveStatus = el("div");
        if (draftNotice) settingsSetLocalizedTextContent(saveStatus, draftNotice);
        const refreshStatus = el("div");
        const save = el("button", "button primary", "Save Playnite settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        let actionInProgress = false;
        const cancel = button(settingsTextMessage("Cancel"), () => {
          if (!actionInProgress) {
            pendingProviderDrafts.delete("playnite");
            updateDraftStatus();
            void loadPlaynite();
          }
        }, "secondary");
        const refresh = button(
          "Refresh now",
          () => runAction(refresh, refreshStatus, async () => {
            const updated = await refreshPlayniteProvider(context.api);
            if (!runtimeStatus.isConnected) return;
            currentRuntime = updated;
            runtimeStatus.className = updated.last_error ? "error" : "muted";
            settingsSetLocalizedTextContent(runtimeStatus,
              updated.revision !== settings.revision || updated.enabled !== settings.values.enabled ||
              updated.auto_sync !== settings.values.auto_sync
                ? "Saved Playnite settings are not yet reflected in provider status. Refresh to check application."
                : playniteStatusDescription(updated));
            updateRefreshAvailability();
            context.notify(settingsTextMessage("Playnite refresh requested."));
          }),
        );
        const updateRefreshAvailability = (): void => {
          refresh.disabled =
            !context.api.allows("configuration.write") ||
            !enabled.checked ||
            !currentRuntime.playnite_running ||
            currentRuntime.snapshot_pending;
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
          settingsComponentField(
            "Included categories",
            filterField(syncCategories),
            "Additional installed games whose category matches any listed value; an empty list adds no category matches.",
          ),
          settingsComponentField(
            "Excluded categories",
            filterField(excludeCategories),
            "Games in these categories are excluded even if another setting would include them.",
          ),
          settingsComponentField(
            "Included plugin IDs",
            filterField(syncPlugins),
            "Additional installed games from any listed Playnite plugin; an empty list adds no plugin matches.",
          ),
          settingsComponentField(
            "Excluded plugin IDs",
            filterField(excludePlugins),
            "Games from these plugin IDs are excluded even if another setting would include them.",
          ),
          settingsComponentField(
            "Excluded game IDs",
            filterField(excludeGames),
            "Game IDs to omit from every import selection.",
          ),
          runtimeStatus,
          saveStatus,
          el("div", "actions", save, cancel, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, saveStatus, async () => {
            actionInProgress = true;
            cancel.disabled = true;
            playniteLoads.invalidate();
            let saveStarted = false;
            let requestedValues: PlayniteSettings["values"] | null = null;
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
                  JSON.stringify(syncCategories.values()),
                  "Sync categories",
                ),
                exclude_categories: parsePlayniteFilterList(
                  JSON.stringify(excludeCategories.values()),
                  "Excluded categories",
                ),
                sync_plugins: parsePlayniteFilterList(
                  JSON.stringify(syncPlugins.values()),
                  "Sync plugin IDs",
                ),
                exclude_plugins: parsePlayniteFilterList(
                  JSON.stringify(excludePlugins.values()),
                  "Excluded plugin IDs",
                ),
                exclude_games: parsePlayniteFilterList(
                  JSON.stringify(excludeGames.values()),
                  "Excluded game IDs",
                ),
              });
              requestedValues = values;
              const existingPending = pendingProviderDrafts.get("playnite");
              if (existingPending?.overlaps.length && !existingPending.reviewed)
                throw new Error("Review overlapping fields before saving again.");
              pendingProviderDrafts.set("playnite", {
                baseline: settings.values as unknown as Record<string, unknown>,
                submitted: values as unknown as Record<string, unknown>,
                overlaps: [],
                reviewed: true,
              });
              updateDraftStatus();
              setSettingsFormDisabled(form, true);
              saveStarted = true;
              await savePlayniteSettings(context.api, settings, values);
              pendingProviderDrafts.delete("playnite");
              updateDraftStatus();
              await loadPlaynite();
              context.notify(settingsTextMessage("Playnite settings saved and confirmed. Runtime status is shown below."));
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                await loadPlaynite(requestedValues ?? undefined,
                  "Settings changed elsewhere. Your draft is preserved; review it before saving again.");
                context.notify(settingsTextMessage("Playnite settings changed elsewhere. Your draft is preserved for review."));
                return;
              }
              if (saveStarted) {
                await loadPlaynite(requestedValues ?? undefined,
                  "Playnite save could not be confirmed. Your draft is preserved for review against the latest saved values.");
                context.notify(settingsTextMessage("Playnite save could not be confirmed. Review your preserved draft against the latest saved values before saving again."));
                return;
              }
              throw error;
            } finally {
              actionInProgress = false;
              if (form.isConnected) setSettingsFormDisabled(form, false);
              cancel.disabled = false;
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
        if (draftValues) {
          const baseline = drafts?.capture(playniteArea);
          if (baseline) drafts?.markSaved(baseline);
          settingsSetDraftCheckbox(enabled, draftValues.enabled);
          settingsSetDraftCheckbox(autoSync, draftValues.auto_sync);
          settingsSetDraftCheckbox(syncAllInstalled, draftValues.sync_all_installed);
          settingsSetDraftCheckbox(removeUninstalled, draftValues.autosync_remove_uninstalled);
          settingsSetDraftNumber(deleteAfterDays, draftValues.autosync_delete_after_days);
          settingsSetDraftCheckbox(requireReplacement, draftValues.autosync_require_replacement);
          settingsSetDraftNumber(focusAttempts, draftValues.focus_attempts);
          settingsSetDraftNumber(focusTimeoutSecs, draftValues.focus_timeout_secs);
          settingsSetDraftCheckbox(focusExitOnFirst, draftValues.focus_exit_on_first);
          settingsSetDraftCheckbox(fullscreenEntryEnabled, draftValues.fullscreen_entry_enabled);
          settingsSetDraftNumber(recentGames, draftValues.recent_games);
          settingsSetDraftNumber(recentMaxAgeDays, draftValues.recent_max_age_days);
          syncCategories.setValues(draftValues.sync_categories);
          excludeCategories.setValues(draftValues.exclude_categories);
          syncPlugins.setValues(draftValues.sync_plugins);
          excludePlugins.setValues(draftValues.exclude_plugins);
          excludeGames.setValues(draftValues.exclude_games);
          configureProviderConflictReview("playnite", saveStatus, save, (values, fields) => {
            const selected = new Set(fields);
            if (selected.has("enabled")) settingsSetDraftCheckbox(enabled, values.enabled as boolean);
            if (selected.has("auto_sync")) settingsSetDraftCheckbox(autoSync, values.auto_sync as boolean);
            if (selected.has("sync_all_installed")) settingsSetDraftCheckbox(syncAllInstalled, values.sync_all_installed as boolean);
            if (selected.has("autosync_remove_uninstalled")) settingsSetDraftCheckbox(removeUninstalled, values.autosync_remove_uninstalled as boolean);
            if (selected.has("autosync_delete_after_days")) settingsSetDraftNumber(deleteAfterDays, values.autosync_delete_after_days as number);
            if (selected.has("autosync_require_replacement")) settingsSetDraftCheckbox(requireReplacement, values.autosync_require_replacement as boolean);
            if (selected.has("focus_attempts")) settingsSetDraftNumber(focusAttempts, values.focus_attempts as number);
            if (selected.has("focus_timeout_secs")) settingsSetDraftNumber(focusTimeoutSecs, values.focus_timeout_secs as number);
            if (selected.has("focus_exit_on_first")) settingsSetDraftCheckbox(focusExitOnFirst, values.focus_exit_on_first as boolean);
            if (selected.has("fullscreen_entry_enabled")) settingsSetDraftCheckbox(fullscreenEntryEnabled, values.fullscreen_entry_enabled as boolean);
            if (selected.has("recent_games")) settingsSetDraftNumber(recentGames, values.recent_games as number);
            if (selected.has("recent_max_age_days")) settingsSetDraftNumber(recentMaxAgeDays, values.recent_max_age_days as number);
            if (selected.has("sync_categories")) syncCategories.setValues(values.sync_categories as string[]);
            if (selected.has("exclude_categories")) excludeCategories.setValues(values.exclude_categories as string[]);
            if (selected.has("sync_plugins")) syncPlugins.setValues(values.sync_plugins as string[]);
            if (selected.has("exclude_plugins")) excludePlugins.setValues(values.exclude_plugins as string[]);
            if (selected.has("exclude_games")) excludeGames.setValues(values.exclude_games as string[]);
          });
        }
      } catch (error) {
        if (context.content.isConnected)
          playniteArea.replaceChildren(errorPanel(error, () => { void loadPlaynite(draftValues, draftNotice); }));
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
        const motionAsDs4 = input("input_motion_as_ds4", "", "checkbox");
        motionAsDs4.checked = settings.values.motion_as_ds4;
        const touchpadAsDs4 = input("input_touchpad_as_ds4", "", "checkbox");
        touchpadAsDs4.checked = settings.values.touchpad_as_ds4;
        const protonDualsenseCompatibility = input("input_proton_dualsense_compatibility", "", "checkbox");
        protonDualsenseCompatibility.checked = settings.values.proton_dualsense_compatibility;
        const ds5InputtinoRandomizeMac = input("input_ds5_inputtino_randomize_mac", "", "checkbox");
        ds5InputtinoRandomizeMac.checked = settings.values.ds5_inputtino_randomize_mac;
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
        const keybindings = keybindingsControl(settings.values.keybindings, true);
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
          const option = el("option", "", settingsTextLiteral(choice.label));
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
            "Motion sensors select DS4 when automatic",
            motionAsDs4,
            "In automatic mode, selects the PlayStation controller profile when the client reports motion sensors.",
          ),
          field(
            "Touchpad selects DS4 when automatic",
            touchpadAsDs4,
            "In automatic mode, selects the PlayStation controller profile when the client reports a touchpad.",
          ),
          field(
            "Proton DualSense compatibility",
            protonDualsenseCompatibility,
            "For Proton launches, preserve Sony audio and device names unless the application launch already sets those variables.",
          ),
          field(
            "Randomize DS5 inputtino MAC",
            ds5InputtinoRandomizeMac,
            "On Linux, randomize the emulated DS5 controller MAC instead of deriving it from the controller slot.",
          ),
          field(
            "Native pen and touch policy",
            nativePenTouch,
            "On Linux, the companion can map one Touch contact to mouse emulation when this setting is enabled and the caller has the mouse grant. Native Linux Pen events and multitouch are unsupported; this does not create a native touch device. Windows native pen/touch support depends on backend capability.",
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
          keybindings.field,
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
          el("div", "actions", save, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
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
                  keybindings.read(),
                  motionAsDs4.checked,
                  touchpadAsDs4.checked,
                  protonDualsenseCompatibility.checked,
                  ds5InputtinoRandomizeMac.checked,
                ),
                ds4_back_as_touchpad_click: ds4BackAsTouchpadClick.checked,
              };
              saveStarted = true;
              const saved = await saveInputPolicySettings(context.api, settings, values);
              await loadInput();
              context.notify(saved.applied && !saved.cleanup_required
                ? settingsTextMessage("Input policy saved and confirmed. Runtime status is shown below.")
                : settingsTextMessage("Input policy saved and confirmed; releasing held native input is pending."));
            } catch (error) {
              if (saveStarted) {
                await loadInput();
                context.notify(error instanceof ApiError && error.code === "revision_conflict"
                  ? settingsTextMessage("Input policy changed elsewhere. Review the latest values before saving again.")
                  : settingsTextMessage("Input policy save could not be confirmed. Review the latest values before saving again."));
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
        settingsSetLocalizedTextContent(target,
          status.settings_ready && status.enabled !== settings.values.enabled
            ? "Saved Steam discovery setting is not yet reflected in provider status. Refresh to check application."
            : steamStatusDescription(status));
      } catch (error) {
        if (!target.isConnected || !steamLoads.isCurrent(generation)) return;
        settingsSetLocalizedTextContent(target,
          error instanceof ApiError && error.code === "forbidden"
            ? "Runtime status requires steam.read; settings remain available with configuration access."
            : "Current provider status is unavailable; settings remain editable.");
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
        const upnp = input("upnp", "", "checkbox");
        upnp.checked = settings.upnp;
        const fecPercentage = input(
          "fec_percentage",
          settings.configured_fec_percentage === null
            ? ""
            : String(settings.configured_fec_percentage),
          "number",
        );
        fecPercentage.min = "1";
        fecPercentage.max = "255";
        fecPercentage.step = "1";
        fecPercentage.placeholder = `Startup default (${settings.effective_fec_percentage}%)`;
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
          ["pipewire", "PipeWire portal selection (Linux)"],
          ["x11", "X11 / XWayland root readback (Linux)"],
          ["kms", "DRM/KMS active primary plane readback (Linux)"],
        ];
        for (const [value, label] of captureChoices) {
          const option = el("option", "", label);
          option.value = value;
        capture.append(option);
      }
      capture.value = settings.configured_capture;
      const displayPolicy = el("textarea");
      displayPolicy.name = "display_policy";
      displayPolicy.rows = 18;
      displayPolicy.spellcheck = false;
      displayPolicy.value = JSON.stringify({
        ...Object.fromEntries(displayPolicyKeys.map((key) => [key, null])),
        ...(settings.configured_display_policy ?? {}),
      }, null, 2);
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
                const option = el("option", "", settingsTextLiteral(`${output.device_name} (${output.id})`));
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
          const saved = el("option", "", settingsTextLiteral(`Saved output: ${settings.configured_output_name}`));
          saved.value = settings.configured_output_name;
          outputName.append(saved);
        }
        outputName.value = settings.configured_output_name ?? "";
        const streamAudio = input("stream_audio", "", "checkbox");
        streamAudio.checked = settings.stream_audio;
        const nvencPreset = input("nvenc_preset", String(settings.configured_nvenc_preset), "number");
        nvencPreset.min = "1";
        nvencPreset.max = "7";
        nvencPreset.step = "1";
        const nvencH264Cavlc = input("nvenc_h264_cavlc", "", "checkbox");
        nvencH264Cavlc.checked = settings.configured_nvenc_h264_cavlc;
        const nvencTwoPass = el("select");
        nvencTwoPass.name = "nvenc_twopass";
        for (const [value, label] of [["disabled", "Disabled"], ["quarter_res", "Quarter resolution"], ["full_res", "Full resolution"]] as const) {
          const option = el("option", "", label);
          option.value = value;
          nvencTwoPass.append(option);
        }
        nvencTwoPass.value = settings.configured_nvenc_twopass;
        const nvencVbvIncrease = input("nvenc_vbv_increase", String(settings.configured_nvenc_vbv_increase), "number");
        nvencVbvIncrease.min = "0";
        nvencVbvIncrease.max = "400";
        nvencVbvIncrease.step = "1";
        const nvencSplitEncode = el("select");
        nvencSplitEncode.name = "nvenc_split_encode";
        for (const [value, label] of [["auto", "Automatic"], ["enabled", "Enabled"], ["disabled", "Disabled"]] as const) {
          const option = el("option", "", label);
          option.value = value;
          nvencSplitEncode.append(option);
        }
        nvencSplitEncode.value = settings.configured_nvenc_split_encode;
        const nvencRealtimeHags = input("nvenc_realtime_hags", "", "checkbox");
        nvencRealtimeHags.checked = settings.configured_nvenc_realtime_hags;
        const nvencOpenGlVulkanOnDxgi = input("nvenc_opengl_vulkan_on_dxgi", "", "checkbox");
        nvencOpenGlVulkanOnDxgi.checked = settings.configured_nvenc_opengl_vulkan_on_dxgi;
        const nvencLatencyOverPower = input("nvenc_latency_over_power", "", "checkbox");
        nvencLatencyOverPower.checked = settings.configured_nvenc_latency_over_power;
        const vaapiStrictRcBuffer = input("vaapi_strict_rc_buffer", "", "checkbox");
        vaapiStrictRcBuffer.checked = settings.configured_vaapi_strict_rc_buffer;
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
        const audioEndpoints: Array<{ id: string; name: string; default_multimedia: boolean }> = [];
        if (context.api.allows("media.read")) {
          try {
            const discovered = await context.api.post<{
              endpoints: Array<{ id: string; name: string; default_multimedia: boolean }>;
            }>("media/audio_endpoints");
            if (!context.content.isConnected || !hostLoads.isCurrent(generation)) return;
            for (const endpoint of discovered.endpoints) {
              try {
                if (!endpoint.id || typeof endpoint.name !== "string" ||
                    typeof endpoint.default_multimedia !== "boolean" ||
                    parseAudioSinkSelection(endpoint.id) !== endpoint.id ||
                    audioEndpoints.some((current) => current.id === endpoint.id))
                  continue;
                audioEndpoints.push(endpoint);
                const option = el("option", "", settingsTextLiteral(`${endpoint.name}${endpoint.default_multimedia ? " (default)" : ""} (${endpoint.id})`));
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
          const saved = el("option", "", settingsTextLiteral(`Saved endpoint: ${settings.configured_audio_sink}`));
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
        const installSteamAudioDrivers = input("install_steam_audio_drivers", "", "checkbox");
        installSteamAudioDrivers.checked = settings.install_steam_audio_drivers;
        const steamAudioDriverStatus = el("p", "muted", "Steam Streaming Speakers driver status requires media.read.");
        if (context.api.allows("media.read")) {
          try {
            const driverStatus = parseSteamAudioDriverInstallStatus(
              await context.api.post<unknown>("media/audio_driver_status"),
            );
            if (!context.content.isConnected || !hostLoads.isCurrent(generation)) return;
            steamAudioDriverStatus.className = driverStatus.state === "error" ||
              driverStatus.state === "installed_with_route_restore_errors" ||
              (driverStatus.state === "installed_endpoint_unavailable" && driverStatus.restore_failures.length > 0)
              ? "error" : "muted";
            steamAudioDriverStatus.textContent = steamAudioDriverInstallStatusText(driverStatus);
          } catch {
            steamAudioDriverStatus.textContent = "Steam Streaming Speakers driver status could not be read.";
          }
        }
        const externalIp = input("external_ip", settings.configured_external_ip ?? "");
        externalIp.maxLength = 4096;
        const encryptionChoices: Array<[string, string]> = [
          ["0", "Disabled"],
          ["1", "Optional"],
          ["2", "Required"],
        ];
        const makeEncryptionSelect = (name: string, current: 0 | 1 | 2): HTMLSelectElement => {
          const control = el("select");
          control.name = name;
          for (const [value, label] of encryptionChoices) {
            const option = el("option", "", label);
            option.value = value;
            control.append(option);
          }
          control.value = String(current);
          return control;
        };
        const lanEncryptionMode = makeEncryptionSelect("lan_encryption_mode", settings.lan_encryption_mode);
        const wanEncryptionMode = makeEncryptionSelect("wan_encryption_mode", settings.wan_encryption_mode);
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
        const softwareEncoder = softwareEncoderOverrideControls({
          sw_preset: settings.configured_sw_preset,
          sw_tune: settings.configured_sw_tune,
          min_threads: settings.configured_min_threads,
          qp: settings.configured_qp,
          encoder: settings.configured_encoder,
        }, true);
        const softwareAvailability = el("p", "muted", "Software x264 availability has not been checked.");
        if (context.api.allows("media.read")) {
          try {
            const availability = parseSoftwareEncoderAvailability(await context.api.post<unknown>("media/encoders"));
            if (!context.content.isConnected || !hostLoads.isCurrent(generation)) return;
            softwareAvailability.textContent = availability;
          } catch {
            softwareAvailability.textContent = "Software x264 availability could not be checked. Explicit software launch requires a real available backend.";
          }
        }
        const status = el("div");
        const save = el("button", "button primary", "Save host settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          ...softwareEncoder.fields,
          softwareAvailability,
          el("p", "muted", settingsTextLiteral(`Effective software settings: preset ${JSON.stringify(settings.effective_sw_preset ?? "superfast")}, tune ${JSON.stringify(settings.effective_sw_tune ?? "zerolatency")}, minimum slice request ${settings.effective_min_threads ?? 2}; software requests max(client-negotiated slices, this value), subject to x264 frame-size and backend limits. Effective QP metadata: ${settings.effective_qp ?? 28}; current x264 ABR ignores it because Amaterasu has no VAAPI CQP consumer.`)),
          field("Name shown to Moonlight", hostname, "Leave blank to use this PC's name."),
          field("Moonlight HTTP port", port, "HTTPS uses the port five below this value."),
          field(
            "Global FEC percentage",
            fecPercentage,
            settingsTextLiteral(`Controls the repair percentage used for new streams unless an application or paired client overrides it. Leave blank to use the startup value (${settings.effective_fec_percentage}%). Valid values are 1–255.`),
          ),
          field(
            "Automatic router port mapping (UPnP)",
            upnp,
            "Requests gateway mappings for active stream listeners. The management listener is mapped only when its peer policy allows WAN access. This setting has no effect when the gateway does not support UPnP.",
          ),
          el(
            "p",
            settings.upnp_status === "error" ? "error" : "muted",
            settingsTextLiteral(`UPnP status: ${settings.upnp_status.replaceAll("_", " ")}${settings.upnp_error ? ` · ${settings.upnp_error}` : ""}`),
          ),
          field(
            "Address family",
            addressFamily,
            "Choose IPv4 only, both families, or retain the startup policy.",
          ),
          field(
            "Use startup bind address",
            useStartupBindAddress,
            settingsTextLiteral(`The effective bind address is ${settings.bind_address}. Turn this off to choose an explicit address; a blank value is distinct from startup policy.`),
          ),
          field(
            "Bind address override",
            bindAddress,
            "Use a literal IPv4 or IPv6 address. An explicit blank uses the startup policy's wildcard/loopback behavior.",
          ),
          field(
            "Capture backend",
            capture,
            settingsTextLiteral(`Automatic uses the startup backend. The current backend is ${settings.capture_provider ?? "unavailable"}; explicit providers fail if unavailable. X11 requires the host process's authorized DISPLAY and reads output pixels through the X server. DRM/KMS reads only an active primary plane and omits cursor/overlay planes and the CRTC color pipeline; it does not capture managed virtual displays. Save and reopen settings to list outputs for a new backend.`),
          ),
          markHostSettingsOwner("amaterasu.display", field(
            "Display and adapter policy (JSON)",
            displayPolicy,
            "Use the listed setting names and typed JSON values. This editor saves a full policy snapshot: omitted keys reset to the product default. Per-application and paired-client values may override stream-scoped settings. Native support remains platform/provider-specific and unsupported changes fail closed.",
          )),
          markHostSettingsOwner("amaterasu.display", field(
            "Capture output",
            outputName,
            "Choose a display for future Moonlight streams. Automatic retains the configured startup output.",
          )),
          field("Stream audio", streamAudio, "Enable host audio capture for future streams. Applications and paired clients may override this setting."),
          field("NVENC preset", nvencPreset, "Select NVENC preset P1 through P7 for future streams."),
          field("NVENC H.264 CAVLC", nvencH264Cavlc, "Request CAVLC entropy coding for NVENC H.264."),
          field("NVENC two-pass mode", nvencTwoPass, "Choose disabled, quarter-resolution, or full-resolution two-pass encoding."),
          field("NVENC VBV increase (%)", nvencVbvIncrease, "Increase NVENC VBV buffer size by 0–400 percent."),
          field("NVENC split encode", nvencSplitEncode, "Choose automatic driver selection or explicitly enable or disable split encoding."),
          field("NVENC real-time HAGS", nvencRealtimeHags, "Configure the NVENC real-time scheduling hint for systems using hardware-accelerated GPU scheduling."),
          field("NVENC OpenGL/Vulkan on DXGI", nvencOpenGlVulkanOnDxgi, "Configure NVENC handling for OpenGL and Vulkan capture through DXGI."),
          field("NVENC latency over power", nvencLatencyOverPower, "Prefer encoding latency over power efficiency."),
          field("VAAPI strict rate-control buffer", vaapiStrictRcBuffer, "Use strict rate-control buffer behavior for VAAPI."),
          field(
            "WGC pacing smoothing",
            wgcPacing,
            settingsTextLiteral(`Preserve the frame pacing grid after eligible Windows WGC capture misses. Current effective policy: ${settings.effective_wgc_pacing_smoothing ? "enabled" : "disabled"}. Constant-rate WGC currently lacks fresh-frame identity for this policy.`),
          ),
          field("Audio capture endpoint", audioSink, settingsTextLiteral(`Choose the output device used for loopback capture in future streams. Automatic uses the startup endpoint. Current effective endpoint: ${settings.effective_audio_sink}.`)),
          field("Set a virtual audio sink", virtualSinkEnabled, "Keep the selector unset to use the regular capture endpoint. An explicitly empty selector is preserved as distinct from unset; Amaterasu currently cannot perform Sunshine's host-audio fallback selection for it."),
          field("Virtual sink ID or name", virtualSink, "Match a Windows virtual output by ID, friendly name, device description, or adapter name. The selected virtual sink takes precedence over the regular endpoint for future streams."),
          field(
            "Global preparation commands (JSON)",
            globalPreparation,
            "Enter an ordered JSON array of up to 8 run commands with optional undo commands. Executables and working directories must be absolute native paths; blank input clears the list.",
          ),
          globalPreparationValidation,
          field("Capture selected audio endpoint without changing default outputs", audioSinkCaptureOnly, "For an explicitly selected Windows endpoint, keep the host's Console, Multimedia, and Communications default outputs. The setting applies to future streams."),
          field("Install Steam Streaming Speakers driver when missing", installSteamAudioDrivers, "Enabled by default on Windows. Amaterasu best-effort installs Steam's x64 INF from Common Files when the endpoint is missing. Installer failures do not block streaming. Default roles are restored only when they remain on Steam's endpoint. Disabling does not cancel a Windows installation already in progress; its endpoint check and route cleanup finish."),
          steamAudioDriverStatus,
          field("External IP", externalIp, "Stored for configuration parity. This host does not currently advertise or detect an external IP from this setting; blank resets it."),
          field("LAN video encryption", lanEncryptionMode, "Choose whether video-stream encryption is disabled, negotiated when supported, or required for LAN peers. Applies to new streams."),
          field("WAN video encryption", wanEncryptionMode, "Choose whether video-stream encryption is disabled, negotiated when supported, or required for WAN peers. Applies to new streams."),
          field(
            "Video batch size (KiB)",
            videoBatchSize,
            settingsTextLiteral(`Use 16, 32, or 64 KiB; blank or 0 selects 64 KiB. Current effective size: ${settings.effective_video_max_batch_size_kb} KiB${settings.video_batch_size_fallback ? " (configured value fell back to 64 KiB)" : ""}.`),
          ),
          field(
            "Maximum bitrate (kbps)",
            maxBitrate,
            settingsTextLiteral(`A positive value caps the host's initial stream bitrate. Blank resets to no host cap; zero or negative values also leave it uncapped. Caps below 100 kbps cannot launch with the current encoder. Current cap: ${settings.effective_max_bitrate_kbps === null ? "none" : `${settings.effective_max_bitrate_kbps} kbps`}.`),
          ),
          field(
            "Minimum frame rate (FPS)",
            minimumFpsTarget,
            settingsTextLiteral(`On idle capture, repeat the last accepted frame to target this rate. Zero uses the negotiated stream FPS; blank resets to 20 FPS. Current effective target: ${settings.effective_minimum_fps_target} FPS.`),
          ),
          field(
            "Stream ping timeout (ms)",
            pingTimeout,
            settingsTextLiteral(`Disconnect a stream after this much time without a ping. Zero expires immediately; blank resets to the default. Current effective timeout: ${settings.effective_ping_timeout_ms} ms.`),
          ),
          ...(outputDiscoveryIssue ? [el("p", "muted", outputDiscoveryIssue)] : []),
          ...(audioDiscoveryIssue ? [el("p", "muted", audioDiscoveryIssue)] : []),
          el(
            "p",
            "muted",
            settingsTextLiteral(`Active HTTP addresses: ${settings.http_addresses?.join(", ") || "off"} · HTTPS: ${settings.https_addresses?.join(", ") || "off"} · RTSP: ${settings.rtsp_addresses?.join(", ") || "off"}`),
          ),
          el("p", "muted", settingsTextLiteral(`Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}, RTSP ${settings.active_rtsp_port ?? "off"}`)),
          ...(settings.listener_error ? [el("p", "error", settings.listener_error)] : []),
          ...(settings.rtsp_listener_error ? [el("p", "error", settings.rtsp_listener_error)] : []),
          status,
          el("div", "actions", save, badge(settingsTextLiteral(`Revision ${settings.revision}`))),
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
              upnp: upnp.checked,
              fec_percentage: parseGlobalFecPercentage(fecPercentage.value),
              ...network,
              capture: parseCaptureSelection(capture.value),
              display_policy: parseDisplayPolicyOverrideMap(displayPolicy.value, "global"),
              output_name: parseOutputNameSelection(outputName.value),
              stream_audio: streamAudio.checked,
              nvenc_preset: nvencPreset.valueAsNumber,
              nvenc_h264_cavlc: nvencH264Cavlc.checked,
              nvenc_twopass: nvencTwoPass.value,
              nvenc_vbv_increase: nvencVbvIncrease.valueAsNumber,
              nvenc_split_encode: nvencSplitEncode.value,
              nvenc_realtime_hags: nvencRealtimeHags.checked,
              nvenc_opengl_vulkan_on_dxgi: nvencOpenGlVulkanOnDxgi.checked,
              nvenc_latency_over_power: nvencLatencyOverPower.checked,
              vaapi_strict_rc_buffer: vaapiStrictRcBuffer.checked,
              video_max_batch_size_kb: parseVideoMaxBatchSize(videoBatchSize.value),
              max_bitrate: parseMaxBitrate(maxBitrate.value),
              minimum_fps_target: parseMinimumFpsTarget(minimumFpsTarget.value),
              ping_timeout_ms: parsePingTimeout(pingTimeout.value),
              audio_sink: parseAudioSinkSelection(audioSink.value),
              virtual_sink: parseVirtualSinkSelection(virtualSinkEnabled.checked, virtualSink.value),
              audio_sink_capture_only: audioSinkCaptureOnly.checked,
              install_steam_audio_drivers: installSteamAudioDrivers.checked,
              external_ip: parseExternalIp(externalIp.value),
              lan_encryption_mode: parseEncryptionMode(lanEncryptionMode.value),
              wan_encryption_mode: parseEncryptionMode(wanEncryptionMode.value),
              wgc_pacing_smoothing: parseWgcPacingSmoothing(wgcPacing.value),
              ...softwareEncoder.read(),
              global_preparation: parsePreparationSteps(globalPreparation.value),
            };
            hostLoads.invalidate();
            try {
              await saveNvhttpHostSettings(context.api, settings, values);
              context.notify(settingsTextMessage("Host settings saved and read back. Future-stream options apply to new streams; check active listener status below."));
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
          hostArea.replaceChildren(
            panel("Moonlight host settings", form),
            renderNvhttpAudioSettings(context, settings, audioEndpoints, audioDiscoveryIssue, loadHost),
          );
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
          form.append(field(settingsTextLiteral(name), control, settingsTextLiteral(`Type: ${definition.type}`)));
        }
        const status = el("div");
        const save = el("button", "button primary", "Save settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          el("p", "muted", "Saving confirms stored values. Runtime use of registered settings is not verified on this page."),
          status,
          el("div", "actions", save, badge(settingsTextLiteral(`Revision ${document.revision}`))),
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
              context.notify(settingsTextMessage("Registered settings saved and read back. Runtime application is not verified here."));
              await load();
            } catch (error) {
              if (error instanceof ApiError && error.code === "revision_conflict") {
                await load();
                context.notify(settingsTextMessage("Registered settings changed elsewhere. Review the latest values and save again."));
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
    if (typeof Amaterasu.renderSessionExtensionSettings === "function")
      Amaterasu.renderSessionExtensionSettings(context, sessionExtensionsArea);
    void load();
    void loadAuth();
    void loadHostLogging();
    void loadHost();
    void loadPipewireChooser();
    void loadSteam();
    void loadLutris();
    void loadPlaynite();
    void loadInput();
    renderExtensionSlot(context, metricsArea, "settings", owners => {
        if (typeof Amaterasu.renderSessionExtensionSettings === "function")
          Amaterasu.renderSessionExtensionSettings(context, sessionExtensionsArea, {kind:"host"}, undefined, owners);
      });
    renderUpdaterSettings(context, updaterArea);
    renderSystemTraySettings(context, systemTrayArea);
    void loadPairing();
  }
}
