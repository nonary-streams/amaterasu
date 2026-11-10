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
  parsePerformanceReply, performanceContent, performanceMomentText, renderPerformance,
  formatPerformanceDuration, performancePluginName,
} = sandbox.Amaterasu;
// Values built inside the VM belong to another realm; compare their plain data.
const plain = (value) => JSON.parse(JSON.stringify(value));
const descendants = (element) => [element, ...element.children.flatMap((child) => child instanceof Element ? descendants(child) : [])];
const classesOf = (node) => `${node.className} ${node.attributes.class ?? ""}`.split(" ");
const byClass = (element, name) => descendants(element).filter((node) => classesOf(node).includes(name));
const byTag = (element, tag) => descendants(element).filter((node) => node.tagName === tag);
const rowsOf = (table) => byTag(table, "tr").slice(1).map((row) => row.children.map((cell) => cell.textContent));
async function wait(predicate) {
  // Generous: other suites and builds may be using the machine.
  for (let attempt = 0; attempt < 3000; attempt++) { if (predicate()) return; await new Promise((resolve) => setTimeout(resolve, 1)); }
  assert.fail("timed out waiting for the performance panel");
}
test.beforeEach(() => { timers.clear(); document.visibilityState = "visible"; });

const NOW = 1_800_000_000_000;
const metric = (stage, calls, meanUs, p95Us, maxUs) =>
  ({ stage, calls, mean_us: meanUs, p95_upper_us: p95Us, max_us: maxUs, total_us: calls * meanUs });
function report(overrides = {}) {
  return {
    schema: 2, collecting: true,
    recent: {
      window_us: 60_000_000, dropped_records: 0,
      tracks: [
        { track: "input", completed: 4_000, slow: 3, abandoned: 0, budget_us: 8_000, total: metric("total", 4_000, 1_300, 4_000, 141_000),
          steps: [
            metric("socket.arrived>control.authenticated", 4_000, 1_000, 2_000, 116_000),
            metric("control.authenticated>session.dequeued", 4_000, 250, 1_000, 3_700),
            metric("session.submitted>input.accepted", 4_000, 50, 100, 260),
          ] },
        { track: "video", completed: 2_000, slow: 0, abandoned: 0, budget_us: 12_000, total: metric("total", 2_000, 5_800, 8_000, 11_000),
          steps: [metric("video.encoder_submitted>video.packet_queued", 2_000, 5_000, 8_000, 10_500)] },
      ],
      host: { pass: metric("host.pass", 30_000, 500, 2_000, 121_000), wait: metric("host.wait", 30_000, 1_200, 1_900, 1_900) },
      spans: [
        metric("host.tick:amaterasu.session-lifecycle", 30, 400, 1_000, 119_000),
        metric("input.batch:direct", 3_900, 40, 100, 170),
        metric("host.tick:amaterasu.pairing", 40, 4_300, 9_000, 9_100),
        metric("host.health:amaterasu.state", 6, 300, 400, 420),
        metric("host.tick:vendor.overlay", 5, 300, 500, 512),
      ],
      spans_omitted: 0,
    },
    trend: {
      newest_unix_ms: NOW - 2_000, age_s: [40, 30, 20, 10, 0],
      tracks: {
        input: { budget_us: 8_000, p95_upper_us: [2_000, 2_000, 4_000, 2_000, 2_000], max_us: [2_100, null, 141_000, 8_000, 3_000] },
        video: { budget_us: 12_000, p95_upper_us: [8_000, 8_000, 8_000, 8_000, 8_000], max_us: [9_000, 9_500, 11_000, 9_000, 9_100] },
      },
      host_pass_max_us: [2_000, 2_100, 121_000, 9_100, 2_000],
    },
    slow: [
      { kind: "span", observed_unix_ms: NOW - 5_000, until_unix_ms: NOW - 5_000, count: 1, span: "host.tick", label: "amaterasu.pairing",
        by: "@amaterasu.host-lifecycle", us: 9_100 },
      { kind: "flow", observed_unix_ms: NOW - 21_000, until_unix_ms: NOW - 20_990, count: 18, track: "input", us: 141_000, budget_us: 8_000,
        step: "control.authenticated>session.dequeued", step_us: 122_000,
        held_by: { span: "host.tick", label: "amaterasu.session-lifecycle", us: 119_000 } },
      { kind: "flow", observed_unix_ms: NOW - 90_000, until_unix_ms: NOW - 30_000, count: 1, track: "video", us: 12_400, budget_us: 12_000,
        step: "video.encoder_submitted>video.packet_queued", step_us: 11_900, held_by: null },
    ],
    ...overrides,
  };
}
const reply = (trace = report()) => ({ ok: true, supported: true, trace });

