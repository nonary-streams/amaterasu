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
  for (const source of ["src/model.ts", "src/ui.ts", "src/provider_refresh.ts", "src/integrations.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext(`Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; Amaterasu.literalText = value => value; globalThis.localizedMessage = value => ({ kind: "localized", value }); Amaterasu.localizedMessage = globalThis.localizedMessage;`, dom);
  }
  dom.renderRtxHdrIntegration = dom.Amaterasu.renderRtxHdrIntegration = () => {};
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


test("dashboard refresh uses shared provider operations and reports provider readback", async () => {
  class Element {
    constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; this.parentElement = null; this.disabled = false; this.hidden = false; }
    append(...children) { for (const child of children.filter((item) => item != null)) { this.children.push(child); if (typeof child !== "string") child.parentElement = this; } }
    replaceChildren(...children) { for (const child of this.children) if (typeof child !== "string") child.parentElement = null; this.children = []; this.append(...children); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    setAttribute() {}
    click() { if (!this.disabled && !this.hidden) this.listeners.get("click")?.(); }
    get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.value ?? child?.textContent ?? "").join(""); }
    set textContent(value) { this.replaceChildren(String(value)); }
  }
  const dom = vm.createContext({ document: { createElement: (tag) => new Element(tag) }, Error, Number, Object });
  for (const source of ["src/model.ts", "src/ui.ts", "src/provider_refresh.ts", "src/integrations.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext(`Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; Amaterasu.literalText = value => value; globalThis.localizedMessage = value => ({ kind: "localized", value }); Amaterasu.localizedMessage = globalThis.localizedMessage;`, dom);
  }
  dom.renderRtxHdrIntegration = dom.Amaterasu.renderRtxHdrIntegration = () => {};
  const content = new Element("main");
  const calls = [];
  const lutris = { ok: true, settings_ready: true, enabled: true, platform_supported: true,
    database_available: true, executable_available: true, available: true, game_count: 2, refresh_pending: false, last_error: null };
  const playnite = { ok: true, settings_ready: true, enabled: true, playnite_running: true,
    snapshot_pending: false, snapshot_game_count: 4, last_error: null };
  dom.Amaterasu.renderIntegrations({ content, navigate() {}, api: {
    allows: () => true,
    async post(route) {
      calls.push(route);
      if (route === "steam/status") return { ok: true, settings_ready: true, enabled: true, game_count: 0, warning_count: 0, scanning: false, complete: true };
      if (route === "lutris/status") return lutris;
      if (route === "lutris/refresh") return { ...lutris, refresh_pending: true };
      if (route === "playnite/status") return calls.filter((item) => item === route).length === 1
        ? playnite : { ...playnite, snapshot_pending: true };
      if (route === "playnite/refresh") return { ok: true };
      throw new Error(`unexpected route: ${route}`);
    },
  } });
  await new Promise(setImmediate);
  const all = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? all(child) : [])];
  const findButton = (label) => all(content).find((node) => node.tagName === "button" && node.textContent === label);
  const lutrisButton = findButton("Refresh now");
  const playniteButton = all(content).filter((node) => node.tagName === "button" && node.textContent === "Refresh now")[1];
  assert.ok(lutrisButton && !lutrisButton.disabled);
  assert.ok(playniteButton && !playniteButton.disabled);
  lutrisButton.click();
  await new Promise(setImmediate);
  assert.match(content.textContent, /Scanning/);
  assert.equal(lutrisButton.disabled, true);
  playniteButton.click();
  await new Promise(setImmediate);
  assert.match(content.textContent, /Syncing/);
  assert.equal(playniteButton.disabled, true);
  assert.deepEqual(calls, ["steam/status", "lutris/status", "playnite/status", "lutris/refresh", "playnite/refresh", "playnite/status"]);
});
