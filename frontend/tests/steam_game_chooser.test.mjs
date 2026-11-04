import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class FakeElement {
  children = [];
  attributes = new Map();
  listeners = new Map();
  text = "";
  hidden = false;
  disabled = false;
  open = false;
  constructor(tagName, document) { this.tagName = tagName.toUpperCase(); this.document = document; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  addEventListener(type, callback) { const events = this.listeners.get(type) ?? []; events.push(callback); this.listeners.set(type, events); }
  dispatch(type, values = {}) {
    const event = { type, target: this, preventDefault() { this.defaultPrevented = true; }, ...values };
    for (const callback of this.listeners.get(type) ?? []) callback(event);
    return event;
  }
  click() { this.dispatch("click"); }
  focus() { this.document.activeElement = this; }
  showModal() { this.open = true; }
  close() { this.open = false; this.dispatch("close"); }
  remove() { this.removed = true; }
  get textContent() { return this.text + this.children.map((child) => child?.textContent ?? String(child)).join(""); }
  set textContent(value) { this.text = String(value); this.children = []; }
}
class ApiError extends Error { constructor(message, code, status) { super(message); this.code = code; this.status = status; } }

function setup(api) {
  const document = { activeElement: null, body: null, createElement(tag) { return new FakeElement(tag, this); } };
  document.body = new FakeElement("body", document);
  const context = vm.createContext({ document, ApiError, AbortController, Error, Number, String, Object, Array, Set });
  const source = ["src/ui.ts", "src/steam_game_chooser.ts"]
    .map((file) => readFileSync(file, "utf8")).join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const pending = context.Amaterasu.chooseSteamGame(api);
  const dialog = document.body.children[0];
  return { pending, dialog, document };
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

test("browser cancel button resolves null", async () => {
  const view = setup({ allows: () => true, async post() { return page([]); } });
  await settle();
  button(view.dialog, "Cancel").click();
  assert.equal(await view.pending, null);
});
