import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const plain = value => JSON.parse(JSON.stringify(value));
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };

function fixture(scope, write = true, values = { enabled: false }, provider, fullControls = false) {
  class Element {
    children = [];
    listeners = {};
    isConnected = true;
    checked = false;
    value = "";
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
  const calls = [], editors = [], pending = [], errors = [], notifications = [];
  const descriptor = { provider: "example.pacing", display_name: "Pacing controls", actions: [], options: [
    { id: "enabled", label: "Enabled", help: "", definition: { type: "boolean", default: true }, allowed_scopes: ["host", "application", "paired_client"], apply_policy: "next_stream" },
    { id: "installation", label: "Installation", help: "", definition: { type: "string", default: "" }, allowed_scopes: ["host"], apply_policy: "next_session" },
  ] };
  const runtime = vm.createContext({ structuredClone, Amaterasu: {
    literalText: value => ({ kind: "literal", value }),
    el: element,
    input: () => element("input"),
    button: (label, click) => { const node = element("button", "", label); node.listeners.click = click; return node; },
    panel: (title, ...children) => element("panel", "", title, ...children),
    extensionFieldEditor: (definition, initial) => {
      const editor = { node: element("editor"), initial, definition, value: () => initial };
      editors.push(editor);
      return editor;
    },
    extensionRadioGroupEditor: (definition, initial, label, choiceLabels) => {
      const editor = { node: element("radio-group"), initial, definition, label, choiceLabels, value: () => initial };
      editors.push(editor);
      return editor;
    },
    extensionSliderEditor: (definition, initial, label) => {
      const editor = { node: element("slider"), initial, definition, label, value: () => initial };
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
  const context = { content: { isConnected: true }, notify(message) { notifications.push(message); }, api: {
    allows: () => write,
    async post(route, payload) {
      calls.push([route, plain(payload)]);
      if (route.endsWith("/list")) return { items: [descriptor], next: null, revision: 3 };
      if (route.endsWith("/settings_get")) return { revision: fullControls && calls.some(([route]) => route.endsWith("/settings_update")) ? 8 : 7, values, effective: {
        enabled: { value: false, source: scope }, installation: { value: "", source: null },
      } };
      return { ok: true, revision: 8 };
    },
  } };
  if (fullControls) runtime.Amaterasu.renderApplicationPluginOverrides(context, area, scope.id, undefined, true);
  else runtime.Amaterasu.renderSessionExtensionSettings(context, area, scope, provider);
  return { area, calls, nodes, editors, pending, errors, notifications, descriptor };
}

test("application cards use global values without inheritance switches and save only edits", async () => {
  const env = fixture({ kind: "application", id: "app-0000000000000001" }, true, {}, undefined, true);
  await settle();
  assert.equal(env.editors.length, 1, "application controls render immediately, host-only options stay hidden");
  assert.equal(env.nodes.some(node => node.tag === "label" && node.children.includes(" Use inherited value")), false);
  const form = env.nodes.find(node => node.tag === "form");
  form.listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.equal(env.calls.some(([route]) => route.endsWith("/settings_update")), false);
  env.editors[0].value = () => true;
  env.editors[0].node.listeners.input();
  form.listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.deepEqual(env.calls.find(([route]) => route.endsWith("/settings_update"))[1].values, { enabled: true });
  assert.deepEqual(env.errors, []);
});

for (const expanded of [undefined, false, true]) {
  test(`generated application sections honor default_expanded=${expanded} without creating overrides`, async () => {
    const env = fixture({ kind: "application", id: "app-0000000000000001" }, true, {}, undefined, true);
    if (expanded !== undefined) env.descriptor.default_expanded = expanded;
    await settle();
    const section = env.nodes.find(node => node.className === "application-plugin-section");
    assert.equal(section.tag, "details");
    assert.equal(section.open, expanded === true);
    assert.equal(env.calls.some(([route]) => route.endsWith("/settings_update")), false);
    section.open = !section.open;
    assert.equal(env.calls.some(([route]) => route.endsWith("/settings_update")), false);
  });
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


test("setting policy guidance and save feedback distinguish persistence from application", async () => {
  const env = fixture({ kind: "host" });
  await settle();
  const guidance = env.nodes.filter(node => node.tag === "small").map(node => node.children[0]);
  assert.ok(guidance.includes("Requires a new or resumed stream."));
  assert.ok(guidance.includes("Requires a new application session."));
  env.nodes.find(node => node.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.deepEqual(env.notifications, ["Plugin settings saved. Application to an active session or stream is not confirmed."]);
});

test("live and unknown policy guidance does not claim runtime application", async () => {
  const live = fixture({ kind: "application", id: "game" });
  live.descriptor.options[0].apply_policy = "live_action";
  await settle();
  assert.ok(live.nodes.some(node => node.tag === "small" && node.children[0] === "May apply during an active session or stream; saving does not confirm application."));
  const unknown = fixture({ kind: "application", id: "game" });
  unknown.descriptor.options[0].apply_policy = "future_policy";
  await settle();
  assert.ok(unknown.nodes.some(node => node.tag === "small" && node.children[0] === "Application timing is not available."));
});


test("radio-group descriptor presentations use the enum control and preserve its wire value", async () => {
  const env = fixture({ kind: "application", id: "game" });
  env.descriptor.options[0].presentation = "radio_group";
  env.descriptor.options[0].definition = { type: "enum", default: "steady", options: ["steady", "responsive"] };
  await settle();
  assert.equal(env.editors.length, 1);
  assert.equal(env.editors[0].node.tag, "radio-group");
  assert.equal(env.editors[0].label, "Enabled");
  env.editors[0].value = () => "responsive";
  env.nodes.find(node => node.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.equal(env.calls.find(([route]) => route.endsWith("/settings_update"))[1].values.enabled, "responsive");
  assert.deepEqual(env.errors, []);
});

test("a future presentation hint falls back to its typed editor and saves edits", async () => {
  const env = fixture({ kind: "application", id: "game" }, true, {});
  env.descriptor.options[0].presentation = "future_control";
  await settle();
  assert.equal(env.editors.length, 1);
  assert.equal(env.editors[0].definition.type, "boolean");
  env.editors[0].value = () => true;
  env.editors[0].node.listeners.input();
  env.nodes.find(node => node.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(env.pending);
  assert.equal(env.calls.find(([route]) => route.endsWith("/settings_update"))[1].values.enabled, true);
  assert.deepEqual(env.errors, []);
});

for (const sharedCatalog of [false, true]) test(`application overrides ${sharedCatalog ? "in the shared RTX HDR chooser" : "in the standalone chooser"} preserve values and save with the provider revision`, async () => {
  class Element {
    children = []; listeners = {}; isConnected = true; value = ""; disabled = false;
    constructor(tag, children = []) { this.tag = tag; this.children = children; }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; if (this.tag === "select") this.value = children[0]?.value ?? ""; }
    addEventListener(name, callback) { this.listeners[name] = callback; }
    setAttribute() {} matches() { return false; } querySelectorAll() { return []; } querySelector() { return null; }
    remove() { this.removed = true; }
  }
  const nodes = [], calls = [], pending = [], initialValues = [];
  const element = (tag, _class, ...children) => { const n = new Element(tag, children); nodes.push(n); return n; };
  const descriptor = { provider: "example", display_name: "Example", options: [
    { id: "enabled", label: "Enabled", help: "", presentation: "future_control", definition: { type: "boolean", default: true }, allowed_scopes: ["application"] },
    { id: "global", label: "Global", help: "", definition: { type: "boolean", default: true }, allowed_scopes: ["host"] },
  ] };
  const runtime = vm.createContext({ structuredClone, Amaterasu: {
    el: element, literalText: value => value,
    input: () => element("input"),
    button: (label, click) => { const n = element("button", "", label); n.listeners.click = click; return n; },
    panel: (title, ...children) => element("panel", "", title, ...children),
    extensionFieldEditor: (_definition, initial) => { initialValues.push(initial); return { node: element("editor"), value: () => initial }; },
    runAction: (_button, _status, action) => { const p = action(); pending.push(p); return p; },
    errorPanel: error => { throw error; },
  } });
  Object.assign(runtime, runtime.Amaterasu);
  vm.runInContext(ts.transpile(readFileSync(new URL("../src/session_extensions.ts", import.meta.url), "utf8"),
    { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), runtime);
  const area = element("area");
  const catalogSearch = element("input");
  const catalogList = element("div");
  const overrideCatalog = { querySelector: selector => selector === 'input[type="search"]' ? catalogSearch : catalogList };
  let guard;
  const context = { content: { isConnected: true }, notify() {}, registerDraftGuard(check) { guard = check; }, api: {
    allows: () => true,
    async post(route, payload) {
      calls.push([route, plain(payload)]);
      if (route.endsWith("/list")) return { items: [descriptor], next: null, revision: 1 };
      if (route.endsWith("/settings_get")) return calls.some(([name]) => name.endsWith("/settings_update"))
        ? { revision: 8, values: { unknown: 42 }, effective: { enabled: { value: true, source: { kind: "host" } } } }
        : { revision: 7, values: { enabled: false, unknown: 42 }, effective: {
          enabled: { value: false, source: { kind: "application", id: "game" } },
        } };
      return { revision: 8 };
    },
  } };
  if (sharedCatalog) {
    descriptor.display_name = "RTX HDR";
    descriptor.options[0].label = "Enable RTX HDR";
  }
  runtime.Amaterasu.renderApplicationPluginOverrides(context, area, "game", sharedCatalog ? overrideCatalog : undefined);
  await settle();
  assert.equal(guard(), false);
  assert.equal(nodes.filter(n => n.tag === "editor").length, 1);
  const sharedButton = catalogList.children[0]?.children.find(n => n.tag === "button");
  if (sharedCatalog) {
    assert.equal(catalogList.children.length, 1);
    assert.equal(sharedButton.children[0], "Enable RTX HDR");
    assert.equal(sharedButton.hidden, false, "existing override remains available for editing");
    assert.equal(catalogList.children[0].children.filter(n => n.tag === "button").length, 1, "host-only options stay out of application choices");
  }
  const chooser = nodes.find(n => n.tag === "details");
  assert.equal(chooser.children[0].tag, "summary");
  assert.equal(chooser.children[0].children[0], "Add override");
  assert.equal(chooser.children[1].children.some(n => n.tag === "p"), true, "inherited hint has its own block");
  nodes.find(n => n.tag === "button" && n.children[0] === "Remove override").listeners.click();
  assert.equal(guard(), true);
  assert.equal(runtime.Amaterasu.applicationPluginOverridesDirty(area), true);
  nodes.find(n => n.tag === "form").listeners.submit({ preventDefault() {} });
  await Promise.all(pending);
  assert.deepEqual(calls.find(([route]) => route.endsWith("/settings_update"))[1], {
    provider: "example", scope: { kind: "application", id: "game" }, expected_revision: 7, values: { unknown: 42 },
  });
  assert.equal(guard(), false);
  if (sharedCatalog) {
    assert.equal(sharedButton.hidden, false);
    catalogSearch.value = "unrelated";
    catalogSearch.listeners.input();
    assert.equal(sharedButton.hidden, true);
    assert.equal(catalogList.children[0].hidden, true);
    catalogSearch.value = "RTX HDR";
    catalogSearch.listeners.input();
    assert.equal(sharedButton.hidden, false);
    sharedButton.listeners.click();
    assert.equal(sharedButton.hidden, false);
  } else nodes.find(n => n.tag === "button" && n.children[0] === "Add override").listeners.click();
  assert.equal(guard(), true, "adding an override must guard navigation");
  assert.deepEqual(initialValues, [false, true], "re-added override starts from refreshed inherited value");
});
