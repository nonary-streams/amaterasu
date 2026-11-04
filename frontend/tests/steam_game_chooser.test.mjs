import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class FakeText {
  constructor(text) { this.textContent = String(text); this.nodeType = 3; }
}
class FakeElement {
  children = [];
  attributes = new Map();
  listeners = new Map();
  text = "";
  hidden = false;
  disabled = false;
  open = false;
  isConnected = false;
  parentElement = null;
  constructor(tagName, document) { this.tagName = tagName.toUpperCase(); this.document = document; }
  setConnected(connected) { this.isConnected = connected; for (const child of this.children) child?.setConnected?.(connected); }
  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      if (typeof child === "object") child.parentElement = this;
      this.children.push(child);
      child?.setConnected?.(this.isConnected);
    }
  }
  replaceChildren(...children) {
    for (const child of this.children) child?.setConnected?.(false);
    this.children = [];
    this.append(...children);
  }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  addEventListener(type, callback) { const events = this.listeners.get(type) ?? []; events.push(callback); this.listeners.set(type, events); }
  dispatch(type, values = {}) {
    const event = { type, target: this, preventDefault() { this.defaultPrevented = true; }, ...values };
    for (const entry of this.listeners.get(type) ?? []) entry(event);
    return event;
  }
  click() { this.dispatch("click"); }
  focus() { this.document.activeElement = this; }
  showModal() { this.open = true; }
  close() { this.open = false; this.dispatch("close"); }
  remove() {
    if (this.parentElement) this.parentElement.children = this.parentElement.children.filter(child => child !== this);
    this.parentElement = null;
    this.removed = true;
    this.setConnected(false);
  }
  get textContent() { return this.text + this.children.map((child) => child?.textContent ?? String(child)).join(""); }
  set textContent(value) { this.text = String(value); this.replaceChildren(); }
}
class ApiError extends Error { constructor(message, code, status) { super(message); this.code = code; this.status = status; } }

function setup(api, options = {}) {
  const document = { activeElement: null, body: null, documentElement: null,
    createElement(tag) { return new FakeElement(tag, this); },
    createTextNode(text) { return new FakeText(text); },
    querySelectorAll(selector) {
      const name = selector.slice(1, -1).replace("data-locale-", "");
      return all(this.documentElement).filter((node) => node instanceof FakeElement && node.attributes.has(`data-locale-${name}`));
    },
    createTreeWalker(root) {
      const nodes = [];
      const walk = (node) => {
        if (node?.nodeType === 3) nodes.push(node);
        else for (const child of node?.children ?? []) walk(child);
      };
      walk(root);
      let index = 0;
      return { nextNode() { return nodes[index++] ?? null; } };
    },
  };
  document.documentElement = new FakeElement("html", document);
  document.body = new FakeElement("body", document);
  document.documentElement.append(document.body);
  document.documentElement.setConnected(true);
  const context = vm.createContext({
    document, ApiError, AbortController, Error, Number, String, Object, Array, Set,
    NodeFilter: { SHOW_TEXT: 4 },
  });
  const sources = options.liveLocale
    ? ["src/locale-catalogs.ts", "src/ui.ts", "src/steam_game_chooser.ts", "src/locale.ts"]
    : ["src/ui.ts", "src/steam_game_chooser.ts"];
  const source = sources.map((file) => readFileSync(file, "utf8")).join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const opener = new FakeElement("button", document);
  document.body.append(opener);
  opener.focus();
  const pending = context.Amaterasu.chooseSteamGame(api, options.signal);
  const dialog = document.body.children.find((node) => node instanceof FakeElement && node.tagName === "DIALOG");
  return { pending, dialog, document, opener, context };
}
function all(node) { return [node, ...(node.children ?? []).filter((child) => child instanceof FakeElement).flatMap(all)]; }
function button(dialog, label) { return all(dialog).find((node) => node.tagName === "BUTTON" && node.textContent === label); }
async function settle() { await new Promise((resolve) => setImmediate(resolve)); await new Promise((resolve) => setImmediate(resolve)); }
function page(games, next_after = null, revision = 7) {
  return { revision, games, next_after, scanning: false, complete: true };
}

