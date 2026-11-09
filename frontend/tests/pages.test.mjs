import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.listeners = new Map();
    this.attributes = new Map();
    this.isConnected = false;
  }

  setConnected(connected) {
    this.isConnected = connected;
    for (const child of this.children) child?.setConnected?.(connected);
  }

  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      this.children.push(child);
      child?.setConnected?.(this.isConnected);
    }
  }

  replaceChildren(...children) {
    for (const child of this.children) child?.setConnected?.(false);
    this.children = [];
    this.append(...children);
  }

  hasChildNodes() {
    return this.children.length > 0;
  }

  addEventListener(event, listener) {
    this.listeners.set(event, listener);
  }

  setAttribute(name, value) { this.attributes.set(name, value); }

  click() {
    this.listeners.get("click")?.();
  }

  get textContent() {
    return this.children
      .map((child) =>
        typeof child === "string" ? child : child?.textContent ?? "",
      )
      .join("");
  }

  set textContent(value) {
    this.replaceChildren(String(value));
  }
}

const pageTestContext = createPageContext();
for (const source of ["src/types.ts", "src/model.ts", "src/ui.ts", "src/pages.ts"]) {
  vm.runInContext(
    ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    pageTestContext,
  );
  if (source === "src/ui.ts") {
    vm.runInContext(
      "Object.assign(globalThis, Amaterasu); globalThis.renderExtensionSlot = () => {}; globalThis.renderHostLogs = () => {}; globalThis.renderPerformance = () => async () => {}; Amaterasu.renderManagedStreamControls = () => {}",
      pageTestContext,
    );
  }
  if (source === "src/session_history.ts" || source === "src/session_timeline.ts") vm.runInContext("Object.assign(globalThis, Amaterasu)", pageTestContext);
}

function createPageContext() {
  const timers = new Map();
  const visibilityListeners = new Set();
  const observers = new Set();
  let nextTimer = 0;
  return vm.createContext({
    document: {
      body: {},
      visibilityState: "visible",
      createElement: (tagName) => new FakeElement(tagName),
      createElementNS: (_namespace, tagName) => new FakeElement(tagName),
      createTextNode: (text) => text,
      addEventListener: (_event, listener) => visibilityListeners.add(listener),
      removeEventListener: (_event, listener) => visibilityListeners.delete(listener),
    },
    MutationObserver: class {
      constructor(callback) { this.callback = callback; }
      observe() { observers.add(this); }
      disconnect() { observers.delete(this); }
    },
    window: {
      setTimeout(callback, delay) {
        const id = ++nextTimer;
        timers.set(id, { callback, delay });
        return id;
      },
      clearTimeout(id) {
        timers.delete(id);
      },
    },
    timers,
    visibilityListeners,
    observers,
    Error,
    TextEncoder,
  });
}

const pages = pageTestContext.Amaterasu;

test("diagnostic page filtering searches messages, targets and fields without changing records", () => {
  const records = [
    { sequence: 1, level: "info", target: "capture", message: "Started", fields: { output: "HDMI-A-1" } },
    { sequence: 2, level: "error", target: "encoder", message: "Failed", fields: { reason: "Timeout" } },
  ];
  assert.deepEqual(Array.from(pages.filterDiagnosticPage(records, " HDMI-a-1 ", "")).map((item) => item.sequence), [1]);
  assert.deepEqual(Array.from(pages.filterDiagnosticPage(records, "TIMEOUT", "error")).map((item) => item.sequence), [2]);
  assert.deepEqual(Array.from(pages.filterDiagnosticPage(records, "timeout", "info")), []);
  assert.equal(records.length, 2);
});

function cleanupOverview(t, content) {
  t.after(() => {
    content.setConnected(false);
    for (const observer of pageTestContext.observers) observer.callback();
  });
}

