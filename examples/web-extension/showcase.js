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
      if (!supportsShowcaseControls(sdk)) return mountCompatibilityFallback(container);
      if (sdk.signal?.aborted) return () => {};

      const root = element("main", "okami-showcase");
      root.setAttribute("aria-label", "Component showcase");
      const controls = [];
      const dialogs = [];
      let removeStylesheet = () => {};
      let cleanedUp = false;

      const cleanup = () => {
        if (cleanedUp) return;
        cleanedUp = true;
        sdk.signal?.removeEventListener("abort", cleanup);
        for (const control of controls) {
          try { control.dispose(); } catch {}
        }
        for (const dialog of dialogs) {
          try { dialog.close(); } catch {}
        }
        removeStylesheet();
        root.remove();
      };

      try {
        sdk.signal?.addEventListener("abort", cleanup, { once: true });
        removeStylesheet = attachShowcaseStylesheet(root);
        const intro = element("header", "showcase-intro");
        intro.append(element("h2", "", "Ink, paper, and living controls"),
          element("p", "", "A component board for Amaterasu's Ōkami theme. The controls use host UI SDK components and semantic theme tokens."),
          element("div", "showcase-brush"));
        const grid = element("div", "showcase-theme-grid");
        grid.append(buildTheme("light", sdk, controls, dialogs),
          buildTheme("dark", sdk, controls, dialogs));
        root.append(intro, grid);
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
      sdk.features.includes("ui.controls/1");
  }

  function mountCompatibilityFallback(container) {
    const root = element("main", "okami-showcase");
    root.setAttribute("aria-label", "Component showcase fallback");
    const heading = element("h2", "", "Basic component showcase");
    const explanation = element("p", "",
      "This host does not provide the versioned reusable controls. Basic native controls remain available.");
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
    button.addEventListener("click", () => { status.textContent = "Fallback button works."; });
    root.append(heading, explanation, label, button, status);
    container.append(root);
    return () => root.remove();
  }

  function attachShowcaseStylesheet(root) {
    const stylesheet = document.createElement("link");
    stylesheet.rel = "stylesheet";
    stylesheet.href = showcaseStylesheetUrl;
    const status = element("p", "showcase-style-status",
      "Showcase styles could not be loaded; host styles remain available.");
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
    const removeListeners = () => {
      stylesheet.removeEventListener?.("load", onLoad);
      stylesheet.removeEventListener?.("error", showFallback);
    };
    const supportsStylesheetLoading = Boolean(showcaseStylesheetUrl) &&
      typeof stylesheet.addEventListener === "function" &&
      typeof document.head?.append === "function";
    if (supportsStylesheetLoading) {
      status.textContent = "Loading showcase styles.";
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
    try {
      root.append(status);
    } catch (error) {
      disposed = true;
      removeListeners();
      stylesheet.remove();
      throw error;
    }
    return () => {
      disposed = true;
      removeListeners();
      stylesheet.remove();
    };
  }

  function retainControl(control, controls) {
    controls.push(control);
    return control;
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    node.className = className || "";
    if (text) node.textContent = text;
    return node;
  }

  function panel(title) {
    const section = element("section", "panel showcase-panel");
    section.append(element("h4", "", title));
    return section;
  }

  function buildTheme(themeName, sdk, controls, dialogs) {
    const swatch = element("section", "showcase-theme");
    swatch.setAttribute("data-theme", themeName);
    swatch.setAttribute("aria-label", themeName + " appearance sample");
    swatch.append(element("h3", "", themeName[0].toUpperCase() + themeName.slice(1) + " appearance"));

    const buttons = panel("Buttons and states");
    const actions = element("div", "showcase-actions");
    const actionStatus = element("span", "badge good", "Ready");
    const primary = element("button", "button primary", "Primary action");
    primary.type = "button";
    primary.addEventListener("click", () => { actionStatus.textContent = "Action received"; });
    const secondary = element("button", "button secondary", "Secondary");
    secondary.type = "button";
    const disabled = element("button", "button secondary", "Unavailable");
    disabled.type = "button";
    disabled.disabled = true;
    actions.append(primary, secondary, disabled, actionStatus);
    buttons.append(actions);
    swatch.append(buttons);

    const inputs = panel("Inputs and validation");
    const emailField = element("label", "showcase-field");
    emailField.append(element("span", "", "Email address"));
    const email = element("input", "");
    email.type = "email";
    email.name = "email-" + themeName;
    email.required = true;
    email.placeholder = "name@example.com";
    email.setAttribute("aria-invalid", "true");
    email.setAttribute("aria-describedby", "email-help-" + themeName);
    const emailHelp = element("small", "", "Required field · validation feedback");
    emailHelp.id = "email-help-" + themeName;
    emailField.append(email, emailHelp);
    const notesField = element("label", "showcase-field");
    notesField.append(element("span", "", "Notes"));
    const notes = element("input", "");
    notes.type = "text";
    notes.name = "notes-" + themeName;
    notes.placeholder = "Use Tab to move through controls";
    notesField.append(notes);
    inputs.append(emailField, notesField);
    swatch.append(inputs);

    const tabsPanel = panel("Keyboard tabs");
    const tabList = element("div", "showcase-tabs");
    tabList.setAttribute("role", "tablist");
    tabList.setAttribute("aria-label", themeName + " sample tabs");
    const tabNames = ["Overview", "Details", "History"];
    const tabButtons = [];
    const tabPanels = [];
    function selectTab(index, focus) {
      tabButtons.forEach((tab, tabIndex) => {
        const selected = tabIndex === index;
        tab.setAttribute("aria-selected", String(selected));
        tab.setAttribute("tabindex", selected ? "0" : "-1");
        tabPanels[tabIndex].hidden = !selected;
      });
      if (focus) tabButtons[index].focus();
    }
    tabNames.forEach((name, index) => {
      const tab = element("button", "button secondary showcase-tab", name);
      tab.type = "button";
      tab.id = "showcase-" + themeName + "-tab-" + index;
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-controls", "showcase-" + themeName + "-panel-" + index);
      tab.setAttribute("aria-selected", index === 0 ? "true" : "false");
      tab.setAttribute("tabindex", index === 0 ? "0" : "-1");
      tab.addEventListener("click", () => selectTab(index, false));
      tab.addEventListener("keydown", (event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const active = tabButtons.findIndex((candidate) => candidate.getAttribute("aria-selected") === "true");
        const next = event.key === "Home" ? 0 : event.key === "End" ? tabButtons.length - 1 :
          (active + (event.key === "ArrowRight" ? 1 : -1) + tabButtons.length) % tabButtons.length;
        selectTab(next, true);
      });
      tabButtons.push(tab);
      tabList.append(tab);
      const content = element("div", "showcase-tab-panel", name + " content · arrow keys move between tabs");
      content.id = "showcase-" + themeName + "-panel-" + index;
      content.setAttribute("role", "tabpanel");
      content.setAttribute("aria-labelledby", tab.id);
      content.setAttribute("tabindex", "0");
      content.hidden = index !== 0;
      tabPanels.push(content);
    });
    tabsPanel.append(tabList, ...tabPanels);
    swatch.append(tabsPanel);

    const shared = panel("Reusable host controls");
    const toggle = retainControl(
      sdk.ui.toggle({ label: "Enable detail", value: true }), controls);
    const select = retainControl(sdk.ui.select({ label: "Profile", choices: [
      { value: "balanced", label: "Balanced" }, { value: "quiet", label: "Quiet" }, { value: "sharp", label: "Sharp" },
    ], value: "balanced" }), controls);
    const radios = retainControl(sdk.ui.radioGroup({ label: "Surface", choices: [
      { value: "paper", label: "Paper" }, { value: "ink", label: "Ink" },
    ], value: "paper" }), controls);
    const slider = retainControl(sdk.ui.slider({
      label: "Brush strength", minimum: 0, maximum: 100, step: 5, value: 65,
    }), controls);
    [toggle, select, radios, slider].forEach((control) => shared.append(control.element));

    swatch.append(shared);

    const statuses = panel("Cards and status");
    statuses.append(element("p", "muted", "Host status"),
      element("span", "badge good", "Ready"), element("span", "badge accent", "Preview"),
      element("span", "badge bad", "Needs attention"));
    swatch.append(statuses);

    const alerts = panel("Alerts");
    const warning = element("aside", "showcase-alert", "Warning · Review this setting before saving.");
    warning.setAttribute("role", "status");
    const error = element("aside", "showcase-alert", "Error · The requested value is unavailable.");
    error.setAttribute("role", "alert");
    error.setAttribute("data-kind", "error");
    alerts.append(warning, error);
    swatch.append(alerts);

    const tablePanel = panel("Table");
    const tableScroll = element("div", "showcase-table-wrap");
    const table = element("table", "showcase-table");
    table.append(element("caption", "", "Provider readiness"));
    const head = element("thead");
    const headerRow = element("tr");
    ["Provider", "State", "Updated"].forEach((value) => {
      const heading = element("th", "", value);
      heading.setAttribute("scope", "col");
      headerRow.append(heading);
    });
    head.append(headerRow);
    const body = element("tbody");
    [["Steam", "Ready", "Just now"], ["Lutris", "Scanning", "1 min ago"]].forEach((row) => {
      const tr = element("tr");
      row.forEach((value, index) => {
        const cell = element("td", "", value);
        if (index === 0) cell.setAttribute("scope", "row");
        tr.append(cell);
      });
      body.append(tr);
    });
    table.append(head, body);
    tableScroll.append(table);
    tablePanel.append(tableScroll);
    swatch.append(tablePanel);

    const modalPanel = panel("Modal and focus return");
    const trigger = element("button", "button secondary", "Open dialog");
    trigger.type = "button";
    const dialog = element("dialog", "showcase-modal");
    dialog.id = "showcase-" + themeName + "-dialog";
    dialog.setAttribute("aria-labelledby", "showcase-" + themeName + "-dialog-title");
    const title = element("h4", "", "Confirm this sample");
    title.id = "showcase-" + themeName + "-dialog-title";
    const copy = element("p", "", "Native modal behavior keeps focus inside until the dialog closes.");
    const modalActions = element("div", "actions");
    const cancel = element("button", "button secondary", "Cancel");
    cancel.type = "button";
    cancel.addEventListener("click", () => dialog.close());
    const confirm = element("button", "button primary", "Continue");
    confirm.type = "button";
    confirm.addEventListener("click", () => dialog.close());
    modalActions.append(cancel, confirm);
    dialog.append(title, copy, modalActions);
    trigger.setAttribute("aria-haspopup", "dialog");
    trigger.setAttribute("aria-controls", dialog.id);
    trigger.addEventListener("click", () => dialog.showModal());
    dialog.addEventListener("close", () => trigger.focus());
    dialogs.push(dialog);
    modalPanel.append(trigger, dialog);
    swatch.append(modalPanel);
    return swatch;
  }
  })();
