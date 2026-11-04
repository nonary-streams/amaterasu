namespace Amaterasu {
  export interface ExtensionUiTabItem {
    id: string;
    label: ExtensionUiText;
    content: HTMLElement;
    disabled?: boolean;
  }

  export interface ExtensionUiTabsOptions {
    label: ExtensionUiText;
    tabs: readonly ExtensionUiTabItem[];
    activeId?: string;
    onChange?: (id: string) => void;
  }

  export interface ExtensionUiTabsControl {
    readonly element: HTMLElement;
    active(): string | null;
    select(id: string): void;
    setTabs(tabs: readonly ExtensionUiTabItem[], activeId?: string): void;
    setDisabled(id: string, disabled: boolean): void;
    dispose(): void;
  }

  export interface ExtensionUiTabsV1 {
    tabs(options: ExtensionUiTabsOptions): ExtensionUiTabsControl;
  }

  interface ExtensionUiTabEntry {
    id: string;
    disabled: boolean;
    button: HTMLButtonElement;
    panel: HTMLElement;
  }

  let nextExtensionUiTabsId = 0;

  function validateExtensionUiTabText(value: ExtensionUiText): void {
    if (typeof value === "string") {
      if (!value.trim()) throw new Error("Plugin tab labels cannot be empty.");
      return;
    }
    if (!value || typeof value !== "object" || typeof value.source !== "string" || !value.source.trim())
      throw new Error("Invalid plugin tab label.");
    const translations = value.translations ?? {};
    if (!translations || typeof translations !== "object" || Array.isArray(translations) ||
        Object.values(translations).some((translation) => typeof translation !== "string" || !translation.trim()))
      throw new Error("Invalid plugin tab translations.");
  }

  function extensionUiTabsText(value: ExtensionUiText): Text | string {
    validateExtensionUiTabText(value);
    if (typeof value === "string") return localizedTextNode(value);
    const translations = value.translations ?? {};
    return localeFormattedTextNode((locale) =>
      translations[locale] ?? translations[locale.split("-")[0] ?? locale] ?? translateText(value.source));
  }

  function validateExtensionUiTabs(tabs: readonly ExtensionUiTabItem[]): void {
    if (!Array.isArray(tabs) || tabs.length < 1 || tabs.length > 64)
      throw new Error("Plugin tabs must contain between one and 64 items.");
    const ids = new Set<string>();
    const contents = new Set<HTMLElement>();
    for (const tab of tabs) {
      if (!tab || typeof tab !== "object" || typeof tab.id !== "string" ||
          !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(tab.id) || ids.has(tab.id) ||
          typeof tab.disabled !== "undefined" && typeof tab.disabled !== "boolean" ||
          !tab.content || typeof tab.content !== "object" || tab.content.nodeType !== 1 ||
          typeof tab.content.nodeName !== "string" || contents.has(tab.content))
        throw new Error("Invalid plugin tab item.");
      validateExtensionUiTabText(tab.label);
      ids.add(tab.id);
      contents.add(tab.content);
    }
  }

  export function createExtensionUiTabsV1(signal: AbortSignal): ExtensionUiTabsV1 {
    if (!signal || typeof signal.addEventListener !== "function" || typeof signal.removeEventListener !== "function")
      throw new Error("Plugin tabs require a lifecycle signal.");
    if (signal.aborted) throw new Error("Plugin page is no longer active.");

    const makeController = (): AbortController => {
      if (signal.aborted) throw new Error("Plugin page is no longer active.");
      return new AbortController();
    };
    const tabs: ExtensionUiTabsV1 = {
      tabs(options) {
        if (!options || typeof options !== "object" || Array.isArray(options))
          throw new Error("Invalid plugin tabs options.");
        if (typeof options.onChange !== "undefined" && typeof options.onChange !== "function")
          throw new Error("Invalid plugin tab change callback.");
        validateExtensionUiTabText(options.label);
        validateExtensionUiTabs(options.tabs);
        const controller = makeController();
        const prefix = `amaterasu-plugin-tabs-${++nextExtensionUiTabsId}`;
        const labelId = `${prefix}-label`;
        const label = el("span", "field-label plugin-tabs-label", extensionUiTabsText(options.label));
        label.id = labelId;
        const tablist = el("div", "plugin-tablist");
        tablist.setAttribute("role", "tablist");
        tablist.setAttribute("aria-labelledby", labelId);
        const panels = el("div", "plugin-tab-panels");
        const element = el("div", "field plugin-tabs", label, tablist, panels);
        let entries: ExtensionUiTabEntry[] = [];
        let activeId: string | null = null;
        let disposed = false;

        const enabledEntry = (id: string | null): ExtensionUiTabEntry | undefined =>
          id === null ? undefined : entries.find((entry) => entry.id === id && !entry.disabled);
        const firstEnabled = (): ExtensionUiTabEntry | undefined => entries.find((entry) => !entry.disabled);
        const updateSelection = (): void => {
          for (const entry of entries) {
            const selected = entry.id === activeId;
            entry.button.disabled = entry.disabled;
            entry.button.setAttribute("aria-selected", String(selected));
            entry.button.tabIndex = selected ? 0 : -1;
            entry.panel.hidden = !selected;
          }
        };
        const makeEntry = (tab: ExtensionUiTabItem): ExtensionUiTabEntry => {
          const encodedId = encodeURIComponent(tab.id);
          const button = el("button", "plugin-tab", extensionUiTabsText(tab.label));
          button.type = "button";
          button.id = `${prefix}-tab-${encodedId}`;
          button.setAttribute("role", "tab");
          button.setAttribute("data-tab-id", tab.id);
          button.setAttribute("aria-controls", `${prefix}-panel-${encodedId}`);
          const panel = el("div", "plugin-tabpanel", tab.content);
          panel.id = `${prefix}-panel-${encodedId}`;
          panel.setAttribute("role", "tabpanel");
          panel.setAttribute("aria-labelledby", button.id);
          panel.tabIndex = 0;
          return { id: tab.id, disabled: tab.disabled === true, button, panel };
        };
        const chooseNextActive = (requested: string | undefined, nextTabs: readonly ExtensionUiTabItem[]): string | null => {
          if (requested !== undefined) {
            const tab = nextTabs.find((candidate) => candidate.id === requested);
            if (!tab || tab.disabled === true) throw new Error("Plugin tab selection must name an enabled tab.");
            return tab.id;
          }
          const retained = nextTabs.find((tab) => tab.id === activeId && tab.disabled !== true);
          return retained?.id ?? nextTabs.find((tab) => tab.disabled !== true)?.id ?? null;
        };
        const commitTabs = (nextTabs: readonly ExtensionUiTabItem[], requested: string | undefined): void => {
          validateExtensionUiTabs(nextTabs);
          const focusedElement = document.activeElement instanceof HTMLElement && element.contains(document.activeElement)
            ? document.activeElement
            : null;
          const focusedTabId = focusedElement
            ? entries.find((entry) => entry.button === focusedElement)?.id ?? null
            : null;
          const focusedPanelId = focusedElement
            ? entries.find((entry) => entry.panel.contains(focusedElement))?.id ?? null
            : null;
          const nextActiveId = chooseNextActive(requested, nextTabs);
          const currentById = new Map(entries.map((entry) => [entry.id, entry]));
          const nextEntries = nextTabs.map((tab) => {
            const existing = currentById.get(tab.id);
            if (!existing) return makeEntry(tab);
            return { ...existing, disabled: tab.disabled === true };
          });

          const nextIds = new Set(nextEntries.map((entry) => entry.id));
          for (const entry of entries) {
            if (!nextIds.has(entry.id)) {
              entry.button.remove();
              entry.panel.remove();
            }
          }
          for (let index = 0; index < nextEntries.length; index++) {
            const entry = nextEntries[index]!;
            const tab = nextTabs[index]!;
            entry.button.replaceChildren(extensionUiTabsText(tab.label));
            entry.button.disabled = entry.disabled;
            if (entry.panel.childNodes.length !== 1 || entry.panel.firstElementChild !== tab.content)
              entry.panel.replaceChildren(tab.content);
            if (tablist.children[index] !== entry.button)
              tablist.insertBefore(entry.button, tablist.children[index] ?? null);
            if (panels.children[index] !== entry.panel)
              panels.insertBefore(entry.panel, panels.children[index] ?? null);
          }
          entries = nextEntries;
          activeId = nextActiveId;
          updateSelection();
          if (focusedElement && focusedElement.isConnected && document.activeElement === document.body) {
            const panelFocusStillVisible = focusedPanelId !== null && focusedPanelId === activeId &&
              entries.some((entry) => entry.id === focusedPanelId && entry.panel.contains(focusedElement));
            if (panelFocusStillVisible) focusedElement.focus();
            else {
              const focusedTab = focusedTabId === null ? undefined : entries.find((entry) => entry.id === focusedTabId && !entry.disabled);
              if (focusedTab) focusedTab.button.focus();
              else if (focusedPanelId !== null || focusedTabId !== null) enabledEntry(activeId)?.button.focus();
            }
          }
        };
        const activate = (id: string, focus: boolean, notify: boolean): void => {
          const entry = entries.find((candidate) => candidate.id === id);
          if (!entry) throw new Error("Unknown plugin tab.");
          if (entry.disabled) throw new Error("Cannot select a disabled plugin tab.");
          const changed = activeId !== id;
          activeId = id;
          updateSelection();
          if (focus) entry.button.focus();
          if (changed && notify) options.onChange?.(id);
        };
        const moveFocus = (id: string): void => {
          const entry = entries.find((candidate) => candidate.id === id);
          if (!entry || entry.disabled) return;
          activate(id, true, true);
        };
        const onClick = (event: MouseEvent): void => {
          const target = event.target instanceof Element ? event.target.closest<HTMLElement>('[role="tab"]') : null;
          if (!target || !tablist.contains(target)) return;
          const id = target.getAttribute("data-tab-id");
          if (id !== null) {
            const entry = entries.find((candidate) => candidate.id === id);
            if (entry && !entry.disabled) activate(id, false, true);
          }
        };
        const onKeyDown = (event: KeyboardEvent): void => {
          const target = event.target instanceof Element ? event.target.closest<HTMLElement>('[role="tab"]') : null;
          if (!target || !tablist.contains(target)) return;
          const id = target.getAttribute("data-tab-id");
          if (id === null) return;
          const enabled = entries.filter((entry) => !entry.disabled);
          if (enabled.length < 1) return;
          const index = enabled.findIndex((entry) => entry.id === id);
          if (index < 0) return;
          let next: ExtensionUiTabEntry | undefined;
          const rtl = getComputedStyle(tablist).direction === "rtl";
          if (event.key === "Home") next = enabled[0];
          else if (event.key === "End") next = enabled.at(-1);
          else if (event.key === "ArrowRight") next = enabled[(index + (rtl ? -1 : 1) + enabled.length) % enabled.length];
          else if (event.key === "ArrowLeft") next = enabled[(index + (rtl ? 1 : -1) + enabled.length) % enabled.length];
          else return;
          event.preventDefault();
          if (next) moveFocus(next.id);
        };
        tablist.addEventListener("click", onClick, { signal: controller.signal });
        tablist.addEventListener("keydown", onKeyDown, { signal: controller.signal });
        const setTabs = (nextTabs: readonly ExtensionUiTabItem[], requested?: string): void => {
          if (!disposed) commitTabs(nextTabs, requested);
        };
        const setDisabled = (id: string, disabled: boolean): void => {
          if (disposed) return;
          if (typeof disabled !== "boolean") throw new Error("Plugin tab disabled state must be boolean.");
          const entry = entries.find((candidate) => candidate.id === id);
          if (!entry) throw new Error("Unknown plugin tab.");
          if (entry.disabled === disabled) return;
          const focused = document.activeElement === entry.button || entry.panel.contains(document.activeElement);
          entry.disabled = disabled;
          entry.button.disabled = disabled;
          if (disabled && activeId === id) activeId = firstEnabled()?.id ?? null;
          else if (!disabled && activeId === null) activeId = id;
          updateSelection();
          if (focused) enabledEntry(activeId)?.button.focus();
        };
        const select = (id: string): void => {
          if (!disposed) activate(id, false, false);
        };
        const dispose = (): void => {
          if (disposed) return;
          disposed = true;
          controller.abort();
          signal.removeEventListener("abort", dispose);
        };
        if (signal.aborted) dispose();
        else signal.addEventListener("abort", dispose, { once: true });
        commitTabs(options.tabs, options.activeId);
        return Object.freeze({
          element,
          active: () => activeId,
          select,
          setTabs,
          setDisabled,
          dispose,
        });
      },
    };
    return Object.freeze(tabs);
  }
}

