import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function setup(entries) {
  class Script { remove() {} }
  let currentScript = null;
  let mountCount = 0;
  let cleanupCount = 0;
  let capturedSdk;
  const calls = [];
  const document = {
    get currentScript() { return currentScript; },
    querySelector() { return null; },
    createElement() { return new Script(); },
    head: { append(script) {
      currentScript = script;
      context.window.AmaterasuExtensions.v1.register("hello", {
        title: "Hello", mount(_container, sdk) {
          mountCount++;
          capturedSdk = sdk;
          return () => { cleanupCount++; };
        },
      });
      currentScript = null;
      queueMicrotask(() => script.onload());
    } },
  };
  const context = vm.createContext({ window: {}, document, HTMLScriptElement: Script, AbortController, console });
  context.createExtensionUiV1 = (signal) => Object.freeze({ version: 1, signal });
  const source = readFileSync(new URL("../src/extensions.ts", import.meta.url), "utf8");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  const api = { async post(route, payload) { calls.push({ route, payload }); return route === "extensions/list" ? { extensions: entries } : { ok: true, result: { answer: 42 } }; } };
  return { pages: new context.Amaterasu.ExtensionPages(api), calls, sdk: () => capturedSdk, mounts: () => mountCount, cleanups: () => cleanupCount, registry: context.window.AmaterasuExtensions };
}
const valid = { id: "hello", name: "Hello", digest: "a".repeat(64), sdk_version: 1, entrypoint: "example.js", methods: [{ alias: "status" }] };

test("extension registration is bound to a loaded signed script and calls use declared aliases", async () => {
  const env = setup([valid]);
  assert.throws(() => env.registry.v1.register("hello", { title: "Hijack", mount() {} }));
  assert.equal((await env.pages.discover()).length, 1);
  await env.pages.mount("hello", {});
  assert.equal(env.mounts(), 1);
  assert.deepEqual(Array.from(env.sdk().features), ["ui.controls/1", "ui.forms/1"]);
  assert.equal(Object.isFrozen(env.sdk().features), true);
  assert.equal(env.sdk().ui.version, 1);
  assert.equal(env.sdk().ui.signal, env.sdk().signal);
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
