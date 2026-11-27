namespace Amaterasu {
  const settingsRenderers = new WeakMap<HTMLElement, object>();
  export function claimModuleSettings(context: PageContext, owners: ReadonlySet<string>): void {
    for (const node of context.content.querySelectorAll<HTMLElement>("[data-settings-owner]")) {
      const claimed = owners.has(node.getAttribute("data-settings-owner") ?? "");
      node.classList.toggle("module-settings-claimed", claimed);
      node.inert = claimed;
    }
  }
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
    default_expanded?: boolean;
    options: SessionExtensionOption[];
    actions: SessionExtensionAction[];
  }

  export function pluginSettingsSection(title: string, defaultExpanded: boolean, content: HTMLElement): HTMLDetailsElement {
    const section = el("details", "application-plugin-section",
      el("summary", "application-plugin-summary", literalText(title)),
      el("div", "application-plugin-content", content));
    section.open = defaultExpanded;
    return section;
  }
  interface SessionExtensionSettings {
    revision: number;
    values: Record<string, unknown>;
    effective: Record<string, { value: unknown; source: unknown }>;
  }

  function sessionExtensionApplyPolicyHelp(policy: string): string {
    switch (policy) {
      case "next_session": return "Requires a new application session.";
      case "next_stream": return "Requires a new or resumed stream.";
      case "live_action": return "May apply during an active session or stream; saving does not confirm application.";
      default: return "Application timing is not available.";
    }
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

  interface EverydayPluginSections {
    area: HTMLElement;
    options: ReadonlyMap<string, ReadonlySet<string>>;
    hasSignedSections: boolean;
    onRemainingReady(load: () => void): void;
  }

  export function renderSessionExtensionSettings(context: PageContext, area: HTMLElement, scope: SessionExtensionScope = { kind: "host" }, provider?: string, excluded: ReadonlySet<string> = new Set(), everyday?: EverydayPluginSections): void {
    const renderer = {}; settingsRenderers.set(area, renderer);
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
        const scopedDescriptors = descriptors.filter((descriptor) => !excluded.has(descriptor.provider) && (!provider || descriptor.provider === provider) && Array.isArray(descriptor.options) && descriptor.options.some((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes(scope.kind)));
        const isCurrent = () => settingsRenderers.get(area) === renderer && current === generation && context.content.isConnected && area.isConnected;
        if (!isCurrent()) return;
        const panels: HTMLElement[] = [];
        const everydayPanels: HTMLElement[] = [];
        const remaining: Array<() => void> = [];
        for (const descriptor of scopedDescriptors) {
          const title = descriptor.display_name || descriptor.provider;
          const body = el("div", "", el("p", "muted", "Loading settings…"));
          const featured = everyday && scope.kind === "host" ? everyday.options.get(descriptor.provider) : undefined;
          const hasFeatured = !!featured && descriptor.options.some(option => featured.has(option.id) && option.allowed_scopes.includes(scope.kind));
          const section = scope.kind === "host" && !provider
            ? pluginSettingsSection(title, hasFeatured || descriptor.default_expanded === true, body) : null;
          (hasFeatured ? everydayPanels : panels).push(section ?? panel(literalText(title), body));
          let providerLoad = 0;
          const loadProvider = async (confirmed?: SessionExtensionSettings): Promise<void> => {
            if (!isCurrent()) return;
            const request = ++providerLoad;
            try {
              const document = confirmed ?? await context.api.post<SessionExtensionSettings>("session-extensions/settings_get", { provider: descriptor.provider, scope });
              if (!isCurrent() || !body.isConnected || request !== providerLoad) return;
              body.replaceChildren(renderForm(descriptor, document, loadProvider, hasFeatured ? featured : undefined));
            } catch (error) {
              if (!isCurrent() || !body.isConnected || request !== providerLoad) return;
              if (section) section.open = true;
              body.replaceChildren(errorPanel(error, () => loadProvider()));
            }
          };
          if (everyday && !hasFeatured) remaining.push(() => void loadProvider());
          else void loadProvider();
        }
        function renderForm(descriptor: SessionExtensionDescriptor, document: SessionExtensionSettings, reload: (confirmed?: SessionExtensionSettings) => Promise<void>, featured?: ReadonlySet<string>): HTMLElement {
          const options = descriptor.options.filter((option) => option.allowed_scopes.includes(scope.kind));
          const form = el("form", "settings-form");
          const more = el("details", "everyday-more-options", el("summary", "", "More settings"));
          const editors = new Map<string, { inherit: HTMLInputElement | null; editor: ExtensionFieldEditor }>();
          for (const option of options) {
            const effective = document.effective[option.id];
            if (!effective) throw new Error("Missing effective extension setting.");
            const isOverride = Object.hasOwn(document.values, option.id);
            const editorValue = isOverride ? document.values[option.id] : effective.value;
            const editor = option.presentation === "keybinding"
              ? extensionKeybindingSettingEditor(option.definition, editorValue)
              : option.presentation === "slider" ? extensionSliderEditor(option.definition, editorValue, option.label)
              : option.presentation === "radio_group" ? extensionRadioGroupEditor(option.definition, editorValue, option.label, option.choice_labels)
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
            const group = el("fieldset", "session-extension-option");
            const title = literalText(option.label);
            const policyHint = el("small", "muted", sessionExtensionApplyPolicyHelp(option.apply_policy));
            if (scope.kind === "host" && editor.node.matches("input, select, textarea, .extension-slider, .radio-group")) {
              const row = el(editor.node.matches("input, select, textarea") ? "label" : "div", "field",
                el("span", "field-label", title), editor.node);
              group.setAttribute("aria-label", option.label);
              if (option.help) row.append(el("small", "muted", literalText(option.help)));
              row.append(policyHint);
              group.append(row);
            } else {
              group.append(el("legend", "field-label", title), editor.node);
              if (inherit) group.append(el("label", "session-extension-inherit", inherit, " Use inherited value"));
              if (option.help) group.append(el("small", "muted", literalText(option.help)));
              group.append(policyHint);
            }
            group.disabled = !context.api.allows("configuration.write");
            (featured && !featured.has(option.id) ? more : form).append(group);
            editors.set(option.id, { inherit, editor });
          }
          const status = el("div");
          const save = el("button", "button primary", "Save settings");
          save.type = "submit";
          save.disabled = !context.api.allows("configuration.write");
          if (featured && options.some(option => !featured.has(option.id))) form.append(more);
          form.append(status, save);
          form.addEventListener("submit", (event) => {
            event.preventDefault();
            void runAction(save, status, async () => {
              const values: Payload = { ...document.values };
              for (const [id, entry] of editors) {
                if (entry.inherit?.checked) {
                  delete values[id];
                  continue;
                }
                const next = entry.editor.value();
                if (featured && sameConfigurationValue(next, document.effective[id]!.value)) continue;
                values[id] = next;
              }
              if (featured && sameConfigurationValue(values, document.values)) return;
              form.inert = true;
              try {
                const result = await context.api.post<{ revision: number }>("session-extensions/settings_update", {
                  provider: descriptor.provider, scope, expected_revision: document.revision, values,
                });
                let confirmed: SessionExtensionSettings | undefined;
                if (featured) {
                  confirmed = await context.api.post<SessionExtensionSettings>("session-extensions/settings_get", { provider: descriptor.provider, scope });
                  if (!Number.isSafeInteger(result.revision) || result.revision <= document.revision ||
                      confirmed.revision !== result.revision || !sameConfigurationValue(confirmed.values, values))
                    throw new Error("The plugin could not confirm these changes. Reload its saved settings to review them.");
                }
                context.notify("Plugin settings saved. Application to an active session or stream is not confirmed.");
                await reload(confirmed);
              } finally {
                form.inert = false;
              }
            });
          });
          return form;
        }
        for (const descriptor of descriptors) {
          if (scope.kind !== "host" || (provider && descriptor.provider !== provider)) continue;
          if (!Array.isArray(descriptor.actions)) {
            panels.push(panel(literalText(descriptor.display_name || descriptor.provider), errorPanel(new Error("Invalid extension actions response."))));
            continue;
          }
          const actions = descriptor.actions.filter((action) => Array.isArray(action.allowed_targets) && action.allowed_targets.includes("host") && ["read_only", "host_mutation"].includes(action.execution));
          if (actions.length) panels.push(panel(`${descriptor.provider} actions`, ...actions.map((action) => renderExtensionAction(context, action))));
        }
        everyday?.area.replaceChildren(...everydayPanels);
        everyday?.onRemainingReady(() => { for (const read of remaining.splice(0)) read(); });
        if (!panels.length && !everydayPanels.length && !everyday?.hasSignedSections)
          panels.push(el("p", "muted", "No plugin settings are available for this scope."));
        area.replaceChildren(...panels);
      } catch (error) {
        if (settingsRenderers.get(area) === renderer && current === generation && context.content.isConnected && area.isConnected) {
          const retry = errorPanel(error, load);
          if (everyday) everyday.area.replaceChildren(retry);
          area.replaceChildren(...(everyday ? [] : [retry]));
        }
      }
    }
    void load();
  }
  const applicationOverrideGuards = new WeakMap<HTMLElement, () => boolean>();

  const applicationOverrideSaves = new WeakMap<HTMLElement, () => boolean>();
  const applicationSectionDisposers = new WeakMap<HTMLElement, () => void>();

  export function disposeApplicationPluginSections(area: HTMLElement): void {
    applicationSectionDisposers.get(area)?.();
  }

  export function renderApplicationPluginSections(context: PageContext, area: HTMLElement,
    application: ApplicationExtensionContext, overrideCatalog?: HTMLElement): void {
    disposeApplicationPluginSections(area);
    const settings = el("div", "application-plugin-settings");
    const custom = el("div", "application-plugin-sections");
    const nativeDisposers: Array<() => void> = [];
    let unregister: (() => void) | undefined;
    area.replaceChildren(settings, custom);
    const renderFallback = (claimed: ReadonlySet<string>) => {
      if (context.api.allows("configuration.read"))
        renderApplicationPluginOverrides({ ...context, registerDraftGuard: (_check, cleanup) => {
          nativeDisposers.push(cleanup);
          return cleanup;
        } }, settings, application.id, overrideCatalog, true, claimed);
    };
    const pages = new ExtensionPages(context.api);
    const mounts = new Set<ApplicationExtensionMount>();
    let disposed = false;
    const dispose = () => {
      if (disposed) return;
      disposed = true;
      observer?.disconnect();
      pages.reset();
      for (const mount of mounts) mount.dispose();
      mounts.clear();
      for (const cleanup of nativeDisposers.splice(0)) cleanup();
      applicationOverrideGuards.delete(area);
      applicationOverrideSaves.delete(area);
      applicationSectionDisposers.delete(area);
      unregister?.();
    };
    const observer = typeof MutationObserver === "function" ? new MutationObserver(() => {
      if (!area.isConnected) dispose();
    }) : undefined;
    observer?.observe(context.content, { childList: true, subtree: true });
    const dirty = () => applicationPluginOverridesDirty(settings) || [...mounts].some((mount) => mount.isDirty());
    applicationOverrideGuards.set(area, dirty);
    applicationOverrideSaves.set(area, () => applicationPluginOverridesSaving(settings) || [...mounts].some((mount) => mount.isBusy()));
    applicationSectionDisposers.set(area, dispose);
    unregister = context.registerDraftGuard?.(() => area.isConnected && dirty(), dispose);
    void pages.discover().then((descriptors) => {
      if (disposed || !area.isConnected) return;
      const sections = descriptors.filter((entry) => entry.placements?.includes("application"));
      claimModuleSettings(context, new Set(sections.flatMap(entry => entry.settings_owner ?? entry.settings_provider ? [entry.settings_owner ?? entry.settings_provider!] : [])));
      renderFallback(new Set(sections.flatMap((entry) => entry.settings_provider ? [entry.settings_provider] : [])));
      for (const descriptor of sections) {
        const container = el("div");
        const section = pluginSettingsSection(descriptor.name, descriptor.default_expanded === true, container);
        custom.append(section);
        const mount = pages.mountApplication(descriptor.id, container, application);
        mounts.add(mount);
        void mount.ready.catch((error) => {
          mount.dispose();
          mounts.delete(mount);
          if (!disposed && container.isConnected) container.replaceChildren(errorPanel(error));
        });
      }
    }).catch((error) => {
      if (!disposed && custom.isConnected) {
        renderFallback(new Set());
        custom.replaceChildren(errorPanel(error));
      }
    });
  }

  export function applicationPluginOverridesSaving(area: HTMLElement): boolean {
    return applicationOverrideSaves.get(area)?.() ?? false;
  }

  export function applicationPluginOverridesDirty(area: HTMLElement): boolean {
    return applicationOverrideGuards.get(area)?.() ?? false;
  }

  export function renderApplicationPluginOverrides(context: PageContext, area: HTMLElement, applicationId: string, overrideCatalog?: HTMLElement, showAllOptions = false, claimedProviders: ReadonlySet<string> = new Set()): void {
    const scope: SessionExtensionScope = { kind: "application", id: applicationId };
    let busy = false;
    let disposed = false;
    const catalogSearch = overrideCatalog?.querySelector<HTMLInputElement>('input[type="search"]');
    const catalogChoices = overrideCatalog?.querySelector<HTMLElement>(".catalog-override-catalog");
    const sharedChoices = catalogSearch && catalogChoices ? { search: catalogSearch, list: catalogChoices } : undefined;
    const sharedGroups: HTMLElement[] = [];
    const sharedRefreshers: Array<() => void> = [];
    sharedChoices?.search.addEventListener("input", () => {
      for (const refresh of sharedRefreshers) refresh();
    });
    const dirtyChecks: Array<() => boolean> = [];
    const isDirty = () => busy || dirtyChecks.some((check) => check());
    applicationOverrideGuards.set(area, isDirty);
    applicationOverrideSaves.set(area, () => busy);
    context.registerDraftGuard?.(() => area.isConnected && isDirty(), () => {
      disposed = true;
      applicationOverrideGuards.delete(area);
      applicationOverrideSaves.delete(area);
    });

    async function load(): Promise<void> {
      if (disposed) return;
      for (const group of sharedGroups.splice(0)) group.remove();
      sharedRefreshers.length = 0;
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
        const providers = descriptors.filter((descriptor) => !claimedProviders.has(descriptor.provider) && Array.isArray(descriptor.options) &&
          descriptor.options.some((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes("application")));
        const documents: SessionExtensionSettings[] = [];
        // Keep discovery from exhausting the registry's bounded pending requests.
        for (let offset = 0; offset < providers.length; offset += 4) {
          const batch = providers.slice(offset, offset + 4);
          documents.push(...await Promise.all(batch.map((descriptor) => context.api.post<SessionExtensionSettings>(
            "session-extensions/settings_get", { provider: descriptor.provider, scope }))));
          if (disposed || !context.content.isConnected || !area.isConnected) return;
        }
        if (disposed || !context.content.isConnected || !area.isConnected) return;
        const panels = providers.map((descriptor, index) => {
          const document = documents[index];
          if (!document || !document.values || !document.effective || !Number.isSafeInteger(document.revision))
            throw new Error("Invalid extension settings response.");
          let savedRevision = document.revision;
          let effectiveAvailable = true;
          let baseline = JSON.stringify(document.values);
          const options = descriptor.options.filter((option) => Array.isArray(option.allowed_scopes) && option.allowed_scopes.includes("application"));
          const entries = new Map<string, { editor: ExtensionFieldEditor; overridden: boolean }>();
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
          const providerName = descriptor.display_name || descriptor.provider;
          const sharedGroup = sharedChoices ? el("div", "catalog-override-group", el("h4", "", literalText(providerName))) : undefined;
          const sharedButtons: Array<{ option: SessionExtensionOption; button: HTMLButtonElement }> = [];
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
            if (sharedChoices && sharedGroup) {
              const sharedQuery = sharedChoices.search.value.trim().toLocaleLowerCase();
              for (const entry of sharedButtons) {
                entry.button.hidden = !`${providerName} ${entry.option.label} ${entry.option.id} ${entry.option.help}`.toLocaleLowerCase().includes(sharedQuery);
                entry.button.setAttribute("data-selected", String(entries.has(entry.option.id)));
                entry.button.setAttribute("aria-label", `${entries.has(entry.option.id) ? "Edit" : "Add"} ${entry.option.label} override`);
                entry.button.disabled = busy || !effectiveAvailable || !context.api.allows("configuration.write");
              }
              sharedGroup.hidden = sharedButtons.every((entry) => entry.button.hidden);
            }
            describeInherited();
          };
          const addOption = (option: SessionExtensionOption, value: unknown, inherited = false) => {
            const editor = option.presentation === "keybinding"
              ? extensionKeybindingSettingEditor(option.definition, value)
              : option.presentation === "slider" ? extensionSliderEditor(option.definition, value, option.label)
              : option.presentation === "radio_group" ? extensionRadioGroupEditor(option.definition, value, option.label, option.choice_labels)
              : extensionFieldEditor(option.definition, value, option.choice_labels);
            const controls = editor.node.matches("input,select,textarea") ? [editor.node] : Array.from(editor.node.querySelectorAll("input,select,textarea"));
            if (controls.length === 1) controls[0]!.setAttribute("aria-label", option.label);
            const row = el("fieldset", "session-extension-option catalog-plugin-override-row", el("legend", "field-label", literalText(option.label)), editor.node);
            const entry = { editor, overridden: !inherited };
            const edited = () => { entry.overridden = true; };
            editor.node.addEventListener("input", edited);
            editor.node.addEventListener("change", edited);
            if (option.help) row.append(el("small", "muted", literalText(option.help)));
            row.append(el("small", "muted", sessionExtensionApplyPolicyHelp(option.apply_policy)));
            if (!showAllOptions) row.append(button("Remove override", () => { entries.delete(option.id); row.remove(); refreshChoices(); }));
            entries.set(option.id, entry);
            rows.append(row);
          };
          const add = button("Add override", () => {
            const option = options.find((candidate) => candidate.id === choices.value);
            const effective = option && document.effective[option.id];
            if (!effectiveAvailable || !option || !effective) return;
            addOption(option, structuredClone(effective.value));
            refreshChoices();
          });
          if (sharedChoices && sharedGroup) {
            for (const option of options) {
              const choice = button(option.label, () => {
                const effective = document.effective[option.id];
                if (busy || !context.api.allows("configuration.write")) return;
                if (!entries.has(option.id)) {
                  if (!effectiveAvailable || !effective) return;
                  addOption(option, structuredClone(effective.value));
                  refreshChoices();
                }
                const control = entries.get(option.id)?.editor.node;
                control?.scrollIntoView?.({ behavior: "smooth", block: "nearest" });
                (control?.matches("input,select,textarea") ? control : control?.querySelector<HTMLElement>("input,select,textarea"))?.focus();
              }, "secondary");
              choice.className = "button secondary catalog-override-choice";
              sharedButtons.push({ option, button: choice });
              sharedGroup.append(choice);
            }
            sharedGroups.push(sharedGroup);
            sharedRefreshers.push(refreshChoices);
          }
          search.addEventListener("input", refreshChoices);
          choices.addEventListener("change", describeInherited);
          const populate = () => {
            for (const option of options) {
              const overridden = Object.hasOwn(document.values, option.id);
              if (overridden || showAllOptions) {
                const effective = document.effective[option.id];
                if (!overridden && !effective) throw new Error("Missing effective plugin setting.");
                addOption(option, overridden ? document.values[option.id] : effective!.value, !overridden);
              }
            }
          };
          populate();
          refreshChoices();
          const values = (): Payload => {
            const result: Payload = { ...document.values };
            for (const option of options) {
              const entry = entries.get(option.id);
              if (entry?.overridden) result[option.id] = entry.editor.value();
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
          fields.append(rows, ...(sharedChoices || showAllOptions ? [] : [chooser]), actions);
          form.append(fields, status);
          form.addEventListener("submit", (event) => {
            event.preventDefault();
            if (busy || !context.api.allows("configuration.write")) return;
            void runAction(save, status, async () => {
              const submitted = values();
              if (JSON.stringify(submitted) === baseline) return;
              busy = true;
              fields.disabled = true;
              refreshChoices();
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
                  if (showAllOptions) { entries.clear(); rows.replaceChildren(); populate(); }
                  refreshChoices();
                } catch {
                  throw new Error("Plugin overrides were saved, but inherited values could not be refreshed. Reopen the application editor before adding an override.");
                }
                context.notify("Plugin overrides saved independently of application changes. Application to an active session or stream is not confirmed.");
              } finally {
                busy = false;
                fields.disabled = !context.api.allows("configuration.write");
                refreshChoices();
              }
            });
          });
          return showAllOptions
            ? pluginSettingsSection(descriptor.display_name || descriptor.provider, descriptor.default_expanded === true, form)
            : panel(literalText(descriptor.display_name || descriptor.provider), form);
        });
        if (sharedChoices) sharedChoices.list.append(...sharedGroups);
        if (showAllOptions && !panels.length) { area.replaceChildren(); return; }
        area.replaceChildren(el("p", "muted", showAllOptions ? "Plugin settings are saved separately from application changes." : "Plugin overrides are saved separately from application changes."),
          ...(panels.length ? panels : [el("p", "muted", "No plugin settings are available for this application.")]));
      } catch (error) {
        if (!disposed && context.content.isConnected && area.isConnected) area.replaceChildren(errorPanel(error, load));
      }
    }
    void load();
  }

}
