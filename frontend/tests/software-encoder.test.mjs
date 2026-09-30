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
  }
  append(...children) { this.children.push(...children); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  fire(name) { this.listeners.get(name)?.(); }
}

const context = vm.createContext({
  FormData, TextEncoder,
  document: { createElement: (tagName) => new Element(tagName) },
});
for (const source of ["src/model.ts", "src/ui.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }), context);
  vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
}
const ordinary = (value) => JSON.parse(JSON.stringify(value));
const controls = (fields) => {
  const values = new Map();
  const visit = (node) => {
    if (typeof node === "string") return;
    if (node.name) values.set(node.name, node);
    for (const child of node.children ?? []) visit(child);
  };
  fields.forEach(visit);
  return values;
};

test("application software editor preserves exact strings and signed integer overrides", () => {
  const editor = context.Amaterasu.softwareEncoderOverrideControls({
    sw_preset: "  slow  ", sw_tune: "film,zerolatency", min_threads: -2, qp: 28, encoder: "auto",
  });
  const fields = controls(editor.fields);
  assert.equal(fields.get("sw_preset_mode").value, "value");
  assert.equal(fields.get("sw_preset_value").value, "  slow  ");
  assert.equal(fields.get("sw_tune_value").value, "film,zerolatency");
  assert.equal(fields.get("min_threads").value, "-2");
  assert.equal(fields.get("min_threads").min, "-2147483648");
  assert.equal(fields.get("min_threads").max, "2147483647");
  assert.equal(fields.get("qp").value, "28");
  assert.equal(fields.get("encoder_backend").value, "auto");
  const form = new FormData();
  form.set("name", "Software application");
  form.set("kind", "executable");
  form.set("path", "/usr/bin/true");
  const readEntry = () => {
    for (const [name, field] of fields) form.set(name, field.value);
    return ordinary(context.Amaterasu.entryFromForm(form));
  };
  assert.deepEqual(readEntry().config_overrides, {
    sw_preset: "  slow  ", sw_tune: "film,zerolatency", min_threads: -2, qp: 28, encoder: "auto",
  });
  const mode = fields.get("sw_preset_mode");
  mode.value = "inherit";
  mode.fire("change");
  assert.equal(fields.get("sw_preset_value").disabled, true);
  assert.equal(Object.hasOwn(readEntry().config_overrides, "sw_preset"), false);
  mode.value = "value";
  mode.fire("change");
  assert.equal(fields.get("sw_preset_value").disabled, false);
  assert.equal(fields.get("sw_preset_value").value, "  slow  ");
  fields.get("sw_preset_value").value = "";
  fields.get("min_threads").value = "-2147483648";
  fields.get("qp").value = "2147483647";
  fields.get("encoder_backend").value = "software";
  assert.deepEqual(readEntry().config_overrides, {
    sw_preset: "", sw_tune: "film,zerolatency", min_threads: -2147483648, qp: 2147483647, encoder: "software",
  });
  assert.deepEqual(ordinary(editor.read()), readEntry().config_overrides);
});

test("global software editor shows defaults and preserves the complete signed C int range", () => {
  const editor = context.Amaterasu.softwareEncoderOverrideControls({ min_threads: null, qp: null }, true);
  const fields = controls(editor.fields);
  assert.equal(fields.get("min_threads").placeholder, "Default request (2)");
  assert.equal(fields.get("min_threads").value, "");
  assert.equal(fields.get("qp").placeholder, "Default (28)");
  fields.get("min_threads").value = "2147483647";
  fields.get("qp").value = "-2147483648";
  assert.equal(editor.read().min_threads, 2147483647);
  assert.equal(editor.read().qp, -2147483648);
  fields.get("min_threads").value = "";
  assert.equal(editor.read().min_threads, null);
  for (const value of ["-2147483649", "2147483648", "1.5", "--1"]) {
    fields.get("min_threads").value = value;
    assert.throws(() => editor.read());
  }
  for (const value of ["-2147483649", "2147483648", "1.5"]) {
    fields.get("qp").value = value;
    assert.throws(() => editor.read());
  }
});
