namespace Amaterasu {
  type ProviderName = "steam" | "lutris" | "playnite";
  export interface ProviderSummary {
    state: string;
    tone: "" | "good" | "bad";
    detail: string;
    detailTemplate?: string;
    detailValues?: Record<string, number>;
  }

  function providerRecord(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("Provider status is invalid.");
    const record = value as Record<string, unknown>;
    if (record.ok !== true) throw new Error("Provider status is unavailable.");
    return record;
  }

  function providerBoolean(record: Record<string, unknown>, key: string): boolean {
    if (typeof record[key] !== "boolean") throw new Error("Provider status is invalid.");
    return record[key] as boolean;
  }

  function providerCount(record: Record<string, unknown>, key: string): number {
    const count = record[key];
    if (typeof count !== "number" || !Number.isSafeInteger(count) || count < 0)
      throw new Error("Provider status is invalid.");
    return count;
  }

  export function summarizeProvider(name: ProviderName, value: unknown): ProviderSummary {
    const record = providerRecord(value);
    if (name === "steam" && (record.settings_ready === undefined || record.enabled === undefined))
      return { state: "Unverified", tone: "", detail: "The host did not report Steam provider settings." };
    const ready = providerBoolean(record, "settings_ready");
    const enabled = providerBoolean(record, "enabled");
    if (!ready) return { state: "Loading settings", tone: "", detail: "Provider settings are not ready yet." };
    if (!enabled) return { state: "Disabled", tone: "", detail: "This provider is disabled in host settings." };
    if (name === "steam") {
      const count = providerCount(record, "game_count");
      const scanning = providerBoolean(record, "scanning");
      const complete = providerBoolean(record, "complete");
      const warnings = providerCount(record, "warning_count");
      return {
        state: scanning ? "Scanning" : complete ? "Scan complete" : "Awaiting scan",
        tone: "",
        detail: `${count} discovered games · ${warnings} scan warnings.`,
        detailTemplate: "Discovered games: {count}. Scan warnings: {warnings}.",
        detailValues: { count, warnings },
      };
    }
    if (name === "lutris") {
      if (!providerBoolean(record, "platform_supported"))
        return { state: "Unsupported", tone: "", detail: "Lutris discovery is available on Linux hosts." };
      const database = providerBoolean(record, "database_available");
      const executable = providerBoolean(record, "executable_available");
      const available = providerBoolean(record, "available");
      if (!available)
        return { state: "Needs setup", tone: "", detail: !database || !executable
          ? "The Lutris database or executable is unavailable to the host."
          : "No usable Lutris database and executable installation pair was found." };
      const count = providerCount(record, "game_count");
      const pending = providerBoolean(record, "refresh_pending");
      const lastError = record.last_error;
      if (lastError !== null && typeof lastError !== "string") throw new Error("Provider status is invalid.");
      return {
        state: lastError ? "Scan issue" : pending ? "Scanning" : "Available",
        tone: lastError ? "bad" : "",
        detail: lastError ? `Last scan issue: ${lastError}` : `${count} discovered games.`,
        ...(!lastError ? { detailTemplate: "Discovered games: {count}.", detailValues: { count } } : {}),
      };
    }
    const running = providerBoolean(record, "playnite_running");
    const count = providerCount(record, "snapshot_game_count");
    const pending = providerBoolean(record, "snapshot_pending");
    const lastError = record.last_error;
    if (lastError !== null && typeof lastError !== "string") throw new Error("Provider status is invalid.");
    return {
      state: lastError ? "Sync issue" : pending ? "Syncing" : running ? "Running" : "Not detected",
      tone: lastError ? "bad" : "",
      detail: lastError ? `Last sync issue: ${lastError}` : `${count} games in the last snapshot.`,
      ...(!lastError ? { detailTemplate: "Games in the last snapshot: {count}.", detailValues: { count } } : {}),
    };
  }