test("a reply is read into a report, and anything else is unsupported or an error", () => {
  assert.equal(parsePerformanceReply({ ok: true, supported: false, trace: null }), null);
  assert.throws(() => parsePerformanceReply({ ok: false, code: "performance_trace_unavailable" }), /did not return/);
  assert.throws(() => parsePerformanceReply(reply({ schema: 1, windows: [] })), /cannot read/);
  assert.throws(() => parsePerformanceReply(null), /did not return/);

  const parsed = plain(parsePerformanceReply(reply()));
  assert.equal(parsed.periodUs, 60_000_000);
  assert.equal(parsed.tracks[0].track, "input");
  assert.equal(parsed.tracks[0].budgetUs, 8_000);
  assert.deepEqual(parsed.tracks[0].steps[2], { stage: "session.submitted>input.accepted", calls: 4_000, meanUs: 50, p95Us: 100, maxUs: 260, totalUs: 200_000 });
  assert.equal(parsed.hostPass.maxUs, 121_000);
  assert.equal(parsed.calls.length, 5);
  assert.deepEqual(parsed.trend.tracks.input.maxUs, [2_100, null, 141_000, 8_000, 3_000]);
  assert.deepEqual(parsed.moments[1].heldBy, { span: "host.tick", label: "amaterasu.session-lifecycle", us: 119_000 });
  assert.equal(parsed.moments[2].heldBy, null);

  // A report that leaves fields out still reads: missing numbers are zero.
  const sparse = plain(parsePerformanceReply(reply({ schema: 2, collecting: false, recent: null, trend: null, slow: [{}] })));
  assert.equal(sparse.periodUs, 0);
  assert.equal(sparse.trend, null);
  assert.deepEqual(sparse.tracks, []);
  assert.equal(sparse.moments[0].count, 1);
});

test("durations and plugin names are written for a reader", () => {
  assert.equal(formatPerformanceDuration(4), "<0.01 ms");
  assert.equal(formatPerformanceDuration(50), "0.05 ms");
  assert.equal(formatPerformanceDuration(1_300), "1.3 ms");
  assert.equal(formatPerformanceDuration(12_400), "12.4 ms");
  assert.equal(formatPerformanceDuration(141_000), "141 ms");
  assert.equal(formatPerformanceDuration(2_500_000), "2.5 s");
  assert.equal(performancePluginName("amaterasu.session-lifecycle"), "Session lifecycle");
  assert.equal(performancePluginName("vendor.overlay-plugin"), "Vendor overlay plugin");
});

test("a slow moment says what was slow, where, and what held the host loop", () => {
  const moments = parsePerformanceReply(reply()).moments;
  assert.equal(performanceMomentText(moments[0]), "Pairing held the host loop, delaying every other plugin.");
  assert.equal(performanceMomentText(moments[1]),
    "18 input events: slowest at reaching the stream session (122 ms), during a 119 ms call by Session lifecycle.");
  assert.equal(performanceMomentText(moments[2]), "One video frame: slowest at encoding (11.9 ms).");
  assert.equal(performanceMomentText({ ...moments[0], count: 4 }), "Pairing held the host loop 4 times, delaying every other plugin.");
});