test("overview reports partial service failures and only offers permitted actions", async (t) => {
  const content = new FakeElement("main");
  content.setConnected(true);
  cleanupOverview(t, content);
  const navigated = [];
  const api = {
    allows: (scope) => scope === "catalog.read",
    post: async (route) => {
      if (route === "catalog/list") return { entries: [], next_cursor: null };
      throw new Error("Not available to this account");
    },
  };
  pages.renderOverview({ content, api, navigate: (page) => navigated.push(page), notify() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(content.textContent, /Some information is unavailable/);
  assert.match(content.textContent, /MANAGEMENT SERVICE ACCESS/);
  assert.doesNotMatch(content.textContent, /Services responding/);
  assert.match(content.textContent, /Not available to this account/);
  const descendants = (node) => [node, ...node.children.flatMap((child) =>
    child instanceof FakeElement ? descendants(child) : [])];
  const managementStatus = descendants(content).find((node) => node.className === "overview-status");
  assert.equal(managementStatus.attributes.get("role"), "status");
  assert.equal(managementStatus.attributes.get("aria-live"), "polite");
  const actions = descendants(content).filter((node) => node.className === "button quick-action");
  assert.equal(actions.length, 1);
  assert.match(actions[0].textContent, /Library/);
  actions[0].click();
  assert.deepEqual(navigated, ["catalog"]);
});

function historyStatus(overrides = {}) {
  return {
    ok: true,
    status: {
      available: true,
      configuration_ready: true,
      applied_revision: 4,
      enabled: true,
      effective_enabled: true,
      degraded: false,
      failed_operations: 0,
      active_streams: 1,
      pending_activities: 0,
      sessions: 4,
      samples: 20,
      events: 3,
      live_bytes: 4096,
      last_prune: {
        sessions_deleted: 2,
        live_bytes: 4096,
        size_limit_bytes: 0,
        size_limit_unmet: false,
      },
      ...overrides,
    },
  };
}

function liveStatus(count = 0) {
  return { ok: true, epoch: "a".repeat(32), sampled_at_ms: "1000", sampled_at_unix_ms: "1700000000000",
    sessions: Array.from({ length: count }, (_, index) => ({ session_id: `stream-${index}`, started_at_unix: 1700000000,
      device_name: "Client", codec: "hevc", width: 1920, height: 1080, target_fps: 60,
      target_bitrate_kbps: 10000, frames_sent: "1", bytes_sent: "2", sequence_gaps: "0" })) };
}

test("overview presents reported stream activity without treating it as client delivery", async (t) => {
  const content = new FakeElement("main");
  content.setConnected(true);
  cleanupOverview(t, content);
  const navigated = [];
  pages.renderOverview({ content, navigate: (page) => navigated.push(page), notify() {}, api: {
    allows: (scope) => scope === "session.read",
    async post(route) {
      if (route === "live-streams/status") return liveStatus(2);
      if (route === "history/history_status") return historyStatus({ active_streams: 0, pending_activities: 1,
        enabled: false, effective_enabled: false, degraded: true });
      throw new Error("Other service unavailable");
    },
  } });
  await waitFor(() => content.textContent.includes("2 active streams reported"), "reported activity");
  assert.match(content.textContent, /1 pending launch/);
  assert.match(content.textContent, /Launch tracking is degraded/);
  assert.match(content.textContent, /Client media delivery is not verified/);
  assert.doesNotMatch(content.textContent, /Host measurements require/);
  const descendants = (node) => [node, ...node.children.flatMap((child) =>
    child instanceof FakeElement ? descendants(child) : [])];
  const activitySummary = descendants(content).find((node) => node.className === "overview-activity-summary")?.children[0];
  assert.equal(activitySummary.attributes.get("role"), "status");
  assert.equal(activitySummary.attributes.get("aria-live"), "polite");
  findButton(content, "View sessions").click();
  assert.deepEqual(navigated, ["sessions"]);
});

test("overview keeps live activity available when history is unavailable", async (t) => {
  const content = new FakeElement("main");
  content.setConnected(true);
  cleanupOverview(t, content);
  pages.renderOverview({ content, navigate() {}, notify() {}, api: {
    allows: (scope) => scope === "session.read",
    async post(route) {
      if (route === "live-streams/status") return liveStatus(1);
      if (route === "history/history_status") return historyStatus({ available: false, configuration_ready: false, active_streams: 0 });
      throw new Error("Other service unavailable");
    },
  } });
  await waitFor(() => content.textContent.includes("Pending launch tracking is unavailable"), "unavailable history");
  assert.match(content.textContent, /1 active stream reported/);
  assert.doesNotMatch(content.textContent, /No active streams reported/);
});

test("overview ignores an older activity response after refresh", async (t) => {
  const content = new FakeElement("main");
  content.setConnected(true);
  cleanupOverview(t, content);
  const pending = [];
  pages.renderOverview({ content, navigate() {}, notify() {}, api: {
    allows: (scope) => scope === "session.read",
    async post(route) {
      if (route === "live-streams/status") return new Promise((resolve) => pending.push(resolve));
      throw new Error("Other service unavailable");
    },
  } });
  findButton(content, "Refresh").click();
  assert.equal(pending.length, 2);
  pending[1](liveStatus(0));
  await waitFor(() => content.textContent.includes("No active streams reported"), "fresh activity");
  pending[0](liveStatus(2));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.doesNotMatch(content.textContent, /2 active streams/);
});

test("overview polls while visible, ignores hidden responses, and cleans up on navigation", async (t) => {
  const content = new FakeElement("main");
  content.setConnected(true);
  cleanupOverview(t, content);
  const pending = [];
  pages.renderOverview({ content, navigate() {}, notify() {}, api: {
    allows: (scope) => scope === "session.read",
    async post(route) {
      if (route === "live-streams/status") return new Promise((resolve, reject) => pending.push({ resolve, reject }));
      if (route === "history/history_status") return historyStatus();
      throw new Error("Other service unavailable");
    },
  } });
  const action = findButton(content, "View sessions");
  pending[0].resolve(liveStatus(1));
  await waitFor(() => pageTestContext.timers.size === 1, "scheduled activity refresh");
  const [timerId, timer] = [...pageTestContext.timers][0];
  assert.equal(timer.delay, 10_000);
  pageTestContext.timers.delete(timerId);
  timer.callback();
  assert.equal(pending.length, 2);
  pageTestContext.document.visibilityState = "hidden";
  for (const listener of pageTestContext.visibilityListeners) listener();
  pending[1].resolve(liveStatus(0));
  await new Promise((resolve) => setImmediate(resolve));
  assert.match(content.textContent, /1 active stream reported/);
  assert.equal(pageTestContext.timers.size, 0);
  pageTestContext.document.visibilityState = "visible";
  for (const listener of pageTestContext.visibilityListeners) listener();
  assert.equal(pending.length, 3);
  pending[2].reject(new Error("offline"));
  await waitFor(() => content.textContent.includes("Live stream status is unavailable"), "unavailable live status");
  assert.doesNotMatch(content.textContent, /No active streams reported/);
  assert.equal(findButton(content, "View sessions"), action);
  content.setConnected(false);
  for (const observer of pageTestContext.observers) observer.callback();
  assert.equal(pageTestContext.timers.size, 0);
  assert.equal(pageTestContext.visibilityListeners.size, 0);
});

function syncStatus(overrides = {}) {
  const base = {
    revision: 3,
    settings_ready: true,
    enabled: true,
    game_count: 0,
    scanning: false,
    complete: true,
    warnings: [],
    warning_count: 0,
    force_sync_pending: false,
    catalog_sync: {
      enabled: false,
      syncing: false,
      force_sync_pending: false,
      last_error: null,
    },
  };
  return {
    ...base,
    ...overrides,
    catalog_sync: { ...base.catalog_sync, ...overrides.catalog_sync },
  };
}

function createHarness({
  allowsRefresh,
  allowsRead = true,
  forceReply,
  statuses,
  listReplies = [],
}) {
  const content = new FakeElement("main");
  content.setConnected(true);
  const calls = [];
  const notifications = [];
  const api = {
    allows: (scope) =>
      (scope === "steam.refresh" && allowsRefresh) ||
      (scope === "steam.read" && allowsRead),
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "steam/list") {
        return listReplies.shift() ?? {
          revision: 3,
          games: [],
          next_after: null,
          scanning: false,
          complete: true,
        };
      }
      if (route === "steam/status") return statuses.shift() ?? syncStatus();
      if (route === "steam/force_sync") return forceReply;
      throw new Error(`Unexpected route ${route}`);
    },
  };
  pages.renderSteam({
    content,
    api,
    notify: (message) => notifications.push(message),
    navigate() {},
  });
  return { content, calls, notifications, timers: pageTestContext.timers };
}

