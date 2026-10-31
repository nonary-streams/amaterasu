import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ Error, Number, Object });
vm.runInContext(ts.transpile(readFileSync("src/integrations.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
const summary = context.Amaterasu.summarizeProvider;

test("provider cards describe reported state without inferring readiness", () => {
  assert.equal(summary("steam", { ok: true, game_count: 1 }).state, "Unverified");
  assert.equal(summary("steam", { ok: true, settings_ready: false, enabled: false }).state, "Loading settings");
  assert.equal(summary("steam", { ok: true, settings_ready: true, enabled: false }).state, "Disabled");
  const steam = summary("steam", { ok: true, settings_ready: true, enabled: true, game_count: 12, warning_count: 2, scanning: false, complete: true });
  assert.equal(steam.state, "Scan complete");
  assert.match(steam.detail, /12 discovered games · 2 scan warnings/);
  const lutris = { ok: true, settings_ready: true, enabled: true, platform_supported: true,
    database_available: true, executable_available: true, available: true, game_count: 3, refresh_pending: false, last_error: "scan failed" };
  assert.equal(summary("lutris", lutris).state, "Scan issue");
  assert.equal(summary("lutris", { ...lutris, platform_supported: false }).state, "Unsupported");
  assert.equal(summary("lutris", { ...lutris, available: false }).state, "Needs setup");
  assert.equal(summary("playnite", { ok: true, settings_ready: true, enabled: true,
    playnite_running: false, snapshot_game_count: 0, snapshot_pending: false, last_error: null }).state, "Not detected");
  assert.throws(() => summary("steam", { ok: true, settings_ready: true, enabled: true, game_count: -1 }));
});

test("integration cards isolate unavailable providers and respect read scopes", async () => {
  class Element {
    constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; }
    append(...children) { this.children.push(...children.filter((child) => child != null)); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    setAttribute() {}
    click() { this.listeners.get("click")?.(); }
    get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
    set textContent(value) { this.replaceChildren(String(value)); }
  }
  const dom = vm.createContext({ document: { createElement: (tag) => new Element(tag) }, Error, Number, Object });
  for (const source of ["src/model.ts", "src/ui.ts", "src/integrations.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; Amaterasu.literalText = value => value;", dom);
  }
  const content = new Element("main");
  const calls = [];
  const navigation = [];
  dom.Amaterasu.renderIntegrations({ content, navigate: (page) => navigation.push(page), api: {
    allows: (scope) => scope === "configuration.read",
    async post(route) {
      calls.push(route);
      if (route === "playnite/status") throw new Error("provider offline");
      assert.equal(route, "lutris/status");
      return { ok: true, settings_ready: true, enabled: true, platform_supported: true,
        database_available: true, executable_available: true, available: true,
        game_count: 3, refresh_pending: false, last_error: null };
    },
  } });
  await new Promise(setImmediate);
  assert.deepEqual(calls.sort(), ["lutris/status", "playnite/status"]);
  assert.match(content.textContent, /cannot read this provider's status/);
  assert.match(content.textContent, /3 discovered games/);
  assert.match(content.textContent, /provider offline/);
  const all = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? all(child) : [])];
  all(content).find((node) => node.tagName === "button" && node.textContent === "Configure").click();
  assert.deepEqual(navigation, ["settings/integrations"]);
});