test("Steam chooser paginates with a stable revision and returns only the explicit game selection", async () => {
  const calls = [];
  const api = { allows: (scope) => scope === "steam.read", async post(route, payload) {
    calls.push({ route, payload });
    return calls.length === 1
      ? page([{ app_id: 10, name: "First", install_directory: "/games/first" }], 10)
      : page([{ app_id: 20, name: "Second" }]);
  } };
  const view = setup(api);
  await settle();
  assert.equal(view.dialog.textContent.includes("First"), true);
  button(view.dialog, "Next →").click();
  await settle();
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [
    { route: "steam/list", payload: { limit: 16 } },
    { route: "steam/list", payload: { revision: 7, after: 10, limit: 16 } },
  ]);
  const select = all(view.dialog).find((node) => node.tagName === "BUTTON" && node.getAttribute("aria-label") === "Select Second");
  assert.ok(select);
  select.click();
  assert.deepEqual(JSON.parse(JSON.stringify(await view.pending)), { app_id: 20, name: "Second" });
  assert.equal(view.dialog.removed, true);
});

test("permission denial explains manual entry and never calls Steam discovery", async () => {
  let calls = 0;
  const view = setup({ allows: () => false, async post() { calls++; throw new Error("must not request"); } });
  assert.match(view.dialog.textContent, /steam.read permission/);
  assert.match(view.dialog.textContent, /enter a Steam app ID manually/);
  assert.equal(calls, 0);
  button(view.dialog, "Cancel").click();
  assert.equal(await view.pending, null);
});

test("unavailable Steam discovery has a recoverable status and Cancel preserves the whole editor draft", async () => {
  const editorDraft = { steam_id: "00123", steam_install_dir: "/before", title: "Unsaved title", arguments: ["--test"] };
  const original = structuredClone(editorDraft);
  const view = setup({ allows: () => true, async post() { throw new ApiError("provider offline", "steam_unavailable", 503); } });
  await settle();
  assert.match(view.dialog.textContent, /Steam games are unavailable/);
  view.dialog.dispatch("cancel");
  assert.equal(await view.pending, null);
  assert.deepEqual(editorDraft, original);
});

test("explicit selection returns optional directory without applying it to caller-owned drafts", async () => {
  const editorDraft = { steam_id: "", steam_install_dir: "", title: "Unsaved title", arguments: ["--test"] };
  const view = setup({ allows: () => true, async post() {
    return page([{ app_id: 42, name: "Chosen", install_directory: "/chosen" }]);
  } });
  await settle();
  button(view.dialog, "Select").click();
  const selection = await view.pending;
  assert.deepEqual(JSON.parse(JSON.stringify(selection)), { app_id: 42, name: "Chosen", install_directory: "/chosen" });
  assert.deepEqual(editorDraft, { steam_id: "", steam_install_dir: "", title: "Unsaved title", arguments: ["--test"] });
});

test("cancel restores opener focus", async () => {
  const view = setup({ allows: () => true, async post() { return page([]); } });
  await settle();
  assert.equal(view.document.activeElement, button(view.dialog, "Cancel"));
  button(view.dialog, "Cancel").click();
  assert.equal(await view.pending, null);
  assert.equal(view.document.activeElement, view.opener);
});

test("real empty-string install_directory is treated as unavailable", async () => {
  const view = setup({ allows: () => true, async post() {
    return page([{ app_id: 9, name: "Pathless", install_directory: "" }]);
  } });
  await settle();
  button(view.dialog, "Select").click();
  assert.deepEqual(JSON.parse(JSON.stringify(await view.pending)), { app_id: 9, name: "Pathless" });
});

test("search walks bounded pages of the actual steam/list API and filters by name or app ID", async () => {
  const calls = [];
  const firstGames = Array.from({ length: 16 }, (_, i) => ({ app_id: i + 1, name: `Other ${i + 1}` }));
  const api = { allows: () => true, async post(route, payload) {
    calls.push({ route, payload: { ...payload } });
    if (calls.length === 1 || calls.length === 2) return page(firstGames, 16);
    return page([{ app_id: 42, name: "Target game", install_directory: "/target" }]);
  } };
  const view = setup(api);
  await settle();
  const search = all(view.dialog).find((node) => node.name === "steam_game_search");
  search.value = "target";
  button(view.dialog, "Search").click();
  await settle();
  assert.equal(calls.length, 3);
  assert.ok(calls.every((call) => call.route === "steam/list"));
  assert.deepEqual(JSON.parse(JSON.stringify(calls[1].payload)), { revision: 7, limit: 16 });
  assert.deepEqual(JSON.parse(JSON.stringify(calls[2].payload)), { revision: 7, after: 16, limit: 16 });
  assert.equal(all(view.dialog).filter((node) => node.tagName === "BUTTON" && node.textContent === "Select").length, 1);
  assert.match(view.dialog.textContent, /Target game/);
  button(view.dialog, "Select").click();
  assert.deepEqual(JSON.parse(JSON.stringify(await view.pending)), { app_id: 42, name: "Target game", install_directory: "/target" });
});

