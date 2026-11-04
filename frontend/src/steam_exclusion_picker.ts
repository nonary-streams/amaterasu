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
    heading.textContent = "Choose excluded Steam games";
    const help = document.createElement("p");
    help.textContent = "Search discoveries and select games to exclude. Existing name-only or unavailable exclusions are preserved.";
    const search = document.createElement("input");
    search.type = "search";
    search.autocomplete = "off";
    search.setAttribute("aria-label", "Search Steam games");
    search.placeholder = "Search games";
    const status = document.createElement("p");
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    const results = document.createElement("div");
    results.setAttribute("aria-label", "Steam games");
    const more = document.createElement("button");
    more.type = "button";
    more.textContent = "Load more games";

    let draft = initial.map((entry) => ({ ...entry }));
    let revision: number | null = null;
    let after: number | null = null;
    let games: SteamDiscovery[] = [];
    let loading = false;
    let scanWarning = "";
    let loadError = "";
    let revisionChanged = false;

    function render(): void {
      const query = search.value.trim().toLocaleLowerCase();
      const visible = games.filter((game) => game.name.toLocaleLowerCase().includes(query));
      results.replaceChildren();

      if (!api.allows("steam.read")) {
        status.textContent = "Steam list unavailable: steam.read permission is required. Existing exclusions are unchanged.";
        more.hidden = true;
        return;
      }
      if (loading) {
        status.textContent = "Loading Steam games…";
      } else if (loadError) {
        status.textContent = loadError + " Your current draft is preserved.";
      } else if (revision === null) {
        status.textContent = "Steam game list has not been loaded.";
      } else {
        const unavailable = draft.filter((entry) =>
          entry.id && !games.some((game) => String(game.app_id) === entry.id),
        );
        status.textContent =
          visible.length + " matching loaded games · " + draft.length + " exclusions" +
          (unavailable.length
            ? " · " + unavailable.length + " existing ID exclusion(s) are not in this list and will be preserved"
            : "") +
          (revisionChanged ? " · The Steam list changed while paging; restarted at the latest revision." : "") +
          (scanWarning ? " · " + scanWarning : "");
      }

      const selected = new Set(draft.flatMap((entry) => entry.id ? [entry.id] : []));
      for (const game of visible) {
        const label = document.createElement("label");
        label.className = "steam-exclusion-option";
        const checkbox = document.createElement("input");
        checkbox.type = "checkbox";
        checkbox.checked = selected.has(String(game.app_id));
        checkbox.addEventListener("change", () => {
          const id = String(game.app_id);
          draft = draft.filter((entry) => entry.id !== id);
          if (checkbox.checked) draft.push({ id, name: game.name });
          onChange(draft.map((entry) => ({ ...entry })));
          render();
        });
        const name = document.createElement("span");
        name.textContent = game.name;
        const id = document.createElement("small");
        id.textContent = "Steam app " + game.app_id;
        label.append(checkbox, name, id);
        results.append(label);
      }
      if (revision !== null && visible.length === 0 && !loading) {
        results.append(document.createTextNode(
          query ? "No matching Steam games on loaded pages." : "No Steam discoveries are available.",
        ));
      }
      more.hidden = after === null || loading || !api.allows("steam.read");
      more.disabled = loading;
    }

    async function load(): Promise<void> {
      if (!api.allows("steam.read") || loading) return;
      loading = true;
      loadError = "";
      render();
      try {
        const reply = await api.post<SteamListReply>("steam/list", {
          ...(revision !== null ? { revision } : {}),
          ...(after !== null ? { after } : {}),
          limit: 50,
        });
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
          if (!Number.isSafeInteger(game.app_id) || game.app_id < 1 || typeof game.name !== "string") {
            throw new Error("Steam game list contains an invalid entry.");
          }
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
        loadError = error instanceof Error ? error.message : "Unable to load Steam games.";
      } finally {
        loading = false;
        render();
      }
    }

    search.addEventListener("input", render);
    more.addEventListener("click", () => { void load(); });
    root.append(heading, help, search, status, results, more);
    render();
    void load();
    return { node: root, value: () => draft.map((entry) => ({ ...entry })) };
  }
}