namespace Amaterasu {
  // The plugin manager: what this host runs, what its repositories offer, and
  // the repositories themselves.

  export const PLUGINS_VIEWS: readonly PluginsView[] = ["installed", "available", "repositories"];

  type Loadable<T> =
    | { state: "loading" }
    | { state: "ready"; value: T }
    | { state: "failed"; error: unknown };

  type Part = Node | TextValue | null | undefined;

  const VIEW_LABELS: Readonly<Record<PluginsView, string>> = {
    installed: "Installed",
    available: "Available",
    repositories: "Repositories",
  };
  const CHANGE_POLL_MS = 600;
  const CHANGE_TIMEOUT_MS = 150_000;
  const COLLAPSED_NAMES = 8;

  function lit(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function formatDate(unixSeconds: number): string {
    const locale = typeof Amaterasu.activeIntlLocale === "function" ? Amaterasu.activeIntlLocale() : undefined;
    try {
      return new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(new Date(unixSeconds * 1000));
    } catch {
      return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
    }
  }

  function factList(rows: readonly (readonly [string, Part] | null)[]): HTMLElement {
    const list = el("dl", "plugin-facts");
    for (const row of rows) {
      if (!row) continue;
      list.append(el("div", "plugin-fact", el("dt", "", row[0]), el("dd", "", row[1])));
    }
    return list;
  }

  function detailSection(title: string, ...children: Part[]): HTMLElement {
    return el("section", "plugin-detail-section", el("h3", "plugin-label", title), ...children);
  }

  function quietState(title: string, description: string, action?: HTMLElement): HTMLElement {
    return el("div", "plugin-quiet-state", el("h3", "", title), el("p", "muted", description), action);
  }

  /// A list of service or module names that stays short until asked.
  function nameList(names: readonly string[], render: (name: string) => Part): HTMLElement {
    const list = el("ul", "plugin-names");
    const show = (count: number) => list.replaceChildren(...names.slice(0, count).map((name) => el("li", "", render(name))));
    if (names.length <= COLLAPSED_NAMES + 2) {
      show(names.length);
      return list;
    }
    show(COLLAPSED_NAMES);
    const more = button(lit(`Show all ${names.length}`), () => {
      show(names.length);
      more.remove();
    }, "link-button plugin-more");
    return el("div", "", list, more);
  }

  interface BrowserEntry {
    key: string;
    name: string;
    summary: TextValue;
    meta: string;
    group: string;
    emphasis: boolean;
  }

  interface PluginBrowser {
    node: HTMLElement;
    show(entries: readonly BrowserEntry[], emptyState: HTMLElement): void;
    select(key: string | null): void;
  }

  /// A list of plugins with the selected one's details beside it. On a narrow
  /// screen the details open under the selected row instead.
  function pluginBrowser(options: {
    label: string;
    grouped: boolean;
    selected: () => string | null;
    onSelect: (key: string | null) => void;
    detail: (key: string) => HTMLElement | null;
    signal: AbortSignal;
  }): PluginBrowser {
    const list = el("div", "plugin-list");
    setLocalizedAttribute(list, "aria-label", options.label);
    list.setAttribute("role", "group");
    const pane = el("aside", "plugin-detail-pane");
    const node = el("div", "plugin-browser", list, pane);
    const wideLayout = typeof window.matchMedia === "function" ? window.matchMedia("(min-width: 1101px)") : null;
    const isWide = () => wideLayout?.matches ?? true;
    const rows = new Map<string, { item: HTMLElement; control: HTMLButtonElement }>();
    let order: string[] = [];
    let detail: HTMLElement | null = null;

    function place(): void {
      const selected = options.selected();
      for (const [key, row] of rows) {
        const current = key === selected;
        row.control.classList.toggle("selected", current);
        if (current) row.control.setAttribute("aria-current", "true");
        else row.control.removeAttribute("aria-current");
        if (isWide()) row.control.removeAttribute("aria-expanded");
        else row.control.setAttribute("aria-expanded", String(current));
      }
      const row = selected ? rows.get(selected) : undefined;
      if (!row || !detail) {
        detail?.remove();
        pane.replaceChildren();
      } else if (isWide()) {
        pane.replaceChildren(detail);
      } else {
        pane.replaceChildren();
        row.item.append(detail);
      }
    }

    function select(key: string | null): void {
      options.onSelect(key);
      detail?.remove();
      detail = key ? options.detail(key) : null;
      place();
    }

    function activate(key: string): void {
      if (key !== options.selected()) select(key);
      else if (!isWide()) select(null);
    }

    list.addEventListener("keydown", (event) => {
      const offset = event.key === "ArrowDown" ? 1 : event.key === "ArrowUp" ? -1 : 0;
      const target = (event.target as HTMLElement).closest<HTMLButtonElement>(".plugin-row");
      if (!target || event.altKey || event.ctrlKey || event.metaKey) return;
      const index = order.findIndex((key) => rows.get(key)?.control === target);
      const next = offset ? index + offset : event.key === "Home" ? 0 : event.key === "End" ? order.length - 1 : -1;
      const key = order[next];
      const row = key === undefined ? undefined : rows.get(key);
      if (index < 0 || !row || key === undefined) return;
      event.preventDefault();
      row.control.focus();
      if (isWide()) select(key);
    }, { signal: options.signal });
    wideLayout?.addEventListener("change", () => {
      if (isWide() && !options.selected() && order[0] !== undefined) select(order[0]);
      else place();
    }, { signal: options.signal });

    return {
      node,
      select,
      show(entries, emptyState) {
        rows.clear();
        order = entries.map((entry) => entry.key);
        node.classList.toggle("plugin-browser--empty", entries.length === 0);
        if (!entries.length) {
          list.replaceChildren(emptyState);
          select(null);
          return;
        }
        const sections: HTMLElement[] = [];
        let group: string | null = null;
        let items: HTMLElement | null = null;
        for (const entry of entries) {
          if (!items || (options.grouped && entry.group !== group)) {
            group = entry.group;
            items = el("ul", "plugin-rows");
            if (options.grouped) sections.push(el("h3", "plugin-label plugin-group-heading", entry.group));
            sections.push(items);
          }
          const control = button("", () => activate(entry.key), "plugin-row");
          control.replaceChildren(
            el("span", "plugin-row-text",
              el("span", "plugin-row-name", lit(entry.name)),
              el("span", "plugin-row-summary", entry.summary)),
            el("span", `plugin-row-meta${entry.emphasis ? " plugin-row-meta--emphasis" : ""}`, lit(entry.meta)),
          );
          const item = el("li", "", control);
          items.append(item);
          rows.set(entry.key, { item, control });
        }
        list.replaceChildren(...sections);
        const selected = options.selected();
        if (selected && rows.has(selected)) select(selected);
        else select(isWide() ? entries[0]?.key ?? null : null);
      },
    };
  }

  export function renderPlugins(context: PageContext): void {
    const page = new AbortController();
    context.registerDraftGuard?.(() => false, () => page.abort());
    const api = context.api;
    const canRead = api.allows("configuration.read");
    const canManage = api.role === "administrator" && api.allows("configuration.write");

    let view: PluginsView = context.initialPluginsView ?? "installed";
    let selection: Loadable<PackageSelectionStatus> = { state: "loading" };
    let listing: Loadable<RepositoryPage> = { state: "loading" };
    let loadRequest = 0;
    let selectedInstalled: string | null = null;
    let selectedAvailable: string | null = null;
    const installedFilter: PluginFilter = { query: "", category: "" };
    const availableFilter: PluginFilter = { query: "", category: "" };
    const descriptions = new Map<string, ModuleDescription>();
    let describeSupported = true;
    let changing: { repository: string; kind: RepositoryChangeKind } | null = null;
    const changeFailures = new Map<string, string>();

    const consoleDigest = consoleDigestFromAssets(
      Array.from(document.querySelectorAll<HTMLScriptElement>("script[src]"), (script) => script.src));

    const tabs = new Map<PluginsView, { control: HTMLButtonElement; count: HTMLElement }>();
    const tabList = el("div", "plugin-tabs");
    tabList.setAttribute("role", "tablist");
    setLocalizedAttribute(tabList, "aria-label", "Plugin views");
    const viewArea = el("div", "plugin-view");
    viewArea.setAttribute("role", "tabpanel");
    viewArea.id = "plugin-view";
    for (const name of PLUGINS_VIEWS) {
      const count = el("span", "plugin-tab-count");
      const control = button("", () => showView(name), "plugin-tab");
      control.replaceChildren(el("span", "", VIEW_LABELS[name]), count);
      control.setAttribute("role", "tab");
      control.setAttribute("aria-controls", viewArea.id);
      control.id = `plugin-tab-${name}`;
      tabs.set(name, { control, count });
      tabList.append(control);
    }
    tabList.addEventListener("keydown", (event) => {
      const offset = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
      if (!offset) return;
      event.preventDefault();
      const next = PLUGINS_VIEWS[(PLUGINS_VIEWS.indexOf(view) + offset + PLUGINS_VIEWS.length) % PLUGINS_VIEWS.length];
      if (next) { showView(next); tabs.get(next)?.control.focus(); }
    });

    context.content.append(
      sectionHeading(
        "Plugins",
        "The modules this host runs, and the repositories it can get more from.",
        button("Refresh", load),
      ),
      tabList,
      viewArea,
    );

    const ready = <T>(value: Loadable<T>): T | null => value.state === "ready" ? value.value : null;
    const repositories = (): ApprovedRepositoryView[] => ready(listing)?.repositories ?? [];
    const activeRepositories = (): ApprovedRepositoryView[] => repositories().filter((repository) => !repository.revoked);
    const offers = (): AvailablePlugin[] => availablePlugins(repositories(), ready(selection), consoleDigest);

    function showView(next: PluginsView): void {
      if (next === view) return;
      view = next;
      context.replacePluginsView?.(next);
      render();
    }

    function render(): void {
      const counts: Record<PluginsView, number | null> = {
        installed: ready(selection)?.running.modules?.length ?? null,
        available: ready(listing) ? offers().length : null,
        repositories: ready(listing) ? activeRepositories().length : null,
      };
      for (const [name, tab] of tabs) {
        const selected = name === view;
        tab.control.setAttribute("aria-selected", String(selected));
        tab.control.tabIndex = selected ? 0 : -1;
        tab.count.textContent = counts[name] === null ? "" : String(counts[name]);
      }
      viewArea.setAttribute("aria-labelledby", `plugin-tab-${view}`);
      if (!canRead) {
        viewArea.replaceChildren(quietState("Plugins are unavailable",
          "This account needs permission to read the host's configuration."));
        return;
      }
      if (view === "installed") renderInstalled();
      else if (view === "available") renderAvailable();
      else renderRepositories();
    }

    // --- Loading -------------------------------------------------------------

    async function load(): Promise<void> {
      const request = ++loadRequest;
      selection = { state: "loading" };
      listing = { state: "loading" };
      descriptions.clear();
      render();
      if (!canRead) return;
      // Both answers arrive together so the page is drawn once: a second
      // draw would take the focus out of a search the reader has started.
      const settled = <T>(work: Promise<T>): Promise<Loadable<T>> => work.then(
        (value) => ({ state: "ready", value }),
        (error: unknown) => ({ state: "failed", error }),
      );
      const [nextSelection, nextListing] = await Promise.all([
        settled(api.post<unknown>("package-selection/status", {}).then(parsePackageSelectionStatus)),
        settled(listRepositories()),
      ]);
      if (request !== loadRequest || !viewArea.isConnected) return;
      selection = nextSelection;
      listing = nextListing;
      render();
    }

    /// Reads every page of the listing; a repository's choices can span pages.
    async function listRepositories(): Promise<RepositoryPage> {
      const byId = new Map<string, ApprovedRepositoryView>();
      let offset = 0;
      while (true) {
        const pageReply = parseRepositoryPage(await api.post<unknown>("repositories/list", { offset }));
        for (const repository of pageReply.repositories) {
          const previous = byId.get(repository.id);
          if (previous && (previous.fingerprint !== repository.fingerprint || previous.revoked !== repository.revoked))
            throw new Error("The repository list changed while it was being read. Refresh to try again.");
          byId.set(repository.id, previous
            ? { ...previous, candidates: [...previous.candidates, ...repository.candidates] }
            : repository);
        }
        if (pageReply.nextOffset === null)
          return { ...pageReply, repositories: [...byId.values()] };
        if (pageReply.nextOffset <= offset) throw new Error("The repository list did not advance.");
        offset = pageReply.nextOffset;
      }
    }

    async function reloadRepositories(): Promise<void> {
      const request = loadRequest;
      try {
        const value = await listRepositories();
        if (request === loadRequest) listing = { state: "ready", value };
      } catch (error) {
        if (request === loadRequest) listing = { state: "failed", error };
      }
      if (viewArea.isConnected) render();
    }

    function copyable(value: string, shown: string, what: string): HTMLElement {
      const text = el("code", "plugin-digest", lit(shown));
      text.title = value;
      const copy = button("Copy", async () => {
        try {
          await navigator.clipboard.writeText(value);
          context.notify(lit(`Copied the ${what}.`));
        } catch {
          context.notify("Copying is not available in this browser.");
        }
      }, "link-button plugin-copy");
      copy.setAttribute("aria-label", `Copy the ${what}`);
      return el("span", "plugin-copyable", text, copy);
    }

    function repositoryLink(id: string): HTMLElement {
      return button(lit(id), () => showView("repositories"), "link-button plugin-link");
    }

    function toolbar(
      filter: PluginFilter,
      placeholder: string,
      categories: readonly PluginCategory[],
      onChange: () => void,
      count: HTMLElement,
    ): HTMLElement {
      const search = input("plugin_search", filter.query, "search");
      setLocalizedAttribute(search, "placeholder", placeholder);
      setLocalizedAttribute(search, "aria-label", placeholder);
      search.setAttribute("data-draft-ignore", "");
      search.addEventListener("input", () => { filter.query = search.value; onChange(); });
      const bar = el("div", "plugin-toolbar", search);
      if (categories.length > 1) {
        const category = el("select", "plugin-filter");
        setLocalizedAttribute(category, "aria-label", "Category");
        category.setAttribute("data-draft-ignore", "");
        const all = el("option", "", "All categories");
        all.value = "";
        category.append(all, ...categories.map((name) => {
          const option = el("option", "", name);
          option.value = name;
          return option;
        }));
        category.value = filter.category;
        category.addEventListener("change", () => { filter.category = category.value as PluginCategory | ""; onChange(); });
        bar.append(category);
      }
      bar.append(count);
      return bar;
    }

    // --- Installed -------------------------------------------------------------

    function renderInstalled(): void {
      if (selection.state === "loading") {
        viewArea.replaceChildren(el("p", "muted plugin-loading", "Loading installed plugins…"));
        return;
      }
      if (selection.state === "failed") {
        viewArea.replaceChildren(errorPanel(selection.error, load));
        return;
      }
      const status = selection.value;
      const plugins = installedPlugins(status);
      if (!status.running.available || !plugins.length) {
        viewArea.replaceChildren(
          packageSummary(status, 0),
          quietState("The host did not report its modules",
            "This host version does not list the modules it loaded, or the running package could not be read."),
        );
        return;
      }
      const byId = new Map(plugins.map((plugin) => [plugin.id, plugin]));
      const count = el("span", "muted plugin-count");
      const browser = pluginBrowser({
        label: "Installed plugins",
        grouped: true,
        selected: () => selectedInstalled,
        onSelect: (key) => { selectedInstalled = key; },
        detail: (key) => {
          const plugin = byId.get(key);
          return plugin ? installedDetail(plugin, status, byId, focusInstalled) : null;
        },
        signal: page.signal,
      });
      const categories = PLUGIN_CATEGORIES.filter((category) => plugins.some((plugin) => plugin.category === category));
      const bar = toolbar(installedFilter, "Search installed plugins", categories, showMatches, count);

      function showMatches(): void {
        const matches = plugins.filter((plugin) => matchesPluginFilter(plugin, installedFilter));
        count.textContent = matches.length === plugins.length
          ? `${plugins.length} installed`
          : `${matches.length} of ${plugins.length}`;
        browser.show(
          matches.map((plugin) => ({
            key: plugin.id,
            name: plugin.name,
            summary: plugin.summary || lit(plugin.id),
            meta: plugin.version,
            group: plugin.category,
            emphasis: false,
          })),
          quietState("No plugins match", "Try a different search or category."),
        );
      }

      /// Selects a plugin named in another plugin's details, clearing a
      /// filter that would hide it.
      function focusInstalled(id: string): void {
        const plugin = byId.get(id);
        if (!plugin) return;
        if (!matchesPluginFilter(plugin, installedFilter)) {
          installedFilter.query = "";
          installedFilter.category = "";
          selectedInstalled = id;
          renderInstalled();
          return;
        }
        browser.select(id);
      }

      viewArea.replaceChildren(packageSummary(status, plugins.length), bar, browser.node);
      showMatches();
    }

    function packageSummary(status: PackageSelectionStatus, modules: number): HTMLElement {
      const running = status.running;
      const line = el("p", "plugin-package-line");
      if (running.digest) {
        line.append(
          el("span", "muted", "Running package"), " ",
          copyable(running.digest, shortDigest(running.digest), "package digest"),
        );
        if (modules) line.append(el("span", "muted", lit(` · ${modules} modules`)));
        if (running.source) line.append(el("span", "muted", " · from "), repositoryLink(running.source.repository));
      } else {
        line.append(el("span", "muted", "The host did not report which package it is running."));
      }
      const listedAs = (digest: string | null): string => {
        if (!digest) return "—";
        if (listing.state !== "ready") return "";
        const names = activeRepositories().flatMap((repository) => repository.candidates
          .filter((choice) => choice.kind === "native_set" && choice.manifest_sha256 === digest)
          .map(() => repository.id));
        return [...new Set(names)].join(", ") || "Not in a current catalog";
      };
      const phase = (name: string, digest: string | null): Part[] => [
        name,
        digest ? copyable(digest, shortDigest(digest), "package digest") : el("span", "muted", "None"),
        lit(listedAs(digest)),
      ];
      const details = el("details", "plugin-package-details",
        el("summary", "", "Package history"),
        table(["Recorded as", "Package", "Listed by"], [
          phase("Running now", running.digest),
          phase("Active", status.active),
          phase("Previous", status.previous),
          phase("Pending", status.pending),
          phase("On trial", status.trial),
        ]),
        el("p", "muted", lit(`Only the running package is loaded. The others are what the host has recorded for its next start or a rollback. Highest release sequence accepted: ${status.highestSequence}.`)),
      );
      return el("div", "plugin-package", line, details);
    }

    function installedDetail(
      plugin: InstalledPlugin,
      status: PackageSelectionStatus,
      installed: ReadonlyMap<string, InstalledPlugin>,
      focus: (id: string) => void,
    ): HTMLElement {
      const facts = el("div");
      const relations = el("div");
      const source = status.running.source;
      const moduleLink = (id: string, version?: string): Part => {
        const known = installed.get(id);
        const label = known ? known.name : id;
        const link = known
          ? button(lit(label), () => focus(id), "link-button plugin-link")
          : el("span", "", lit(label));
        return version ? el("span", "", link, el("span", "muted", lit(` ${version}`))) : link;
      };
      const service = (name: string): Part => el("code", "plugin-service", lit(name));

      function show(description: ModuleDescription | null): void {
        facts.replaceChildren(factList([
          ["Version", lit(plugin.version)],
          ["Category", plugin.category],
          ["Type", plugin.runtime ? "Runtime" : "Module"],
          ["Supplied by", source ? repositoryLink(source.repository) : el("span", "muted", "Not recorded")],
          description ? ["Size", lit(formatBytes(description.size))] : null,
          ["Library SHA-256", copyable(plugin.sha256, shortDigest(plugin.sha256), "library digest")],
        ]));
        if (!description) return;
        const provides = [...new Set([...description.provides, ...description.nativeProvides])];
        const uses = [...new Set([...description.calls, ...description.nativeUses])];
        const events = [...new Set([...description.publishes, ...description.subscribes, ...description.hooks])];
        const sections: HTMLElement[] = [];
        const byShownName = (a: string, b: string) =>
          (installed.get(a)?.name ?? a).localeCompare(installed.get(b)?.name ?? b);
        const versions = new Map(description.requires.map((dependency) => [dependency.id, dependency.version]));
        if (versions.size)
          sections.push(detailSection("Depends on",
            nameList([...versions.keys()].sort(byShownName), (id) => moduleLink(id, versions.get(id)))));
        if (description.requiredBy.length)
          sections.push(detailSection("Required by",
            nameList([...description.requiredBy].sort(byShownName), (id) => moduleLink(id))));
        if (provides.length) sections.push(detailSection("Provides", nameList(provides, service)));
        if (uses.length) sections.push(detailSection("Uses", nameList(uses, service)));
        if (events.length) sections.push(detailSection("Events and hooks", nameList(events, service)));
        if (sections.length)
          sections.push(el("p", "muted plugin-footnote",
            "Declared by the signed package. Host policy decides what each module is granted."));
        relations.replaceChildren(...sections);
      }

      show(descriptions.get(plugin.id) ?? null);
      if (describeSupported && !descriptions.has(plugin.id)) {
        relations.replaceChildren(el("p", "muted plugin-footnote", "Loading details…"));
        void api.post<unknown>("modules/describe", { id: plugin.id }).then(parseModuleDescription).then((description) => {
          descriptions.set(plugin.id, description);
          if (relations.isConnected) show(description);
        }).catch((error: unknown) => {
          // A host without this route still lists its modules; it only has
          // less to say about each one.
          if (error instanceof ApiError && (error.status === 404 || error.code === "not_found")) describeSupported = false;
          if (relations.isConnected) relations.replaceChildren();
        });
      }
      const detail = el("section", "plugin-detail",
        el("header", "plugin-detail-header",
          el("h2", "", lit(plugin.name)),
          el("p", "plugin-identity", lit(plugin.id)),
          plugin.summary ? el("p", "plugin-detail-summary", plugin.summary) : null),
        facts,
        relations,
      );
      detail.setAttribute("aria-label", `${plugin.name} details`);
      return detail;
    }

    // --- Available -------------------------------------------------------------

    function renderAvailable(): void {
      if (listing.state === "loading") {
        viewArea.replaceChildren(el("p", "muted plugin-loading", "Loading repository catalogs…"));
        return;
      }
      if (listing.state === "failed") {
        viewArea.replaceChildren(errorPanel(listing.error, load));
        return;
      }
      const catalogs = listing.value;
      const plugins = offers();
      if (!plugins.length) {
        const empty = activeRepositories().length
          ? quietState("Nothing is offered for this host",
              "None of this host's repositories lists a plugin for its platform right now.",
              button("View repositories", () => showView("repositories")))
          : quietState("No repositories yet",
              "Add a publisher's repository to browse the plugins it offers.",
              catalogs.manageable && canManage
                ? button("Add repository", () => openAddRepository(), "primary")
                : button("View repositories", () => showView("repositories")));
        viewArea.replaceChildren(empty);
        return;
      }
      const byKey = new Map(plugins.map((plugin) => [plugin.key, plugin]));
      const count = el("span", "muted plugin-count");
      const severalSources = new Set(plugins.map((plugin) => plugin.repository.id)).size > 1;
      const browser = pluginBrowser({
        label: "Available plugins",
        grouped: severalSources,
        selected: () => selectedAvailable,
        onSelect: (key) => { selectedAvailable = key; },
        detail: (key) => {
          const plugin = byKey.get(key);
          return plugin ? availableDetail(plugin) : null;
        },
        signal: page.signal,
      });
      const categories = PLUGIN_CATEGORIES.filter((category) => plugins.some((plugin) => plugin.category === category));
      const showMatches = (): void => {
        const matches = plugins.filter((plugin) => matchesPluginFilter(plugin, availableFilter));
        count.textContent = matches.length === plugins.length
          ? `${plugins.length} available`
          : `${matches.length} of ${plugins.length}`;
        browser.show(
          [...matches].sort((a, b) => severalSources ? a.repository.id.localeCompare(b.repository.id) : 0).map((plugin) => ({
            key: plugin.key,
            name: plugin.name,
            summary: plugin.summary || lit(plugin.slot),
            meta: `${availabilityLabel(plugin)} · ${plugin.version}`,
            group: plugin.repository.id,
            emphasis: plugin.status !== "installed",
          })),
          quietState("No plugins match", "Try a different search or category."),
        );
      };
      viewArea.replaceChildren(
        toolbar(availableFilter, "Search available plugins", categories, showMatches, count),
        browser.node,
      );
      showMatches();
    }

    function availableDetail(plugin: AvailablePlugin): HTMLElement {
      const consoleInterface = plugin.kind === "web_bundle";
      const state = consoleInterface
        ? plugin.status === "installed"
          ? "In use. You are looking at this interface."
          : "Not in use."
        : plugin.status === "installed"
          ? plugin.runningThisPackage
            ? "Installed. This host is running the package this repository lists."
            : `Installed. Version ${plugin.version} is loaded from a different package than the one listed here.`
          : plugin.status === "update"
            ? `Version ${plugin.installedVersion} is installed. This repository offers ${plugin.version}.`
            : "Not installed.";
      const installing: Part[] = [];
      if (!(plugin.status === "installed" && plugin.runningThisPackage)) {
        if (plugin.slot === "amaterasu.rtx-hdr" && plugin.status === "available") {
          installing.push(
            el("p", "", "RTX HDR is installed from Integrations, which also downloads the NVIDIA runtime it needs."),
            button("Set up RTX HDR", () => context.navigate("integrations")),
          );
        } else if (consoleInterface) {
          installing.push(el("p", "", "This console cannot switch its own interface yet. An interface bundle is selected on the host with the release tool."));
        } else {
          installing.push(el("p", "", "This console cannot install a module set yet. A set replaces every module the host runs, and the host has to approve what its new modules may do before it loads them."));
        }
      }
      const detail = el("section", "plugin-detail",
        el("header", "plugin-detail-header",
          el("h2", "", lit(plugin.name)),
          el("p", "plugin-identity", lit(plugin.slot)),
          plugin.summary ? el("p", "plugin-detail-summary", plugin.summary) : null),
        el("p", "plugin-detail-state", lit(state)),
        factList([
          ["Version", lit(plugin.version)],
          plugin.otherVersions.length ? ["Also offered", lit(plugin.otherVersions.join(", "))] : null,
          ["Type", consoleInterface ? "Console interface" : "Module set"],
          ["Implementation", el("code", "plugin-service", lit(plugin.implementation))],
          ["Repository", repositoryLink(plugin.repository.id)],
          ["Publisher key", copyable(plugin.repository.fingerprint, shortDigest(plugin.repository.fingerprint), "publisher key fingerprint")],
          ["Signed manifest", copyable(plugin.manifest, shortDigest(plugin.manifest), "manifest digest")],
        ]),
        installing.length ? detailSection("Installing", ...installing) : null,
      );
      detail.setAttribute("aria-label", `${plugin.name} details`);
      return detail;
    }

    // --- Repositories ----------------------------------------------------------

    function renderRepositories(): void {
      if (listing.state === "loading") {
        viewArea.replaceChildren(el("p", "muted plugin-loading", "Loading repositories…"));
        return;
      }
      if (listing.state === "failed") {
        viewArea.replaceChildren(errorPanel(listing.error, load));
        return;
      }
      const enrolled = listing.value;
      const manage = enrolled.manageable && canManage;
      const active = activeRepositories();
      const removed = repositories().filter((repository) => repository.revoked);
      const full = enrolled.capacity !== null && repositories().length >= enrolled.capacity;
      const add = button("Add repository", () => openAddRepository(), "primary");
      add.disabled = full || changing !== null;
      const intro = el("div", "repository-intro",
        el("p", "", "A repository is a publisher's signed list of plugins. The host accepts a catalog only when it is signed by the publisher key you approved."),
        manage ? add : null,
      );
      const notes: HTMLElement[] = [];
      if (!manage)
        notes.push(el("p", "muted repository-note", enrolled.manageable
          ? "Only an administrator can add or remove repositories."
          : "This host version lists its repositories here but changes them with the release tool on the host."));
      if (manage && full)
        notes.push(el("p", "muted repository-note", "This host holds as many repositories as it can, counting removed ones."));
      const list = active.length
        ? el("div", "repository-list", ...active.map((repository) => repositoryCard(repository, manage)))
        : quietState("No repositories yet", "Add a publisher's repository to browse the plugins it offers.");
      const removedList = removed.length
        ? el("details", "repository-removed",
            el("summary", "", lit(`Removed repositories (${removed.length})`)),
            el("div", "repository-list", ...removed.map((repository) => repositoryCard(repository, false))))
        : null;
      viewArea.replaceChildren(intro, ...notes, list, removedList ?? "");
    }

    function repositoryCard(repository: ApprovedRepositoryView, manage: boolean): HTMLElement {
      const summary = summarizeRepository(repository, formatDate);
      const running = ready(selection)?.running.source?.repository === repository.id;
      const busy = changing?.repository === repository.id ? changing.kind : null;
      const failure = changeFailures.get(repository.id);
      const actions = el("div", "actions repository-actions");
      if (manage && !repository.revoked) {
        const check = button(busy === "refresh" ? "Checking…" : "Check for updates",
          () => changeRepository(repository.id, "refresh"));
        const remove = button(busy === "remove" ? "Removing…" : "Remove", () => removeRepository(repository));
        check.disabled = changing !== null;
        // The host refuses this too; the reason is the line under the status.
        remove.disabled = changing !== null || running;
        actions.append(check, remove);
      }
      const card = el("article", `repository-card${repository.revoked ? " repository-card--removed" : ""}`,
        el("header", "repository-card-header",
          el("div", "repository-card-title",
            el("h3", "", lit(repository.id)),
            el("p", "repository-location", lit(repository.location))),
          actions),
        el("p", `repository-status${summary.attention ? " repository-status--attention" : ""}`, lit(summary.status)),
        running ? el("p", "muted repository-status", manage
          ? "The package this host is running came from this repository, so it cannot be removed."
          : "The package this host is running came from this repository.") : null,
        factList([
          ["Publisher key", copyable(repository.fingerprint, groupedFingerprint(repository.fingerprint), "publisher key fingerprint")],
        ]),
      );
      if (failure) {
        const alert = el("p", "repository-failure", lit(failure));
        alert.setAttribute("role", "alert");
        card.append(alert);
      }
      return card;
    }

    /// Starts a repository change and waits for the host to finish it.
    async function runRepositoryChange(
      route: RepositoryChangeKind,
      body: Payload,
      repository: string,
    ): Promise<RepositoryOperation> {
      changing = { repository, kind: route };
      changeFailures.delete(repository);
      if (view === "repositories") render();
      try {
        const started = await api.post<{ operation?: unknown }>(`repositories/${route}`, body);
        const operation = started.operation;
        if (typeof operation !== "number") throw new Error("The host did not confirm the change.");
        const deadline = Date.now() + CHANGE_TIMEOUT_MS;
        while (true) {
          await new Promise((resolve) => setTimeout(resolve, CHANGE_POLL_MS));
          if (page.signal.aborted) throw new Error("The page was closed before the change finished.");
          const current = parseRepositoryPage(await api.post<unknown>("repositories/list", { offset: 0 })).operation;
          if (current && current.id === operation && current.state !== "running") return current;
          if (current && current.id > operation) throw new Error("Another repository change replaced this one. Refresh to see its result.");
          // A restarted host numbers its changes from one again and remembers none.
          if (!current || current.id < operation) throw new Error("The host restarted before this change finished. Refresh to see what was saved.");
          if (Date.now() > deadline) throw new Error("The host is still working on this change. Refresh in a moment to see its result.");
        }
      } finally {
        changing = null;
      }
    }

    async function changeRepository(repository: string, kind: "refresh" | "remove"): Promise<void> {
      try {
        const result = await runRepositoryChange(kind, { id: repository }, repository);
        if (result.state === "failed") changeFailures.set(repository, repositoryChangeMessage(result.code));
        else context.notify(lit(kind === "refresh"
          ? `Checked ${repository} for updates.`
          : `Removed ${repository}.`));
      } catch (error) {
        changeFailures.set(repository, error instanceof ApiError
          ? repositoryMessageFor(error)
          : messageOf(error));
      }
      await reloadRepositories();
    }

    function repositoryMessageFor(error: ApiError): string {
      return repositoryChangeMessage(error.code) === repositoryChangeMessage(null)
        ? error.message
        : repositoryChangeMessage(error.code);
    }

    async function removeRepository(repository: ApprovedRepositoryView): Promise<void> {
      const confirmed = await confirmAction(
        lit(`Remove ${repository.id}?`),
        "Its plugins will no longer be offered to this host. This repository ID can never be added again.",
        "Remove repository",
      );
      if (confirmed) await changeRepository(repository.id, "remove");
    }

    function openAddRepository(): void {
      const dialog = el("dialog", "plugin-form-dialog");
      setLocalizedAttribute(dialog, "aria-label", "Add a repository");
      const text = (name: string, placeholder: string): HTMLInputElement => {
        const control = input(name);
        control.required = true;
        control.autocomplete = "off";
        control.spellcheck = false;
        control.placeholder = placeholder;
        control.setAttribute("autocapitalize", "none");
        return control;
      };
      const location = text("repository_location", "https://");
      location.type = "url";
      const id = text("repository_id", "publisher-plugins");
      const publicKey = text("repository_public_key", "");
      const fingerprint = text("repository_fingerprint", "");
      publicKey.className = fingerprint.className = "plugin-key-input";
      const trust = input("repository_trust", "", "checkbox");
      trust.required = true;
      const controls = { location, id, publicKey, fingerprint } as const;
      const status = el("div", "plugin-form-status");
      status.setAttribute("aria-live", "polite");
      const submit = el("button", "button primary", "Add repository");
      submit.type = "submit";
      let working = false;
      const close = (): void => { dialog.close(); dialog.remove(); };
      const cancel = button("Cancel", () => { if (!working) close(); });

      // Publishers hand these details out as one block; pasting it anywhere
      // in the form fills the fields it names. The fingerprint is left for
      // the administrator to confirm.
      const form = el("form", "plugin-form",
        el("h2", "", "Add a repository"),
        el("p", "muted", "Enter the details its publisher gives you. Pasting the publisher's repository details into any field fills in the rest."),
        field("Address", location, "Where the publisher hosts its catalog."),
        field("Repository ID", id, "Exactly as the publisher writes it."),
        field("Publisher key", publicKey, "The publisher's public signing key: 64 hexadecimal characters."),
        field("Key fingerprint", fingerprint, "The key's SHA-256 fingerprint. Get it from the publisher somewhere other than the address above, so you know the key is really theirs."),
        el("div", "notice plugin-trust",
          el("strong", "", "Plugins are not sandboxed"),
          el("p", "", "A plugin runs inside this host with your user account's access. A publisher you add can offer any module, including the one that serves this console. Add only publishers you trust.")),
        el("label", "plugin-trust-check", trust, el("span", "", "I trust this publisher")),
        status,
        el("div", "actions", cancel, submit),
      );
      form.addEventListener("paste", (event) => {
        const descriptor = parseRepositoryDescriptor(event.clipboardData?.getData("text") ?? "");
        if (!descriptor) return;
        event.preventDefault();
        if (descriptor.location) location.value = descriptor.location;
        if (descriptor.id) id.value = descriptor.id;
        if (descriptor.publicKey) publicKey.value = descriptor.publicKey;
        (descriptor.location ? descriptor.id ? descriptor.publicKey ? fingerprint : publicKey : id : location).focus();
      });
      const fail = (message: string, focus: keyof typeof controls | null): void => {
        const alert = el("p", "repository-failure", lit(message));
        alert.setAttribute("role", "alert");
        status.replaceChildren(alert);
        for (const [name, control] of Object.entries(controls)) {
          if (name === focus) control.setAttribute("aria-invalid", "true");
          else control.removeAttribute("aria-invalid");
        }
        if (focus) controls[focus].focus();
      };
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (working || !form.reportValidity()) return;
        const hex = (control: HTMLInputElement) => control.value.replace(/[\s:]/g, "").toLowerCase();
        const request = {
          id: id.value.trim(),
          location: location.value.trim(),
          public_key: hex(publicKey),
          fingerprint: hex(fingerprint),
        };
        // What the host would refuse outright is caught here, in its words.
        const refused =
          !/^https:\/\/\S+$/.test(request.location) || request.location.length > 256 ? "invalid_repository_location"
          : !/^[a-z0-9._-]{1,128}$/.test(request.id) ? "invalid_repository_id"
          : !/^[a-f0-9]{64}$/.test(request.public_key) ? "invalid_publisher_key"
          : null;
        if (refused) {
          fail(repositoryChangeMessage(refused), repositoryChangeField(refused));
          return;
        }
        if (!/^[a-f0-9]{64}$/.test(request.fingerprint)) {
          fail("The key fingerprint is 64 hexadecimal characters.", "fingerprint");
          return;
        }
        working = true;
        submit.disabled = cancel.disabled = true;
        status.replaceChildren(el("p", "muted", "Checking the repository's catalog against this key…"));
        void runRepositoryChange("add", request, request.id).then(async (result) => {
          if (result.state === "failed") {
            fail(repositoryChangeMessage(result.code), repositoryChangeField(result.code));
            return;
          }
          close();
          await reloadRepositories();
          const added = repositories().find((repository) => repository.id === request.id);
          const count = added?.candidates.length ?? 0;
          context.notify(lit(`Added ${request.id}. ${count === 1 ? "1 plugin is" : `${count} plugins are`} available.`));
          showView("repositories");
        }).catch((error: unknown) => {
          const code = error instanceof ApiError ? error.code : null;
          fail(error instanceof ApiError ? repositoryMessageFor(error) : messageOf(error), repositoryChangeField(code));
        }).finally(() => {
          working = false;
          submit.disabled = cancel.disabled = false;
          if (viewArea.isConnected && view === "repositories") render();
        });
      });
      dialog.addEventListener("cancel", (event) => {
        event.preventDefault();
        if (!working) close();
      });
      dialog.append(form);
      document.body.append(dialog);
      dialog.showModal();
      location.focus();
    }

    void load();
  }
}