function findAll(node, predicate) {
  return [
    ...(predicate(node) ? [node] : []),
    ...node.children
      .filter((child) => child instanceof FakeElement)
      .flatMap((child) => findAll(child, predicate)),
  ];
}

function findButton(root, label) {
  return findAll(
    root,
    (node) => node.tagName === "button" && node.textContent === label,
  )[0];
}

async function waitFor(predicate, description) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  assert.fail(`Timed out waiting for ${description}`);
}

function pageReady(content) {
  return findAll(
    content,
    (node) => node.tagName === "h3" && node.textContent === "No Steam discoveries",
  ).length > 0;
}

test("Steam page hides force catalog sync without steam.refresh", async () => {
  const harness = createHarness({
    allowsRefresh: false,
    forceReply: null,
    statuses: [syncStatus()],
  });

  await waitFor(
    () => pageReady(harness.content),
    "initial Steam page render",
  );
  assert.equal(findButton(harness.content, "Force catalog sync"), undefined);
});

test("Steam force sync uses its route and stops polling at confirmed terminal state", async () => {
  const pending = syncStatus({
    scanning: true,
    force_sync_pending: true,
    catalog_sync: { syncing: true, force_sync_pending: true },
  });
  const succeeded = syncStatus({ revision: 4 });
  const accepted = {
    ...pending,
    ok: true,
    accepted: true,
    settings_ready: true,
  };
  const harness = createHarness({
    allowsRefresh: true,
    forceReply: accepted,
    statuses: [syncStatus(), syncStatus(), pending, succeeded],
    listReplies: [
      {
        revision: 3,
        games: [],
        next_after: 42,
        scanning: false,
        complete: true,
      },
      {
        revision: 3,
        games: [],
        next_after: null,
        scanning: false,
        complete: true,
      },
      {
        revision: 3,
        games: [],
        next_after: 55,
        scanning: true,
        complete: true,
      },
      {
        revision: 4,
        games: [],
        next_after: null,
        scanning: false,
        complete: true,
      },
    ],
  });

  await waitFor(
    () => pageReady(harness.content),
    "initial Steam page render",
  );
  const forceSync = findButton(harness.content, "Force catalog sync");
  assert.ok(forceSync);
  assert.equal(forceSync.disabled, false);
  findButton(harness.content, "Next →").click();
  await waitFor(
    () =>
      harness.calls.filter((call) => call.route === "steam/list").length >= 2 &&
      harness.content.textContent.includes("Page 2"),
    "the rendered second Steam list page",
  );
  assert.deepEqual(
    {
      ...harness.calls.filter((call) => call.route === "steam/list")[1].payload,
    },
    { revision: 3, after: 42, limit: 16 },
  );

  forceSync.click();

  await waitFor(
    () =>
      harness.timers.size === 1 &&
      harness.calls.filter((call) => call.route === "steam/list").length >= 3,
    "the accepted request to reload and begin polling",
  );
  const listCallsAfterAcceptance = harness.calls.filter(
    (call) => call.route === "steam/list",
  );
  assert.deepEqual({ ...listCallsAfterAcceptance[2].payload }, { limit: 16 });
  const forceCall = harness.calls.find(
    (call) => call.route === "steam/force_sync",
  );
  assert.equal(forceCall.route, "steam/force_sync");
  assert.deepEqual({ ...forceCall.payload }, {});
  const [timerId, timer] = harness.timers.entries().next().value;
  assert.equal(timer.delay, 1000);
  harness.timers.delete(timerId);
  timer.callback();

  await waitFor(
    () =>
      harness.notifications.includes(
        "Steam discovery completed and catalog sync succeeded.",
      ),
    "the confirmed successful terminal status",
  );
  await waitFor(
    () => harness.calls.filter((call) => call.route === "steam/list").length >= 4,
    "the post-sync Steam list refresh",
  );
  const listCallsAfterSync = harness.calls.filter(
    (call) => call.route === "steam/list",
  );
  assert.deepEqual({ ...listCallsAfterSync[3].payload }, { limit: 16 });
  assert.equal(harness.timers.size, 0);
  assert.equal(
    harness.calls.filter((call) => call.route === "steam/status").length,
    4,
  );
  assert.equal(pages.steamForceSyncOutcome(succeeded), "success");
  assert.equal(
    pages.steamForceSyncOutcome(
      syncStatus({
        complete: false,
        catalog_sync: { last_error: "incomplete scan" },
      }),
    ),
    "failure",
  );
  assert.equal(
    pages.steamForceSyncOutcome(
      syncStatus({ catalog_sync: { last_error: "catalog unavailable" } }),
    ),
    "failure",
  );
  assert.equal(
    pages.steamForceSyncOutcome(syncStatus({ enabled: false })),
    "failure",
  );
});

