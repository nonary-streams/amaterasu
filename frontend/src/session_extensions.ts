namespace Amaterasu {
  export type SessionExtensionScope = { kind: "host" } |
    { kind: "application" | "paired_client" | "session"; id: string };
  interface SessionExtensionOption {
    id: string;
    label: string;
    help: string;
    definition: ExtensionFieldDefinition;
    choice_labels?: Record<string, string>;
    presentation?: string | null;
    allowed_scopes: string[];
    apply_policy: string;
  }
  interface SessionExtensionDescriptor {
    provider: string;
    display_name?: string | null;
    options: SessionExtensionOption[];
    actions: SessionExtensionAction[];
  }
  interface SessionExtensionSettings {
    revision: number;
    values: Record<string, unknown>;
    effective: Record<string, { value: unknown; source: unknown }>;
  }

  export function showSessionExtensionSettings(context: PageContext, scope: SessionExtensionScope, title: string, provider?: string): void {
    if (!context.content.isConnected) return;
    const area = el("div");
    const dialog = el("dialog", "extension-settings-dialog");
    dialog.setAttribute("aria-label", title);
    const drafts = trackDrafts(area);
    const dispose = () => { drafts.dispose(); dialog.remove(); };
    const unregister = context.registerDraftGuard?.(() => dialog.isConnected && drafts.isDirty(), dispose);
    const close = () => {
      if (drafts.isDirty() && !window.confirm("Discard your unsaved plugin settings?")) return;
      dialog.close();
      if (unregister) unregister();
      else dispose();
    };
    dialog.append(el("h2", "", literalText(title)), button("Close", close), area);
    dialog.addEventListener("cancel", (event) => { event.preventDefault(); close(); });
    document.body.append(dialog);
    dialog.showModal();
    renderSessionExtensionSettings(context, area, scope, provider);
  }

  export function renderSessionExtensionSettings(context: PageContext, area: HTMLElement, scope: SessionExtensionScope = { kind: "host" }, provider?: string): void {
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
        const scopedDescriptors = descriptors.filter((descriptor) => (!provider || descriptor.provider === provider) && Array.isArray(descriptor.options) && descriptor.options.some((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes(scope.kind)));
        const settings = await Promise.all(scopedDescriptors.map((descriptor) => context.api.post<SessionExtensionSettings>("session-extensions/settings_get", { provider: descriptor.provider, scope })));
        if (current !== generation || !context.content.isConnected || !area.isConnected) return;
        const panels: HTMLElement[] = [];
        scopedDescriptors.forEach((descriptor, index) => {
          const document = settings[index];
          if (!document || !Array.isArray(descriptor.options)) throw new Error("Invalid extension settings response.");
          const options = descriptor.options.filter((option) => option.allowed_scopes.includes(scope.kind));
          if (!options.length) return;
          const form = el("form", "settings-form");
          const editors = new Map<string, { inherit: HTMLInputElement | null; editor: ExtensionFieldEditor }>();
          for (const option of options) {
            const effective = document.effective[option.id];
            if (!effective) throw new Error("Missing effective extension setting.");
            const isOverride = Object.hasOwn(document.values, option.id);
            const editorValue = isOverride ? document.values[option.id] : effective.value;
            if (option.presentation != null && option.presentation !== "keybinding")
              throw new Error("Unsupported extension setting presentation.");
            const editor = option.presentation === "keybinding"
              ? extensionKeybindingSettingEditor(option.definition, editorValue)
              : extensionFieldEditor(option.definition, editorValue, option.choice_labels);
            // Host settings are ordinary editable values. Scoped values may
            // inherit, but editing a control must always save that edit.
            const inherit = scope.kind === "host" ? null : input("", "", "checkbox");
            if (inherit) {
              inherit.checked = !isOverride;
              const override = () => { inherit.checked = false; };
              editor.node.addEventListener("input", override);
              editor.node.addEventListener("change", override);
            }
            const controls = editor.node.matches("input,select,textarea")
              ? [editor.node] : Array.from(editor.node.querySelectorAll("input,select,textarea"));
            if (controls.length === 1) controls[0]!.setAttribute("aria-label", option.label);
            const group = el("fieldset", "session-extension-option",
              el("legend", "field-label", literalText(option.label)), editor.node);
            if (inherit) group.append(el("label", "session-extension-inherit", inherit, " Use inherited value"));
            if (option.help) group.append(el("small", "muted", literalText(option.help)));
            form.append(group);
            editors.set(option.id, { inherit, editor });
          }
          const status = el("div");
          const save = el("button", "button primary", "Save settings");
          save.type = "submit";
          save.disabled = !context.api.allows("configuration.write");
          form.append(status, save);
          form.addEventListener("submit", (event) => {
            event.preventDefault();
            void runAction(save, status, async () => {
              const values: Payload = { ...document.values };
              for (const [id, entry] of editors) {
                if (entry.inherit?.checked) delete values[id];
                else values[id] = entry.editor.value();
              }
              await context.api.post("session-extensions/settings_update", { provider: descriptor.provider, scope, expected_revision: document.revision, values });
              context.notify("Plugin settings saved.");
              await load();
            });
          });
          panels.push(panel(literalText(descriptor.display_name || "Plugin settings"), form));
        });
        for (const descriptor of descriptors) {
          if (scope.kind !== "host" || (provider && descriptor.provider !== provider)) continue;
          if (!Array.isArray(descriptor.actions)) throw new Error("Invalid extension actions response.");
          const actions = descriptor.actions.filter((action) => Array.isArray(action.allowed_targets) && action.allowed_targets.includes("host") && ["read_only", "host_mutation"].includes(action.execution));
          if (actions.length) panels.push(panel(`${descriptor.provider} actions`, ...actions.map((action) => renderExtensionAction(context, action))));
        }
        area.replaceChildren(...(panels.length ? panels : [el("p", "muted", "No plugin settings are available for this scope.")]));
      } catch (error) {
        if (current === generation && context.content.isConnected && area.isConnected) area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }
}
