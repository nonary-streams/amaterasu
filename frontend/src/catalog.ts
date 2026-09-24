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

  export async function readSteamArtwork(
    api: Api,
    reference: NonNullable<Entry["artwork"]>,
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
    const add = button("+ Add application", () => showEditor(), "primary");
    add.disabled = !api.allows("catalog.write");
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
        const grid = el("div", "catalog-grid");
        for (const row of reply.entries) grid.append(card(row));
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

    function card(row: CatalogRow): HTMLElement {
      const providerLaunch = row.entry.launch.kind === "playnite";
      const edit = button("Edit", () => showEditor(row));
      edit.disabled = !api.allows("catalog.write") || providerLaunch;
      if (providerLaunch) edit.title = "Playnite launch targets are managed by the provider.";
      const actions = [edit];
      if (providerLaunch) {
        const configure = button(
          "Streaming settings",
          () => showProviderOverrideEditor(row),
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
      if (row.entry.artwork?.provider === "steam" && api.allows("steam.read")) {
        void readSteamArtwork(api, row.entry.artwork).then((blob) => {
          if (!content.isConnected || !art.isConnected) return;
          const url = URL.createObjectURL(blob);
          const image = el("img") as HTMLImageElement;
          image.alt = "";
          image.addEventListener("load", () => URL.revokeObjectURL(url), { once: true });
          image.addEventListener("error", () => URL.revokeObjectURL(url), { once: true });
          image.src = url;
          art.prepend(image);
        }).catch(() => {
          // Keep the initials when local artwork is absent or changes mid-read.
        });
      }
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

    function showProviderOverrideEditor(row: CatalogRow): void {
      editor.replaceChildren(el("p", "muted", "Loading input profiles…"));
      void loadInputRuntimeStatus().then((status) => {
        if (context.content.isConnected) renderProviderOverrideEditor(row, status);
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

    function renderProviderOverrideEditor(row: CatalogRow, runtimeStatus: InputRuntimeStatus): void {
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
      const form = el(
        "form",
        "editor-form",
        field(
          "FEC percentage",
          fec,
          "Leave blank to inherit the global setting. Supported values are 1–255.",
        ),
        field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this game streams audio."),
        audioSink.controls,
        virtualSink.controls,
        field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."),
        field("Back button timeout (milliseconds)", backButtonTimeout, "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."),
        field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."),
        field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."),
        field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."),
        field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."),
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
          const config_overrides = providerConfigOverrides(fec.value, streamAudio.value, audioSink.mode.value, audioSink.id.value, captureOnly.value, backButtonTimeout.value, virtualSink.mode.value, virtualSink.selector.value, controller.value, gamepad.value, keyboard.value, mouse.value, runtimeStatus.recognized_profiles);
          try {
            const reply = await api.post<{
              id: string;
              revision: number;
              config_overrides: { fec_percentage?: number; stream_audio?: boolean; audio_sink?: string; virtual_sink?: string; audio_sink_capture_only?: boolean; back_button_timeout?: number; controller?: boolean; gamepad?: string; keyboard?: boolean; mouse?: boolean };
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

    function showEditor(row?: CatalogRow): void {
      void loadInputRuntimeStatus().then((runtimeStatus) => {
        if (context.content.isConnected) renderEditor(row, runtimeStatus);
      });
    }

    function renderEditor(row: CatalogRow | undefined, runtimeStatus: InputRuntimeStatus): void {
      const editRevision = revision;
      const form = entryForm(row?.entry, runtimeStatus);
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

  function entryForm(entry: Entry | undefined, runtimeStatus: InputRuntimeStatus): HTMLFormElement {
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
      field("Audio streaming", streamAudio, "Inherit the host setting, or choose whether this application streams audio."),
      audioSink.controls,
      virtualSink.controls,
      field("Capture audio without changing default outputs", captureOnly, "Inherit the host setting, enable capture only, or allow Windows default-output routing."),
      field("Back button timeout (milliseconds)", backButtonTimeout, "Leave blank to inherit. Use -1 to disable the timeout; other signed 32-bit values set the duration."),
      field("Controller input", controller, "Inherit the next scope, or enable/disable gamepad input for this application."),
      field("Gamepad profile", gamepad, "Inherit the next scope. Recognized profiles unavailable on this backend stay selectable and are labeled unavailable."),
      field("Keyboard input", keyboard, "Inherit the next scope, or enable/disable keyboard input for this application."),
      field("Mouse input", mouse, "Inherit the next scope, or enable/disable mouse input for this application."),
      steamIdField,
      steamInstallDirField,
      fullscreen,
      executable,
    );
    showKind();
    return form;
  }
}