test("Steam force sync does not poll after an immediate unaccepted response", async () => {
  const harness = createHarness({
    allowsRefresh: true,
    forceReply: { ok: false, code: "steam_disabled" },
    statuses: [syncStatus()],
  });

  await waitFor(
    () => pageReady(harness.content),
    "initial Steam page render",
  );
  findButton(harness.content, "Force catalog sync").click();
  await waitFor(
    () => harness.content.textContent.includes("steam_disabled"),
    "the returned immediate force-sync error",
  );
  assert.equal(
    harness.calls.filter((call) => call.route === "steam/status").length,
    1,
  );
  assert.equal(harness.timers.size, 0);
});

test("Steam force sync rejects responses without exact settings readiness", async () => {
  for (const readiness of [false, undefined]) {
    const accepted = {
      ...syncStatus({
        scanning: true,
        force_sync_pending: true,
        catalog_sync: { syncing: true, force_sync_pending: true },
      }),
      ok: true,
      accepted: true,
    };
    if (readiness === false) accepted.settings_ready = false;
    else delete accepted.settings_ready;

    const harness = createHarness({
      allowsRefresh: true,
      forceReply: accepted,
      statuses: [syncStatus()],
    });

    await waitFor(
      () => pageReady(harness.content),
      "initial Steam page render",
    );
    findButton(harness.content, "Force catalog sync").click();
    await waitFor(
      () =>
        harness.content.textContent.includes(
          "Force catalog sync was not accepted.",
        ),
      "the rejected malformed force-sync response",
    );
    assert.equal(
      harness.calls.filter((call) => call.route === "steam/force_sync").length,
      1,
    );
    assert.equal(
      harness.calls.filter((call) => call.route === "steam/status").length,
      1,
    );
    assert.equal(harness.timers.size, 0);
  }
});

