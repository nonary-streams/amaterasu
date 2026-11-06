namespace Amaterasu {
  function catalogTextLiteral(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function catalogTextMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function" ? Amaterasu.localizedMessage(value) : value;
  }

  function catalogLocalizedAttribute(
    target: Element,
    attribute: "aria-label" | "title" | "placeholder",
    source: string,
  ): void {
    if (typeof Amaterasu.setLocalizedAttribute === "function") {
      Amaterasu.setLocalizedAttribute(target, attribute, source);
    } else {
      target.setAttribute(attribute, source);
    }
  }

  function catalogLocalizedAttributeTemplate(
    target: Element,
    attribute: "aria-label" | "title" | "placeholder",
    source: string,
    values: Readonly<Record<string, TextValue>>,
  ): void {
    if (typeof Amaterasu.setLocalizedAttributeTemplate === "function") {
      Amaterasu.setLocalizedAttributeTemplate(target, attribute, source, values);
      return;
    }
    let rendered = source;
    for (const [name, value] of Object.entries(values)) {
      const literal = typeof value === "string" ? value : value.value;
      rendered = rendered.replaceAll(`{${name}}`, () => literal);
    }
    target.setAttribute(attribute, rendered);
  }

  interface CatalogApplicationDisplaySettings {
    "display-output"?: string;
    "virtual-screen"?: boolean;
    "virtual-display-mode"?: string;
    "virtual-display-layout"?: string;
  }

  type CatalogEntryWithDisplaySettings = Entry & CatalogApplicationDisplaySettings;

  export function catalogApplicationDisplayFormValues(entry: Entry | undefined): {
    display_output: string;
    virtual_screen: "" | "true" | "false";
    virtual_display_mode: "" | "disabled" | "per_client" | "shared";
    virtual_display_layout: "" | "exclusive" | "extended" | "extended_primary" | "extended_isolated" | "extended_primary_isolated";
  } {
    const settings = entry as CatalogEntryWithDisplaySettings | undefined;
    const displayOutput = settings?.["display-output"];
    if (displayOutput !== undefined && displayOutput !== null &&
        (typeof displayOutput !== "string" ||
          new TextEncoder().encode(displayOutput).length > 1_024 ||
          /[\u0000-\u001f\u007f-\u009f]/u.test(displayOutput)))
      throw new Error("Existing display output must contain at most 1024 UTF-8 bytes without control characters.");
    const virtualScreen = settings?.["virtual-screen"];
    if (virtualScreen !== undefined && virtualScreen !== null && typeof virtualScreen !== "boolean")
      throw new Error("Existing virtual screen setting must be a boolean.");
    const virtualDisplayMode = settings?.["virtual-display-mode"];
    if (virtualDisplayMode !== undefined && virtualDisplayMode !== null &&
        (typeof virtualDisplayMode !== "string" ||
          !["disabled", "per_client", "shared"].includes(virtualDisplayMode)))
      throw new Error("Existing virtual display mode must be disabled, per_client, or shared.");
    const virtualDisplayLayout = settings?.["virtual-display-layout"];
    if (virtualDisplayLayout !== undefined && virtualDisplayLayout !== null &&
        (typeof virtualDisplayLayout !== "string" ||
          !["exclusive", "extended", "extended_primary", "extended_isolated", "extended_primary_isolated"].includes(virtualDisplayLayout)))
      throw new Error("Existing virtual display layout must be a supported layout token.");
    return {
      display_output: displayOutput ?? "",
      virtual_screen: virtualScreen === undefined || virtualScreen === null
        ? ""
        : virtualScreen ? "true" : "false",
      virtual_display_mode: (virtualDisplayMode ?? "") as "" | "disabled" | "per_client" | "shared",
      virtual_display_layout: (virtualDisplayLayout ?? "") as "" | "exclusive" | "extended" | "extended_primary" | "extended_isolated" | "extended_primary_isolated",
    };
  }

  export function parseCatalogApplicationDisplaySettings(
    data: Pick<FormData, "get">,
  ): CatalogApplicationDisplaySettings {
    const displayOutput = String(data.get("display_output") ?? "").trim();
    if (new TextEncoder().encode(displayOutput).length > 1_024 ||
        /[\u0000-\u001f\u007f-\u009f]/u.test(displayOutput))
      throw new Error("Display output must contain at most 1024 UTF-8 bytes without control characters.");
    const virtualScreen = String(data.get("virtual_screen") ?? "");
    if (virtualScreen !== "" && virtualScreen !== "true" && virtualScreen !== "false")
      throw new Error("Virtual screen must inherit the host setting or be true or false.");
    const virtualDisplayMode = String(data.get("virtual_display_mode") ?? "");
    if (virtualDisplayMode !== "" &&
        !["disabled", "per_client", "shared"].includes(virtualDisplayMode))
      throw new Error("Virtual display mode must inherit or be disabled, per_client, or shared.");
    const virtualDisplayLayout = String(data.get("virtual_display_layout") ?? "");
    if (virtualDisplayLayout !== "" &&
        !["exclusive", "extended", "extended_primary", "extended_isolated", "extended_primary_isolated"].includes(virtualDisplayLayout))
      throw new Error("Virtual display layout must inherit or use a supported layout token.");
    return {
      ...(displayOutput ? { "display-output": displayOutput } : {}),
      ...(virtualScreen === "" ? {} : { "virtual-screen": virtualScreen === "true" }),
      ...(virtualDisplayMode === "" ? {} : { "virtual-display-mode": virtualDisplayMode }),
      ...(virtualDisplayLayout === "" ? {} : { "virtual-display-layout": virtualDisplayLayout }),
    };
  }

  async function fetchArtwork(api: Pick<Api, "post">, route: string, payload: Payload, active: () => boolean): Promise<Blob> {
    const controller = new AbortController();
    if (!active()) throw new Error("Artwork transfer cancelled.");
    const timer = window.setInterval(() => { if (!active()) controller.abort(); }, 100);
    try {
      const image = await api.post<Blob>(route, payload, true, controller.signal);
      if (!active()) throw new Error("Artwork transfer cancelled.");
      return image;
    } finally { window.clearInterval(timer); }
  }
  export async function readSteamArtwork(api: Pick<Api, "post">, reference: Extract<NonNullable<Entry["artwork"]>, { provider: "steam" }>, active: () => boolean = () => true): Promise<Blob> {
    if (!Number.isSafeInteger(reference.app_id) || reference.app_id < 1 || reference.app_id > 0xffff_ffff || !/^[0-9]{1,20}$/.test(reference.revision)) throw new Error("Invalid Steam artwork reference.");
    return fetchArtwork(api, "steam/artwork", { app_id: reference.app_id, revision: reference.revision }, active);
  }
  export async function readLutrisArtwork(api: Pick<Api, "post">, reference: Extract<NonNullable<Entry["artwork"]>, { provider: "lutris" }>, active: () => boolean = () => true): Promise<Blob> {
    if (!Number.isSafeInteger(reference.game_id) || reference.game_id < 1 || !/^[0-9a-f]{64}$/.test(reference.revision)) throw new Error("Invalid Lutris artwork reference.");
    return fetchArtwork(api, "lutris/artwork", { game_id: reference.game_id, revision: reference.revision }, active);
  }
  /** Only catalog identity crosses HTTP; provider routing stays on the host. */
  export async function readCatalogArtwork(api: Pick<Api, "post">, id: string, revision: string, active: () => boolean = () => true): Promise<Blob> {
    if (!/^app-[0-9a-f]{16}$/.test(id) || !revision || revision.length > 128) throw new Error("Invalid catalog artwork reference.");
    return fetchArtwork(api, "library/artwork", { id, revision }, active);
  }

  // Cards are assembled before insertion. Defer reads until rendering finishes,
  // and keep provider work bounded even when a library has many covers.
  const catalogArtworkQueue: Array<() => Promise<void>> = [];
  const catalogArtworkConcurrency = 4;
  let catalogArtworkRunning = 0;
  let catalogArtworkScheduled = false;

  function scheduleCatalogArtwork(read: () => Promise<void>): void {
    catalogArtworkQueue.push(read);
    if (catalogArtworkScheduled) return;
    catalogArtworkScheduled = true;
    queueMicrotask(drainCatalogArtworkQueue);
  }

  function drainCatalogArtworkQueue(): void {
    catalogArtworkScheduled = false;
    while (catalogArtworkRunning < catalogArtworkConcurrency && catalogArtworkQueue.length) {
      const read = catalogArtworkQueue.shift();
      if (!read) break;
      catalogArtworkRunning++;
      void read().finally(() => {
        catalogArtworkRunning--;
        drainCatalogArtworkQueue();
      });
    }
  }

  export function showCatalogArtwork(
    api: Pick<Api, "post" | "allows">,
    reference: Entry["artwork"],
    art: HTMLElement,
    content: HTMLElement,
    onLoad?: () => void,
    catalogId?: string,
  ): void {
    if (!reference) return;
    if (catalogId && !api.allows("catalog.read")) return;
    if (!catalogId && reference.provider === "steam" && !api.allows("steam.read")) return;
    if (!catalogId && reference.provider === "lutris" && !api.allows("lutris.read")) return;
    if (!catalogId && reference.provider === "external") return;
    const active = () => content.isConnected && art.isConnected;
    scheduleCatalogArtwork(async () => {
      if (!active()) return;
      try {
        const blob = await (catalogId
          ? readCatalogArtwork(api, catalogId, reference.provider === "external" ? reference.reference.revision : reference.revision, active)
          : reference.provider === "steam" ? readSteamArtwork(api, reference, active)
          : reference.provider === "lutris" ? readLutrisArtwork(api, reference, active)
          : Promise.reject(new Error("Catalog artwork identity is required.")));
        if (!active()) return;
        const url = URL.createObjectURL(blob);
        const image = el("img") as HTMLImageElement;
        image.alt = "";
        let released = false;
        const observer = new MutationObserver(() => {
          if (!active()) release();
        });
        const release = (): void => {
          if (released) return;
          released = true;
          observer.disconnect();
          URL.revokeObjectURL(url);
        };
        image.addEventListener("load", () => { release(); if (active()) onLoad?.(); }, { once: true });
        image.addEventListener("error", () => { image.remove(); release(); }, { once: true });
        observer.observe(document, { childList: true, subtree: true });
        image.src = url;
        art.prepend(image);
      } catch {
        // Keep initials when a provider image is missing or its revision changes.
      }
    });
  }

  function audioOverrideSelect(name: string, current: boolean | undefined): HTMLSelectElement {
    const select = el("select");
    select.name = name;
    const choices: Array<[string, string]> = [["", "Inherit"], ["true", "Enabled"], ["false", "Disabled"]];
    for (const [value, label] of choices) {
      const option = el("option", "", label);
      option.value = value;
      select.append(option);
    }
    select.value = current === undefined ? "" : String(current);
    return select;
  }

  export type CatalogNvencSettingsOverrides = Pick<
    NonNullable<Entry["config_overrides"]>,
    | "nvenc_preset"
    | "nvenc_h264_cavlc"
    | "nvenc_twopass"
    | "nvenc_vbv_increase"
    | "nvenc_split_encode"
    | "nvenc_realtime_hags"
    | "nvenc_opengl_vulkan_on_dxgi"
    | "nvenc_latency_over_power"
    | "vaapi_strict_rc_buffer"
  >;

  export function mergeCatalogConfigOverrides(
    saved: Entry["config_overrides"] | undefined,
    renderedFields: Iterable<string>,
    ...edited: Array<object | undefined>
  ): NonNullable<Entry["config_overrides"]> {
    const merged = Object.assign({}, saved, ...edited.filter((source) => source != null)) as Record<string, unknown>;
    for (const name of renderedFields) {
      if (!edited.some((source) => source != null && Object.hasOwn(source, name)))
        delete merged[name];
    }
    return merged as NonNullable<Entry["config_overrides"]>;
  }

  type CatalogNvencSettingsOverrideFormValues = Record<
    keyof CatalogNvencSettingsOverrides,
    string
  >;

  export function catalogNvencSettingsOverrideFormValues(
    overrides: Entry["config_overrides"] | undefined,
  ): CatalogNvencSettingsOverrideFormValues {
    const text = (value: boolean | number | string | null | undefined) =>
      value == null ? "" : String(value);
    return {
      nvenc_preset: text(overrides?.nvenc_preset),
      nvenc_h264_cavlc: text(overrides?.nvenc_h264_cavlc),
      nvenc_twopass: text(overrides?.nvenc_twopass),
      nvenc_vbv_increase: text(overrides?.nvenc_vbv_increase),
      nvenc_split_encode: text(overrides?.nvenc_split_encode),
      nvenc_realtime_hags: text(overrides?.nvenc_realtime_hags),
      nvenc_opengl_vulkan_on_dxgi: text(overrides?.nvenc_opengl_vulkan_on_dxgi),
      nvenc_latency_over_power: text(overrides?.nvenc_latency_over_power),
      vaapi_strict_rc_buffer: text(overrides?.vaapi_strict_rc_buffer),
    };
  }

  function catalogNvencSettingsOverrideFields(
    overrides: Entry["config_overrides"] | undefined,
  ): HTMLElement[] {
    const values = catalogNvencSettingsOverrideFormValues(overrides);
    const preset = input("nvenc_preset", values.nvenc_preset, "number");
    preset.min = "1";
    preset.max = "7";
    preset.step = "1";
    preset.placeholder = "Inherit";
    const cavlc = audioOverrideSelect(
      "nvenc_h264_cavlc",
      values.nvenc_h264_cavlc === "" ? undefined : values.nvenc_h264_cavlc === "true",
    );
    const twopass = catalogEnumOverrideSelect(
      "nvenc_twopass", values.nvenc_twopass, ["disabled", "quarter_res", "full_res"],
    );
    const vbvIncrease = input("nvenc_vbv_increase", values.nvenc_vbv_increase, "number");
    vbvIncrease.min = "0";
    vbvIncrease.max = "400";
    vbvIncrease.step = "1";
    vbvIncrease.placeholder = "Inherit";
    const splitEncode = catalogEnumOverrideSelect(
      "nvenc_split_encode", values.nvenc_split_encode, ["auto", "enabled", "disabled"],
    );
    const realtimeHags = audioOverrideSelect(
      "nvenc_realtime_hags",
      values.nvenc_realtime_hags === "" ? undefined : values.nvenc_realtime_hags === "true",
    );
    const openglVulkanOnDxgi = audioOverrideSelect(
      "nvenc_opengl_vulkan_on_dxgi",
      values.nvenc_opengl_vulkan_on_dxgi === "" ? undefined : values.nvenc_opengl_vulkan_on_dxgi === "true",
    );
    const latencyOverPower = audioOverrideSelect(
      "nvenc_latency_over_power",
      values.nvenc_latency_over_power === "" ? undefined : values.nvenc_latency_over_power === "true",
    );
    const vaapiStrictRcBuffer = audioOverrideSelect(
      "vaapi_strict_rc_buffer",
      values.vaapi_strict_rc_buffer === "" ? undefined : values.vaapi_strict_rc_buffer === "true",
    );
    return [
      field("NVENC preset", preset, "Inherit or choose P1 through P7."),
      field("NVENC H.264 CAVLC", cavlc, "Inherit the next scope or set CAVLC on or off."),
      field("NVENC two-pass mode", twopass, "Inherit or choose disabled, quarter resolution, or full resolution."),
      field("NVENC VBV increase (%)", vbvIncrease, "Inherit or set an integer from 0 to 400."),
      field("NVENC split encode", splitEncode, "Inherit or choose automatic, enabled, or disabled."),
      field("NVENC real-time HAGS", realtimeHags, "Inherit the next scope or set the real-time HAGS option."),
      field("NVENC OpenGL/Vulkan on DXGI", openglVulkanOnDxgi, "Inherit the next scope or set this DXGI capture option."),
      field("NVENC latency over power", latencyOverPower, "Inherit the next scope or prefer latency over power."),
      field("VAAPI strict rate-control buffer", vaapiStrictRcBuffer, "Inherit the next scope or set strict rate-control buffer behavior."),
    ];
  }

  export function parseCatalogNvencSettingsOverrides(
    data: FormData,
    current?: Entry["config_overrides"],
  ): CatalogNvencSettingsOverrides {
    const text = (name: keyof CatalogNvencSettingsOverrides) => String(data.get(name) ?? "").trim();
    const integer = (name: "nvenc_preset" | "nvenc_vbv_increase", minimum: number, maximum: number): number | null => {
      const value = text(name);
      if (value === "") return null;
      if (!/^[0-9]+$/.test(value))
        throw new Error(`${name} must be an integer from ${minimum} to ${maximum}, or blank to inherit.`);
      const number = Number(value);
      if (!Number.isSafeInteger(number) || number < minimum || number > maximum)
        throw new Error(`${name} must be an integer from ${minimum} to ${maximum}, or blank to inherit.`);
      return number;
    };
    const boolean = (name: "nvenc_h264_cavlc" | "nvenc_realtime_hags" | "nvenc_opengl_vulkan_on_dxgi" | "nvenc_latency_over_power" | "vaapi_strict_rc_buffer"): boolean | null => {
      const value = text(name);
      if (value === "") return null;
      if (value === "true") return true;
      if (value === "false") return false;
      throw new Error(`${name} must be enabled, disabled, or inherited.`);
    };
    const enumeration = <T extends string>(name: "nvenc_twopass" | "nvenc_split_encode", choices: readonly T[]): T | null => {
      const value = text(name);
      if (value === "") return null;
      if ((choices as readonly string[]).includes(value) || value === current?.[name]) return value as T;
      throw new Error(`${name} has an unsupported value.`);
    };
    return {
      nvenc_preset: integer("nvenc_preset", 1, 7),
      nvenc_h264_cavlc: boolean("nvenc_h264_cavlc"),
      nvenc_twopass: enumeration<NvencTwoPass>("nvenc_twopass", ["disabled", "quarter_res", "full_res"]),
      nvenc_vbv_increase: integer("nvenc_vbv_increase", 0, 400),
      nvenc_split_encode: enumeration<NvencSplitEncode>("nvenc_split_encode", ["auto", "enabled", "disabled"]),
      nvenc_realtime_hags: boolean("nvenc_realtime_hags"),
      nvenc_opengl_vulkan_on_dxgi: boolean("nvenc_opengl_vulkan_on_dxgi"),
      nvenc_latency_over_power: boolean("nvenc_latency_over_power"),
      vaapi_strict_rc_buffer: boolean("vaapi_strict_rc_buffer"),
    };
  }

  const refreshRateOptionValues = ["auto", "disabled", "manual", "prefer_highest"] as const;
  const catalogFrameGenerationProviders = [
    "lossless-scaling",
    "game-provided",
    "nvidia-smooth-motion",
  ] as const satisfies readonly CatalogFrameGenerationProvider[];
  const catalogFrameGenerationModes = [
    "off",
    ...catalogFrameGenerationProviders,
  ] as const satisfies readonly CatalogFrameGenerationMode[];

  function catalogSavedUnsupportedEnumValueToken(values: readonly string[]): string {
    let token = "__amaterasu_keep_saved_unsupported_enum_value__";
    while (values.includes(token)) token += "_";
    return token;
  }

  function catalogEnumOverrideSelect(
    name: string,
    current: string | null | undefined,
    values: readonly string[],
  ): HTMLSelectElement {
    const select = el("select");
    select.name = name;
    const inherit = el("option", "", "Inherit");
    inherit.value = "";
    select.append(inherit);
    for (const value of values) {
      const option = el("option", "", catalogTextLiteral(value));
      option.value = value;
      select.append(option);
    }
    if (current != null && !values.includes(current)) {
      const saved = el("option", "", catalogTextLiteral(`Saved value: ${current}`));
      saved.value = current;
      select.append(saved);
    }
    select.value = current ?? "";
    return select;
  }

  export type CatalogSessionPolicyOverrides = Pick<
    NonNullable<Entry["config_overrides"]>,
    | "lossless_scaling_framegen"
    | "lossless_scaling_target_fps"
    | "frame_generation_provider"
    | "frame_generation_mode"
    | "ds4_back_as_touchpad_click"
  >;

  type CatalogSessionPolicyOverrideControlValues = Record<
    keyof CatalogSessionPolicyOverrides,
    string
  >;

  export function catalogSessionPolicyOverrideFormValues(
    overrides: Entry["config_overrides"] | undefined,
  ): CatalogSessionPolicyOverrideControlValues {
    const text = (value: boolean | number | string | null | undefined) =>
      value == null ? "" : String(value);
    return {
      lossless_scaling_framegen: text(overrides?.lossless_scaling_framegen),
      lossless_scaling_target_fps: text(overrides?.lossless_scaling_target_fps),
      frame_generation_provider: text(overrides?.frame_generation_provider),
      frame_generation_mode: text(overrides?.frame_generation_mode),
      ds4_back_as_touchpad_click: text(overrides?.ds4_back_as_touchpad_click),
    };
  }

  function catalogSessionPolicyOverrideFields(
    overrides: Entry["config_overrides"] | undefined,
    _capabilities: CatalogConfigOverrideCapabilities,
  ): HTMLElement[] {
    const values = catalogSessionPolicyOverrideFormValues(overrides);
    const losslessScalingFramegen = audioOverrideSelect(
      "lossless_scaling_framegen",
      values.lossless_scaling_framegen === ""
        ? undefined
        : values.lossless_scaling_framegen === "true",
    );
    const frameGenerationProvider = catalogEnumOverrideSelect(
      "frame_generation_provider",
      values.frame_generation_provider,
      catalogFrameGenerationProviders,
    );
    const frameGenerationMode = catalogEnumOverrideSelect(
      "frame_generation_mode",
      values.frame_generation_mode,
      catalogFrameGenerationModes,
    );
    const losslessScalingTargetFps = input(
      "lossless_scaling_target_fps",
      values.lossless_scaling_target_fps,
      "number",
    );
    losslessScalingTargetFps.min = "1";
    losslessScalingTargetFps.max = "1000";
    losslessScalingTargetFps.step = "1";
    losslessScalingTargetFps.placeholder = "Stream FPS";
    const ds4BackAsTouchpadClick = audioOverrideSelect(
      "ds4_back_as_touchpad_click",
      values.ds4_back_as_touchpad_click === ""
        ? undefined
        : values.ds4_back_as_touchpad_click === "true",
    );
    return [
      field(
        "Lossless Scaling frame generation",
        losslessScalingFramegen,
        "Enable Lossless Scaling when its provider is active. Blank keeps the mode unresolved unless another setting supplies it.",
      ),
      field(
        "Frame generation provider",
        frameGenerationProvider,
        "Blank inherits. Select the active game's frame generation provider; Lossless Scaling also requires its framegen toggle.",
      ),
      field(
        "Frame generation mode",
        frameGenerationMode,
        "Blank inherits. An explicit mode takes precedence over provider and Lossless Scaling settings.",
      ),
      field(
        "Lossless Scaling target FPS",
        losslessScalingTargetFps,
        "Blank uses the active stream FPS when Lossless Scaling mode is selected; explicit targets must be 1–1000 FPS.",
      ),
      field(
        "DS4 Back as touchpad click",
        ds4BackAsTouchpadClick,
        "Only applies to a DS4 profile. Blank inherits; explicit false is preserved.",
      ),
    ];
  }

  export type CatalogInputCompatibilityOverrides = Pick<
    NonNullable<Entry["config_overrides"]>,
    | "motion_as_ds4"
    | "touchpad_as_ds4"
    | "ds5_inputtino_randomize_mac"
    | "proton_dualsense_compatibility"
  >;

  export function catalogInputCompatibilityOverrideFields(
    overrides: Entry["config_overrides"] | undefined,
  ): HTMLElement[] {
    const motion = audioOverrideSelect("motion_as_ds4", overrides?.motion_as_ds4 ?? undefined);
    const touchpad = audioOverrideSelect("touchpad_as_ds4", overrides?.touchpad_as_ds4 ?? undefined);
    const randomMac = audioOverrideSelect(
      "ds5_inputtino_randomize_mac",
      overrides?.ds5_inputtino_randomize_mac ?? undefined,
    );
    const proton = audioOverrideSelect(
      "proton_dualsense_compatibility",
      overrides?.proton_dualsense_compatibility ?? undefined,
    );
    return [
      field("Motion sensors select DS4", motion, "Inherit the next scope or override DS4 selection for automatic controller profiles."),
      field("Touchpad selects DS4", touchpad, "Inherit the next scope or override DS4 selection for automatic controller profiles."),
      field("Randomize DS5 inputtino MAC", randomMac, "Linux only; inherit or override random versus slot-derived DS5 MAC addresses."),
      field("Proton DualSense compatibility", proton, "Linux Proton launch environment; inherit the next scope or override."),
    ];
  }

  export function parseCatalogInputCompatibilityOverrides(
    data: FormData,
  ): CatalogInputCompatibilityOverrides {
    const text = (name: keyof CatalogInputCompatibilityOverrides) =>
      String(data.get(name) ?? "");
    return {
      motion_as_ds4: Amaterasu.parseInputBooleanOverride(
        text("motion_as_ds4"), "Motion selects DS4",
      ),
      touchpad_as_ds4: Amaterasu.parseInputBooleanOverride(
        text("touchpad_as_ds4"), "Touchpad selects DS4",
      ),
      ds5_inputtino_randomize_mac: Amaterasu.parseInputBooleanOverride(
        text("ds5_inputtino_randomize_mac"), "DS5 random MAC",
      ),
      proton_dualsense_compatibility: Amaterasu.parseInputBooleanOverride(
        text("proton_dualsense_compatibility"), "Proton DualSense compatibility",
      ),
    };
  }

  function parseCatalogSessionPolicyEnum<T extends string>(
    value: string,
    allowedValues: readonly T[],
    currentValue: string | null | undefined,
    label: string,
  ): T | null {
    if (value === "") return null;
    if (
      currentValue != null &&
      !allowedValues.includes(currentValue as T) &&
      (value === currentValue || value === catalogSavedUnsupportedEnumValueToken(allowedValues))
    ) return currentValue as T;
    if (!allowedValues.includes(value as T))
      throw new Error(`${label} must be one of the supported values.`);
    return value as T;
  }

  function parseCatalogLosslessScalingTargetFps(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (!/^[0-9]+$/.test(trimmed))
      throw new Error("Lossless Scaling target FPS must be an integer from 1 to 1000.");
    const targetFps = Number(trimmed);
    if (!Number.isSafeInteger(targetFps) || targetFps < 1 || targetFps > 1000)
      throw new Error("Lossless Scaling target FPS must be an integer from 1 to 1000.");
    return targetFps;
  }

  export function parseCatalogSessionPolicyOverrides(
    data: FormData,
    _capabilities: CatalogConfigOverrideCapabilities,
    currentOverrides?: Entry["config_overrides"],
  ): CatalogSessionPolicyOverrides {
    const text = (name: keyof CatalogSessionPolicyOverrides) =>
      String(data.get(name) ?? "");
    return {
      lossless_scaling_framegen: Amaterasu.parseInputBooleanOverride(
        text("lossless_scaling_framegen"),
        "Lossless Scaling frame generation",
      ),
      lossless_scaling_target_fps: parseCatalogLosslessScalingTargetFps(
        text("lossless_scaling_target_fps"),
      ),
      frame_generation_provider: parseCatalogSessionPolicyEnum(
        text("frame_generation_provider"),
        catalogFrameGenerationProviders,
        currentOverrides?.frame_generation_provider,
        "Frame generation provider",
      ),
      frame_generation_mode: parseCatalogSessionPolicyEnum(
        text("frame_generation_mode"),
        catalogFrameGenerationModes,
        currentOverrides?.frame_generation_mode,
        "Frame generation mode",
      ),
      ds4_back_as_touchpad_click: Amaterasu.parseInputBooleanOverride(
        text("ds4_back_as_touchpad_click"),
        "DS4 Back as touchpad click",
      ),
    };
  }

  export function catalogRefreshRateOptionText(
    value: RefreshRateOption | null | undefined,
  ): string {
    return value ?? "";
  }

  export function parseCatalogRefreshRateOptionOverride(data: FormData): RefreshRateOption | null {
    return Amaterasu.parseRefreshRateOptionOverride(
      String(data.get("dd_refresh_rate_option") ?? ""),
    );
  }

  export function catalogManualRefreshRateCompanionVisible(
    value: string | null | undefined,
  ): boolean {
    return Amaterasu.usesManualRefreshRateCompanion(value);
  }

  export function catalogPrefer10BitSdrOverrideValue(value: unknown): boolean | null {
    if (value == null) return null;
    if (typeof value !== "boolean")
      throw new Error("Saved 10-bit SDR preference must be a boolean or null.");
    return value;
  }

  function catalogRefreshRateOptionFields(
    current: RefreshRateOption | null | undefined,
  ): HTMLElement[] {
    const option = catalogEnumOverrideSelect(
      "dd_refresh_rate_option",
      catalogRefreshRateOptionText(current),
      refreshRateOptionValues,
    );
    const manualCompanionNotice = el(
      "p",
      "muted",
      "Manual mode uses the separate dd_manual_refresh_rate setting.",
    );
    const updateManualCompanionNotice = () => {
      manualCompanionNotice.hidden = !catalogManualRefreshRateCompanionVisible(option.value);
    };
    option.addEventListener("change", updateManualCompanionNotice);
    updateManualCompanionNotice();
    return [
      field(
        "Refresh rate option",
        option,
        "Blank inherits the next scope. Choose auto, disabled, manual, or prefer_highest.",
      ),
      manualCompanionNotice,
    ];
  }

  function inputGamepadOverrideSelect(
    name: string,
    current: string | undefined,
    status: InputRuntimeStatus,
  ): HTMLSelectElement {
    const select = el("select");
    select.name = name;
    const inherit = el("option", "", "Inherit application/host");
    inherit.value = "";
    select.append(inherit);
    for (const choice of inputGamepadProfileChoices(
      current ?? "",
      status.supported_profiles,
      status.recognized_profiles,
    )) {
      const option = el("option", "", catalogTextLiteral(choice.label));
      option.value = choice.value;
      select.append(option);
    }
    select.value = current ?? "";
    return select;
  }

  function audioSinkOverrideControls(current: string | undefined): {
    mode: HTMLSelectElement;
    id: HTMLInputElement;
    controls: HTMLElement;
  } {
    const mode = el("select");
    mode.name = "audio_sink_mode";
    for (const [value, label] of [
      ["inherit", "Inherit host endpoint"],
      ["automatic", "Automatic/default endpoint"],
      ["specific", "Specific endpoint ID"],
    ]) {
      const option = el("option", "", label);
      option.value = value ?? "";
      mode.append(option);
    }
    mode.value = current === undefined ? "inherit" : current === "" ? "automatic" : "specific";
    const id = input("audio_sink_id", current ?? "");
    id.maxLength = 4096;
    const update = () => { id.disabled = mode.value !== "specific"; };
    mode.addEventListener("change", update);
    update();
    return {
      mode,
      id,
      controls: el("div", "form-grid",
        field("Audio capture endpoint", mode, "Inherit the host, choose automatic, or use an exact endpoint ID."),
        field("Specific endpoint ID", id, "Used only when a specific endpoint is selected."),
      ),
    };
  }

  function virtualSinkOverrideControls(current: string | undefined): {
    mode: HTMLSelectElement;
    selector: HTMLInputElement;
    controls: HTMLElement;
  } {
    const mode = el("select");
    mode.name = "virtual_sink_mode";
    for (const [value, label] of [
      ["inherit", "Inherit application/host"],
      ["automatic", "Clear inherited virtual sink"],
      ["specific", "Specific virtual sink selector"],
    ]) {
      const option = el("option", "", label);
      option.value = value ?? "";
      mode.append(option);
    }
    mode.value = current === undefined ? "inherit" : current === "" ? "automatic" : "specific";
    const selector = input("virtual_sink_selector", current ?? "");
    selector.maxLength = 4096;
    catalogLocalizedAttribute(selector, "placeholder", "Exact ID or device name");
    const update = () => { selector.disabled = mode.value !== "specific"; };
    mode.addEventListener("change", update);
    update();
    return {
      mode,
      selector,
      controls: el("div", "form-grid",
        field("Virtual audio sink", mode, "Inherit the next scope, clear inherited selection, or choose a virtual device."),
        field("Virtual sink ID or name", selector, "Used for a specific device; Windows matches IDs and names exactly."),
      ),
    };
  }

  type LibrarySort = "name" | "name-desc" | "source";
  interface LibraryIndexRow { id: string; name: string; source: string; }
  interface LibraryIndex {
    revision: number;
    capabilities: CatalogConfigOverrideCapabilities;
    rows: LibraryIndexRow[];
    cachedRows: Map<string, CatalogRow>;
  }

  function librarySource(entry: Entry): string {
    if (entry.launch.kind === "playnite" || entry.launch.kind === "playnite_fullscreen") return "Playnite";
    if (entry.steam_id || entry.artwork?.provider === "steam") return "Steam";
    if (entry.artwork?.provider === "lutris") return "Lutris";
    if (entry.artwork?.provider === "external") return entry.artwork.source;
    return "Other";
  }

  function sortLibraryRows(rows: LibraryIndexRow[], sort: LibrarySort): LibraryIndexRow[] {
    let locale = typeof document !== "undefined" ? document.documentElement?.lang : "";
    locale = locale?.replaceAll("_", "-") ?? "";
    let collator: Intl.Collator;
    try { collator = new Intl.Collator(locale || undefined, { numeric: true, sensitivity: "base" }); }
    catch { collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" }); }
    return [...rows].sort((left, right) => {
      const name = collator.compare(left.name, right.name);
      if (sort === "source") return collator.compare(left.source, right.source) || name || left.id.localeCompare(right.id);
      return (sort === "name-desc" ? -name : name) || left.id.localeCompare(right.id);
    });
  }

  export function libraryLocation(address: URL): { query: string; sort: LibrarySort } {
    const fragment = address.hash.slice(1);
    const parameters = new URLSearchParams(fragment.startsWith("catalog?") ? fragment.slice(8) : "");
    const requestedSort = parameters.get("sort");
    return {
      query: (parameters.get("q") ?? "").trim(),
      sort: requestedSort === "name-desc" || requestedSort === "source" ? requestedSort : "name",
    };
  }

  export function updateLibraryLocation(address: URL, query: string, sort: LibrarySort): URL {
    const next = new URL(address);
    const parameters = new URLSearchParams(next.hash.startsWith("#catalog?") ? next.hash.slice(9) : "");
    if (query) parameters.set("q", query);
    else parameters.delete("q");
    if (sort !== "name") parameters.set("sort", sort);
    else parameters.delete("sort");
    const suffix = parameters.toString();
    next.hash = suffix ? `catalog?${suffix}` : "catalog";
    return next;
  }

  async function scanLibraryIndex(
    api: Api, query: string, active: () => boolean, progress: (count: number) => void,
  ): Promise<LibraryIndex> {
    const rows: LibraryIndexRow[] = [];
    const cachedRows = new Map<string, CatalogRow>();
    const seenIds = new Set<string>();
    const seenCursors = new Set<string>();
    let cursor: unknown = null;
    let revision: number | null = null;
    let capabilities: CatalogConfigOverrideCapabilities | null = null;
    do {
      if (!active()) throw new Error("Library scan cancelled.");
      const reply: CatalogReply = await api.post(query ? "catalog/search" : "catalog/list", {
        ...(query ? { query } : {}), cursor, limit: 32,
      });
      if (!active()) throw new Error("Library scan cancelled.");
      if (revision !== null && reply.revision !== revision) throw new Error("The catalog changed while sorting. Refresh the library to try again.");
      revision = reply.revision;
      capabilities = reply.config_override_capabilities;
      for (const row of reply.entries) {
        if (seenIds.has(row.id) || rows.length >= 100_000) throw new Error("The catalog returned an invalid or oversized library.");
        seenIds.add(row.id);
        rows.push({id: row.id, name: row.entry.name, source: librarySource(row.entry)});
        if (cachedRows.size < 32) cachedRows.set(row.id, row);
      }
      progress(rows.length);
      cursor = reply.next_cursor;
      if (cursor !== null) {
        const marker = JSON.stringify(cursor);
        if (seenCursors.has(marker)) throw new Error("The catalog returned a repeated continuation.");
        seenCursors.add(marker);
      }
    } while (cursor !== null);
    if (revision === null || capabilities === null) throw new Error("The catalog returned no library revision.");
    return {revision, capabilities, rows, cachedRows};
  }

  export function renderCatalog(context: PageContext): void {
    const { content, api, notify } = context;
    const search = input("search");
    search.type = "search";
    catalogLocalizedAttribute(search, "placeholder", "Search names, sources, commands and tags");
    catalogLocalizedAttribute(search, "aria-label", "Search catalog");
    const searchForm = el("form", "search-form", search);
    const searchButton = el("button", "button secondary", "Search");
    searchButton.type = "submit";
    searchForm.append(searchButton);
    const results = el("div");
    results.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && selectedIds.size) {
        event.preventDefault();
        clearSelection();
      }
    });
    const editor = el("div");
    const editorDrafts = context.registerDraftGuard ? trackDrafts(editor) : null;
    let editorSaving = false;
    let editorLifecycle: AbortController | null = null;
    const catalogDraftDirty = () => {
      const form = editor.querySelector<HTMLElement>(".editor-form");
      return !!form && !!editorDrafts?.isDirty(form);
    };
    let pluginArea: HTMLElement | null = null;
    const pluginDirty = () => !!pluginArea && typeof applicationPluginOverridesDirty === "function" && applicationPluginOverridesDirty(pluginArea);
    if (editorDrafts) context.registerDraftGuard?.(
      () => editorSaving || catalogDraftDirty() || pluginDirty(),
      () => {
        editorLifecycle?.abort();
        editorLifecycle = null;
        editorDrafts.dispose();
      },
    );
    let editorGeneration = 0;
    const status = el("div");
    const selectionCount = el("span", "muted");
    const selectionBar = el("div", "library-selection");
    const selectedIds = new Set<string>();
    const visibleSelection = new Map<string, () => void>();
    let selectionAnchor = "";
    let selectedRevision: number | null = null;
    let visibleIds: string[] = [];
    const clearSelectionButton = button("Clear selection", () => clearSelection(), "quiet");
    selectionBar.setAttribute("role", "status");
    selectionBar.hidden = true;
    selectionBar.append(selectionCount, el("span", "muted", "Space selects · Escape clears"), clearSelectionButton);
    function updateSelection(): void {
      selectionBar.hidden = selectedIds.size === 0;
      selectionCount.textContent = `${selectedIds.size} selected`;
      for (const update of visibleSelection.values()) update();
    }
    function clearSelection(): void {
      selectedIds.clear();
      selectedRevision = null;
      selectionAnchor = "";
      updateSelection();
    }
    function toggleSelection(id: string): void {
      if (selectedIds.has(id)) selectedIds.delete(id);
      else selectedIds.add(id);
      selectedRevision = selectedIds.size ? revision : null;
      selectionAnchor = id;
      updateSelection();
    }
    function selectRange(from: string, to: string): void {
      const first = visibleIds.indexOf(selectionAnchor || from);
      const last = visibleIds.indexOf(to);
      if (first < 0 || last < 0) return;
      for (let index = Math.min(first, last); index <= Math.max(first, last); index++) {
        const id = visibleIds[index];
        if (id) selectedIds.add(id);
      }
      if (!selectionAnchor) selectionAnchor = from;
      selectedRevision = revision;
      updateSelection();
    }
    function moveCollectionFocus(current: HTMLButtonElement, key: string, extend: boolean): void {
      if (!currentGrid) return;
      const start = current.getBoundingClientRect();
      const centerX = start.left + start.width / 2;
      const centerY = start.top + start.height / 2;
      const candidates = Array.from(currentGrid.querySelectorAll<HTMLButtonElement>("button.catalog-open:not(:disabled)"))
        .filter((candidate) => candidate !== current)
        .map((candidate) => {
          const rect = candidate.getBoundingClientRect();
          const x = rect.left + rect.width / 2 - centerX;
          const y = rect.top + rect.height / 2 - centerY;
          const horizontal = key === "ArrowLeft" || key === "ArrowRight";
          const primary = horizontal ? x : y;
          const secondary = horizontal ? y : x;
          const forward = (key === "ArrowRight" || key === "ArrowDown") ? primary > 1 : primary < -1;
          return { candidate, forward, distance: Math.abs(primary) + Math.abs(secondary) * 4 };
        })
        .filter((candidate) => candidate.forward)
        .sort((a, b) => a.distance - b.distance);
      const next = candidates[0]?.candidate;
      if (!next) return;
      if (extend && current.dataset.catalogId && next.dataset.catalogId)
        selectRange(current.dataset.catalogId, next.dataset.catalogId);
      next.focus();
    }
    const initialLocation = typeof window === "undefined" || typeof window.location === "undefined"
      ? { query: "", sort: "name" as LibrarySort }
      : libraryLocation(new URL(window.location.href));
    let query = initialLocation.query;
    search.value = query;
    let revision = 0;
    let page = 0;
    let generation = 0;
    let libraryIndex: LibraryIndex | null = null;
    let configOverrideCapabilities: CatalogConfigOverrideCapabilities | null = null;
    let currentGrid: HTMLElement | null = null;
    let openMenu: { menu: HTMLDetailsElement; trigger: HTMLButtonElement; observer: MutationObserver } | null = null;
    function closeCatalogMenu(restoreFocus = false): void {
      const current = openMenu;
      if (!current) return;
      openMenu = null;
      current.menu.open = false;
      document.removeEventListener("pointerdown", dismissCatalogMenu);
      current.observer.disconnect();
      if (restoreFocus && current.trigger.isConnected) current.trigger.focus();
    }
    function dismissCatalogMenu(event: PointerEvent): void {
      if (openMenu && event.target instanceof Node && !openMenu.menu.contains(event.target)) closeCatalogMenu();
    }
    function showCatalogMenu(menu: HTMLDetailsElement, trigger: HTMLButtonElement, focusAction: boolean): void {
      if (openMenu?.menu !== menu) {
        closeCatalogMenu();
        const observer = new MutationObserver(() => {
          if (!menu.isConnected) closeCatalogMenu();
        });
        openMenu = { menu, trigger, observer };
        observer.observe(document.body, { childList: true, subtree: true });
        document.addEventListener("pointerdown", dismissCatalogMenu);
      }
      menu.open = true;
      if (focusAction) (menu.querySelector<HTMLButtonElement>(".catalog-card-actions button:not(:disabled)") ?? menu.querySelector<HTMLElement>("summary"))?.focus();
    }
    let view: "grid" | "list" = "grid";
    try {
      if (typeof localStorage !== "undefined" && localStorage.getItem("amaterasu.library.view") === "list") view = "list";
    } catch { /* The library remains usable when storage is unavailable. */ }
    const gridView = button("Grid", () => setView("grid"), "library-view-button");
    const listView = button("List", () => setView("list"), "library-view-button");
    const viewControls = el("div", "library-view-controls", gridView, listView);
    viewControls.setAttribute("role", "group");
    viewControls.setAttribute("aria-label", "Library view");
    function setView(next: "grid" | "list"): void {
      view = next;
      gridView.setAttribute("aria-pressed", String(view === "grid"));
      listView.setAttribute("aria-pressed", String(view === "list"));
      if (currentGrid) currentGrid.className = `catalog-grid catalog-grid--${view}`;
      try { if (typeof localStorage !== "undefined") localStorage.setItem("amaterasu.library.view", view); }
      catch { /* View preference is optional. */ }
    }
    setView(view);
    const sort = el("select", "library-sort") as HTMLSelectElement;
    sort.name = "sort";
    catalogLocalizedAttribute(sort, "aria-label", "Sort library");
    for (const [value, label] of [["name", "Name (A–Z)"], ["name-desc", "Name (Z–A)"], ["source", "Known source"]]) {
      const option = el("option", "", label) as HTMLOptionElement;
      option.value = value ?? "";
      sort.append(option);
    }
    sort.value = initialLocation.sort;
    function replaceLibraryLocation(): void {
      if (typeof window === "undefined" || typeof window.location === "undefined" || typeof window.history === "undefined") return;
      const next = updateLibraryLocation(new URL(window.location.href), query, sort.value as LibrarySort);
      if (next.href !== window.location.href) window.history.replaceState(window.history.state, "", next);
    }
    sort.addEventListener("change", () => { replaceLibraryLocation(); page = 0; void load(); });
    const add = button("+ Add application", () => {
      if (configOverrideCapabilities) showEditor(undefined, configOverrideCapabilities);
    }, "primary");
    add.disabled = true;
    const sources = button("Library sources", () => context.navigate("integrations"));
    sources.disabled = !api.allows("configuration.read");
    content.append(
      sectionHeading(
        "Your library",
        "A durable catalog of the applications on your host.",
        sources, add,
      ),
      el("div", "library-toolbar", searchForm, sort, viewControls),
      selectionBar,
      status,
      editor,
      results,
    );

    async function load(): Promise<void> {
      const current = ++generation;
      results.setAttribute("aria-busy", "true");
      try {
        const active = () => content.isConnected && current === generation;
        const changed = () => {
          if (active()) {
            libraryIndex = null;
            if (selectedIds.size) {
              clearSelection();
              notify("The catalog changed, so the selection was cleared.");
            }
          }
          return new Error("The catalog changed while sorting. Refresh the library to try again.");
        };
        if (!libraryIndex) {
          results.replaceChildren(el("p", "muted", "Sorting the library…"));
          const indexed = await scanLibraryIndex(api, query, active, (count) => {
            if (active()) results.replaceChildren(el("p", "muted", catalogTextLiteral(`Sorting the library… ${count} applications found`)));
          });
          if (!active()) return;
          libraryIndex = indexed;
        }
        const indexed = libraryIndex;
        const probe = await api.post<CatalogReply>("catalog/list", { cursor: null, limit: 1 });
        if (!active()) return;
        if (probe.revision !== indexed.revision) {
          throw changed();
        }
        const ordered = sortLibraryRows(indexed.rows, sort.value as LibrarySort);
        const selectedRows = ordered.slice(page * 12, (page + 1) * 12);
        const rows: CatalogRow[] = [];
        for (let offset = 0; offset < selectedRows.length; offset += 4) {
          const chunk = selectedRows.slice(offset, offset + 4);
          const loaded = await Promise.all(chunk.map(async ({ id }) => {
            const cached = indexed.cachedRows.get(id);
            if (cached) return cached;
            const response = await api.post<{ revision: number; id: string; entry: Entry }>("catalog/get", { id });
            if (response.revision !== indexed.revision || response.id !== id) {
              throw changed();
            }
            return { id, entry: response.entry };
          }));
          if (!active()) return;
          rows.push(...loaded);
        }
        if (!content.isConnected || current !== generation) return;
        if (selectedRevision !== null && selectedRevision !== indexed.revision) {
          clearSelection();
          notify("The catalog changed, so the selection was cleared.");
        }
        revision = indexed.revision;
        visibleIds = rows.map((row) => row.id);
        visibleSelection.clear();
        configOverrideCapabilities = indexed.capabilities;
        add.disabled = !api.allows("catalog.write");
        const grid = el("div", `catalog-grid catalog-grid--${view}`);
        currentGrid = grid;
        for (const row of rows)
          grid.append(card(row, indexed.capabilities));
        results.replaceChildren(
          el(
            "div",
            "section-meta",
            el("h2", "", "Collection"),
            el("span", "muted", catalogTextLiteral(`${ordered.length} applications`)),
          ),
          rows.length
            ? grid
            : empty(
                query
                  ? "No matching applications"
                  : "Make room for your favorites",
                query
                    ? "Try a different name, source, command or tag."
                    : "Add your first application to start building this host’s library.",
              ),
          pager(
            page
              ? () => {
                  page--;
                  void load();
                }
              : null,
            (page + 1) * 12 < ordered.length
              ? () => {
                  page++;
                  void load();
                }
              : null,
            catalogTextLiteral(`Page ${page + 1}`),
          ),
        );
      } catch (error) {
        if (content.isConnected && current === generation) {
          if (error && typeof error === "object" && "code" in error && error.code === "revision_conflict" && selectedIds.size) {
            clearSelection();
            notify("The catalog changed, so the selection was cleared.");
          }
          results.replaceChildren(errorPanel(error, reset));
        }
      } finally {
        if (current === generation) results.removeAttribute("aria-busy");
      }
    }

    function reset(): void {
      libraryIndex = null;
      page = 0;
      void load();
    }
    let searchTimer = 0;
    function searchCatalog(): void {
      window.clearTimeout(searchTimer);
      const next = search.value.trim();
      if (next === query) return;
      query = next;
      replaceLibraryLocation();
      reset();
    }
    search.addEventListener("input", () => {
      window.clearTimeout(searchTimer);
      searchTimer = window.setTimeout(() => {
        if (content.isConnected) searchCatalog();
      }, 300);
    });
    search.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && search.value) {
        event.preventDefault();
        search.value = "";
        searchCatalog();
      }
    });
    searchForm.addEventListener("submit", (event) => {
      event.preventDefault();
      searchCatalog();
    });

    if (typeof window !== "undefined" && typeof window.addEventListener === "function" && typeof MutationObserver !== "undefined") {
      const restoreLocation = (): void => {
        if (!content.isConnected || (window.location.hash !== "#catalog" &&
            !window.location.hash.startsWith("#catalog?"))) return;
        const restored = libraryLocation(new URL(window.location.href));
        window.clearTimeout(searchTimer);
        search.value = restored.query;
        const queryChanged = query !== restored.query;
        const sortChanged = sort.value !== restored.sort;
        query = restored.query;
        sort.value = restored.sort;
        if (queryChanged) reset();
        else if (sortChanged) { page = 0; void load(); }
      };
      window.addEventListener("popstate", restoreLocation);
      window.addEventListener("hashchange", restoreLocation);
      const observer = new MutationObserver(() => {
        if (!content.isConnected) {
          observer.disconnect();
          window.removeEventListener("popstate", restoreLocation);
          window.removeEventListener("hashchange", restoreLocation);
          window.clearTimeout(searchTimer);
        }
      });
      observer.observe(document, { childList: true, subtree: true });
    }

    function card(
      row: CatalogRow,
      capabilities: CatalogConfigOverrideCapabilities,
    ): HTMLElement {
      const providerLaunch = row.entry.launch.kind === "playnite";
      const launchLabel = row.entry.launch.kind === "desktop" ? "Desktop" :
        row.entry.launch.kind === "playnite_fullscreen" ? "Playnite Fullscreen" :
        row.entry.steam_id ? "Steam-linked" : row.entry.launch.kind === "executable" ? "Executable" :
        providerLaunch ? "Playnite" : "Application";
      const edit = button(api.allows("catalog.write") ? "Edit" : "Settings", () => showEditor(row, capabilities));
      edit.disabled = (!api.allows("catalog.write") && !api.allows("configuration.read")) || providerLaunch;
      if (providerLaunch) catalogLocalizedAttribute(edit, "title", "Playnite launch targets are managed by the provider.");
      const launch = button("Launch", () => void runAction(launch, status, async () => {
        const bytes = crypto.getRandomValues(new Uint8Array(16));
        const operation = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
        await api.post("launch/start", { app_id: row.id, operation_id: `web.${operation}` });
        notify(catalogTextMessage("Launch requested. Check Applications for its status."));
      }), "primary");
      launch.disabled = typeof api.allowsLaunchAction !== "function" || !api.allowsLaunchAction("start");
      const actions = [launch, edit];
      if (providerLaunch) {
        const configure = button(
          "Streaming settings",
          () => showProviderOverrideEditor(row, capabilities),
          "secondary",
        );
        configure.disabled = !api.allows("catalog.write") && !api.allows("configuration.read");
        actions.unshift(configure);
      }
      const remove = button(
        "Delete",
        async () => {
          if (
            !(await confirmAction(
              catalogTextLiteral(`Delete ${row.entry.name}?`),
              catalogTextLiteral("This removes its catalog entry. It does not remove files or stop a running application."),
              "Delete entry",
            ))
          )
            return;
          await runAction(remove, status, async () => {
            await api.post("catalog/delete", {
              id: row.id,
              expected_revision: revision,
            });
            notify(catalogTextMessage("Application removed from the catalog."));
            reset();
          });
        },
        "quiet danger-text",
      );
      remove.disabled = !api.allows("catalog.write");
      actions.push(remove);
      const art = el(
        "span", "app-art",
        el("span", "app-monogram", catalogTextLiteral(row.entry.name.slice(0, 2).toUpperCase())),
        badge(launchLabel),
      );
      showCatalogArtwork(api, row.entry.artwork, art, content, undefined, row.id);
      const open = button("", () => {
        if (providerLaunch) showProviderOverrideEditor(row, capabilities);
        else showEditor(row, capabilities);
      }, "catalog-open");
      open.disabled = !api.allows("catalog.write");
      open.dataset.catalogId = row.id;
      open.addEventListener("keydown", (event) => {
        if ((event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) && !event.isComposing) {
          event.preventDefault();
          showCatalogMenu(menu, open, true);
        } else if (event.key === " " && !event.isComposing) {
          event.preventDefault();
          toggleSelection(row.id);
        } else if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) {
          event.preventDefault();
          moveCollectionFocus(open, event.key, event.shiftKey);
        } else if (event.key === "Escape" && selectedIds.size) {
          event.preventDefault();
          event.stopPropagation();
          clearSelection();
        }
      });
      catalogLocalizedAttributeTemplate(open, "aria-label", "Edit {name}", { name: catalogTextLiteral(row.entry.name) });
      open.append(art, el("span", "catalog-card-title",
        el("strong", "", catalogTextLiteral(row.entry.name)),
        el("small", "muted", launchLabel)));
      const summary = el("summary", "catalog-menu-toggle", "Actions");
      catalogLocalizedAttributeTemplate(summary, "aria-label", "Actions for {name}", { name: catalogTextLiteral(row.entry.name) });
      const menu = el("details", "catalog-card-menu", summary,
        el("div", "catalog-card-menu-content",
          el("p", "muted", row.entry.description ? catalogTextLiteral(row.entry.description) : "No description added."),
          el("div", "tags", ...(row.entry.tags ?? []).map((tag) => badge(catalogTextLiteral(tag)))),
          el("p", "path", catalogTextLiteral(launchPath(row.entry))),
          el("div", "catalog-card-actions", ...actions)));
      menu.addEventListener("toggle", () => {
        if (menu.open) showCatalogMenu(menu, open, false);
        else if (openMenu?.menu === menu) closeCatalogMenu();
      });
      menu.addEventListener("keydown", (event) => {
        if (event.key === "Escape" && menu.open) {
          event.preventDefault();
          event.stopPropagation();
          closeCatalogMenu(true);
        }
      });
      const select = button("", () => toggleSelection(row.id), "catalog-select");
      const article = el("article", "app-card", open, select, menu);
      article.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        showCatalogMenu(menu, open, true);
      });
      const update = () => {
        const selected = selectedIds.has(row.id);
        article.className = `app-card${selected ? " is-selected" : ""}`;
        select.textContent = selected ? "✓" : "+";
        select.setAttribute("aria-pressed", String(selected));
        catalogLocalizedAttributeTemplate(select, "aria-label", selected ? "Deselect {name}" : "Select {name}",
          { name: catalogTextLiteral(row.entry.name) });
      };
      visibleSelection.set(row.id, update);
      update();
      return article;
    }

    function closeEditor(): boolean {
      if (editorSaving) {
        notify("Wait for the application save to finish.");
        return false;
      }
      if (pluginArea && typeof applicationPluginOverridesSaving === "function" && applicationPluginOverridesSaving(pluginArea)) {
        notify("Wait for the plugin override save to finish.");
        return false;
      }
      if ((catalogDraftDirty() || pluginDirty()) && !window.confirm("Discard your unsaved application and plugin changes?")) return false;
      editorLifecycle?.abort();
      editorLifecycle = null;
      pluginArea = null;
      editorGeneration += 1;
      editor.replaceChildren();
      return true;
    }

    function showProviderOverrideEditor(
      row: CatalogRow,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      if (!closeEditor()) return;
      const request = editorGeneration;
      editor.replaceChildren(el("p", "muted", "Loading input profiles…"));
      void loadInputRuntimeStatus().then((status) => {
        if (context.content.isConnected && request === editorGeneration)
          renderProviderOverrideEditor(row, status, capabilities);
      });
    }

    function lockEditorControls(form: HTMLFormElement): () => void {
      if (typeof form.querySelectorAll !== "function") return () => {};
      const controls = Array.from(form.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement | HTMLButtonElement>(
        "input, textarea, select, button",
      ));
      const states = controls.map((control) => control.disabled);
      controls.forEach((control) => { control.disabled = true; });
      return () => controls.forEach((control, index) => { control.disabled = states[index] ?? false; });
    }

    async function loadInputRuntimeStatus(): Promise<InputRuntimeStatus> {
      try {
        return await api.post<InputRuntimeStatus>("input/status");
      } catch {
        return {
          ok: false,
          gamepad: "auto",
          supported_profiles: ["auto"],
          recognized_profiles: ["auto"],
          effective_gamepad_profile: null,
          gamepad_unavailable_reason: null,
          controller_input: false,
          controller_enabled: true,
          controller_cleanup_required: false,
          lease: null,
        };
      }
    }

    function renderProviderOverrideEditor(
      row: CatalogRow,
      runtimeStatus: InputRuntimeStatus,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      let editRevision = revision;
      const currentFec = row.entry.config_overrides?.fec_percentage;
      const fec = input(
        "fec_percentage",
        currentFec === undefined ? "" : String(currentFec),
        "number",
      );
      fec.min = "1";
      fec.max = "255";
      fec.step = "1";
      fec.placeholder = "Inherit";
      catalogLocalizedAttributeTemplate(
        fec,
        "aria-label",
        "{field} override for {client}",
        {
          field: catalogTextMessage("FEC percentage"),
          client: catalogTextLiteral(row.entry.name),
        },
      );
      const sessionPolicyFields = catalogSessionPolicyOverrideFields(
        row.entry.config_overrides,
        capabilities,
      );
      const nvencSettingsFields = catalogNvencSettingsOverrideFields(row.entry.config_overrides);
      const inputCompatibilityFields = catalogInputCompatibilityOverrideFields(
        row.entry.config_overrides,
      );
      const streamAudio = audioOverrideSelect(
        "stream_audio",
        row.entry.config_overrides?.stream_audio,
      );
      const audioSink = audioSinkOverrideControls(row.entry.config_overrides?.audio_sink);
      const virtualSink = virtualSinkOverrideControls(row.entry.config_overrides?.virtual_sink);
      const captureOnly = audioOverrideSelect(
        "audio_sink_capture_only",
        row.entry.config_overrides?.audio_sink_capture_only,
      );
      const backButtonTimeout = input(
        "back_button_timeout",
        row.entry.config_overrides?.back_button_timeout?.toString() ?? "",
        "number",
      );
      backButtonTimeout.min = String(-0x8000_0000);
      backButtonTimeout.max = String(0x7fff_ffff);
      backButtonTimeout.step = "1";
      backButtonTimeout.placeholder = "Inherit";
      catalogLocalizedAttributeTemplate(
        backButtonTimeout,
        "aria-label",
        "{field} override for {client}",
        {
          field: catalogTextMessage("Back button timeout (milliseconds)"),
          client: catalogTextLiteral(row.entry.name),
        },
      );
      const controller = audioOverrideSelect("controller", row.entry.config_overrides?.controller);
      const gamepad = inputGamepadOverrideSelect("gamepad", row.entry.config_overrides?.gamepad, runtimeStatus);
      const keyboard = audioOverrideSelect("keyboard", row.entry.config_overrides?.keyboard);
      const mouse = audioOverrideSelect("mouse", row.entry.config_overrides?.mouse);
      const advancedInput = advancedInputOverrideControls(row.entry.config_overrides);
      const softwareEncoder = softwareEncoderOverrideControls(row.entry.config_overrides);
      const refreshRateOptionFields = catalogRefreshRateOptionFields(
        row.entry.config_overrides?.dd_refresh_rate_option,
      );
      const displayPolicy = el("textarea");
      displayPolicy.name = "display_policy";
      displayPolicy.rows = 12;
      displayPolicy.spellcheck = false;
      displayPolicy.value = JSON.stringify(row.entry.config_overrides?.display_policy ?? {}, null, 2);
      const form = el("form", "editor-form",
        catalogEditorSection("Streaming and audio", "Override streaming and audio for this provider-managed application.",
          field("FEC percentage", fec, "Leave blank to inherit the global setting. Supported values are 1–255."),
          ...sessionPolicyFields,
          field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this game streams audio."),
          audioSink.controls, virtualSink.controls,
          field("Capture audio without changing default outputs", captureOnly,
            "Inherit the host setting, enable capture only, or allow Windows default-output routing.")),
        catalogEditorDisclosure("Display", "Choose display and adapter overrides.",
          ...refreshRateOptionFields,
          field("Display and adapter policy (JSON)", displayPolicy,
            "Sparse application overrides. Use null or empty values to inherit; global-only keys are rejected.")),
        catalogEditorDisclosure("Input", "Set per-application controller, keyboard, and mouse preferences.",
          field("Back button timeout (milliseconds)", backButtonTimeout,
            "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."),
          field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."),
          field("Gamepad profile", gamepad, "Inherit the next scope. Recognized unavailable profiles remain selectable."),
          field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."),
          field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."),
          ...inputCompatibilityFields, ...advancedInput.fields),
        catalogEditorDisclosure("Encoder", "Override software and NVENC encoder settings.",
          ...softwareEncoder.fields, ...nvencSettingsFields));
      const overrides = catalogOverrideEditor(catalogOverrideSections(Array.from(form.children) as HTMLElement[]), { ...row.entry.config_overrides });
      form.replaceChildren(overrides.node);
      form.hidden = !api.allows("catalog.write");
      revealInvalidEditorControls(form);
      const feedback = el("div");
      const save = el("button", "button primary", "Save streaming settings");
      save.type = "submit";
      const cancel = button("Cancel", () => { closeEditor(); });
      form.append(feedback, el("div", "catalog-editor-save-bar", el("span", "muted", "Save or cancel your changes."), el("div", "actions", cancel, save)));
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!form.reportValidity()) return;
        if (pluginDirty()) {
          feedback.replaceChildren(el("p", "notice error", "Save plugin overrides before saving this application, or cancel to discard them."));
          return;
        }
        void runAction(save, feedback, async () => {
          const config_overrides = mergeCatalogConfigOverrides(
            row.entry.config_overrides,
            new FormData(form).keys(),
            advancedInput.read(),
            softwareEncoder.read(),
            providerConfigOverrides(fec.value, streamAudio.value, audioSink.mode.value, audioSink.id.value, captureOnly.value, backButtonTimeout.value, virtualSink.mode.value, virtualSink.selector.value, controller.value, gamepad.value, keyboard.value, mouse.value, runtimeStatus.recognized_profiles),
            parseCatalogSessionPolicyOverrides(
              new FormData(form),
              capabilities,
              row.entry.config_overrides,
            ),
            parseCatalogNvencSettingsOverrides(new FormData(form), row.entry.config_overrides),
            parseCatalogInputCompatibilityOverrides(new FormData(form)),
            {
              dd_refresh_rate_option: parseCatalogRefreshRateOptionOverride(new FormData(form)),
              display_policy: parseDisplayPolicyOverrideMap(displayPolicy.value, "application"),
            },
          );
          for (const key of overrides.removed) delete (config_overrides as Record<string, unknown>)[key];
          editorSaving = true;
          editor.inert = true;
          const unlock = lockEditorControls(form);
          try {
            const reply = await api.post<{
              id: string;
              revision: number;
              config_overrides: {
                fec_percentage?: number;
                stream_audio?: boolean;
                audio_sink?: string;
                virtual_sink?: string;
                audio_sink_capture_only?: boolean;
                back_button_timeout?: number;
                controller?: boolean;
                gamepad?: string;
                keyboard?: boolean;
                mouse?: boolean;
                dd_refresh_rate_option?: RefreshRateOption | null;
                display_policy?: Record<string, unknown> | null;
              } & CatalogSessionPolicyOverrides & CatalogNvencSettingsOverrides & CatalogInputCompatibilityOverrides;
              changed: boolean;
              replayed: boolean;
            }>("catalog/update_provider_overrides", {
              id: row.id,
              expected_revision: editRevision,
              config_overrides,
            });
            editRevision = reply.revision;
            editor.replaceChildren();
            notify(
              reply.replayed
                ? `The earlier streaming override save for ${row.entry.name} is confirmed.`
                : reply.changed
                  ? `Streaming override saved for ${row.entry.name}.`
                  : `Streaming override for ${row.entry.name} is unchanged.`,
            );
            await load();
          } catch (error) {
            if (error instanceof ApiError && error.code === "revision_conflict") {
              const reload = button("Reload library", () => {
                if (!closeEditor()) return;
                void load();
              });
              feedback.replaceChildren(el("div", "notice error",
                el("strong", "", "Application changed elsewhere"),
                el("p", "", "Your edits are still here. Review or copy them before reloading the library."), reload));
              return;
            }
            throw error;
          } finally {
            editorSaving = false;
            editor.inert = false;
            unlock();
          }
        });
      });
      const cover = catalogEditorCover(row);
      editor.replaceChildren(
        panel(
          "Playnite streaming settings",
          el("p", "muted", catalogTextLiteral(`${row.entry.name} · Playnite game ${row.entry.launch.kind === "playnite" ? row.entry.launch.game_id : ""}`)),
          el(
            "p",
            "muted",
            "The Playnite game identity and provider-managed launch metadata stay unchanged.",
          ),
          el("div", "catalog-editor-layout", cover.figure, form),
          el("section", "catalog-editor-section", el("h3", "", "Plugin overrides"), pluginArea = el("div")),
          ...(!api.allows("catalog.write") ? [button("Close", () => { closeEditor(); })] : []),
        ),
      );
      if (api.allows("configuration.read") && typeof renderApplicationPluginOverrides === "function") renderApplicationPluginOverrides(context, pluginArea, row.id, form.hidden ? undefined : form.querySelector<HTMLElement>(".catalog-overrides") ?? undefined);
      else pluginArea.textContent = "Plugin settings require configuration read access.";
      showCatalogArtwork(api, row.entry.artwork, cover.art, content, () => { cover.fallback.hidden = true; }, row.id);
      editor.scrollIntoView({ behavior: "smooth", block: "start" });
      fec.focus();
    }

    function showEditor(
      row: CatalogRow | undefined,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      if (!closeEditor()) return;
      const request = editorGeneration;
      editor.replaceChildren(el("p", "muted", "Loading input profiles…"));
      void loadInputRuntimeStatus().then((runtimeStatus) => {
        if (context.content.isConnected && request === editorGeneration)
          renderEditor(row, runtimeStatus, capabilities);
      });
    }

    function renderEditor(
      row: CatalogRow | undefined,
      runtimeStatus: InputRuntimeStatus,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      const editRevision = revision;
      const lifecycle = new AbortController();
      editorLifecycle = lifecycle;
      const form = entryForm(row?.entry, runtimeStatus, capabilities, api, lifecycle.signal);
      form.hidden = !api.allows("catalog.write");
      const feedback = el("div");
      const save = el(
        "button",
        "button primary",
        row ? "Save changes" : "Add application",
      );
      save.type = "submit";
      const cancel = button("Cancel", () => { closeEditor(); });
      form.append(feedback, el("div", "catalog-editor-save-bar", el("span", "muted", "Save or cancel your changes."), el("div", "actions", cancel, save)));
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!form.reportValidity()) return;
        if (pluginDirty()) {
          feedback.replaceChildren(el("p", "notice error", "Save plugin overrides before saving this application, or cancel to discard them."));
          return;
        }
        void runAction(save, feedback, async () => {
          const entry = entryFromForm(new FormData(form), runtimeStatus.recognized_profiles);
          Object.assign(entry, parseCatalogApplicationDisplaySettings(new FormData(form)));
          entry.config_overrides = mergeCatalogConfigOverrides(
            row?.entry.config_overrides,
            new FormData(form).keys(),
            entry.config_overrides,
            parseCatalogSessionPolicyOverrides(
              new FormData(form),
              capabilities,
              row?.entry.config_overrides,
            ),
            parseCatalogNvencSettingsOverrides(new FormData(form), row?.entry.config_overrides),
            parseCatalogInputCompatibilityOverrides(new FormData(form)),
          );
          for (const key of catalogRemovedOverrides.get(form) ?? []) delete (entry.config_overrides as Record<string, unknown>)[key];
          editorSaving = true;
          editor.inert = true;
          const unlock = lockEditorControls(form);
          try {
            await api.post(row ? "catalog/update" : "catalog/create", {
              ...(row ? { id: row.id } : {}),
              entry,
              expected_revision: editRevision,
            });
            lifecycle.abort();
            if (editorLifecycle === lifecycle) editorLifecycle = null;
            editor.replaceChildren();
            notify(catalogTextMessage(row ? "Application updated." : "Application added."));
            reset();
          } finally {
            editorSaving = false;
            editor.inert = false;
            unlock();
          }
        });
      });
      const cover = row ? catalogEditorCover(row) : null;
      editor.replaceChildren(
        panel(
          row ? "Edit application" : "Add application",
          el(
            "p",
            "muted",
            "Save launch metadata for this host. Adding an entry does not launch it.",
          ),
          cover ? el("div", "catalog-editor-layout", cover.figure, form) : form,
          el("section", "catalog-editor-section", el("h3", "", "Plugin overrides"), pluginArea = el("div", "", ...(row ? [] : [el("p", "muted", "Plugin overrides are available after creating this application.")]))),
          ...(!api.allows("catalog.write") ? [button("Close", () => { closeEditor(); })] : []),
        ),
      );
      if (row && api.allows("configuration.read") && typeof renderApplicationPluginOverrides === "function") renderApplicationPluginOverrides(context, pluginArea, row.id, form.hidden ? undefined : form.querySelector<HTMLElement>(".catalog-overrides") ?? undefined);
      else if (row && !api.allows("configuration.read")) pluginArea.textContent = "Plugin settings require configuration read access.";
      if (cover && row) showCatalogArtwork(api, row.entry.artwork, cover.art, content, () => { cover.fallback.hidden = true; }, row.id);
      editor.scrollIntoView({ behavior: "smooth", block: "start" });
      form.querySelector("input")?.focus();
    }
    void load();
  }

  function catalogEditorCover(row: CatalogRow): { figure: HTMLElement; art: HTMLElement; fallback: HTMLElement } {
    const fallback = el("span", "catalog-editor-artwork-fallback", "Cover unavailable");
    const art = el("div", "catalog-editor-artwork", fallback);
    const provider = row.entry.artwork?.provider;
    const description = provider === "steam" ? "Steam artwork" :
      provider === "lutris" ? "Lutris artwork" : provider === "external" ? "Provider artwork" : "No provider cover";
    const figure = el("figure", "catalog-editor-cover", art,
      el("figcaption", "muted", description));
    return { figure, art, fallback };
  }

  function catalogEditorSection(title: string, description: string, ...children: HTMLElement[]): HTMLElement {
    const heading = el("h3", "", title);
    heading.tabIndex = -1;
    return el("section", "catalog-editor-section",
      el("div", "catalog-editor-heading", heading, el("p", "muted", description)),
      ...children);
  }

  function catalogEditorDisclosure(title: string, description: string, ...children: HTMLElement[]): HTMLElement {
    return el("details", "catalog-editor-section catalog-editor-disclosure",
      el("summary", "", el("strong", "", title), el("small", "muted", description)),
      el("div", "catalog-editor-disclosure-content", ...children));
  }

  function catalogEditorNavigation(items: Array<{
    label: string;
    target: HTMLElement;
    reveal?: HTMLDetailsElement | null;
  }>): HTMLElement {
    const navigation = el("nav", "catalog-editor-nav");
    catalogLocalizedAttribute(navigation, "aria-label", "Application editor sections");
    navigation.append(el("span", "catalog-editor-nav-label", catalogTextMessage("SECTIONS")));
    for (const item of items) {
      const control = button(catalogTextMessage(item.label), () => {
        if (item.target.tagName.toLowerCase() === "details")
          (item.target as HTMLDetailsElement).open = true;
        if (item.reveal) item.reveal.open = true;
        const reduceMotion = typeof window !== "undefined" &&
          typeof window.matchMedia === "function" &&
          window.matchMedia("(prefers-reduced-motion: reduce)").matches;
        item.target.scrollIntoView({ behavior: reduceMotion ? "auto" : "smooth", block: "start" });
        const focusTarget = item.reveal?.querySelector("summary") ??
          (item.target.tagName.toLowerCase() === "details"
            ? item.target.querySelector("summary")
            : item.target.querySelector("h3"));
        if (focusTarget instanceof HTMLElement) focusTarget.focus();
      }, "quiet");
      control.className += " catalog-editor-nav-item";
      control.setAttribute("aria-controls", item.target.id);
      navigation.append(control);
    }
    return navigation;
  }

  // Controls remain in the form so the established parsers and conditional
  // controls retain their behavior when their rows are hidden.
  function catalogOverrideEditor(sections: Array<{ area: string; fields: HTMLElement[] }>, saved: Record<string, unknown>): { node: HTMLElement; removed: Set<string> } {
    const removed = new Set<string>();
    const active = el("div", "catalog-override-active");
    const empty = el("p", "catalog-override-empty muted", "Choose a setting to customize it for this application. All other settings inherit their defaults.");
    const search = input("", "", "search");
    search.placeholder = "Search settings";
    search.setAttribute("data-draft-ignore", "");
    search.setAttribute("aria-label", "Search available overrides");
    const list = el("div", "catalog-override-catalog");
    const marker = input("", "", "text");
    marker.className = "catalog-override-draft-marker";
    marker.tabIndex = -1;
    marker.setAttribute("aria-hidden", "true");
    const rows: Array<{ area: string; label: string; description: string; row: HTMLElement; choice: HTMLButtonElement; selected: boolean }> = [];
    const groups: HTMLElement[] = [];
    const mapping: Record<string, string> = { encoder_backend: "encoder", audio_sink_mode: "audio_sink", virtual_sink_mode: "virtual_sink", sw_preset_mode: "sw_preset", sw_tune_mode: "sw_tune", display_output: "display-output", virtual_screen: "virtual-screen", virtual_display_mode: "virtual-display-mode", virtual_display_layout: "virtual-display-layout" };
    for (const section of sections) {
      const group = el("div", "catalog-override-group", el("h4", "", section.area));
      groups.push(group);
      list.append(group);
      for (let index = 0; index < section.fields.length; index++) {
        const fieldNode = section.fields[index]!;
        const first = fieldNode.querySelector<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>("input,select,textarea");
        if (!first) continue;
        const key = mapping[first.name] ?? first.name;
        const parts = [fieldNode];
        if ((key === "sw_preset" || key === "sw_tune") && section.fields[index + 1]) parts.push(section.fields[++index]!);
        const controls = parts.flatMap(part => Array.from(part.querySelectorAll<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>("input,select,textarea")));
        const label = fieldNode.querySelector(".field-label")?.textContent?.trim() || key;
        const description = fieldNode.querySelector(".muted")?.textContent?.trim() ?? "";
        const row = el("div", "catalog-override-row");
        const choice = button(label, () => {
          if (!item.selected) { item.selected = true; removed.delete(key); marker.value += "+"; update(); }
          first.focus();
        });
        choice.className = "button secondary catalog-override-choice";
        const remove = button("Remove", () => {
          item.selected = false;
          removed.add(key);
          for (const control of controls) {
            if (control instanceof HTMLSelectElement) control.value = Array.from(control.options).some(option => option.value === "inherit") ? "inherit" : "";
            else control.value = control.name === "display_policy" ? "{}" : "";
            control.dispatchEvent(new Event("change", { bubbles: true }));
          }
          marker.value += "-";
          update();
        }, "secondary");
        remove.setAttribute("aria-label", `Remove ${label} override`);
        row.append(el("div", "catalog-override-row-content", ...parts), remove);
        const item = { area: section.area, label, description, row, choice, selected: Object.hasOwn(saved, key) && saved[key] != null };
        rows.push(item);
        active.append(row);
        group.append(choice);
      }
    }
    function update(): void {
      const query = search.value.trim().toLocaleLowerCase();
      for (const item of rows) {
        item.row.hidden = !item.selected;
        item.choice.hidden = !`${item.area} ${item.label} ${item.description}`.toLocaleLowerCase().includes(query);
        item.choice.setAttribute("data-selected", String(item.selected));
        item.choice.setAttribute("aria-label", `${item.selected ? "Edit" : "Add"} ${item.label} override`);
      }
      for (const group of groups) group.hidden = !group.querySelector("button:not([hidden])");
      empty.hidden = rows.some(item => item.selected);
    }
    search.addEventListener("input", update);
    const heading = el("h3", "", catalogTextMessage("Settings overrides"));
    heading.tabIndex = -1;
    const node = el("section", "catalog-editor-section catalog-overrides",
      el("div", "catalog-editor-heading", heading, el("p", "muted", catalogTextMessage("Inheritance is field-specific. Use the field's Inherit option or placeholder; Automatic audio selection and exact empty encoder strings are explicit choices."))),
      marker, el("div", "catalog-override-layout",
        el("div", "catalog-override-browser", el("div", "catalog-override-pane-heading", el("h4", "", "Browse settings"), el("p", "muted", "Choose a setting to add or edit.")), search, list),
        el("div", "catalog-override-editor", el("div", "catalog-override-pane-heading", el("h4", "", "Active overrides"), el("p", "muted", "Changes apply to this application.")), empty, active)));
    update();
    return { node, removed };
  }

  function catalogOverrideSections(sections: HTMLElement[]): Array<{ area: string; fields: HTMLElement[] }> {
    return sections.map(section => ({
      area: section.querySelector("h3, summary strong")?.textContent?.trim() ?? "Settings",
      fields: Array.from((section.querySelector(".catalog-editor-disclosure-content") ?? section).children).filter((child): child is HTMLElement => child instanceof HTMLElement && !child.classList.contains("catalog-editor-heading")),
    }));
  }

  function revealInvalidEditorControls(form: HTMLFormElement): void {
    form.addEventListener("invalid", (event) => {
      let details = (event.target as HTMLElement | null)?.closest("details");
      while (details && form.contains(details)) {
        details.open = true;
        details = details.parentElement?.closest("details") ?? null;
      }
    }, true);
  }

  const catalogRemovedOverrides = new WeakMap<HTMLFormElement, Set<string>>();

  function entryForm(
    entry: Entry | undefined,
    runtimeStatus: InputRuntimeStatus,
    capabilities: CatalogConfigOverrideCapabilities,
    api: Api,
    lifecycle: AbortSignal,
  ): HTMLFormElement {
    const form = el("form", "editor-form");
    const name = input("name", entry?.name ?? "");
    name.required = true;
    name.maxLength = 160;
    const description = el("textarea");
    description.name = "description";
    description.value = entry?.description ?? "";
    description.maxLength = 1024;
    description.rows = 2;
    const tags = input("tags", entry?.tags?.join(", ") ?? "");
    const appDisplayValues = catalogApplicationDisplayFormValues(entry);
    const displayOutput = input("display_output", appDisplayValues.display_output);
    displayOutput.maxLength = 1_024;
    const virtualScreen = el("select");
    virtualScreen.name = "virtual_screen";
    for (const [value, label] of [
      ["", "Inherit"],
      ["true", "Request a virtual screen"],
      ["false", "Do not request a virtual screen"],
    ] as const) {
      const option = el("option", "", label);
      option.value = value;
      virtualScreen.append(option);
    }
    virtualScreen.value = appDisplayValues.virtual_screen;
    const virtualDisplayMode = el("select");
    virtualDisplayMode.name = "virtual_display_mode";
    for (const [value, label] of [
      ["", "Inherit"],
      ["disabled", "Disabled"],
      ["per_client", "Per client"],
      ["shared", "Shared"],
    ] as const) {
      const option = el("option", "", label);
      option.value = value;
      virtualDisplayMode.append(option);
    }
    virtualDisplayMode.value = appDisplayValues.virtual_display_mode;
    const virtualDisplayLayout = el("select");
    virtualDisplayLayout.name = "virtual_display_layout";
    for (const [value, label] of [
      ["", "Inherit"],
      ["exclusive", "Exclusive"],
      ["extended", "Extended"],
      ["extended_primary", "Extended primary"],
      ["extended_isolated", "Extended isolated"],
      ["extended_primary_isolated", "Extended primary isolated"],
    ] as const) {
      const option = el("option", "", label);
      option.value = value;
      virtualDisplayLayout.append(option);
    }
    virtualDisplayLayout.value = appDisplayValues.virtual_display_layout;
    const outputValues = applicationOutputFormValues(entry?.output);
    const outputMode = el("select");
    outputMode.name = "output_mode";
    for (const [value, label] of [["unset", "Use default"], ["value", "Set output"]] as const) {
          const option = el("option", "", label);
      option.value = value;
      outputMode.append(option);
    }
    outputMode.value = outputValues.output_mode;
    const outputValue = input("output_value", outputValues.output_value);
    outputValue.maxLength = 1024;
    const outputFile = field(
      "Command output file",
      outputValue,
      "Append application, preparation, and detached command output here. External provider game output is unavailable. Relative paths use the host working directory. Empty text or null discards output.",
    );
    const showOutput = () => {
      outputValue.disabled = outputMode.value !== "value";
      outputFile.hidden = outputValue.disabled;
    };
    outputMode.addEventListener("change", showOutput);
    showOutput();
    const excludeGlobalPreparation = el("input");
    excludeGlobalPreparation.type = "checkbox";
    excludeGlobalPreparation.name = "exclude_global_prep_cmd";
    excludeGlobalPreparation.checked = entry?.exclude_global_prep_cmd ?? false;
    const kind = el("select");
    kind.name = "kind";
    for (const [value, text] of [
      ["application", "Application / bundle"],
      ["executable", "Executable with arguments"],
      ["playnite_fullscreen", "Playnite Fullscreen"],
      ["desktop", "Desktop session"],
    ]) {
      const option = el("option", "", text);
      option.value = value ?? "";
      kind.append(option);
    }
    kind.value = entry?.launch.kind ?? "application";
    const path = input("path", entry ? launchPath(entry) : "");
    path.required = true;
    path.placeholder = "/Applications/Example.app";
    const executableLaunch =
      entry?.launch.kind === "executable" ? entry.launch : undefined;
    const argumentsField = el("textarea");
    argumentsField.name = "arguments";
    argumentsField.rows = 2;
    argumentsField.value = JSON.stringify(executableLaunch?.arguments ?? []);
    const preparedLaunch = entry?.launch.kind === "executable" || entry?.launch.kind === "desktop" || entry?.launch.kind === "playnite_fullscreen"
      ? entry.launch : undefined;
    const directory = input("working_directory", preparedLaunch?.working_directory ?? "");
    const autoDetach = el("input");
    autoDetach.type = "checkbox";
    autoDetach.name = "auto_detach";
    autoDetach.checked = executableLaunch?.auto_detach ?? true;
    const elevated = input("elevated", "", "checkbox");
    elevated.checked = entry?.elevated ?? false;
    const waitAll = el("input");
    waitAll.type = "checkbox";
    waitAll.name = "wait_all";
    waitAll.checked = executableLaunch?.wait_all ?? true;
    const exitTimeout = input(
      "exit_timeout_seconds",
      String(executableLaunch?.exit_timeout_seconds ?? 5),
      "number",
    );
    exitTimeout.min = "0";
    exitTimeout.max = "300";
    exitTimeout.step = "1";
    const detached = el("textarea");
    detached.name = "detached";
    detached.rows = 3;
    detached.value = executableLaunch?.detached?.join("\n") ?? "";
    const preparation = input(
      "preparation",
      JSON.stringify(preparedLaunch?.preparation ?? []),
      "hidden",
    );
    const rawPreparation = input("prep_cmd", JSON.stringify(entry?.prep_cmd ?? []), "hidden");
    const fec = input(
      "fec_percentage",
      entry?.config_overrides?.fec_percentage?.toString() ?? "",
      "number",
    );
    fec.min = "1";
    fec.max = "255";
    fec.step = "1";
    const sessionPolicyFields = catalogSessionPolicyOverrideFields(
      entry?.config_overrides,
      capabilities,
    );
    const nvencSettingsFields = catalogNvencSettingsOverrideFields(entry?.config_overrides);
    const inputCompatibilityFields = catalogInputCompatibilityOverrideFields(
      entry?.config_overrides,
    );
    const streamAudio = audioOverrideSelect(
      "stream_audio",
      entry?.config_overrides?.stream_audio,
    );
    const audioSink = audioSinkOverrideControls(entry?.config_overrides?.audio_sink);
    const virtualSink = virtualSinkOverrideControls(entry?.config_overrides?.virtual_sink);
    const captureOnly = audioOverrideSelect(
      "audio_sink_capture_only",
      entry?.config_overrides?.audio_sink_capture_only,
    );
    const backButtonTimeout = input(
      "back_button_timeout",
      entry?.config_overrides?.back_button_timeout?.toString() ?? "",
      "number",
    );
    backButtonTimeout.min = String(-0x8000_0000);
    backButtonTimeout.max = String(0x7fff_ffff);
    backButtonTimeout.step = "1";
    backButtonTimeout.placeholder = "Inherit";
    const controller = audioOverrideSelect("controller", entry?.config_overrides?.controller);
    const gamepad = inputGamepadOverrideSelect("gamepad", entry?.config_overrides?.gamepad, runtimeStatus);
    const keyboard = audioOverrideSelect("keyboard", entry?.config_overrides?.keyboard);
    const mouse = audioOverrideSelect("mouse", entry?.config_overrides?.mouse);
    const advancedInput = advancedInputOverrideControls(entry?.config_overrides);
    const softwareEncoder = softwareEncoderOverrideControls(entry?.config_overrides);
    const refreshRateOptionFields = catalogRefreshRateOptionFields(
      entry?.config_overrides?.dd_refresh_rate_option,
    );
    const displayPolicy = el("textarea");
    displayPolicy.name = "display_policy";
    displayPolicy.rows = 12;
    displayPolicy.spellcheck = false;
    displayPolicy.value = JSON.stringify(entry?.config_overrides?.display_policy ?? {}, null, 2);
    const prefer10BitSdr = audioOverrideSelect(
      "prefer_10bit_sdr",
      catalogPrefer10BitSdrOverrideValue(entry?.config_overrides?.prefer_10bit_sdr) ?? undefined,
    );
    const steamId = input("steam_id", entry?.steam_id ?? "");
    steamId.maxLength = 10;
    const steamInstallDir = input("steam_install_dir", entry?.steam_install_dir ?? "");
    steamInstallDir.maxLength = 1024;
    const fullscreenTimeout = input(
      "fullscreen_exit_timeout_seconds",
      String(entry?.launch.kind === "playnite_fullscreen" || entry?.launch.kind === "desktop" ? entry.launch.exit_timeout_seconds : 10),
      "number",
    );
    fullscreenTimeout.min = "0";
    fullscreenTimeout.max = "300";
    fullscreenTimeout.step = "1";
    fullscreenTimeout.required = true;
    const processPolicy = el(
      "div",
      "form-grid",
      field("Auto detach", autoDetach, "Retain a quick successful launcher."),
      field("Run with administrator privileges", elevated, "Windows uses an available elevated user token. Other platforms keep the host's normal launch identity."),
      field("Wait for all processes", waitAll, "Keep the app active while its process group is running."),
      field("Exit timeout (seconds)", exitTimeout, "0 skips the graceful shutdown window; maximum 300."),
      field("Detached commands", detached, "One ordered command per line; up to 16."),
    );
    const executable = el(
      "div",
      "form-grid",
      field(
        "Arguments",
        argumentsField,
        'A JSON array of literal arguments. Example: ["--fullscreen"]',
      ),
      processPolicy,
    );
    const sharedLaunchOptions = el("div", "form-grid", field("Working directory", directory, "Optional absolute path."), preparation);
    const pathField = field("Absolute application path", path);
    const steamIdField = field("Steam app ID", steamId, "Optional canonical Steam app ID for provider-aware process tracking.");
    const steamInstallDirField = field("Steam install directory", steamInstallDir, "Optional absolute game directory; Linux tracks only newly launched processes inside it.");
    const steamChooserStatus = el("p", "muted");
    steamChooserStatus.setAttribute("role", "status");
    steamChooserStatus.hidden = true;
    const setSteamChooserStatus = (message: string): void => {
      steamChooserStatus.hidden = false;
      if (typeof Amaterasu.setLocalizedTextContent === "function")
        Amaterasu.setLocalizedTextContent(steamChooserStatus, message);
      else
        steamChooserStatus.textContent = message;
    };
    const chooseSteamGameButton = button(catalogTextMessage("Choose a Steam game"), async () => {
      if (!api.allows("catalog.write") || lifecycle.aborted || !form.isConnected) return;
      const selected = await chooseSteamGame(api, lifecycle);
      if (!selected || lifecycle.aborted || !form.isConnected) return;
      if (!api.allows("catalog.write") || !api.allows("steam.read")) {
        setSteamChooserStatus("A required permission changed; the selection was not applied.");
        return;
      }

      const selectedId = String(selected.app_id);
      const directoryAvailable = selected.install_directory !== undefined &&
        selected.install_directory.trim() !== "";
      const changingGame = steamId.value !== selectedId;
      let keepDirectory = false;
      let clearDirectory = false;
      if (!directoryAvailable && changingGame && steamInstallDir.value.trim() !== "") {
        const choice = await chooseSteamDirectoryChange(lifecycle);
        if (!choice || lifecycle.aborted || !form.isConnected) return;
        if (choice === "review") {
          setSteamChooserStatus("Selection was not applied. Review the Steam fields and choose again.");
          return;
        }
        if (!api.allows("catalog.write") || !api.allows("steam.read")) {
          setSteamChooserStatus("A required permission changed; the selection was not applied.");
          return;
        }
        keepDirectory = choice === "keep";
        clearDirectory = choice === "clear";
      }

      steamId.value = selectedId;
      steamId.dispatchEvent(new Event("input", { bubbles: true }));
      steamId.dispatchEvent(new Event("change", { bubbles: true }));
      if (directoryAvailable) {
        steamInstallDir.value = selected.install_directory!;
        steamInstallDir.dispatchEvent(new Event("input", { bubbles: true }));
        steamInstallDir.dispatchEvent(new Event("change", { bubbles: true }));
      } else if (clearDirectory) {
        steamInstallDir.value = "";
        steamInstallDir.dispatchEvent(new Event("input", { bubbles: true }));
        steamInstallDir.dispatchEvent(new Event("change", { bubbles: true }));
      }

      const status = directoryAvailable
        ? "Steam game selected. Other application edits were kept."
        : keepDirectory
          ? "Steam game selected. Kept the existing directory by choice."
          : clearDirectory
            ? "Steam game selected. Cleared the existing directory by choice."
            : "Steam game selected. Its install directory is unavailable; the existing directory was kept.";
      setSteamChooserStatus(status);
    });
    catalogLocalizedAttribute(chooseSteamGameButton, "aria-label", "Choose a Steam game");
    const steamGameChooser = el("div", "catalog-steam-chooser",
      chooseSteamGameButton, steamChooserStatus);
    const fullscreen = field("Exit timeout (seconds)", fullscreenTimeout, "Graceful session close window, from 0 to 300 seconds.");
    const showKind = () => {
      executable.hidden = kind.value !== "executable";
      elevated.disabled = kind.value !== "executable";
      const processless = kind.value === "playnite_fullscreen" || kind.value === "desktop";
      fullscreen.hidden = !processless;
      fullscreenTimeout.disabled = !processless;
      pathField.hidden = processless;
      path.disabled = processless;
      path.required = !processless;
      steamIdField.hidden = processless;
      steamInstallDirField.hidden = processless;
      steamGameChooser.hidden = processless;
      sharedLaunchOptions.hidden = kind.value === "application";
      directory.disabled = kind.value === "application";
      path.placeholder =
        kind.value === "executable"
          ? "/opt/games/example"
          : "/Applications/Example.app";
    };
    kind.addEventListener("change", showKind);
    form.append(
      catalogEditorSection("Identity", "Name and describe this application in your library.",
        field("Name", name), field("Description", description),
        field("Tags", tags, "Separate tags with commas.")),
      catalogEditorDisclosure("Launch", "Choose how the host starts and tracks the application.",
        field("Launch type", kind), pathField, steamIdField, steamInstallDirField,
        steamGameChooser, fullscreen, sharedLaunchOptions, executable,
        field("Command output", outputMode, "Use default discards command output without saving an output value."),
        outputFile, rawPreparation,
        field("Skip global preparation commands", excludeGlobalPreparation,
          "Do not run the host-wide preparation commands for this application.")),
      catalogEditorDisclosure("Display", "Choose display and virtual-screen overrides.",
        field("Display output override", displayOutput,
          "Leave empty to inherit the next display selection. This is separate from the command output file."),
        field("Virtual screen request", virtualScreen,
          "Inherit the host setting or explicitly request or avoid a virtual screen."),
        field("Virtual display mode", virtualDisplayMode,
          "Inherit the host mode or explicitly disable, use per-client, or share a virtual display."),
        field("Virtual display layout", virtualDisplayLayout,
          "Inherit the host layout or select how virtual displays are arranged."),
        ...refreshRateOptionFields,
        field("Display and adapter policy (JSON)", displayPolicy,
          "Sparse application overrides. Use null or empty values to inherit; global-only keys are rejected."),
        field("10-bit SDR preference", prefer10BitSdr,
          "Inherit the next setting scope, or set an explicit preference for this application.")),
      catalogEditorDisclosure("Streaming and audio", "Override streaming, frame limiting, and audio for this application.",
        field("FEC percentage", fec, "Leave blank to inherit the global streaming setting; supported values are 1–255."),
        ...sessionPolicyFields,
        field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this application streams audio."),
        audioSink.controls, virtualSink.controls,
        field("Capture audio without changing default outputs", captureOnly,
          "Inherit the host setting, enable capture only, or allow Windows default-output routing.")),
      catalogEditorDisclosure("Input", "Set per-application controller, keyboard, and mouse preferences.",
        field("Back button timeout (milliseconds)", backButtonTimeout,
          "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."),
        field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."),
        field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."),
        field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."),
        field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."),
        ...inputCompatibilityFields, ...advancedInput.fields),
      catalogEditorDisclosure("Encoder", "Override software and NVENC encoder settings.",
        ...softwareEncoder.fields, ...nvencSettingsFields),
    );
    const identitySection = form.children[0] as HTMLElement;
    const launchSection = form.children[1] as HTMLElement;
    identitySection.id = "catalog-editor-identity";
    launchSection.id = "catalog-editor-launch";
    (launchSection as HTMLDetailsElement).open = !entry;
    const sections = Array.from(form.children).slice(2) as HTMLElement[];
    const overrides = catalogOverrideEditor(catalogOverrideSections(sections), { ...entry, ...entry?.config_overrides });
    for (const section of sections) section.remove();
    overrides.node.id = "catalog-editor-overrides";
    form.replaceChildren(
      catalogEditorNavigation([
        { label: "Identity", target: identitySection },
        { label: "Launch", target: launchSection },
        {
          label: "Settings overrides",
          target: overrides.node,
          reveal: overrides.node.querySelector<HTMLDetailsElement>("details"),
        },
      ]),
      identitySection,
      launchSection,
      overrides.node,
    );
    catalogRemovedOverrides.set(form, overrides.removed);
    revealInvalidEditorControls(form);
    showKind();
    return form;
  }
}
