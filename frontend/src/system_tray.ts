namespace Amaterasu {
  export interface SystemTraySettings {
    system_tray: boolean;
  }

  export interface SystemTraySettingsReply {
    ok: true;
    settings: SystemTraySettings;
    revision: number;
    availability?: "not_ready" | "disabled" | "available" | "unavailable";
  }

  const systemTrayRenderGenerations = new WeakMap<HTMLElement, number>();

  export function defaultSystemTraySettings(): SystemTraySettings {
    return { system_tray: true };
  }

  export function parseSystemTraySettings(value: unknown): SystemTraySettings {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("System tray settings are invalid.");
    const settings = value as Record<string, unknown>;
    if (Object.keys(settings).length !== 1 || typeof settings.system_tray !== "boolean")
      throw new Error("System tray settings must contain only a boolean system_tray value.");
    return { system_tray: settings.system_tray };
  }

  export function parseSystemTraySettingsReply(value: unknown): SystemTraySettingsReply {
    if (!value || typeof value !== "object" || Array.isArray(value))
      throw new Error("System tray settings reply is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || typeof reply.revision !== "number" ||
        !Number.isSafeInteger(reply.revision) || reply.revision < 0 ||
        (reply.availability !== undefined &&
          !["not_ready", "disabled", "available", "unavailable"].includes(String(reply.availability))))
      throw new Error("System tray settings reply is invalid.");
    return {
      ok: true,
      revision: reply.revision,
      settings: parseSystemTraySettings(reply.settings),
      ...(reply.availability === undefined ? {} : {
        availability: reply.availability as SystemTraySettingsReply["availability"],
      }),
    };
  }

  async function readSystemTraySettings(api: Pick<Api, "post">): Promise<SystemTraySettingsReply> {
    return parseSystemTraySettingsReply(await api.post<unknown>("system-tray/settings"));
  }

  export async function saveSystemTraySettings(
    api: Pick<Api, "post">,
    current: SystemTraySettingsReply,
    settings: SystemTraySettings,
  ): Promise<SystemTraySettingsReply> {
    const desired = parseSystemTraySettings(settings);
    const updated = parseSystemTraySettingsReply(await api.post<unknown>("system-tray/settings_update", {
      settings: desired,
      expected_revision: current.revision,
    }));
    if (updated.revision !== current.revision + 1 ||
        updated.settings.system_tray !== desired.system_tray)
      throw new Error("System tray settings update could not be confirmed. Refresh and check the saved value.");
    const saved = await readSystemTraySettings(api);
    if (saved.revision !== updated.revision || saved.settings.system_tray !== desired.system_tray)
      throw new Error("System tray settings could not be confirmed after saving. Refresh and check the saved value.");
    return saved;
  }

  export function renderSystemTraySettings(context: PageContext, container: HTMLElement): void {
    const generation = (systemTrayRenderGenerations.get(container) ?? 0) + 1;
    systemTrayRenderGenerations.set(container, generation);
    const isCurrent = (): boolean =>
      container.isConnected && systemTrayRenderGenerations.get(container) === generation;

    const load = async (): Promise<void> => {
      container.replaceChildren(el("p", "muted", "Loading system tray preferences…"));
      try {
        let current = await readSystemTraySettings(context.api);
        if (!isCurrent()) return;
        const checkbox = input("system_tray", "", "checkbox");
        checkbox.checked = current.settings.system_tray;
        const status = el("p", "muted", `Saved settings revision ${current.revision}.`);
        status.setAttribute("role", "status");
        status.setAttribute("aria-live", "polite");
        const save = el("button", "button primary", "Save system tray preference");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        const form = el("form", "settings-form");
        form.append(
          field("Show the system tray icon and desktop notifications", checkbox,
            "Enabled by default. Desktop integration may be unavailable in a headless session."),
          el("div", "actions", save, badge(`Revision ${current.revision}`)),
          status,
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            current = await saveSystemTraySettings(context.api, current, {
              system_tray: checkbox.checked,
            });
            status.replaceChildren(`Saved settings revision ${current.revision}.`);
            context.notify("System tray preference saved and read back.");
            await load();
          });
        });
        container.replaceChildren(panel("System tray", form));
      } catch (error) {
        if (isCurrent()) container.replaceChildren(errorPanel(error, load));
      }
    };

    void load();
  }
}
