namespace Amaterasu {
  export interface SteamExclusion {
    id?: string;
    name?: string;
  }

  export interface SteamDiscovery {
    app_id: number;
    name: string;
    installed: boolean;
    install_directory?: string | null;
  }

  export interface SteamListReply {
    revision: number;
    games: SteamDiscovery[];
    next_after: number | null;
    scanning: boolean;
    complete: boolean;
  }

  export interface SteamExclusionPicker {
    node: HTMLElement;
    value(): SteamExclusion[];
    setValue(values: readonly SteamExclusion[]): void;
    dispose(): void;
  }

  function steamPickerMessage(source: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function"
      ? Amaterasu.localizedMessage(source) : source;
  }

  function steamPickerLiteral(source: string): TextValue {
    return typeof Amaterasu.literalText === "function"
      ? Amaterasu.literalText(source) : source;
  }

  function steamPickerSetText(target: Element, source: string): void {
    if (typeof Amaterasu.setLocalizedTextContent === "function")
      Amaterasu.setLocalizedTextContent(target, source);
    else target.textContent = source;
  }

  function steamPickerSetTextTemplate(
    target: Element,
    source: string,
    values: Readonly<Record<string, TextValue>>,
  ): void {
    const format = (locale: string): string => {
      let rendered = typeof Amaterasu.translateText === "function"
        ? Amaterasu.translateText(source) : source;
      const formatter = new Intl.NumberFormat(locale);
      for (const [name, value] of Object.entries(values)) {
        const raw = typeof value === "string" ? value : value.value;
        const formatted = /^\d+$/.test(raw) ? formatter.format(Number(raw)) : raw;
        rendered = rendered.replaceAll(`{${name}}`, () => formatted);
      }
      return rendered;
    };
    if (typeof Amaterasu.localeFormattedTextNode === "function") {
      target.replaceChildren(Amaterasu.localeFormattedTextNode(format));
    } else {
      steamPickerSetText(target, format("en"));
    }
  }

  function steamPickerSetAttribute(
    target: Element,
    attribute: "aria-label" | "placeholder" | "title",
    source: string,
  ): void {
    if (typeof Amaterasu.setLocalizedAttribute === "function")
      Amaterasu.setLocalizedAttribute(target, attribute, source);
    else target.setAttribute(attribute, source);
  }

  function steamPickerSetTemplate(
    target: Element,
    attribute: "aria-label",
    source: string,
    values: Readonly<Record<string, TextValue>>,
  ): void {
    if (typeof Amaterasu.setLocalizedAttributeTemplate === "function")
      Amaterasu.setLocalizedAttributeTemplate(target, attribute, source, values);
    else {
      let rendered = source;
      for (const [name, value] of Object.entries(values)) {
        const text = typeof value === "string" ? value : value.value;
        rendered = rendered.replaceAll(`{${name}}`, () => text);
      }
      target.setAttribute(attribute, rendered);
    }
  }