test("Steam force sync reports unobservable progress to refresh-only sessions", async () => {
  const pending = syncStatus({
    force_sync_pending: true,
    catalog_sync: { force_sync_pending: true },
  });
  const harness = createHarness({
    allowsRefresh: true,
    allowsRead: false,
    forceReply: {
      ...pending,
      ok: true,
      accepted: true,
      settings_ready: true,
    },
    statuses: [],
  });

  await waitFor(
    () =>
      findAll(
        harness.content,
        (node) => node.tagName === "h3" && node.textContent === "Steam status unavailable",
      ).length > 0,
    "the refresh-only page state",
  );
  const forceSync = findButton(harness.content, "Force catalog sync");
  assert.ok(forceSync);
  forceSync.click();
  await waitFor(
    () => harness.content.textContent.includes("lacks steam.read"),
    "the unobservable-progress notice",
  );
  assert.equal(
    harness.calls.filter((call) => call.route === "steam/force_sync").length,
    1,
  );
  assert.equal(
    harness.calls.filter((call) => call.route === "steam/status").length,
    0,
  );
  assert.equal(harness.timers.size, 0);
});

function historySummary(sessionId) {
  return {
    session_id: sessionId,
    protocol: "rtsp",
    client_name: "Client",
    device_name: "Device",
    app_name: sessionId,
    width: 1920,
    height: 1080,
    target_fps: 60,
    target_bitrate_kbps: 10000,
    codec: "h264",
    started_at_unix: 100,
    ended_at_unix: 120,
    duration_seconds: 20,
  };
}

function historyDetail(sessionId) {
  return {
    ok: true,
    detail: {
      session: historySummary(sessionId),
      sample_count: 1,
      event_count: 1,
      samples_truncated: false,
      events_truncated: false,
      samples: [{ session_id: sessionId, recorded_at_unix: 101, frames_sent: 60,
        bytes_sent: 4096, dropped_frames: 0, actual_fps: 60, actual_bitrate_kbps: 8000 }],
      events: [{ session_id: sessionId, recorded_at_unix: 102, event_type: "stream_started", payload: null }],
    },
  };
}

function diagnosticReply(after, message, overrides = {}) {
  const sequence = after + 1;
  return {
    ok: true,
    records: [{ sequence, level: "info", target: "capture", message, fields: { source: "host" } }],
    next_cursor: sequence,
    has_more: false,
    latest_sequence: sequence,
    oldest_sequence: sequence,
    dropped_total: 0,
    resync_required: false,
    ...overrides,
  };
}

