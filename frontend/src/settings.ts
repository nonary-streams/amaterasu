namespace Amaterasu {
  export function renderSettings(context: PageContext): void {
    const area = el("div");
    const hostArea = el("div");
    const steamArea = el("div");
    context.content.append(
      sectionHeading(
        "Settings",
        "Edit host options and values registered for this management console.",
        button("Refresh", () => {
          void load();
          void loadHost();
          void loadSteam();
        }),
      ),
      area,
      hostArea,
      steamArea,
    );
    async function loadSteam(): Promise<void> {
      steamArea.replaceChildren(el("p", "muted", "Loading Steam settings…"));
      try {
        const settings = await context.api.post<SteamSettings>("steam/settings");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const enabled = input("steam_enabled", "", "checkbox");
        enabled.checked = settings.values.enabled;
        const status = el("div");
        const save = el("button", "button primary", "Save Steam settings");
        save.type = "submit";
        save.disabled = !context.api.allows("configuration.write");
        form.append(
          field(
            "Enable Steam library discovery",
            enabled,
            "Catalog synchronization and Steam launch support are not available yet.",
          ),
          status,
          el("div", "actions", save, badge(`Revision ${settings.revision}`)),
        );
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          void runAction(save, status, async () => {
            await context.api.post("steam/settings_update", {
              enabled: enabled.checked,
              expected_revision: settings.revision,
            });
            context.notify("Steam discovery setting saved.");
            await loadSteam();
          });
        });
        steamArea.replaceChildren(panel("Steam", form));
      } catch (error) {
        if (context.content.isConnected)
          steamArea.replaceChildren(errorPanel(error, loadSteam));
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
    void loadHost();
    void loadSteam();
  }
}
