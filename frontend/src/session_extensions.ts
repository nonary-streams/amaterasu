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
  const applicationOverrideGuards = new WeakMap<HTMLElement, () => boolean>();

  const applicationOverrideSaves = new WeakMap<HTMLElement, () => boolean>();

  export function applicationPluginOverridesSaving(area: HTMLElement): boolean {
    return applicationOverrideSaves.get(area)?.() ?? false;
  }

  export function applicationPluginOverridesDirty(area: HTMLElement): boolean {
    return applicationOverrideGuards.get(area)?.() ?? false;
  }

  export function renderApplicationPluginOverrides(context: PageContext, area: HTMLElement, applicationId: string): void {
    const scope: SessionExtensionScope = { kind: "application", id: applicationId };
    let busy = false;
    const dirtyChecks: Array<() => boolean> = [];
    const isDirty = () => busy || dirtyChecks.some((check) => check());
    applicationOverrideGuards.set(area, isDirty);
    applicationOverrideSaves.set(area, () => busy);
    context.registerDraftGuard?.(() => area.isConnected && isDirty(), () => {
      applicationOverrideGuards.delete(area);
      applicationOverrideSaves.delete(area);
    });

    async function load(): Promise<void> {
      try {
        const descriptors: SessionExtensionDescriptor[] = [];
        let after: string | null = null;
        let revision: number | null = null;
        const seen = new Set<string>();
        do {
          const page: { items: SessionExtensionDescriptor[]; next: string | null; revision: number } = await context.api.post("session-extensions/list", { after, limit: 32, revision });
          if (!Array.isArray(page.items) || (page.next !== null && typeof page.next !== "string") ||
              !Number.isSafeInteger(page.revision) || page.revision < 0 || (revision !== null && revision !== page.revision))
            throw new Error("Invalid or changed extension discovery response.");
          revision = page.revision;
          descriptors.push(...page.items);
          after = page.next;
          if (after && seen.has(after)) throw new Error("Extension discovery cursor repeated.");
          if (after) seen.add(after);
          if (descriptors.length > 1024) throw new Error("Too many extensions.");
        } while (after);
        const providers = descriptors.filter((descriptor) => Array.isArray(descriptor.options) &&
          descriptor.options.some((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes("application")));
        const documents: SessionExtensionSettings[] = [];
        // Keep discovery from exhausting the registry's bounded pending requests.
        for (let offset = 0; offset < providers.length; offset += 4) {
          const batch = providers.slice(offset, offset + 4);
          documents.push(...await Promise.all(batch.map((descriptor) => context.api.post<SessionExtensionSettings>(
            "session-extensions/settings_get", { provider: descriptor.provider, scope }))));
          if (!context.content.isConnected || !area.isConnected) return;
        }
        if (!context.content.isConnected || !area.isConnected) return;
        const panels = providers.map((descriptor, index) => {
          const document = documents[index];
          if (!document || !document.values || !document.effective || !Number.isSafeInteger(document.revision))
            throw new Error("Invalid extension settings response.");
          let savedRevision = document.revision;
          let effectiveAvailable = true;
          let baseline = JSON.stringify(document.values);
          const options = descriptor.options.filter((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes("application"));
          const entries = new Map<string, { editor: ExtensionFieldEditor }>();
          const form = el("form", "settings-form catalog-plugin-overrides-form");
          const fields = el("fieldset", "catalog-plugin-overrides-fields");
          fields.disabled = !context.api.allows("configuration.write");
          const rows = el("div", "catalog-plugin-overrides-rows");
          const search = input("", "", "search");
          search.placeholder = "Search settings";
          search.setAttribute("aria-label", "Search plugin settings");
          search.setAttribute("data-draft-ignore", "");
          const choices = el("select");
          choices.setAttribute("aria-label", "Setting to override");
          choices.setAttribute("data-draft-ignore", "");
          const inherited = el("p", "muted catalog-plugin-overrides-inherited");
          const describeInherited = () => {
            if (!effectiveAvailable) { inherited.textContent = "Reload the application editor to refresh inherited values."; return; }
            const effective = document.effective[choices.value];
            if (!effective) { inherited.textContent = ""; return; }
            const source = effective.source as { kind?: string } | null;
            if (source?.kind === "application") {
              inherited.textContent = "Inherited value is available after reloading settings.";
              return;
            }
            inherited.textContent = `Current inherited value: ${JSON.stringify(effective.value)} (${source?.kind === "host" ? "host" : source === null ? "plugin default" : "other scope"})`;
          };
          const refreshChoices = () => {
            const query = search.value.trim().toLocaleLowerCase();
            const available = options.filter((option) => !entries.has(option.id) &&
              `${option.label} ${option.id} ${option.help}`.toLocaleLowerCase().includes(query));
            choices.replaceChildren(...available.map((option) => {
              const item = el("option", "", literalText(option.label));
              item.value = option.id;
              return item;
            }));
            add.disabled = !effectiveAvailable || !available.length;
            describeInherited();
          };
          const addOption = (option: SessionExtensionOption, value: unknown) => {
            if (option.presentation != null && option.presentation !== "keybinding") throw new Error("Unsupported extension setting presentation.");
            const editor = option.presentation === "keybinding"
              ? extensionKeybindingSettingEditor(option.definition, value)
              : extensionFieldEditor(option.definition, value, option.choice_labels);
            const controls = editor.node.matches("input,select,textarea") ? [editor.node] : Array.from(editor.node.querySelectorAll("input,select,textarea"));
            if (controls.length === 1) controls[0]!.setAttribute("aria-label", option.label);
            const row = el("fieldset", "session-extension-option catalog-plugin-override-row", el("legend", "field-label", literalText(option.label)), editor.node);
            if (option.help) row.append(el("small", "muted", literalText(option.help)));
            row.append(button("Remove override", () => { entries.delete(option.id); row.remove(); refreshChoices(); }));
            entries.set(option.id, { editor });
            rows.append(row);
          };
          const add = button("Add override", () => {
            const option = options.find((candidate) => candidate.id === choices.value);
            const effective = option && document.effective[option.id];
            if (!effectiveAvailable || !option || !effective) return;
            addOption(option, structuredClone(effective.value));
            refreshChoices();
          });
          search.addEventListener("input", refreshChoices);
          choices.addEventListener("change", describeInherited);
          for (const option of options) if (Object.hasOwn(document.values, option.id)) addOption(option, document.values[option.id]);
          refreshChoices();
          const values = (): Payload => {
            const result: Payload = { ...document.values };
            for (const option of options) {
              const entry = entries.get(option.id);
              if (entry) result[option.id] = entry.editor.value();
              else delete result[option.id];
            }
            return result;
          };
          dirtyChecks.push(() => { try { return JSON.stringify(values()) !== baseline; } catch { return true; } });
          const status = el("div");
          const save = el("button", "button primary", "Save plugin overrides");
          save.type = "submit";
          const chooser = el("details", "catalog-override-chooser catalog-plugin-override-chooser",
            el("summary", "", "Add override"),
            el("div", "catalog-plugin-override-chooser-content", search, choices, inherited, add));
          const actions = el("div", "catalog-plugin-overrides-actions", save);
          fields.append(rows, chooser, actions);
          form.append(fields, status);
          form.addEventListener("submit", (event) => {
            event.preventDefault();
            if (busy || !context.api.allows("configuration.write")) return;
            void runAction(save, status, async () => {
              const submitted = values();
              if (JSON.stringify(submitted) === baseline) return;
              busy = true;
              fields.disabled = true;
              try {
                const reply = await context.api.post<{ revision: number }>("session-extensions/settings_update", {
                  provider: descriptor.provider, scope, expected_revision: savedRevision, values: submitted,
                });
                if (!Number.isSafeInteger(reply.revision) || reply.revision <= savedRevision) throw new Error("Plugin save could not be confirmed. Reopen the editor before retrying.");
                savedRevision = reply.revision;
                baseline = JSON.stringify(submitted);
                document.values = submitted;
                effectiveAvailable = false;
                refreshChoices();
                try {
                  const refreshed = await context.api.post<SessionExtensionSettings>("session-extensions/settings_get", { provider: descriptor.provider, scope });
                  if (!refreshed || refreshed.revision !== savedRevision || !refreshed.effective)
                    throw new Error("Settings changed during refresh.");
                  document.effective = refreshed.effective;
                  effectiveAvailable = true;
                  refreshChoices();
                } catch {
                  throw new Error("Plugin overrides were saved, but inherited values could not be refreshed. Reopen the application editor before adding an override.");
                }
                context.notify("Plugin overrides saved independently of application changes.");
              } finally {
                busy = false;
                fields.disabled = !context.api.allows("configuration.write");
              }
            });
          });
          return panel(literalText(descriptor.display_name || descriptor.provider), form);
        });
        area.replaceChildren(el("p", "muted", "Plugin overrides are saved separately from application changes."),
          ...(panels.length ? panels : [el("p", "muted", "No plugin settings are available for this application.")]));
      } catch (error) {
        if (context.content.isConnected && area.isConnected) area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }

}
