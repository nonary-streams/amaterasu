namespace Amaterasu {
  type ProviderName = "steam" | "lutris" | "playnite";
  export interface ProviderSummary {
    state: string;
    tone: "" | "good" | "bad";
    detail: string;
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
      return { state: lastError ? "Scan issue" : pending ? "Scanning" : "Available", tone: lastError ? "bad" : "",
        detail: lastError ? `Last scan issue: ${lastError}` : `${count} discovered games.` };
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
    context.content.append(sectionHeading("Integrations", "Connect and manage application providers on this host.", button("Refresh", load)), area);
    async function load(): Promise<void> {
      const request = ++generation;
      const cards = providers.map((provider) => {
        const status = el("div", "integration-status", el("p", "muted", "Loading provider status…"));
        const settings = button("Configure", () => context.navigate("settings/integrations"));
        settings.disabled = !context.api.allows("configuration.read");
        const actions = el("div", "actions", settings);
        if (provider.name === "steam" && context.api.allows("steam.read"))
          actions.append(button("Browse games", () => context.navigate("steam")));
        const card = el("section", "panel integration-card", el("h2", "", provider.label),
          el("p", "muted", provider.description), status, actions);
        return { provider, status, card };
      });
      area.replaceChildren(...cards.map((item) => item.card));
      await Promise.all(cards.map(async ({ provider, status }) => {
        if (!context.api.allows(provider.scope)) {
          status.replaceChildren(badge("Unavailable"), el("p", "muted", "This account cannot read this provider's status."));
          return;
        }
        try {
          const summary = summarizeProvider(provider.name, await context.api.post<unknown>(`${provider.name}/status`));
          if (!context.content.isConnected || request !== generation) return;
          status.replaceChildren(badge(summary.state, summary.tone), el("p", "muted", literalText(summary.detail)));
        } catch (error) {
          if (context.content.isConnected && request === generation) status.replaceChildren(errorPanel(error));
        }
      }));
    }
    void load();
  }
}
