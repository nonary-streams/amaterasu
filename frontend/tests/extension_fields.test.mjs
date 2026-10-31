import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function editorEnvironment() {
  class Element {
    children = [];
    value = "";
    checked = false;
    validity = { valid: true };
    listeners = {};
    setAttribute() {}
    replaceChildren(...children) { this.children = []; this.append(...children); }
    get lastElementChild() { return this.children.filter((child) => child instanceof Element).at(-1); }
    append(...children) { this.children.push(...children); for (const child of children) if (child instanceof Element) child.parent = this; }
    addEventListener(type, callback) { this.listeners[type] = callback; }
    remove() { this.parent.children = this.parent.children.filter((child) => child !== this); }
    click() { this.listeners.click(); }
  }
  const context = vm.createContext({ document: { createElement: () => new Element() }, structuredClone });
  const source = ["ui.ts", "extension_fields.ts"]
    .map((file) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"))
    .join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  return context.Amaterasu;
}
const normalized = (value) => JSON.parse(JSON.stringify(value));

test("nested schema editors preserve false, zero and empty strings", () => {
  const create = editorEnvironment().extensionFieldEditor;
  const editor = create({ type: "object", default: {}, fields: {
    enabled: { type: "boolean", default: true },
    count: { type: "integer", default: 1 },
    name: { type: "string", default: "name" },
    modes: { type: "list", default: [], item: { type: "enum", default: "a", options: ["a", "b"] } },
  } }, { enabled: false, count: 0, name: "", modes: ["b"] });
  assert.deepEqual(normalized(editor.value()), { enabled: false, count: 0, name: "", modes: ["b"] });
  const list = editor.node.children[3].children[1];
  list.children[1].click();
  assert.deepEqual(normalized(editor.value()).modes, ["b", "a"]);
  list.children[0].children[0].children[1].click();
  assert.deepEqual(normalized(editor.value()).modes, ["a"]);
});

test("invalid numbers and unsupported schemas fail closed", () => {
  const create = editorEnvironment().extensionFieldEditor;
  const editor = create({ type: "integer", default: 0 }, 1);
  editor.node.value = "1.5";
  assert.throws(() => editor.value());
  assert.throws(() => create({ type: "unknown", default: null }, null));
});


test("keybinding editor retains provider key identities and action parameters", () => {
  const { extensionKeybindingEditor } = editorEnvironment();
  const binding = { id: "remove", enabled: true, source: "session_input", context: "stream", target: "current_session", propagation: "consume_trigger", action_id: "example/remove", arguments: { force: false }, trigger: { platform: "windows_scan", code: 30, extended: true }, modifiers: [{ platform: "linux_evdev", code: 29 }] };
  const editor = extensionKeybindingEditor(binding);
  assert.deepEqual(normalized(editor.value()), binding);
});


test("tagged union renders and preserves the selected variant", () => {
  const create = editorEnvironment().extensionFieldEditor;
  const definition = { type: "tagged_union", default: { platform: "linux", code: 1 }, tag: "platform", variants: {
    linux: { code: { type: "integer", default: 1 } },
    windows: { code: { type: "integer", default: 2 }, extended: { type: "boolean", default: false } },
  } };
  const editor = create(definition, { platform: "windows", code: 42, extended: true });
  assert.deepEqual(normalized(editor.value()), { platform: "windows", code: 42, extended: true });
});