test("diagnostics rejects malformed signed records, cursors and unsafe counters", () => {
  const parse = pages.parseDiagnosticQueryReply;
  assert.equal(parse(diagnosticReply(0, "ready"), 0).records[0].message, "ready");
  for (const reply of [
    diagnosticReply(0, "bad", { next_cursor: 0 }),
    diagnosticReply(0, "bad", { has_more: true }),
    diagnosticReply(0, "bad", { records: [{ ...diagnosticReply(0, "bad").records[0], fields: { source: 7 } }] }),
    diagnosticReply(0, "bad", { records: [{ ...diagnosticReply(0, "bad").records[0], sequence: Number.MAX_SAFE_INTEGER + 1 }] }),
    diagnosticReply(0, "bad", { records: null }),
  ]) assert.throws(() => parse(reply, 0));
  assert.equal(pages.parseDiagnosticCountersReply({ ok: true, counters: { dropped: 2 } }).dropped, 2);
  for (const counters of [
    { ok: true, counters: { dropped: Number.MAX_SAFE_INTEGER + 1 } },
    { ok: true, counters: { dropped: -1 } },
    { ok: true, counters: [] },
  ]) assert.throws(() => pages.parseDiagnosticCountersReply(counters));
});

function diagnosticSession(id, overrides = {}) {
  return {
    session_id: id,
    protocol: "moonlight",
    client_name: `private-client-${id}`,
    device_name: `private-device-${id}`,
    app_name: `private-app-${id}`,
    width: 1920,
    height: 1080,
    target_fps: 60,
    target_bitrate_kbps: 12000,
    codec: "h264",
    started_at_unix: 10,
    ended_at_unix: 20,
    duration_seconds: 10,
    ...overrides,
  };
}

function diagnosticHistoryDetail(session, events = []) {
  return { ok: true, detail: {
    session,
    sample_count: 0,
    event_count: events.length,
    samples_truncated: false,
    events_truncated: false,
    samples: [],
    events: events.map((event, index) => ({
      session_id: session.session_id,
      recorded_at_unix: 15 + index,
      payload: null,
      ...event,
    })),
  } };
}

test("diagnostics shows bounded redacted session outcomes without implying record correlation", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  const sessions = [diagnosticSession("session-private-1"), diagnosticSession("session-private-2")];
  const calls = [];
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "session.read" || scope === "diagnostics.read",
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "diagnostics/query") return diagnosticReply(payload.after, "ordinary caller record");
      if (route === "diagnostics/counters") return { ok: true, counters: {} };
      if (route === "history/list_sessions") return { ok: true, sessions, next_offset: null };
      if (route === "history/session_detail" && payload.session_id === sessions[0].session_id)
        return diagnosticHistoryDetail(sessions[0], [{ event_type: "stream_failed", payload: { state: "failed", reason: "capture_read_failed" } }]);
      if (route === "history/session_detail")
        return diagnosticHistoryDetail(sessions[1], [{ event_type: "stream_failed", payload: { reason: "future_private_reason" } }]);
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => content.textContent.includes("Failure · Capture") && content.textContent.includes("stage not identified"), "session outcome summaries");
  assert.match(content.textContent, /caller-scoped diagnostics/);
  assert.match(content.textContent, /no record-to-session correlation is implied/);
  assert.match(content.textContent, /Client and device identity are not displayed/);
  assert.doesNotMatch(content.textContent, /private-client|private-device|private-app|session-private|future_private_reason/);
  assert.equal(calls.filter((call) => call.route === "history/list_sessions")[0].payload.limit, 4);
  assert.equal(calls.filter((call) => call.route === "history/session_detail").length, 2);
});

test("diagnostics session outcome panel reports missing history and enforces session.read", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  const calls = [];
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "diagnostics.read",
    async post(route) {
      calls.push(route);
      if (route === "diagnostics/query") return diagnosticReply(0, "ready");
      if (route === "diagnostics/counters") return { ok: true, counters: {} };
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => content.textContent.includes("needs session.read"), "session permission state");
  assert.equal(calls.some((route) => route.startsWith("history/")), false);

  const missing = diagnosticHistoryDetail(diagnosticSession("missing-event"), []);
  assert.equal(pages.diagnosticSessionOutcome(missing.detail), "Ended · no termination event recorded");
  missing.detail.events_truncated = true;
  assert.match(pages.diagnosticSessionOutcome(missing.detail), /history is truncated/);
  assert.equal(pages.diagnosticSessionOutcome(null), "Outcome unavailable");
});