test("search stops at its page bound instead of scanning without limit", async () => {
  let calls = 0;
  const api = { allows: () => true, async post(_route, payload) {
    calls++;
    const pageNumber = Math.floor(calls === 1 ? 0 : (calls - 2) % 100) + 1;
    return page([{ app_id: pageNumber, name: "No match" }], pageNumber * 16);
  } };
  const view = setup(api);
  await settle();
  const search = all(view.dialog).find((node) => node.name === "steam_game_search");
  search.value = "absent";
  button(view.dialog, "Search").click();
  await settle();
  assert.equal(calls, 33, "one browse request plus exactly 32 bounded search pages");
  assert.match(view.dialog.textContent, /Search reached its page limit/);
});

test("stale pagination offers a clean restart using a new Steam revision", async () => {
  const calls = [];
  const api = { allows: () => true, async post(route, payload) {
    calls.push({ route, payload: { ...payload } });
    if (calls.length === 1) return page([{ app_id: 1, name: "Old page" }], 1, 7);
    if (calls.length === 2) return page([{ app_id: 2, name: "Stale page" }], null, 8);
    return page([{ app_id: 3, name: "Fresh page" }], null, 9);
  } };
  const view = setup(api);
  await settle();
  button(view.dialog, "Next →").click();
  await settle();
  assert.match(view.dialog.textContent, /changed while browsing/);
  button(view.dialog, "Restart").click();
  await settle();
  assert.equal(view.dialog.textContent.includes("Fresh page"), true);
  assert.deepEqual(JSON.parse(JSON.stringify(calls[2].payload)), { limit: 16 });
});

test("permission loss during a pending request suppresses its late response", async () => {
  let allowed = true;
  let resolvePage;
  const pendingPage = new Promise(resolve => { resolvePage = resolve; });
  const view = setup({ allows: scope => scope === "steam.read" && allowed, post: () => pendingPage });
  await settle();
  allowed = false;
  resolvePage(page([{ app_id: 42, name: "Late secret" }]));
  await settle();
  assert.match(view.dialog.textContent, /steam.read permission/);
  assert.doesNotMatch(view.dialog.textContent, /Late secret/);
  button(view.dialog, "Cancel").click();
  assert.equal(await view.pending, null);
});

test("editor signal abort closes the modal and ignores a delayed API response", async () => {
  let resolvePage;
  const pendingPage = new Promise(resolve => { resolvePage = resolve; });
  const lifecycle = new AbortController();
  let receivedSignal;
  const view = setup({
    allows: () => true,
    post(_route, _payload, _protectedRoute, signal) { receivedSignal = signal; return pendingPage; },
  }, { signal: lifecycle.signal });
  await settle();
  assert.equal(receivedSignal, lifecycle.signal);
  lifecycle.abort();
  assert.equal(await view.pending, null);
  assert.equal(view.dialog.removed, true);
  resolvePage(page([{ app_id: 42, name: "Late response" }]));
  await settle();
  assert.doesNotMatch(view.dialog.textContent, /Late response/);
});

test("localized dialog labels refresh in place and restore focus to the opener", async () => {
  const view = setup({ allows: () => true, async post() { return page([]); } }, { liveLocale: true });
  await settle();
  const cancel = button(view.dialog, "Cancel");
  assert.equal(cancel.getAttribute("aria-label"), "Cancel");
  view.context.Amaterasu.applyLocale("de");
  assert.equal(button(view.dialog, "Abbrechen"), cancel);
  assert.equal(cancel.getAttribute("aria-label"), "Abbrechen");
  view.context.Amaterasu.applyLocale("en");
  assert.equal(button(view.dialog, "Cancel"), cancel);
  cancel.click();
  assert.equal(await view.pending, null);
  assert.equal(view.document.activeElement, view.opener);
});
