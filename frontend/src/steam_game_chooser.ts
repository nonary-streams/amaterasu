namespace Amaterasu {
  function steamChooserMessage(value: string): TextValue {
    return typeof Amaterasu.localizedMessage === "function"
      ? Amaterasu.localizedMessage(value)
      : value;
  }

  function steamChooserSetText(target: Element, value: string): void {
    if (typeof Amaterasu.setLocalizedTextContent === "function")
      Amaterasu.setLocalizedTextContent(target, value);
    else
      target.textContent = value;
  }

  function steamChooserSetFormattedText(
    target: Element,
    source: string,
    values: Readonly<Record<string, string>>,
  ): void {
    if (typeof Amaterasu.localeFormattedTextNode === "function" &&
        typeof Amaterasu.translateText === "function") {
      const node = Amaterasu.localeFormattedTextNode(() => {
        let rendered = Amaterasu.translateText(source);
        for (const [name, value] of Object.entries(values))
          rendered = rendered.replaceAll(`{${name}}`, () => value);
        return rendered;
      });
      target.replaceChildren(node);
    } else {
      let rendered = source;
      for (const [name, value] of Object.entries(values))
        rendered = rendered.replaceAll(`{${name}}`, () => value);
      target.textContent = rendered;
    }
  }

  function steamChooserSetAttribute(target: Element, attribute: "aria-label", value: string): void {
    if (typeof Amaterasu.setLocalizedAttribute === "function")
      Amaterasu.setLocalizedAttribute(target, attribute, value);
    else
      target.setAttribute(attribute, value);
  }

  function steamChooserSetAttributeTemplate(
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
        const literal = typeof value === "string" ? value : value.value;
        rendered = rendered.replaceAll(`{${name}}`, () => literal);
      }
      target.setAttribute(attribute, rendered);
    }
  }

  export interface SteamChooserGame {
    app_id: number;
    name: string;
    install_directory?: string;
  }

  export interface SteamChooserReply {
    revision: number;
    games: SteamChooserGame[];
    next_after: number | null;
    scanning: boolean;
    complete: boolean;
  }

  export type SteamChooserApi = Pick<Api, "allows" | "post">;
  export type SteamDirectoryChangeChoice = "keep" | "clear" | "review";

  const steamChooserPageSize = 16;
  const steamChooserSearchMaxPages = 32;

  function validateSteamChooserReply(value: unknown): SteamChooserReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Steam game discovery returned an invalid response.");
    const reply = value as Record<string, unknown>;
    if (!Number.isSafeInteger(reply.revision) || (reply.revision as number) < 0 ||
        !Array.isArray(reply.games) || reply.games.length > steamChooserPageSize ||
        (reply.next_after !== null && !Number.isSafeInteger(reply.next_after)) ||
        typeof reply.scanning !== "boolean" || typeof reply.complete !== "boolean")
      throw new Error("Steam game discovery returned an invalid response.");
    const seen = new Set<number>();
    const games = reply.games.map((value): SteamChooserGame => {
      if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("Steam game discovery returned an invalid game.");
      const game = value as Record<string, unknown>;
      const directory = typeof game.install_directory === "string" &&
        game.install_directory.trim() !== "" ? game.install_directory : undefined;
      if (!Number.isSafeInteger(game.app_id) || (game.app_id as number) <= 0 ||
          typeof game.name !== "string" || !game.name.trim() ||
          seen.has(game.app_id as number) ||
          (game.install_directory !== undefined && typeof game.install_directory !== "string"))
        throw new Error("Steam game discovery returned an invalid game.");
      seen.add(game.app_id as number);
      return {
        app_id: game.app_id as number,
        name: game.name,
        ...(directory === undefined ? {} : { install_directory: directory }),
      };
    });
    const nextAfter = reply.next_after as number | null;
    if (nextAfter !== null && nextAfter < 0)
      throw new Error("Steam game discovery returned an invalid cursor.");
    return {
      revision: reply.revision as number,
      games,
      next_after: nextAfter,
      scanning: reply.scanning,
      complete: reply.complete,
    };
  }

  function steamChooserSetFocus(target: HTMLElement): void {
    if (target.isConnected) target.focus();
  }

  /**
   * Browse or search Steam discovery. Search is bounded to 32 pages of the
   * revision-aware steam/list API. Cancel, abort, and late replies never mutate
   * caller-owned application fields.
   */
  export function chooseSteamGame(
    api: SteamChooserApi,
    signal?: AbortSignal,
  ): Promise<SteamChooserGame | null> {
    return new Promise((resolve) => {
      if (signal?.aborted) {
        resolve(null);
        return;
      }
      const opener = document.activeElement as HTMLElement | null;
      const dialog = el("dialog", "steam-game-chooser-dialog");
      steamChooserSetAttribute(dialog, "aria-label", "Choose a Steam game");
      const status = el("p", "steam-game-chooser-status");
      status.setAttribute("role", "status");
      status.setAttribute("aria-live", "polite");
      const list = el("ul", "steam-game-chooser-list");
      steamChooserSetAttribute(list, "aria-label", "Steam games");
      const searchInput = input("steam_game_search");
      searchInput.type = "search";
      searchInput.maxLength = 128;
      steamChooserSetAttribute(searchInput, "aria-label", "Search discovered games");
      if (typeof Amaterasu.setLocalizedAttribute === "function")
        Amaterasu.setLocalizedAttribute(searchInput, "placeholder", "Search by game name or app ID");
      else
        searchInput.placeholder = "Search by game name or app ID";
      const searchButton = button(steamChooserMessage("Search"), () => void searchGames());
      const searchForm = el("form", "steam-game-chooser-search",
        field(steamChooserMessage("Search discovered games"), searchInput),
        searchButton);
      const previous = button(steamChooserMessage("← Previous"), () => void showPage(pageIndex - 1));
      const next = button(steamChooserMessage("Next →"), () => void showPage(pageIndex + 1), "primary");
      const restart = button(steamChooserMessage("Restart"), () => void restartDiscovery());
      restart.hidden = true;
      const clearSearch = button(steamChooserMessage("Clear search"), () => {
        searchInput.value = "";
        activeSearchTerm = "";
        activeRequestController?.abort();
        activeRequestController = null;
        operationToken += 1;
        loading = false;
        searching = false;
        pageIndex = 0;
        cursors.length = 1;
        list.replaceChildren();
        restart.hidden = true;
        void showPage(0);
      });
      clearSearch.hidden = true;
      const cancel = button(steamChooserMessage("Cancel"), () => finish(null));
      steamChooserSetAttribute(cancel, "aria-label", "Cancel");
      const pagination = el("div", "actions steam-game-chooser-pagination", previous, next);
      const recovery = el("div", "actions steam-game-chooser-recovery", restart, clearSearch);
      const heading = el("h2", "", steamChooserMessage("Choose a Steam game"));
      heading.id = "steam-game-chooser-title";
      dialog.setAttribute("aria-labelledby", heading.id);
      dialog.append(
        heading,
        el("p", "muted", steamChooserMessage("Select a discovered game to use its Steam app ID and optional install directory.")),
        searchForm,
        status,
        list,
        pagination,
        recovery,
        el("div", "actions", cancel),
      );

      let settled = false;
      let loading = false;
      let searching = false;
      let revision: number | undefined;
      let pageIndex = 0;
      let activeSearchTerm = "";
      let cursors: (number | null)[] = [null];
      let operationToken = 0;
      let activeRequestController: AbortController | null = null;
      const beginOperation = () => {
        activeRequestController?.abort();
        const controller = new AbortController();
        activeRequestController = controller;
        const token = ++operationToken;
        const abortWithEditor = (): void => controller.abort();
        signal?.addEventListener("abort", abortWithEditor, { once: true });
        return {
          token,
          requestSignal: controller.signal,
          cleanup: () => signal?.removeEventListener("abort", abortWithEditor),
        };
      };
      const finish = (selection: SteamChooserGame | null): void => {
        if (settled) return;
        settled = true;
        activeRequestController?.abort();
        activeRequestController = null;
        operationToken += 1;
        signal?.removeEventListener("abort", onAbort);
        dialog.close();
        dialog.remove();
        if (opener) steamChooserSetFocus(opener);
        resolve(selection);
      };
      const onAbort = (): void => finish(null);
      const updatePager = (hasNext: boolean): void => {
        previous.disabled = loading || searching || pageIndex === 0;
        next.disabled = loading || searching || !hasNext;
        pagination.hidden = searching || (pageIndex === 0 && !hasNext);
        searchInput.disabled = loading || !api.allows("steam.read");
        searchButton.disabled = loading || !api.allows("steam.read");
        restart.disabled = loading;
        clearSearch.hidden = !activeSearchTerm;
      };
      const showRestart = (message: string): void => {
        list.replaceChildren();
        steamChooserSetText(status, message);
        restart.hidden = false;
        updatePager(false);
      };
      const permissionLost = (): void => {
        list.replaceChildren();
        steamChooserSetText(status, "Steam read permission is required. You can enter the app ID manually.");
        restart.hidden = true;
        updatePager(false);
      };
      const renderGames = (games: readonly SteamChooserGame[]): void => {
        list.replaceChildren();
        for (const game of games) {
          const select = button(steamChooserMessage("Select"), () => finish(game));
          steamChooserSetAttributeTemplate(select, "aria-label", "Select {name}", {
            name: typeof Amaterasu.literalText === "function"
              ? Amaterasu.literalText(game.name)
              : game.name,
          });
          const directory = game.install_directory;
          list.append(el("li", "steam-game-chooser-item",
            el("div", "steam-game-chooser-copy",
              el("strong", "", typeof Amaterasu.literalText === "function"
                ? Amaterasu.literalText(game.name)
                : game.name),
              el("span", "steam-game-chooser-details",
                steamChooserMessage("App ID"), " ", String(game.app_id),
                " · ",
                steamChooserMessage(directory ? "Install directory" : "Install directory unavailable"),
                ...(directory ? [" ", typeof Amaterasu.literalText === "function"
                  ? Amaterasu.literalText(directory)
                  : directory] : []))),
            select));
        }
      };
      const showPage = async (requestedPage: number): Promise<void> => {
        if (settled || loading || searching || requestedPage < 0 ||
            requestedPage > pageIndex + 1) return;
        if (!api.allows("steam.read")) {
          permissionLost();
          return;
        }
        loading = true;
        searching = false;
        const operation = beginOperation();
        pageIndex = requestedPage;
        const cursor = cursors[pageIndex] ?? null;
        // A failed reload must not leave a previously discovered next cursor usable.
        cursors.length = pageIndex + 1;
        list.replaceChildren();
        restart.hidden = true;
        steamChooserSetText(status, "Loading Steam games…");
        updatePager(false);
        try {
          const raw = await api.post<SteamChooserReply>("steam/list", {
            ...(revision !== undefined ? { revision } : {}),
            ...(cursor !== null ? { after: cursor } : {}),
            limit: steamChooserPageSize,
          }, true, operation.requestSignal);
          if (settled || signal?.aborted || operation.token !== operationToken) return;
          if (!api.allows("steam.read")) {
            permissionLost();
            return;
          }
          const reply = validateSteamChooserReply(raw);
          if (revision !== undefined && reply.revision !== revision) {
            showRestart("Steam games changed. Restart to load the current list.");
            return;
          }
          revision = reply.revision;
          if (reply.next_after !== null &&
              ((cursor !== null && reply.next_after <= cursor) ||
                cursors.slice(0, pageIndex + 1).includes(reply.next_after))) {
            showRestart("Steam pagination failed. Restart to continue.");
            return;
          }
          if (reply.next_after !== null) cursors[pageIndex + 1] = reply.next_after;
          else cursors.length = pageIndex + 1;
          renderGames(reply.games);
          steamChooserSetText(status, reply.games.length
            ? "Choose a discovered game."
            : reply.complete ? "No Steam games are available."
              : reply.scanning ? "Steam is scanning. Refresh later to see discovered games."
                : "No Steam games are available.");
          updatePager(reply.next_after !== null);
        } catch (error) {
          if (settled || signal?.aborted || operation.token !== operationToken) return;
          if (!api.allows("steam.read")) {
            permissionLost();
            return;
          }
          if (error instanceof ApiError && error.code === "revision_conflict") {
            showRestart("Steam games changed. Restart to load the current list.");
            return;
          }
          showRestart("Steam games are unavailable. Retry or enter the app ID manually.");
        } finally {
          operation.cleanup();
          if (operation.token === operationToken) {
            loading = false;
            activeRequestController = null;
            if (!settled && !signal?.aborted)
              updatePager(cursors[pageIndex + 1] !== undefined);
          }
        }
      };
      const searchGames = async (): Promise<void> => {
        if (settled || loading || !searchInput.value.trim()) {
          if (!settled && !searchInput.value.trim())
            steamChooserSetText(status, "Enter a game name or app ID.");
          return;
        }
        if (!api.allows("steam.read")) {
          permissionLost();
          return;
        }
        const query = searchInput.value.trim().toLocaleLowerCase();
        activeSearchTerm = query;
        loading = true;
        searching = true;
        const operation = beginOperation();
        list.replaceChildren();
        restart.hidden = true;
        clearSearch.hidden = false;
        steamChooserSetText(status, "Searching Steam games…");
        updatePager(false);
        const matches: SteamChooserGame[] = [];
        const seenCursors = new Set<number>();
        let cursor: number | null = null;
        let pagesRead = 0;
        let entriesChecked = 0;
        let complete = false;
        try {
          while (pagesRead < steamChooserSearchMaxPages) {
            if (settled || signal?.aborted || operation.token !== operationToken) return;
            if (!api.allows("steam.read")) {
              permissionLost();
              return;
            }
            const reply = validateSteamChooserReply(await api.post<SteamChooserReply>("steam/list", {
              ...(revision !== undefined ? { revision } : {}),
              ...(cursor !== null ? { after: cursor } : {}),
              limit: steamChooserPageSize,
            }, true, operation.requestSignal));
            if (settled || signal?.aborted || operation.token !== operationToken) return;
            if (!api.allows("steam.read")) {
              permissionLost();
              return;
            }
            if (revision !== undefined && reply.revision !== revision) {
              showRestart("Steam games changed. Restart to search the current list.");
              return;
            }
            revision = reply.revision;
            pagesRead += 1;
            entriesChecked += reply.games.length;
            matches.push(...reply.games.filter((game) =>
              game.name.toLocaleLowerCase().includes(query) ||
              String(game.app_id).includes(query)));
            if (reply.next_after === null) {
              complete = true;
              break;
            }
            if ((cursor !== null && reply.next_after <= cursor) ||
                seenCursors.has(reply.next_after)) {
              showRestart("Steam pagination failed. Restart to continue.");
              return;
            }
            seenCursors.add(reply.next_after);
            cursor = reply.next_after;
          }
          renderGames(matches);
          if (matches.length) {
            if (complete) steamChooserSetText(status, "Search complete. Choose a discovered game.");
            else steamChooserSetFormattedText(status,
              "Found matches within {count} checked games. Browse later pages or refine your search.",
              { count: String(entriesChecked) });
          } else {
            if (complete) steamChooserSetText(status, "No Steam games matched.");
            else steamChooserSetFormattedText(status,
              "Checked {count} games across 32 pages. Browse later pages or refine your search.",
              { count: String(entriesChecked) });
          }
          restart.hidden = true;
          updatePager(false);
        } catch (error) {
          if (settled || signal?.aborted || operation.token !== operationToken) return;
          if (!api.allows("steam.read")) {
            permissionLost();
            return;
          }
          if (error instanceof ApiError && error.code === "revision_conflict") {
            showRestart("Steam games changed. Restart to search the current list.");
            return;
          }
          showRestart("Steam games are unavailable. Retry or enter the app ID manually.");
        } finally {
          operation.cleanup();
          if (operation.token === operationToken) {
            loading = false;
            activeRequestController = null;
            if (!settled && !signal?.aborted) updatePager(false);
          }
        }
      };
      const restartDiscovery = async (): Promise<void> => {
        if (settled || loading) return;
        revision = undefined;
        pageIndex = 0;
        cursors = [null];
        const query = activeSearchTerm;
        activeSearchTerm = "";
        restart.hidden = true;
        if (query) {
          searchInput.value = query;
          await searchGames();
        } else {
          await showPage(0);
        }
      };
      searchForm.addEventListener("submit", (event) => {
        event.preventDefault();
        void searchGames();
      });
      dialog.addEventListener("cancel", (event) => {
        event.preventDefault();
        finish(null);
      });
      signal?.addEventListener("abort", onAbort, { once: true });
      if (signal?.aborted) {
        finish(null);
        return;
      }
      document.body.append(dialog);
      dialog.showModal();
      cancel.focus();
      if (api.allows("steam.read")) void showPage(0);
      else permissionLost();
    });
  }

  export function chooseSteamDirectoryChange(
    signal?: AbortSignal,
  ): Promise<SteamDirectoryChangeChoice | null> {
    return new Promise((resolve) => {
      if (signal?.aborted) {
        resolve(null);
        return;
      }
      const opener = document.activeElement as HTMLElement | null;
      const dialog = el("dialog", "steam-game-chooser-dialog");
      steamChooserSetAttribute(dialog, "aria-label", "Review Steam directory");
      const heading = el("h2", "", steamChooserMessage("Review Steam directory"));
      heading.id = "steam-directory-review-title";
      dialog.setAttribute("aria-labelledby", heading.id);
      const status = el("p", "muted",
        steamChooserMessage("The selected game has no install directory. Your current directory may belong to another game."));
      status.setAttribute("role", "status");
      status.setAttribute("aria-live", "polite");
      const keep = button(steamChooserMessage("Keep current directory"), () => finish("keep"));
      const clear = button(steamChooserMessage("Clear current directory"), () => finish("clear"), "danger");
      const review = button(steamChooserMessage("Review fields"), () => finish("review"));
      steamChooserSetAttribute(review, "aria-label", "Review fields");
      dialog.append(heading, status, el("p", "muted",
        steamChooserMessage("Choose whether to keep or clear the current directory, or review fields without applying.")),
        el("div", "actions", review, keep, clear));
      let settled = false;
      const finish = (choice: SteamDirectoryChangeChoice | null): void => {
        if (settled) return;
        settled = true;
        signal?.removeEventListener("abort", onAbort);
        dialog.close();
        dialog.remove();
        if (opener) steamChooserSetFocus(opener);
        resolve(choice);
      };
      const onAbort = (): void => finish(null);
      dialog.addEventListener("cancel", (event) => {
        event.preventDefault();
        finish("review");
      });
      signal?.addEventListener("abort", onAbort, { once: true });
      if (signal?.aborted) {
        finish(null);
        return;
      }
      document.body.append(dialog);
      dialog.showModal();
      review.focus();
    });
  }
}
