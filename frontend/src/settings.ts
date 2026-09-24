namespace Amaterasu {
  const maxConfiguredAuthTtlSeconds = 0x7fff_ffff;

  export function parseAuthTokenLifetimes(
    sessionTokenTtlSeconds: number,
    rememberMeRefreshTokenTtlSeconds: number,
  ): AuthTokenSettings["values"] {
    if (
      !Number.isSafeInteger(sessionTokenTtlSeconds) ||
      sessionTokenTtlSeconds < 1 ||
      sessionTokenTtlSeconds > maxConfiguredAuthTtlSeconds ||
      !Number.isSafeInteger(rememberMeRefreshTokenTtlSeconds) ||
      rememberMeRefreshTokenTtlSeconds < 1 ||
      rememberMeRefreshTokenTtlSeconds > maxConfiguredAuthTtlSeconds
    )
      throw new Error("Authentication lifetimes must be whole seconds from 1 to 2147483647.");
    return {
      session_token_ttl_seconds: sessionTokenTtlSeconds,
      remember_me_refresh_token_ttl_seconds: rememberMeRefreshTokenTtlSeconds,
    };
  }

  export function parseSteamExclusions(value: string): SteamExclusion[] {
    let parsed: unknown;
    try {
      parsed = JSON.parse(value) as unknown;
    } catch {
      throw new Error("Excluded games must be valid JSON.");
    }
    if (!Array.isArray(parsed) || parsed.length > 256)
      throw new Error("Excluded games must be a JSON array with at most 256 entries.");

    const ids = new Set<string>();
    const names = new Set<string>();
    for (const item of parsed) {
      if (typeof item !== "object" || item === null || Array.isArray(item))
        throw new Error("Each excluded game must be an object.");
      const exclusion = item as Record<string, unknown>;
      if (Object.keys(exclusion).some((key) => key !== "id" && key !== "name"))
        throw new Error("Excluded games may only contain id and name fields.");
      const id = exclusion.id;
      const name = exclusion.name;
      if (id === undefined && name === undefined)
        throw new Error("Each excluded game needs an app ID or name.");

      if (id !== undefined) {
        if (
          typeof id !== "string" ||
          !/^[1-9][0-9]*$/.test(id) ||
          !Number.isSafeInteger(Number(id)) ||
          Number(id) > 0xffff_ffff ||
          String(Number(id)) !== id
        )
          throw new Error("Steam app IDs must be canonical nonzero decimal strings.");
        if (ids.has(id)) throw new Error("Excluded Steam app IDs must be unique.");
        ids.add(id);
      }

      if (name !== undefined) {
        if (
          typeof name !== "string" ||
          name.trim().length === 0 ||
          new TextEncoder().encode(name).length > 512 ||
          /[\u0000-\u001f\u007f-\u009f]/.test(name)
        )
          throw new Error("Exclusion names must contain 1–512 printable UTF-8 bytes.");
        if (id === undefined) {
          const normalized = name.toLowerCase();
          if (names.has(normalized))
            throw new Error("Name-only exclusions must be unique, ignoring case.");
          names.add(normalized);
        }
      }
    }
    return parsed as SteamExclusion[];
  }

  export function steamStatusDescription(status: SteamStatus): string {
    if (status.settings_ready === false)
      return "Provider settings are still loading; runtime state is not confirmed yet.";
    if (typeof status.enabled !== "boolean")
      return "The provider did not report whether Steam discovery is enabled.";
    if (!status.enabled)
      return "Steam discovery is disabled; no scan is running.";
    const scanState = status.scanning
      ? "scan in progress"
      : status.complete
        ? "last scan complete"
        : "no complete scan is available";
    const warningCount = `${status.warning_count} ${status.warning_count === 1 ? "warning" : "warnings"}`;
    return `Steam discovery enabled · ${scanState} · ${status.game_count} games · ${warningCount}.`;
  }

  export function renderSettings(context: PageContext): void {
    const area = el("div");
    const authArea = el("div");
    const hostArea = el("div");
    const steamArea = el("div");
    context.content.append(
      sectionHeading(
        "Settings",
        "Edit host options and values registered for this management console.",
        button("Refresh", () => {
          void load();
          void loadAuth();
          void loadHost();
          void loadSteam();
        }),
      ),
      area,
      authArea,
      hostArea,
      steamArea,
    );
    async function loadAuth(): Promise<void> {
      authArea.replaceChildren(el("p", "muted", "Loading authentication lifetimes…"));
      try {
        const settings = await context.api.post<AuthTokenSettings>("auth/settings");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const sessionTtl = input(
          "session_token_ttl_seconds",
          String(settings.values.session_token_ttl_seconds),
          "number",
        );
        const refreshTtl = input(
          "remember_me_refresh_token_ttl_seconds",
          String(settings.values.remember_me_refresh_token_ttl_seconds),
          "number",
        );
        for (const control of [sessionTtl, refreshTtl]) {
          control.required = true;
          control.min = "1";
          control.max = String(maxConfiguredAuthTtlSeconds);
          control.step = "1";
        }
        const status = el("div");
        const save = el("button", "button primary", "Save authentication settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Administrator session lifetime (seconds)",
            sessionTtl,
            "Access sessions default to 7200 seconds. Existing sessions keep their issued expiry; new logins and refreshes use this value.",
          ),
          field(
            "Remembered device lifetime (seconds)",
            refreshTtl,
            "Remember-me refresh tokens default to 604800 seconds. Non-remembered refresh tokens remain fixed at 86400 seconds.",
          ),
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            const values = parseAuthTokenLifetimes(
              sessionTtl.valueAsNumber,
              refreshTtl.valueAsNumber,
            );
            await context.api.post("auth/settings_update", {
              ...values,
              expected_revision: settings.revision,
            });
            context.notify("Authentication lifetimes saved.");
            await loadAuth();
          });
        });
        authArea.replaceChildren(panel("Authentication session lifetimes", form));
      } catch (error) {
        if (context.content.isConnected)
          authArea.replaceChildren(errorPanel(error, loadAuth));
      }
    }
    async function loadSteam(): Promise<void> {
      steamArea.replaceChildren(el("p", "muted", "Loading Steam settings…"));
      try {
        const settings = await context.api.post<SteamSettings>("steam/settings");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const enabled = input("steam_enabled", "", "checkbox");
        enabled.checked = settings.values.enabled;
        const autoSync = input("steam_auto_sync", "", "checkbox");
        autoSync.checked = settings.values.auto_sync;
        const exclusions = el("textarea");
        exclusions.name = "steam_exclude_games";
        exclusions.rows = 10;
        exclusions.spellcheck = false;
        exclusions.value = JSON.stringify(settings.values.exclude_games, null, 2);
        const status = el("div");
        const runtimeStatus = el(
          "p",
          "muted",
          "Loading current provider status…",
        );
        const save = el("button", "button primary", "Save Steam settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Enable Steam library discovery",
            enabled,
            "Controls whether the provider exposes discovered Steam games and accepts scans.",
          ),
          field(
            "Automatic synchronization",
            autoSync,
            "Controls automatic scans of the configured Steam library roots.",
          ),
          field(
            "Excluded games (JSON)",
            exclusions,
            'Use objects with "id" (canonical app ID), "name", or both. ID takes precedence; name-only entries match without case sensitivity.',
          ),
          runtimeStatus,
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            await context.api.post("steam/settings_update", {
              enabled: enabled.checked,
              auto_sync: autoSync.checked,
              exclude_games: parseSteamExclusions(exclusions.value),
              expected_revision: settings.revision,
            });
            context.notify("Steam discovery setting saved.");
            await loadSteam();
          });
        });
        steamArea.replaceChildren(panel("Steam", form));
        void loadSteamRuntimeStatus(runtimeStatus, context);
      } catch (error) {
        if (context.content.isConnected)
          steamArea.replaceChildren(errorPanel(error, loadSteam));
      }
    }
    async function loadSteamRuntimeStatus(
      target: HTMLElement,
      context: PageContext,
    ): Promise<void> {
      try {
        const status = await context.api.post<SteamStatus>("steam/status");
        if (!target.isConnected) return;
        target.textContent = steamStatusDescription(status);
      } catch (error) {
        if (!target.isConnected) return;
        target.textContent =
          error instanceof ApiError && error.code === "forbidden"
            ? "Runtime status requires steam.read; settings remain available with configuration access."
            : "Current provider status is unavailable; settings remain editable.";
      }
    }
    async function loadHost(): Promise<void> {
      hostArea.replaceChildren(el("p", "muted", "Loading host name…"));
      try {
        const settings = await context.api.post<NvhttpSettings>(
          "nvhttp/settings",
        );
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const hostname = input("hostname", settings.configured_hostname);
        hostname.maxLength = 128;
        const port = input("port", String(settings.port), "number");
        port.required = true;
        port.min = "1029";
        port.max = "65514";
        port.step = "1";
        const status = el("div");
        const save = el("button", "button primary", "Save host settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field("Name shown to Moonlight", hostname, "Leave blank to use this PC's name."),
          field("Moonlight HTTP port", port, "HTTPS uses the port five below this value."),
          el("p", "muted", `Active ports: HTTP ${settings.active_http_port ?? "off"}, HTTPS ${settings.active_https_port ?? "off"}`),
          ...(settings.listener_error ? [el("p", "error", settings.listener_error)] : []),
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            await context.api.post("nvhttp/settings_update", {
              hostname: hostname.value,
              port: port.valueAsNumber,
              expected_revision: settings.revision,
            });
            context.notify("Host settings saved.");
            await loadHost();
          });
        });
        hostArea.replaceChildren(panel("Moonlight host name", form));
      } catch (error) {
        if (context.content.isConnected)
          hostArea.replaceChildren(errorPanel(error, loadHost));
      }
    }
    async function load(): Promise<void> {
      area.replaceChildren(el("p", "muted", "Loading settings…"));
      try {
        const document =
          await context.api.post<Configuration>("configuration/get");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const inputs = new Map<
          string,
          HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement
        >();
        for (const [name, definition] of Object.entries(document.fields)) {
          const value = document.values[name] ?? definition.default;
          let control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
          if (definition.type === "enum") {
            const select = el("select");
            select.name = name;
            for (const optionValue of definition.options) {
              const option = el("option", "", optionValue);
              option.value = optionValue;
              select.append(option);
            }
            select.value = String(value);
            control = select;
          } else if (
            definition.type === "list" ||
            definition.type === "object"
          ) {
            const editor = el("textarea");
            editor.name = name;
            editor.rows = 6;
            editor.value = JSON.stringify(value, null, 2);
            control = editor;
          } else {
            control = input(
              name,
              String(value),
              definition.type === "boolean"
                ? "checkbox"
                : definition.type === "integer" ||
                    definition.type === "decimal"
                  ? "number"
                  : "text",
            );
          }
          if (definition.type === "boolean")
            (control as HTMLInputElement).checked = Boolean(value);
          if (definition.type === "integer" || definition.type === "decimal") {
            const numericControl = control as HTMLInputElement;
            numericControl.step = definition.type === "integer" ? "1" : "any";
            if (definition.minimum != null)
              numericControl.min = String(definition.minimum);
            if (definition.maximum != null)
              numericControl.max = String(definition.maximum);
          }
          if (definition.type === "string") {
            const textControl = control as HTMLInputElement;
            if (definition.min_length != null)
              textControl.minLength = definition.min_length;
            if (definition.max_length != null)
              textControl.maxLength = definition.max_length;
          }
          inputs.set(name, control);
          form.append(field(name, control, `Type: ${definition.type}`));
        }
        const status = el("div");
        const save = el("button", "button primary", "Save settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          status,
          el("div", "actions", save, badge(`Revision ${document.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            const values: Payload = {};
            for (const [name, control] of inputs) {
              const definition = document.fields[name];
              if (definition) values[name] = fieldValue(definition, control);
            }
            await context.api.post("configuration/update", {
              values,
              expected_revision: document.revision,
            });
            context.notify("Settings saved.");
            await load();
          });
        });
        area.replaceChildren(
          Object.keys(document.fields).length
            ? panel("Registered values", form)
            : empty(
                "No settings fields",
                "The registered schema is empty. Schema setup and migration are available through the management API.",
              ),
        );
      } catch (error) {
        if (!context.content.isConnected) return;
        area.replaceChildren(
          error instanceof ApiError && error.code === "not_registered"
            ? empty(
                "No settings schema yet",
                "Register a schema for the management principal through the authenticated configuration API. This screen will then show its editable values.",
              )
            : errorPanel(error, load),
        );
      }
    }
    void load();
    void loadAuth();
    void loadHost();
    void loadSteam();
  }
}
