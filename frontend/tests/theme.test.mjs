import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor() { this.children = []; this.attributes = new Map(); this.listeners = new Map(); }
  append(...children) { this.children.push(...children); }
  setAttribute(name, value) { this.attributes.set(name, value); }
  addEventListener(name, handler) { this.listeners.set(name, handler); }
  click() { this.listeners.get("click")?.(); }
}
function createContext({ stored = null, storageFails = false, systemLight = false } = {}) {
  const root = new Element();
  const media = { matches: systemLight, listeners: new Map(), addEventListener(name, handler) { this.listeners.set(name, handler); } };
  let value = stored;
  const context = vm.createContext({
    document: { documentElement: root, createElement: () => new Element() },
    window: { matchMedia: () => media },
    localStorage: {
      getItem() { if (storageFails) throw new Error("denied"); return value; },
      setItem(_key, next) { if (storageFails) throw new Error("denied"); value = next; },
    },
  });
  for (const source of ["src/ui.ts", "src/theme.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  context.Amaterasu.setLocalizedAttribute = (node, name, text) => node.setAttribute(name, text);
  context.setLocalizedAttribute = context.Amaterasu.setLocalizedAttribute;
  return { context, root, media, saved: () => value };
}

test("theme follows system until an explicit choice is made and persists that choice", () => {
  const { context, root, media, saved } = createContext({ systemLight: true });
  const theme = new context.Amaterasu.ThemeController();
  assert.equal(root.attributes.get("data-theme"), "light");
  const picker = theme.mount();
  const controls = picker.children[1].children;
  assert.equal(controls[2].attributes.get("aria-pressed"), "true");
  controls[1].click();
  assert.equal(saved(), "dark");
  assert.equal(root.attributes.get("data-theme"), "dark");
  media.matches = true;
  media.listeners.get("change")();
  assert.equal(root.attributes.get("data-theme"), "dark");
  controls[2].click();
  assert.equal(root.attributes.get("data-theme"), "light");
  media.matches = false;
  media.listeners.get("change")();
  assert.equal(root.attributes.get("data-theme"), "dark");
});

test("invalid or inaccessible preferences fall back to the system theme", () => {
  for (const options of [{ stored: "unexpected", systemLight: true }, { storageFails: true, systemLight: true }]) {
    const { context, root } = createContext(options);
    const theme = new context.Amaterasu.ThemeController();
    assert.equal(root.attributes.get("data-theme"), "light");
    const controls = theme.mount().children[1].children;
    controls[1].click();
    assert.equal(root.attributes.get("data-theme"), "dark");
  }
});
