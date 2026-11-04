import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag = "div", document = null) {
    this.tagName = tag.toUpperCase(); this.document = document; this.children = [];
    this.attributes = {}; this.listeners = {}; this.value = ""; this.checked = false;
    this.hidden = false; this.disabled = false; this.open = false; this.validity = { valid: true };
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  addEventListener(type, callback, options = {}) {
    (this.listeners[type] ??= []).push({ callback, signal: options.signal });
  }
  dispatch(type, values = {}) {
    const event = { type, target: this, preventDefault() { this.defaultPrevented = true; }, ...values };
    for (const entry of this.listeners[type] ?? []) if (!entry.signal?.aborted) entry.callback(event);
  }
  click() { this.dispatch("click"); }
  focus() { if (this.document) this.document.activeElement = this; }
  showModal() { this.open = true; }
  close() { this.open = false; this.dispatch("close"); }
  remove() { this.removed = true; }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child.textContent ?? "").join(""); }
  set textContent(value) { this.children = [String(value)]; }
}
function setupShowcase() {
  let registered;
  const document = { activeElement: null, createElement(tag) { return new Element(tag, this); } };
  const context = vm.createContext({
    document, window: { AmaterasuExtensions: { v1: { register(_id, page) { registered = page; } } } },
    AbortController, Error, Number, String, Object, Array,
  });
  context.localizedTextNode = (text) => text;
  context.localeFormattedTextNode = (format) => format("en");
  context.translateText = (text) => text;
  context.textValueNode = (value) => typeof value === "string" ? value : value.value;
  const source = ["ui.ts", "extension_ui.ts"]
    .map((file) => readFileSync(new URL("../src/" + file, import.meta.url), "utf8")).join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const lifecycle = new AbortController();
  const sdk = { ui: context.Amaterasu.createExtensionUiV1(lifecycle.signal), signal: lifecycle.signal };
  vm.runInContext(readFileSync(new URL("../../examples/web-extension/showcase.js", import.meta.url), "utf8"), context);
  const container = new Element("main", document);
  const cleanup = registered.mount(container, sdk);
  return { container, cleanup, lifecycle, document };
}
function all(node) {
  return [node, ...(node.children ?? []).flatMap((child) => child instanceof Element ? all(child) : [])];
}

test("Okami showcase consumes host SDK controls and displays both themes and component states", () => {
  const page = setupShowcase();
  const nodes = all(page.container);
  assert.equal(nodes.filter((node) => ["light", "dark"].includes(node.getAttribute("data-theme"))).length, 2);
  const controlPanels = nodes.filter((node) => node.tagName === "SECTION" && node.children[0]?.textContent === "Reusable host controls");
  assert.equal(controlPanels.length, 2);
  assert.ok(controlPanels.every((node) => node.children.length === 5));
  assert.equal(nodes.filter((node) => node.getAttribute("role") === "tablist").length, 2);
  assert.equal(nodes.filter((node) => node.getAttribute("role") === "tabpanel").length, 6);
  assert.equal(nodes.filter((node) => node.tagName === "DIALOG").length, 2);
  assert.equal(nodes.filter((node) => node.getAttribute("role") === "alert").length, 2);
  assert.equal(nodes.filter((node) => node.tagName === "TABLE").length, 2);
  assert.ok(nodes.some((node) => node.className?.includes("button primary")));
  const invalidEmail = nodes.find((node) => node.tagName === "INPUT" && node.getAttribute("aria-invalid") === "true");
  assert.ok(invalidEmail);
  assert.ok(nodes.some((node) => node.id === invalidEmail.getAttribute("aria-describedby")));
  assert.equal(nodes.filter((node) => node.tagName === "TH" && node.getAttribute("scope") === "col").length, 6);
  const styles = nodes.find((node) => node.tagName === "STYLE").textContent;
  assert.ok(styles.includes("@media(max-width:720px)"));
  assert.ok(styles.includes("prefers-reduced-motion"));
  assert.ok(styles.includes("var(--accent)"));
  const hostStyles = readFileSync(new URL("../public/styles.css", import.meta.url), "utf8");
  assert.ok(hostStyles.includes("[data-theme=\"light\"]"));
  assert.ok(hostStyles.includes(":focus-visible"));
  assert.ok(hostStyles.includes("--gold"));
  nodes.find((node) => node.textContent === "Primary action").click();
  assert.ok(nodes.some((node) => node.textContent === "Action received"));
  page.cleanup();
  const controls = nodes.filter((node) => node.className?.includes("plugin-control"));
  assert.ok(controls.every((node) => all(node).flatMap((item) => Object.values(item.listeners ?? {}).flat())
    .every((entry) => !entry.signal || entry.signal.aborted)));
});

test("keyboard tabs support arrows, Home, End, and dialogs return focus to the opener", () => {
  const page = setupShowcase();
  const nodes = all(page.container);
  const tabs = nodes.filter((node) => node.getAttribute("role") === "tab");
  tabs[0].dispatch("keydown", { key: "ArrowRight" });
  assert.equal(tabs[1].getAttribute("aria-selected"), "true");
  assert.equal(page.document.activeElement, tabs[1]);
  tabs[1].dispatch("keydown", { key: "End" });
  assert.equal(tabs[2].getAttribute("aria-selected"), "true");
  tabs[2].dispatch("keydown", { key: "Home" });
  assert.equal(tabs[0].getAttribute("aria-selected"), "true");
  tabs[0].dispatch("keydown", { key: "ArrowLeft" });
  assert.equal(tabs[2].getAttribute("aria-selected"), "true");
  assert.equal(page.document.activeElement, tabs[2]);
  const trigger = nodes.find((node) => node.textContent === "Open dialog");
  const dialog = nodes.find((node) => node.tagName === "DIALOG");
  trigger.click();
  assert.equal(dialog.open, true);
  nodes.find((node) => node.textContent === "Cancel").click();
  assert.equal(dialog.open, false);
  assert.equal(page.document.activeElement, trigger);
  page.cleanup();
});
