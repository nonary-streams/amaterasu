import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const documentState = { activeElement: null };

class Node {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.listeners = new Map();
    this.attributes = new Map();
    this.textContent = "";
    this.value = "";
    this.type = "";
    this.checked = false;
    this.hidden = false;
    this.disabled = false;
    this.className = "";
    this.autocomplete = "";
    this.parentNode = null;
    this.selectionStart = 0;
    this.selectionEnd = 0;
  }
  append(...children) {
    for (const child of children) {
      if (typeof child !== "string") child.parentNode = this;
      this.children.push(child);
    }
  }
  replaceChildren(...children) {
    for (const child of this.children) if (typeof child !== "string") child.parentNode = null;
    this.children = [];
    this.append(...children);
    this.textContent = children.map(child => typeof child === "string" ? child : child.textContent ?? "").join("");
  }
  contains(target) { return target === this || this.children.some(child => typeof child !== "string" && child.contains(target)); }
  setAttribute(name, value) { this.attributes.set(name, value); }
  removeAttribute(name) { this.attributes.delete(name); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  focus() { documentState.activeElement = this; }
  setSelectionRange(start, end) { this.selectionStart = start; this.selectionEnd = end; }
  click() { this.listeners.get("click")?.({ type: "click" }); }
  dispatchEvent(event) { this.listeners.get(event.type)?.(event); return true; }
  removeEventListener() {}
}

const ctx = {
  AbortController,
  Event: class { constructor(type, init = {}) { this.type = type; this.bubbles = init.bubbles ?? false; } },
  Amaterasu: {
    localizedMessage: value => ({ kind: "localized", value }),
    literalText: value => ({ kind: "literal", value }),
    textValueNode: value => typeof value === "string" ? value : value.value,
    setLocalizedTextContent(target, source) { target.textContent = source; target.setAttribute("data-locale-text", source); },
    translateText: source => source,
    localeFormattedTextNode(format) { return Object.assign(new Node("#text"), { textContent: format("en") }); },
    setLocalizedAttribute(target, attribute, source) { target.setAttribute(attribute, source); target.setAttribute(`data-locale-${attribute}`, source); },
    setLocalizedAttributeTemplate(target, attribute, source, values) {
      const rendered = source.replaceAll(/\{(\w+)\}/g, (_, name) => values[name].value);
      target.setAttribute(attribute, rendered);
      target.setAttribute(`data-locale-${attribute}`, source);
    },
  },
  document: {
    get activeElement() { return documentState.activeElement; },
    createElement: tag => new Node(tag),
    createTextNode: source => Object.assign(new Node("#text"), { textContent: source }),
  },
};
vm.createContext(ctx);
vm.runInContext(ts.transpile(readFileSync("src/steam_exclusion_picker.ts", "utf8"), {
  target: ts.ScriptTarget.ES2022,
}), ctx);

const create = (initial, api, onChange = () => {}) =>
  ctx.Amaterasu.steamExclusionPicker(api, initial, onChange);
const tick = async () => { for (let i = 0; i < 30; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const all = (node, predicate) => [
  ...(predicate(node) ? [node] : []),
  ...node.children.flatMap(child => typeof child === "string" ? [] : all(child, predicate)),
];
const find = (node, predicate) => all(node, predicate)[0];

test("paginates by 32, preserves name-only and unavailable exclusions, and selects discoveries", async () => {
  const calls = [];
  const api = {
    allows: scope => scope === "steam.read",
    async post(_route, params) {
      calls.push(params);
      return calls.length === 1
        ? { revision: 7, games: [{ app_id: 10, name: "A Short Hike", installed: true }], next_after: 32, scanning: false, complete: false }
        : { revision: 7, games: [{ app_id: 11, name: "Portal 2", installed: true }], next_after: null, scanning: false, complete: true };
    },
  };
  const initial = [{ name: "Retired title" }, { id: "999", name: "No longer installed" }];
  const changes = [];
  const picker = create(initial, api, value => changes.push(value));
  await tick();
  const status = picker.node.children[5];
  assert.equal(status.textContent, "1 matching loaded games · 2 exclusions");
  picker.node.children[7].click();
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [
    { limit: 32 },
    { revision: 7, after: 32, limit: 32 },
  ]);
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), initial);
  const box = find(picker.node, node => node.tagName === "input" && node.type === "checkbox");
  box.checked = true;
  box.listeners.get("change")?.();
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), [...initial, { id: "10", name: "A Short Hike" }]);
  assert.equal(changes.length, 1);
  assert.equal(status.attributes.get("aria-label"), "2 matching loaded games; 3 exclusions");
});

