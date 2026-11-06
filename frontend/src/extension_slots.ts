namespace Amaterasu {
  const extensionSlotDisposers = new WeakMap<HTMLElement, () => void>();
  /** Generic signed-extension surfaces; the shell has no product renderer. */
  export function renderExtensionSlot(context: PageContext, container: HTMLElement, slot: "overview" | "settings", onOwners?: (owners: ReadonlySet<string>) => void): void {
    extensionSlotDisposers.get(container)?.();
    container.replaceChildren();
    const children: ExtensionPages[] = [];
    let owners: ReadonlySet<string> = new Set();
    let disposed = false;
    const dispose = () => {
      if (disposed) return;
      disposed = true;
      observer.disconnect();
      for (const child of children) child.dispose();
      discovery.dispose();
    };
    extensionSlotDisposers.set(container,dispose);
    const observer = new MutationObserver(() => {
      if (!container.isConnected) dispose();
      else if (slot === "settings" && typeof Amaterasu.claimModuleSettings === "function") Amaterasu.claimModuleSettings(context,owners);
    });
    observer.observe(document.body, { childList: true, subtree: true });
    context.registerDraftGuard?.(() => children.some(child => child.isDirty()), dispose);
    const discovery = new ExtensionPages(context.api);
    void discovery.discover().then(async (descriptors) => {
      if (disposed || !container.isConnected) return;
      owners = new Set(descriptors.filter(entry => entry.placements?.includes(slot)).flatMap(entry => {
        const owner = entry.settings_owner ?? entry.settings_provider; return owner ? [owner] : [];
      }));
      onOwners?.(owners);
      if (slot === "settings" && typeof Amaterasu.claimModuleSettings === "function") Amaterasu.claimModuleSettings(context,owners);
      for (const descriptor of descriptors) {
        if (disposed || !container.isConnected) return;
        if (!descriptor.placements?.includes(slot)) continue;
        const area = el("section", "plugin-surface");
        container.append(area);
        const child = new ExtensionPages(context.api);
        children.push(child);
        try {
          await child.discover();
          if (!disposed && area.isConnected) await child.mountSlot(descriptor.id, area, slot);
        } catch (error) {
          if (!disposed) area.replaceChildren(errorPanel(error));
        }
      }
    }).catch((error) => { if (!disposed) container.replaceChildren(errorPanel(error)); });
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