test("diagnostics ignores stale recent-session replies after refresh", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  const pendingLists = [];
  const fresh = diagnosticSession("fresh-session");
  const stale = diagnosticSession("stale-session");
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: () => true,
    async post(route, payload = {}) {
      if (route === "diagnostics/query") return diagnosticReply(payload.after, "diagnostic ready");
      if (route === "diagnostics/counters") return { ok: true, counters: {} };
      if (route === "history/list_sessions") return new Promise((resolve) => pendingLists.push(resolve));
      if (route === "history/session_detail") {
        const session = payload.session_id === fresh.session_id ? fresh : stale;
        return diagnosticHistoryDetail(session, [{ event_type: session === fresh ? "stream_stopped" : "stream_cancelled" }]);
      }
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => pendingLists.length === 1, "initial history load");
  findButton(content, "Refresh").click();
  await waitFor(() => pendingLists.length === 2, "refreshed history load");
  pendingLists[1]({ ok: true, sessions: [fresh], next_offset: null });
  await waitFor(() => content.textContent.includes("Stopped"), "fresh redacted history");
  pendingLists[0]({ ok: true, sessions: [stale], next_offset: null });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.doesNotMatch(content.textContent, /stale-session|private-client-stale/);
  assert.match(content.textContent, /Stopped/);
  assert.doesNotMatch(content.textContent, /Cancelled/);
});

function journalPage(after, sequences, latest, overrides = {}) {
  return {
    ok: true,
    records: sequences.map((sequence) => ({ sequence, level: "info", target: "capture", message: `record ${sequence}`, fields: {} })),
    next_cursor: sequences.at(-1) ?? after,
    has_more: (sequences.at(-1) ?? after) < latest,
    latest_sequence: latest,
    oldest_sequence: latest ? 1 : null,
    dropped_total: 0,
    resync_required: false,
    ...overrides,
  };
}

