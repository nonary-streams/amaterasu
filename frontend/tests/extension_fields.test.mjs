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
    attributes = {};
    setAttribute(name, value) { this.attributes[name] = value; }
    removeAttribute(name) { delete this.attributes[name]; }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    get lastElementChild() { return this.children.filter((child) => child instanceof Element).at(-1); }
    append(...children) { this.children.push(...children); for (const child of children) if (child instanceof Element) child.parent = this; }
    addEventListener(type, callback) { this.listeners[type] = callback; }
    dispatch(type) { this.listeners[type]?.({ type, target: this }); }
    remove() { this.parent.children = this.parent.children.filter((child) => child !== this); }
    click() { this.listeners.click(); }
  }
  const context = vm.createContext({ document: { createElement: () => new Element() }, structuredClone, AbortController });
  const source = ["ui.ts", "extension_fields.ts"]
    .map((file) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"))
    .join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  return context.Amaterasu;
}
const normalized = (value) => JSON.parse(JSON.stringify(value));

test("enum choices show plugin labels while preserving wire values", () => {
  const create = editorEnvironment().extensionFieldEditor;
  const editor = create({ type: "enum", default: "early", options: ["early", "late"] }, "late", {
    early: "Smooth pacing", late: "Lower latency",
  });
  assert.equal(editor.node.children[0].children[0], "Smooth pacing");
  assert.equal(editor.node.children[0].value, "early");
  assert.equal(editor.node.children[1].children[0], "Lower latency");
  assert.equal(editor.value(), "late");
});

test("radio presentation exposes a labelled native radio group and keeps enum wire values", () => {
  const { extensionRadioGroupEditor } = editorEnvironment();
  const editor = extensionRadioGroupEditor({ type: "enum", default: "steady", options: ["steady", "responsive"] },
    "steady", "Pacing", { steady: "Smooth pacing", responsive: "Lower latency" });
  const [label, choices] = editor.node.children;
  const [steady, responsive] = choices.children;
  assert.equal(editor.node.attributes.role, "radiogroup");
  assert.ok(editor.node.attributes["aria-labelledby"]);
  assert.equal(label.children[0], "Pacing");
  assert.equal(steady.children[0].type, "radio");
  assert.equal(steady.children[0].checked, true);
  assert.equal(steady.children[1].children[0], "Smooth pacing");
  assert.equal(responsive.children[0].value, "responsive");
  responsive.children[0].checked = true;
  responsive.children[0].dispatch("change");
  assert.equal(editor.value(), "responsive");
  assert.throws(() => extensionRadioGroupEditor({ type: "enum", options: ["a", "b"] }, "missing", "Pacing"), /Invalid radio/);
  assert.throws(() => extensionRadioGroupEditor({ type: "boolean" }, true, "Enabled"), /Invalid radio/);
});

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

test("bounded slider and number controls synchronize input and reject invalid edits", () => {
  const { extensionSliderEditor, extensionFieldEditor } = editorEnvironment();
  const definition = { type: "integer", default: 10, minimum: 0, maximum: 100 };
  const editor = extensionSliderEditor(definition, 10, "Strength");
  const [slider, number] = editor.node.children;
  assert.equal(slider.type, "range");
  assert.equal(slider.attributes["aria-label"], "Strength slider");
  assert.equal(number.attributes["aria-label"], "Strength value");
  assert.equal(slider.min, "0");
  assert.equal(slider.max, "100");
  assert.equal(slider.step, "1");
  slider.value = "35"; slider.dispatch("input");
  assert.equal(number.value, "35");
  assert.equal(editor.value(), 35);
  number.value = "42"; number.dispatch("input");
  assert.equal(slider.value, "42");
  assert.equal(editor.value(), 42);
  number.value = "101"; number.dispatch("input");
  assert.equal(slider.value, "42");
  assert.throws(() => editor.value(), /valid number/);
  number.value = "1.5"; number.dispatch("input");
  assert.throws(() => editor.value(), /valid number/);
  const ordinary = extensionFieldEditor(definition, 10);
  assert.equal(ordinary.node.type, "number");
  assert.equal(ordinary.value(), 10);
  assert.throws(() => extensionSliderEditor({ ...definition, maximum: null }, 10, "Bad"), /Invalid slider/);
  assert.throws(() => extensionSliderEditor({ ...definition, type: "string" }, 10, "Bad"), /Invalid slider/);
  const fractional = extensionSliderEditor({ type: "decimal", minimum: -2, maximum: -1 }, -1.5, "Fraction");
  assert.equal(fractional.node.children[0].step, "any");
  assert.equal(fractional.node.children[0].value, "-1.5");
  assert.equal(fractional.value(), -1.5);
});


