import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class TextNode {
  constructor(value) { this.nodeType = 3; this.data = value; this.parentNode = null; }
  get textContent() { return this.data; }
  set textContent(value) { this.data = String(value); }
}

class Element {
  constructor(tag) {
    this.nodeType = 1;
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.attributes = {};
    this.parentNode = null;
    this.id = "";
    this.className = "";
    this.tabIndex = -1;
  }
  get childNodes() { return this.children; }
  get textContent() { return this.children.map((child) => child.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(new TextNode(String(value))); }
  append(...children) {
    for (const child of children) {
      if (!child || typeof child !== "object" || typeof child.nodeType !== "number") throw new TypeError("Node required");
      child.parentNode?.removeChild?.(child);
      child.parentNode = this;
      this.children.push(child);
    }
  }
  replaceChildren(...children) {
    for (const child of this.children) child.parentNode = null;
    this.children = [];
    this.append(...children);
  }
  removeChild(child) {
    this.children = this.children.filter((candidate) => candidate !== child);
    child.parentNode = null;
  }
  remove() { this.parentNode?.removeChild(this); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return this.attributes[name] ?? null; }
  removeAttribute(name) { delete this.attributes[name]; }
}

function setup(locale = "fr-CA") {
  const document = {
    createElement(tag) { return new Element(tag); },
    createTextNode(value) { return new TextNode(value); },
  };
  const context = vm.createContext({
    document,
    Node: { ELEMENT_NODE: 1, TEXT_NODE: 3, COMMENT_NODE: 8, DOCUMENT_FRAGMENT_NODE: 11 },
    AbortController,
  });
  context.translateText = (value) => `host:${value}`;
  context.activeIntlLocale = () => locale;
  context.localizedTextNode = (value) => document.createTextNode(context.translateText(value));
  context.localeFormattedTextNode = (format) => document.createTextNode(format(locale));
  const source = ["ui.ts", "extension_ui.ts", "extension_ui_layout.ts"]
    .map((file) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"))
    .join("\n");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  context.Amaterasu.setLocalizedAttribute = (element, attribute, value) => element.setAttribute(attribute, context.translateText(value));
  context.Amaterasu.setLocalizedAttributeFormatter = (element, attribute, format) => element.setAttribute(attribute, format(locale));
  const lifecycle = new AbortController();
  return { document, context, lifecycle, layout: context.Amaterasu.createExtensionUiLayoutV1(lifecycle.signal) };
}

test("card creates a labelled native section, accepts only DOM children, and disposes with its lifecycle", () => {
  const { document, lifecycle, layout } = setup();
  const card = layout.card({ label: { source: "Project summary", translations: { fr: "Résumé du projet" } } });
  assert.equal(card.element.tagName, "SECTION");
  assert.equal(card.element.className, "panel plugin-ui-card");
  assert.equal(card.element.getAttribute("aria-labelledby"), card.element.children[0].id);
  assert.equal(card.element.children[0].tagName, "H2");
  assert.equal(card.element.children[0].textContent, "Résumé du projet");

  const host = document.createElement("main");
  host.append(card.element);
  const pluginContent = document.createElement("p");
  pluginContent.textContent = "<img src=x onerror=alert(1)>";
  card.append(pluginContent);
  assert.equal(card.content.children[0], pluginContent);
  assert.equal(card.content.textContent, "<img src=x onerror=alert(1)>");
  assert.throws(() => card.append("<img src=x>"), /DOM nodes/);

  lifecycle.abort();
  assert.equal(host.children.length, 0);
  assert.equal(card.content.children.length, 0);
  card.append(pluginContent);
  card.dispose();
  assert.equal(card.content.children.length, 0);
  assert.throws(() => layout.card({ label: "Late page" }), /no longer active/);
});

test("table provides a caption, scoped headers, localized text cells, responsive region, and safe row updates", () => {
  const { lifecycle, layout } = setup();
  const table = layout.table({
    caption: { source: "Build status", translations: { fr: "État de compilation" } },
    columns: [
      { header: { source: "Component", translations: { fr: "Composant" } } },
      { header: "State" },
    ],
    rows: [["UI", "Ready"]],
  });
  assert.equal(table.element.tagName, "DIV");
  assert.equal(table.element.getAttribute("role"), "region");
  assert.equal(table.element.tabIndex, 0);
  assert.equal(table.element.getAttribute("aria-label"), "État de compilation");
  const tableElement = table.element.children[0];
  assert.equal(tableElement.tagName, "TABLE");
  assert.equal(tableElement.children[0].tagName, "CAPTION");
  assert.equal(tableElement.children[0].textContent, "État de compilation");
  const headers = tableElement.children[1].children[0].children;
  assert.deepEqual(headers.map((header) => header.textContent), ["Composant", "host:State"]);
  assert.ok(headers.every((header) => header.tagName === "TH" && header.getAttribute("scope") === "col"));
  const body = tableElement.children[2];
  assert.equal(body.children[0].children[0].textContent, "host:UI");

  table.setRows([["<script>unsafe()</script>", { source: "Complete", translations: { fr: "Terminé" } }]]);
  assert.equal(body.children[0].children[0].children[0].nodeType, 3);
  assert.equal(body.children[0].children[0].textContent, "host:<script>unsafe()</script>");
  assert.equal(body.children[0].children[1].textContent, "Terminé");
  assert.throws(() => table.setRows([["missing cell"]]), /table rows/);
  assert.equal(body.children[0].children[1].textContent, "Terminé");

  lifecycle.abort();
  assert.equal(table.element.parentNode, null);
  assert.equal(tableElement.children.length, 0);
  table.setRows([["ignored", "after disposal"]]);
  assert.equal(tableElement.children.length, 0);
});

test("table validates labels, row shapes, and bounded dimensions before rendering", () => {
  const { layout } = setup();
  assert.throws(() => layout.card({ label: "  " }), /card label/);
  assert.throws(() => layout.table({ caption: "Summary", columns: [], rows: [] }), /columns/);
  assert.throws(() => layout.table({ caption: "Summary", columns: [{ header: "One" }], rows: [["one", "extra"]] }), /table rows/);
  assert.throws(() => layout.table({ caption: "Summary", columns: [{ header: "One" }], rows: null }), /table rows/);
  assert.throws(() => layout.table({ caption: "Summary", columns: Array.from({ length: 33 }, () => ({ header: "Column" })) }), /columns/);
  assert.throws(() => layout.table({ caption: "Summary", columns: [{ header: "One" }], rows: Array.from({ length: 1001 }, () => ["row"]) }), /table rows/);
  assert.throws(() => layout.table({ caption: { source: "Caption", translations: { fr: " " } }, columns: [{ header: "One" }] }), /translations/);
});
