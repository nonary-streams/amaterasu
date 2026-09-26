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
    document: { createElement: (tagName) => new FakeElement(tagName) },
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
  });
}

const pages = pageTestContext.Amaterasu;

function historyStatus(overrides = {}) {
  return {
    ok: true,
    status: {
      available: true,
      configuration_ready: true,
      enabled: true,
      effective_enabled: true,
      degraded: false,
      failed_operations: 0,
      active_streams: 1,
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
  assert.match(describe(historyStatus()), /recording is active/);
  assert.match(describe(historyStatus()), /4 stored sessions/);
  assert.match(describe(historyStatus()), /deleted 2 sessions/);
  assert.match(describe(historyStatus({ enabled: false, effective_enabled: true })), /disable change takes effect after active streams end/);
  assert.match(describe(historyStatus({ enabled: true, effective_enabled: false })), /enable change takes effect after active streams end/);
  assert.match(describe(historyStatus({ configuration_ready: false, degraded: true })), /configuration is not ready/);
  assert.match(describe(historyStatus({ available: false, configuration_ready: false })), /storage is unavailable/);
  assert.match(describe(historyStatus({ degraded: true, failed_operations: 3 })), /3 history storage operations have failed/);
  assert.match(describe(historyStatus({ degraded: true, last_prune: {
    ...historyStatus().status.last_prune, size_limit_bytes: 1024, size_limit_unmet: true,
  } })), /size limit remains unmet/);
});

test("history runtime status rejects malformed signed replies and counters", () => {
  const parse = pages.parseHistoryRuntimeStatus;
  for (const reply of [null, {}, { ...historyStatus(), ok: false },
    historyStatus({ available: "true" }), historyStatus({ failed_operations: -1 }),
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
    started_at_unix: 100,
    ended_at_unix: 120,
    duration_seconds: 20,
  };
}
