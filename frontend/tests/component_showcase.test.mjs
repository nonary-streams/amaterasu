import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag = "div", document = null) {
    this.tagName = tag.toUpperCase(); this.nodeName = this.tagName; this.nodeType = 1;
    this.ownerDocument = document; this.children = [];
    this.attributes = {}; this.listeners = {}; this.value = ""; this.checked = false;
    this.dataset = {};
    this.hidden = false; this.disabled = false; this.open = false; this.validity = { valid: true };
    this.classList = { add: (name) => { this.className = [this.className, name].filter(Boolean).join(" "); } };
  }
  get childNodes() { return this.children; }
  get firstElementChild() { return this.children.find((child) => child instanceof Element); }
  get isConnected() { return !this.removed && (this.parentElement?.isConnected ?? true); }
  append(...children) { for (const child of children) this.insertBefore(child, null); }
  insertBefore(child, next) {
    if (child instanceof Element) {
      if (child.parentElement) child.remove();
      child.parentElement = this;
      child.removed = false;
    }
    this.children.splice(next === null ? this.children.length : this.children.indexOf(next), 0, child);
  }
  replaceChildren(...children) {
    for (const child of [...this.children]) if (child instanceof Element) child.remove();
    this.children = [];
    this.append(...children);
  }
  contains(node) { return all(this).includes(node); }
  closest(selector) {
    return selector === '[role="tab"]' && this.getAttribute("role") === "tab"
      ? this : this.parentElement?.closest(selector) ?? null;
  }
  querySelectorAll(tag) { return all(this).slice(1).filter((node) => node.tagName.toLowerCase() === tag); }
  querySelector(selector) {
    return all(this).slice(1).find((node) =>
      selector.startsWith(".") && (node.className || node.getAttribute("class") || "")
        .split(/\s+/).includes(selector.slice(1))) ?? null;
  }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  hasAttribute(name) { return Object.hasOwn(this.attributes, name); }
  removeAttribute(name) { delete this.attributes[name]; }
  addEventListener(type, callback, options = {}) {
    (this.listeners[type] ??= []).push({ callback, signal: options.signal });
  }
  removeEventListener(type, callback) {
    this.listeners[type] = (this.listeners[type] ?? []).filter((entry) => entry.callback !== callback);
  }
  dispatch(type, values = {}) {
    const event = { type, target: this, preventDefault() { this.defaultPrevented = true; }, ...values };
    for (let node = this; node; node = node.parentElement) {
      for (const entry of node.listeners[type] ?? []) if (!entry.signal?.aborted) entry.callback(event);
      if (type !== "click" && type !== "keydown") break;
    }
  }
  click() { this.dispatch("click"); }
  focus() { if (this.ownerDocument) this.ownerDocument.activeElement = this; }
  showModal() { this.open = true; }
  close(returnValue) { this.returnValue = returnValue; this.open = false; this.dispatch("close"); }
  remove() {
    if (this.parentElement) {
      const siblings = this.parentElement.children;
      siblings.splice(siblings.indexOf(this), 1);
      this.parentElement = null;
    }
    this.removed = true;
  }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child.textContent ?? "").join(""); }
  set textContent(value) { this.children = [String(value)]; }
}
class Button extends Element {}

const uiFeatures = ["ui.controls/1", "ui.controls/2", "ui.controls/3", "ui.forms/2", "ui.layout/1",
  "ui.elements/1", "ui.feedback/1", "ui.tabs/1", "ui.dialog/1", "ui.dialog-actions/1", "ui.charts/1",
  "ui.summary/1", "ui.sparklines/1"];
const uiSource = ["ui.ts", "extension_ui.ts", "extension_ui_layout.ts", "extension_ui_feedback.ts",
  "extension_ui_dialog.ts", "extension_ui_elements.ts", "extension_ui_tabs.ts", "extension_ui_charts.ts",
  "extension_ui_summary.ts"]
  .map((file) => readFileSync(new URL("../src/" + file, import.meta.url), "utf8")).join("\n");
const compiledUi = ts.transpile(uiSource, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None });

