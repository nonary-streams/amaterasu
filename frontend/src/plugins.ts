namespace Amaterasu {
  // The Modules page: one line that says whether the host is up to date, then
  // Amaterasu Core (every module Amaterasu ships, kept as one package) and the
  // plugins other developers offer; every module the host loaded; and the
  // repositories all of it comes from. The page keeps the `plugins` address
  // and class names it had when it was called Plugins.

  type Loadable<T> =
    | { state: "loading" }
    | { state: "ready"; value: T }
    | { state: "failed"; error: unknown };

  type Part = Node | TextValue | null | undefined;

  const VIEW_LABELS: Readonly<Record<PluginsView, string>> = {
    overview: "Overview",
    modules: "All modules",
    repositories: "Repositories",
  };
  const CHANGE_POLL_MS = 600;
  const CHANGE_TIMEOUT_MS = 150_000;
  const COLLAPSED_NAMES = 8;
  const COLLAPSED_CHANGES = 6;
  const COLLAPSED_VERSIONS = 6;
  /// How often, and how many times, a host still reading its packages is asked again.
  const VERSIONS_RETRY_MS = 700;
  const VERSIONS_RETRIES = 8;
  const UPDATE_POLL_MS = 1500;
  /// How long a host that is installing may go without answering before the
  /// page stops waiting for it.
  const RESTART_PATIENCE_MS = 180_000;
  const CHECKED_REFRESH_MS = 30_000;
  /// Above this many, the plugins other developers offer get a search box.
  const SEARCHABLE_PLUGINS = 6;

  function lit(value: string): TextValue {
    return typeof Amaterasu.literalText === "function" ? Amaterasu.literalText(value) : value;
  }

  function intlLocale(): string | undefined {
    return typeof Amaterasu.activeIntlLocale === "function" ? Amaterasu.activeIntlLocale() : undefined;
  }

  function formatDate(unixSeconds: number): string {
    try {
      return new Intl.DateTimeFormat(intlLocale(), { dateStyle: "medium" }).format(new Date(unixSeconds * 1000));
    } catch {
      return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
    }
  }

  function formatDateTime(unixMs: number): string {
    try {
      return new Intl.DateTimeFormat(intlLocale(), { dateStyle: "medium", timeStyle: "short" }).format(new Date(unixMs));
    } catch {
      return new Date(unixMs).toISOString();
    }
  }

  function formatUntil(until: { value: number; unit: "minute" | "hour" | "day" }): string {
    try {
      return new Intl.RelativeTimeFormat(intlLocale(), { numeric: "always" }).format(until.value, until.unit);
    } catch {
      return `in ${until.value} ${until.unit}${until.value === 1 ? "" : "s"}`;
    }
  }

  function formatElapsed(elapsed: { value: number; unit: "minute" | "hour" | "day" }): string {
    try {
      return new Intl.RelativeTimeFormat(intlLocale(), { numeric: "always" }).format(-elapsed.value, elapsed.unit);
    } catch {
      return `${elapsed.value} ${elapsed.unit}${elapsed.value === 1 ? "" : "s"} ago`;
    }
  }

  const shownText = new WeakMap<Element, string>();

  /// Replaces an element's text only when it differs. A redraw that changed
  /// nothing then leaves a live region silent and a focused control alone.
  function setText(node: HTMLElement, value: TextValue | null): void {
    const key = value === null ? "" : typeof value === "string" ? `message:${value}` : `${value.kind}:${value.value}`;
    if (shownText.get(node) === key) return;
    shownText.set(node, key);
    node.replaceChildren(...(value === null ? [] : el("span", "", value).childNodes));
  }

  function sourceLink(source: SourceRepository): HTMLAnchorElement {
    const link = el("a", "plugin-source-link", lit(source.name));
    link.href = source.url;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    setLocalizedAttribute(link, "title", "Opens on GitHub in a new tab");
    return link;
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

  /// A list that stays short until asked: service or module names, or the
  /// modules an update changes. One or two more than `collapsed` are shown
  /// outright rather than hidden behind a control.
  function nameList<T>(
    items: readonly T[],
    render: (item: T) => Part,
    collapsed = COLLAPSED_NAMES,
    className = "plugin-names",
  ): HTMLElement {
    const list = el("ul", className);
    const show = (count: number) => list.replaceChildren(...items.slice(0, count).map((item) => el("li", "", render(item))));
    if (items.length <= collapsed + 2) {
      show(items.length);
      return list;
    }
    show(collapsed);
    const more = button(lit(`Show all ${items.length}`), () => {
      show(items.length);
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
    /// Moves the keyboard to a row, for a reader sent here from another view.
    focus(key: string): void;
  }

  /// A list of plugins with the selected one's details beside it. On a narrow
  /// screen the details open under the selected row instead.
  function pluginBrowser(options: {
    label: string;
    grouped: boolean;
    /// Details always open under the selected row, and nothing is selected until asked.
    inline?: boolean;
    selected: () => string | null;
    onSelect: (key: string | null) => void;
    detail: (key: string) => HTMLElement | null;
    signal: AbortSignal;
  }): PluginBrowser {
    const list = el("div", "plugin-list");
    setLocalizedAttribute(list, "aria-label", options.label);
    list.setAttribute("role", "group");
    const pane = el("aside", "plugin-detail-pane");
    const node = el("div", `plugin-browser${options.inline ? " plugin-browser--inline" : ""}`, list, pane);
    const wideLayout = typeof window.matchMedia === "function" ? window.matchMedia("(min-width: 1101px)") : null;
    const isWide = () => !options.inline && (wideLayout?.matches ?? true);
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
      focus(key) {
        const control = rows.get(key)?.control;
        control?.focus({ preventScroll: true });
        // On a narrow screen the details open under the row, so it goes to the top.
        control?.scrollIntoView({ block: isWide() ? "center" : "start" });
      },
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
            // Inside a section of the overview, a group sits one level under its heading.
            if (options.grouped) sections.push(el(options.inline ? "h3" : "h2", "plugin-label plugin-group-heading", entry.group));
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

  // --- Updates ---------------------------------------------------------------

  /// Something the page keeps up to date: how its state is read, how a
  /// request is sent, and the row that state makes. Amaterasu Core is the only
  /// one a host has routes for today; a plugin from another developer would be
  /// one more feed, with nothing else on the page changing.
  interface UpdateFeed {
    id: string;
    /// Reads the source's state from the host. A failure becomes the row's
    /// state rather than a rejection.
    read(): Promise<void>;
    /// Sends one request. Rejects with the host's refusal in plain words.
    request(action: UpdateAction): Promise<void>;
    /// Changes how the source is kept, or which of its packages runs.
    change(change: UpdateChange): Promise<void>;
    /// The row for what was last read.
    view(): UpdateSourceView;
    /// More to show under the row: what the source includes, and its earlier packages.
    extras?(): readonly HTMLElement[];
  }

  type RowAction = Exclude<UpdateAction, "check">;

  interface UpdateRow {
    node: HTMLElement;
    show(view: UpdateSourceView, working: boolean, refusal: string | null, extras: readonly HTMLElement[]): void;
    /// Brings "Next check" up to the present.
    tick(): void;
    /// Puts the keyboard on the row's name, when the control it was on has gone.
    focus(): void;
  }

  const AUTOMATIC_LABELS: Readonly<Record<AutomaticMode, string>> = {
    off: "Off",
    download: "Download, then ask",
    install: "Install when idle",
  };

  const AUTOMATIC_HINTS: Readonly<Record<AutomaticMode, string>> = {
    off: "Updates are downloaded and installed only when you ask.",
    download: "The host looks for updates every six hours and downloads what it finds. You choose when to install.",
    install: "The host looks for updates every six hours and installs them once no stream is running. It restarts to finish.",
  };

  let rowCount = 0;

  /// One source: its name and what it is, its state with the request that
  /// state calls for, what it runs, and whatever an update needs shown. The
  /// row is built once and then changed in place, so reading the host again
  /// does not move the keyboard.
  function updateSourceRow(options: {
    copyable: (value: string, shown: string, what: string) => HTMLElement;
    repositoryLink: (id: string) => HTMLElement;
    /// Whether this account may send requests at all, and whether it may install.
    canChange: boolean;
    canInstall: boolean;
    onAction: (action: RowAction) => void;
    onRetry: () => void;
    onAutomatic: (mode: AutomaticMode) => void;
  }): UpdateRow {
    const name = el("h2", "plugin-update-name");
    name.tabIndex = -1;
    const about = el("p", "muted plugin-update-about");
    const mark = el("span", "plugin-update-mark");
    mark.setAttribute("aria-hidden", "true");
    const headline = el("span", "plugin-update-headline");
    const state = el("p", "plugin-update-state", mark, headline);
    state.setAttribute("aria-live", "polite");
    const controls: Readonly<Record<RowAction, HTMLButtonElement>> = {
      cancel: button("Cancel", () => options.onAction("cancel")),
      download: button("Download update", () => options.onAction("download"), "primary"),
      activate: button("Install when idle", () => options.onAction("activate"), "primary"),
    };
    const retry = button("Try again", options.onRetry);
    const actions = el("div", "actions plugin-update-actions", controls.cancel, retry, controls.download, controls.activate);
    const facts = el("p", "plugin-update-facts");
    const detail = el("p", "plugin-update-detail");
    detail.setAttribute("aria-live", "polite");
    const refused = el("p", "plugin-update-refusal");
    refused.setAttribute("role", "alert");
    const permission = el("p", "muted plugin-update-permission");
    const steps = el("ol", "plugin-update-steps");
    const bar = el("progress", "plugin-update-bar");
    setLocalizedAttribute(bar, "aria-label", "Download progress");
    const received = el("span", "plugin-update-received");
    const transfer = el("div", "plugin-update-transfer", bar, received);
    const summary = el("p", "plugin-update-summary");
    const changeList = el("div", "plugin-update-change-list");
    changeList.setAttribute("role", "group");
    setLocalizedAttribute(changeList, "aria-label", "What this update changes");
    const changes = el("div", "plugin-update-changes", summary, changeList);

    const mode = el("select", "plugin-automatic-mode");
    mode.name = "plugin_automatic";
    mode.id = `plugin-automatic-${++rowCount}`;
    mode.setAttribute("data-draft-ignore", "");
    for (const choice of AUTOMATIC_MODES) {
      const option = el("option", "", AUTOMATIC_LABELS[choice]);
      option.value = choice;
      mode.append(option);
    }
    mode.addEventListener("change", () => options.onAutomatic(mode.value as AutomaticMode));
    const modeLabel = el("label", "plugin-setting-label", "Automatic updates");
    modeLabel.htmlFor = mode.id;
    const modeHint = el("span", "plugin-setting-hint-text");
    const nextCheck = el("span", "plugin-setting-next");
    const modeNote = el("p", "muted plugin-setting-hint", modeHint, nextCheck);
    mode.setAttribute("aria-describedby", modeNote.id = `${mode.id}-hint`);
    const automatic = el("div", "plugin-setting", modeLabel, mode, modeNote);

    const extras = el("div", "plugin-update-extra");
    const node = el("article", "plugin-update-source",
      el("header", "plugin-update-source-head",
        el("div", "plugin-update-title", name, about),
        el("div", "plugin-update-status", state, actions)),
      facts, detail, refused, permission, steps, transfer, changes, automatic, extras);
    let shownFacts = "";
    let shownSteps = "";
    let shownChanges = "";
    let shownExtras: readonly HTMLElement[] = [];
    let nextCheckAt: number | null = null;
    const permits = (action: RowAction): boolean => options.canChange && (action !== "activate" || options.canInstall);

    function tick(): void {
      const until = nextCheckAt === null ? null : timeUntil(Date.now(), nextCheckAt);
      setText(nextCheck, until ? lit(` Next check ${formatUntil(until)}.`) : null);
    }

    function showFacts(view: UpdateSourceView): void {
      const key = `${view.running}\n${view.repository}\n${view.version}`;
      if (key === shownFacts) return;
      shownFacts = key;
      const fact = (...parts: Part[]): HTMLElement => el("span", "plugin-update-fact", ...parts);
      const shown: HTMLElement[] = [];
      if (view.version) shown.push(fact(el("span", "muted", "Version"), " ", lit(view.version)));
      if (view.running)
        shown.push(fact(el("span", "muted", "Package"), " ",
          options.copyable(view.running, shortDigest(view.running), "package digest")));
      if (view.repository) shown.push(fact(el("span", "muted", "from"), " ", options.repositoryLink(view.repository)));
      facts.replaceChildren(...shown);
      facts.hidden = shown.length === 0;
    }

    function showProgress(view: UpdateSourceView): void {
      const key = view.steps.map((step) => `${step.label} ${step.state}`).join();
      if (key !== shownSteps) {
        shownSteps = key;
        steps.replaceChildren(...view.steps.map((step) => {
          const item = el("li", `plugin-update-step plugin-update-step--${step.state}`, step.label);
          if (step.state === "current") item.setAttribute("aria-current", "step");
          return item;
        }));
      }
      steps.hidden = view.steps.length === 0;
      transfer.hidden = view.transfer === null;
      if (!view.transfer) return;
      if (view.transfer.total === null) {
        bar.removeAttribute("value");
        setText(received, lit(`${formatBytes(view.transfer.received)} received`));
      } else {
        bar.max = view.transfer.total;
        bar.value = view.transfer.received;
        setText(received, lit(`${formatBytes(view.transfer.received)} of ${formatBytes(view.transfer.total)}`));
      }
    }

    function showChanges(view: UpdateSourceView): void {
      changes.hidden = view.summary === null;
      setText(summary, view.summary === null ? null : lit(view.summary));
      // The same update keeps its list, and with it a reader's "Show all".
      const key = `${view.update}\n${view.changes.map((change) => `${change.id} ${change.detail}`).join("\n")}`;
      if (key === shownChanges) return;
      shownChanges = key;
      changeList.hidden = view.changes.length === 0;
      changeList.replaceChildren(nameList(view.changes, (change) => el("span", `plugin-update-change plugin-update-change--${change.change}`,
        el("span", "plugin-update-change-name", lit(change.name)),
        el("span", "plugin-update-change-detail", lit(change.detail))), COLLAPSED_CHANGES, "plugin-update-list"));
    }

    function showAutomatic(view: UpdateSourceView, working: boolean): void {
      automatic.hidden = view.automatic === null;
      nextCheckAt = view.automatic && view.automatic.mode !== "off" ? view.automatic.nextCheckUnixMs : null;
      if (!view.automatic) return;
      // The choice just made stays shown while the host is asked; its answer replaces it.
      if (!working) mode.value = view.automatic.mode;
      mode.disabled = working || !options.canInstall;
      setText(modeHint, options.canInstall
        ? AUTOMATIC_HINTS[view.automatic.mode]
        : "Only an administrator who is signed in to the console can change this.");
      tick();
    }

    function showExtras(next: readonly HTMLElement[]): void {
      if (next.length === shownExtras.length && next.every((extra, index) => extra === shownExtras[index])) return;
      // A list the reader had open stays open when it is drawn again.
      next.forEach((extra, index) => {
        const previous = shownExtras[index];
        if (extra !== previous && extra instanceof HTMLDetailsElement && previous instanceof HTMLDetailsElement) extra.open = previous.open;
      });
      next.forEach((extra, index) => {
        if (extras.children[index] !== extra) extras.insertBefore(extra, extras.children[index] ?? null);
      });
      while (extras.children.length > next.length) extras.lastElementChild?.remove();
      shownExtras = [...next];
    }

    return {
      node,
      tick,
      focus: () => name.focus(),
      show(view, working, refusal, next) {
        const focused = document.activeElement;
        node.dataset.state = view.state;
        node.dataset.tone = view.tone;
        setText(name, lit(view.name));
        setText(about, view.about);
        setText(headline, view.headline);
        // A host that takes no update requests says so once, in the sentence below.
        state.hidden = view.state === "unavailable";
        setText(detail, view.detail);
        detail.hidden = view.detail === null;
        setText(refused, refusal === null ? null : lit(refusal));
        refused.hidden = refusal === null;
        showFacts(view);
        const offered = (Object.keys(controls) as RowAction[]).filter((action) => view.actions.includes(action));
        for (const [action, control] of Object.entries(controls) as [RowAction, HTMLButtonElement][]) {
          control.hidden = !offered.includes(action);
          control.disabled = working || !permits(action);
        }
        retry.hidden = view.state !== "unreadable";
        retry.disabled = working;
        actions.hidden = offered.length === 0 && retry.hidden;
        // An account that can change nothing is told so once, by the summary.
        permission.hidden = !(offered.includes("activate") && options.canChange && !options.canInstall);
        setText(permission, permission.hidden ? null : "Installing an update needs an administrator who is signed in to the console.");
        showProgress(view);
        showChanges(view);
        showAutomatic(view, working);
        showExtras(next);
        // A control the host's progress took away hands the keyboard to the row.
        if (!working && focused instanceof HTMLButtonElement && actions.contains(focused) && (focused.hidden || focused.disabled))
          name.focus();
      },
    };
  }

  interface UpdatesPanel {
    /// The page's one-line answer, with the request that checks every source.
    summary: HTMLElement;
    /// One row per source.
    node: HTMLElement;
    /// Reads every source without drawing, so the page can show all it loaded at once.
    read(): Promise<void>;
    /// Draws what was read and follows any source the host is working on.
    show(): void;
    /// Asks a source to change how it is kept or which of its packages runs.
    /// Resolves once the host has answered; a refusal is shown on the row.
    change(id: string, change: UpdateChange): Promise<void>;
    /// True while a request to a source is in flight.
    working(id: string): boolean;
  }

  /// The sources the page keeps up to date, and what they add up to.
  function updatesPanel(options: {
    feeds: readonly UpdateFeed[];
    signal: AbortSignal;
    canChange: boolean;
    canInstall: boolean;
    copyable: (value: string, shown: string, what: string) => HTMLElement;
    repositoryLink: (id: string) => HTMLElement;
    /// Called after a source changed on its own: a request finished, or the host moved on.
    onChange: () => void;
  }): UpdatesPanel {
    interface Entry {
      feed: UpdateFeed;
      row: UpdateRow;
      /// The request in flight, "read" while the row is only being read again,
      /// or "change" while a setting or a version switch is being sent.
      acting: UpdateAction | "read" | "change" | null;
      refusal: string | null;
      timer: number | undefined;
    }
    const entries: Entry[] = options.feeds.map((feed) => {
      const entry: Entry = {
        feed,
        acting: null,
        refusal: null,
        timer: undefined,
        row: updateSourceRow({
          copyable: options.copyable,
          repositoryLink: options.repositoryLink,
          canChange: options.canChange,
          canInstall: options.canInstall,
          onAction: (action) => void perform(entry, action),
          onRetry: () => void perform(entry, null),
          onAutomatic: (mode) => void perform(entry, { kind: "automatic", mode }),
        }),
      };
      return entry;
    });
    // While a check runs the control stays in place and focusable, saying so,
    // rather than being disabled out from under the keyboard.
    const check = button("Check for updates", () => {
      if (check.getAttribute("aria-disabled") === "true") return;
      for (const entry of entries) if (entry.feed.view().actions.includes("check")) void perform(entry, "check");
    });
    const mark = el("span", "plugin-summary-mark");
    mark.setAttribute("aria-hidden", "true");
    const headline = el("p", "plugin-summary-headline");
    headline.setAttribute("role", "status");
    const checked = el("time");
    const checkedLine = el("p", "muted plugin-summary-detail", "Last checked", " ", checked);
    const summary = el("section", "plugin-summary",
      mark,
      el("div", "plugin-summary-text", headline, checkedLine,
        options.canChange ? null : el("p", "muted plugin-summary-note", "This account can see updates but cannot change them.")),
      el("div", "actions", check));
    setLocalizedAttribute(summary, "aria-label", "Updates");
    const node = el("div", "plugin-update-sources", ...entries.map((entry) => entry.row.node));
    let checkedAt: number | null = null;

    function tick(): void {
      for (const entry of entries) entry.row.tick();
      if (checkedAt === null) return;
      const elapsed = elapsedSince(Date.now(), checkedAt);
      setText(checked, elapsed ? lit(formatElapsed(elapsed)) : "just now");
      checked.title = formatDateTime(checkedAt);
    }

    function draw(): void {
      let checking = false;
      let checkable = false;
      const views = entries.map((entry) => {
        const view = entry.feed.view();
        entry.row.show(view, entry.acting !== null, entry.refusal, entry.feed.extras?.() ?? []);
        checking ||= entry.acting === "check" || view.state === "checking";
        checkable ||= view.actions.includes("check");
        return view;
      });
      const answer = updatesSummary(views);
      summary.dataset.tone = answer.tone;
      setText(headline, answer.headline);
      checkedAt = answer.checkedUnixMs;
      checkedLine.hidden = checkedAt === null;
      if (checkedAt !== null) checked.dateTime = new Date(checkedAt).toISOString();
      tick();
      setText(check, checking ? "Checking…" : "Check for updates");
      check.setAttribute("aria-disabled", String(checking));
      check.disabled = !checking &&
        (!checkable || !options.canChange || entries.some((entry) => entry.acting !== null));
    }

    function follow(entry: Entry): void {
      window.clearTimeout(entry.timer);
      if (options.signal.aborted || entry.acting !== null || !entry.feed.view().busy) return;
      entry.timer = window.setTimeout(() => {
        void entry.feed.read().then(() => {
          if (options.signal.aborted || entry.acting !== null) return;
          draw();
          options.onChange();
          follow(entry);
        });
      }, UPDATE_POLL_MS);
    }

    /// Sends a request or a change for one source, or reads it again when there is none to send.
    async function perform(entry: Entry, work: UpdateAction | UpdateChange | null): Promise<void> {
      if (entry.acting !== null || options.signal.aborted) return;
      const origin = document.activeElement instanceof HTMLElement && document.activeElement.isConnected &&
        (document.activeElement instanceof HTMLButtonElement || document.activeElement instanceof HTMLSelectElement)
        ? document.activeElement : null;
      if (work === "activate") {
        const confirmed = await confirmAction(
          lit(`Install the ${entry.feed.view().name} update?`),
          "The host restarts with the new package as soon as no stream is running. This console is unavailable for a moment while it does.",
          "Install when idle",
        );
        if (!confirmed || options.signal.aborted || entry.acting !== null) return;
      }
      window.clearTimeout(entry.timer);
      entry.acting = work === null ? "read" : typeof work === "string" ? work : "change";
      entry.refusal = null;
      draw();
      try {
        if (work === null) await entry.feed.read();
        else if (typeof work === "string") await entry.feed.request(work);
        else await entry.feed.change(work);
      } catch (error) {
        entry.refusal = messageOf(error);
        // A refusal can mean the host has moved on; show where it is now.
        await entry.feed.read();
      } finally {
        entry.acting = null;
      }
      if (options.signal.aborted) return;
      draw();
      // The keyboard stays where the reader left it, unless that control is gone.
      if (origin && (summary.contains(origin) || node.contains(origin)) &&
          (document.activeElement === origin || document.activeElement === document.body)) {
        if (origin.isConnected && !origin.hidden && !origin.disabled) origin.focus();
        else entry.row.focus();
      }
      options.onChange();
      follow(entry);
    }

    const clock = window.setInterval(tick, CHECKED_REFRESH_MS);
    options.signal.addEventListener("abort", () => {
      window.clearInterval(clock);
      for (const entry of entries) window.clearTimeout(entry.timer);
    }, { once: true });

    const find = (id: string): Entry | undefined => entries.find((entry) => entry.feed.id === id);
    return {
      summary,
      node,
      read: async () => { await Promise.all(entries.map((entry) => entry.feed.read())); },
      show() {
        draw();
        for (const entry of entries) follow(entry);
      },
      async change(id, change) {
        const entry = find(id);
        if (entry) await perform(entry, change);
      },
      working: (id) => (find(id)?.acting ?? null) !== null,
    };
  }

  /// Amaterasu Core's feed: the host's own package, read and changed through
  /// the `package-update` routes. `running` is what the package selection says
  /// the host runs, for a host whose update status says nothing.
  function coreUpdateFeed(
    api: Pick<Api, "post">,
    running: () => { digest: string | null; repository: string | null; version: string | null } | null,
    extras: () => readonly HTMLElement[],
  ): UpdateFeed {
    let reading: UpdateReading = { state: "loading" };
    // Replies can arrive out of order; only the newest request's is kept.
    let latest = 0;
    let silentSince: number | null = null;
    const missing = (error: unknown): boolean =>
      error instanceof ApiError && (error.status === 404 || error.code === "not_found");
    const installing = (): boolean =>
      reading.state === "ready" && reading.status.supported && reading.status.phase === "activating";

    /// Sends a request whose answer is the source's new state.
    async function send(route: string, body: Payload): Promise<void> {
      const ticket = ++latest;
      try {
        const status = parsePackageUpdateStatus(await api.post<unknown>(`package-update/${route}`, body));
        if (ticket !== latest) return;
        silentSince = null;
        reading = { state: "ready", status };
      } catch (error) {
        if (!missing(error))
          throw new Error(error instanceof ApiError
            ? packageUpdateRefusalMessage(error.code) ?? error.message
            : messageOf(error));
        if (ticket === latest) reading = { state: "absent" };
      }
    }

    return {
      id: CORE_UPDATE_SOURCE,
      view: () => coreUpdateSource(reading, running()),
      extras,
      async read() {
        const ticket = ++latest;
        try {
          const status = parsePackageUpdateStatus(await api.post<unknown>("package-update/status", {}));
          if (ticket !== latest) return;
          silentSince = null;
          reading = { state: "ready", status };
        } catch (error) {
          if (ticket !== latest) return;
          if (missing(error)) {
            reading = { state: "absent" };
          } else if (installing()) {
            // A host that is installing stops answering while it restarts.
            silentSince ??= Date.now();
            if (Date.now() - silentSince >= RESTART_PATIENCE_MS)
              reading = { state: "failed", message: "The host has not answered since it began installing. It may still be restarting; try again in a moment." };
          } else {
            reading = { state: "failed", message: messageOf(error) };
          }
        }
      },
      request: (action) => send(action, {}),
      change: (change) => change.kind === "automatic"
        ? send("automatic", { mode: change.mode })
        : send("restore", { digest: change.digest }),
    };
  }

  export function renderPlugins(context: PageContext): void {
    const page = new AbortController();
    context.registerDraftGuard?.(() => false, () => page.abort());
    const api = context.api;
    const canRead = api.allows("configuration.read");
    const canManage = api.role === "administrator" && api.allows("configuration.write");

    // An address saved under a view's former name still opens that view.
    let view: PluginsView = pluginsViewFromAddress(context.initialPluginsView ?? "") ?? DEFAULT_PLUGINS_VIEW;
    let selection: Loadable<PackageSelectionStatus> = { state: "loading" };
    let listing: Loadable<RepositoryPage> = { state: "loading" };
    let loadRequest = 0;
    let selectedInstalled: string | null = null;
    let selectedAvailable: string | null = null;
    /// Set when another view sends the reader to a module, so its row takes the keyboard.
    let revealInstalled = false;
    let record: HTMLElement | null = null;
    let lastUpdateState: UpdateSourceState = "loading";
    /// True while the host was last seen switching to an earlier package.
    let wasRestoring = false;
    let shownUpdate: string | null = null;
    let reloadedFor: string | null = null;
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
    setLocalizedAttribute(tabList, "aria-label", "Module views");
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

    const ready = <T>(value: Loadable<T>): T | null => value.state === "ready" ? value.value : null;
    const repositories = (): ApprovedRepositoryView[] => ready(listing)?.repositories ?? [];
    const activeRepositories = (): ApprovedRepositoryView[] => repositories().filter((repository) => !repository.revoked);
    const offers = (): AvailablePlugin[] =>
      otherPublisherOffers(availablePlugins(repositories(), ready(selection), consoleDigest), ready(selection));

    // What Amaterasu Core includes is drawn again for each update; the row,
    // and the lists of earlier packages under it, are built once.
    const included = el("section", "plugin-included");
    setLocalizedAttribute(included, "aria-label", "Included in Amaterasu Core");
    const versions = versionsList();
    const core = coreUpdateFeed(api, () => {
      const status = ready(selection);
      return status ? {
        digest: status.running.digest,
        repository: status.running.source?.repository ?? null,
        version: runningPackageVersion(repositories(), status.running.digest),
      } : null;
    }, () => [included, ...(core.view().versions ? [versions.node] : []), ...(record ? [record] : [])]);
    const updates = updatesPanel({
      feeds: [core],
      signal: page.signal,
      canChange: api.allows("configuration.write"),
      canInstall: canManage,
      copyable,
      repositoryLink,
      onChange: updatesChanged,
    });
    const others = el("section", "plugin-update-source plugin-others");
    const overview = el("div", "plugin-overview", updates.node, others);

    context.content.append(
      sectionHeading("Modules", "What this host runs and how it stays up to date."),
      ...(canRead ? [updates.summary] : []),
      tabList,
      viewArea,
    );

    function showView(next: PluginsView): void {
      if (next === view) return;
      view = next;
      context.replacePluginsView?.(next);
      render();
    }

    function render(): void {
      const counts: Record<PluginsView, number | null> = {
        overview: null,
        modules: ready(selection)?.running.modules?.length ?? null,
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
        viewArea.replaceChildren(quietState("Modules are unavailable",
          "This account needs permission to read the host's configuration."));
        return;
      }
      if (view === "overview") renderOverview();
      else if (view === "modules") renderInstalled();
      else renderRepositories();
    }

    /// A source moved on by itself: an update appeared or went, or the host
    /// came back running another package.
    function updatesChanged(): void {
      const row = core.view();
      const installed = lastUpdateState === "activating" && row.state !== "activating";
      const switched = wasRestoring;
      lastUpdateState = row.state;
      wasRestoring = row.restoring !== null;
      versions.sync();
      const known = ready(selection)?.running.digest ?? null;
      if (row.running && known && row.running !== known && row.running !== reloadedFor) {
        // Everything on the page describes the package that was replaced.
        reloadedFor = row.running;
        if (installed)
          context.notify(lit(`${switched ? "Version switched" : "Update installed"}. The host is running package ${shortDigest(row.running)}.`));
        versions.forget();
        void load();
        return;
      }
      if (row.update !== shownUpdate && view === "overview" && viewArea.isConnected) renderIncluded();
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
        updates.read(),
      ]);
      if (request !== loadRequest || !viewArea.isConnected) return;
      selection = nextSelection;
      listing = nextListing;
      showUpdates();
      render();
    }

    /// Draws the sources from everything the page has loaded.
    function showUpdates(): void {
      const status = ready(selection);
      record = status ? packageRecord(status) : null;
      lastUpdateState = core.view().state;
      wasRestoring = core.view().restoring !== null;
      updates.show();
      versions.sync();
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
      if (!viewArea.isConnected) return;
      // The package history names the repositories that list each package.
      showUpdates();
      render();
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

    // --- Overview ---------------------------------------------------------------

    /// The modules the host loaded, or the reason there are none to show.
    function runningModules(): InstalledPlugin[] | null {
      if (selection.state === "loading") {
        viewArea.replaceChildren(el("p", "muted plugin-loading", "Loading modules…"));
        return null;
      }
      if (selection.state === "failed") {
        viewArea.replaceChildren(errorPanel(selection.error, load));
        return null;
      }
      const plugins = installedPlugins(selection.value);
      if (selection.value.running.available && plugins.length) return plugins;
      viewArea.replaceChildren(quietState("The host did not report its modules",
        "This host version does not list the modules it loaded, or the running package could not be read."));
      return null;
    }

    function renderOverview(): void {
      renderIncluded();
      renderOthers();
      // The sources keep their place in the document, and with it the keyboard.
      if (viewArea.firstElementChild !== overview || viewArea.childElementCount !== 1) viewArea.replaceChildren(overview);
    }

    /// What Amaterasu Core is made of: a row per family, then what belongs to none.
    function renderIncluded(): void {
      const row = core.view();
      shownUpdate = row.update;
      if (selection.state === "loading") {
        included.replaceChildren(el("p", "muted plugin-loading", "Loading modules…"));
        return;
      }
      if (selection.state === "failed") {
        included.replaceChildren(errorPanel(selection.error, load));
        return;
      }
      const plugins = installedPlugins(selection.value);
      if (!selection.value.running.available || !plugins.length) {
        included.replaceChildren(el("p", "muted plugin-included-note",
          "This host version does not list the modules it loaded, or the running package could not be read."));
        return;
      }
      const running = new Map(plugins.map((plugin) => [plugin.id, plugin]));
      const changes = new Map(row.changes.map((change) => [change.id, change]));
      const { builtIn, foreign } = modulesOutsideFamilies(plugins);
      // Drawing the rows again for a new update keeps the keyboard where it was.
      const focused = document.activeElement instanceof HTMLElement && included.contains(document.activeElement)
        ? document.activeElement.dataset.focusKey : undefined;
      const all = button(lit(`All ${plugins.length} modules`), () => showView("modules"), "link-button plugin-link");
      all.dataset.focusKey = "all modules";
      included.replaceChildren(
        el("header", "plugin-included-head", el("p", "plugin-label", "Included"), all),
        el("ul", "plugin-families",
          ...MODULE_FAMILIES.map((family) => familyRow(family, running, changes)),
          builtIn.length ? looseRow("built-in", BUILT_IN_LABEL,
            "Streaming, pairing, the library, this console and the rest of the host.", builtIn, changes, AMATERASU_SOURCE) : null,
          foreign.length ? looseRow("foreign", "From other developers",
            "Modules another developer wrote that ship inside this package.", foreign, changes, null) : null),
      );
      if (focused)
        Array.from(included.querySelectorAll<HTMLElement>("[data-focus-key]"))
          .find((control) => control.dataset.focusKey === focused)?.focus();
    }

    /// A module's name and version, opening its details in All modules.
    function memberControl(plugin: InstalledPlugin, change: ModuleChange | undefined): HTMLElement {
      const control = button("", () => openModule(plugin.id), "plugin-family-member");
      // Core has one version; a module's own is shown where an update changes it.
      control.replaceChildren(el("span", "plugin-family-member-name", lit(plugin.name)));
      if (change)
        control.append(el("span", "plugin-family-member-version plugin-family-member-version--changes", lit(change.detail)));
      control.dataset.focusKey = `member ${plugin.id}`;
      setLocalizedAttribute(control, "title", "Show in All modules");
      return el("li", "", control);
    }

    function familyState(installed: boolean, changing: boolean): HTMLElement | null {
      if (changing) return el("span", "plugin-family-state plugin-family-state--changes", "Changes in this update");
      return installed ? null : el("span", "plugin-family-state", "Not installed");
    }

    function familyRow(
      family: ModuleFamily,
      running: ReadonlyMap<string, InstalledPlugin>,
      changes: ReadonlyMap<string, ModuleChange>,
    ): HTMLElement {
      const members = el("ul", "plugin-family-members");
      for (const id of family.members) {
        const plugin = running.get(id);
        const change = changes.get(id);
        if (plugin) {
          members.append(memberControl(plugin, change));
        } else if (change?.change === "added") {
          members.append(el("li", "plugin-family-member plugin-family-member--added",
            el("span", "plugin-family-member-name", lit(change.name)),
            el("span", "plugin-family-member-version plugin-family-member-version--changes", lit(change.detail))));
        }
      }
      const installed = family.members.some((id) => running.has(id));
      const heading = el("h3", "plugin-family-name", family.name);
      heading.id = `plugin-family-${family.id}`;
      // RTX HDR is not picked from a list: it is set up where its runtime is downloaded.
      const setup: Part[] = !installed && !members.childElementCount && family.members.includes("amaterasu.rtx-hdr")
        ? [button("Set up RTX HDR", () => context.navigate("integrations"), "link-button plugin-link")]
        : [];
      const sources = el("ul", "plugin-family-sources", ...family.sources.map((source, index) => {
        const link = sourceLink(source);
        link.dataset.focusKey = `source ${family.id} ${index}`;
        return el("li", "", link);
      }));
      setLocalizedAttribute(sources, "aria-label", "Source");
      const item = el("li", `plugin-family${installed || members.childElementCount ? "" : " plugin-family--absent"}`,
        el("div", "plugin-family-title", heading, familyState(installed, family.members.some((id) => changes.has(id)))),
        el("div", "plugin-family-about",
          el("p", "plugin-family-summary", family.summary),
          members.childElementCount ? members : null,
          ...setup),
        sources,
      );
      item.setAttribute("aria-labelledby", heading.id);
      return item;
    }

    /// The modules no family accounts for. A few are named; many are counted,
    /// and the count opens the list of them all.
    function looseRow(
      id: string,
      name: string,
      summary: string,
      plugins: readonly InstalledPlugin[],
      changes: ReadonlyMap<string, ModuleChange>,
      source: SourceRepository | null,
    ): HTMLElement {
      const heading = el("h3", "plugin-family-name", name);
      heading.id = `plugin-family-${id}`;
      const members = el("ul", "plugin-family-members");
      if (plugins.length <= 4) {
        members.append(...plugins.map((plugin) => memberControl(plugin, changes.get(plugin.id))));
      } else {
        const count = button(lit(`${plugins.length} modules`), () => showView("modules"), "plugin-family-member");
        count.dataset.focusKey = `members ${id}`;
        members.append(el("li", "", count));
      }
      const sources = el("ul", "plugin-family-sources");
      if (source) {
        const link = sourceLink(source);
        link.dataset.focusKey = `source ${id}`;
        sources.append(el("li", "", link));
        setLocalizedAttribute(sources, "aria-label", "Source");
      }
      const item = el("li", "plugin-family",
        el("div", "plugin-family-title", heading, familyState(true, plugins.some((plugin) => changes.has(plugin.id)))),
        el("div", "plugin-family-about", el("p", "plugin-family-summary", summary), members),
        sources,
      );
      item.setAttribute("aria-labelledby", heading.id);
      return item;
    }

    /// Opens a module's details in All modules, clearing a filter that would hide it.
    function openModule(id: string): void {
      const plugin = installedPlugins(ready(selection)).find((candidate) => candidate.id === id);
      if (!plugin) return;
      if (!matchesPluginFilter(plugin, installedFilter)) {
        installedFilter.query = "";
        installedFilter.category = "";
      }
      selectedInstalled = id;
      revealInstalled = true;
      showView("modules");
    }

    // --- All modules -------------------------------------------------------------

    function renderInstalled(): void {
      const status = ready(selection);
      const loaded = runningModules();
      if (!loaded || !status) return;
      const plugins: InstalledPlugin[] = loaded;
      const byId = new Map(plugins.map((plugin) => [plugin.id, plugin]));
      const count = el("span", "muted plugin-count");
      const browser = pluginBrowser({
        label: "All modules",
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
      const bar = toolbar(installedFilter, "Search modules", categories, showMatches, count);

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
          quietState("No modules match", "Try a different search or category."),
        );
      }

      /// Selects a module named in another module's details, clearing a
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

      viewArea.replaceChildren(bar, browser.node);
      showMatches();
      if (revealInstalled && selectedInstalled) browser.focus(selectedInstalled);
      revealInstalled = false;
    }

    /// What the host has written down about its packages, for whoever is
    /// following an install or a recovery.
    function packageRecord(status: PackageSelectionStatus): HTMLElement {
      const running = status.running;
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
      return el("details", "plugin-package-details plugin-package-record",
        el("summary", "", "Package record"),
        table(["Recorded as", "Package", "Listed by"], [
          phase("Running now", running.digest),
          phase("Active", status.active),
          phase("Previous", status.previous),
          phase("Pending", status.pending),
          phase("On trial", status.trial),
        ]),
        el("p", "muted", lit(`Only the running package is loaded. The others are what the host has recorded for its next start or a rollback. Highest release sequence accepted: ${status.highestSequence}.`)),
      );
    }

    /// The earlier packages the host has kept, read when the list is opened,
    /// with the request that switches to one.
    function versionsList(): { node: HTMLDetailsElement; sync(): void; forget(): void } {
      const body = el("div", "plugin-versions-body");
      const node = el("details", "plugin-package-details plugin-versions", el("summary", "", "Earlier versions"), body);
      let listed: Loadable<PackageVersions> | null = null;
      let request = 0;
      let switches: HTMLButtonElement[] = [];
      let expanded = false;

      /// A switch is offered only while nothing else is being asked of the host.
      function sync(): void {
        const row = core.view();
        const idle = !row.busy && !updates.working(CORE_UPDATE_SOURCE);
        for (const control of switches) control.disabled = !idle || !canManage;
      }

      async function switchTo(version: PackageVersion): Promise<void> {
        const confirmed = await confirmAction(
          lit(`Switch to package ${shortDigest(version.digest)}?`),
          version.switchesBack
            ? "The host restarts with this earlier package as soon as no stream is running, and keeps the one it has if the earlier one does not start. An automatic update will not bring back the package you are leaving."
            : "The host restarts with this earlier package as soon as no stream is running, and keeps the one it has if the earlier one does not start. This package is from before version switching: once it is running, this console can no longer switch packages, and the way forward is a newer update or the host's command line.",
          "Switch version",
        );
        if (!confirmed || page.signal.aborted) return;
        await updates.change(CORE_UPDATE_SOURCE, { kind: "restore", digest: version.digest });
      }

      function draw(): void {
        switches = [];
        if (!listed || listed.state === "loading" || (listed.state === "ready" && listed.value.loading)) {
          body.replaceChildren(el("p", "muted", "Loading earlier versions…"));
          return;
        }
        if (listed.state === "failed") {
          body.replaceChildren(
            el("p", "plugin-update-refusal", lit(messageOf(listed.error))),
            button("Try again", () => void read()));
          return;
        }
        const { versions: rows, total, supported } = listed.value;
        const earlier = rows.filter((version) => version.state !== "running");
        if (!supported || !earlier.length) {
          body.replaceChildren(el("p", "muted", supported
            ? "The host has not kept an earlier package to switch to."
            : "This host cannot switch between packages."));
          return;
        }
        const action = (version: PackageVersion): Part => {
          if (version.state === "running") return el("span", "muted", "Running now");
          if (!version.restorable) return el("span", "muted", lit(packageVersionReason(version.reason)));
          const control = button("Switch to this version", () => void switchTo(version));
          control.setAttribute("aria-label", `Switch to package ${shortDigest(version.digest)}`);
          switches.push(control);
          return control;
        };
        // The most recent few are what a reader usually wants; the rest are a request away.
        const shown = expanded || rows.length <= COLLAPSED_VERSIONS + 2 ? rows : rows.slice(0, COLLAPSED_VERSIONS);
        const more = shown.length < rows.length
          ? button(lit(`Show all ${rows.length}`), () => { expanded = true; draw(); }, "link-button plugin-more")
          : null;
        body.replaceChildren(
          table(["Package", "Release", "First installed", "Compared with now", ""], shown.map((version) => [
            el("span", "plugin-version-package",
              copyable(version.digest, shortDigest(version.digest), "package digest"),
              version.state === "previous" ? el("span", "muted", "Previous") : null),
            lit(version.sequence),
            version.installedUnixMs === null ? el("span", "muted", "—") : lit(formatDateTime(version.installedUnixMs)),
            lit(packageVersionDifference(version)),
            action(version),
          ])),
          more ?? "",
          el("p", "muted", lit(
            (total > rows.length ? `The ${rows.length} most recent of the ${total} packages this host has kept. ` : "") +
            "The host tries a package before it switches, and keeps running the current one if the other does not start." +
            (canManage ? "" : " Switching takes an administrator who is signed in to the console."))),
          // Said before the choice is made, and again when one of them is chosen.
          earlier.some((version) => version.restorable && !version.switchesBack)
            ? el("p", "muted plugin-versions-oneway", "Packages from before version switching are one way: once one is running, this console can no longer switch packages. They say so when you choose them.")
            : "",
        );
        sync();
      }

      /// A host that has just started may still be reading its packages; it is asked again a few times.
      async function read(attempt = 0): Promise<void> {
        const ticket = ++request;
        if (attempt === 0) {
          listed = { state: "loading" };
          draw();
        }
        let outcome: Loadable<PackageVersions>;
        try {
          const value = parsePackageVersions(await api.post<unknown>("package-update/versions", {}));
          if (value.loading && attempt >= VERSIONS_RETRIES) throw new Error("The host is still reading its packages. Try again in a moment.");
          outcome = { state: "ready", value };
        } catch (error) {
          outcome = { state: "failed", error };
        }
        if (ticket !== request || page.signal.aborted) return;
        listed = outcome;
        draw();
        if (outcome.state === "ready" && outcome.value.loading)
          window.setTimeout(() => { if (ticket === request && !page.signal.aborted) void read(attempt + 1); }, VERSIONS_RETRY_MS);
      }

      node.addEventListener("toggle", () => { if (node.open && !listed) void read(); });
      page.signal.addEventListener("abort", () => { request++; }, { once: true });
      return {
        node,
        sync,
        // The list describes the package that was replaced; it is read again when next opened.
        forget() {
          request++;
          listed = null;
          if (node.open) void read();
        },
      };
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
      const origin = moduleOrigin(plugin.id);
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
          origin.family ? ["Part of", origin.family.name] : null,
          origin.source ? ["Source", el("span", "plugin-source",
            sourceLink(origin.source),
            origin.family || !origin.label ? null : el("span", "muted", origin.label))] : null,
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

    // --- Plugins from other developers ---------------------------------------------

    /// What the repositories offer beyond the host's own package, compared with what the host runs.
    function renderOthers(): void {
      const manage = (ready(listing)?.manageable ?? false) && canManage;
      const add = manage ? button("Add repository", () => openAddRepository()) : null;
      const head = el("header", "plugin-update-source-head",
        el("div", "plugin-update-title",
          el("h2", "plugin-update-name", "Plugins"),
          el("p", "muted plugin-update-about", "Modules from other developers, offered by the repositories this host trusts.")),
        add ? el("div", "plugin-update-status", el("div", "actions plugin-others-actions", add)) : null);
      if (listing.state === "loading") {
        others.replaceChildren(head, el("p", "muted plugin-loading", "Loading repository catalogs…"));
        return;
      }
      if (listing.state === "failed") {
        others.replaceChildren(head, errorPanel(listing.error, load));
        return;
      }
      const plugins = offers();
      if (!plugins.length) {
        others.replaceChildren(head, el("p", "muted plugin-others-empty", activeRepositories().length > 1
          ? "None of this host's repositories offers a plugin from another developer right now."
          : manage
            ? "No plugins yet. Add a developer's repository to see what it offers."
            : "No plugins yet."));
        return;
      }
      const byKey = new Map(plugins.map((plugin) => [plugin.key, plugin]));
      const count = el("span", "muted plugin-count");
      const severalSources = new Set(plugins.map((plugin) => plugin.repository.id)).size > 1;
      const browser = pluginBrowser({
        label: "Plugins from other developers",
        grouped: severalSources,
        inline: true,
        selected: () => selectedAvailable,
        onSelect: (key) => { selectedAvailable = key; },
        detail: (key) => {
          const plugin = byKey.get(key);
          return plugin ? availableDetail(plugin, severalSources ? "h4" : "h3") : null;
        },
        signal: page.signal,
      });
      const categories = PLUGIN_CATEGORIES.filter((category) => plugins.some((plugin) => plugin.category === category));
      const showMatches = (): void => {
        const matches = plugins.filter((plugin) => matchesPluginFilter(plugin, availableFilter));
        count.textContent = matches.length === plugins.length
          ? `${plugins.length} offered`
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
      // A handful of plugins needs no search; a catalog's worth does.
      const searchable = plugins.length > SEARCHABLE_PLUGINS || availableFilter.query !== "" || availableFilter.category !== "";
      others.replaceChildren(
        head,
        ...(searchable ? [toolbar(availableFilter, "Search plugins", categories, showMatches, count)] : []),
        browser.node,
      );
      showMatches();
    }

    function availableDetail(plugin: AvailablePlugin, heading: "h3" | "h4"): HTMLElement {
      const consoleInterface = plugin.kind === "web_bundle";
      const state = consoleInterface
        ? plugin.status === "installed"
          ? "In use. You are looking at this interface."
          : "Not in use."
        : plugin.status === "installed"
          // An offer of the very package the host runs is not listed here.
          ? `Installed. Version ${plugin.version} is loaded from a different package than the one listed here.`
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
          el(heading, "plugin-detail-name", lit(plugin.name)),
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