  export function renderIntegrations(context: PageContext): void {
    const area = el("div", "integration-grid");
    const providers: { name: ProviderName; label: string; description: string; scope: string }[] = [
      { name: "steam", label: "Steam", description: "Discover games from local Steam libraries.", scope: "steam.read" },
      { name: "lutris", label: "Lutris", description: "Discover games from the current user's Lutris library.", scope: "configuration.read" },
      { name: "playnite", label: "Playnite", description: "Sync a Playnite library and provider-managed applications.", scope: "configuration.read" },
    ];
    let generation = 0;
    const activeRefreshes = new Set<ProviderName>();
    type Card = {
      provider: typeof providers[number];
      status: HTMLElement;
      feedback: HTMLElement;
      card: HTMLElement;
      refreshButton: HTMLButtonElement | null;
      providerStatus: Record<string, unknown> | null;
      setProviderStatus(value: unknown): void;
    };
    let currentCards: Card[] = [];
    const rtxHdrArea = el("div");
    const refreshStatuses = button(localizedMessage("Refresh provider statuses"), () => load(true));
    if (typeof Amaterasu.setLocalizedAttribute === "function")
      Amaterasu.setLocalizedAttribute(refreshStatuses, "aria-label", "Refresh provider statuses");
    context.content.append(sectionHeading("Integrations", "Connect and manage integrations on this host.", refreshStatuses), rtxHdrArea, area);
    async function load(announceReadback = false): Promise<void> {
      const request = ++generation;
      refreshStatuses.disabled = true;
      renderRtxHdrIntegration(context, rtxHdrArea);
      const cards = providers.map((provider): Card => {
        const status = el("div", "integration-status", el("p", "muted", localizedMessage("Loading provider status...")));
        const feedback = el("div", "integration-action-status");
        feedback.setAttribute("role", "status");
        feedback.setAttribute("aria-live", "polite");
        feedback.setAttribute("aria-atomic", "true");
        const settings = button(localizedMessage("Configure"), () => context.navigate("settings/integrations"));
        settings.disabled = !context.api.allows("configuration.read");
        const actions = el("div", "actions", settings);
        let refreshButton: HTMLButtonElement | null = null;
        if (provider.name === "steam" && context.api.allows("steam.read")) {
          actions.append(button(localizedMessage("Manage Steam library"), () => context.navigate("steam")));
          if (context.api.allows("steam.refresh")) {
            refreshButton = button(localizedMessage("Refresh now"), () => refreshProvider("steam"));
            if (typeof Amaterasu.setLocalizedAttributeTemplate === "function")
              Amaterasu.setLocalizedAttributeTemplate(
                refreshButton,
                "aria-label",
                "Request a {provider} refresh",
                { provider: literalText(provider.label) },
              );
            refreshButton.disabled = true;
            actions.append(refreshButton);
          }
        } else if (provider.name !== "steam") {
          refreshButton = button(localizedMessage("Refresh now"), () => refreshProvider(provider.name as "lutris" | "playnite"));
          if (typeof Amaterasu.setLocalizedAttributeTemplate === "function")
            Amaterasu.setLocalizedAttributeTemplate(
              refreshButton,
              "aria-label",
              "Request a {provider} refresh",
              { provider: literalText(provider.label) },
            );
          refreshButton.disabled = true;
          refreshButton.hidden = !canRequestRefresh(provider.name);
          actions.append(refreshButton);
        }
        const card = el("section", "panel integration-card", el("h2", "", provider.label),
          el("p", "muted", localizedMessage(provider.description)), status, actions, feedback);
        const item: Card = {
          provider, status, feedback, card, refreshButton, providerStatus: null,
          setProviderStatus(value: unknown) {
            item.providerStatus = value && typeof value === "object" && !Array.isArray(value)
              ? value as Record<string, unknown> : null;
            if (refreshButton) refreshButton.disabled = !canRefresh(provider.name, item.providerStatus) ||
              !canRequestRefresh(provider.name);
          },
        };
        return item;
      });
      currentCards = cards;
      area.replaceChildren(...cards.map((item) => item.card));
      try {
        await Promise.all(cards.map(async ({ provider, status, setProviderStatus }) => {
          if (!context.api.allows(provider.scope)) {
            status.replaceChildren(badge("Unavailable"), el("p", "muted",
              localizedMessage("This account cannot read this provider's status.")));
            return;
          }
          try {
            const value = await context.api.post<unknown>(`${provider.name}/status`);
            if (!context.content.isConnected || request !== generation) return;
            const summary = summarizeProvider(provider.name, value);
            showProviderStatus(status, summary);
            setProviderStatus(value);
            if (announceReadback && !activeRefreshes.has(provider.name))
              cards.find((item) => item.provider.name === provider.name)?.feedback.replaceChildren(
                el("span", "", localizedMessage("Provider status read completed; no provider refresh was requested.")),
              );
          } catch (error) {
            if (context.content.isConnected && request === generation) {
              status.replaceChildren(errorPanel(error));
              if (announceReadback)
                cards.find((item) => item.provider.name === provider.name)?.feedback.replaceChildren(
                  el("span", "", localizedMessage("Provider status check failed.")),
                  el("span", "", literalText(error instanceof Error ? error.message : String(error))),
                );
            }
          }
        }));
      } finally {
        if (context.content.isConnected && request === generation) refreshStatuses.disabled = false;
      }
    }
    function showProviderStatus(target: HTMLElement, summary: ProviderSummary): void {
      const observedAt = new Date();
      const detail = summary.tone === "bad"
        ? literalText(summary.detail)
        : summary.detailTemplate
          ? providerCountTemplate(summary.detailTemplate, summary.detailValues ?? {})
          : localizedMessage(summary.detail);
      const timestamp = el("time", "", literalText(observedAt.toISOString()));
      timestamp.dateTime = observedAt.toISOString();
      const observed = el("p", "muted", localizedMessage("Status observed"), " ", timestamp);
      target.replaceChildren(badge(localizedMessage(summary.state), summary.tone), el("p", "muted", detail), observed);
    }

    function providerCountTemplate(template: string, values: Record<string, number>): Node | string {
      const format = (locale: string): string => {
        let message = typeof Amaterasu.translateText === "function"
          ? Amaterasu.translateText(template) : template;
        for (const [key, value] of Object.entries(values))
          message = message.replaceAll(`{${key}}`, new Intl.NumberFormat(locale).format(value));
        return message;
      };
      return typeof Amaterasu.localeFormattedTextNode === "function"
        ? Amaterasu.localeFormattedTextNode(format)
        : format("en");
    }

    function currentCard(item: Card): boolean {
      return context.content.isConnected && item.card.parentElement === area;
    }

    function canRequestRefresh(name: ProviderName): boolean {
      return name === "steam"
        ? context.api.allows("steam.refresh")
        : context.api.allows("configuration.write");
    }
    function canRefresh(name: ProviderName, status: Record<string, unknown> | null): boolean {
      if (activeRefreshes.has(name)) return false;
      if (!status || status.ok !== true || status.settings_ready !== true || status.enabled !== true) return false;
      if (name === "steam") return status.scanning !== true;
      if (name === "lutris")
        return status.platform_supported === true && status.available === true && status.refresh_pending !== true;
      if (name === "playnite")
        return status.playnite_running === true && status.snapshot_pending !== true;
      return false;
    }
    async function refreshProvider(name: ProviderName): Promise<void> {
      const item = currentCards.find((candidate) => candidate.provider.name === name);
      if (!item?.refreshButton || item.refreshButton.disabled) return;
      const target = item.refreshButton;
      if (activeRefreshes.has(name)) return;
      activeRefreshes.add(name);
      try {
        await runAction(target, item.feedback, async () => {
          item.feedback.replaceChildren(el("span", "", localizedMessage("Refreshing provider...")));
          let requestAccepted = false;
          const accepted = (): void => {
            requestAccepted = true;
            if (currentCard(item))
              item.feedback.replaceChildren(el("span", "", localizedMessage("Refresh request accepted. Checking provider status.")));
          };
          try {
            let updated: unknown;
            if (name === "steam") {
              await context.api.post("steam/refresh");
              accepted();
              updated = await context.api.post<Record<string, unknown>>("steam/status");
            } else if (name === "lutris") {
              updated = await refreshLutrisProvider(context.api, accepted);
            } else {
              updated = await refreshPlayniteProvider(context.api, accepted);
            }
            if (!currentCard(item)) return;
            const summary = summarizeProvider(name, updated);
            showProviderStatus(item.status, summary);
            item.feedback.replaceChildren(
              summary.tone === "bad"
                ? el("span", "", localizedMessage("Refresh accepted; provider status reports an issue."), " ", literalText(summary.detail))
                : el("span", "", localizedMessage("Refresh accepted; current provider status was read back.")),
            );
            item.setProviderStatus(updated);
          } catch (error) {
            item.setProviderStatus(null);
            if (currentCard(item)) item.feedback.replaceChildren(
              el("span", "", localizedMessage(requestAccepted
                ? "Refresh accepted, but status readback failed. Check status before retrying."
                : "Refresh outcome is uncertain. Check status before retrying.")),
              el("span", "", literalText(error instanceof Error ? error.message : String(error))),
            );
          }
        });
      } finally {
        activeRefreshes.delete(name);
        if (currentCard(item))
          target.disabled = !canRequestRefresh(name) || !canRefresh(name, item.providerStatus);
        else if (context.content.isConnected) void load();
      }
    }
    void load();
  }
}
