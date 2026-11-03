import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor() { this.children = []; this.listeners = new Map(); this.attributes = new Set(); }
  setAttribute(name) { this.attributes.add(name); }
  hasAttribute(name) { return this.attributes.has(name); }
  append(...nodes) { this.children.push(...nodes); }
  contains(node) { return this === node || this.children.some((child) => child.contains?.(node)); }
  querySelectorAll() { return this.children.flatMap((child) => child instanceof Control ? [child] : child.querySelectorAll()); }
  addEventListener(name, handler) { this.listeners.set(name, handler); }
  removeEventListener(name) { this.listeners.delete(name); }
  change() { this.listeners.get("input")?.(); }
}
class Control extends Element { constructor(value = "") { super(); this.value = value; } }
class Input extends Control { constructor(value, type = "text", checked = false) { super(value); this.type = type; this.checked = checked; } }
const context = vm.createContext({ HTMLInputElement: Input, MutationObserver: class { observe() {} disconnect() {} } });
vm.runInContext(ts.transpile(readFileSync("src/ui.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), context);

test("draft tracking preserves later edits and unrelated device drafts after a confirmed save", () => {
  const root = new Element();
  const firstCard = new Element();
  const secondCard = new Element();
  const first = new Input("0");
  const second = new Input("", "checkbox", false);
  const hidden = new Input("metadata", "hidden");
  const filter = new Input("", "search");
  filter.setAttribute("data-draft-ignore");
  firstCard.append(first, hidden, filter);
  secondCard.append(second);
  root.append(firstCard, secondCard);
  const states = [];
  const tracker = context.Amaterasu.trackDrafts(root, (dirty) => states.push(dirty));
  assert.equal(tracker.isDirty(), false);
  first.value = "35";
  second.checked = true;
  root.change();
  const submitted = tracker.capture(firstCard);
  first.value = "36";
  tracker.markSaved(submitted);
  assert.equal(tracker.isDirty(firstCard), true, "edits after submission are still unsaved");
  assert.equal(tracker.isDirty(secondCard), true, "another client's draft remains unsaved");
  first.value = "35";
  assert.equal(tracker.isDirty(firstCard), false);
  second.checked = false;
  hidden.value = "changed hidden metadata";
  filter.value = "filter text";
  assert.equal(tracker.isDirty(), false);
  assert.deepEqual(states, [true, true]);
  tracker.dispose();
  assert.equal(root.listeners.size, 0);
});

test("closing a plugin editor retains the page's independent draft guard", () => {
  const guards = new context.Amaterasu.DraftGuardGroup();
  let pageDirty = true;
  let pageDisposed = 0;
  let pluginDisposed = 0;
  guards.register(() => pageDirty, () => pageDisposed++);
  const closePlugin = guards.register(() => true, () => pluginDisposed++);
  assert.equal(guards.isDirty(), true);
  closePlugin();
  assert.equal(pluginDisposed, 1);
  assert.equal(guards.isDirty(), true, "page edits must remain protected");
  pageDirty = false;
  assert.equal(guards.isDirty(), false);
  guards.dispose();
  closePlugin();
  assert.equal(pageDisposed, 1);
  assert.equal(pluginDisposed, 1, "a guard is disposed exactly once");
});
