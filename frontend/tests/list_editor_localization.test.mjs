import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) { this.tagName = tag.toUpperCase(); this.children = []; this.attributes = new Map(); this.listeners = new Map(); this.value = ""; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = [...children]; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  dispatchEvent(event) { this.listeners.get(event.type)?.(event); return true; }
  textContent(value) {
    if (arguments.length) this.replaceChildren(String(value));
    else return this.children.map(child => typeof child === "string" ? child : child.textContent).join("");
  }
  set textContent(value) { this.replaceChildren(String(value)); }
  get textContent() { return this.children.map(child => typeof child === "string" ? child : child.textContent).join(""); }
}
class TextNode {
  constructor(value) { this.textContent = value; this.nodeType = 3; }
}
const root = new Element("html");
const nodes = [];
const document = {
  documentElement: root,
  createElement: tag => new Element(tag),
  createTextNode(value) { const node = new TextNode(value); nodes.push(node); return node; },
  createTreeWalker(_root, whatToShow) {
    assert.equal(whatToShow, 4);
    let index = 0;
    return { nextNode() { return nodes[index++] ?? null; } };
  },
  querySelectorAll(selector) {
    const attribute = selector.match(/^\[data-locale-(.+)\]$/)?.[1];
    if (!attribute) return [];
    const found = [];
    const visit = node => {
      if (typeof node === "string") return;
      if (node.attributes?.has(`data-locale-${attribute}`)) found.push(node);
      for (const child of node.children ?? []) visit(child);
    };
    visit(root);
    return found;
  },
};
const context = vm.createContext({
  document, NodeFilter: { SHOW_TEXT: 4 }, AbortController,
  Event: class { constructor(type, init = {}) { this.type = type; this.bubbles = init.bubbles ?? false; } },
});
const localeSource = ts.transpile(
  `${readFileSync("src/locale-catalogs.ts", "utf8")}\n${readFileSync("src/locale.ts", "utf8")}`,
  { target: ts.ScriptTarget.ES2022 },
);
vm.runInContext(localeSource, context);
for (const source of ["src/list_editor.ts", "src/steam_exclusion_picker.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }), context);
}

test("editor labels and row action attributes follow active locale changes", () => {
  const editor = context.Amaterasu.accessibleListEditor({
    label: "Settings",
    values: ["example"],
    formatValue: value => value,
    parseValue: value => value,
  });
  root.append(editor.element);
  const legend = editor.element.children[0];
  const input = editor.element.children[1].children[0].children[0];
  const remove = editor.element.children[1].children[0].children[1];
  assert.equal(legend.textContent, "Settings");
  assert.equal(input.getAttribute("aria-label"), "Settings, item 1");
  assert.equal(remove.getAttribute("aria-label"), "Remove Settings item 1");
  context.Amaterasu.applyLocale("fr");
  assert.equal(legend.textContent, "Paramètres");
  assert.equal(input.getAttribute("aria-label"), "Paramètres, item 1");
  assert.equal(remove.getAttribute("aria-label"), "Supprimer l’élément Paramètres 1");
});

test("Steam picker count and app labels follow live localized templates", async () => {
  context.Amaterasu.applyLocale("en");
  const picker = context.Amaterasu.steamExclusionPicker({
    allows: () => true,
    async post() {
      return { revision: 1, games: [{ app_id: 4567, name: "Example game", installed: true }], next_after: null, scanning: false, complete: true };
    },
  }, []);
  root.append(picker.node);
  await new Promise(resolve => setImmediate(resolve));
  const status = picker.node.children.find(child => child.getAttribute?.("role") === "status");
  const appId = picker.node.querySelectorAll?.("[aria-label]")?.find(child => child.getAttribute("aria-label") === "Steam app 4,567") ??
    (() => {
      const walk = node => {
        for (const child of node.children ?? []) {
          if (typeof child === "string") continue;
          if (child.tagName === "SMALL" && child.textContent.includes("Steam app")) return child;
          const found = walk(child); if (found) return found;
        }
        return null;
      };
      return walk(picker.node);
    })();
  assert.match(status.textContent, /1 matching loaded games/);
  assert.equal(appId.textContent, "Steam app 4,567");
  context.Amaterasu.applyLocale("fr");
  assert.match(status.textContent, /jeux chargés correspondants/);
  assert.equal(appId.textContent, "Application Steam 4 567");
  picker.dispose();
});

test("Steam no-permission guidance is live localized", () => {
  context.Amaterasu.applyLocale("en");
  const picker = context.Amaterasu.steamExclusionPicker({
    allows: () => false,
    async post() { throw new Error("must not request discoveries"); },
  }, [{ id: "999", name: "Unavailable" }]);
  root.append(picker.node);
  const status = picker.node.children.find(child => child.getAttribute?.("role") === "status");
  assert.match(status.textContent, /steam.read permission/);
  context.Amaterasu.applyLocale("fr");
  assert.match(status.textContent, /autorisations? steam.read/);
  assert.match(status.textContent, /restent modifiables/);
  picker.dispose();
});
