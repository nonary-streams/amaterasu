(() => {
  // Capture the signed script URL while the browser exposes document.currentScript.
  const showcaseStylesheetUrl = (() => {
    const sourceUrl = document.currentScript?.src;
    if (!sourceUrl) return "";
    try {
      const stylesheetUrl = new URL("showcase.css", sourceUrl);
      return stylesheetUrl.origin === location.origin ? stylesheetUrl.href : "";
    } catch {
      return "";
    }
  })();

  window.AmaterasuExtensions.v1.register("showcase", {
    title: "UI elements",
    mount(container, sdk) {
      if (sdk?.signal?.aborted) return () => {};
      if (!supportsShowcaseControls(sdk)) {
        return mountCompatibilityFallback(container, sdk?.signal);
      }

      const root = element("section", "okami-showcase");
      root.setAttribute("aria-label", "Component showcase");
      const controls = [];
      let removeStylesheet = () => {};
      let cleanedUp = false;

      const cleanup = () => {
        if (cleanedUp) return;
        cleanedUp = true;
        sdk.signal?.removeEventListener("abort", cleanup);
        for (const control of controls.reverse()) {
          try { control.dispose(); } catch {}
        }
        removeStylesheet();
        root.remove();
      };

      try {
        sdk.signal?.addEventListener("abort", cleanup, { once: true });
        removeStylesheet = attachShowcaseStylesheet(root, sdk);
        const use = (control) => {
          controls.push(control);
          return control;
        };
        const grid = element("div", "showcase-theme-grid");
        const appearance = use(sdk.ui.tabs({
          label: "Appearance",
          tabs: [
            { id: "light", label: "Light", content: buildTheme("light", sdk.ui, use) },
            { id: "dark", label: "Dark", content: buildTheme("dark", sdk.ui, use) },
          ],
        }));
        grid.append(element("h1", "showcase-title", "UI Elements"), appearance.element);
        root.append(grid);
        container.append(root);
        return cleanup;
      } catch (error) {
        cleanup();
        throw error;
      }
    },
  });

  function supportsShowcaseControls(sdk) {
    return sdk?.version === 1 &&
      sdk.ui?.version === 1 &&
      Array.isArray(sdk.features) &&
      ["ui.controls/1", "ui.controls/2", "ui.controls/3", "ui.forms/2", "ui.layout/1",
        "ui.elements/1", "ui.feedback/1", "ui.tabs/1", "ui.dialog/1", "ui.dialog-actions/1", "ui.charts/1",
        "ui.summary/1", "ui.sparklines/1"]
        .every((feature) => sdk.features.includes(feature));
  }

  function mountCompatibilityFallback(container, signal) {
    const root = element("section", "okami-showcase");
    root.setAttribute("aria-label", "Component showcase fallback");
    const heading = element("h2", "", "Basic component showcase");
    const explanation = element("p", "",
      "This host supports the basic preview. Update the host to explore the full component library.");
    const label = element("label", "showcase-field");
    label.append(element("span", "", "Sample text"));
    const input = element("input", "");
    input.type = "text";
    input.setAttribute("aria-label", "Sample text");
    label.append(input);
    const button = element("button", "button secondary", "Try fallback button");
    button.type = "button";
    const status = element("p", "", "Fallback controls are ready.");
    status.setAttribute("role", "status");
    const onButtonClick = () => { status.textContent = "Fallback button works."; };
    button.addEventListener("click", onButtonClick);
    let disposed = false;
    const cleanup = () => {
      if (disposed) return;
      disposed = true;
      signal?.removeEventListener("abort", cleanup);
      button.removeEventListener("click", onButtonClick);
      root.remove();
    };
    try {
      signal?.addEventListener("abort", cleanup, { once: true });
      root.append(heading, explanation, label, button, status);
      container.append(root);
      return cleanup;
    } catch (error) {
      cleanup();
      throw error;
    }
  }

  function attachShowcaseStylesheet(root, sdk) {
    const status = element("p", "showcase-style-status", "Loading showcase styles.");
    status.setAttribute("role", "status");
    status.setAttribute("aria-live", "polite");
    let disposed = false;

    const showFallback = () => {
      if (disposed) return;
      root.dataset.stylesheetState = "fallback";
      status.textContent = "Showcase styles could not be loaded; host styles remain available.";
    };
    const onLoad = () => {
      if (disposed) return;
      root.dataset.stylesheetState = "loaded";
      status.remove();
    };
    root.append(status);
    if (sdk.features.includes("ui.assets/1") && typeof sdk.assets?.stylesheet === "function") {
      void sdk.assets.stylesheet("showcase.css").then(onLoad, showFallback);
      return () => { disposed = true; };
    }
    const stylesheet = document.createElement("link");
    stylesheet.rel = "stylesheet";
    stylesheet.href = showcaseStylesheetUrl;
    const removeListeners = () => {
      stylesheet.removeEventListener?.("load", onLoad);
      stylesheet.removeEventListener?.("error", showFallback);
    };
    const supportsStylesheetLoading = Boolean(showcaseStylesheetUrl) &&
      typeof stylesheet.addEventListener === "function" &&
      typeof document.head?.append === "function";
    if (supportsStylesheetLoading) {
      stylesheet.addEventListener("load", onLoad);
      stylesheet.addEventListener("error", showFallback);
      try {
        document.head.append(stylesheet);
      } catch {
        showFallback();
        stylesheet.remove();
      }
    } else {
      showFallback();
    }
    return () => {
      disposed = true;
      removeListeners();
      stylesheet.remove();
    };
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    node.className = className || "";
    if (text) node.textContent = text;
    return node;
  }

  const deviceRows = [
    ["Living room TV", "Online", "Moonlight", "2 min ago"],
    ["Steam Deck", "Offline", "Moonlight", "1 hour ago"],
    ["Laptop", "Online", "Browser", "Just now"],
  ];

  const metricRows = [
    ["cpu", "CPU", "%", sampleSeries(4, 4, 37, 0.8, { 8: 1.8, 24: 2.4, 40: 1.2, 63: 4, 68: 2.1, 83: 3 })],
    ["gpu", "GPU", "%", sampleSeries(3, 3, 19, 0.55, { 8: 2, 13: 3, 34: 3.4, 43: 1.8, 64: 2, 74: 4.6 })],
    ["encoder", "Encode", "%", sampleSeries(0, 0.5, 29, 0.42, { 6: 4.5, 12: 1.6, 25: 2, 31: 2.7, 48: 3.9, 64: 2.4, 85: 2.5 })],
    ["network", "Network", "Mbps", sampleSeries(0, 1.2, 11, 0.8, { 2: 5, 7: 1.9, 15: 4.4, 25: 6.7, 42: 4, 59: 3.4, 73: 5.3, 89: 2.8 })],
  ];

  function sampleSeries(latest, baseline, seed, variation, spikes) {
    return Array.from({ length: 96 }, (_, time) => {
      seed = (seed * 48271) % 2147483647;
      const ripple = ((seed % 997) / 997 - 0.5) * variation;
      return time === 95 ? latest : Math.max(0, baseline + ripple + (spikes[time] ?? 0));
    });
  }

  function buildTheme(themeName, ui, use) {
    const swatch = element("section", "showcase-theme");
    swatch.setAttribute("data-theme", themeName);
    swatch.setAttribute("aria-label", themeName + " appearance sample");

    const card = (label, panelName, children, appearance = "plain") => {
      const panel = use(ui.card({ label, appearance }));
      panel.element.classList.add("showcase-card");
      panel.element.dataset.panel = panelName;
      panel.content.append(...children);
      swatch.append(panel.element);
    };

    const actionStatus = use(ui.status({ message: "" }));
    actionStatus.element.classList.add("showcase-announcement");
    const button = (label, variant, onClick = () => actionStatus.setMessage("Action received."), disabled = false) =>
      use(ui.button({ label, variant, onClick, disabled })).element;
    const actions = element("div", "showcase-actions showcase-button-variants");
    actions.append(button("Primary", "primary"), button("Secondary", "secondary"),
      button("Outline", "outline"), button("Ghost", "ghost"));
    const states = element("div", "showcase-actions showcase-button-states");
    for (const [label, state] of [["Default", "default"], ["Hover", "hover"], ["Active", "active"], ["Unavailable", "disabled"]]) {
      const sample = button(label, state === "disabled" ? "secondary" : "outline", undefined, state === "disabled");
      sample.dataset.state = state;
      states.append(sample);
    }
    card("Buttons", "buttons", [actions, states, actionStatus.element]);

    const switches = element("div", "showcase-switch-examples");
    for (const [label, value, disabled] of [["On", true, false], ["Off", false, false], ["Disabled", false, true]]) {
      switches.append(use(ui.toggle({ label, value, disabled })).element);
    }
    const checks = element("div", "showcase-switch-examples showcase-check-examples");
    checks.append(
      use(ui.checkbox({ label: "Checked", value: true })).element,
      use(ui.checkbox({ label: "Unchecked", value: false })).element,
      use(ui.checkbox({ label: "Disabled", value: true, disabled: true })).element,
    );
    card("Toggles & checkboxes", "switches", [switches, checks]);

    const badges = element("div", "showcase-actions");
    for (const [label, tone] of [["Online", "success"], ["Warning", "warning"], ["Error", "error"], ["Info", "info"]]) {
      badges.append(use(ui.badge({ label, tone })).element);
    }
    card("Badges", "badges", [badges]);

    const ready = use(ui.statusCard({
      label: "Status card", title: "Host Ready", description: "Example streaming status.", tone: "success",
    }));
    ready.element.dataset.panel = "status";
    swatch.append(ready.element);

    const facts = element("div", "showcase-info-grid");
    for (const [label, icon, value, description] of [
      ["GPU", "gpu", "NVIDIA RTX 4090", "3% · 45 °C"],
      ["Capture", "capture", "WGC", "3840 × 2160 at 120 Hz"],
      ["Encoder", "encoder", "HEVC (NVENC)", "YUV444 · 10-bit"],
    ]) {
      facts.append(use(ui.infoCard({ label, value, description, icon })).element);
    }
    card("Info cards", "facts", [facts]);

    const illustration = element("div", "amaterasu-banner showcase-illustrated");
    const illustrated = use(ui.card({ label: "Card header", appearance: "brush" }));
    illustrated.append(element("p", "", "Use a card to group related settings or explain an action."));
    illustration.append(illustrated.element);
    card("Panel / card", "illustration", [illustration]);

    const quickSettings = [];
    for (const [label, value] of [
      ["Enable streaming", true], ["Use virtual display", true], ["HDR (10-bit)", true],
      ["Auto-start host", false], ["Keep host awake", true],
    ]) {
      quickSettings.push(use(ui.toggle({ label, value })).element);
    }
    card("Quick settings", "quick", quickSettings);

    const metrics = element("div", "showcase-metrics");
    for (const [id, label, unit, samples] of metricRows) {
      metrics.append(use(ui.metricChart({
        label, unit, minimum: 0, maximum: 10, appearance: "sparkline",
        series: [{ id, label, points: samples.map((value, time) => ({ time, value })) }],
      })).element);
    }
    card("Metric / performance", "metrics", [metrics]);

    const table = use(ui.table({
      caption: "Sample paired devices",
      columns: ["Name", "Status", "Type", "Last seen"].map((header) => ({ header })),
      rows: deviceRows,
    }));
    const deviceStatus = use(ui.status({ message: "3 devices shown." }));
    const search = use(ui.searchField({
      label: "Find a device", value: "", placeholder: "Search by name",
      onChange(query) {
        const match = query.trim().toLowerCase();
        const visible = deviceRows.filter(([name]) => name.toLowerCase().includes(match));
        table.setRows(visible);
        deviceStatus.setMessage(
          visible.length === 0 ? "No matching devices." :
            visible.length === 1 ? "1 device shown." : `${visible.length} devices shown.`,
        );
      },
    }));
    const text = use(ui.textField({
      label: "Device name", value: "", placeholder: "Device name",
      required: true,
      onChange: (value) => text.setState({ error: value.trim() ? null : "Name is required." }),
    }));
    const select = use(ui.select({
      label: "Profile", value: "balanced", choices: [
        { value: "balanced", label: "Balanced" }, { value: "quality", label: "High quality" },
        { value: "latency", label: "Low latency" },
      ],
    }));
    card("Inputs", "inputs", [text.element, select.element, search.element]);

    const streamingSettings = element("div", "showcase-tab-controls");
    streamingSettings.append(
      use(ui.slider({ label: "Target bitrate", minimum: 10, maximum: 80, step: 5, value: 30 })).element,
      use(ui.numberField({ label: "Target frame rate", minimum: 30, maximum: 240, step: 1, value: 60 })).element,
    );
    const renderingSettings = use(ui.radioGroup({
      label: "Scaling mode", value: "fit", choices: [
        { value: "fit", label: "Fit to screen" }, { value: "native", label: "Native resolution" },
      ],
    }));
    const tabs = use(ui.tabs({
      label: "Settings sections",
      tabs: [
        { id: "general", label: "General", content: element("p", "", "Set how the host starts.") },
        { id: "streaming", label: "Streaming", content: streamingSettings },
        { id: "audio", label: "Audio", content: element("p", "", "Choose an audio device.") },
        { id: "advanced", label: "Advanced", content: renderingSettings.element },
      ],
    }));
    const progress = use(ui.progress({ label: "Scan progress", maximum: 100, value: 64, message: "64%" }));
    const tabProgress = element("div", "showcase-tab-progress");
    for (const [label, child] of [["Tabs", tabs.element], ["Progress bar", progress.element]]) {
      const panel = use(ui.card({ label }));
      panel.element.classList.add("showcase-inner-card");
      panel.append(child);
      tabProgress.append(panel.element);
    }
    tabProgress.dataset.panel = "tab-progress";
    swatch.append(tabProgress);

    const alerts = [];
    for (const [severity, message] of [
      ["success", "The profile was saved."],
      ["warning", "Changes take effect on the next stream."],
      ["error", "Could not connect to the sample device."],
      ["info", "A new sample profile is available."],
    ]) {
      const alert = use(ui.alert({ message, severity, dismissible: severity === "info" }));
      alerts.push(alert.element);
    }
    card("Alerts / notifications", "alerts", alerts);

    const confirm = button("Confirm", "primary", () => dialog.close());
    const dialog = use(ui.dialog({
      title: "Confirm changes",
      description: "Apply these sample settings?",
      actions: [confirm],
    }));
    const dialogPreview = element("div", "amaterasu-blossom-panel showcase-dialog-preview");
    const dialogActions = element("div", "showcase-actions showcase-dialog-actions");
    dialogActions.append(button("Cancel", "outline", () => actionStatus.setMessage("Action cancelled.")),
      button("Open dialog", "primary", () => dialog.open()));
    dialogPreview.append(element("h3", "", "Confirm changes"),
      element("p", "", "Apply these sample settings?"), dialogActions, dialog.element);
    card("Modal", "modal", [dialogPreview]);
    deviceStatus.element.classList.add("showcase-announcement");
    card("Table", "devices", [table.element, deviceStatus.element]);

    const tooltipText = "Pair a client to start streaming.";
    const help = use(ui.tooltip({ trigger: button("Pair client", "ghost"), message: tooltipText }));
    const note = element("p", "plugin-tooltip showcase-tooltip-preview", tooltipText);
    card("Tooltip", "tooltip", [note, help.element]);

    const icons = element("div", "showcase-actions");
    for (const [icon, label, message, variant] of [
      ["settings", "Profile settings", "Open the sample profile settings.", "outline"],
      ["edit", "Edit profile", "Edit the sample streaming profile.", "outline"],
      ["delete", "Remove profile", "Remove the sample streaming profile.", "danger"],
      ["more", "More options", "See additional profile actions.", "outline"],
    ]) {
      const action = use(ui.iconButton({ icon, label, variant, onClick: () => actionStatus.setMessage(label + " selected.") }));
      const tooltip = use(ui.tooltip({ trigger: action.element, message }));
      icons.append(tooltip.element);
    }
    card("Icon buttons", "icons", [icons]);
    return swatch;
  }

})();
