import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ Error, Number, Object });
vm.runInContext(ts.transpile(readFileSync("src/rtx_hdr.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
const parse = context.Amaterasu.parseRtxHdrStatus;
const status = { ok: true, phase: "idle", transferred_bytes: 0, total_bytes: null,
  error: null, restart_required: false, supported: true, runtime_ready: false, package_ready: false, provider_active: false };

test("RTX HDR status validates download and activation state", () => {
  assert.equal(parse(status).provider_active, false);
  assert.equal(parse({ ...status, provider_active: null }).provider_active, null);
  assert.equal(parse({ ...status, phase: "downloading", transferred_bytes: 5, total_bytes: 10 }).total_bytes, 10);
  for (const invalid of [
    { ...status, phase: "active" }, { ...status, transferred_bytes: -1 },
    { ...status, total_bytes: "10" }, { ...status, runtime_ready: "true" }, { ...status, package_ready: "true" },
    { ...status, error: { message: "oops" } },
  ]) assert.throws(() => parse(invalid));
});

test("installation action follows reported state without claiming activation", async () => {
  class Element {
    constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; this.disabled = false; }
    append(...children) { this.children.push(...children.filter((child) => child != null)); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    setAttribute() {}
    click() { if (!this.disabled) this.listeners.get("click")?.(); }
    get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  }
  const dom = vm.createContext({ document: { createElement: (tag) => new Element(tag) },
    window: { setTimeout: () => 1, clearTimeout() {} }, Error, Number, Object, Math });
  for (const source of ["src/model.ts", "src/ui.ts", "src/rtx_hdr.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; Amaterasu.literalText = value => value;", dom);
  }
  let current = { ...status };
  const calls = [];
  const area = new Element("div");
  dom.Amaterasu.renderRtxHdrIntegration({ api: { allows: () => true, async post(route) {
    calls.push(route);
    if (route === "session-extensions/list") return { items: [], next: null, revision: 1 };
    if (route === "rtx-hdr/install") current = current.phase === "idle"
      ? { ...status, phase: "failed", error: "rtx_hdr_package_unavailable", runtime_ready: true, provider_active: null }
      : { ...status, phase: "ready", runtime_ready: true, package_ready: true, provider_active: null };
    return route === "rtx-hdr/status" ? current : { ok: true };
  } } }, area);
  await new Promise(setImmediate);
  const all = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? all(child) : [])];
  assert.match(area.textContent, /Not installed/);
  all(area).find((node) => node.tagName === "button" && node.textContent === "Prepare RTX HDR").click();
  await new Promise(setImmediate);
  assert.deepEqual(calls, ["rtx-hdr/status", "session-extensions/list", "rtx-hdr/install", "rtx-hdr/status"]);
  assert.match(area.textContent, /Preparation failed/);
  assert.match(area.textContent, /Check the approved repositories and connection/);
  const retry = all(area).find((node) => node.tagName === "button" && node.textContent === "Prepare RTX HDR");
  assert.equal(retry.disabled, false);
  retry.click();
  await new Promise(setImmediate);
  assert.match(area.textContent, /Plugin package staged/);
  assert.match(area.textContent, /plugin package are staged/);
  assert.equal(all(area).find((node) => node.tagName === "button" && node.textContent === "Prepare RTX HDR").disabled, true);
  assert.equal(all(area).some((node) => node.tagName === "button" && node.textContent === "Configure"), false);
  const activate = all(area).find((node) => node.tagName === "button" && node.textContent === "Activate RTX HDR");
  assert.ok(activate);
  assert.equal(activate.disabled, false);
  activate.click();
  await new Promise(setImmediate);
  assert.ok(calls.includes("rtx-hdr/activate"));
  assert.equal(all(area).find((node) => node.tagName === "button" && node.textContent === "Activate RTX HDR").disabled, true);
});

test("settings descriptor enables configuration without claiming the provider is active", async () => {
  class Element {
    constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; this.disabled = false; }
    append(...children) { this.children.push(...children.filter((child) => child != null)); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    setAttribute() {}
    click() { if (!this.disabled) this.listeners.get("click")?.(); }
    get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  }
  const dom = vm.createContext({ document: { createElement: tag => new Element(tag) },
    window: { setTimeout: () => 1, clearTimeout() {} }, Error, Number, Object, Math, Set });
  for (const source of ["src/model.ts", "src/ui.ts", "src/rtx_hdr.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; Amaterasu.literalText = value => value;", dom);
  }
  const dialogs = [];
  dom.showSessionExtensionSettings = (...args) => dialogs.push(args);
  const area = new Element("div");
  dom.Amaterasu.renderRtxHdrIntegration({ api: { allows: () => true, async post(route) {
    if (route === "rtx-hdr/status") return { ...status, runtime_ready: true, package_ready: true, phase: "ready", provider_active: null };
    if (route === "session-extensions/list") return { items: [{ provider: "amaterasu.rtx-hdr", options: [{ allowed_scopes: ["host"] }] }], next: null, revision: 1 };
    throw new Error(`unexpected route ${route}`);
  } } }, area);
  await new Promise(setImmediate);
  const all = node => [node, ...node.children.flatMap(child => child instanceof Element ? all(child) : [])];
  assert.match(area.textContent, /provider is loaded/);
  const configure = all(area).find(node => node.tagName === "button" && node.textContent === "Configure");
  assert.ok(configure);
  configure.click();
  assert.equal(dialogs[0][3], "amaterasu.rtx-hdr");
});


test("pending activation waits for the current stream and can be cancelled", async () => {
  class Element {
    constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; this.disabled = false; }
    append(...children) { this.children.push(...children.filter((child) => child != null)); }
    replaceChildren(...children) { this.children = []; this.append(...children); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    setAttribute() {}
    click() { if (!this.disabled) this.listeners.get("click")?.(); }
    get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  }
  const dom = vm.createContext({ document: { createElement: tag => new Element(tag) },
    window: { setTimeout: () => 1, clearTimeout() {} }, Error, Number, Object, Math, Set });
  for (const source of ["src/model.ts", "src/ui.ts", "src/rtx_hdr.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; Amaterasu.literalText = value => value;", dom);
  }
  let current = { ...status, runtime_ready: true, package_ready: true, phase: "waiting_stream", provider_active: null };
  const calls = []; const area = new Element("div");
  dom.Amaterasu.renderRtxHdrIntegration({ api: { allows: () => true, async post(route) {
    calls.push(route);
    if (route === "session-extensions/list") return { items: [], next: null, revision: 1 };
    if (route === "rtx-hdr/cancel") current = { ...current, phase: "ready" };
    return route === "rtx-hdr/status" ? current : { ok: true };
  } } }, area);
  await new Promise(setImmediate);
  const all = node => [node, ...node.children.flatMap(child => child instanceof Element ? all(child) : [])];
  assert.match(area.textContent, /current stream continues unchanged/);
  assert.match(area.textContent, /game session to end/);
  const cancel = all(area).find(node => node.tagName === "button" && node.textContent === "Cancel activation");
  assert.ok(cancel); assert.equal(cancel.disabled, false); cancel.click();
  await new Promise(setImmediate);
  assert.ok(calls.includes("rtx-hdr/cancel"));
  assert.match(area.textContent, /Plugin package staged/);
});
