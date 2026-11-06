import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class ChildList extends Array {
  item(index) { return this[index] ?? null; }
}

class Element {
  constructor(tagName) {
    this.tagName = String(tagName).toUpperCase();
    this.children = new ChildList();
    this.parentElement = null;
    this.listeners = new Map();
    this.attributes = new Map();
    this.className = "";
    this.value = "";
    this.checked = false;
    this.disabled = false;
    this.hidden = false;
    this.required = false;
    this.type = "";
    this.name = "";
    this.isConnected = false;
    this.tabIndex = 0;
  }
  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      this.children.push(child);
      if (typeof child !== "string") {
        child.parentElement = this;
        child.setConnected?.(this.isConnected);
      }
    }
  }
  replaceChildren(...children) {
    for (const child of this.children) {
      if (typeof child !== "string") {
        child.parentElement = null;
        child.setConnected?.(false);
      }
    }
    this.children = new ChildList();
    this.append(...children);
  }
  setConnected(value) {
    this.isConnected = value;
    for (const child of this.children)
      if (typeof child !== "string") child.setConnected?.(value);
  }
  addEventListener(type, callback, options = {}) {
    const entries = this.listeners.get(type) ?? [];
    entries.push({ callback, signal: options.signal, capture: options.capture === true });
    this.listeners.set(type, entries);
  }
  removeEventListener(type, callback) {
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter(entry => entry.callback !== callback));
  }
  dispatchEvent(event) {
    if (!event.target) event.target = this;
    let node = this;
    while (node) {
      event.currentTarget = node;
      for (const entry of node.listeners.get(event.type) ?? []) {
        if (entry.signal?.aborted) continue;
        entry.callback(event);
        if (event.immediatePropagationStopped) break;
      }
      if (!event.bubbles || event.propagationStopped) break;
      node = node.parentElement;
    }
    return !event.defaultPrevented;
  }
  fire(type, extras = {}) {
    const event = {
      type, target: this, bubbles: true, defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true; },
      stopPropagation() { this.propagationStopped = true; },
      stopImmediatePropagation() { this.immediatePropagationStopped = true; this.propagationStopped = true; },
      ...extras,
    };
    this.dispatchEvent(event);
    return event;
  }
  click() { if (!this.disabled && !this.hidden) this.fire("click"); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  hasAttribute(name) { return this.attributes.has(name); }
  removeAttribute(name) { this.attributes.delete(name); }
  set textContent(value) {
    this.replaceChildren(String(value));
  }
  get textContent() {
    return this.children.map(child => typeof child === "string" ? child : child.textContent).join("");
  }
  get valueAsNumber() { return Number(this.value); }
  set valueAsNumber(value) { this.value = String(value); }
  contains(target) {
    for (let node = target; node; node = node.parentElement) if (node === this) return true;
    return false;
  }
  closest(selector) {
    for (let node = this; node; node = node.parentElement) if (matches(node, selector)) return node;
    return null;
  }
  querySelectorAll(selector) {
    const selectors = selector.split(",").map(value => value.trim());
    const found = [];
    const visit = node => {
      for (const child of node.children) {
        if (typeof child === "string") continue;
        if (selectors.some(value => matches(child, value))) found.push(child);
        visit(child);
      }
    };
    visit(this);
    return found;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] ?? null; }
  reportValidity() { return true; }
  focus() { this.focused = true; }
}
class HTMLInputElement extends Element { constructor() { super("input"); } }
class HTMLTextAreaElement extends Element { constructor() { super("textarea"); } }
class HTMLSelectElement extends Element { constructor() { super("select"); } }
class HTMLButtonElement extends Element { constructor() { super("button"); } }
function matches(node, selector) {
  if (selector === "input, textarea, select") return ["INPUT", "TEXTAREA", "SELECT"].includes(node.tagName);
  if (selector === "input, select, textarea") return ["INPUT", "TEXTAREA", "SELECT"].includes(node.tagName);
  if (selector === "input[type=checkbox]") return node.tagName === "INPUT" && node.type === "checkbox";
  if (selector === "button") return node.tagName === "BUTTON";
  if (selector === "[role=status]") return node.getAttribute("role") === "status";
  if (selector.startsWith(".")) return node.className.split(/\s+/).includes(selector.slice(1));
  if (selector.includes(" > ")) {
    const [parentSelector, childSelector] = selector.split(" > ");
    return matches(node, childSelector) && !!node.parentElement && matches(node.parentElement, parentSelector);
  }
  return node.tagName === selector.toUpperCase();
}
const find = (node, predicate) => {
  if (predicate(node)) return node;
  for (const child of node.children ?? []) {
    if (typeof child === "string") continue;
    const result = find(child, predicate);
    if (result) return result;
  }
  return null;
};
const tick = async () => { for (let i = 0; i < 10; i++) await new Promise(resolve => setImmediate(resolve)); };
const ordinary = value => JSON.parse(JSON.stringify(value));

