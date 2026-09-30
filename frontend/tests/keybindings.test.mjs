import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tagName) {
    this.tagName = tagName;
    this.children = [];
    this.listeners = new Map();
    this.value = "";
    this.isConnected = false;
  }
  setConnected(connected) {
    this.isConnected = connected;
    for (const child of this.children) child?.setConnected?.(connected);
  }
  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      this.children.push(child);
      child?.setConnected?.(this.isConnected);
    }
  }
  replaceChildren(...children) {
    for (const child of this.children) child?.setConnected?.(false);
    this.children = [];
    this.append(...children);
  }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute() {}
  removeAttribute() {}
  scrollIntoView() {}
  focus() {}
  reportValidity() { return true; }
  querySelector(selector) { return find(this, (node) => node.tagName === selector); }
  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
  }
  set textContent(value) { this.replaceChildren(value); }
  fire(name) { this.listeners.get(name)?.({ preventDefault() {} }); }
}

class SelectElement extends Element {}

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children ?? []) {
    if (typeof child === "string") continue;
    const found = find(child, predicate);
    if (found) return found;
  }
  return null;
}

class EditorFormData extends FormData {
  constructor(form) {
    super();
    const collect = (node) => {
      if (node.name && !node.disabled && ["input", "select", "textarea"].includes(node.tagName) &&
          (node.type !== "checkbox" || node.checked))
        this.set(node.name, node.value);
      for (const child of node.children ?? []) if (typeof child !== "string") collect(child);
    };
    if (form) collect(form);
  }
}

const ordinary = (value) => JSON.parse(JSON.stringify(value));
const settle = () => new Promise(setImmediate);

test("application DOM saves ordered keybindings, empty replacement, then inheritance using catalog revisions", async () => {
  const context = vm.createContext({
    FormData: EditorFormData, TextEncoder, URL, Error,
    HTMLSelectElement: SelectElement,
    document: { createElement: (tagName) => tagName === "select" ? new SelectElement(tagName) : new Element(tagName) },
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts", "src/catalog.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const content = new Element("main");
  content.setConnected(true);
  let revision = 7;
  let entry = {
    name: "Mapped application", description: "", tags: [],
    launch: { kind: "application", path: "/opt/game" },
    config_overrides: { keybindings: [65, 66, 65, 67] },
  };
  const updates = [];
  context.Amaterasu.renderCatalog({
    content, notify() {}, api: {
      allows: () => true,
      async post(route, body) {
        if (route === "catalog/list") return {
          ok: true, revision, next_cursor: null,
          config_override_capabilities: {
            frame_limiter_provider: ["auto"], rtss_frame_limit_type: ["async"],
          },
          entries: [{ id: "app-1", revision, entry }],
        };
        if (route === "input/status") throw new Error("unavailable");
        assert.equal(route, "catalog/update");
        assert.equal(body.id, "app-1");
        assert.equal(body.expected_revision, revision);
        updates.push(ordinary(body));
        entry = ordinary(body.entry);
        revision += 1;
        return { ok: true, revision };
      },
    },
  });
  await settle();
  let expectedUpdates = 0;
  for (const [text, expected] of [["[0x41,0x44,0x41,0x45]", [65, 68, 65, 69]], ["[]", []], ["", undefined]]) {
    find(content, (node) => node.tagName === "button" && node.textContent === "Edit").fire("click");
    await settle();
    const form = find(content, (node) => node.className === "editor-form");
    const control = find(form, (node) => node.name === "keybindings");
    assert.equal(control.value, JSON.stringify(entry.config_overrides?.keybindings) ?? "");
    if (updates.length === 0) {
      control.value = "[0,256]";
      form.fire("submit");
      await settle();
      assert.equal(updates.length, 0, "invalid byte map reached a catalog mutation");
    }
    control.value = text;
    form.fire("submit");
    await settle();
    expectedUpdates += 1;
    assert.equal(updates.length, expectedUpdates, form.textContent);
    assert.equal(form.isConnected, false, form.textContent);
    assert.deepEqual(entry.config_overrides.keybindings, expected);
    assert.equal(Object.hasOwn(entry.config_overrides, "keybindings"), expected !== undefined);
  }
  assert.deepEqual(updates.map((update) => update.expected_revision), [7, 8, 9]);
});