test("the summary states each track and the host loop, marking values at their limit", () => {
  const content = performanceContent(parsePerformanceReply(reply()));
  const [input, video, host] = byClass(content, "perf-figure");

  assert.equal(byTag(input, "h3")[0].textContent, "Input");
  assert.equal(byClass(input, "perf-value")[0].textContent, "1.3 ms");
  assert.deepEqual(byClass(input, "perf-fact").map((fact) => fact.textContent),
    ["95% within4.0 ms", "Worst141 ms", "Input events4,000", "Took 8 ms or more3"]);
  // The worst input event and the count over the limit are marked; video stayed inside its limit.
  assert.deepEqual(byClass(input, "perf-over").map((node) => node.textContent), ["141 ms", "3"]);
  assert.equal(byClass(video, "perf-value")[0].textContent, "5.8 ms");
  assert.equal(byClass(video, "perf-over").length, 0);

  assert.equal(byClass(host, "perf-value")[0].textContent, "0.50 ms");
  assert.deepEqual(byClass(host, "perf-fact").map((fact) => fact.textContent),
    ["95% within2.0 ms", "Worst pass121 ms", "Passes per second500", "Longest callSession lifecycle, 119 ms"]);
});

test("the trend places each window by its age and marks the ones at or over the limit", () => {
  const content = performanceContent(parsePerformanceReply(reply()));
  const chart = byClass(byClass(content, "perf-figure")[0], "perf-trend-chart")[0];
  const bars = byClass(chart, "perf-trend-bar");
  // Four windows had input; the newest sits in the last of 180 slots.
  assert.deepEqual(bars.map((bar) => Number(bar.attributes.x) - 0.15), [175, 177, 178, 179]);
  // 141 ms and exactly 8 ms are at or over the 8 ms limit; the spike is clipped to full height.
  assert.deepEqual(bars.map((bar) => classesOf(bar).includes("perf-trend-over")), [false, true, true, false]);
  assert.equal(bars[1].attributes.height, "100");
  assert.match(chart.attributes["aria-label"], /2 of 4 periods reached 8 ms/);
  assert.equal(byClass(content, "perf-trend-caption")[0].textContent, "Worst per 10 s, last 30 min. Line at 8 ms.");
});

test("stages list their share of the time, and plugin calls only what ran on the host loop", () => {
  const content = performanceContent(parsePerformanceReply(reply()));
  const [inputStages, videoStages, calls, moments] = byClass(content, "perf-table");

  assert.deepEqual(rowsOf(inputStages), [
    ["Waiting to be read from the networksocket.arrived → control.authenticated", "77%", "1.0 ms", "2.0 ms", "116 ms"],
    ["Reaching the stream sessioncontrol.authenticated → session.dequeued", "19%", "0.25 ms", "1.0 ms", "3.7 ms"],
    ["Injecting into the systemsession.submitted → input.accepted", "4%", "0.05 ms", "0.10 ms", "0.26 ms"],
  ]);
  assert.equal(rowsOf(videoStages)[0][0], "Encodingvideo.encoder_submitted → video.packet_queued");
  // Longest first. The input batch span ran inside a plugin's turn and is not a turn itself.
  assert.deepEqual(rowsOf(calls), [
    ["Session lifecycleamaterasu.session-lifecycle", "", "119 ms", "0.40 ms", "30"],
    ["Pairingamaterasu.pairing", "", "9.1 ms", "4.3 ms", "40"],
    ["Vendor overlayvendor.overlay", "", "0.51 ms", "0.30 ms", "5"],
    ["Saved state (health check)amaterasu.state", "", "0.42 ms", "0.30 ms", "6"],
  ]);
  // Only calls of 8 ms or more are marked.
  assert.deepEqual(byClass(calls, "perf-over").map((cell) => cell.textContent), ["119 ms", "9.1 ms"]);
  assert.equal(rowsOf(moments).length, 3);
  assert.equal(rowsOf(moments)[1][1], "141 ms");
  // A burst within a second shows one time; a run that lasted shows when it began and ended.
  assert.doesNotMatch(rowsOf(moments)[1][0], / to /);
  assert.match(rowsOf(moments)[2][0], /^.+ to .+$/);
});