const sources = [
  "src/types.ts", "src/model.ts", "src/api.ts", "src/ui.ts",
  "src/list_editor.ts", "src/steam_exclusion_picker.ts", "src/settings.ts",
];
function fixture() {
  const document = {
    createElement(name) {
      if (name === "input") return new HTMLInputElement();
      if (name === "textarea") return new HTMLTextAreaElement();
      if (name === "select") return new HTMLSelectElement();
      if (name === "button") return new HTMLButtonElement();
      return new Element(name);
    },
    createTextNode(value) { return String(value); },
  };
  class MutationObserver { constructor(callback) { this.callback = callback; } observe() {} disconnect() {} }
  const context = vm.createContext({
    document, TextEncoder, URL, setTimeout, clearTimeout, setImmediate,
    MutationObserver, AbortController, Event: class { constructor(type, init = {}) { this.type = type; this.bubbles = init.bubbles ?? false; } },
    HTMLInputElement, HTMLTextAreaElement, HTMLSelectElement, HTMLButtonElement,
  });
  for (const source of sources) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("if (typeof Amaterasu !== \"undefined\") Object.assign(globalThis, Amaterasu)", context);
  }
  vm.runInContext(`
    globalThis.ApiError = Amaterasu.ApiError;
    globalThis.renderExtensionSlot = () => {};
    globalThis.renderHostMetricsDashboard = () => {};
    globalThis.renderUpdaterSettings = () => {};
    globalThis.renderSystemTraySettings = () => {};
    globalThis.renderManagementPolicy = () => {};
    globalThis.renderCsrfPolicy = () => {};
    globalThis.createPairedClientEditor = () => async () => {};
  `, context);

  const steam = {
    ok: true, revision: 1,
    values: {
      enabled: true, auto_sync: true, include_tools: false,
      sync_all_installed: true, autosync_remove_uninstalled: true,
      recent_games: 20, recent_max_age_days: 365,
      exclude_games: [{ name: "Name-only legacy" }, { id: "999", name: "Unavailable legacy" }],
    },
  };
  const lutris = {
    ok: true, revision: 1,
    values: {
      enabled: true, auto_sync: true, autosync_remove_uninstalled: true,
      include_steam: false,
      exclude_games: ["77", { name: "Legacy name-only" }, { id: "999", name: "Unavailable game" }],
    },
  };
  const playnite = {
    ok: true, revision: 1,
    values: {
      enabled: true, auto_sync: true, sync_all_installed: true,
      autosync_remove_uninstalled: true, autosync_delete_after_days: 0,
      autosync_require_replacement: false, focus_attempts: 3,
      focus_timeout_secs: 5, focus_exit_on_first: true,
      fullscreen_entry_enabled: true, recent_games: 30, recent_max_age_days: 365,
      sync_categories: ["legacy-category"], exclude_categories: ["unknown-category"],
      sync_plugins: ["vendor.unknown"], exclude_plugins: ["plugin.disabled"],
      exclude_games: ["retired-game-id"],
    },
  };
  const state = { steam, lutris, playnite };
  const calls = [];
  const conflictNext = new Set();
  const conflictOverrides = new Map();
  const permissions = new Set(["configuration.read", "configuration.write", "steam.read"]);
  const apiError = (code) => vm.runInContext(`new Amaterasu.ApiError("provider error", ${JSON.stringify(code)}, 409)`, context);
  const status = {
    steam: { settings_ready: true, enabled: true, game_count: 2, scanning: false, complete: true, warnings: [], warning_count: 0 },
    lutris: { ok: true, settings_ready: true, enabled: true, auto_sync: true, autosync_remove_uninstalled: true, include_steam: false, available: true, platform_supported: true, database_available: true, executable_available: true, settings_revision: 1, game_count: 2, refresh_pending: false, last_error: null },
    playnite: { ok: true, settings_ready: true, enabled: true, auto_sync: true, playnite_running: true, playnite_paths: [], snapshot_pending: false, snapshot_game_count: 2, launchable_game_count: 2, active_provider_launches: 0, catalog_sync: null, last_error: null, revision: 1 },
  };
  const api = {
    allows(scope) { return permissions.has(scope); },
    async post(route, body = {}) {
      calls.push([route, ordinary(body)]);
      const match = /^(steam|lutris|playnite)\/(settings|settings_update|status|list|refresh)$/.exec(route);
      if (!match) throw new Error(`unavailable route: ${route}`);
      const [, provider, operation] = match;
      if (operation === "settings") return ordinary(state[provider]);
      if (operation === "status") return ordinary(status[provider]);
      if (provider === "steam" && operation === "list")
        return { revision: 1, games: [{ app_id: 10, name: "Known game", installed: true }], next_after: body.after ? null : 10, scanning: false, complete: true };
      if (operation === "refresh") return { ok: true };
      if (operation === "settings_update") {
        if (conflictNext.delete(provider)) {
          state[provider].revision += 1;
          if (provider === "steam") state[provider].values.auto_sync = false;
          if (provider === "lutris") state[provider].values.auto_sync = false;
          if (provider === "playnite") state[provider].values.auto_sync = true;
          Object.assign(state[provider].values, conflictOverrides.get(provider) ?? {});
          throw apiError("revision_conflict");
        }
        const values = { ...body };
        delete values.expected_revision;
        state[provider] = { ok: true, revision: state[provider].revision + 1, values };
        status[provider].revision = state[provider].revision;
        status[provider].settings_revision = state[provider].revision;
        return ordinary(state[provider]);
      }
      throw new Error(`unhandled route: ${route}`);
    },
  };
  const content = new Element("main");
  content.setConnected(true);
  let draftGuard = null;
  const notices = [];
  context.Amaterasu.renderSettings({
    content, api, initialSettingsCategory: "integrations",
    notify(message) { notices.push(message); },
    registerDraftGuard(isDirty, dispose) { draftGuard = { isDirty, dispose }; },
  });
  return { context, content, state, calls, conflictNext, conflictOverrides, permissions, api, notices, get draftGuard() { return draftGuard; } };
}
function areaFor(f, provider) {
  return find(f.content, node => node.className === `settings-area-${provider}`);
}
function formFor(f, provider) {
  return find(areaFor(f, provider), node => node.tagName === "FORM");
}
function inputNamed(form, name) {
  return form.querySelectorAll("input, textarea, select").find(control => control.name === name);
}
function buttonNamed(form, label) {
  return form.querySelectorAll("button").find(button => button.textContent === label);
}
function submit(form) { form.fire("submit", { preventDefault() { this.defaultPrevented = true; } }); }

