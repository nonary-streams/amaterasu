import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tagName) { this.tagName = tagName; this.children = []; this.listeners = new Map(); this.attributes = {}; this.isConnected = false; }
  setConnected(value) { this.isConnected = value; for (const child of this.children) child?.setConnected?.(value); }
  append(...children) { for (const child of children) { if (child == null) continue; this.children.push(child); child?.setConnected?.(this.isConnected); } }
  replaceChildren(...children) { for (const child of this.children) child?.setConnected?.(false); this.children = []; this.append(...children); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  addEventListener(event, callback) { this.listeners.set(event, callback); }
  click() { this.listeners.get("click")?.(); }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
}

const storage = new Map();
let rejectStorage = false;
const sandbox = vm.createContext({ document: { createElement: (tag) => new Element(tag), createTextNode: (text) => text },
  window: { localStorage: {getItem(key) { if (rejectStorage) throw Error("blocked"); return storage.get(key) ?? null; }, setItem(key, value) { if (rejectStorage) throw Error("blocked"); storage.set(key, value); }} },
  Map, Set, JSON, Number, Error, encodeURIComponent });
for (const source of ["src/ui.ts", "src/diagnostic_alerts.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), sandbox);
  vm.runInContext("Object.assign(globalThis, Amaterasu)", sandbox);
}
const {streamCompatibilityAlerts, renderDiagnosticAlerts} = sandbox.Amaterasu;
function warning(flags = "0x00000010", date = "2026-11-13 14:00:00") {
  return `[${date}]: Warning: amaterasu.rtsp::rtsp.negotiation: Optional client features ${flags} are unsupported and ignored; streaming can continue.\n`;
}
function find(root, label) {
  if (root.tagName === "button" && root.textContent === label) return root;
  for (const child of root.children ?? []) { if (typeof child !== "object") continue; const result = find(child, label); if (result) return result; }
}
function page(host = "https://host.test", allowed = true) {
  const target = new Element("div");
  const observer = renderDiagnosticAlerts({api: {hostOrigin: host, allows: () => allowed}}, target);
  return {target, observer};
}

test("recognizes actual source-formatted warning, deduplicates and ignores other prose and invalid masks", () => {
  const contents = warning() + warning("0x00000010", "2026-11-13 15:00:00") + warning("0xfffffff0") + warning("0x00000000") + warning("0x00000001")
    + warning().replace("amaterasu.rtsp::", "untrusted::") + warning().replace("Warning:", "Info:") + "prefix " + warning();
  const alerts = streamCompatibilityAlerts(contents);
  assert.equal(alerts.length, 2);
  assert.equal(alerts[0].occurrences, 2);
  assert.equal(alerts[0].lastSeen, "2026-11-13 15:00:00");
  assert.equal(alerts[1].featureFlags, "0xfffffff0");
  assert.equal(streamCompatibilityAlerts(Array.from({length: 50}, (_, i) => warning(`0x${((i + 1) * 16).toString(16).padStart(8, "0")}`)).join("")).length, 32);
});

test("dismiss and restore are persistent per host; new feature masks remain visible", () => {
  storage.clear();
  const first = page();
  first.observer.snapshot(warning() + warning());
  assert.match(first.target.textContent, /Some optional client features are unavailable/);
  assert.match(first.target.textContent, /Seen 2 time/);
  find(first.target, "Dismiss").click();
  assert.doesNotMatch(first.target.textContent, /Some optional client features are unavailable/);
  const reloaded = page();
  reloaded.observer.snapshot(warning());
  assert.match(reloaded.target.textContent, /No undismissed/);
  reloaded.observer.snapshot(warning() + warning("0x00000020"));
  assert.match(reloaded.target.textContent, /Feature flags: 0x00000020/);
  assert.doesNotMatch(reloaded.target.textContent, /Feature flags: 0x00000010/);
  const other = page("https://other.test");
  other.observer.snapshot(warning());
  assert.match(other.target.textContent, /Feature flags: 0x00000010/);
  find(reloaded.target, "Restore dismissed alerts").click();
  assert.match(reloaded.target.textContent, /Feature flags: 0x00000010/);
});

test("loading, permission, failure, and unavailable storage are honest and nonblocking", () => {
  storage.clear();
  const loaded = page();
  assert.match(loaded.target.textContent, /Checking the retained/);
  assert.doesNotMatch(loaded.target.textContent, /No undismissed/);
  loaded.observer.snapshot(warning());
  loaded.observer.unavailable();
  assert.match(loaded.target.textContent, /could not be refreshed/);
  assert.match(loaded.target.textContent, /Feature flags: 0x00000010/);
  assert.match(page("https://host.test", false).target.textContent, /read access is required/);
  rejectStorage = true;
  const blocked = page();
  blocked.observer.snapshot(warning());
  find(blocked.target, "Dismiss").click();
  assert.match(blocked.target.textContent, /only while this page is open/);
  assert.match(blocked.target.textContent, /No undismissed/);
  rejectStorage = false;
});

test("malformed persisted data is ignored and raw log text never becomes alert markup", () => {
  storage.clear();
  storage.set("amaterasu.diagnostic-alert-dismissals.v1.https%3A%2F%2Fhost.test", JSON.stringify([{}, "<script>", "optional-client-features:0x00000010"]));
  const loaded = page();
  loaded.observer.snapshot(warning() + "<script>alert(1)</script>");
  assert.match(loaded.target.textContent, /No undismissed/);
  assert.doesNotMatch(loaded.target.textContent, /<script>/);
});

test("structured stream adjustments appear independently of the stats plugin", () => {
  const record = {event:"stream_adjustment", stream:"rtsp-0000000000000001", code:"refresh_hint_ignored", requested:6000, effective:0};
  const line = `[2026-11-15 12:00:00]: Warning: amaterasu.rtsp::rtsp.compatibility: ${JSON.stringify(record)}\n`;
  const alerts = streamCompatibilityAlerts(line);
  assert.equal(alerts.length, 1);
  assert.equal(alerts[0].adjustment.stream, record.stream);
  const view = page();
  view.observer.snapshot(line);
  assert.match(view.target.textContent, /inconsistent display refresh/);
  assert.match(view.target.textContent, /rtsp-0000000000000001/);
  assert.equal(streamCompatibilityAlerts(line.replace("amaterasu.rtsp::", "untrusted::")).length, 0);
});

test("native initialization fallback includes its reason without trusting other modules", () => {
  const record = {event:"stream_adjustment", stream:"rtsp-0000000000000002", code:"encoder_fallback", requested:1, effective:0, reason:"nvenc_open_failed"};
  const line = `[2026-11-15 12:00:00]: Warning: amaterasu.stream-session::stream.compatibility: ${JSON.stringify(record)}\n`;
  const view = page();
  view.observer.snapshot(line);
  assert.match(view.target.textContent, /compatible encoder/);
  assert.match(view.target.textContent, /nvenc_open_failed/);
  assert.equal(streamCompatibilityAlerts(line.replace("amaterasu.stream-session", "untrusted")).length, 0);
});

test("invalid optional hints are shown without exposing their raw values", () => {
  const record = {event:"stream_adjustment", stream:"rtsp-0000000000000003", code:"invalid_optional_hints", requested:3, effective:0};
  const line = `[2026-11-15 12:00:00]: Warning: amaterasu.rtsp::rtsp.compatibility: ${JSON.stringify(record)}\n`;
  const view = page();
  view.observer.snapshot(line);
  assert.match(view.target.textContent, /Invalid optional client hints were ignored/);
  assert.match(view.target.textContent, /Ignored hint mask: 0x3/);
});

test("structured feature warnings remain visible without a history plugin", () => {
  const record = {event:"stream_warning", stream:"rtsp-0000000000000004", warning:{code:"proton_policy_unavailable",source:"amaterasu.nvhttp/1",reason:"read_unavailable",effective:false}};
  const line = `[2026-11-15 12:00:00]: Warning: amaterasu.nvhttp::stream.warning: ${JSON.stringify(record)}\n`;
  const view = page();
  view.observer.snapshot(line);
  assert.match(view.target.textContent, /controller compatibility preference was unavailable/);
  assert.match(view.target.textContent, /Used: false/);
  assert.equal(streamCompatibilityAlerts(line.replace("amaterasu.nvhttp", "untrusted")).length, 0);
});

test("application output warnings include a bounded OS error", () => {
  const record = {event:"stream_warning", stream:"rtsp-0000000000000005", warning:{code:"application_output_unavailable",source:"example.application/1",reason:"output_open_failed",os_error:13,effective:"discarded"}};
  const line = `[2026-11-15 12:00:00]: Warning: amaterasu.nvhttp::stream.warning: ${JSON.stringify(record)}\n`;
  const view = page();
  view.observer.snapshot(line);
  assert.match(view.target.textContent, /application continued without that output/);
  assert.match(view.target.textContent, /OS error: 13/);
});

test("invalid client hint alerts identify the ignored field", () => {
  const record = {event:"stream_warning",stream:"rtsp-0000000000000006",warning:{code:"invalid_client_hint",source:"amaterasu.nvhttp/1",reason:"clientVrrRequested",requested:2,effective:"ignored"}};
  const view = page();
  view.observer.snapshot(`[2026-11-15 12:00:00]: Warning: amaterasu.nvhttp::stream.warning: ${JSON.stringify(record)}\n`);
  assert.match(view.target.textContent, /invalid optional client hint was ignored/i);
  assert.match(view.target.textContent, /clientVrrRequested/);
  assert.match(view.target.textContent, /Requested: 2/);
});