function setupShowcase({
  scriptUrl = "https://host.test/v1/extensions/showcase/" + "a".repeat(64) + "/assets/showcase.js",
  sdkOverrides = {},
  uiOverrideFactory = (ui) => ui,
  autoMount = true,
} = {}) {
  const registeredPages = [];
  const document = {
    activeElement: null,
    body: new Element("body"),
    currentScript: null,
    head: new Element("head"),
    createElement(tag) { return tag === "button" ? new Button(tag, this) : new Element(tag, this); },
    createElementNS(_namespace, tag) { return this.createElement(tag); },
    createTextNode(text) { return String(text); },
  };
  const stylesheetLinks = [];
  const appendToHead = document.head.append.bind(document.head);
  document.head.append = (node) => {
    stylesheetLinks.push(node);
    appendToHead(node);
  };
  const context = vm.createContext({
    document, window: { AmaterasuExtensions: { v1: { register(_id, page) { registeredPages.push(page); } } } },
    AbortController, Error, Number, String, Object, Array, URL,
    HTMLElement: Element, HTMLButtonElement: Button, Element,
    Node: { ELEMENT_NODE: 1, TEXT_NODE: 3, COMMENT_NODE: 8, DOCUMENT_FRAGMENT_NODE: 11 },
    MutationObserver: class { observe() {} disconnect() {} },
    getComputedStyle: () => ({ direction: "ltr" }),
    location: { origin: "https://host.test" },
  });
  context.localizedTextNode = (text) => text;
  context.localeFormattedTextNode = (format) => format("en");
  context.translateText = (text) => text;
  context.textValueNode = (value) => typeof value === "string" ? value : value.value;
  vm.runInContext(compiledUi, context);
  context.Amaterasu.setLocalizedAttribute = (node, name, value) => node.setAttribute(name, value);
  const source = readFileSync(new URL("../../examples/web-extension/showcase.js", import.meta.url), "utf8");
  const loadShowcase = (sourceUrl) => {
    document.currentScript = { src: sourceUrl };
    vm.runInContext(source, context);
    return registeredPages.at(-1);
  };
  const lifecycle = new AbortController();
  const host = context.Amaterasu;
  const ui = {
    ...host.createExtensionUiWithDialogsV1(lifecycle.signal),
    ...host.createExtensionUiControlsV3(lifecycle.signal),
    ...host.createExtensionUiElementsV1(lifecycle.signal),
    ...host.createExtensionUiTabsV1(lifecycle.signal),
    ...host.createExtensionUiChartsV1(lifecycle.signal),
    ...host.createExtensionUiSummaryV1(lifecycle.signal),
  };
  const sdk = uiOverrideFactory({
    version: 1,
    features: uiFeatures,
    ui,
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
    get stylesheet() { return stylesheetLinks[0]; },
  };
}
function all(node) {
  return [node, ...(node.children ?? []).flatMap((child) => child instanceof Element ? all(child) : [])];
}

test("Component showcase renders shared SDK primitives in both appearances", () => {
  const page = setupShowcase();
  const nodes = all(page.container);
  assert.equal(nodes.filter((node) => ["light", "dark"].includes(node.getAttribute("data-theme"))).length, 2);
  const sampleBoards = nodes.filter((node) => node.className === "showcase-theme amaterasu-paper");
  assert.ok(sampleBoards.every((board) => board.children.filter((node) => node.dataset.panel).length === 15));
  assert.equal(nodes.filter((node) => node.getAttribute("role") === "tablist").length, 3);
  assert.equal(nodes.filter((node) => node.getAttribute("role") === "tabpanel").length, 10);
  assert.equal(nodes.filter((node) => node.tagName === "DIALOG").length, 2);
  assert.equal(nodes.filter((node) => node.className?.includes("plugin-feedback-alert")).length, 8);
  assert.equal(nodes.filter((node) => node.tagName === "TABLE").length, 2);
  assert.ok(nodes.some((node) => node.className?.includes("button primary")));
  assert.ok(nodes.some((node) => node.tagName === "INPUT" && node.type === "search"));
  assert.ok(nodes.some((node) => node.tagName === "INPUT" && node.type === "checkbox" && !node.hasAttribute("role")));
  assert.ok(nodes.some((node) => node.getAttribute("aria-label") === "Edit profile"));
  assert.equal(nodes.filter((node) => node.getAttribute("role") === "tooltip").length, 10);
  assert.equal(nodes.filter((node) => node.className === "badge plugin-badge").length, 8);
  assert.equal(nodes.filter((node) => node.tagName === "PROGRESS").length, 2);
  assert.equal(nodes.filter((node) => node.className?.includes("plugin-metric-chart-sparkline")).length, 8);
  assert.equal(nodes.filter((node) => node.className?.includes("ink-status-card")).length, 2);
  assert.equal(nodes.filter((node) => node.className?.includes("ink-info-card")).length, 6);
  const requiredField = nodes.find((node) => node.tagName === "INPUT" && node.getAttribute("placeholder") === "Device name");
  assert.equal(requiredField.required, true);
  requiredField.value = " ";
  requiredField.dispatch("input");
  assert.equal(requiredField.getAttribute("aria-invalid"), "true");
  assert.ok(all(page.container).some((node) => node.id === requiredField.getAttribute("aria-describedby")));
  requiredField.value = "Living room PC";
  requiredField.dispatch("input");
  assert.equal(requiredField.getAttribute("aria-invalid"), null);
  assert.equal(nodes.filter((node) => node.tagName === "TH" && node.getAttribute("scope") === "col").length, 8);
  assert.equal(page.stylesheet.rel, "stylesheet");
  assert.equal(page.stylesheet.href,
    "https://host.test/v1/extensions/showcase/" + "a".repeat(64) + "/assets/showcase.css");
  page.stylesheet.dispatch("load");
  assert.equal(nodes.find((node) => node.className === "showcase-style-status").removed, true);
  const styles = readFileSync(new URL("../../examples/web-extension/showcase.css", import.meta.url), "utf8");
  assert.ok(styles.includes("@container (max-width: 530px)"));
  assert.ok(styles.includes("var(--surface-radius)"));
  assert.ok(!nodes.some((node) => node.tagName === "STYLE"));
  const hostStyles = readFileSync(new URL("../public/styles.css", import.meta.url), "utf8");
  assert.ok(hostStyles.includes("[data-theme=\"light\"]"));
  assert.ok(hostStyles.includes(":focus-visible"));
  assert.ok(hostStyles.includes("--gold"));
  nodes.find((node) => node.tagName === "BUTTON" && node.textContent === "Primary").click();
  assert.ok(nodes.some((node) => node.textContent === "Action received."));
  const light = nodes.find((node) => node.getAttribute("data-theme") === "light");
  const controlsInLight = all(light);
  const search = controlsInLight.find((node) => node.tagName === "INPUT" && node.type === "search");
  search.value = "steam";
  search.dispatch("input");
  const table = controlsInLight.find((node) => node.tagName === "TABLE");
  assert.match(table.textContent, /Steam Deck/);
  assert.doesNotMatch(table.textContent, /Living room TV/);
  assert.ok(controlsInLight.some((node) => node.textContent === "1 device shown."));
  const info = controlsInLight.find((node) => node.getAttribute("data-dismissible") !== null);
  const dismiss = all(info).find((node) => node.getAttribute("aria-label") === "Dismiss alert");
  dismiss.click();
  assert.equal(info.hidden, true);
  page.cleanup();
  const controls = nodes.filter((node) => node.className?.includes("plugin-control"));
  assert.ok(controls.every((node) => all(node).flatMap((item) => Object.values(item.listeners ?? {}).flat())
    .every((entry) => !entry.signal || entry.signal.aborted)));
});

test("the modern showcase delegates signed styles to the host asset SDK", async () => {
  const paths = [];
  let resolveStylesheet;
  const page = setupShowcase({
    sdkOverrides: {
      features: [...uiFeatures, "ui.assets/1"],
      assets: { stylesheet(path) {
        paths.push(path);
        return new Promise((resolve) => { resolveStylesheet = resolve; });
      } },
    },
  });
  assert.deepEqual(paths, ["showcase.css"]);
  assert.equal(page.stylesheet, undefined);
  const root = page.container.children[0];
  page.lifecycle.abort();
  resolveStylesheet();
  await Promise.resolve();
  assert.equal(root.removed, true);
  assert.notEqual(root.dataset.stylesheetState, "loaded");
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
    { sdkOverrides: { features: ["ui.controls/1"] } },
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
  let createdToggles = 0;
  const page = setupShowcase({
    autoMount: false,
    uiOverrideFactory: (sdk) => ({
      ...sdk,
      ui: {
        ...sdk.ui,
        toggle(options) {
          if (++createdToggles > 1) throw new Error("test control factory failure");
          const control = sdk.ui.toggle(options);
          return {
            ...control,
            dispose() {
              disposedControls += 1;
              control.dispose();
            },
          };
        },
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
  const lightBoard = all(page.container).find((node) => node.getAttribute("data-theme") === "light");
  const nodes = all(lightBoard);
  const tabs = nodes.filter((node) => node.getAttribute("role") === "tab");
  tabs[0].dispatch("keydown", { key: "ArrowRight" });
  assert.equal(tabs[1].getAttribute("aria-selected"), "true");
  assert.ok(page.document.activeElement === tabs[1], "ArrowRight should focus the next settings tab");
  tabs[1].dispatch("keydown", { key: "End" });
  assert.equal(tabs[3].getAttribute("aria-selected"), "true");
  tabs[3].dispatch("keydown", { key: "Home" });
  assert.equal(tabs[0].getAttribute("aria-selected"), "true");
  tabs[0].dispatch("keydown", { key: "ArrowLeft" });
  assert.equal(tabs[3].getAttribute("aria-selected"), "true");
  assert.ok(page.document.activeElement === tabs[3], "ArrowLeft should wrap to the final settings tab");
  const trigger = nodes.find((node) => node.tagName === "BUTTON" && node.textContent === "Open dialog");
  const dialog = nodes.find((node) => node.tagName === "DIALOG");
  trigger.focus();
  trigger.click();
  assert.equal(dialog.open, true);
  const actions = all(dialog).find((node) => node.className?.includes("plugin-dialog-actions"));
  const dialogButtons = all(actions).filter((node) => node.tagName === "BUTTON");
  assert.deepEqual(dialogButtons.map((button) => button.textContent).sort(), ["Cancel", "Confirm"]);
  dialogButtons.find((button) => button.textContent === "Cancel").click();
  assert.equal(dialog.open, false);
  assert.ok(page.document.activeElement === trigger, "Cancel should return focus to the dialog opener");
  page.cleanup();
});