// Mount and submit the actual provider settings forms; exercise the same save functions and API routes as the app.
test("Steam picker cancel, revisioned save, legacy retention, and conflict draft rebasing", async () => {
  const f = fixture();
  await tick();
  let form = formFor(f, "steam");
  assert.ok(form);
  assert.equal(f.draftGuard.isDirty(), false);
  let game = find(form, node => node.tagName === "LABEL" && node.textContent.includes("Known game"));
  game.querySelector("input").checked = true;
  game.querySelector("input").fire("change");
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(f.content, "Refresh")?.disabled, true);
  assert.deepEqual(ordinary(f.state.steam.values.exclude_games), [
    { name: "Name-only legacy" }, { id: "999", name: "Unavailable legacy" },
  ]);
  buttonNamed(form, "Cancel").click();
  await tick();
  form = formFor(f, "steam");
  assert.equal(f.draftGuard.isDirty(), false);
  assert.equal(find(form, node => node.tagName === "LABEL" && node.textContent.includes("Known game")).querySelector("input").checked, false);
  assert.equal(f.calls.some(([route]) => route === "steam/settings_update"), false);

  game = find(form, node => node.tagName === "LABEL" && node.textContent.includes("Known game"));
  game.querySelector("input").checked = true;
  game.querySelector("input").fire("change");
  const autoSync = inputNamed(form, "steam_auto_sync");
  autoSync.checked = false;
  autoSync.fire("change");
  submit(form);
  await tick();
  assert.deepEqual(f.calls.find(([route]) => route === "steam/settings_update")[1], {
    enabled: true, auto_sync: false, include_tools: false, sync_all_installed: true,
    autosync_remove_uninstalled: true, recent_games: 20, recent_max_age_days: 365,
    exclude_games: [{ name: "Name-only legacy" }, { id: "999", name: "Unavailable legacy" }, { id: "10", name: "Known game" }],
    expected_revision: 1,
  });
  assert.equal(f.state.steam.revision, 2);
  assert.deepEqual(ordinary(f.state.steam.values.exclude_games.slice(0, 2)), [
    { name: "Name-only legacy" }, { id: "999", name: "Unavailable legacy" },
  ]);

  f.conflictNext.add("steam");
  form = formFor(f, "steam");
  const oldRevisionDraft = inputNamed(form, "steam_auto_sync");
  oldRevisionDraft.checked = true;
  oldRevisionDraft.fire("change");
  inputNamed(form, "steam_recent_games").value = "5";
  inputNamed(form, "steam_recent_games").fire("input");
  submit(form);
  await tick();
  form = formFor(f, "steam");
  assert.match(areaFor(f, "steam").textContent, /rebased/);
  assert.match(areaFor(f, "steam").textContent, /Revision 3/);
  assert.equal(inputNamed(form, "steam_auto_sync").checked, true);
  assert.equal(inputNamed(form, "steam_recent_games").value, "5");
  assert.equal(f.state.steam.values.auto_sync, false);
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(f.content, "Refresh")?.disabled, true);
  buttonNamed(form, "Cancel").click();
  await tick();
  form = formFor(f, "steam");
  assert.equal(inputNamed(form, "steam_auto_sync").checked, false);
  assert.equal(inputNamed(form, "steam_recent_games").value, "20");
  assert.equal(f.draftGuard.isDirty(), false);
  f.draftGuard.dispose();
});

