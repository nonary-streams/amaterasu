import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag = "div") {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.attributes = {};
    this.listeners = {};
    this.textContent = "";
    this.value = 0;
    this.max = 1;
    this.hidden = false;
  }
  get childNodes() { return this.children; }
  append(...children) {
    for (const child of children) {
      if (child && typeof child === "object") child.parent = this;
      this.children.push(child);
    }
  }
  remove() {
    if (this.parent) this.parent.children = this.parent.children.filter((child) => child !== this);
    this.parent = null;
  }
  replaceChildren(...children) {
    for (const child of this.children) if (child && typeof child === "object") child.parent = null;
    this.children = [];
    this.append(...children);
  }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  removeAttribute(name) { delete this.attributes[name]; }
  addEventListener(type, callback, options = {}) { (this.listeners[type] ??= []).push({ callback, signal: options.signal }); }
  removeEventListener(type, callback) { this.listeners[type] = (this.listeners[type] ?? []).filter((entry) => entry.callback !== callback); }
}

function setup() {
  const document = { createElement(tag) { const element = new Element(tag); element.ownerDocument = this; return element; } };
  const context = vm.createContext({ document, AbortController });
  context.localizedTextNode = (text) => text;
  context.localeFormattedTextNode = (format) => format("fr-CA");
  context.translateText = (text) => `translated:${text}`;
  const source = ["ui.ts", "extension_ui.ts", "extension_ui_feedback.ts"]
    .map((file) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"))
    .join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const controller = new AbortController();
  const feedback = context.Amaterasu.createExtensionUiFeedbackV1(controller.signal);
  return { controller, feedback, context };
}

test("status and alert use text-only accessible live regions and support updates", () => {
  const { feedback } = setup();
  const status = feedback.status({ message: "Ready", busy: true });
  assert.equal(status.element.getAttribute("role"), "status");
  assert.equal(status.element.getAttribute("aria-live"), "polite");
  assert.equal(status.element.getAttribute("aria-atomic"), "true");
  assert.equal(status.element.getAttribute("aria-busy"), "true");
  assert.deepEqual(status.element.children, ["Ready"]);
  status.setMessage("<img src=x onerror=alert(1)>");
  assert.deepEqual(status.element.children, ["<img src=x onerror=alert(1)>"]);
  status.setBusy(false);
  assert.equal(status.element.getAttribute("aria-busy"), "false");

  const alert = feedback.alert({ message: { source: "Saved", translations: { fr: "Enregistré" } }, severity: "success" });
  assert.equal(alert.element.getAttribute("role"), "alert");
  assert.equal(alert.element.getAttribute("aria-live"), "assertive");
  assert.equal(alert.element.getAttribute("data-severity"), "success");
  assert.deepEqual(alert.element.children, ["Enregistré"]);
  alert.setSeverity("warning");
  alert.setMessage("Try again");
  assert.equal(alert.element.getAttribute("data-severity"), "warning");
  assert.deepEqual(alert.element.children, ["Try again"]);
});

test("progress uses a labeled native progress element with bounded and indeterminate values", () => {
  const { feedback } = setup();
  const progress = feedback.progress({ label: "Import", maximum: 10, value: 3, message: "Three of ten" });
  assert.equal(progress.element.children[0].tagName, "LABEL");
  const bar = progress.element.children[1];
  assert.equal(bar.tagName, "PROGRESS");
  assert.equal(progress.element.children[0].htmlFor, bar.id);
  assert.equal(bar.getAttribute("max"), "10");
  assert.equal(bar.getAttribute("value"), "3");
  assert.equal(bar.getAttribute("aria-describedby"), progress.element.children[2].id);
  assert.equal(progress.value(), 3);
  progress.setValue(12);
  assert.equal(progress.value(), 10);
  assert.equal(bar.getAttribute("value"), "10");
  progress.setValue(-2);
  assert.equal(progress.value(), 0);
  progress.setValue(null);
  assert.equal(progress.value(), null);
  assert.equal(bar.getAttribute("value"), null);
  progress.setMessage(null);
  assert.equal(progress.element.children.length, 2);
  assert.equal(bar.getAttribute("aria-describedby"), null);
  progress.setMessage("Back to work");
  assert.equal(progress.element.children.length, 3);
  assert.equal(progress.element.children[2].children[0], "Back to work");
});

test("localized text uses provided locale fallback and arbitrary markup remains text", () => {
  const { feedback } = setup();
  const status = feedback.status({ message: { source: "Uploading", translations: { fr: "Téléversement" } } });
  assert.deepEqual(status.element.children, ["Téléversement"]);
  status.setMessage("<script>bad()</script>");
  assert.deepEqual(status.element.children, ["<script>bad()</script>"]);
});

test("invalid options, severities, translations, and non-finite progress are rejected", () => {
  const { feedback } = setup();
  assert.throws(() => feedback.status({ message: "x", busy: "yes" }), /busy/);
  assert.throws(() => feedback.status({ message: { source: "x", translations: { fr: " " } } }), /translations/);
  assert.throws(() => feedback.alert({ message: "x", severity: "critical" }), /severity/);
  assert.throws(() => feedback.alert({ message: "x", severity: null }), /severity/);
  assert.throws(() => feedback.progress({ label: "x", maximum: null }), /maximum/);
  assert.throws(() => feedback.progress({ label: "x", maximum: 0 }), /maximum/);
  assert.throws(() => feedback.progress({ label: "x", maximum: Infinity }), /maximum/);
  assert.throws(() => feedback.progress({ label: "x", value: NaN }), /value/);
  const progress = feedback.progress({ label: "x" });
  assert.throws(() => progress.setValue(Infinity), /value/);
});

test("handles stop mutating after dispose or host abort, and dispose is idempotent", () => {
  const { controller, feedback } = setup();
  const status = feedback.status({ message: "before" });
  status.dispose();
  status.dispose();
  status.setMessage("after");
  status.setBusy(true);
  assert.deepEqual(status.element.children, ["before"]);
  assert.equal(status.element.getAttribute("aria-busy"), "false");

  const progress = feedback.progress({ label: "Work", value: 2 });
  controller.abort();
  progress.setValue(8);
  progress.setMessage("Finished");
  assert.equal(progress.value(), 2);
  assert.equal(progress.element.children[1].getAttribute("value"), "2");
  assert.throws(() => feedback.alert({ message: "late" }), /no longer active/);
});

test("combined UI factory preserves version 1 and exposes existing controls with feedback", () => {
  const { controller, context } = setup();
  const combined = context.Amaterasu.createExtensionUiWithFeedbackV1(controller.signal);
  assert.equal(combined.version, 1);
  assert.equal(Object.isFrozen(combined), true);
  assert.equal(typeof combined.toggle, "function");
  assert.equal(typeof combined.status, "function");
  assert.equal(typeof combined.alert, "function");
  assert.equal(typeof combined.progress, "function");
});
