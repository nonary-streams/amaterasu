namespace Amaterasu {
  export function renderSettings(context: PageContext): void {
    const area = el("div");
    context.content.append(
      sectionHeading(
        "Settings",
        "Edit the values registered for this management console.",
        button("Refresh", load),
      ),
      area,
    );
    async function load(): Promise<void> {
      area.replaceChildren(el("p", "muted", "Loading settings…"));
      try {
        const document =
          await context.api.post<Configuration>("configuration/get");
        if (!context.content.isConnected) return;
        const form = el("form", "settings-form");
        const inputs = new Map<string, HTMLInputElement>();
        for (const [name, definition] of Object.entries(document.fields)) {
          const control = input(
            name,
            String(document.values[name] ?? definition.default),
            definition.type === "boolean"
              ? "checkbox"
              : definition.type === "integer"
                ? "number"
                : "text",
          );
          if (definition.type === "boolean")
            control.checked = Boolean(
              document.values[name] ?? definition.default,
            );
          if (definition.type === "integer") {
            control.step = "1";
            if (definition.minimum != null)
              control.min = String(definition.minimum);
            if (definition.maximum != null)
              control.max = String(definition.maximum);
          }
          if (definition.type === "string") {
            if (definition.min_length != null)
              control.minLength = definition.min_length;
            if (definition.max_length != null)
              control.maxLength = definition.max_length;
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
  }
}