test("Lutris ordered editor retains unknown items, saves, and rebases a conflict draft", async () => {
  const f = fixture();
  await tick();
  let form = formFor(f, "lutris");
  assert.ok(form);
  const editor = find(form, node => node.tagName === "FIELDSET" && node.textContent.includes("Excluded Lutris games"));
  const entries = () => editor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded Lutris games, item"));
  assert.deepEqual(entries().map(node => node.value), ["77", '{"name":"Legacy name-only"}', '{"id":"999","name":"Unavailable game"}']);
  buttonNamed(editor, "Add item").click();
  let rows = entries();
  rows[3].value = "100";
  rows[3].fire("input");
  assert.equal(f.draftGuard.isDirty(), true);
  buttonNamed(form, "Cancel").click();
  await tick();
  form = formFor(f, "lutris");
  const canceledEditor = find(form, node => node.tagName === "FIELDSET" && node.textContent.includes("Excluded Lutris games"));
  assert.equal(canceledEditor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded Lutris games, item")).length, 3);
  assert.equal(f.draftGuard.isDirty(), false);

  const canceledEntries = canceledEditor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded Lutris games, item"));
  buttonNamed(canceledEditor, "Add item").click();
  rows = canceledEditor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded Lutris games, item"));
  rows[3].value = "100";
  rows[3].fire("input");
  inputNamed(form, "lutris_auto_sync").checked = false;
  inputNamed(form, "lutris_auto_sync").fire("change");
  submit(form);
  await tick();
  const update = f.calls.find(([route]) => route === "lutris/settings_update");
  assert.equal(update[1].expected_revision, 1);
  assert.deepEqual(update[1].exclude_games, ["77", { name: "Legacy name-only" }, { id: "999", name: "Unavailable game" }, "100"]);
  assert.equal(f.state.lutris.revision, 2);
  assert.equal(f.state.lutris.values.auto_sync, false);

  f.conflictNext.add("lutris");
  form = formFor(f, "lutris");
  inputNamed(form, "lutris_auto_sync").checked = true;
  inputNamed(form, "lutris_auto_sync").fire("change");
  submit(form);
  await tick();
  form = formFor(f, "lutris");
  assert.match(areaFor(f, "lutris").textContent, /rebased/);
  assert.match(areaFor(f, "lutris").textContent, /Revision 3/);
  assert.equal(inputNamed(form, "lutris_auto_sync").checked, true);
  assert.equal(f.state.lutris.values.auto_sync, false);
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(f.content, "Refresh")?.disabled, true);
  buttonNamed(form, "Cancel").click();
  await tick();
  assert.equal(inputNamed(formFor(f, "lutris"), "lutris_auto_sync").checked, false);
  assert.equal(f.draftGuard.isDirty(), false);
  f.draftGuard.dispose();
});

