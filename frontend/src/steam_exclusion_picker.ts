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

  function steamPickerSetAttribute(
    target: Element,
    attribute: "aria-label" | "placeholder",
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
    let disposed = false;

    function signalDraft(): void {
      draftSignal.value = JSON.stringify(draft);
      draftSignal.dispatchEvent(new Event("input", { bubbles: true }));
    }
    signalDraft();

    function renderStatus(): void {
      if (!api.allows("steam.read")) {
        steamPickerSetText(status, "Steam list unavailable: steam.read permission is required. Existing exclusions are unchanged.");
        more.hidden = true;
        return;
      }
      if (loading) {
        steamPickerSetText(status, "Loading Steam games…");
      } else if (loadError) {
        steamPickerSetText(status, `${loadError} Your current draft is preserved.`);
      } else if (revision === null) {
        steamPickerSetText(status, "Steam game list has not been loaded.");
      } else {
        const unavailable = draft.filter((entry) =>
          entry.id && !games.some((game) => String(game.app_id) === entry.id),
        );
        steamPickerSetText(status,
          `${games.filter((game) => game.name.toLocaleLowerCase().includes(search.value.trim().toLocaleLowerCase())).length} matching loaded games · ${draft.length} exclusions` +
          (unavailable.length
            ? ` · ${unavailable.length} existing ID exclusion(s) are not in this list and will be preserved`
            : "") +
          (revisionChanged ? " · The Steam list changed while paging; restarted at the latest revision." : "") +
          (scanWarning ? ` · ${scanWarning}` : ""));
      }
      more.hidden = after === null || loading || disposed || !api.allows("steam.read");
      more.disabled = loading || disposed;
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
          if (checkbox.checked) draft.push({ id, name: game.name });
          signalDraft();
          onChange(draft.map((entry) => ({ ...entry })));
          renderStatus();
        }, { signal: lifecycle.signal });
        const name = document.createElement("span");
        name.textContent = game.name;
        const id = document.createElement("small");
        steamPickerSetTemplate(id, "aria-label", "Steam app {appId}", {
          appId: steamPickerLiteral(String(game.app_id)),
        });
        id.textContent = `Steam app ${game.app_id}`;
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
          limit: 50,
        });
        if (disposed) return;
        if (!Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
            !Array.isArray(reply.games) ||
            !(reply.next_after === null || Number.isSafeInteger(reply.next_after)) ||
            typeof reply.scanning !== "boolean" || typeof reply.complete !== "boolean") {
          throw new Error("Steam game list response is invalid.");
        }
        if (revision !== null && reply.revision !== revision) {
          // Cursors belong to one snapshot. Discard pages, but retain the dirty draft.
          revision = null;
          after = null;
          games = [];
          revisionChanged = true;
          loading = false;
          await load();
          return;
        }

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
        loadError = error instanceof Error ? error.message : "Unable to load Steam games.";
      } finally {
        loading = false;
        if (!disposed) render();
      }
    }

    search.addEventListener("input", render, { signal: lifecycle.signal });
    more.addEventListener("click", () => { void load(); }, { signal: lifecycle.signal });
    root.append(heading, help, search, status, results, more, draftSignal);
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
