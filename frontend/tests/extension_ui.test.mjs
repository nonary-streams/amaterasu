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

function setupConsumer({ features = ["ui.controls/1"], withUi = true } = {}) {
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
  const sdk = { features, signal: controller.signal };
  if (withUi) sdk.ui = context.Amaterasu.createExtensionUiV1(controller.signal);
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
  assert.equal(radios.tagName, "DIV");
  assert.equal(radios.attributes.role, "radiogroup");
  assert.ok(radios.attributes["aria-labelledby"]);
  assert.equal(radios.children[0].children[0], "Style de rendu");
  assert.equal(radios.children[1].children[0].type, "radio");
  assert.equal(radios.children[1].children[0].checked, true);
  page.cleanup();
  assert.equal(page.controller.signal.aborted, false);
  for (const child of [toggle.children.at(-1), radios.children[1].children[0]])
    assert.ok(child.listeners.change.every((entry) => entry.signal.aborted));
});

test("an SDK v1 consumer renders a fallback when an older host lacks UI features", () => {
  const page = setupConsumer({ features: [], withUi: false });
  assert.equal(page.container.children.length, 1);
  assert.equal(page.container.children[0].textContent, "This host does not support the reusable controls in this page.");
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
  assert.equal(inputs[0].attributes["aria-labelledby"], inputs[1].attributes["aria-labelledby"]);
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


test("slider rejects values outside its step grid without changing either input", () => {
  const page = setupConsumer();
  const slider = page.sdk.ui.slider({ label: "Power", minimum: 0, maximum: 1, step: 0.1, value: 0.3 });
  const [range, number] = slider.element.children.at(-1).children;
  assert.equal(slider.value(), 0.3);
  assert.throws(() => slider.setValue(0.35), /step/);
  assert.equal(slider.value(), 0.3);
  assert.equal(range.value, "0.3");
  assert.equal(number.value, "0.3");
  assert.throws(() => page.sdk.ui.slider({ label: "Bad", minimum: 0, maximum: 10, step: 3, value: 4 }), /step/);
  slider.dispose();
  page.cleanup();
});

test("slider does not swallow a throwing change callback", () => {
  const page = setupConsumer();
  const slider = page.sdk.ui.slider({ label: "Power", minimum: 0, maximum: 10, step: 2, value: 2, onChange() { throw new Error("consumer callback failed"); } });
  const [range, number] = slider.element.children.at(-1).children;
  number.value = "4";
  assert.throws(() => number.dispatch("input"), /consumer callback failed/);
  assert.equal(range.value, "4");
  assert.equal(slider.value(), 4);
  slider.dispose();
  page.cleanup();
});