test("Playnite list editors save exact values and keep edits visible through conflicts", async () => {
  const f = fixture();
  await tick();
  let form = formFor(f, "playnite");
  assert.ok(form);
  const gamesEditor = find(form, node => node.tagName === "FIELDSET" && node.textContent.includes("Excluded game IDs"));
  const gameEntries = () => gamesEditor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded game IDs, item"));
  assert.deepEqual(gameEntries().map(node => node.value), ["retired-game-id"]);
  buttonNamed(gamesEditor, "Add item").click();
  let entries = gameEntries();
  entries[1].value = "new-game-id";
  entries[1].fire("input");
  assert.equal(f.draftGuard.isDirty(), true);
  buttonNamed(form, "Cancel").click();
  await tick();
  form = formFor(f, "playnite");
  assert.equal(f.draftGuard.isDirty(), false);
  let editor = find(form, node => node.tagName === "FIELDSET" && node.textContent.includes("Excluded game IDs"));
  let controls = editor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded game IDs, item"));
  assert.deepEqual(controls.map(node => node.value), ["retired-game-id"]);

  editor = find(form, node => node.tagName === "FIELDSET" && node.textContent.includes("Excluded game IDs"));
  controls = editor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded game IDs, item"));
  controls[0].value = "legacy-game-id";
  controls[0].fire("input");
  buttonNamed(editor, "Add item").click();
  controls = editor.querySelectorAll("input").filter(node => node.getAttribute("aria-label")?.startsWith("Excluded game IDs, item"));
  controls[1].value = "second-game-id";
  controls[1].fire("input");
  inputNamed(form, "playnite_auto_sync").checked = false;
  inputNamed(form, "playnite_auto_sync").fire("change");
  submit(form);
  await tick();
  const update = f.calls.find(([route]) => route === "playnite/settings_update");
  assert.equal(update[1].expected_revision, 1);
  assert.deepEqual(update[1].exclude_games, ["legacy-game-id", "second-game-id"]);
  assert.deepEqual(update[1].sync_categories, ["legacy-category"]);
  assert.deepEqual(update[1].exclude_plugins, ["plugin.disabled"]);
  assert.equal(f.state.playnite.revision, 2);

  f.conflictNext.add("playnite");
  form = formFor(f, "playnite");
  inputNamed(form, "playnite_auto_sync").checked = false;
  inputNamed(form, "playnite_auto_sync").fire("change");
  submit(form);
  await tick();
  form = formFor(f, "playnite");
  assert.match(areaFor(f, "playnite").textContent, /rebased/);
  assert.match(areaFor(f, "playnite").textContent, /Revision 3/);
  assert.equal(inputNamed(form, "playnite_auto_sync").checked, true);
  assert.equal(f.state.playnite.values.auto_sync, true);
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(f.content, "Refresh")?.disabled, true);
  buttonNamed(form, "Cancel").click();
  await tick();
  assert.equal(inputNamed(formFor(f, "playnite"), "playnite_auto_sync").checked, true);
  assert.equal(f.draftGuard.isDirty(), false);
  f.draftGuard.dispose();
});


test("Steam save locks every editor control until a delayed provider update and readback finish", async () => {
  const f = fixture();
  await tick();
  const form = formFor(f, "steam");
  const autoSync = inputNamed(form, "steam_auto_sync");
  autoSync.checked = false;
  autoSync.fire("change");
  let releaseUpdate;
  let startedUpdate;
  const started = new Promise(resolve => { startedUpdate = resolve; });
  const gate = new Promise(resolve => { releaseUpdate = resolve; });
  let releaseList;
  let startedList;
  const listStarted = new Promise(resolve => { startedList = resolve; });
  const listGate = new Promise(resolve => { releaseList = resolve; });
  let holdNextPage = true;
  const post = f.api.post.bind(f.api);
  f.api.post = async (route, body = {}) => {
    if (route === "steam/list" && body.after === 10 && holdNextPage) {
      holdNextPage = false;
      startedList();
      await listGate;
    }
    if (route === "steam/settings_update") {
      startedUpdate();
      await gate;
    }
    return post(route, body);
  };

  buttonNamed(form, "Load more games").click();
  await listStarted;
  submit(form);
  await started;
  assert.equal(autoSync.disabled, true);
  assert.equal(inputNamed(form, "steam_recent_games").disabled, true);
  assert.equal(buttonNamed(form, "Cancel").disabled, true);
  const picker = find(form, node => node.className === "steam-exclusion-picker");
  assert.equal(picker.querySelectorAll("input").filter(node => !node.hidden).every(node => node.disabled), true);
  releaseList();
  await tick();
  assert.equal(picker.querySelectorAll("input[type=checkbox]").every(node => node.disabled), true);
  releaseUpdate();
  await tick();
  assert.equal(f.state.steam.values.auto_sync, false);
  assert.equal(inputNamed(formFor(f, "steam"), "steam_auto_sync").checked, false);
  f.draftGuard.dispose();
});