test("an idle host reports its loop and says that nothing is streaming", () => {
  const idle = report();
  idle.recent.tracks = [];
  idle.slow = [];
  const content = performanceContent(parsePerformanceReply(reply(idle)));
  const [input, video, host] = byClass(content, "perf-figure");
  assert.equal(byClass(input, "perf-caption")[0].textContent, "no input events in the last minute");
  assert.equal(byClass(video, "perf-value")[0].textContent, "–");
  // Earlier activity still shows in the trend, against the limit it had then.
  assert.equal(byClass(input, "perf-trend-bar").length, 4);
  assert.equal(byClass(host, "perf-value")[0].textContent, "0.50 ms");
  assert.equal(byClass(content, "perf-table").length, 1);
  assert.match(content.textContent, /Nothing exceeded its limit in the retained history\./);
  assert.doesNotMatch(content.textContent, /Where the time goes/);
});

test("before the first window the panel says it is collecting", () => {
  const starting = performanceContent(parsePerformanceReply(reply({ schema: 2, collecting: true, recent: null, trend: null, slow: [] })));
  assert.equal(starting.textContent, "Collecting the first ten seconds of measurements.");
  const untraced = performanceContent(parsePerformanceReply(reply({ schema: 2, collecting: false, recent: null, trend: null, slow: [] })));
  assert.match(untraced.textContent, /Nothing has been measured yet/);
});

function mount(post) {
  const target = new Element("div");
  target.setConnected(true);
  const refresh = renderPerformance({ api: { post } }, target);
  return { target, refresh };
}

test("the panel loads its report, refreshes on a timer and stops when the page is left", async () => {
  const calls = [];
  const { target } = mount(async (path) => { calls.push(path); return reply(); });
  await wait(() => byClass(target, "perf-summary").length === 1);
  assert.deepEqual(calls, ["diagnostics/performance"]);
  assert.match(byClass(target, "perf-status")[0].textContent, /^Updated .*Refreshes every 10 seconds\.$/);
  assert.equal(timers.size, 1);
  const [[id, timer]] = [...timers];
  assert.equal(timer.delay, 10_000);

  timers.delete(id);
  timer.callback();
  await wait(() => calls.length === 2);

  // Leaving the page ends the loop: the next timer does nothing.
  target.setConnected(false);
  await wait(() => timers.size === 1);
  [...timers.values()][0].callback();
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(calls.length, 2);
});

test("a failed refresh keeps the figures on screen and says so", async () => {
  let fail = false;
  const { target, refresh } = mount(async () => { if (fail) throw new Error("response too large"); return reply(); });
  await wait(() => byClass(target, "perf-summary").length === 1);
  fail = true;
  await refresh();
  assert.equal(byClass(target, "perf-summary").length, 1);
  assert.equal(byClass(target, "error").length, 0);
  assert.match(byClass(target, "perf-status")[0].textContent, /^Could not refresh at .*: response too large\. Trying again shortly\.$/);
});

test("a first load that fails offers a retry, and an older host is named as such", async () => {
  const failing = mount(async () => { throw new Error("service unavailable"); });
  await wait(() => byClass(failing.target, "error").length === 1);
  assert.match(failing.target.textContent, /service unavailable/);

  const older = mount(async () => ({ ok: true, supported: false, trace: null }));
  await wait(() => /does not measure performance yet/.test(older.target.textContent));
  assert.equal(byClass(older.target, "error").length, 0);
});