test("restarts a changed snapshot without losing the draft", async () => {
  const calls = [];
  let n = 0;
  const api = {
    allows: () => true,
    async post(_route, params) {
      calls.push(params);
      n += 1;
      if (n === 1) return { revision: 4, games: [{ app_id: 1, name: "One" }], next_after: 8, scanning: false, complete: true };
      if (n === 2) return { revision: 5, games: [], next_after: null, scanning: true, complete: false };
      return { revision: 5, games: [{ app_id: 2, name: "Two" }], next_after: null, scanning: false, complete: true };
    },
  };
  const picker = create([{ name: "Name only" }], api);
  await tick();
  picker.node.children[7].click();
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(calls)), [
    { limit: 32 },
    { revision: 4, after: 8, limit: 32 },
    { limit: 32 },
  ]);
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), [{ name: "Name only" }]);
});

test("bounds repeated revision_conflict retries and offers a manual retry", async () => {
  let conflictCalls = 0;
  const conflictPicker = create([], {
    allows: () => true,
    async post() {
      conflictCalls += 1;
      throw Object.assign(new Error("stale cursor"), { code: "revision_conflict" });
    },
  });
  await tick();
  assert.equal(conflictCalls, 3);
  assert.match(conflictPicker.node.children[5].textContent, /could not be loaded/);
  assert.equal(conflictPicker.node.children[7].hidden, false);
  assert.equal(conflictPicker.node.children[7].textContent, "Retry loading games");
  conflictPicker.node.children[7].click();
  await tick();
  assert.equal(conflictCalls, 6);

  let mismatchCalls = 0;
  const mismatchPicker = create([], {
    allows: () => true,
    async post(_route, params) {
      mismatchCalls += 1;
      if (mismatchCalls === 1) return { revision: 1, games: [], next_after: 1, scanning: false, complete: true };
      return { revision: 2, games: [], next_after: 1, scanning: false, complete: true };
    },
  });
  await tick();
  mismatchPicker.node.children[7].click();
  await tick();
  assert.equal(mismatchCalls, 3);
  assert.equal(mismatchPicker.node.children[5].textContent, "0 matching loaded games · 0 exclusions");
  assert.equal(mismatchPicker.node.children[7].hidden, false);
});

test("saved exclusions can be edited and removed without steam.read", async () => {
  const initial = [{ name: "Retired title" }, { id: "999", name: "Unavailable" }];
  const changes = [];
  const picker = create(initial, { allows: () => false, async post() { throw new Error("must not call"); } },
    value => changes.push(value));
  await tick();
  const nameInput = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Edit name-only Steam exclusion");
  assert.ok(nameInput, JSON.stringify(all(picker.node, node => node.tagName === "input").map(node => [node.type, node.attributes.get("aria-label")])));
  nameInput.value = "Renamed retired title";
  nameInput.listeners.get("input")?.();
  assert.equal(picker.value()[0].name, "Renamed retired title");
  const remove = find(picker.node, node => node.tagName === "button" && node.textContent === "Remove exclusion");
  remove.click();
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), [{ id: "999", name: "Unavailable" }]);
  assert.equal(changes.length, 2);
});

test("selection limit is 256 while saved values remain removable", async () => {
  const initial = Array.from({ length: 256 }, (_, i) => ({ name: `Saved ${i}` }));
  const picker = create(initial, {
    allows: () => true,
    async post() {
      return { revision: 1, games: [{ app_id: 10, name: "New game" }], next_after: null, scanning: false, complete: true };
    },
  });
  await tick();
  const box = find(picker.node, node => node.tagName === "input" && node.type === "checkbox");
  box.checked = true;
  box.listeners.get("change")?.();
  assert.equal(picker.value().length, 256);
  assert.equal(box.checked, false);
  const remove = find(picker.node, node => node.tagName === "button" && node.textContent === "Remove exclusion");
  remove.click();
  const refreshedBox = find(picker.node, node => node.tagName === "input" && node.type === "checkbox");
  refreshedBox.checked = true;
  refreshedBox.listeners.get("change")?.();
  assert.equal(picker.value().length, 256);
  assert.equal(picker.value().at(-1).id, "10");
});

test("disposed picker ignores outstanding list responses and clears its view", async () => {
  let resolve;
  const picker = create([], {
    allows: () => true,
    post: () => new Promise(done => { resolve = done; }),
  });
  picker.dispose();
  resolve({ revision: 1, games: [{ app_id: 1, name: "Late" }], next_after: null, scanning: false, complete: true });
  await tick();
  assert.equal(picker.node.children.length, 0);
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), []);
});

test("steam.read unavailable shows an access message", async () => {
  const picker = create([{ id: "42", name: "Preserved" }], {
    allows: () => false,
    async post() { throw new Error("must not call"); },
  });
  await tick();
  assert.match(picker.node.children[5].textContent, /steam.read permission/);
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), [{ id: "42", name: "Preserved" }]);
});