test("failed revision-conflict readback keeps the draft through retry, then cancel restores latest values", async () => {
  const f = fixture();
  await tick();
  f.conflictNext.add("steam");
  let failSettingsRead = true;
  const post = f.api.post.bind(f.api);
  f.api.post = async (route, body = {}) => {
    if (route === "steam/settings" && failSettingsRead) {
      failSettingsRead = false;
      throw f.context.ApiError
        ? new f.context.ApiError("settings read failed", "forbidden", 403)
        : new f.context.Amaterasu.ApiError("settings read failed", "forbidden", 403);
    }
    return post(route, body);
  };
  let form = formFor(f, "steam");
  inputNamed(form, "steam_recent_games").value = "5";
  inputNamed(form, "steam_recent_games").fire("input");
  submit(form);
  await tick();
  const alert = find(areaFor(f, "steam"), node => node.getAttribute("role") === "alert");
  assert.ok(alert);
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(f.content, "Refresh")?.disabled, true);
  buttonNamed(alert, "Try again").click();
  await tick();
  form = formFor(f, "steam");
  assert.equal(inputNamed(form, "steam_recent_games").value, "5");
  assert.equal(f.state.steam.values.recent_games, 20);
  assert.equal(f.draftGuard.isDirty(), true);
  buttonNamed(form, "Cancel").click();
  await tick();
  assert.equal(inputNamed(formFor(f, "steam"), "steam_recent_games").value, "20");
  assert.equal(f.draftGuard.isDirty(), false);
  f.draftGuard.dispose();
});

test("Steam saved exclusions remain editable without steam.read and picker disposal follows navigation", async () => {
  const f = fixture();
  await tick();
  f.permissions.delete("steam.read");
  const form = formFor(f, "steam");
  const picker = find(form, node => node.className === "steam-exclusion-picker");
  const rows = picker.children.find(node => node.className === "steam-exclusion-saved").children;
  assert.equal(rows.length, 2);
  assert.equal(rows[0].children[0].disabled, false);
  rows[0].children[1].value = "Renamed legacy entry";
  rows[0].children[1].fire("input");
  rows[1].children[0].value = "998";
  rows[1].children[0].fire("input");
  assert.deepEqual(JSON.parse(inputNamed(form, "steam_exclusion_draft").value), [
    { name: "Renamed legacy entry" }, { id: "998", name: "Unavailable legacy" },
  ]);
  rows[0].children[2].click();
  assert.equal(f.draftGuard.isDirty(), true);
  assert.deepEqual(f.calls.filter(([route]) => route === "steam/list").length, 1);
  assert.deepEqual(ordinary(f.state.steam.values.exclude_games), [
    { name: "Name-only legacy" }, { id: "999", name: "Unavailable legacy" },
  ]);
  f.draftGuard.dispose();
  assert.equal(picker.children.length, 0);
});

test("successful provider save with failed readback reload preserves submitted values for review", async () => {
  const f = fixture();
  await tick();
  let settingsReadsToFail = 2;
  const post = f.api.post.bind(f.api);
  f.api.post = async (route, body = {}) => {
    if (route === "lutris/settings" && settingsReadsToFail > 0) {
      settingsReadsToFail--;
      throw new f.context.Amaterasu.ApiError("readback unavailable", "forbidden", 403);
    }
    return post(route, body);
  };
  let form = formFor(f, "lutris");
  inputNamed(form, "lutris_auto_sync").checked = false;
  inputNamed(form, "lutris_auto_sync").fire("change");
  submit(form);
  await tick();
  assert.equal(f.state.lutris.revision, 2);
  const alert = find(areaFor(f, "lutris"), node => node.getAttribute("role") === "alert");
  assert.ok(alert);
  buttonNamed(alert, "Try again").click();
  await tick();
  form = formFor(f, "lutris");
  assert.equal(inputNamed(form, "lutris_auto_sync").checked, false);
  assert.match(areaFor(f, "lutris").textContent, /Overlapping fields: auto_sync/);
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(form, "Save Lutris settings").disabled, true);
  buttonNamed(form, "Review and keep latest values").click();
  assert.equal(buttonNamed(form, "Save Lutris settings").disabled, false);
  f.draftGuard.dispose();
});

