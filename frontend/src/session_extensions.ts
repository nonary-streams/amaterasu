namespace Amaterasu {
  interface SessionExtensionOption {
    id: string;
    label: string;
    help: string;
    definition: ExtensionFieldDefinition;
    presentation?: string | null;
    allowed_scopes: string[];
    apply_policy: string;
  }
  interface SessionExtensionDescriptor {
    provider: string;
    options: SessionExtensionOption[];
  }
  interface SessionExtensionSettings {
    revision: number;
    values: Record<string, unknown>;
    effective: Record<string, { value: unknown; source: unknown }>;
  }

  export function renderSessionExtensionSettings(context: PageContext, area: HTMLElement): void {
    let generation = 0;
    async function load(): Promise<void> {
      const current = ++generation;
      try {
        const descriptors: SessionExtensionDescriptor[] = [];
        let after: string | null = null;
        let revision: number | null = null;
        const seen = new Set<string>();
        do {
          const page: { items: SessionExtensionDescriptor[]; next: string | null; revision: number } = await context.api.post("session-extensions/list", { after, limit: 32, revision });
          if (!Array.isArray(page.items) || (page.next !== null && typeof page.next !== "string")) throw new Error("Invalid extension discovery response.");
          if (!Number.isSafeInteger(page.revision) || page.revision < 0 || (revision !== null && page.revision !== revision)) throw new Error("Extension discovery changed. Refresh and try again.");
          revision = page.revision;
          descriptors.push(...page.items);
          after = page.next;
          if (after && seen.has(after)) throw new Error("Extension discovery cursor repeated.");
          if (after) seen.add(after);
          if (descriptors.length > 1024) throw new Error("Too many extensions.");
        } while (after);
        const hostDescriptors = descriptors.filter((descriptor) => Array.isArray(descriptor.options) && descriptor.options.some((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes("host")));
        const settings = await Promise.all(hostDescriptors.map((descriptor) => context.api.post<SessionExtensionSettings>("session-extensions/settings_get", { provider: descriptor.provider, scope: { kind: "host" } })));
        if (current !== generation || !context.content.isConnected) return;
        const panels: HTMLElement[] = [];
        hostDescriptors.forEach((descriptor, index) => {
          const document = settings[index];
          if (!document || !Array.isArray(descriptor.options)) throw new Error("Invalid extension settings response.");
          const options = descriptor.options.filter((option) => option.allowed_scopes.includes("host"));
          if (!options.length) return;
          const form = el("form", "settings-form");
          const editors = new Map<string, { inherit: HTMLInputElement; editor: ExtensionFieldEditor }>();
          for (const option of options) {
            const effective = document.effective[option.id];
            if (!effective) throw new Error("Missing effective extension setting.");
            const isOverride = Object.hasOwn(document.values, option.id);
            const editorValue = isOverride ? document.values[option.id] : effective.value;
            if (option.presentation != null && option.presentation !== "keybinding")
              throw new Error("Unsupported extension setting presentation.");
            const editor = option.presentation === "keybinding"
              ? extensionKeybindingEditor(editorValue)
              : extensionFieldEditor(option.definition, editorValue);
            const inherit = input("", "", "checkbox");
            inherit.checked = !isOverride;
            form.append(el("div", "field", el("span", "field-label", option.label), editor.node,
              el("label", "", inherit, " Inherit default"),
              el("small", "muted", `${option.help} · Applies: ${option.apply_policy} · Source: ${effective.source === null ? "default" : JSON.stringify(effective.source)}`)));
            editors.set(option.id, { inherit, editor });
          }
          const status = el("div");
          const save = el("button", "button primary", "Save extension settings");
          save.type = "submit";
          save.disabled = !context.api.allows("configuration.write");
          form.append(status, save);
          form.addEventListener("submit", (event) => {
            event.preventDefault();
            void runAction(save, status, async () => {
              const values: Payload = { ...document.values };
              for (const [id, entry] of editors) {
                if (entry.inherit.checked) delete values[id];
                else values[id] = entry.editor.value();
              }
              await context.api.post("session-extensions/settings_update", { provider: descriptor.provider, scope: { kind: "host" }, expected_revision: document.revision, values });
              context.notify("Extension settings saved. Active sessions may use an earlier snapshot.");
              await load();
            });
          });
          panels.push(panel(descriptor.provider, form));
        });
        area.replaceChildren(...panels);
      } catch (error) {
        if (current === generation && context.content.isConnected) area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }
}
