// A consuming UI showcase using the host's versioned extension control SDK.
window.AmaterasuExtensions.v1.register("showcase", {
  title: "UI elements",
  mount(container, sdk) {
    const style = document.createElement("style");
    style.textContent = ".okami-showcase{display:grid;gap:20px;color:var(--fg)}" +
      ".okami-showcase *{box-sizing:border-box}" +
      ".showcase-intro{padding:clamp(18px,3vw,30px);border:1px solid var(--line);border-radius:16px;background:var(--panel)}" +
      ".showcase-intro h2,.showcase-theme>h3{margin:0;font:500 clamp(24px,3vw,34px)/1.15 var(--font-display)}" +
      ".showcase-intro p{max-width:64ch;color:var(--secondary)}" +
      ".showcase-brush{width:min(190px,48vw);height:4px;margin-top:16px;border-radius:99px;background:linear-gradient(90deg,var(--accent),var(--gold),transparent);transform:rotate(-1deg);transform-origin:left}" +
      ".showcase-theme-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:16px;align-items:start}" +
      ".showcase-theme{min-width:0;display:grid;gap:12px;padding:clamp(12px,2vw,20px);border:1px solid var(--line);border-radius:16px;color:var(--fg);background:var(--bg)}" +
      ".showcase-theme>h3{font-size:20px}" +
      ".showcase-panel{min-width:0;margin:0;padding:16px}" +
      ".showcase-panel h4{margin:0 0 12px;font:500 16px/1.3 var(--font-display)}" +
      ".showcase-actions{display:flex;flex-wrap:wrap;gap:8px;align-items:center}" +
      ".showcase-field{display:grid;gap:6px;margin:10px 0}" +
      ".showcase-field>span:first-child{color:var(--fg);font-weight:600}" +
      ".showcase-field input{width:100%;min-height:40px;padding:8px 10px;border:1px solid var(--line);border-radius:9px;color:var(--fg);background:var(--bg);font:inherit}" +
      ".showcase-field input[aria-invalid=true]{border-color:var(--danger)}" +
      ".showcase-field small{color:var(--muted)}" +
      ".showcase-tabs{display:flex;gap:6px;overflow-x:auto;border-bottom:1px solid var(--line)}" +
      ".showcase-tab[aria-selected=true]{color:var(--accent);border-color:var(--accent);background:var(--selected-bg)}" +
      ".showcase-tab-panel{min-height:74px;padding:12px 4px;color:var(--secondary)}" +
      ".showcase-alert{margin:8px 0 0;padding:12px;border:1px solid var(--warning-border);border-radius:10px;color:var(--warning-text);background:var(--warning-bg)}" +
      ".showcase-alert[data-kind=error]{color:var(--error-text);border-color:var(--error-border);background:var(--error-bg)}" +
      ".showcase-table-wrap{overflow-x:auto}.showcase-table{width:100%;border-collapse:collapse}" +
      ".showcase-table th,.showcase-table td{padding:9px 10px;border-bottom:1px solid var(--line);text-align:left;white-space:nowrap}" +
      ".showcase-table th{color:var(--secondary);font-size:12px}" +
      ".showcase-modal{width:min(calc(100% - 32px),480px);max-width:480px;border:1px solid var(--line);border-radius:16px;color:var(--fg);background:var(--panel)}" +
      ".showcase-modal::backdrop{background:color-mix(in srgb,var(--ink) 68%,transparent)}" +
      ".showcase-modal h4{font:500 21px/1.2 var(--font-display)}" +
      "@media(max-width:720px){.showcase-theme-grid{grid-template-columns:minmax(0,1fr)}.showcase-theme{padding:12px}.showcase-panel{padding:14px}.showcase-actions .button{flex:1 1 auto}}" +
      "@media(prefers-reduced-motion:reduce){.okami-showcase *,.okami-showcase *::before,.okami-showcase *::after{scroll-behavior:auto;transition:none}}";
    const root = element("main", "okami-showcase");
    root.setAttribute("aria-label", "Component showcase");
    const intro = element("header", "showcase-intro");
    intro.append(element("h2", "", "Ink, paper, and living controls"),
      element("p", "", "A component board for Amaterasu's Ōkami theme. The controls use host UI SDK components and semantic theme tokens."),
      element("div", "showcase-brush"));
    const grid = element("div", "showcase-theme-grid");
    const controls = [];
    const dialogs = [];
    grid.append(buildTheme("light", sdk, controls, dialogs), buildTheme("dark", sdk, controls, dialogs));
    root.append(intro, grid);
    container.append(style, root);
    return () => {
      controls.forEach((control) => control.dispose());
      dialogs.forEach((dialog) => dialog.close());
      style.remove();
      root.remove();
    };
  },
});

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
  const toggle = sdk.ui.toggle({ label: "Enable detail", value: true });
  const select = sdk.ui.select({ label: "Profile", choices: [
    { value: "balanced", label: "Balanced" }, { value: "quiet", label: "Quiet" }, { value: "sharp", label: "Sharp" },
  ], value: "balanced" });
  const radios = sdk.ui.radioGroup({ label: "Surface", choices: [
    { value: "paper", label: "Paper" }, { value: "ink", label: "Ink" },
  ], value: "paper" });
  const slider = sdk.ui.slider({ label: "Brush strength", minimum: 0, maximum: 100, step: 5, value: 65 });
  [toggle, select, radios, slider].forEach((control) => { controls.push(control); shared.append(control.element); });
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
