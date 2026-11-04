import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.attributes = {};
    this.listeners = {};
    this.value = "";
    this._textContent = null;
    this.disabled = false;
    this.type = "";
    this.className = "";
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = [...children]; }
  setAttribute(name, value) { this.attributes[name] = value; }
  removeAttribute(name) { delete this.attributes[name]; }
  getAttribute(name) { return this.attributes[name] ?? null; }
  addEventListener(type, callback, options = {}) {
    (this.listeners[type] ??= []).push({ callback, signal: options.signal });
  }
  dispatch(type) {
    for (const entry of this.listeners[type] ?? [])
      if (!entry.signal?.aborted) entry.callback({ type, target: this });
  }
  dispatchEvent(event) { this.dispatch(event.type); return true; }
  focus() { this.focused = true; }
  querySelector(selector) { return selector === "input" ? this.children.find(child => child.tagName === "INPUT") : null; }
  get textContent() {
    return this._textContent ?? this.children.map(child => child.textContent).join("");
  }
  set textContent(value) { this._textContent = String(value); }
}
const document = { createElement: tag => new Element(tag) };
Object.defineProperty(Array.prototype, "item", { value(index) { return this[index] ?? null; } });
const context = vm.createContext({
  document, AbortController,
  Event: class { constructor(type, init = {}) { this.type = type; this.bubbles = init.bubbles ?? false; } },
  Amaterasu: {
    localizedMessage: value => ({ kind: "localized", value }),
    literalText: value => ({ kind: "literal", value }),
  },
});
vm.runInContext(ts.transpile(readFileSync("src/list_editor.ts", "utf8"), {
  target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.None,
}), context);
const ordinary = value => JSON.parse(JSON.stringify(value));
const inputs = editor => editor.element.children[1].children.map(row => row.children[0]);
const buttons = editor => editor.element.children[1].children.map(row => row.children[1]);

test("editor preserves unknown structured entries and original order until changed", () => {
  const unknown = { id: "future-id", vendorField: [1, 2] };
  const initial = [{ id: "1", name: "Known" }, unknown, "legacy-id"];
  let changes = [];
  const editor = context.Amaterasu.accessibleListEditor({
    label: "Lutris exclusions",
    values: initial,
    formatValue: value => typeof value === "string" ? value : JSON.stringify(value),
    parseValue: text => JSON.parse(text),
    onChange: values => changes.push(ordinary(values)),
  });
  assert.equal(editor.element.tagName, "FIELDSET");
  assert.equal(editor.element.children[0].tagName, "LEGEND");
  assert.equal(editor.element.children[1].getAttribute("aria-label"), "Lutris exclusions items");
  assert.deepEqual(ordinary(editor.values()), initial);
  assert.equal(inputs(editor)[1].value, JSON.stringify(unknown));
  inputs(editor)[0].value = JSON.stringify({ id: "2", name: "Edited" });
  inputs(editor)[0].dispatch("input");
  const expected = [{ id: "2", name: "Edited" }, unknown, "legacy-id"];
  assert.deepEqual(ordinary(editor.values()), expected);
  assert.deepEqual(changes.at(-1), expected);
  assert.equal(inputs(editor)[0].getAttribute("aria-label"), "Lutris exclusions, item 1");
  editor.dispose();
});

test("adding, editing and removing Playnite entries keeps their order", () => {
  let changed;
  const editor = context.Amaterasu.accessibleListEditor({
    label: "Excluded games",
    values: ["unknown-plugin-game", "local-id"],
    formatValue: value => value,
    parseValue: value => {
      if (!value.trim()) throw new Error("Enter a nonempty value.");
      return value;
    },
    onChange: values => { changed = [...values]; },
  });
  const add = editor.element.children[3];
  add.dispatch("click");
  assert.equal(inputs(editor).length, 3);
  assert.equal(editor.element.children[2].textContent, "Enter a value for the new item.");
  inputs(editor)[2].value = "new-game-id";
  inputs(editor)[2].dispatch("input");
  assert.deepEqual(editor.values(), ["unknown-plugin-game", "local-id", "new-game-id"]);
  assert.deepEqual(changed, ["unknown-plugin-game", "local-id", "new-game-id"]);
  buttons(editor)[0].dispatch("click");
  assert.deepEqual(editor.values(), ["local-id", "new-game-id"]);
  assert.deepEqual(changed, ["local-id", "new-game-id"]);
  assert.equal(buttons(editor)[0].getAttribute("aria-label"), "Remove Excluded games item 1");
  editor.dispose();
});

test("invalid edits are announced and excluded from readable values until corrected", () => {
  const editor = context.Amaterasu.accessibleListEditor({
    label: "Plugin IDs",
    values: ["playnite.plugin"],
    formatValue: value => value,
    parseValue: value => {
      if (!/^[a-z.]+$/.test(value)) throw new Error("Use lowercase plugin IDs.");
      return value;
    },
  });
  const input = inputs(editor)[0];
  input.value = "BAD VALUE";
  input.dispatch("input");
  assert.equal(input.getAttribute("aria-invalid"), "true");
  assert.equal(editor.element.children[2].textContent, "Use lowercase plugin IDs.");
  assert.throws(() => editor.values(), /Resolve invalid/);
  input.value = "other.plugin";
  input.dispatch("input");
  assert.equal(input.getAttribute("aria-invalid"), null);
  assert.deepEqual(editor.values(), ["other.plugin"]);
});

test("editor enforces item bounds and updates from replacement values", () => {
  const editor = context.Amaterasu.accessibleListEditor({
    label: "Categories", values: ["one"], maxItems: 1,
    formatValue: value => value, parseValue: value => value,
  });
  assert.equal(editor.element.children[3].disabled, true);
  assert.throws(() => editor.setValues(["one", "two"]), /at most 1/);
  editor.setValues(["new"]);
  assert.deepEqual(editor.values(), ["new"]);
  assert.throws(() => context.Amaterasu.accessibleListEditor({
    label: "Invalid", values: ["one", "two"], maxItems: 1,
    formatValue: value => value, parseValue: value => value,
  }), /Invalid list editor options/);
  editor.dispose();
});

test("consumer callback errors are not misreported as invalid input", () => {
  const editor = context.Amaterasu.accessibleListEditor({
    label: "Categories",
    values: ["original"],
    formatValue: value => value,
    parseValue: value => value,
    onChange() { throw new Error("consumer callback failed"); },
  });
  const input = inputs(editor)[0];
  input.value = "updated";
  assert.throws(() => input.dispatch("input"), /consumer callback failed/);
  assert.equal(input.getAttribute("aria-invalid"), null);
  assert.deepEqual(editor.values(), ["updated"]);
});
