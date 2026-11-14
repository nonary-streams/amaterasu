namespace Amaterasu {
  export function libraryRouteFromAddress(address: string): LibraryRoute | null {
    if (address === "catalog" || address === "applications" || address.startsWith("catalog?")) return "catalog";
    if (address === "catalog/integrations") return address;
    if (address === "steam" || address === "catalog/integrations/steam") return "catalog/integrations/steam";
    return null;
  }

  export function libraryViewFromAddress(address: string): LibraryView | null {
    const route = libraryRouteFromAddress(address);
    return route === "catalog" ? "games" : route === "catalog/integrations" ? "integrations" :
      route === "catalog/integrations/steam" ? "integrations/steam" : null;
  }

  export function libraryViewAvailable(api: Pick<Api, "allows">, view: LibraryView): boolean {
    if (view === "games") return api.allows("catalog.read");
    if (view === "integrations/steam") return api.allows("steam.read") || api.allows("steam.refresh");
    return api.allows("configuration.read") || api.allows("steam.read") || api.allows("steam.refresh");
  }

  export function renderLibrary(context: PageContext): void {
    const requested = context.initialLibraryView ?? "games";
    const view = libraryViewAvailable(context.api, requested) ? requested :
      libraryViewAvailable(context.api, "games") ? "games" : "integrations";
    const selectedTab = view === "games" ? "games" : "integrations";
    const tabList = el("div", "library-tabs");
    tabList.setAttribute("role", "tablist");
    setLocalizedAttribute(tabList, "aria-label", "Library views");
    const area = el("div", "library-view");
    area.id = "library-view";
    area.setAttribute("role", "tabpanel");
    area.setAttribute("aria-labelledby", `library-tab-${selectedTab}`);
    const tabs: HTMLButtonElement[] = [];
    for (const [name, label, route] of [
      ["games", "Games", "catalog"],
      ["integrations", "Integrations", "catalog/integrations"],
    ] as const) {
      if (!libraryViewAvailable(context.api, name)) continue;
      const tab = button(label, () => context.navigate(route), "library-tab");
      tab.id = `library-tab-${name}`;
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-controls", area.id);
      tab.setAttribute("aria-selected", String(name === selectedTab));
      tab.tabIndex = name === selectedTab ? 0 : -1;
      tab.addEventListener("keydown", (event) => {
        const index = tabs.indexOf(tab);
        const next = event.key === "ArrowRight" ? (index + 1) % tabs.length :
          event.key === "ArrowLeft" ? (index + tabs.length - 1) % tabs.length :
          event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1 : null;
        if (next === null) return;
        event.preventDefault();
        tabs[next]?.focus();
        tabs[next]?.click();
      });
      tabs.push(tab);
      tabList.append(tab);
    }
    context.content.append(sectionHeading("Library", "Manage your games and library integrations."), tabList, area);
    const childContext = { ...context, content: area };
    if (view === "games") renderCatalog(childContext, true);
    else if (view === "integrations/steam") {
      area.append(button("Back to integrations", () => context.navigate("catalog/integrations"), "quiet"));
      renderSteam(childContext, true);
    } else renderLibraryIntegrations(childContext, true);
  }
}