test("diagnostic snapshot searches every retained page and stops at the original endpoint", async () => {
  const calls = [];
  const api = { async post(route, { after, limit }) {
    assert.equal(route, "diagnostics/query");
    assert.equal(limit, 4);
    calls.push(after);
    return after === 0 ? journalPage(0, [1, 2, 3, 4], 6) : journalPage(4, [5, 6, 7, 8], 8);
  } };
  const snapshot = await pages.readDiagnosticSnapshot(api, () => true);
  assert.deepEqual(calls, [0, 4]);
  assert.deepEqual(Array.from(snapshot.records, (record) => record.sequence), [1, 2, 3, 4, 5, 6]);
  assert.equal(snapshot.latestSequence, 6);
  assert.equal(snapshot.incomplete, false);
  assert.equal(pages.filterDiagnosticPage(snapshot.records, "record 6", "").length, 1);
  assert.match(pages.formatDiagnosticRecords(snapshot.records), /#6 \[info\] capture: record 6/);
});

test("diagnostic snapshots report retention and concurrent eviction accurately", async () => {
  const replies = [
    journalPage(0, [3, 4, 5, 6], 8, { oldest_sequence: 3, dropped_total: 2, resync_required: true }),
    journalPage(6, [8, 9], 9, { oldest_sequence: 8, dropped_total: 7, resync_required: true }),
  ];
  const snapshot = await pages.readDiagnosticSnapshot({ async post() { return replies.shift(); } }, () => true);
  assert.equal(snapshot.droppedTotal, 2);
  assert.equal(snapshot.incomplete, true);
  assert.deepEqual(Array.from(snapshot.records, (record) => record.sequence), [3, 4, 5, 6, 8]);
  const retained = await pages.readDiagnosticSnapshot({ async post() {
    return journalPage(0, [3, 4], 4, { oldest_sequence: 3, dropped_total: 2, resync_required: true });
  } }, () => true);
  assert.equal(retained.incomplete, false);
  assert.equal(retained.droppedTotal, 2);
});

test("diagnostic snapshots stop at journal capacity and reject stalled cursors", async () => {
  let calls = 0;
  const full = await pages.readDiagnosticSnapshot({ async post(_route, { after }) {
    calls++;
    return journalPage(after, Array.from({ length: 4 }, (_, i) => after + i + 1), 128);
  } }, () => true);
  assert.equal(calls, 32);
  assert.equal(full.records.length, 128);
  assert.equal(full.incomplete, false);
  await assert.rejects(pages.readDiagnosticSnapshot({ async post() {
    return journalPage(0, [], 1);
  } }, () => true), /did not advance/);
  const cancelled = await pages.readDiagnosticSnapshot({ async post() { return journalPage(0, [1], 1); } }, () => false);
  assert.equal(cancelled, null);
});

test("diagnostics ignores older snapshot replies and errors after refresh", async () => {
  const pending = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "diagnostics.read",
    async post(route, payload) {
      if (route === "diagnostics/counters") return { ok: true, counters: { reads: 1 } };
      if (route === "diagnostics/query")
        return new Promise((resolve, reject) => pending.push({ after: payload.after, resolve, reject }));
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  pending[0].resolve(diagnosticReply(0, "first", { has_more: true, latest_sequence: 2 }));
  await waitFor(() => pending.length === 2, "pending second snapshot page");
  findButton(content, "Refresh").click();
  await waitFor(() => pending.length === 3, "pending refresh");
  pending[2].resolve(diagnosticReply(0, "fresh"));
  await waitFor(() => content.textContent.includes("fresh"), "refreshed diagnostics");
  pending[1].resolve(diagnosticReply(1, "stale"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(content.textContent, /fresh/);
  assert.doesNotMatch(content.textContent, /stale/);
  assert.match(content.textContent, /1 of 1 retained records/);
  findButton(content, "Refresh").click();
  findButton(content, "Refresh").click();
  await waitFor(() => pending.length === 5, "overlapping refreshes");
  pending[4].resolve(diagnosticReply(0, "latest"));
  await waitFor(() => content.textContent.includes("latest"), "latest diagnostics");
  pending[3].reject(new Error("old request failed"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(content.textContent, /latest/);
  assert.doesNotMatch(content.textContent, /old request failed/);
});

test("diagnostics navigates filtered records and resets selection when filters change", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "diagnostics.read",
    async post(route, payload = {}) {
      if (route === "diagnostics/counters") return { ok: true, counters: {} };
      if (route === "diagnostics/query") return journalPage(payload.after, [1, 2, 3], 3);
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => content.textContent.includes("record 3"), "diagnostics records");
  const descendants = (node) => [node, ...node.children.flatMap((child) =>
    child instanceof FakeElement ? descendants(child) : [])];
  const filter = descendants(content).find((node) => node.name === "diagnostics_filter");
  const navigation = descendants(content).find((node) => node.className === "diagnostics-match-navigation");
  assert.equal(navigation.hidden, true);
  filter.value = "record";
  filter.listeners.get("input")();
  assert.equal(navigation.hidden, false);
  assert.match(navigation.textContent, /Match 1 of 3/);
  findButton(content, "Next match").click();
  assert.match(navigation.textContent, /Match 2 of 3/);
  let prevented = false;
  filter.listeners.get("keydown")({ key: "Enter", shiftKey: true, isComposing: false, preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  assert.match(navigation.textContent, /Match 1 of 3/);
  findButton(content, "Previous match").click();
  assert.match(navigation.textContent, /Match 3 of 3/);
  filter.value = "no match";
  filter.listeners.get("input")();
  assert.match(navigation.textContent, /No matches/);
  assert.equal(findButton(content, "Next match").disabled, true);
  filter.value = "";
  filter.listeners.get("input")();
  assert.equal(navigation.hidden, true);
});

test("diagnostics retains records when counters are unavailable", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "diagnostics.read",
    async post(route) {
      if (route === "diagnostics/counters") throw new Error("Counters unavailable");
      return diagnosticReply(0, "record available");
    },
  } });
  await waitFor(() => content.textContent.includes("record available"), "records without counters");
  assert.match(content.textContent, /Counters unavailable/);
});

test("operator can stop managed launches but cannot terminate raw processes", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  const api = {
    allows(scope) { return scope === "application.read"; },
    allowsLaunchAction(action) { return action === "stop"; },
    async post(route) {
      if (route === "launch/list") return { launches: [{ launch_id: 1, app_id: "app-0000000000000001", state: "active" }] };
      if (route === "application/list") return { processes: [{ process_id: 2, operation_id: "sample", state: "running" }], next_offset: null };
      throw new Error(`Unexpected route ${route}`);
    },
  };
  pages.renderApplications({ content, api, notify() {}, navigate() {} });
  await waitFor(() => findButton(content, "Stop") && findButton(content, "Terminate"), "application controls");
  assert.equal(findButton(content, "Stop").disabled, false);
  assert.equal(findButton(content, "Terminate").disabled, true);
});