  /** Accessible, cursor-paginated chooser; persistence is controlled by the caller. */
  export function steamExclusionPicker(
    api: Pick<Api, "post" | "allows">,
    initial: readonly SteamExclusion[],
    onChange: (draft: SteamExclusion[]) => void = () => {},
  ): SteamExclusionPicker {
    const root = document.createElement("section");
    root.className = "steam-exclusion-picker";
    const heading = document.createElement("h3");
    steamPickerSetText(heading, "Choose excluded Steam games");
    const help = document.createElement("p");
    steamPickerSetText(help, "Search discoveries and select games to exclude. Existing name-only or unavailable exclusions are preserved.");
    const search = document.createElement("input");
    search.type = "search";
    search.autocomplete = "off";
    steamPickerSetAttribute(search, "aria-label", "Search Steam games");
    steamPickerSetAttribute(search, "placeholder", "Search games");
    const status = document.createElement("p");
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    const savedLabel = document.createElement("h4");
    steamPickerSetText(savedLabel, "Current exclusions");
    const saved = document.createElement("div");
    saved.className = "steam-exclusion-saved";
    steamPickerSetAttribute(saved, "aria-label", "Current Steam exclusions");
    const results = document.createElement("div");
    steamPickerSetAttribute(results, "aria-label", "Steam games");
    const more = document.createElement("button");
    more.type = "button";
    more.className = "button secondary";
    steamPickerSetText(more, "Load more games");
    const draftSignal = document.createElement("input");
    draftSignal.type = "text";
    draftSignal.hidden = true;
    draftSignal.tabIndex = -1;
    draftSignal.name = "steam_exclusion_draft";
    draftSignal.setAttribute("aria-hidden", "true");
    const lifecycle = new AbortController();

    let draft = initial.map((entry) => ({ ...entry }));
    let revision: number | null = null;
    let after: number | null = null;
    let games: SteamDiscovery[] = [];
    let loading = false;
    let scanWarning = "";
    let loadError = "";
    let revisionChanged = false;
    let revisionRetries = 0;
    let disposed = false;

    const MAX_EXCLUSIONS = 256;

    function signalDraft(): void {
      draftSignal.value = JSON.stringify(draft);
      draftSignal.dispatchEvent(new Event("input", { bubbles: true }));
    }
    signalDraft();

    function renderStatus(): void {
      if (!api.allows("steam.read")) {
        steamPickerSetText(status, "Steam discovery is unavailable without steam.read permission. Saved exclusions can still be edited.");
        more.hidden = true;
        return;
      }
      if (loading) {
        steamPickerSetText(status, "Loading Steam games…");
      } else if (loadError) {
        steamPickerSetText(status, "Steam games could not be loaded. Your current draft is preserved.");
        status.setAttribute("title", loadError);
      } else if (revision === null) {
        steamPickerSetText(status, "Steam game list has not been loaded.");
      } else {
        const matching = games.filter((game) => game.name.toLocaleLowerCase().includes(search.value.trim().toLocaleLowerCase())).length;
        steamPickerSetTextTemplate(status, "{matchingCount} matching loaded games · {exclusionCount} exclusions", {
          matchingCount: steamPickerLiteral(String(matching)),
          exclusionCount: steamPickerLiteral(String(draft.length)),
        });
        steamPickerSetTemplate(status, "aria-label", "{matchingCount} matching loaded games; {exclusionCount} exclusions", {
          matchingCount: steamPickerLiteral(String(matching)),
          exclusionCount: steamPickerLiteral(String(draft.length)),
        });
        const warning = scanWarning || (revisionChanged
          ? "The Steam list changed while paging; restarted at the latest revision."
          : "");
        if (warning) steamPickerSetAttribute(status, "title", warning);
      }
      more.hidden = (!loadError && after === null) || disposed || !api.allows("steam.read");
      more.disabled = loading || disposed;
      steamPickerSetText(more, loadError ? "Retry loading games" : "Load more games");
    }

    function renderSaved(): void {
      saved.replaceChildren();
      draft.forEach((entry, index) => {
        const row = document.createElement("div");
        row.className = "steam-exclusion-saved-item";
        const idInput = document.createElement("input");
        idInput.type = "text";
        idInput.inputMode = "numeric";
        idInput.value = entry.id ?? "";
        steamPickerSetAttribute(idInput, "aria-label", "Edit excluded Steam app ID");
        steamPickerSetAttribute(idInput, "placeholder", "Optional Steam app ID");
        idInput.addEventListener("input", () => {
          if (disposed) return;
          const id = idInput.value.trim();
          if (id) draft[index] = { ...draft[index], id };
          else {
            const current = draft[index];
            if (!current) return;
            const { id: _oldId, ...withoutId } = current;
            draft[index] = withoutId;
          }
          signalDraft();
          onChange(draft.map((value) => ({ ...value })));
          renderStatus();
        }, { signal: lifecycle.signal });
        const name = document.createElement("input");
        name.type = "text";
        name.value = entry.name ?? "";
        steamPickerSetAttribute(name, "aria-label", entry.id
          ? "Edit excluded Steam game name"
          : "Edit name-only Steam exclusion");
        name.addEventListener("input", () => {
          if (disposed) return;
          draft[index] = { ...draft[index], name: name.value };
          signalDraft();
          onChange(draft.map((value) => ({ ...value })));
          renderStatus();
        }, { signal: lifecycle.signal });
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "button secondary";
        steamPickerSetText(remove, "Remove exclusion");
        steamPickerSetTemplate(remove, "aria-label", "Remove exclusion {index}", {
          index: steamPickerLiteral(String(index + 1)),
        });
        remove.addEventListener("click", () => {
          if (disposed) return;
          draft = draft.filter((_, currentIndex) => currentIndex !== index);
          signalDraft();
          onChange(draft.map((value) => ({ ...value })));
          render();
        }, { signal: lifecycle.signal });
        row.append(idInput, name, remove);
        saved.append(row);
      });
    }

    function renderResults(): void {
      const query = search.value.trim().toLocaleLowerCase();
      const visible = games.filter((game) => game.name.toLocaleLowerCase().includes(query));
      results.replaceChildren();
      const selected = new Set(draft.flatMap((entry) => entry.id ? [entry.id] : []));
      for (const game of visible) {
        const label = document.createElement("label");
        label.className = "steam-exclusion-option";
        const checkbox = document.createElement("input");
        checkbox.type = "checkbox";
        checkbox.checked = selected.has(String(game.app_id));
        checkbox.addEventListener("change", () => {
          if (disposed) return;
          const id = String(game.app_id);
          draft = draft.filter((entry) => entry.id !== id);
          if (checkbox.checked) {
            if (draft.length >= MAX_EXCLUSIONS) {
              checkbox.checked = false;
              steamPickerSetText(status, "You can exclude up to 256 Steam games. Remove an exclusion before adding another.");
              return;
            }
            draft.push({ id, name: game.name });
          }
          signalDraft();
          onChange(draft.map((entry) => ({ ...entry })));
          render();
        }, { signal: lifecycle.signal });
        const name = document.createElement("span");
        name.textContent = game.name;
        const id = document.createElement("small");
        steamPickerSetTemplate(id, "aria-label", "Steam app {appId}", {
          appId: steamPickerLiteral(String(game.app_id)),
        });
        steamPickerSetTextTemplate(id, "Steam app {appId}", {
          appId: steamPickerLiteral(String(game.app_id)),
        });
        label.append(checkbox, name, id);
        results.append(label);
      }
      if (revision !== null && visible.length === 0 && !loading) {
        const emptyMessage = query
          ? "No matching Steam games on loaded pages."
          : "No Steam discoveries are available.";
        results.append(typeof Amaterasu.textValueNode === "function"
          ? Amaterasu.textValueNode(steamPickerMessage(emptyMessage))
          : emptyMessage);
      }
    }

    function render(): void {
      if (disposed) return;
      renderStatus();
      renderSaved();
      renderResults();
    }

    async function load(): Promise<void> {
      if (disposed || !api.allows("steam.read") || loading) return;
      loading = true;
      loadError = "";
      renderStatus();
      try {
        const reply = await api.post<SteamListReply>("steam/list", {
          ...(revision !== null ? { revision } : {}),
          ...(after !== null ? { after } : {}),
          limit: 32,
        });
        if (disposed) return;
        if (!Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
            !Array.isArray(reply.games) ||
            !(reply.next_after === null || Number.isSafeInteger(reply.next_after)) ||
            typeof reply.scanning !== "boolean" || typeof reply.complete !== "boolean") {
          throw new Error("Steam game list response is invalid.");
        }
        if (revision !== null && reply.revision !== revision) {
          // Cursors belong to one snapshot. Bound retries if the list keeps changing.
          if (revisionRetries >= 2) {
            throw new Error("Steam game list kept changing. Retry loading games to try again.");
          }
          revisionRetries += 1;
          revision = null;
          after = null;
          games = [];
          revisionChanged = true;
          loading = false;
          await load();
          return;
        }

        revisionRetries = 0;
        revision = reply.revision;
        const uniqueGames = new Map(games.map((game) => [game.app_id, game]));
        for (const game of reply.games) {
          if (!Number.isSafeInteger(game.app_id) || game.app_id < 1 || typeof game.name !== "string")
            throw new Error("Steam game list contains an invalid entry.");
          uniqueGames.set(game.app_id, game);
        }
        games = [...uniqueGames.values()];
        after = reply.next_after;
        scanWarning = reply.scanning
          ? "Steam scan is still in progress."
          : !reply.complete
            ? "Steam scan is incomplete; existing exclusions are preserved."
            : "";
      } catch (error) {
        if (disposed) return;
        const revisionConflict = typeof error === "object" && error !== null &&
          "code" in error && error.code === "revision_conflict";
        if (revisionConflict && revisionRetries < 2) {
          revisionRetries += 1;
          revision = null;
          after = null;
          games = [];
          revisionChanged = true;
          loading = false;
          await load();
          return;
        }
        loadError = revisionConflict
          ? "Steam game list kept changing. Retry loading games to try again."
          : error instanceof Error ? error.message : "Unable to load Steam games.";
      } finally {
        loading = false;
        if (!disposed) render();
      }
    }

    search.addEventListener("input", render, { signal: lifecycle.signal });
    more.addEventListener("click", () => {
      if (loadError) revisionRetries = 0;
      void load();
    }, { signal: lifecycle.signal });
    root.append(heading, help, savedLabel, saved, search, status, results, more, draftSignal);
    render();
    void load();
    return {
      node: root,
      value: () => draft.map((entry) => ({ ...entry })),
      setValue(values) {
        if (disposed) throw new Error("The Steam exclusion picker has been disposed.");
        if (!Array.isArray(values)) throw new Error("Steam exclusions must be a list.");
        draft = values.map((entry) => ({ ...entry }));
        signalDraft();
        render();
      },
      dispose() {
        if (disposed) return;
        disposed = true;
        lifecycle.abort();
        root.replaceChildren();
      },
    };
  }
}
