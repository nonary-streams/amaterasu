namespace Amaterasu {
  interface SteamArtworkChunk {
    ok: boolean;
    app_id: number;
    revision: string;
    offset: number;
    next_offset: number | null;
    total_bytes: number;
    mime_type: string;
    data: string;
  }

  interface LutrisArtworkChunk {
    ok: boolean;
    game_id: number;
    revision: string;
    offset: number;
    next_offset: number | null;
    total_bytes: number;
    mime_type: string;
    data: string;
  }

  export async function readSteamArtwork(
    api: Pick<Api, "post">,
    reference: Extract<NonNullable<Entry["artwork"]>, { provider: "steam" }>,
  ): Promise<Blob> {
    if (
      reference.provider !== "steam" ||
      !Number.isSafeInteger(reference.app_id) ||
      reference.app_id < 1 ||
      reference.app_id > 0xffff_ffff ||
      !/^[0-9]{1,20}$/.test(reference.revision)
    ) throw new Error("Invalid Steam artwork reference.");
    let offset = 0;
    let totalBytes: number | null = null;
    let mimeType: string | null = null;
    const chunks: ArrayBuffer[] = [];
    while (true) {
      const reply = await api.post<SteamArtworkChunk>("steam/artwork_read", {
        app_id: reference.app_id,
        revision: reference.revision,
        offset,
      });
      if (
        reply.ok !== true ||
        reply.app_id !== reference.app_id ||
        reply.revision !== reference.revision ||
        reply.offset !== offset ||
        !Number.isSafeInteger(reply.total_bytes) ||
        reply.total_bytes < 1 ||
        reply.total_bytes > 16 * 1024 * 1024 ||
        !["image/png", "image/jpeg", "image/webp"].includes(reply.mime_type) ||
        typeof reply.data !== "string" ||
        (totalBytes !== null && totalBytes !== reply.total_bytes) ||
        (mimeType !== null && mimeType !== reply.mime_type)
      ) throw new Error("Steam artwork changed during transfer.");
      totalBytes = reply.total_bytes;
      mimeType = reply.mime_type;
      const decoded = atob(reply.data);
      if (decoded.length < 1 || decoded.length > 8192 || offset + decoded.length > totalBytes)
        throw new Error("Invalid Steam artwork chunk.");
      const bytes = new Uint8Array(decoded.length);
      for (let index = 0; index < decoded.length; index++)
        bytes[index] = decoded.charCodeAt(index);
      chunks.push(bytes.buffer);
      const end = offset + decoded.length;
      if (reply.next_offset === null) {
        if (end !== totalBytes) throw new Error("Incomplete Steam artwork transfer.");
        return new Blob(chunks, { type: mimeType });
      }
      if (reply.next_offset !== end || end >= totalBytes || end % 8192 !== 0)
        throw new Error("Invalid Steam artwork cursor.");
      offset = end;
    }
  }

  export async function readLutrisArtwork(
    api: Pick<Api, "post">,
    reference: Extract<NonNullable<Entry["artwork"]>, { provider: "lutris" }>,
  ): Promise<Blob> {
    if (
      reference.provider !== "lutris" ||
      !Number.isSafeInteger(reference.game_id) ||
      reference.game_id < 1 ||
      !/^[0-9a-f]{64}$/.test(reference.revision)
    ) throw new Error("Invalid Lutris artwork reference.");
    let offset = 0;
    let totalBytes: number | null = null;
    let mimeType: string | null = null;
    const chunks: ArrayBuffer[] = [];
    while (true) {
      const reply = await api.post<LutrisArtworkChunk>("lutris/artwork_read", {
        game_id: reference.game_id,
        revision: reference.revision,
        offset,
      });
      if (
        reply.ok !== true ||
        reply.game_id !== reference.game_id ||
        reply.revision !== reference.revision ||
        reply.offset !== offset ||
        !Number.isSafeInteger(reply.total_bytes) ||
        reply.total_bytes < 1 ||
        reply.total_bytes > 16 * 1024 * 1024 ||
        !["image/png", "image/jpeg", "image/webp"].includes(reply.mime_type) ||
        typeof reply.data !== "string" ||
        reply.data.length > 10924 ||
        (totalBytes !== null && totalBytes !== reply.total_bytes) ||
        (mimeType !== null && mimeType !== reply.mime_type)
      ) throw new Error("Lutris artwork changed during transfer.");
      totalBytes = reply.total_bytes;
      mimeType = reply.mime_type;
      const decoded = atob(reply.data);
      if (decoded.length < 1 || decoded.length > 8192 || offset + decoded.length > totalBytes)
        throw new Error("Invalid Lutris artwork chunk.");
      const bytes = new Uint8Array(decoded.length);
      for (let index = 0; index < decoded.length; index++)
        bytes[index] = decoded.charCodeAt(index);
      chunks.push(bytes.buffer);
      const end = offset + decoded.length;
      if (reply.next_offset === null) {
        if (end !== totalBytes) throw new Error("Incomplete Lutris artwork transfer.");
        return new Blob(chunks, { type: mimeType });
      }
      if (reply.next_offset !== end || end >= totalBytes || end % 8192 !== 0)
        throw new Error("Invalid Lutris artwork cursor.");
      offset = end;
    }
  }

  export function showCatalogArtwork(
    api: Pick<Api, "post" | "allows">,
    reference: Entry["artwork"],
    art: HTMLElement,
    content: HTMLElement,
  ): void {
    if (!reference) return;
    if (reference.provider === "steam" && !api.allows("steam.read")) return;
    if (reference.provider === "lutris" && !api.allows("lutris.read")) return;
    const imageRead = reference.provider === "steam"
      ? readSteamArtwork(api, reference)
      : readLutrisArtwork(api, reference);
    void imageRead.then((blob) => {
      if (!content.isConnected || !art.isConnected) return;
      const url = URL.createObjectURL(blob);
      const image = el("img") as HTMLImageElement;
      image.alt = "";
      let released = false;
      const observer = new MutationObserver(() => {
        if (!content.isConnected || !art.isConnected) release();
      });
      const release = (): void => {
        if (released) return;
        released = true;
        observer.disconnect();
        URL.revokeObjectURL(url);
      };
      image.addEventListener("load", release, { once: true });
      image.addEventListener("error", release, { once: true });
      observer.observe(document, { childList: true, subtree: true });
      image.src = url;
      art.prepend(image);
    }).catch(() => {
      // Keep initials when a provider image is missing or its revision changes.
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

  const refreshRateOptionValues = ["auto", "disabled", "manual", "prefer_highest"] as const;
  const catalogAutoVirtualFramegenModes = ["enabled", "vrr", "legacy", "disabled"] as const;

  export type CatalogFrameLimiterCapabilityName =
    | "frame_limiter_provider"
    | "rtss_frame_limit_type";

  function catalogSavedUnsupportedEnumValueToken(values: readonly string[]): string {
    let token = "__amaterasu_keep_saved_unsupported_enum_value__";
    while (values.includes(token)) token += "_";
    return token;
  }

  export function catalogFrameLimiterOverrideOptionValues(
    name: CatalogFrameLimiterCapabilityName,
    capabilities: CatalogConfigOverrideCapabilities,
    current: string | null | undefined,
  ): string[] {
    const values = capabilities[name];
    const options = ["", ...values];
    if (current != null && !values.includes(current))
      options.push(catalogSavedUnsupportedEnumValueToken(values));
    return options;
  }

  export function catalogFrameLimiterOverrideIsDisabled(
    name: CatalogFrameLimiterCapabilityName,
    capabilities: CatalogConfigOverrideCapabilities,
  ): boolean {
    return name === "rtss_frame_limit_type" && capabilities.rtss_frame_limit_type.length === 0;
  }

  function catalogFrameLimiterCapabilitySelect(
    name: CatalogFrameLimiterCapabilityName,
    current: string | null | undefined,
    capabilities: CatalogConfigOverrideCapabilities,
  ): HTMLElement {
    const values = capabilities[name];
    const unsupportedCurrent = current != null && !values.includes(current);
    const savedValueToken = unsupportedCurrent
      ? catalogSavedUnsupportedEnumValueToken(values)
      : undefined;
    const select = el("select");
    select.name = name;
    const inherit = el("option", "", "Inherit");
    inherit.value = "";
    select.append(inherit);
    for (const value of values) {
      const option = el("option", "", value);
      option.value = value;
      select.append(option);
    }
    if (savedValueToken !== undefined) {
      const saved = el("option", "", `Keep saved value: ${current} (unsupported on this host)`);
      saved.value = savedValueToken;
      select.append(saved);
    }
    select.value = savedValueToken ?? current ?? "";

    if (!catalogFrameLimiterOverrideIsDisabled(name, capabilities)) return select;

    select.disabled = true;
    select.removeAttribute("name");
    const preservedValue = input(
      name,
      savedValueToken ?? "",
      "hidden",
    );
    const controls: HTMLElement[] = [
      select,
      preservedValue,
      el("span", "muted", "Unsupported on this host."),
    ];
    if (savedValueToken !== undefined) {
      const clear = button("Clear override", () => {
        preservedValue.value = "";
        select.value = "";
        clear.disabled = true;
      }, "quiet");
      clear.type = "button";
      controls.push(clear);
    }
    return el("div", "catalog-enum-override", ...controls);
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
      const option = el("option", "", value);
      option.value = value;
      select.append(option);
    }
    if (current != null && !values.includes(current)) {
      const saved = el("option", "", `Saved value: ${current}`);
      saved.value = current;
      select.append(saved);
    }
    select.value = current ?? "";
    return select;
  }

  export type CatalogFrameLimiterOverrides = Pick<
    NonNullable<Entry["config_overrides"]>,
    | "frame_limiter_enable"
    | "frame_limiter_fps_limit"
    | "frame_limiter_provider"
    | "frame_limiter_disable_vsync"
    | "frame_limiter_auto_virtual_framegen"
    | "rtss_frame_limit_type"
    | "ds4_back_as_touchpad_click"
  >;

  type CatalogFrameLimiterOverrideControlValues = Record<
    keyof CatalogFrameLimiterOverrides,
    string
  >;

  function catalogFrameLimiterOverrideField(
    label: string,
    control: HTMLElement,
    hint: string,
  ): HTMLElement {
    if (control instanceof HTMLSelectElement) return field(label, control, hint);
    control.querySelector("select")?.setAttribute("aria-label", label);
    return el(
      "div",
      "field",
      el("span", "field-label", label),
      control,
      el("small", "muted", hint),
    );
  }

  export function catalogFrameLimiterOverrideFormValues(
    overrides: Entry["config_overrides"] | undefined,
  ): CatalogFrameLimiterOverrideControlValues {
    const text = (value: boolean | number | string | null | undefined) =>
      value == null ? "" : String(value);
    return {
      frame_limiter_enable: text(overrides?.frame_limiter_enable),
      frame_limiter_fps_limit: text(overrides?.frame_limiter_fps_limit),
      frame_limiter_provider: text(overrides?.frame_limiter_provider),
      frame_limiter_disable_vsync: text(overrides?.frame_limiter_disable_vsync),
      frame_limiter_auto_virtual_framegen: text(overrides?.frame_limiter_auto_virtual_framegen),
      rtss_frame_limit_type: text(overrides?.rtss_frame_limit_type),
      ds4_back_as_touchpad_click: text(overrides?.ds4_back_as_touchpad_click),
    };
  }

  function catalogFrameLimiterOverrideFields(
    overrides: Entry["config_overrides"] | undefined,
    capabilities: CatalogConfigOverrideCapabilities,
  ): HTMLElement[] {
    const values = catalogFrameLimiterOverrideFormValues(overrides);
    const enable = audioOverrideSelect(
      "frame_limiter_enable",
      values.frame_limiter_enable === "" ? undefined : values.frame_limiter_enable === "true",
    );
    const fpsLimit = input("frame_limiter_fps_limit", values.frame_limiter_fps_limit, "number");
    fpsLimit.min = "0";
    fpsLimit.max = "1000";
    fpsLimit.step = "0.001";
    fpsLimit.placeholder = "Inherit";
    const provider = catalogFrameLimiterCapabilitySelect(
      "frame_limiter_provider",
      values.frame_limiter_provider,
      capabilities,
    );
    const disableVsync = audioOverrideSelect(
      "frame_limiter_disable_vsync",
      values.frame_limiter_disable_vsync === ""
        ? undefined
        : values.frame_limiter_disable_vsync === "true",
    );
    const autoVirtualFramegen = catalogEnumOverrideSelect(
      "frame_limiter_auto_virtual_framegen",
      values.frame_limiter_auto_virtual_framegen,
      catalogAutoVirtualFramegenModes,
    );
    const rtssFrameLimitType = catalogFrameLimiterCapabilitySelect(
      "rtss_frame_limit_type",
      values.rtss_frame_limit_type,
      capabilities,
    );
    const ds4BackAsTouchpadClick = audioOverrideSelect(
      "ds4_back_as_touchpad_click",
      values.ds4_back_as_touchpad_click === ""
        ? undefined
        : values.ds4_back_as_touchpad_click === "true",
    );
    return [
      field("Frame limiter", enable, "Blank inherits. Runtime application remains pending."),
      field("Frame limiter FPS limit", fpsLimit, "Blank inherits. Use 0–1000 Hz in 0.001 Hz steps."),
      catalogFrameLimiterOverrideField(
        "Frame limiter provider",
        provider,
        "Choose a provider supported by this host; blank inherits.",
      ),
      field("Disable VSync for RTSS", disableVsync, "Blank inherits the next scope."),
      field(
        "Auto virtual frame generation mode",
        autoVirtualFramegen,
        "Blank inherits. Choose enabled, vrr, legacy, or disabled.",
      ),
      catalogFrameLimiterOverrideField(
        "RTSS frame limit type",
        rtssFrameLimitType,
        "Blank inherits. Available RTSS choices depend on host support.",
      ),
      field(
        "DS4 Back as touchpad click",
        ds4BackAsTouchpadClick,
        "Only applies to a DS4 profile. Blank inherits; explicit false is preserved.",
      ),
    ];
  }

  function parseCatalogFrameLimiterEnum<T extends string>(
    value: string,
    allowedValues: readonly T[],
    currentValue: string | null | undefined,
    label: string,
  ): T | null {
    if (value === "") return null;
    if (
      currentValue != null &&
      !allowedValues.includes(currentValue as T) &&
      value === catalogSavedUnsupportedEnumValueToken(allowedValues)
    ) return currentValue as T;
    if (!allowedValues.includes(value as T))
      throw new Error(`${label} must be one of the supported values.`);
    return value as T;
  }

  function parseCatalogFrameLimiterFpsLimit(value: string): number | null {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    if (
      !/^(?:[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+))(?:[eE][+-]?[0-9]+)?$/.test(trimmed)
    )
      throw new Error(
        "Frame limiter FPS limit must be from 0 to 1000 in 0.001 Hz steps.",
      );
    const fpsLimit = Number(trimmed);
    if (!Number.isFinite(fpsLimit) || fpsLimit < 0 || fpsLimit > 1000)
      throw new Error(
        "Frame limiter FPS limit must be from 0 to 1000 in 0.001 Hz steps.",
      );
    const millihertz = fpsLimit * 1000;
    const roundingTolerance = Number.EPSILON * Math.max(Math.abs(millihertz), 1) * 4;
    if (Math.abs(millihertz - Math.round(millihertz)) > roundingTolerance)
      throw new Error(
        "Frame limiter FPS limit must be from 0 to 1000 in 0.001 Hz steps.",
      );
    return fpsLimit;
  }

  export function parseCatalogFrameLimiterOverrides(
    data: FormData,
    capabilities: CatalogConfigOverrideCapabilities,
    currentOverrides?: Entry["config_overrides"],
  ): CatalogFrameLimiterOverrides {
    const text = (name: keyof CatalogFrameLimiterOverrides) =>
      String(data.get(name) ?? "");
    return {
      frame_limiter_enable: Amaterasu.parseInputBooleanOverride(
        text("frame_limiter_enable"),
        "Frame limiter",
      ),
      frame_limiter_fps_limit: parseCatalogFrameLimiterFpsLimit(
        text("frame_limiter_fps_limit"),
      ),
      frame_limiter_provider: parseCatalogFrameLimiterEnum(
        text("frame_limiter_provider"),
        capabilities.frame_limiter_provider,
        currentOverrides?.frame_limiter_provider,
        "Frame limiter provider",
      ),
      frame_limiter_disable_vsync: Amaterasu.parseInputBooleanOverride(
        text("frame_limiter_disable_vsync"),
        "Disable VSync for RTSS",
      ),
      frame_limiter_auto_virtual_framegen: parseCatalogFrameLimiterEnum(
        text("frame_limiter_auto_virtual_framegen"),
        catalogAutoVirtualFramegenModes,
        undefined,
        "Auto virtual frame generation mode",
      ),
      rtss_frame_limit_type: parseCatalogFrameLimiterEnum(
        text("rtss_frame_limit_type"),
        capabilities.rtss_frame_limit_type,
        currentOverrides?.rtss_frame_limit_type,
        "RTSS frame limit type",
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
      const option = el("option", "", choice.label);
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
    selector.placeholder = "Exact ID or device name";
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

  export function renderCatalog(context: PageContext): void {
    const { content, api, notify } = context;
    const search = input("search");
    search.type = "search";
    search.placeholder = "Search names, descriptions, tags";
    search.setAttribute("aria-label", "Search catalog");
    const searchForm = el("form", "search-form", search);
    const searchButton = el("button", "button secondary", "Search");
    searchButton.type = "submit";
    searchForm.append(searchButton);
    const results = el("div");
    const editor = el("div");
    const status = el("div");
    let query = "";
    let revision = 0;
    let cursors: unknown[] = [null];
    let page = 0;
    let generation = 0;
    let configOverrideCapabilities: CatalogConfigOverrideCapabilities | null = null;
    const add = button("+ Add application", () => {
      if (configOverrideCapabilities) showEditor(undefined, configOverrideCapabilities);
    }, "primary");
    add.disabled = true;
    content.append(
      sectionHeading(
        "Your library",
        "A durable catalog of the applications on your host.",
        add,
      ),
      searchForm,
      status,
      editor,
      results,
    );

    async function load(): Promise<void> {
      const current = ++generation;
      results.setAttribute("aria-busy", "true");
      try {
        const reply = await api.post<CatalogReply>(
          query ? "catalog/search" : "catalog/list",
          { ...(query ? { query } : {}), cursor: cursors[page], limit: 12 },
        );
        if (!content.isConnected || current !== generation) return;
        revision = reply.revision;
        configOverrideCapabilities = reply.config_override_capabilities;
        add.disabled = !api.allows("catalog.write");
        const grid = el("div", "catalog-grid");
        for (const row of reply.entries)
          grid.append(card(row, reply.config_override_capabilities));
        results.replaceChildren(
          el(
            "div",
            "section-meta",
            el("span", "", `${reply.entries.length} applications on this page`),
            badge(`Revision ${revision}`),
          ),
          reply.entries.length
            ? grid
            : empty(
                query
                  ? "No matches on this page"
                  : "Make room for your favorites",
                reply.next_cursor
                  ? "Search continues on the next page."
                  : query
                    ? "Try a different name or tag."
                    : "Add your first application to start building this host’s library.",
              ),
          pager(
            page
              ? () => {
                  page--;
                  void load();
                }
              : null,
            reply.next_cursor != null
              ? () => {
                  cursors = cursors.slice(0, page + 1);
                  cursors.push(reply.next_cursor);
                  page++;
                  void load();
                }
              : null,
            `Page ${page + 1}`,
          ),
        );
      } catch (error) {
        if (content.isConnected && current === generation)
          results.replaceChildren(errorPanel(error, reset));
      } finally {
        results.removeAttribute("aria-busy");
      }
    }

    function reset(): void {
      cursors = [null];
      page = 0;
      void load();
    }
    searchForm.addEventListener("submit", (event) => {
      event.preventDefault();
      query = search.value.trim();
      reset();
    });

    function card(
      row: CatalogRow,
      capabilities: CatalogConfigOverrideCapabilities,
    ): HTMLElement {
      const providerLaunch = row.entry.launch.kind === "playnite";
      const edit = button("Edit", () => showEditor(row, capabilities));
      edit.disabled = !api.allows("catalog.write") || providerLaunch;
      if (providerLaunch) edit.title = "Playnite launch targets are managed by the provider.";
      const actions = [edit];
      if (providerLaunch) {
        const configure = button(
          "Streaming settings",
          () => showProviderOverrideEditor(row, capabilities),
          "secondary",
        );
        configure.disabled = !api.allows("catalog.write");
        actions.unshift(configure);
      }
      const remove = button(
        "Delete",
        async () => {
          if (
            !(await confirmAction(
              `Delete ${row.entry.name}?`,
              "This removes its catalog entry. It does not remove files or stop a running application.",
              "Delete entry",
            ))
          )
            return;
          await runAction(remove, status, async () => {
            await api.post("catalog/delete", {
              id: row.id,
              expected_revision: revision,
            });
            notify("Application removed from the catalog.");
            reset();
          });
        },
        "quiet danger-text",
      );
      remove.disabled = !api.allows("catalog.write");
      actions.push(remove);
      const art = el(
        "div",
        "app-art",
        el("span", "", row.entry.name.slice(0, 2).toUpperCase()),
        badge(
          row.entry.launch.kind === "executable"
            ? "Executable"
            : providerLaunch
              ? "Playnite"
              : "Application",
        ),
      );
      showCatalogArtwork(api, row.entry.artwork, art, content);
      return el(
        "article",
        "app-card",
        art,
        el(
          "div",
          "app-details",
          el("h2", "", row.entry.name),
          el(
            "p",
            "muted description",
            row.entry.description || "No description added.",
          ),
          el("div", "tags", ...(row.entry.tags ?? []).map((tag) => badge(tag))),
          el("p", "path", launchPath(row.entry)),
          el("div", "card-footer", ...actions),
        ),
      );
    }

    function showProviderOverrideEditor(
      row: CatalogRow,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      editor.replaceChildren(el("p", "muted", "Loading input profiles…"));
      void loadInputRuntimeStatus().then((status) => {
        if (context.content.isConnected)
          renderProviderOverrideEditor(row, status, capabilities);
      });
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
      fec.setAttribute("aria-label", `FEC percentage override for ${row.entry.name}`);
      const frameLimiterFields = catalogFrameLimiterOverrideFields(
        row.entry.config_overrides,
        capabilities,
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
      backButtonTimeout.setAttribute("aria-label", `Back button timeout override for ${row.entry.name}`);
      const controller = audioOverrideSelect("controller", row.entry.config_overrides?.controller);
      const gamepad = inputGamepadOverrideSelect("gamepad", row.entry.config_overrides?.gamepad, runtimeStatus);
      const keyboard = audioOverrideSelect("keyboard", row.entry.config_overrides?.keyboard);
      const mouse = audioOverrideSelect("mouse", row.entry.config_overrides?.mouse);
      const advancedInput = advancedInputOverrideControls(row.entry.config_overrides);
      const refreshRateOptionFields = catalogRefreshRateOptionFields(
        row.entry.config_overrides?.dd_refresh_rate_option,
      );
      const form = el(
        "form",
        "editor-form",
        field(
          "FEC percentage",
          fec,
          "Leave blank to inherit the global setting. Supported values are 1–255.",
        ),
        ...frameLimiterFields,
        ...refreshRateOptionFields,
        field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this game streams audio."),
        audioSink.controls,
        virtualSink.controls,
        field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."),
        field("Back button timeout (milliseconds)", backButtonTimeout, "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."),
        field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."),
        field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."),
        field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."),
        field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."),
        ...advancedInput.fields,
      );
      const feedback = el("div");
      const save = el("button", "button primary", "Save streaming settings");
      save.type = "submit";
      const cancel = button("Cancel", () => editor.replaceChildren());
      form.append(feedback, el("div", "actions", save, cancel));
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!form.reportValidity()) return;
        void runAction(save, feedback, async () => {
          const config_overrides = {
            ...advancedInput.read(),
            ...providerConfigOverrides(fec.value, streamAudio.value, audioSink.mode.value, audioSink.id.value, captureOnly.value, backButtonTimeout.value, virtualSink.mode.value, virtualSink.selector.value, controller.value, gamepad.value, keyboard.value, mouse.value, runtimeStatus.recognized_profiles),
            ...parseCatalogFrameLimiterOverrides(
              new FormData(form),
              capabilities,
              row.entry.config_overrides,
            ),
            dd_refresh_rate_option: parseCatalogRefreshRateOptionOverride(new FormData(form)),
          };
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
              } & CatalogFrameLimiterOverrides;
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
              editor.replaceChildren();
              await load();
              notify(
                `Streaming settings for ${row.entry.name} changed elsewhere. The latest value was reloaded; reopen the editor to review it.`,
              );
              return;
            }
            throw error;
          }
        });
      });
      editor.replaceChildren(
        panel(
          "Playnite streaming settings",
          el("p", "muted", `${row.entry.name} · Playnite game ${row.entry.launch.kind === "playnite" ? row.entry.launch.game_id : ""}`),
          el(
            "p",
            "muted",
            "The Playnite game identity and provider-managed launch metadata stay unchanged.",
          ),
          form,
        ),
      );
      editor.scrollIntoView({ behavior: "smooth", block: "start" });
      fec.focus();
    }

    function showEditor(
      row: CatalogRow | undefined,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      void loadInputRuntimeStatus().then((runtimeStatus) => {
        if (context.content.isConnected)
          renderEditor(row, runtimeStatus, capabilities);
      });
    }

    function renderEditor(
      row: CatalogRow | undefined,
      runtimeStatus: InputRuntimeStatus,
      capabilities: CatalogConfigOverrideCapabilities,
    ): void {
      const editRevision = revision;
      const form = entryForm(row?.entry, runtimeStatus, capabilities);
      const feedback = el("div");
      const save = el(
        "button",
        "button primary",
        row ? "Save changes" : "Add application",
      );
      save.type = "submit";
      const cancel = button("Cancel", () => editor.replaceChildren());
      form.append(feedback, el("div", "actions", save, cancel));
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (!form.reportValidity()) return;
        void runAction(save, feedback, async () => {
          const entry = entryFromForm(new FormData(form), runtimeStatus.recognized_profiles);
          entry.config_overrides = {
            ...(entry.config_overrides ?? {}),
            ...parseCatalogFrameLimiterOverrides(
              new FormData(form),
              capabilities,
              row?.entry.config_overrides,
            ),
          };
          await api.post(row ? "catalog/update" : "catalog/create", {
            ...(row ? { id: row.id } : {}),
            entry,
            expected_revision: editRevision,
          });
          editor.replaceChildren();
          notify(row ? "Application updated." : "Application added.");
          reset();
        });
      });
      editor.replaceChildren(
        panel(
          row ? "Edit application" : "Add application",
          el(
            "p",
            "muted",
            "Save launch metadata for this host. Adding an entry does not launch it.",
          ),
          form,
        ),
      );
      editor.scrollIntoView({ behavior: "smooth", block: "start" });
      form.querySelector("input")?.focus();
    }
    void load();
  }

  function entryForm(
    entry: Entry | undefined,
    runtimeStatus: InputRuntimeStatus,
    capabilities: CatalogConfigOverrideCapabilities,
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
    const directory = input(
      "working_directory",
      entry?.launch.kind === "executable"
        ? (entry.launch.working_directory ?? "")
        : "",
    );
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
      JSON.stringify(executableLaunch?.preparation ?? []),
      "hidden",
    );
    const fec = input(
      "fec_percentage",
      entry?.config_overrides?.fec_percentage?.toString() ?? "",
      "number",
    );
    fec.min = "1";
    fec.max = "255";
    fec.step = "1";
    const frameLimiterFields = catalogFrameLimiterOverrideFields(
      entry?.config_overrides,
      capabilities,
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
    const refreshRateOptionFields = catalogRefreshRateOptionFields(
      entry?.config_overrides?.dd_refresh_rate_option,
    );
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
      String(entry?.launch.kind === "playnite_fullscreen" ? entry.launch.exit_timeout_seconds : 10),
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
      field("Working directory", directory, "Optional absolute path."),
      processPolicy,
      preparation,
    );
    const pathField = field("Absolute application path", path);
    const steamIdField = field("Steam app ID", steamId, "Optional canonical Steam app ID for provider-aware process tracking.");
    const steamInstallDirField = field("Steam install directory", steamInstallDir, "Optional absolute game directory; Linux tracks only newly launched processes inside it.");
    const fullscreen = field("Exit timeout (seconds)", fullscreenTimeout, "Graceful Playnite Fullscreen close window, from 0 to 300 seconds.");
    const showKind = () => {
      executable.hidden = kind.value !== "executable";
      elevated.disabled = kind.value !== "executable";
      const isFullscreen = kind.value === "playnite_fullscreen";
      fullscreen.hidden = !isFullscreen;
      pathField.hidden = isFullscreen;
      path.required = !isFullscreen;
      steamIdField.hidden = isFullscreen;
      steamInstallDirField.hidden = isFullscreen;
      path.placeholder =
        kind.value === "executable"
          ? "/opt/games/example"
          : "/Applications/Example.app";
    };
    kind.addEventListener("change", showKind);
    form.append(
      el("div", "form-grid", field("Name", name), field("Launch type", kind)),
      field("Description", description),
      field("Tags", tags, "Separate tags with commas."),
      field(
        "Skip global preparation commands",
        excludeGlobalPreparation,
        "Do not run the host-wide preparation commands for this application.",
      ),
      pathField,
      field("FEC percentage", fec, "Leave blank to inherit the global streaming setting; supported values are 1–255."),
      ...frameLimiterFields,
      ...refreshRateOptionFields,
      field(
        "10-bit SDR preference",
        prefer10BitSdr,
        "Inherit the next setting scope, or set an explicit preference for this application.",
      ),
      field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this application streams audio."),
      audioSink.controls,
      virtualSink.controls,
      field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."),
      field("Back button timeout (milliseconds)", backButtonTimeout, "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."),
      field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."),
      field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."),
      field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."),
      field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."),
      ...advancedInput.fields,
      steamIdField,
      steamInstallDirField,
      fullscreen,
      executable,
    );
    showKind();
    return form;
  }
}
