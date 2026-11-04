import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tagName, document = null) {
    this.tagName = tagName.toLowerCase();
    this.document = document;
    this.children = [];
    this.listeners = new Map();
    this.attributes = new Map();
    this.value = "";
    this.dataset = {};
    this.hidden = false;
    this.disabled = false;
    this.isConnected = false;
  }
  setConnected(connected) { this.isConnected = connected; for (const child of this.children) child?.setConnected?.(connected); }
  append(...children) { for (const child of children) { if (child == null) continue; if (typeof child === "object") child.parentElement = this; this.children.push(child); child?.setConnected?.(this.isConnected); } }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  addEventListener(name, callback) { const listeners = this.listeners.get(name) ?? []; listeners.push(callback); this.listeners.set(name, listeners); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  showModal() { this.open = true; }
  close() { this.open = false; }
  focus() {}
  scrollIntoView() {}
  reportValidity() { return true; }
  remove() { if (this.parentElement) this.parentElement.children = this.parentElement.children.filter(child => child !== this); this.parentElement = null; this.setConnected(false); }
  get classList() { return { contains: name => (this.className ?? "").split(" ").includes(name) }; }
  querySelectorAll(selector) {
    const matches = node => selector.split(",").some(part => {
      part = part.trim();
      if (part.startsWith(".")) return node.classList.contains(part.slice(1));
      return node.tagName === part;
    });
    const found = [];
    const walk = node => { for (const child of node.children ?? []) if (typeof child !== "string") { if (matches(child)) found.push(child); walk(child); } };
    walk(this);
    return found;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] ?? null; }
  dispatchEvent(event) { this.fire(event.type); return true; }
  fire(name, event = {}) { for (const listener of this.listeners.get(name) ?? []) listener({ preventDefault() {}, ...event }); }
  get textContent() { return this.children.map(child => typeof child === "string" ? child : child.textContent).join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
}
class SelectElement extends Element {}
class EditorFormData extends FormData {
  constructor(form) {
    super();
    const collect = node => {
      if (node.name && !node.disabled && ["input", "select", "textarea"].includes(node.tagName) &&
          (node.type !== "checkbox" || node.checked)) this.set(node.name, node.value);
      for (const child of node.children ?? []) if (typeof child !== "string") collect(child);
    };
    if (form) collect(form);
  }
}
const ordinary = value => JSON.parse(JSON.stringify(value));
const settle = () => new Promise(resolve => setImmediate(resolve));
function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children ?? []) if (typeof child !== "string") {
    const found = find(child, predicate);
    if (found) return found;
  }
  return null;
}
function setup(canReadSteam, saveBehavior = async () => ({ ok: true, revision: 12 }), steamDirectory = "/games/chosen") {
  const document = { body: null, createElement(tag) { return tag === "select" ? new SelectElement(tag, this) : new Element(tag, this); } };
  document.body = new Element("body", document);
  const context = vm.createContext({
    FormData: EditorFormData, TextEncoder, URL, Error, Event, Set, Map,
    HTMLElement: Element, HTMLSelectElement: SelectElement, document,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts", "src/steam_game_chooser.ts", "src/catalog.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const content = new Element("main");
  content.setConnected(true);
  let catalogRevision = 11;
  let entry = {
    name: "Original name", description: "Original description", tags: ["original"],
    launch: { kind: "executable", path: "/opt/original", arguments: ["--original"] },
    steam_id: "77", steam_install_dir: "/manual/path",
  };
  const calls = [];
  const api = {
    allows(scope) { return scope === "catalog.write" || (scope === "steam.read" && canReadSteam); },
    async post(route, payload) {
      calls.push({ route, payload: ordinary(payload) });
      if (route === "catalog/list") return {
        ok: true, revision: catalogRevision, next_cursor: null,
        config_override_capabilities: { frame_limiter_provider: ["auto"], rtss_frame_limit_type: [] },
        entries: [{ id: "app-1", revision: catalogRevision, entry }],
      };
      if (route === "input/status") throw new Error("runtime status unavailable");
      if (route === "steam/list") return {
        revision: 3,
        games: [{ app_id: 42, name: "Chosen Game", ...(steamDirectory === null ? {} : { install_directory: steamDirectory }) }],
        next_after: null, scanning: false, complete: true,
      };
      if (route === "catalog/update") return saveBehavior(payload);
      throw new Error(`Unexpected route ${route}`);
    },
  };
  context.Amaterasu.renderCatalog({ content, notify() {}, navigate() {}, api });
  return { content, document, calls, get entry() { return entry; }, set entry(value) { entry = value; }, get catalogRevision() { return catalogRevision; } };
}
async function openEditor(page) {
  await settle();
  find(page.content, node => node.tagName === "button" && node.textContent === "Edit").fire("click");
  await settle();
  return find(page.content, node => node.className === "editor-form");
}
function named(form, name) { return find(form, node => node.name === name); }
function chooseButton(form) { return find(form, node => node.tagName === "button" && node.textContent === "Choose a Steam game"); }
function save(form) { form.fire("submit"); }

test("application editor applies only an explicitly selected Steam record and saves other drafts at the catalog revision", async () => {
  const page = setup(true);
  const form = await openEditor(page);
  named(form, "name").value = "Unsaved title";
  named(form, "description").value = "Draft description";
  named(form, "tags").value = "alpha, beta";
  named(form, "path").value = "/opt/draft";
  named(form, "arguments").value = '["--draft"]';
  chooseButton(form).fire("click");
  await settle();
  const dialog = page.document.body.children[0];
  assert.equal(dialog.open, true);
  assert.match(dialog.textContent, /Chosen Game/);
  find(dialog, node => node.tagName === "button" && node.textContent === "Select").fire("click");
  await settle();
  assert.equal(named(form, "steam_id").value, "42");
  assert.equal(named(form, "steam_install_dir").value, "/games/chosen");
  assert.equal(named(form, "name").value, "Unsaved title");
  assert.equal(named(form, "description").value, "Draft description");
  assert.equal(named(form, "tags").value, "alpha, beta");
  assert.equal(named(form, "path").value, "/opt/draft");
  assert.equal(named(form, "arguments").value, '["--draft"]');
  save(form);
  await settle();
  const update = page.calls.find(call => call.route === "catalog/update");
  assert.equal(update.payload.expected_revision, 11);
  assert.equal(update.payload.entry.steam_id, "42");
  assert.equal(update.payload.entry.steam_install_dir, "/games/chosen");
  assert.equal(update.payload.entry.name, "Unsaved title");
  assert.equal(update.payload.entry.description, "Draft description");
  assert.deepEqual(update.payload.entry.tags, ["alpha", "beta"]);
  assert.deepEqual(update.payload.entry.launch, { kind: "executable", program: "/opt/draft", arguments: ["--draft"], working_directory: null, auto_detach: true, wait_all: true, exit_timeout_seconds: 5 });
});

test("cancel preserves manual Steam fields and unrelated editor drafts", async () => {
  const page = setup(true);
  const form = await openEditor(page);
  named(form, "name").value = "Manual draft";
  named(form, "steam_id").value = "123";
  named(form, "steam_install_dir").value = "/typed/path";
  chooseButton(form).fire("click");
  await settle();
  const dialog = page.document.body.children[0];
  assert.match(dialog.textContent, /Chosen Game/);
  assert.equal(page.calls.filter(call => call.route === "steam/list").length, 1);
  find(dialog, node => node.tagName === "button" && node.textContent === "Cancel").fire("click");
  await settle();
  assert.equal(named(form, "steam_id").value, "123");
  assert.equal(named(form, "steam_install_dir").value, "/typed/path");
  assert.equal(named(form, "name").value, "Manual draft");
  save(form);
  await settle();
  const update = page.calls.find(call => call.route === "catalog/update");
  assert.equal(update.payload.entry.steam_id, "123");
  assert.equal(update.payload.entry.steam_install_dir, "/typed/path");
  assert.equal(update.payload.entry.name, "Manual draft");
});

test("catalog revision conflict leaves selected Steam values and all other unsaved edits in the open editor", async () => {
  const page = setup(true, async () => { const error = new Error("Catalog changed elsewhere."); error.code = "revision_conflict"; throw error; });
  const form = await openEditor(page);
  named(form, "name").value = "Keep after conflict";
  named(form, "steam_install_dir").value = "/manual/path";
  chooseButton(form).fire("click");
  await settle();
  const dialog = page.document.body.children[0];
  find(dialog, node => node.tagName === "button" && node.textContent === "Select").fire("click");
  await settle();
  assert.equal(named(form, "steam_id").value, "42");
  assert.equal(named(form, "steam_install_dir").value, "/games/chosen");
  save(form);
  await settle();
  const update = page.calls.find(call => call.route === "catalog/update");
  assert.equal(update.payload.expected_revision, 11);
  assert.equal(update.payload.entry.steam_id, "42");
  assert.equal(update.payload.entry.name, "Keep after conflict");
  assert.equal(find(page.content, node => node.className === "editor-form"), form);
  assert.equal(named(form, "steam_id").value, "42");
  assert.equal(named(form, "steam_install_dir").value, "/games/chosen");
  assert.equal(named(form, "name").value, "Keep after conflict");
});

test("selection without an install directory preserves the editor's manually entered directory", async () => {
  const page = setup(true, async () => ({ ok: true, revision: 12 }), null);
  const form = await openEditor(page);
  named(form, "steam_install_dir").value = "/manual/path";
  chooseButton(form).fire("click");
  await settle();
  const dialog = page.document.body.children[0];
  find(dialog, node => node.tagName === "button" && node.textContent === "Select").fire("click");
  await settle();
  assert.equal(named(form, "steam_id").value, "42");
  assert.equal(named(form, "steam_install_dir").value, "/manual/path");
  assert.match(form.textContent, /existing directory was kept/);
  save(form);
  await settle();
  const update = page.calls.find(call => call.route === "catalog/update");
  assert.equal(update.payload.entry.steam_id, "42");
  assert.equal(update.payload.entry.steam_install_dir, "/manual/path");
});

test("application editor reports missing steam.read without issuing discovery requests", async () => {
  const page = setup(false);
  const form = await openEditor(page);
  named(form, "steam_id").value = "123";
  chooseButton(form).fire("click");
  await settle();
  const dialog = page.document.body.children[0];
  assert.match(dialog.textContent, /steam.read permission/);
  assert.equal(page.calls.filter(call => call.route === "steam/list").length, 0);
  find(dialog, node => node.tagName === "button" && node.textContent === "Cancel").fire("click");
  await settle();
  assert.equal(named(form, "steam_id").value, "123");
});
