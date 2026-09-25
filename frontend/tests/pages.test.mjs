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

function createHarness({ allowsRefresh, allowsRead = true, forceReply, statuses }) {
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
        return {
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
  const succeeded = syncStatus();
  const accepted = {
    ...pending,
    ok: true,
    accepted: true,
    settings_ready: true,
  };
  const harness = createHarness({
    allowsRefresh: true,
    forceReply: accepted,
    statuses: [syncStatus(), pending, succeeded, succeeded],
  });

  await waitFor(
    () => pageReady(harness.content),
    "initial Steam page render",
  );
  const forceSync = findButton(harness.content, "Force catalog sync");
  assert.ok(forceSync);
  assert.equal(forceSync.disabled, false);
  forceSync.click();

  await waitFor(
    () => harness.timers.size === 1,
    "the locally accepted request to begin polling",
  );
  const forceCall = harness.calls.find(
    (call) => call.route === "steam/force_sync",
  );
  assert.deepEqual(forceCall, { route: "steam/force_sync", payload: {} });
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
    () => harness.calls.filter((call) => call.route === "steam/list").length >= 2,
    "the post-sync Steam list refresh",
  );
  assert.equal(harness.timers.size, 0);
  assert.equal(
    harness.calls.filter((call) => call.route === "steam/status").length,
    3,
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
