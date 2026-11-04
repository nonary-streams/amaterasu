import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag = "div") { this.tagName = tag.toUpperCase(); this.children = []; this.attributes = {}; this.listeners = {}; this.value = ""; this.checked = false; this.validity = { valid: true }; }
  append(...children) { this.children.push(...children); }
  setAttribute(name, value) { this.attributes[name] = value; }
  addEventListener(type, callback, options = {}) {
    const entry = { callback, signal: options.signal };
    (this.listeners[type] ??= []).push(entry);
  }
  dispatch(type) { for (const entry of this.listeners[type] ?? []) if (!entry.signal?.aborted) entry.callback({ type, target: this }); }
}

function setupConsumer() {
  let registered;
  const context = vm.createContext({
    document: { createElement: (tag) => new Element(tag) },
    window: { AmaterasuExtensions: { v1: { register(_id, page) { registered = page; } } } },
    AbortController,
  });
  context.localizedTextNode = (text) => text;
  context.localeFormattedTextNode = (format) => format("fr-CA");
  context.translateText = (text) => text;
  context.textValueNode = (value) => typeof value === "string" ? value : value.value;
  const source = ["ui.ts", "extension_ui.ts"]
    .map((file) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"))
    .join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const controller = new AbortController();
  const sdk = { ui: context.Amaterasu.createExtensionUiV1(controller.signal), signal: controller.signal };
  vm.runInContext(readFileSync(new URL("../../examples/web-extension/controls.js", import.meta.url), "utf8"), context);
  const container = new Element("main");
  const cleanup = registered.mount(container, sdk);
  return { container, cleanup, controller, sdk };
}

test("independent plugin consumer gets accessible native controls and lifecycle cleanup", () => {
  const page = setupConsumer();
  assert.equal(page.container.children.length, 2);
  const [toggle, radios] = page.container.children;
  assert.equal(toggle.tagName, "LABEL");
  assert.equal(toggle.children.at(-1).type, "checkbox");
  assert.equal(radios.tagName, "FIELDSET");
  assert.equal(radios.children[0].tagName, "LEGEND");
  assert.equal(radios.children[0].children[0], "Style de rendu");
  assert.equal(radios.children[1].children[0].type, "radio");
  assert.equal(radios.children[1].children[0].checked, true);
  page.cleanup();
  assert.equal(page.controller.signal.aborted, false);
  for (const child of [toggle.children.at(-1), radios.children[1].children[0]])
    assert.ok(child.listeners.change.every((entry) => entry.signal.aborted));
});

test("control instances dispose idempotently when the host lifecycle aborts", () => {
  const page = setupConsumer();
  page.controller.abort();
  page.cleanup();
  for (const element of page.container.children.flatMap((item) => item.children)) {
    if (element?.listeners?.change) assert.ok(element.listeners.change.every((entry) => entry.signal.aborted));
  }
});


test("select and slider controls retain wire values and report valid user changes", () => {
  const page = setupConsumer();
  const changes = [];
  const select = page.sdk.ui.select({
    label: "Profile",
    choices: [{ value: "slow", label: "Slow" }, { value: "fast", label: "Fast" }],
    value: "slow",
    onChange: (value) => changes.push(value),
  });
  const selectElement = select.element.children[1];
  assert.equal(select.value(), "slow");
  select.setValue("fast");
  assert.equal(select.value(), "fast");
  assert.deepEqual(changes, []);
  selectElement.value = "slow";
  selectElement.dispatch("change");
  assert.deepEqual(changes, ["slow"]);
  assert.throws(() => select.setValue("missing"), /Unsupported select/);

  const slider = page.sdk.ui.slider({ label: "Strength", minimum: 0, maximum: 10, step: 1, value: 4, onChange: (value) => changes.push(value) });
  const inputs = slider.element.children.at(-1).children;
  assert.equal(slider.value(), 4);
  assert.ok(inputs[0].attributes["aria-labelledby"]);
  assert.ok(inputs[1].attributes["aria-labelledby"]);
  inputs[0].value = "7";
  inputs[0].dispatch("input");
  assert.equal(slider.value(), 7);
  assert.equal(inputs[1].value, "7");
  assert.equal(changes.at(-1), 7);
  inputs[1].value = "bad";
  inputs[1].dispatch("input");
  assert.equal(inputs[0].value, "7");
  slider.dispose();
  assert.ok(inputs[0].listeners.input[0].signal.aborted);
  page.cleanup();
});
