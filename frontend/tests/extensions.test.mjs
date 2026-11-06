import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function setup(entries, { applicationOnly = false, deferLoad = false, write = true } = {}) {
  class Script { remove() {} }
  let currentScript = null;
  let mountCount = 0;
  let cleanupCount = 0;
  let capturedSdk;
  const sdks = [], scripts = [];
  const calls = [];
  const document = {
    get currentScript() { return currentScript; },
    querySelector() { return null; },
    createElement() { return new Script(); },
    head: { append(script) {
      currentScript = script;
      const mount = (_container, sdk) => {
          mountCount++;
          capturedSdk = sdk;
          sdks.push(sdk);
          return () => { cleanupCount++; };
      };
      context.window.AmaterasuExtensions.v1.register("hello", {
        title: "Hello", ...(applicationOnly ? {} : { mount }), mountApplication: mount,
      });
      currentScript = null;
      scripts.push(script);
      if (!deferLoad) queueMicrotask(() => script.onload());
    } },
  };
  const context = vm.createContext({ window: {}, document, HTMLScriptElement: Script, AbortController, console });
  context.createExtensionUiV1 = (signal) => Object.freeze({ version: 1, signal });
  context.createExtensionUiWithFeedbackV1 = (signal) => Object.freeze({ version: 1, signal, feedback: true, card() {}, table() {} });
  context.createExtensionUiWithDialogsV1 = (signal) => Object.freeze({ version: 1, signal, feedback: true, card() {}, table() {}, dialog: true });
  context.createExtensionUiChartsV1 = () => Object.freeze({});
  context.createExtensionUiTabsV1 = (signal) => Object.freeze({ signal, tabs: true });
  const source = readFileSync(new URL("../src/extensions.ts", import.meta.url), "utf8");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const api = { allows: () => write, async post(route, payload) {
    calls.push({ route, payload });
    if (route === "extensions/list") return { extensions: entries };
    if (route === "session-extensions/settings_get") return { revision: 7, values: {}, effective: {} };
    if (route === "session-extensions/settings_update") return { revision: 8 };
    return { ok: true, result: { answer: 42 } };
  } };
  return { pages: new context.Amaterasu.ExtensionPages(api), calls, sdks, load: () => scripts.forEach(script => script.onload()), sdk: () => capturedSdk, mounts: () => mountCount, cleanups: () => cleanupCount, registry: context.window.AmaterasuExtensions };
}
const valid = { id: "hello", name: "Hello", digest: "a".repeat(64), sdk_version: 1, entrypoint: "example.js", methods: [{ alias: "status" }] };

test("extension registration is bound to a loaded signed script and calls use declared aliases", async () => {
  const env = setup([valid]);
  assert.throws(() => env.registry.v1.register("hello", { title: "Hijack", mount() {} }));
  assert.equal((await env.pages.discover()).length, 1);
  await env.pages.mount("hello", {});
  assert.equal(env.mounts(), 1);
  assert.deepEqual(Array.from(env.sdk().features), ["ui.controls/1", "ui.forms/1", "ui.forms/2", "ui.controls/2", "ui.feedback/1", "ui.layout/1", "ui.tabs/1", "ui.dialog/1", "ui.charts/1", "ui.assets/1"]);
  assert.equal(Object.isFrozen(env.sdk().features), true);
  assert.equal(env.sdk().ui.version, 1);
  assert.equal(env.sdk().ui.signal, env.sdk().signal);
  assert.equal(env.sdk().ui.feedback, true);
  assert.equal(typeof env.sdk().ui.card, "function");
  assert.equal(typeof env.sdk().ui.table, "function");
  assert.equal(env.sdk().ui.tabs, true);
  assert.equal(env.sdk().ui.dialog, true);
  assert.equal((await env.sdk().call("status", { value: 1 })).answer, 42);
  assert.deepEqual(JSON.parse(JSON.stringify(env.calls.at(-1))), { route: "extensions/call", payload: { id: "hello", digest: valid.digest, method: "status", payload: { value: 1 } } });
  await assert.rejects(env.sdk().call("other"));
  env.pages.dispose();
  assert.equal(env.cleanups(), 1);
  assert.equal(env.sdk().signal.aborted, true);
  await assert.rejects(env.sdk().call("status"));
});

