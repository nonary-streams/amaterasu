namespace Amaterasu {
  const extensionSlotDisposers = new WeakMap<HTMLElement, () => void>();
  const extensionSlotDrafts = new WeakMap<HTMLElement, () => boolean>();
  export function extensionSlotDirty(container: HTMLElement): boolean {
    return extensionSlotDrafts.get(container)?.() ?? false;
  }
  /** Generic signed-extension surfaces; the shell has no product renderer. */
  export function renderExtensionSlot(context: PageContext, container: HTMLElement, slot: "overview" | "settings", onOwners?: (owners: ReadonlySet<string>) => void): void {
    extensionSlotDisposers.get(container)?.();
    container.replaceChildren();
    const children: ExtensionPages[] = [];
    const owners = new Set<string>();
    let disposed = false;
    const isDirty = () => children.some(child => child.isDirty());
    extensionSlotDrafts.set(container, isDirty);
    const discovery = new ExtensionPages(context.api);
    let unregister: (() => void) | undefined;
    const dispose = () => {
      if (disposed) return;
      disposed = true;
      extensionSlotDrafts.delete(container);
      observer.disconnect();
      for (const child of children) child.dispose();
      discovery.reset();
      unregister?.();
    };
    extensionSlotDisposers.set(container, dispose);
    const claimOwners = () => {
      if (slot === "settings") claimModuleSettings(context, owners);
    };
    const observer = new MutationObserver(() => {
      if (!container.isConnected) dispose();
      else claimOwners();
    });
    observer.observe(document.body, { childList: true, subtree: true });
    unregister = context.registerDraftGuard?.(isDirty, dispose);
    void discovery.discover().then(descriptors => {
      if (disposed || !container.isConnected) return;
      const entries = descriptors.filter(entry => entry.placements?.includes(slot));
      onOwners?.(new Set(entries.flatMap(entry => entry.settings_provider ? [entry.settings_provider] : [])));
      for (const descriptor of entries) {
        const area = el("section", "plugin-surface");
        const section = slot === "settings"
          ? pluginSettingsSection(descriptor.name, descriptor.default_expanded === true, area)
          : null;
        container.append(section ?? area);
        const child = discovery.createSurface();
        children.push(child);
        const mount = async () => {
          try {
            area.replaceChildren();
            await child.mountSlot(descriptor.id, area, slot);
            if (disposed || !area.isConnected) return;
            const owner = descriptor.settings_owner ?? descriptor.settings_provider;
            if (owner) owners.add(owner);
            claimOwners();
          } catch (error) {
            if (disposed || !area.isConnected) return;
            child.dispose();
            if (section) section.open = true;
            area.replaceChildren(errorPanel(error, mount));
            if (slot === "settings" && descriptor.settings_provider) {
              const fallback = el("div");
              area.append(fallback);
              renderSessionExtensionSettings(context, fallback, { kind: "host" }, descriptor.settings_provider);
            }
          }
        };
        void mount();
      }
    }).catch(error => {
      if (disposed) return;
      onOwners?.(new Set());
      container.replaceChildren(errorPanel(error, () => renderExtensionSlot(context, container, slot, onOwners)));
    });
  }

  export function loadExtensionStylesheet(id: string, digest: string, path: string, signal: AbortSignal): Promise<void> {
    if (signal.aborted || !/^[a-zA-Z0-9._/-]+\.css$/.test(path) ||
        path.split("/").some((part) => !part || part === "." || part === ".."))
      return Promise.reject(new Error("Invalid plugin stylesheet."));
    const base = document.querySelector<HTMLMetaElement>('meta[name="amaterasu-api-base"]')?.content ?? "";
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = `${base}/v1/extensions/${encodeURIComponent(id)}/${digest}/assets/${path.split("/").map(encodeURIComponent).join("/")}`;
    return new Promise((resolve, reject) => {
      const abort = () => { link.remove(); reject(new Error("Plugin surface closed.")); };
      signal.addEventListener("abort", abort, { once: true });
      link.onload = () => resolve();
      link.onerror = () => { link.remove(); signal.removeEventListener("abort", abort); reject(new Error("Plugin stylesheet is unavailable.")); };
      document.head.append(link);
    });
  }
}