test("manual add edits saved exclusions without steam.read and removes a blank optional name", async () => {
  let calls = 0;
  const changes = [];
  const picker = create([{ id: "9", name: "Old title" }], {
    allows: () => false,
    async post() { calls += 1; throw new Error("must not call"); },
  }, value => changes.push(value));
  await tick();
  const name = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Edit excluded Steam game name");
  name.value = "  ";
  name.listeners.get("input")?.();
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), [{ id: "9" }]);

  const manualId = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Steam app ID to exclude");
  const manualName = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Steam game name to exclude");
  manualName.value = "  Manual title ";
  const form = find(picker.node, node => node.tagName === "form" && node.className === "steam-exclusion-manual-add");
  form.dispatchEvent({ type: "submit", preventDefault() {} });
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), [{ id: "9" }, { name: "Manual title" }]);
  assert.equal(manualId.value, "");
  assert.equal(manualName.value, "");
  assert.equal(changes.length, 2);
  assert.equal(calls, 0);
});

test("manual add enforces the 256 exclusion cap", async () => {
  const picker = create(Array.from({ length: 256 }, (_, index) => ({ name: "Saved " + index })), {
    allows: () => false,
    async post() { throw new Error("must not call"); },
  });
  await tick();
  const manualName = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Steam game name to exclude");
  manualName.value = "One too many";
  const form = find(picker.node, node => node.tagName === "form" && node.className === "steam-exclusion-manual-add");
  form.dispatchEvent({ type: "submit", preventDefault() {} });
  assert.equal(picker.value().length, 256);
  assert.match(find(picker.node, node => node.tagName === "p" && node.attributes.get("aria-live") === "polite" && node.textContent.includes("up to 256")).textContent, /up to 256/);
});

test("setDisabled persists through delayed list completion and blocks change handlers", async () => {
  let resolve;
  const picker = create([], {
    allows: () => true,
    post: () => new Promise(done => { resolve = done; }),
  });
  picker.setDisabled(true);
  assert.equal(picker.node.children[7].disabled, true);
  const externalFocus = new Node("input");
  externalFocus.focus();
  resolve({ revision: 1, games: [{ app_id: 8, name: "Late result" }], next_after: null, scanning: false, complete: true });
  await tick();
  const box = find(picker.node, node => node.tagName === "input" && node.type === "checkbox");
  assert.equal(box.disabled, true);
  assert.equal(documentState.activeElement, externalFocus);
  box.checked = true;
  box.listeners.get("change")?.();
  assert.deepEqual(JSON.parse(JSON.stringify(picker.value())), []);
  picker.setDisabled(false);
  assert.equal(find(picker.node, node => node.tagName === "input" && node.type === "checkbox").disabled, false);
});

test("checkbox rerender restores its stable focus target and saved input caret", async () => {
  const picker = create([], {
    allows: () => true,
    async post() {
      return { revision: 1, games: [{ app_id: 10, name: "Focus target" }], next_after: null, scanning: false, complete: true };
    },
  });
  await tick();
  const box = find(picker.node, node => node.tagName === "input" && node.type === "checkbox");
  box.focus();
  box.checked = true;
  box.listeners.get("change")?.();
  const nextBox = find(picker.node, node => node.tagName === "input" && node.type === "checkbox");
  assert.notEqual(nextBox, box);
  assert.equal(documentState.activeElement, nextBox);

  const savedName = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Edit excluded Steam game name");
  savedName.focus();
  savedName.selectionStart = 3;
  savedName.selectionEnd = 5;
  picker.setValue([{ name: "Focused draft" }]);
  const replacement = find(picker.node, node => node.tagName === "input" && node.attributes.get("aria-label") === "Edit name-only Steam exclusion");
  assert.equal(documentState.activeElement, replacement);
  assert.equal(replacement.selectionStart, 3);
  assert.equal(replacement.selectionEnd, 5);
});

test("incomplete and revision restart warnings are visible and stale status labels clear", async () => {
  let call = 0;
  const picker = create([], {
    allows: () => true,
    async post() {
      call += 1;
      if (call === 1) return { revision: 1, games: [], next_after: 1, scanning: false, complete: true };
      if (call === 2) return { revision: 2, games: [], next_after: null, scanning: true, complete: false };
      return { revision: 2, games: [], next_after: null, scanning: false, complete: false };
    },
  });
  await tick();
  picker.node.children[7].click();
  await tick();
  const warning = find(picker.node, node => node.className === "steam-exclusion-warning");
  assert.equal(warning.hidden, false);
  assert.match(warning.textContent, /scan is incomplete/);
  assert.match(warning.textContent, /restarted at the latest revision/);
  assert.equal(picker.node.children[5].attributes.get("aria-label"), "0 matching loaded games; 0 exclusions");
  assert.equal(picker.node.children[5].attributes.has("title"), false);
});
