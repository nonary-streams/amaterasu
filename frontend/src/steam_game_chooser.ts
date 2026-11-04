namespace Amaterasu {
  export interface SteamChooserGame { app_id: number; name: string; install_directory?: string; }
  export interface SteamChooserReply {
    revision: number; games: SteamChooserGame[]; next_after: number | null; scanning: boolean; complete: boolean;
  }
  export type SteamChooserApi = Pick<Api, "allows" | "post">;
  const steamChooserPageSize = 16;

  function validateSteamChooserReply(value: unknown): SteamChooserReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Steam game discovery returned an invalid response.");
    const reply = value as Record<string, unknown>;
    if (!Number.isSafeInteger(reply.revision) || !Array.isArray(reply.games) ||
        (reply.next_after !== null && !Number.isSafeInteger(reply.next_after)) ||
        typeof reply.scanning !== "boolean" || typeof reply.complete !== "boolean")
      throw new Error("Steam game discovery returned an invalid response.");
    const seen = new Set<number>();
    const games = reply.games.map((value): SteamChooserGame => {
      if (!value || typeof value !== "object" || Array.isArray(value))
        throw new Error("Steam game discovery returned an invalid game.");
      const game = value as Record<string, unknown>;
      if (!Number.isSafeInteger(game.app_id) || (game.app_id as number) <= 0 ||
          typeof game.name !== "string" || !game.name.trim() ||
          seen.has(game.app_id as number) ||
          (game.install_directory !== undefined && typeof game.install_directory !== "string"))
        throw new Error("Steam game discovery returned an invalid game.");
      seen.add(game.app_id as number);
      return { app_id: game.app_id as number, name: game.name,
        ...(typeof game.install_directory === "string" ? { install_directory: game.install_directory } : {}) };
    });
    const nextAfter = reply.next_after as number | null;
    if (nextAfter !== null && nextAfter < 0) throw new Error("Steam game discovery returned an invalid cursor.");
    return { revision: reply.revision as number, games, next_after: nextAfter,
      scanning: reply.scanning, complete: reply.complete };
  }

  /**
   * Resolves only after an explicit selection; cancel and permission/unavailability
   * leave all caller-owned form fields untouched. The caller applies only the
   * returned Steam fields, preserving manual entry and other editor drafts.
   */
  export function chooseSteamGame(api: SteamChooserApi): Promise<SteamChooserGame | null> {
    return new Promise((resolve) => {
      const dialog = el("dialog", "steam-game-chooser-dialog");
      dialog.setAttribute("aria-label", "Choose a Steam game");
      const status = el("p", "steam-game-chooser-status");
      status.setAttribute("role", "status");
      const list = el("ul", "steam-game-chooser-list");
      const previous = button("← Previous", () => void showPage(pageIndex - 1));
      const next = button("Next →", () => void showPage(pageIndex + 1), "primary");
      const cancel = button("Cancel", () => finish(null));
      const pagination = el("div", "actions steam-game-chooser-pagination", previous, next);
      dialog.append(el("h2", "", "Choose a Steam game"),
        el("p", "muted", "Select a discovered game to use its Steam app ID and optional install directory."),
        status, list, pagination, el("div", "actions", cancel));

      let settled = false;
      let loading = false;
      let revision: number | undefined;
      let pageIndex = 0;
      const cursors: (number | null)[] = [null];
      const finish = (selection: SteamChooserGame | null): void => {
        if (settled) return;
        settled = true;
        dialog.close();
        dialog.remove();
        resolve(selection);
      };
      const updatePager = (hasNext: boolean): void => {
        previous.disabled = loading || pageIndex === 0;
        next.disabled = loading || !hasNext;
        pagination.hidden = pageIndex === 0 && !hasNext;
      };
      async function showPage(requestedPage: number): Promise<void> {
        if (settled || loading || requestedPage < 0 || requestedPage > pageIndex + 1) return;
        if (!api.allows("steam.read")) {
          status.textContent = "Steam discovery requires steam.read permission. You can enter a Steam app ID manually.";
          list.replaceChildren();
          updatePager(false);
          return;
        }
        loading = true;
        pageIndex = requestedPage;
        const cursor = cursors[pageIndex] ?? null;
        list.replaceChildren();
        status.textContent = "Loading discovered Steam games…";
        updatePager(false);
        try {
          const raw = await api.post<SteamChooserReply>("steam/list", {
            ...(revision !== undefined ? { revision } : {}),
            ...(cursor !== null ? { after: cursor } : {}),
            limit: steamChooserPageSize,
          });
          if (settled) return;
          const reply = validateSteamChooserReply(raw);
          if (revision !== undefined && reply.revision !== revision)
            throw new Error("Steam discovery changed while browsing. Close the chooser and try again.");
          revision = reply.revision;
          if (reply.next_after !== null && cursor !== null && reply.next_after <= cursor)
            throw new Error("Steam game discovery returned an invalid cursor.");
          if (reply.next_after !== null) cursors[pageIndex + 1] = reply.next_after;
          else cursors.length = pageIndex + 1;
          for (const game of reply.games) {
            const select = button("Select", () => finish(game));
            select.setAttribute("aria-label", `Select ${game.name}`);
            const directory = game.install_directory || "Install directory unavailable";
            list.append(el("li", "steam-game-chooser-item",
              el("div", "steam-game-chooser-copy", el("strong", "", game.name),
                el("span", "steam-game-chooser-details", `App ID ${game.app_id} · ${directory}`)),
              select));
          }
          status.textContent = reply.games.length
            ? `Page ${pageIndex + 1} · ${reply.games.length} games`
            : reply.complete ? "No Steam games were discovered."
              : reply.scanning ? "Steam is still scanning. Refresh the chooser later to see discovered games."
                : "No Steam games are available yet.";
          updatePager(reply.next_after !== null);
        } catch (error) {
          if (settled) return;
          const code = error instanceof ApiError ? error.code : "";
          status.textContent = code === "forbidden" || code === "permission_denied"
            ? "Steam discovery requires steam.read permission. You can enter a Steam app ID manually."
            : `Steam games are unavailable: ${error instanceof Error ? error.message : String(error)}`;
          list.replaceChildren();
          updatePager(false);
        } finally {
          loading = false;
          if (!settled) updatePager(cursors[pageIndex + 1] !== undefined);
        }
      }
      dialog.addEventListener("cancel", (event) => { event.preventDefault(); finish(null); });
      document.body.append(dialog);
      dialog.showModal();
      cancel.focus();
      if (api.allows("steam.read")) void showPage(0);
      else {
        status.textContent = "Steam discovery requires steam.read permission. You can enter a Steam app ID manually.";
        updatePager(false);
      }
    });
  }
}
