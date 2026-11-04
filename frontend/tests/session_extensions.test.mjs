import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const plain = value => JSON.parse(JSON.stringify(value));
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

function fixture(scope, write = true, values = { enabled: false }, provider) {
  class Element {
    children = [];
    listeners = {};
    isConnected = true;
    checked = false;
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    addEventListener(name, callback) { this.listeners[name] = callback; }
    matches() { return false; }
    querySelectorAll() { return []; }
    setAttribute(name, value) { this[name] = value; }
  }
  const nodes = [];
  const element = (tag, className, ...children) => {
    const node = new Element();
    Object.assign(node, { tag, className, children });
    nodes.push(node);
    return node;
  };
  const calls = [], editors = [], pending = [], errors = [];
  const descriptor = { provider: "example.pacing", display_name: "Pacing controls", actions: [], options: [
    { id: "enabled", label: "Enabled", help: "", definition: { type: "boolean", default: true }, allowed_scopes: ["host", "application", "paired_client"], apply_policy: "next_stream" },
    { id: "installation", label: "Installation", help: "", definition: { type: "string", default: "" }, allowed_scopes: ["host"], apply_policy: "next_session" },
  ] };
  const runtime = vm.createContext({ Amaterasu: {
    literalText: value => ({ kind: "literal", value }),
    el: element,
    input: () => element("input"),
    panel: (title, ...children) => element("panel", "", title, ...children),
    extensionFieldEditor: (definition, initial) => {
      const editor = { node: element("editor"), initial, definition, value: () => initial };
      editors.push(editor);
      return editor;
    },
    runAction: (_button, _status, work) => {
      const promise = work().catch(error => errors.push(error));
      pending.push(promise);
      return promise;
    },
    errorPanel: error => element("error", "", String(error)),
  } });
  Object.assign(runtime, runtime.Amaterasu);
  vm.runInContext(ts.transpile(readFileSync(new URL("../src/session_extensions.ts", import.meta.url), "utf8"),
    { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), runtime);
  const area = element("area");
  const context = { content: { isConnected: true }, notify() {}, api: {
    allows: () => write,
    async post(route, payload) {
      calls.push([route, plain(payload)]);
      if (route.endsWith("/list")) return { items: [descriptor], next: null, revision: 3 };
      if (route.endsWith("/settings_get")) return { revision: 7, values, effective: {
        enabled: { value: false, source: scope }, installation: { value: "", source: null },
      } };
      return { ok: true, revision: 8 };
    },
  } };
  runtime.Amaterasu.renderSessionExtensionSettings(context, area, scope, provider);
  return { area, calls, nodes, editors, pending, errors };
}

for (const scope of [{ kind: "application", id: "game" }, { kind: "paired_client", id: "client" }]) {
  test(`${scope.kind} settings use the generic scope and preserve explicit false`, async () => {
    const env = fixture(scope);
    await settle();
    assert.deepEqual(env.calls[1], ["session-extensions/settings_get", { provider: "example.pacing", scope }]);
    assert.equal(env.editors.length, 1, "host-only options must be excluded");
    assert.equal(env.editors[0].initial, false);
    const form = env.nodes.find(node => node.tag === "form");
    form.listeners.submit({ preventDefault() {} });
    await Promise.all(env.pending);
    const saved = env.calls.find(([route]) => route.endsWith("/settings_update"));
    assert.deepEqual(saved, ["session-extensions/settings_update", { provider: "example.pacing", scope, expected_revision: 7, values: { enabled: false } }]);
    assert.deepEqual(env.errors, []);
  });
}

test("inherit removes the scoped value and read-only access disables saving", async () => {
  const env = fixture({ kind: "application", id: "game" });
  await settle();
  env.nodes.find(node => node.tag === "input").checked = true;
  env.nodes.find(node => node.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.deepEqual(env.calls.find(([route]) => route.endsWith("/settings_update"))[1].values, {});
  const readOnly = fixture({ kind: "paired_client", id: "client" }, false);
  await settle();
  assert.equal(readOnly.nodes.find(node => node.tag === "button").disabled, true);
});

test("editing default host values saves them without a second inheritance checkbox", async () => {
  const env = fixture({ kind: "host" }, true, {});
  await settle();
  assert.equal(env.nodes.filter(node => node.tag === "input").length, 0);
  env.editors[0].value = () => true;
  env.nodes.find(node => node.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  const saved = env.calls.find(([route]) => route.endsWith("/settings_update"))[1];
  assert.equal(saved.values.enabled, true);
  assert.equal(saved.values.installation, "");
  const panel = env.nodes.find(node => node.tag === "panel");
  assert.equal(panel.children[0].value, "Pacing controls");
  const legend = env.nodes.find(node => node.tag === "legend");
  assert.deepEqual(plain(legend.children[0]), { kind: "literal", value: "Enabled" });
});

test("editing an inherited application value automatically makes it an override", async () => {
  const env = fixture({ kind: "application", id: "game" }, true, {});
  await settle();
  const inherit = env.nodes.find(node => node.tag === "input");
  assert.equal(inherit.checked, true);
  env.editors[0].value = () => true;
  env.editors[0].node.listeners.input();
  assert.equal(inherit.checked, false);
  env.nodes.find(node => node.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.equal(env.calls.find(([route]) => route.endsWith("/settings_update"))[1].values.enabled, true);
});


test("provider filter shows only the selected plugin settings", async () => {
  const matching = fixture({ kind: "host" }, true, {}, "example.pacing");
  await settle();
  assert.equal(matching.editors.length, 2);
  const other = fixture({ kind: "host" }, true, {}, "amaterasu.rtx-hdr");
  await settle();
  assert.equal(other.editors.length, 0);
  assert.equal(other.calls.some(([route]) => route.endsWith("/settings_get")), false);
});

test("application overrides preserve unrelated values, guard structural edits, and save with the provider revision", async () => {
  class Element {
    children = []; listeners = {}; isConnected = true; value = ""; disabled = false;
    constructor(tag, children = []) { this.tag = tag; this.children = children; }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; if (this.tag === "select") this.value = children[0]?.value ?? ""; }
    addEventListener(name, callback) { this.listeners[name] = callback; }
    setAttribute() {} matches() { return false; } querySelectorAll() { return []; }
    remove() { this.removed = true; }
  }
  const nodes = [], calls = [], pending = [];
  const element = (tag, _class, ...children) => { const n = new Element(tag, children); nodes.push(n); return n; };
  const descriptor = { provider: "example", display_name: "Example", options: [
    { id: "enabled", label: "Enabled", help: "", definition: { type: "boolean", default: true }, allowed_scopes: ["application"] },
    { id: "global", label: "Global", help: "", definition: { type: "boolean", default: true }, allowed_scopes: ["host"] },
  ] };
  const runtime = vm.createContext({ structuredClone, Amaterasu: {
    el: element, literalText: value => value,
    input: () => element("input"),
    button: (label, click) => { const n = element("button", "", label); n.listeners.click = click; return n; },
    panel: (title, ...children) => element("panel", "", title, ...children),
    extensionFieldEditor: (_definition, initial) => ({ node: element("editor"), value: () => initial }),
    runAction: (_button, _status, action) => { const p = action(); pending.push(p); return p; },
    errorPanel: error => { throw error; },
  } });
  Object.assign(runtime, runtime.Amaterasu);
  vm.runInContext(ts.transpile(readFileSync(new URL("../src/session_extensions.ts", import.meta.url), "utf8"),
    { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), runtime);
  const area = element("area");
  let guard;
  const context = { content: { isConnected: true }, notify() {}, registerDraftGuard(check) { guard = check; }, api: {
    allows: () => true,
    async post(route, payload) {
      calls.push([route, plain(payload)]);
      if (route.endsWith("/list")) return { items: [descriptor], next: null, revision: 1 };
      if (route.endsWith("/settings_get")) return { revision: 7, values: { enabled: false, unknown: 42 }, effective: {
        enabled: { value: false, source: { kind: "application", id: "game" } },
      } };
      return { revision: 8 };
    },
  } };
  runtime.Amaterasu.renderApplicationPluginOverrides(context, area, "game");
  await settle();
  assert.equal(guard(), false);
  assert.equal(nodes.filter(n => n.tag === "editor").length, 1);
  nodes.find(n => n.tag === "button" && n.children[0] === "Remove override").listeners.click();
  assert.equal(guard(), true);
  assert.equal(runtime.Amaterasu.applicationPluginOverridesDirty(area), true);
  nodes.find(n => n.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(pending);
  assert.deepEqual(calls.find(([route]) => route.endsWith("/settings_update"))[1], {
    provider: "example", scope: { kind: "application", id: "game" }, expected_revision: 7, values: { unknown: 42 },
  });
  assert.equal(guard(), false);
  nodes.find(n => n.tag === "button" && n.children[0] === "Add override").listeners.click();
  assert.equal(guard(), true, "adding an override must guard navigation");
});
