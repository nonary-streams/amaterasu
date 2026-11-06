import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag = "div", document = null) {
    this.tagName = tag.toUpperCase(); this.document = document; this.children = [];
    this.attributes = {}; this.listeners = {}; this.value = ""; this.checked = false;
    this.dataset = {};
    this.hidden = false; this.disabled = false; this.open = false; this.validity = { valid: true };
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  removeAttribute(name) { delete this.attributes[name]; }
  addEventListener(type, callback, options = {}) {
    (this.listeners[type] ??= []).push({ callback, signal: options.signal });
  }
  removeEventListener(type, callback) {
    this.listeners[type] = (this.listeners[type] ?? []).filter((entry) => entry.callback !== callback);
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
function setupShowcase({
  scriptUrl = "https://host.test/v1/extensions/showcase/" + "a".repeat(64) + "/assets/showcase.js",
  sdkOverrides = {},
  uiOverrideFactory = (ui) => ui,
  autoMount = true,
} = {}) {
  const registeredPages = [];
  const document = {
    activeElement: null,
    currentScript: null,
    head: new Element("head"),
    createElement(tag) { return new Element(tag, this); },
  };
  const context = vm.createContext({
    document, window: { AmaterasuExtensions: { v1: { register(_id, page) { registeredPages.push(page); } } } },
    AbortController, Error, Number, String, Object, Array, URL,
    location: { origin: "https://host.test" },
  });
  context.localizedTextNode = (text) => text;
  context.localeFormattedTextNode = (format) => format("en");
  context.translateText = (text) => text;
  context.textValueNode = (value) => typeof value === "string" ? value : value.value;
  const uiSource = ["ui.ts", "extension_ui.ts"]
    .map((file) => readFileSync(new URL("../src/" + file, import.meta.url), "utf8")).join("\n");
  vm.runInContext(ts.transpile(uiSource, {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None,
  }), context);
  const source = readFileSync(new URL("../../examples/web-extension/showcase.js", import.meta.url), "utf8");
  const loadShowcase = (sourceUrl) => {
    document.currentScript = { src: sourceUrl };
    vm.runInContext(source, context);
    return registeredPages.at(-1);
  };
  const lifecycle = new AbortController();
  const baseUi = context.Amaterasu.createExtensionUiV1(lifecycle.signal);
  const sdk = uiOverrideFactory({
    version: 1,
    features: ["ui.controls/1"],
    ui: baseUi,
    signal: lifecycle.signal,
    ...sdkOverrides,
  });
  const registration = loadShowcase(scriptUrl);
  const mountPage = (page = registration) => {
    const container = new Element("main", document);
    const cleanup = page.mount(container, sdk);
    return { container, cleanup };
  };
  const mounted = autoMount ? mountPage() : { container: new Element("main", document), cleanup: undefined };
  return {
    ...mounted,
    lifecycle,
    document,
    sdk,
    registration,
    loadShowcase,
    mountPage,
    get stylesheet() { return document.head.children[0]; },
  };
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
  assert.equal(page.stylesheet.rel, "stylesheet");
  assert.equal(page.stylesheet.href,
    "https://host.test/v1/extensions/showcase/" + "a".repeat(64) + "/assets/showcase.css");
  page.stylesheet.dispatch("load");
  assert.equal(nodes.find((node) => node.getAttribute("role") === "status").removed, true);
  const styles = readFileSync(new URL("../../examples/web-extension/showcase.css", import.meta.url), "utf8");
  assert.ok(styles.includes("@media (max-width: 720px)"));
  assert.ok(styles.includes("prefers-reduced-motion"));
  assert.ok(styles.includes("var(--accent)"));
  assert.ok(!nodes.some((node) => node.tagName === "STYLE"));
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

test("showcase reports a useful fallback when its signed stylesheet cannot load", () => {
  const page = setupShowcase();
  page.stylesheet.dispatch("error");
  const status = all(page.container).find((node) => node.getAttribute("role") === "status");
  assert.ok(status.textContent.includes("host styles remain available"));
  const featureFallback = setupShowcase({ scriptUrl: "" });
  assert.equal(featureFallback.stylesheet, undefined);
  assert.ok(all(featureFallback.container).some((node) =>
    node.getAttribute("role") === "status" &&
    node.textContent.includes("host styles remain available")));
  page.cleanup();
  featureFallback.cleanup();
  assert.equal(page.stylesheet.removed, true);
});
test("older hosts get a native fallback before styles or SDK controls are mounted", () => {
  const oldHostVersions = [
    { sdkOverrides: { version: 2 } },
    { sdkOverrides: { ui: { version: 2 } } },
    { sdkOverrides: { features: [] } },
  ];
  for (const options of oldHostVersions) {
    const page = setupShowcase(options);
    assert.equal(page.stylesheet, undefined);
    const nodes = all(page.container);
    assert.ok(nodes.some((node) => node.textContent.includes("Basic component showcase")));
    assert.equal(nodes.filter((node) => node.className?.includes("plugin-control")).length, 0);
    const button = nodes.find((node) => node.textContent === "Try fallback button");
    button.click();
    assert.ok(nodes.some((node) => node.textContent === "Fallback button works."));
    page.cleanup();
  }
});

test("fallback is disposed by the SDK signal without an explicit cleanup call", () => {
  const page = setupShowcase({ sdkOverrides: { features: [] } });
  const root = page.container.children[0];
  assert.ok(root);
  page.lifecycle.abort();
  assert.equal(root.removed, true);
  assert.equal(page.stylesheet, undefined);
});

test("already-aborted old host mounts neither fallback DOM nor stylesheet", () => {
  const page = setupShowcase({
    sdkOverrides: { features: [] },
    autoMount: false,
  });
  page.lifecycle.abort();
  const mounted = page.mountPage(page.registration);
  assert.equal(mounted.container.children.length, 0);
  assert.equal(page.stylesheet, undefined);
});

test("sequential signed digest versions register independently in one page context", () => {
  const first = setupShowcase();
  const secondRegistration = first.loadShowcase(
    "https://host.test/v1/extensions/showcase/" + "b".repeat(64) + "/assets/showcase.js");
  const second = first.mountPage(secondRegistration);
  assert.ok(first.stylesheet.href.includes("/" + "a".repeat(64) + "/assets/showcase.css"));
  const secondStylesheet = first.document.head.children[1];
  assert.ok(secondStylesheet.href.includes("/" + "b".repeat(64) + "/assets/showcase.css"));
  first.cleanup();
  second.cleanup();
  assert.equal(first.stylesheet.removed, true);
  assert.equal(secondStylesheet.removed, true);
});

test("navigation before stylesheet load cancels its callback and removes the pending link", () => {
  const page = setupShowcase();
  const root = page.container.children[0];
  const stylesheet = page.stylesheet;
  page.lifecycle.abort();
  assert.equal(stylesheet.removed, true);
  assert.equal(root.removed, true);
  assert.equal(stylesheet.listeners.load.length, 0);
  assert.equal(stylesheet.listeners.error.length, 0);
  stylesheet.dispatch("load");
  assert.notEqual(root.dataset.stylesheetState, "loaded");
  page.cleanup();
});

test("partial SDK factory failure disposes prior controls and removes the pending stylesheet", () => {
  let disposedControls = 0;
  const page = setupShowcase({
    autoMount: false,
    uiOverrideFactory: (sdk) => ({
      ...sdk,
      ui: {
        ...sdk.ui,
        toggle(options) {
          const control = sdk.ui.toggle(options);
          return {
            ...control,
            dispose() {
              disposedControls += 1;
              control.dispose();
            },
          };
        },
        select() { throw new Error("test control factory failure"); },
      },
    }),
  });
  assert.throws(() => page.mountPage(page.registration), /test control factory failure/);
  assert.equal(disposedControls, 1);
  assert.equal(page.container.children.length, 0);
  assert.equal(page.stylesheet.removed, true);
  assert.equal(page.stylesheet.listeners.load.length, 0);
  assert.equal(page.stylesheet.listeners.error.length, 0);
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
