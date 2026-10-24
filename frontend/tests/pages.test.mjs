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

  addEventListener(event, listener) {
    this.listeners.set(event, listener);
  }

  setAttribute() {}

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
    vm.runInContext("Object.assign(globalThis, Amaterasu)", pageTestContext);
  }
}

function createPageContext() {
  const timers = new Map();
  let nextTimer = 0;
  return vm.createContext({
    document: {
      createElement: (tagName) => new FakeElement(tagName),
      createTextNode: (text) => text,
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
    Error,
    TextEncoder,
  });
}

const pages = pageTestContext.Amaterasu;

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

test("history runtime status reports readiness, deferred settings, failures and retention", () => {
  const parse = pages.parseHistoryRuntimeStatus;
  const describe = (reply) => pages.historyRuntimeDescription(parse(reply)).join(" ");
  assert.match(describe(historyStatus()), /Recording is enabled for new streams/);
  assert.match(describe(historyStatus()), /4 stored sessions/);
  assert.match(describe(historyStatus()), /deleted 2 sessions/);
  assert.match(describe(historyStatus({ enabled: false, effective_enabled: true })), /disable is deferred until active streams end; new streams still use the enabled policy/);
  assert.match(describe(historyStatus({ enabled: true, effective_enabled: false })), /enable is deferred until active streams end; new streams still use the disabled policy/);
  const waitingEnable = describe(historyStatus({ enabled: true, effective_enabled: false,
    active_streams: 0, pending_activities: 1 }));
  assert.match(waitingEnable, /enable is deferred until 1 pending launch resolves; the effective policy remains disabled/);
  assert.match(waitingEnable, /0 active streams · 1 pending launch/);
  assert.doesNotMatch(waitingEnable, /new streams/);
  const waitingDisable = describe(historyStatus({ enabled: false, effective_enabled: true,
    active_streams: 1, pending_activities: 2 }));
  assert.match(waitingDisable, /disable is deferred until 1 active stream ends and 2 pending launches resolve/);
  assert.match(waitingDisable, /1 active stream · 2 pending launches/);
  assert.doesNotMatch(waitingDisable, /new streams/);
  assert.match(describe(historyStatus({ enabled: true, effective_enabled: true,
    active_streams: 0, pending_activities: 0 })), /Recording is enabled for new streams.*0 pending launches/);
  assert.match(describe(historyStatus({ enabled: false, effective_enabled: false })), /Automatic retention and deletion are paused/);
  assert.doesNotMatch(describe(historyStatus()), /retention and deletion are paused/);
  assert.match(describe(historyStatus({ configuration_ready: false, degraded: true })), /configuration is not ready/);
  assert.match(describe(historyStatus({ available: false, configuration_ready: false })), /storage is unavailable/);
  assert.doesNotMatch(describe(historyStatus({ available: false, configuration_ready: false,
    pending_activities: 0 })), /pending launches|active streams/);
  assert.match(describe(historyStatus({ degraded: true, failed_operations: 3 })), /3 history storage operations have failed/);
  assert.doesNotMatch(describe(historyStatus({ degraded: true, failed_operations: 3 })), /successfully recorded|writes are healthy/);
  assert.match(describe(historyStatus({ degraded: true, last_prune: {
    ...historyStatus().status.last_prune, size_limit_bytes: 1024, size_limit_unmet: true,
  } })), /size limit remains unmet/);
});

test("history runtime status rejects malformed signed replies and counters", () => {
  const parse = pages.parseHistoryRuntimeStatus;
  const legacy = historyStatus();
  delete legacy.status.pending_activities;
  assert.equal(parse(legacy).pending_activities, 0);
  for (const reply of [null, {}, { ...historyStatus(), ok: false },
    historyStatus({ available: "true" }), historyStatus({ failed_operations: -1 }),
    historyStatus({ pending_activities: -1 }), historyStatus({ pending_activities: 1.5 }),
    historyStatus({ pending_activities: "1" }),
    historyStatus({ pending_activities: Number.MAX_SAFE_INTEGER + 1 }),
    historyStatus({ applied_revision: undefined }), historyStatus({ applied_revision: -1 }),
    historyStatus({ applied_revision: "4" }), historyStatus({ applied_revision: 1.5 }),
    historyStatus({ sessions: 1.5 }), historyStatus({ live_bytes: Number.MAX_SAFE_INTEGER + 1 }),
    historyStatus({ last_prune: null }),
    historyStatus({ last_prune: { ...historyStatus().status.last_prune, size_limit_unmet: "false" } }),
  ]) assert.throws(() => parse(reply));
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

test("recorded history follows returned offsets across short pages", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  const offsets = [];
  const replies = [
    { sessions: [historySummary("first")], next_offset: 1 },
    { sessions: [historySummary("second"), historySummary("third")], next_offset: 3 },
    { sessions: [historySummary("fourth")], next_offset: null },
    { sessions: [historySummary("second"), historySummary("third")], next_offset: 3 },
  ];
  pages.renderSessions({
    content,
    notify() {},
    navigate() {},
    api: {
      allows: () => false,
      async post(route, payload) {
        if (route === "session/list") return { sessions: [], next_offset: null };
        if (route === "history/history_status") throw new Error("status unavailable");
        if (route === "history/list_sessions") {
          offsets.push(payload.offset);
          return { ok: true, ...replies.shift() };
        }
        throw new Error(`Unexpected route ${route}`);
      },
    },
  });
  const savedPagerButton = (label) => findAll(
    content,
    (node) => node.tagName === "button" && node.textContent === label,
  )[1];
  await waitFor(() => offsets.length === 1 && savedPagerButton("Next →"), "first history page");
  assert.equal(savedPagerButton("← Previous").disabled, true);
  savedPagerButton("Next →").click();
  await waitFor(() => offsets.length === 2 && content.textContent.includes("second") && savedPagerButton("Next →"), "second history page");
  assert.equal(offsets[1], 1);
  savedPagerButton("Next →").click();
  await waitFor(() => offsets.length === 3 && content.textContent.includes("fourth") && savedPagerButton("Next →"), "final history page");
  assert.equal(offsets[2], 3);
  assert.equal(savedPagerButton("Next →").disabled, true);
  savedPagerButton("← Previous").click();
  await waitFor(() => offsets.length === 4 && content.textContent.includes("second") && savedPagerButton("Next →"), "previous history page");
  assert.equal(offsets[3], 1);
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

test("recorded session pages reject malformed signed rows before exposing delete", () => {
  const valid = { ok: true, sessions: [historySummary("stored")], next_offset: null };
  assert.equal(pages.parseHistoryListReply(valid, 0).sessions[0].session_id, "stored");
  for (const reply of [
    { ...valid, sessions: [{ ...historySummary("stored"), session_id: "" }] },
    { ...valid, sessions: [{ ...historySummary("stored"), session_id: "bad\nkey" }] },
    { ...valid, sessions: [{ ...historySummary("stored"), ended_at_unix: "yesterday" }] },
    { ...valid, sessions: [{ ...historySummary("stored"), app_name: null }] },
    { ...valid, next_offset: 0 },
  ]) assert.throws(() => pages.parseHistoryListReply(reply, 0));
});

test("recorded delete requires an explicit successful signed reply", () => {
  assert.equal(pages.parseHistoryDeleteReply({ ok: true }), undefined);
  for (const reply of [null, {}, { ok: false, code: "active_session" }, { ok: "true" }])
    assert.throws(() => pages.parseHistoryDeleteReply(reply));
});

test("recorded deletion readback requires the signed not-found result", async () => {
  const absent = { post: async () => { throw { code: "not_found" }; } };
  await pages.confirmHistoryDeletion(absent, "stored");
  for (const result of [
    { post: async () => historyDetail("stored") },
    { post: async () => { throw { code: "storage_error" }; } },
  ]) await assert.rejects(() => pages.confirmHistoryDeletion(result, "stored"));
});

test("malformed successful delete reply preserves the selected history and reports failure", async () => {
  const calls = [];
  const notices = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderSessions({ content, notify: (message) => notices.push(message), navigate() {}, api: {
    allows: (scope) => scope === "session.write",
    async post(route) {
      calls.push(route);
      if (route === "session/list") return { sessions: [], next_offset: null };
      if (route === "history/history_status") return historyStatus();
      if (route === "history/list_sessions")
        return { ok: true, sessions: [historySummary("stored")], next_offset: null };
      if (route === "history/session_detail") return historyDetail("stored");
      if (route === "history/delete_session") return {};
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => findButton(content, "Delete")?.disabled === false, "enabled delete");
  findButton(content, "Details").click();
  await waitFor(() => content.textContent.includes("History for stored"), "selected detail");
  const previousConfirm = pageTestContext.confirmAction;
  pageTestContext.confirmAction = async () => true;
  try {
    findButton(content, "Delete").click();
    await waitFor(() => calls.includes("history/delete_session") &&
      content.textContent.includes("History deletion was not confirmed"), "unconfirmed deletion");
  } finally {
    pageTestContext.confirmAction = previousConfirm;
  }
  assert.match(content.textContent, /History for stored/);
  assert.deepEqual(notices, []);
});

test("confirmed recorded delete waits for signed absence before reporting success", async () => {
  let deleted = false;
  let finishReadback;
  const notices = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderSessions({ content, notify: (message) => notices.push(message), navigate() {}, api: {
    allows: (scope) => scope === "session.write",
    async post(route) {
      if (route === "session/list") return { sessions: [], next_offset: null };
      if (route === "history/history_status") return historyStatus();
      if (route === "history/list_sessions")
        return { ok: true, sessions: deleted ? [] : [historySummary("stored")], next_offset: null };
      if (route === "history/delete_session") { deleted = true; return { ok: true }; }
      if (route === "history/session_detail")
        return new Promise((resolve, reject) => { finishReadback = () => reject({ code: "not_found" }); });
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => findButton(content, "Delete")?.disabled === false, "enabled delete");
  const previousConfirm = pageTestContext.confirmAction;
  pageTestContext.confirmAction = async () => true;
  try {
    findButton(content, "Delete").click();
    await waitFor(() => deleted && finishReadback, "pending signed readback");
    assert.deepEqual(notices, []);
    finishReadback();
    await waitFor(() => notices.length === 1 && content.textContent.includes("No recorded streams"), "confirmed delete");
  } finally {
    pageTestContext.confirmAction = previousConfirm;
  }
  assert.deepEqual(notices, ["Recorded session deleted."]);
});

test("recorded deletion follows signed runtime status while list and detail stay readable", async () => {
  let runtime = historyStatus({ enabled: false, effective_enabled: false });
  const calls = [];
  const api = {
    allows: (scope) => scope === "session.write",
    async post(route) {
      calls.push(route);
      if (route === "session/list") return { sessions: [], next_offset: null };
      if (route === "history/history_status") return runtime;
      if (route === "history/list_sessions")
        return { ok: true, sessions: [historySummary("stored")], next_offset: null };
      if (route === "history/session_detail") return historyDetail("stored");
      throw new Error(`Unexpected route ${route}`);
    },
  };
  const mount = () => {
    const content = new FakeElement("main");
    content.setConnected(true);
    pages.renderSessions({ content, api, notify() {}, navigate() {} });
    return content;
  };
  let content = mount();
  await waitFor(() => findButton(content, "Delete")?.disabled &&
    content.textContent.includes("Deletion is unavailable while history recording is disabled"), "disabled history readback");
  findButton(content, "Details").click();
  await waitFor(() => content.textContent.includes("History for stored"), "stored detail while disabled");
  assert.equal(calls.filter((route) => route === "history/delete_session").length, 0);

  runtime = historyStatus({ enabled: false, effective_enabled: true, active_streams: 1 });
  findButton(content, "Refresh history status").click();
  await waitFor(() => findButton(content, "Delete")?.disabled === false, "pending disable effective writer");

  runtime = historyStatus({ enabled: false, effective_enabled: false, active_streams: 0 });
  content.setConnected(false);
  content = mount();
  await waitFor(() => findButton(content, "Delete")?.disabled &&
    content.textContent.includes("Deletion is unavailable while history recording is disabled"), "disabled restart readback");
  runtime = historyStatus();
  findButton(content, "Refresh history status").click();
  await waitFor(() => findButton(content, "Delete")?.disabled === false, "re-enabled writer");
  runtime = historyStatus({ enabled: false, effective_enabled: false });
  const previousConfirm = pageTestContext.confirmAction;
  let confirmations = 0;
  pageTestContext.confirmAction = async () => { confirmations += 1; return true; };
  try {
    findButton(content, "Delete").click();
    await waitFor(() => findButton(content, "Delete")?.disabled &&
      content.textContent.includes("Deletion is unavailable while history recording is disabled"), "delete preflight after writer shutdown");
  } finally {
    pageTestContext.confirmAction = previousConfirm;
  }
  assert.equal(confirmations, 0);
  assert.equal(calls.filter((route) => route === "history/delete_session").length, 0);
  findButton(content, "Details").click();
  await waitFor(() => content.textContent.includes("History for stored"), "detail after rejected delete");
});

test("recorded deletion rechecks signed status after a long-open confirmation", async () => {
  let runtime = historyStatus();
  const calls = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderSessions({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "session.write",
    async post(route) {
      calls.push(route);
      if (route === "session/list") return { sessions: [], next_offset: null };
      if (route === "history/history_status") return runtime;
      if (route === "history/list_sessions")
        return { ok: true, sessions: [historySummary("stored")], next_offset: null };
      if (route === "history/session_detail") return historyDetail("stored");
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await waitFor(() => findButton(content, "Delete")?.disabled === false, "enabled history writer");
  const previousConfirm = pageTestContext.confirmAction;
  let finishConfirmation;
  let confirmations = 0;
  pageTestContext.confirmAction = () => {
    confirmations += 1;
    return new Promise((resolve) => { finishConfirmation = resolve; });
  };
  try {
    findButton(content, "Delete").click();
    await waitFor(() => confirmations === 1, "delete confirmation");
    runtime = historyStatus({ enabled: false, effective_enabled: false });
    finishConfirmation(true);
    await waitFor(() => findButton(content, "Delete")?.disabled &&
      content.textContent.includes("Deletion is unavailable while history recording is disabled"), "disabled writer after confirmation");
  } finally {
    pageTestContext.confirmAction = previousConfirm;
  }
  assert.equal(calls.filter((route) => route === "history/history_status").length, 3);
  assert.equal(calls.filter((route) => route === "history/delete_session").length, 0);
  findButton(content, "Details").click();
  await waitFor(() => content.textContent.includes("History for stored"), "stored detail after confirmation");
});

test("history status failure and stale status or list replies cannot re-enable deletion", async () => {
  const pendingStatuses = [];
  const pendingLists = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderSessions({ content, notify() {}, navigate() {}, api: {
    allows: (scope) => scope === "session.write",
    async post(route) {
      if (route === "session/list") return { sessions: [], next_offset: null };
      if (route === "history/history_status")
        return new Promise((resolve, reject) => pendingStatuses.push({ resolve, reject }));
      if (route === "history/list_sessions")
        return new Promise((resolve) => pendingLists.push(resolve));
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  pendingLists[0]({ ok: true, sessions: [historySummary("stored")], next_offset: null });
  pendingStatuses[0].resolve(historyStatus());
  await waitFor(() => findButton(content, "Delete")?.disabled === false, "initial enabled readback");

  findButton(content, "Refresh history status").click();
  assert.equal(findButton(content, "Delete").disabled, true);
  findButton(content, "Refresh").click();
  await waitFor(() => pendingStatuses.length === 3 && pendingLists.length === 2, "overlapping refreshes");
  pendingStatuses[2].resolve(historyStatus({ enabled: false, effective_enabled: false }));
  pendingLists[1]({ ok: true, sessions: [historySummary("current")], next_offset: null });
  await waitFor(() => content.textContent.includes("current") &&
    findButton(content, "Delete")?.disabled, "newer disabled readback");
  pendingStatuses[1].resolve(historyStatus());
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(findButton(content, "Delete").disabled, true);
  assert.doesNotMatch(content.textContent, /Deletion is unavailable until history runtime status can be confirmed/);

  findButton(content, "Refresh history status").click();
  pendingStatuses[3].reject(new Error("status unavailable"));
  await waitFor(() => content.textContent.includes("Deletion is unavailable until history runtime status can be confirmed"), "unavailable status");
  assert.equal(findButton(content, "Delete").disabled, true);
  assert.ok(findButton(content, "Details"));

  findButton(content, "Refresh").click();
  findButton(content, "Refresh").click();
  await waitFor(() => pendingStatuses.length === 6 && pendingLists.length === 4, "overlapping list refreshes");
  pendingStatuses[5].resolve(historyStatus({ enabled: false, effective_enabled: false }));
  pendingLists[3]({ ok: true, sessions: [historySummary("latest")], next_offset: null });
  await waitFor(() => content.textContent.includes("latest"), "latest history page");
  pendingStatuses[4].resolve(historyStatus());
  pendingLists[2]({ ok: true, sessions: [historySummary("stale")], next_offset: null });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(content.textContent, /latest/);
  assert.doesNotMatch(content.textContent, /stale/);
  assert.equal(findButton(content, "Delete").disabled, true);
});

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

test("recorded detail readback rejects mismatched and malformed signed replies", () => {
  assert.equal(pages.parseHistoryDetailReply(historyDetail("first"), "first").detail.sample_count, 1);
  for (const reply of [
    { ...historyDetail("first"), ok: false },
    historyDetail("other"),
    { ...historyDetail("first"), detail: { ...historyDetail("first").detail, sample_count: -1 } },
    { ...historyDetail("first"), detail: { ...historyDetail("first").detail,
      samples: [{ ...historyDetail("first").detail.samples[0], actual_fps: Number.NaN }] } },
    { ...historyDetail("first"), detail: { ...historyDetail("first").detail,
      events: [{ ...historyDetail("first").detail.events[0], session_id: "other" }] } },
  ]) assert.throws(() => pages.parseHistoryDetailReply(reply, "first"));
});

test("recorded detail keeps the latest selection after older success or failure and reads again after remount", async () => {
  const pending = [];
  const api = {
    allows: () => false,
    async post(route) {
      if (route === "session/list") return { sessions: [], next_offset: null };
      if (route === "history/history_status") return historyStatus();
      if (route === "history/list_sessions")
        return { ok: true, sessions: [historySummary("first"), historySummary("second")], next_offset: null };
      if (route === "history/session_detail")
        return new Promise((resolve, reject) => pending.push({ resolve, reject }));
      throw new Error(`Unexpected route ${route}`);
    },
  };
  const mount = () => {
    const content = new FakeElement("main");
    content.setConnected(true);
    pages.renderSessions({ content, api, notify() {}, navigate() {} });
    return content;
  };
  const content = mount();
  await waitFor(() => findAll(content, (node) => node.tagName === "button" && node.textContent === "Details").length === 2, "history list");
  let details = findAll(content, (node) => node.tagName === "button" && node.textContent === "Details");
  details[0].click();
  details[1].click();
  pending[1].resolve(historyDetail("second"));
  await waitFor(() => content.textContent.includes("History for second"), "newer detail");
  pending[0].resolve(historyDetail("first"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(content.textContent, /History for second/);
  assert.doesNotMatch(content.textContent, /History for first/);

  details[0].click();
  details[1].click();
  pending[3].resolve(historyDetail("second"));
  await waitFor(() => content.textContent.includes("History for second"), "newer detail again");
  pending[2].reject(new Error("old storage failure"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.doesNotMatch(content.textContent, /old storage failure/);

  details[0].click();
  pending[4].reject(new Error("current storage failure"));
  await waitFor(() => content.textContent.includes("current storage failure"), "current detail failure");
  findButton(content, "Try again").click();
  pending[5].resolve(historyDetail("first"));
  await waitFor(() => content.textContent.includes("History for first"), "retried detail readback");

  content.setConnected(false);
  const reopened = mount();
  await waitFor(() => findAll(reopened, (node) => node.tagName === "button" && node.textContent === "Details").length === 2, "reopened history list");
  details = findAll(reopened, (node) => node.tagName === "button" && node.textContent === "Details");
  details[0].click();
  pending[6].resolve(historyDetail("first"));
  await waitFor(() => reopened.textContent.includes("History for first"), "reopened detail");
  assert.match(reopened.textContent, /1 samples.*1 events/);
  assert.match(reopened.textContent, /stream_started/);
});

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

test("diagnostics ignores older page replies and errors after refresh", async () => {
  const pending = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: () => true,
    async post(route, payload) {
      if (route === "diagnostics/counters") return { ok: true, counters: { reads: 1 } };
      if (route === "diagnostics/query")
        return new Promise((resolve, reject) => pending.push({ after: payload.after, resolve, reject }));
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  pending[0].resolve(diagnosticReply(0, "first", { has_more: true, latest_sequence: 2 }));
  await waitFor(() => findButton(content, "Next →"), "next diagnostics page");
  findButton(content, "Next →").click();
  await waitFor(() => pending.length === 2, "pending second page");
  findButton(content, "Refresh").click();
  await waitFor(() => pending.length === 3, "pending refresh");
  pending[2].resolve(diagnosticReply(0, "fresh"));
  await waitFor(() => content.textContent.includes("fresh"), "refreshed diagnostics");
  pending[1].resolve(diagnosticReply(1, "stale"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(content.textContent, /fresh/);
  assert.doesNotMatch(content.textContent, /stale/);
  assert.match(content.textContent, /Page 1/);
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

test("diagnostics resync clears back navigation into evicted records", async () => {
  const pending = [];
  const content = new FakeElement("main");
  content.setConnected(true);
  pages.renderDiagnostics({ content, notify() {}, navigate() {}, api: {
    allows: () => true,
    async post(route, payload) {
      if (route === "diagnostics/counters") return { ok: true, counters: {} };
      if (route === "diagnostics/query")
        return new Promise((resolve) => pending.push({ after: payload.after, resolve }));
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  pending[0].resolve(diagnosticReply(0, "old", { has_more: true, latest_sequence: 5 }));
  await waitFor(() => findButton(content, "Next →"), "next diagnostics page");
  findButton(content, "Next →").click();
  await waitFor(() => pending.length === 2, "next query");
  pending[1].resolve(diagnosticReply(1, "retained", {
    records: [{ sequence: 5, level: "info", target: "capture", message: "retained", fields: {} }],
    next_cursor: 5,
    latest_sequence: 5,
    oldest_sequence: 5,
    dropped_total: 4,
    resync_required: true,
  }));
  await waitFor(() => content.textContent.includes("retained"), "resynced records");
  assert.match(content.textContent, /Page 1/);
  assert.match(content.textContent, /Older records were removed/);
  assert.equal(findButton(content, "← Previous").disabled, true);
});
