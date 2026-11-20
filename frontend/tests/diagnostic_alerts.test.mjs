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
const {lifecycleAlerts, retainedStreamWarnings, renderDiagnosticAlerts} = sandbox.Amaterasu;
const entry = (operation, requested = 16) => ({at_ms: 1000, publisher: "amaterasu.rtsp",
  issue: {operation, disposition:"advisory_warning", phase:"announce", code:"optional_client_features", requested, effective:0}});
function reply(warnings) {
  return {ok:true, issues:warnings, incomplete:false};
}

test("alerts use typed issues and distinguish tickets, refusals, values and incomplete streams", () => {
  const value = reply([entry("one"), entry("two"), entry("one"), entry("one", 32)]);
  value.issues.push(entry("refused"));
  value.issues.push(entry("unbound"));
  value.incomplete = true;
  const result = lifecycleAlerts(value);
  assert.equal(result.alerts.length, 5);
  assert.equal(result.incomplete, true);
  assert.match(result.alerts[0].message, /Requested: 0x10/);
  assert.throws(() => lifecycleAlerts("Optional client features 0x00000010 are unsupported and ignored; streaming can continue."));
});

test("repeat commits show their occurrence count and latest time", () => {
  const first = {...entry("attempt"), seq: 2, at_ms: 1200};
  const repeated = {...entry("attempt"), seq: 3, at_ms: 1600};
  const value = reply([repeated, first]);
  const [alert] = lifecycleAlerts(value).alerts;
  assert.equal(alert.occurrences, 2);
  assert.equal(alert.at, 1600);
});

test("native failures show the service, call and error without grouping different errors", () => {
  const first = entry("attempt");
  first.issue = {...first.issue, disposition: "critical_failure", code: "frame_processor_failed",
    native_failure: {service: "amaterasu.rtx-hdr/1", call: "process", status: 13, capture_generation: 3}};
  const second = {...first, issue: {...first.issue, native_failure: {...first.issue.native_failure, status: -79}}};
  const {alerts} = lifecycleAlerts(reply([first, second]));
  assert.equal(alerts.length, 2);
  assert.match(alerts[0].message, /amaterasu\.rtx-hdr\/1 returned native status 13 from process/);
  assert.match(alerts[0].message, /Capture offer generation 3/);
  assert.match(alerts[1].message, /native status -79/);
  const fallback = {...first, issue: {...first.issue, native_failure: {status: "invalid"}}};
  const [baseAlert] = lifecycleAlerts(reply([fallback])).alerts;
  assert.match(baseAlert.message, /frame processor failed/);
});

test("history reads only typed warning events", () => {
  const detail = {events:[{event_type:"stream_warnings", payload:{warnings:[entry("one").issue]}},
    {event_type:"log", payload:{message:"Optional client features"}}], events_truncated:true};
  assert.match(retainedStreamWarnings(detail), /Requested: 0x10/);
  assert.match(retainedStreamWarnings(detail), /earlier events are unavailable/);
});

test("session access alone reads warnings without a host log", async () => {
  sandbox.window.setTimeout = () => 1;
  sandbox.window.clearTimeout = () => {};
  const target = new Element("div"); target.setConnected(true);
  const routes = [];
  await renderDiagnosticAlerts({api:{allows: scope => scope === "session.read", hostOrigin:"host",
    post: async (route, query) => {assert.equal(query.issues_only, true); routes.push(route); return reply([entry("one")]);}}}, target)();
  assert.deepEqual(routes, ["nvhttp/launch_status"]);
  assert.match(target.textContent, /Requested: 0x10/);
});
