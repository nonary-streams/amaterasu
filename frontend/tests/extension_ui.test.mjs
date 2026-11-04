import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag = "div") { this.tagName = tag.toUpperCase(); this.children = []; this.attributes = {}; this.listeners = {}; this.value = ""; this.checked = false; this.validity = { valid: true }; this.textContent = ""; this.hidden = false; this.disabled = false; this.required = false; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  setAttribute(name, value) { this.attributes[name] = value; }
  getAttribute(name) { return this.attributes[name] ?? null; }
  removeAttribute(name) { delete this.attributes[name]; }
  addEventListener(type, callback, options = {}) {
    const entry = { callback, signal: options.signal, capture: options.capture === true };
    (this.listeners[type] ??= []).push(entry);
  }
  focus() { if (this.ownerDocument) this.ownerDocument.activeElement = this; }
  dispatch(type, extras = {}) {
    const event = { type, target: this, defaultPrevented: false, immediatePropagationStopped: false,
      preventDefault() { this.defaultPrevented = true; },
      stopImmediatePropagation() { this.immediatePropagationStopped = true; }, ...extras };
    for (const entry of this.listeners[type] ?? []) {
      if (entry.signal?.aborted) continue;
      entry.callback(event);
      if (event.immediatePropagationStopped) break;
    }
    return event;
  }
}

