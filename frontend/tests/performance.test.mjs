import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class ClassList {
  constructor(owner) { this.owner = owner; }
  add(name) { if (!this.owner.className.split(" ").includes(name)) this.owner.className += ` ${name}`; }
  remove(name) { this.owner.className = this.owner.className.split(" ").filter((item) => item !== name).join(" "); }
}
class Element {
  constructor(tagName) {
    this.tagName = tagName; this.children = []; this.listeners = new Map(); this.attributes = {};
    this.style = {}; this.className = ""; this.hidden = false; this.isConnected = false; this.classList = new ClassList(this);
  }
  setConnected(value) { this.isConnected = value; for (const child of this.children) child?.setConnected?.(value); }
  append(...children) { for (const child of children) { if (child == null) continue; this.children.push(child); child?.setConnected?.(this.isConnected); } }
  replaceChildren(...children) { for (const child of this.children) child?.setConnected?.(false); this.children = []; this.append(...children); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  addEventListener(event, callback) { this.listeners.set(event, callback); }
  click() { this.listeners.get("click")?.(); }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
}
const timers = new Map();
let nextTimer = 1;
const document = {
  createElement: (tag) => new Element(tag),
  createElementNS: (_namespace, tag) => new Element(tag),
  createTextNode: (text) => text,
  visibilityState: "visible",
};
const window = {
  setTimeout(callback, delay) { const id = nextTimer++; timers.set(id, { callback, delay }); return id; },
  clearTimeout(id) { timers.delete(id); },
};
const sandbox = vm.createContext({ document, window, Date, Math, Error, Map, Set, Number, String, Array, Object });
for (const source of ["src/ui.ts", "src/performance.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), sandbox);
  if (source === "src/ui.ts")
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = (value) => value; globalThis.messageOf = (error) => error.message", sandbox);
}
const {
  parsePerformanceReply, performanceView, performanceContent, renderPerformance,
  formatPerformanceDuration, performancePluginName,
} = sandbox.Amaterasu;
// Values built inside the VM belong to another realm; compare their plain data.
const plain = (value) => JSON.parse(JSON.stringify(value));
const descendants = (element) => [element, ...element.children.flatMap((child) => child instanceof Element ? descendants(child) : [])];
const byClass = (element, name) => descendants(element).filter((node) => node.className.split(" ").includes(name));
async function wait(predicate) {
  for (let attempt = 0; attempt < 200; attempt++) { if (predicate()) return; await new Promise((resolve) => setTimeout(resolve, 1)); }
  assert.fail("timed out waiting for the performance view");
}
test.beforeEach(() => { timers.clear(); document.visibilityState = "visible"; });

const NOW = 1_800_000_000_000;
const metric = (stage, calls, totalUs, maxUs, p95 = maxUs) =>
  ({ stage, calls, total_us: totalUs, max_us: maxUs, mean_us: Math.round(totalUs / calls), p95_upper_us: p95 });
function windowReport(ageMs, overrides = {}) {
  return {
    observed_unix_ms: NOW - ageMs, window_us: 10_000_000, dropped_records: 0,
    tracks: [{
      track: "input", completed: 1000, slow: 0,
      total: metric("total", 1000, 900_000, 2_100, 2_000),
      steps: [
        metric("session.submitted>input.accepted", 1000, 100_000, 900, 250),
        metric("socket.arrived>control.authenticated", 1000, 700_000, 1_900, 2_000),
        metric("control.authenticated>session.dequeued", 1000, 100_000, 300, 100),
      ],
    }],
    spans: [
      metric("host.pass", 5000, 2_500_000, 3_900),
      metric("host.tick:amaterasu.linux-session-video", 600, 480_000, 3_600),
      metric("host.health:amaterasu.linux-session-video", 2, 700, 400),
      metric("host.tick:vendor.overlay-plugin", 3, 1_200, 500),
      metric("input.batch:direct", 1000, 60_000, 900),
    ],
    ...overrides,
  };
}
function reply(trace) {
  return { ok: true, supported: true, trace: { schema: 1, collecting: true, windows: [], trend: [], slow_flows: [], slow_spans: [], ...trace } };
}

test("a healthy host reads as smooth and names the typical time in plain words", () => {
  const view = performanceView(parsePerformanceReply(reply({ windows: [windowReport(5_000), windowReport(15_000)] })), NOW);
  const input = view.tracks.find((track) => track.track === "input");
  assert.equal(input.title, "Input response");
  assert.equal(input.verdict.tone, "good");
  assert.equal(input.verdict.label, "Smooth");
  assert.equal(input.typicalUs, 900);
  assert.equal(input.summary, "A typical input event spends 0.9 ms inside the host.");
  assert.equal(view.host.verdict.label, "Responsive");
  assert.equal(view.host.summary, "No plugin held up the others for long enough to notice.");
  assert.equal(Math.round(view.host.passesPerSecond), 500);
  // A track with no traffic is idle, never "good".
  const video = view.tracks.find((track) => track.track === "video");
  assert.equal(video.verdict.tone, "idle");
  assert.equal(video.summary, "No frames passed through the host in the last minute.");
});

test("steps follow the path of an event and share the time between them", () => {
  const view = performanceView(parsePerformanceReply(reply({ windows: [windowReport(5_000)] })), NOW);
  const steps = view.tracks.find((track) => track.track === "input").steps;
  assert.deepEqual(plain(steps.map((step) => step.label)), [
    "Waiting to be picked up", "Reaching the stream session", "Injecting into the system",
  ]);
  // Colour follows the step, not its rank: slots are fixed by the known path.
  assert.deepEqual(plain(steps.map((step) => step.slot)), [1, 2, 4]);
  assert.equal(Math.round(steps[0].share * 100), 78);
  assert.equal(steps[0].meanUs, 700);
  assert.equal(steps[0].p95Us, 2_000);
});

test("plugins are ranked by how long they kept the others waiting", () => {
  const busy = windowReport(5_000, { spans: [
    metric("host.pass", 5000, 2_500_000, 23_000),
    metric("host.tick:amaterasu.state", 1, 21_400, 21_400),
    metric("host.tick:amaterasu.session-history", 2, 12_000, 9_700),
    metric("host.tick:vendor.overlay-plugin", 3, 1_200, 500),
  ] });
  const view = performanceView(parsePerformanceReply(reply({ windows: [busy] })), NOW);
  assert.deepEqual(plain(view.plugins.map((plugin) => [plugin.name, plugin.worstUs])), [
    ["Saved state", 21_400], ["Session history", 9_700], ["Vendor overlay plugin", 500],
  ]);
  assert.equal(view.host.verdict.label, "Long pauses");
  assert.equal(view.host.summary, "Saved state paused everything for up to 21 ms.");
  // Tick and health calls of one plugin are one row.
  const calm = performanceView(parsePerformanceReply(reply({ windows: [windowReport(5_000)] })), NOW);
  assert.equal(calm.plugins[0].name, "Video pipeline");
  assert.equal(calm.plugins[0].longCalls, 602);
  assert.equal(calm.plugins.length, 2, "spans that are not host calls are not plugins");
});

test("slow flows count against the verdict and old windows are left out", () => {
  const delayed = windowReport(5_000);
  delayed.tracks[0].slow = 4;
  const stale = windowReport(200_000);
  stale.tracks[0].slow = 900;
  const view = performanceView(parsePerformanceReply(reply({ windows: [stale, delayed] })), NOW);
  const input = view.tracks.find((track) => track.track === "input");
  assert.equal(input.completed, 1000);
  assert.equal(input.verdict.label, "Occasional delays");
  assert.equal(input.summary, "4 of 1,000 input events took longer than they should have.");
  delayed.tracks[0].slow = 40;
  const frequent = performanceView(parsePerformanceReply(reply({ windows: [delayed] })), NOW);
  assert.equal(frequent.tracks[0].verdict.label, "Frequent delays");
});

test("a slow moment says which step took the time and which plugin was busy", () => {
  const view = performanceView(parsePerformanceReply(reply({
    slow_flows: [{
      observed_unix_ms: NOW - 20_000, track: "input", total_us: 23_412, budget_us: 8_000,
      steps: [
        { from: "socket.arrived", to: "control.authenticated", us: 19_810 },
        { from: "control.authenticated", to: "session.dequeued", us: 3_602 },
      ],
      slowest_step: { from: "socket.arrived", to: "control.authenticated", us: 19_810, percent: 84,
        during: [{ span: "host.tick", label: "amaterasu.state", us: 21_438, overlap_us: 19_600 }] },
    }, {
      observed_unix_ms: NOW - 90_000, track: "video", total_us: 14_000, budget_us: 12_000,
      steps: [{ from: "video.packet_queued", to: "session.packet_read", us: 14_000 }],
      slowest_step: { from: "video.packet_queued", to: "session.packet_read", us: 14_000, percent: 100,
        during: [{ span: "host.wait", label: null, us: 2_000, overlap_us: 1_900 }] },
    }],
    slow_spans: [{ observed_unix_ms: NOW - 21_000, span: "host.tick", label: "amaterasu.state", us: 21_438 }],
  })), NOW);
  assert.deepEqual(plain(view.moments.map((moment) => moment.headline)), [
    "An input event took 23 ms to reach the system",
    "Saved state held the host for 21 ms",
    "A video frame took 14 ms to leave the host",
  ]);
  assert.equal(view.moments[0].detail,
    "Most of it (84%) was “waiting to be picked up”. Saved state was busy for 21 ms at that moment.");
  assert.match(view.moments[2].detail, /The host was idle between checks/);
  assert.deepEqual(plain(view.moments[0].steps.map((step) => step.slot)), [1, 2]);
});

test("malformed and oversized replies are bounded instead of trusted", () => {
  assert.equal(parsePerformanceReply({ ok: true, supported: false, trace: null }), null);
  assert.throws(() => parsePerformanceReply({ ok: false, code: "performance_trace_unavailable" }));
  assert.throws(() => parsePerformanceReply({ ok: true, supported: true, trace: { schema: 2 } }));
  const hostile = parsePerformanceReply(reply({
    windows: [{ observed_unix_ms: "soon", window_us: -5, tracks: "none", spans: [null, 7, { stage: "x".repeat(900), calls: Infinity }] }],
    slow_spans: Array.from({ length: 5_000 }, () => ({ span: "host.tick", us: 9_000 })),
  }));
  assert.equal(hostile.windows[0].observedUnixMs, 0);
  assert.equal(hostile.windows[0].windowUs, 0);
  assert.deepEqual(plain(hostile.windows[0].tracks), []);
  assert.equal(hostile.windows[0].spans.length, 1);
  assert.equal(hostile.windows[0].spans[0].name.length, 160);
  assert.equal(hostile.windows[0].spans[0].calls, 0);
  assert.equal(hostile.slowSpans.length, 400);
});

test("durations and plugin names are written for people", () => {
  assert.equal(formatPerformanceDuration(40), "under 0.1 ms");
  assert.equal(formatPerformanceDuration(940), "0.9 ms");
  assert.equal(formatPerformanceDuration(4_700), "4.7 ms");
  assert.equal(formatPerformanceDuration(23_412), "23 ms");
  assert.equal(formatPerformanceDuration(1_250_000), "1.3 s");
  assert.equal(performancePluginName("amaterasu.stream-session"), "Stream session");
  assert.equal(performancePluginName("amaterasu.some-new_module"), "Some new module");
  assert.equal(performancePluginName("vendor.overlay-plugin"), "Vendor overlay plugin");
});

test("every figure in a chart is also readable without hovering", () => {
  const busy = windowReport(5_000);
  busy.spans.push(metric("host.tick:amaterasu.state", 1, 21_400, 21_400));
  const content = performanceContent(performanceView(parsePerformanceReply(reply({
    windows: [busy],
    trend: [20_000, 10_000, 0].map((age) => ({ observed_unix_ms: NOW - age, tracks: [{ track: "input", p95_upper_us: 2_000, slow: 0 }] })),
  })), NOW));
  // The plugin list prints each worst pause and flags the noticeable ones with a label.
  const rows = byClass(content, "perf-plugin");
  assert.match(rows[0].textContent, /Saved state.*21 ms at worst.*Long pause/);
  assert.match(rows[0].attributes["aria-label"], /Saved state: held the host for 21 ms at worst/);
  assert.equal(rows[0].children.length, rows[1].children.length, "rows share one grid so bars share one scale");
  // The segmented bar has a legend and a table with the same steps.
  const segments = byClass(content, "perf-segment");
  assert.equal(segments.length, 3);
  assert.equal(segments[0].textContent, "78%");
  assert.equal(segments[1].textContent, "11%");
  assert.match(byClass(content, "perf-segments")[0].attributes["aria-label"], /Waiting to be picked up 78%/);
  assert.equal(byClass(content, "perf-legend")[0].children.length, 3);
  const table = descendants(content).find((node) => node.tagName === "table");
  assert.match(table.textContent, /Waiting to be picked up78%0\.7 ms2\.0 ms1\.9 ms1000socket\.arrived>control\.authenticated/);
  // Status is never colour alone: a mark and a label accompany it.
  const verdicts = byClass(content, "perf-verdict").map((node) => node.textContent);
  assert.ok(verdicts.includes("✓Smooth") && verdicts.includes("✕Long pauses"), verdicts.join(" | "));
  // The sparkline carries a text alternative.
  const sparkline = descendants(content).find((node) => node.attributes.class === "perf-sparkline");
  assert.match(sparkline.attributes["aria-label"], /95% within 2\.0 ms now/);
});

test("the panel loads once, keeps refreshing while visible, and stops when removed", async () => {
  const target = new Element("div");
  target.setConnected(true);
  const calls = [];
  const api = { async post(route, payload) { calls.push([route, payload]); return reply({ windows: [windowReport(5_000)] }); } };
  const refresh = renderPerformance({ api }, target);
  await wait(() => byClass(target, "perf-tile").length === 3);
  assert.deepEqual(calls, [["diagnostics/performance", undefined]]);
  assert.match(byClass(target, "perf-status")[0].textContent, /refreshes every 10 seconds/);

  const [timerId, timer] = [...timers][0];
  assert.equal(timer.delay, 10_000);
  timers.delete(timerId);
  timer.callback();
  await wait(() => calls.length === 2);
  await refresh();
  assert.equal(calls.length, 3);
  await wait(() => timers.size === 1);

  // A hidden tab waits instead of polling; a removed panel stops for good.
  document.visibilityState = "hidden";
  let [, pending] = [...timers][0];
  timers.clear();
  pending.callback();
  assert.equal(calls.length, 3);
  assert.equal(timers.size, 1);
  target.setConnected(false);
  [, pending] = [...timers][0];
  timers.clear();
  pending.callback();
  assert.equal(timers.size, 0);
  assert.equal(calls.length, 3);
});

test("an older host and a failed request are explained instead of shown as healthy", async () => {
  const target = new Element("div");
  target.setConnected(true);
  renderPerformance({ api: { async post() { return { ok: true, supported: false, trace: null }; } } }, target);
  await wait(() => /Performance tracing unavailable/.test(target.textContent));

  const failing = new Element("div");
  failing.setConnected(true);
  renderPerformance({ api: { async post() { throw new Error("The host is restarting."); } } }, failing);
  await wait(() => /The host is restarting\./.test(failing.textContent));
  assert.equal(byClass(failing, "perf-tile").length, 0);
});