test("provider editors are disposed when settings navigation leaves the page", async () => {
  const f = fixture();
  await tick();
  const steamPicker = find(formFor(f, "steam"), node => node.className === "steam-exclusion-picker");
  const listEditors = ["lutris", "playnite"].flatMap(provider => {
    const form = formFor(f, provider);
    return form.querySelectorAll("fieldset").filter(node => node.className === "list-editor");
  });
  assert.ok(steamPicker.children.length > 0);
  assert.ok(listEditors.length > 0);
  assert.ok(listEditors.every(editor => editor.children.length > 0));
  f.draftGuard.dispose();
  assert.equal(steamPicker.children.length, 0);
  assert.ok(listEditors.every(editor => editor.children.length === 0));
});


test("Steam conflict rebases unrelated latest fields and requires an explicit choice for overlap", async () => {
  const f = fixture();
  await tick();
  f.conflictNext.add("steam");
  f.conflictOverrides.set("steam", { recent_games: 10 });
  let form = formFor(f, "steam");
  inputNamed(form, "steam_recent_games").value = "5";
  inputNamed(form, "steam_recent_games").fire("input");
  inputNamed(form, "steam_auto_sync").checked = false;
  inputNamed(form, "steam_auto_sync").fire("change");
  submit(form);
  await tick();

  form = formFor(f, "steam");
  assert.equal(inputNamed(form, "steam_auto_sync").checked, false);
  assert.equal(inputNamed(form, "steam_recent_games").value, "10");
  assert.match(areaFor(f, "steam").textContent, /Overlapping fields: auto_sync, recent_games/);
  const save = buttonNamed(form, "Save Steam settings");
  assert.equal(save.disabled, true);
  assert.equal(f.draftGuard.isDirty(), true);
  assert.equal(buttonNamed(f.content, "Refresh")?.disabled, true);
  inputNamed(form, "steam_include_tools").checked = true;
  inputNamed(form, "steam_include_tools").fire("change");

  buttonNamed(form, "Overwrite overlapping fields").click();
  assert.equal(inputNamed(form, "steam_recent_games").value, "5");
  assert.equal(inputNamed(form, "steam_auto_sync").checked, false);
  assert.equal(inputNamed(form, "steam_include_tools").checked, true);
  assert.equal(save.disabled, false);
  submit(form);
  await tick();
  assert.equal(f.state.steam.values.recent_games, 5);
  assert.equal(f.state.steam.values.auto_sync, false);
  assert.equal(f.state.steam.values.include_tools, true);
  assert.equal(f.draftGuard.isDirty(), false);
  f.draftGuard.dispose();
});


test("Steam keep-latest review preserves edits made while the conflict decision is visible", async () => {
  const f = fixture();
  await tick();
  f.conflictNext.add("steam");
  f.conflictOverrides.set("steam", { recent_games: 10 });
  let form = formFor(f, "steam");
  inputNamed(form, "steam_recent_games").value = "5";
  inputNamed(form, "steam_recent_games").fire("input");
  inputNamed(form, "steam_auto_sync").checked = false;
  inputNamed(form, "steam_auto_sync").fire("change");
  submit(form);
  await tick();

  form = formFor(f, "steam");
  inputNamed(form, "steam_auto_sync").checked = true;
  inputNamed(form, "steam_auto_sync").fire("change");
  inputNamed(form, "steam_include_tools").checked = true;
  inputNamed(form, "steam_include_tools").fire("change");
  buttonNamed(form, "Review and keep latest values").click();
  assert.equal(inputNamed(form, "steam_recent_games").value, "10");
  assert.equal(inputNamed(form, "steam_auto_sync").checked, false);
  assert.equal(inputNamed(form, "steam_include_tools").checked, true);
  submit(form);
  await tick();
  assert.equal(f.state.steam.values.recent_games, 10);
  assert.equal(f.state.steam.values.auto_sync, false);
  assert.equal(f.state.steam.values.include_tools, true);
  assert.equal(f.draftGuard.isDirty(), false);
  f.draftGuard.dispose();
});