function setupConsumer({ features = ["ui.controls/1", "ui.forms/1"], withUi = true } = {}) {
  let registered;
  const document = { activeElement: null, createElement(tag) { const element = new Element(tag); element.ownerDocument = this; return element; } };
  const context = vm.createContext({
    document,
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
  return { container, cleanup, controller, sdk, context };
}

test("independent plugin consumer gets accessible native controls and lifecycle cleanup", () => {
  const page = setupConsumer();
  assert.equal(page.container.children.length, 2);
  const [toggle, radios] = page.container.children;
  assert.equal(toggle.tagName, "DIV");
  assert.equal(toggle.children[0].tagName, "LABEL");
  assert.equal(toggle.children[0].htmlFor, toggle.children[1].id);
  assert.equal(toggle.children[1].type, "checkbox");
  assert.equal(radios.tagName, "DIV");
  assert.equal(radios.attributes.role, "radiogroup");
  assert.ok(radios.attributes["aria-labelledby"]);
  assert.equal(radios.children[0].children[0], "Style de rendu");
  assert.equal(radios.children[1].children[0].children[0].type, "radio");
  assert.equal(radios.children[1].children[0].children[0].checked, true);
  page.cleanup();
  assert.equal(page.controller.signal.aborted, false);
  for (const child of [toggle.children[1], radios.children[1].children[0].children[0]])
    assert.ok(child.listeners.change.every((entry) => entry.signal.aborted));
});

test("an SDK v1 consumer renders a fallback when an older host lacks UI features", () => {
  const page = setupConsumer({ features: ["ui.controls/1"], withUi: true });
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


test("field metadata is localized and associated with native controls", () => {
  const page = setupConsumer();
  const field = page.sdk.ui.toggle({
    label: { source: "Enabled", translations: { fr: "Activé" } },
    value: false,
    help: { source: "Enable this option.", translations: { fr: "Activez cette option." } },
    error: { source: "This choice is required.", translations: { fr: "Ce choix est obligatoire." } },
    required: true,
  });
  const input = field.element.children[1];
  const [help, error] = field.element.children.slice(2);
  assert.equal(field.element.tagName, "DIV");
  assert.equal(field.element.children[0].tagName, "LABEL");
  assert.equal(field.element.children[0].htmlFor, input.id);
  assert.notEqual(help.tagName, "LABEL");
  assert.notEqual(error.tagName, "LABEL");
  const descriptions = input.attributes["aria-describedby"].split(" ");
  assert.deepEqual(descriptions, [help.id, error.id]);
  assert.equal(help.children[0], "Activez cette option.");
  assert.equal(error.children[0], "Ce choix est obligatoire.");
  assert.equal(error.attributes.role, "alert");
  assert.equal(input.attributes["aria-invalid"], "true");
  assert.equal(input.attributes["aria-required"], "true");
  assert.equal(input.required, true);
  field.dispose();
  page.cleanup();
});

test("disabled and read-only controls reject user changes but allow programmatic updates", () => {
  const page = setupConsumer();
  let toggleChanges = 0;
  const toggle = page.sdk.ui.toggle({ label: "Pinned", value: true, readOnly: true, onChange: () => toggleChanges++ });
  const checkbox = toggle.element.children[1];
  checkbox.dispatch("click");
  checkbox.checked = false;
  checkbox.dispatch("change");
  assert.equal(checkbox.checked, true);
  assert.equal(toggleChanges, 0);
  assert.equal(checkbox.attributes["aria-readonly"], "true");
  toggle.setValue(false);
  assert.equal(toggle.value(), false);

  let selectChanges = 0;
  const select = page.sdk.ui.select({ label: "Locked", choices: [{ value: "a", label: "A" }, { value: "b", label: "B" }], value: "a", readOnly: true, onChange: () => selectChanges++ });
  const selectInput = select.element.children[1];
  selectInput.value = "b";
  selectInput.dispatch("change");
  const copied = selectInput.dispatch("keydown", { key: "c", ctrlKey: true });
  const arrow = selectInput.dispatch("keydown", { key: "ArrowDown" });
  assert.equal(copied.defaultPrevented, false);
  assert.equal(arrow.defaultPrevented, true);
  assert.equal(select.value(), "a");
  assert.equal(selectChanges, 0);

  const disabled = page.sdk.ui.toggle({ label: "Unavailable", value: false, disabled: true });
  assert.equal(disabled.element.children[1].disabled, true);
  toggle.dispose(); select.dispose(); disabled.dispose(); page.cleanup();
});

test("choice controls update options transactionally without firing change callbacks", () => {
  const page = setupConsumer();
  const changes = [];
  const select = page.sdk.ui.select({ label: "Mode", choices: [{ value: "a", label: "A" }, { value: "b", label: "B" }], value: "a", onChange: (value) => changes.push(value) });
  select.setChoices([{ value: "b", label: "Beta" }, { value: "c", label: "Gamma" }], "c");
  assert.equal(select.value(), "c");
  assert.deepEqual(select.element.children[1].children.map((option) => option.value), ["b", "c"]);
  assert.deepEqual(changes, []);
  assert.throws(() => select.setChoices([{ value: "b", label: "Beta" }]), /choices/);
  assert.equal(select.value(), "c");
  assert.deepEqual(select.element.children[1].children.map((option) => option.value), ["b", "c"]);
  assert.throws(() => select.setChoices([{ value: "x", label: "X" }, { value: "x", label: "Duplicate" }], "x"), /choices/);
  assert.throws(() => select.setChoices([{ value: "broken", label: { source: "Broken", translations: { "fr-CA": "" } } }], "broken"), /translations/);
  assert.equal(select.value(), "c");
  assert.deepEqual(select.element.children[1].children.map((option) => option.value), ["b", "c"]);

  const radio = page.sdk.ui.radioGroup({ label: "Quality", choices: [{ value: "low", label: "Low" }, { value: "high", label: "High" }], value: "low", required: true, error: "Choose a quality." });
  const oldRadioInput = radio.element.children[1].children[0].children[0];
  radio.setChoices([{ value: "low", label: "Low" }, { value: "ultra", label: "Ultra" }]);
  assert.equal(radio.value(), "low");
  assert.ok(oldRadioInput.listeners.change.every((entry) => entry.signal.aborted));
  assert.equal(radio.element.children[1].children.length, 2);
  const radioInput = radio.element.children[1].children[0].children[0];
  assert.ok(radioInput.attributes["aria-describedby"]);
  assert.equal(radioInput.attributes["aria-invalid"], "true");
  assert.equal(radioInput.attributes["aria-required"], "true");
  assert.throws(() => radio.setChoices([{ value: "x", label: "X" }, { value: "y", label: "Y" }]), /choices/);
  assert.equal(radio.value(), "low");
  assert.deepEqual(changes, []);
  select.dispose(); radio.dispose(); page.cleanup();
});

test("read-only radio groups and sliders expose required/error semantics and reject edits", () => {
  const page = setupConsumer();
  const radio = page.sdk.ui.radioGroup({ label: "Preset", choices: [{ value: "a", label: "A" }, { value: "b", label: "B" }], value: "a", readOnly: true, required: true, error: "Choose a preset." });
  const option = radio.element.children[1].children[1].children[0];
  option.checked = true;
  option.dispatch("change");
  assert.equal(radio.value(), "a");
  assert.equal(radio.element.attributes["aria-readonly"], "true");
  assert.equal(radio.element.attributes["aria-required"], "true");
  assert.ok(radio.element.attributes["aria-describedby"]);
  assert.equal(option.required, true);
  assert.equal(option.dispatch("keydown", { key: "c", ctrlKey: true }).defaultPrevented, false);
  const nextOption = radio.element.children[1].children[0].children[0];
  option.focus();
  const radioChangeEvent = option.dispatch("keydown", { key: "ArrowDown" });
  assert.equal(radioChangeEvent.defaultPrevented, true);
  assert.equal(page.context.document.activeElement, nextOption);
  assert.equal(option.checked, false);
  assert.equal(nextOption.checked, true);
  assert.equal(radio.value(), "a");

  const slider = page.sdk.ui.slider({ label: "Power", minimum: 0, maximum: 10, step: 1, value: 5, readOnly: true, required: true, help: "Read only value." });
  const [range, number] = slider.element.children[1].children;
  range.value = "7";
  range.dispatch("input");
  assert.equal(slider.value(), 5);
  assert.equal(range.value, "5");
  assert.equal(number.readOnly, true);
  assert.equal(number.required, true);
  assert.ok(number.attributes["aria-describedby"]);
  radio.dispose(); slider.dispose(); page.cleanup();
});