test("keybinding editor retains provider key identities and action parameters", () => {
  const { extensionKeybindingEditor } = editorEnvironment();
  const binding = { id: "remove", enabled: true, source: "session_input", context: "stream", target: "current_session", propagation: "consume_trigger", action_id: "example/remove", arguments: { force: false }, trigger: { platform: "windows_scan", code: 30, extended: true }, modifiers: [{ platform: "windows_scan", code: 29, extended: false }] };
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


test("keybinding rejects chords mixing provider encodings", () => {
  const { extensionKeybindingEditor } = editorEnvironment();
  const editor = extensionKeybindingEditor({ id: "x", enabled: true, source: "session_input", context: "stream", target: "current_session", propagation: "forward", action_id: "example/action", arguments: {}, trigger: { platform: "windows_scan", code: 30, extended: false }, modifiers: [{ platform: "linux_evdev", code: 29 }] });
  assert.throws(() => editor.value(), /same key encoding/);
});

test("optional keybinding remains unset until a user explicitly creates it", () => {
  const { optionalExtensionKeybindingEditor } = editorEnvironment();
  const editor = optionalExtensionKeybindingEditor(null);
  assert.equal(editor.value(), null);
  assert.equal(editor.node.children[2].hidden, false);
  editor.node.children[2].click();
  assert.throws(() => editor.value(), /Complete the binding/);
  editor.node.children[3].click();
  assert.equal(editor.value(), null);
});

test("an existing optional binding can be explicitly removed without inventing a shortcut", () => {
  const { optionalExtensionKeybindingEditor } = editorEnvironment();
  const binding = { id: "remove", enabled: true, source: "session_input", context: "active", target: "current_stream", propagation: "consume_trigger", action_id: "example/remove", arguments: {}, trigger: { platform: "linux_evdev", code: 30 }, modifiers: [] };
  const editor = optionalExtensionKeybindingEditor(binding);
  assert.deepEqual(normalized(editor.value()), binding);
  editor.node.children[3].click();
  assert.equal(editor.value(), null);
});

test("nullable schema preserves null and uses its template only after explicit selection", () => {
  const create = editorEnvironment().extensionFieldEditor;
  const editor = create({ type: "nullable", default: null, inner: { type: "string", default: "example" } }, null);
  assert.equal(editor.value(), null);
  editor.node.children[2].click();
  assert.equal(editor.value(), "example");
  editor.node.children[3].click();
  assert.equal(editor.value(), null);
  assert.throws(() => create({ type: "nullable", default: null }, null), /Invalid nullable setting/);
});

test("creating a shortcut requires an explicit trigger and action", () => {
  const { optionalExtensionKeybindingEditor } = editorEnvironment();
  const editor = optionalExtensionKeybindingEditor(null);
  editor.node.children[2].click();
  const fields = editor.node.children[1].children[0].children;
  fields[0].children[1].value = "my-binding";
  fields[2].children[1].value = "active";
  fields[3].children[1].value = "session_input";
  fields[4].children[1].value = "current_stream";
  fields[5].children[1].value = "consume_trigger";
  fields[6].children[1].value = "example/remove";
  assert.throws(() => editor.value(), /valid number|key code/);
  const trigger = fields[8];
  trigger.children[1].children[1].value = "linux_evdev";
  trigger.children[2].children[1].value = "30";
  assert.deepEqual(normalized(editor.value()), {
    id: "my-binding", enabled: false, source: "session_input", context: "active", target: "current_stream",
    propagation: "consume_trigger", action_id: "example/remove", arguments: {},
    trigger: { platform: "linux_evdev", code: 30 }, modifiers: [],
  });
});

test("nullable keybinding does not save a zero-code schema template", () => {
  const { extensionKeybindingSettingEditor } = editorEnvironment();
  const sentinel = { id: "remove", enabled: false, source: "session_input", context: "active", target: "current_stream", propagation: "consume_trigger", action_id: "example/remove", arguments: {}, trigger: { platform: "linux_evdev", code: 0 }, modifiers: [] };
  const definition = { type: "nullable", default: null, inner: { type: "object", default: sentinel, fields: {} } };
  const editor = extensionKeybindingSettingEditor(definition, null);
  assert.equal(editor.value(), null);
  editor.node.children[2].click();
  assert.throws(() => editor.value(), /Complete the binding/);
  const invalid = extensionKeybindingSettingEditor(definition, sentinel);
  assert.throws(() => invalid.value(), /supported key code/);
  const zeroModifier = { ...sentinel, trigger: { platform: "linux_evdev", code: 30 }, modifiers: [{ platform: "linux_evdev", code: 0 }] };
  assert.throws(() => extensionKeybindingSettingEditor(definition, zeroModifier).value(), /supported key code/);
});