test("discovery rejects incompatible, unsafe and duplicate extension descriptors", async () => {
  const env = setup([valid, valid, { ...valid, id: "bad", entrypoint: "../escape.js" }, { ...valid, id: "future", sdk_version: 2 }]);
  assert.equal((await env.pages.discover()).length, 0);
  await assert.rejects(env.pages.mount("hello", {}));
});

test("application-only sections receive app context without settings overrides", async () => {
  const env = setup([{ ...valid, placements: ["application"] }], { applicationOnly: true });
  await env.pages.discover();
  await assert.rejects(env.pages.mount("hello", {}), /location/);
  const section = env.pages.mountApplication("hello", {}, { id: "app-0000000000000001", name: "First game" });
  await section.ready;
  assert.equal(env.sdk().application.id, "app-0000000000000001");
  assert.equal(Object.isFrozen(env.sdk().application), true);
  assert.ok(env.sdk().features.includes("ui.application-section/1"));
  await env.sdk().call("status", { application_id: env.sdk().application.id });
  assert.equal(env.calls.at(-1).payload.payload.application_id, "app-0000000000000001");
  assert.equal(env.calls.some(call => call.route.startsWith("session-extensions/")), false);
  env.sdk().setDirty(true);
  assert.equal(section.isDirty(), true);
  env.sdk().setDirty(false);
  env.sdk().setBusy(true);
  assert.equal(section.isBusy(), true);
  assert.equal(section.isDirty(), true);
  section.dispose();
  assert.equal(section.isBusy(), false);
  assert.equal(env.sdk().signal.aborted, true);
  await assert.rejects(env.sdk().call("status"));
  assert.equal(env.cleanups(), 1);
});

test("different apps have independent plugin lifecycles and draft state", async () => {
  const env = setup([{ ...valid, placements: ["application"] }]);
  await env.pages.discover();
  const first = env.pages.mountApplication("hello", {}, { id: "app-0000000000000001", name: "First" });
  await first.ready;
  const second = env.pages.mountApplication("hello", {}, { id: "app-0000000000000002", name: "Second" });
  await second.ready;
  env.sdks[0].setDirty(true);
  assert.equal(first.isDirty(), true);
  assert.equal(second.isDirty(), false);
  first.dispose();
  assert.equal(env.sdks[0].signal.aborted, true);
  assert.equal(env.sdks[1].signal.aborted, false);
  env.pages.reset();
  assert.equal(env.sdks[1].signal.aborted, true);
  assert.equal(env.cleanups(), 2);
});

test("closing an app while its script loads prevents a late mount", async () => {
  const env = setup([{ ...valid, placements: ["application"] }], { deferLoad: true });
  await env.pages.discover();
  const section = env.pages.mountApplication("hello", {}, { id: "app-0000000000000001", name: "First" });
  section.dispose();
  env.load();
  await section.ready;
  assert.equal(env.mounts(), 0);
});

test("unsupported, duplicate and empty UI placements are rejected", async () => {
  for (const placements of [[], ["other"], ["application", "application"]]) {
    const env = setup([{ ...valid, placements }]);
    assert.equal((await env.pages.discover()).length, 0);
  }
});

test("plugin settings helper binds the selected provider and application scope", async () => {
  const descriptor = { ...valid, placements: ["application"], settings_provider: "amaterasu.rtx-hdr" };
  const env = setup([descriptor]);
  await env.pages.discover();
  const section = env.pages.mountApplication("hello", {}, { id: "app-0000000000000001", name: "Game" });
  await section.ready;
  assert.ok(env.sdk().features.includes("ui.application-settings/1"));
  assert.equal(env.sdk().settings.provider, "amaterasu.rtx-hdr");
  assert.equal((await env.sdk().settings.read()).revision, 7);
  await env.sdk().settings.update(7, { rtx_hdr_enable: true });
  assert.deepEqual(JSON.parse(JSON.stringify(env.calls.at(-1))), {
    route: "session-extensions/settings_update",
    payload: { expected_revision: 7, values: { rtx_hdr_enable: true }, provider: "amaterasu.rtx-hdr", scope: { kind: "application", id: "app-0000000000000001" } },
  });
  section.dispose();
  await assert.rejects(env.sdk().settings.read(), /closed/);
  const readonly = setup([descriptor], { write: false });
  await readonly.pages.discover();
  await readonly.pages.mountApplication("hello", {}, { id: "app-0000000000000002", name: "Other" }).ready;
  assert.equal(readonly.sdk().settings.canWrite, false);
  await assert.rejects(readonly.sdk().settings.update(7, {}), /read-only/);
});