test("Lutris conflict choices restore only overlapping fields and preserve later edits", async () => {
  const overwrite = fixture();
  await tick();
  overwrite.conflictNext.add("lutris");
  let form = formFor(overwrite, "lutris");
  inputNamed(form, "lutris_auto_sync").checked = false;
  inputNamed(form, "lutris_auto_sync").fire("change");
  submit(form);
  await tick();
  form = formFor(overwrite, "lutris");
  inputNamed(form, "lutris_include_steam").checked = true;
  inputNamed(form, "lutris_include_steam").fire("change");
  buttonNamed(form, "Overwrite overlapping fields").click();
  assert.equal(inputNamed(form, "lutris_auto_sync").checked, false);
  assert.equal(inputNamed(form, "lutris_include_steam").checked, true);
  submit(form);
  await tick();
  assert.equal(overwrite.state.lutris.values.auto_sync, false);
  assert.equal(overwrite.state.lutris.values.include_steam, true);
  overwrite.draftGuard.dispose();

  const keepLatest = fixture();
  await tick();
  keepLatest.conflictNext.add("lutris");
  form = formFor(keepLatest, "lutris");
  inputNamed(form, "lutris_auto_sync").checked = false;
  inputNamed(form, "lutris_auto_sync").fire("change");
  submit(form);
  await tick();
  form = formFor(keepLatest, "lutris");
  inputNamed(form, "lutris_auto_sync").checked = true;
  inputNamed(form, "lutris_auto_sync").fire("change");
  inputNamed(form, "lutris_include_steam").checked = true;
  inputNamed(form, "lutris_include_steam").fire("change");
  buttonNamed(form, "Review and keep latest values").click();
  assert.equal(inputNamed(form, "lutris_auto_sync").checked, false);
  assert.equal(inputNamed(form, "lutris_include_steam").checked, true);
  submit(form);
  await tick();
  assert.equal(keepLatest.state.lutris.values.auto_sync, false);
  assert.equal(keepLatest.state.lutris.values.include_steam, true);
  keepLatest.draftGuard.dispose();
});

test("Playnite conflict choices restore only overlapping fields and preserve later edits", async () => {
  const keepLatest = fixture();
  await tick();
  keepLatest.conflictNext.add("playnite");
  keepLatest.conflictOverrides.set("playnite", { recent_games: 60 });
  let form = formFor(keepLatest, "playnite");
  inputNamed(form, "playnite_recent_games").value = "45";
  inputNamed(form, "playnite_recent_games").fire("input");
  submit(form);
  await tick();
  form = formFor(keepLatest, "playnite");
  inputNamed(form, "playnite_recent_games").value = "50";
  inputNamed(form, "playnite_recent_games").fire("input");
  inputNamed(form, "playnite_focus_attempts").value = "8";
  inputNamed(form, "playnite_focus_attempts").fire("input");
  buttonNamed(form, "Review and keep latest values").click();
  assert.equal(inputNamed(form, "playnite_recent_games").value, "60");
  assert.equal(inputNamed(form, "playnite_focus_attempts").value, "8");
  submit(form);
  await tick();
  assert.equal(keepLatest.state.playnite.values.recent_games, 60);
  assert.equal(keepLatest.state.playnite.values.focus_attempts, 8);
  keepLatest.draftGuard.dispose();

  const overwrite = fixture();
  await tick();
  overwrite.conflictNext.add("playnite");
  overwrite.conflictOverrides.set("playnite", { recent_games: 60 });
  form = formFor(overwrite, "playnite");
  inputNamed(form, "playnite_recent_games").value = "45";
  inputNamed(form, "playnite_recent_games").fire("input");
  submit(form);
  await tick();
  form = formFor(overwrite, "playnite");
  inputNamed(form, "playnite_recent_games").value = "50";
  inputNamed(form, "playnite_recent_games").fire("input");
  inputNamed(form, "playnite_focus_attempts").value = "8";
  inputNamed(form, "playnite_focus_attempts").fire("input");
  buttonNamed(form, "Overwrite overlapping fields").click();
  assert.equal(inputNamed(form, "playnite_recent_games").value, "45");
  assert.equal(inputNamed(form, "playnite_focus_attempts").value, "8");
  submit(form);
  await tick();
  assert.equal(overwrite.state.playnite.values.recent_games, 45);
  assert.equal(overwrite.state.playnite.values.focus_attempts, 8);
  overwrite.draftGuard.dispose();
});
