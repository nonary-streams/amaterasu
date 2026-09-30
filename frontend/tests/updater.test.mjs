import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ URL });
class Element {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.listeners = new Map();
    this.isConnected = true;
  }

  append(...children) {
    this.children.push(...children.filter((child) => child != null));
  }

  replaceChildren(...children) {
    this.children = children.filter((child) => child != null);
  }

  addEventListener(name, listener) {
    this.listeners.set(name, listener);
  }

  setAttribute() {}

  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
  }

  fire(name, event = {}) {
    this.listeners.get(name)?.(event);
  }
}

context.document = { createElement: (tagName) => new Element(tagName) };
vm.runInContext(
  ts.transpile(readFileSync("src/ui.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
vm.runInContext(
  ts.transpile(readFileSync("src/updater.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
const updater = context.Amaterasu;
const ordinary = (value) => JSON.parse(JSON.stringify(value));
const defaultSettings = {
  notify_pre_releases: false,
  update_check_interval: 86400,
};

test("updater settings accept zero-disable and bounded seconds", () => {
  const parse = updater.parseUpdaterSettingsReply;
  assert.deepEqual(ordinary(parse({ ok: true, revision: 0, settings: defaultSettings })), {
    ok: true,
    revision: 0,
    settings: defaultSettings,
  });
  assert.equal(parse({ ok: true, revision: 1,
    settings: { notify_pre_releases: true, update_check_interval: 0 } }).settings.update_check_interval, 0);
  assert.equal(parse({ ok: true, revision: 1,
    settings: { notify_pre_releases: false, update_check_interval: 2147483647 } }).settings.update_check_interval, 2147483647);
  for (const interval of [-1, 1.5, Number.NaN, 2147483648, "60"]) {
    assert.throws(() => parse({ ok: true, revision: 0,
      settings: { notify_pre_releases: false, update_check_interval: interval } }));
  }
  for (const settings of [
    { notify_pre_releases: false },
    { notify_pre_releases: false, update_check_interval: 60, extra: true },
    { notify_pre_releases: "false", update_check_interval: 60 },
  ]) {
    assert.throws(() => parse({ ok: true, revision: 0, settings }));
  }
});

test("updater status accepts selected release and delivery states", () => {
  const status = updater.parseUpdaterStatusReply({
    ok: true,
    settings_ready: true,
    revision: 4,
    check_in_progress: false,
    last_check_unix_ms: 1790784000000,
    last_error: null,
    latest_stable: { version: "v2.0.0", url: "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0" },
    latest_prerelease: null,
    available_update: {
      version: "v2.0.0",
      release_url: "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0",
      prerelease: false,
    },
    notification: {
      version: "v2.0.0",
      prerelease: false,
      release_url: "https://github.com/Nonary/amaterasu/releases/tag/v2.0.0",
      delivery: "notified",
    },
  });
  assert.equal(status.available_update.version, "v2.0.0");
  assert.equal(status.notification.delivery, "notified");
  assert.throws(() => updater.parseUpdaterStatusReply({
    ok: true,
    settings_ready: true,
    revision: 4,
    check_in_progress: false,
    last_check_unix_ms: null,
    last_error: null,
    latest_stable: { version: "v2.0.0", url: "https://example.invalid/update" },
    latest_prerelease: null,
    available_update: null,
    notification: null,
  }));
});

test("updater settings use a revisioned write and verify persisted readback", async () => {
  const calls = [];
  let stored = { ok: true, revision: 7, settings: defaultSettings };
  const api = {
    async post(route, body = {}) {
      calls.push([route, ordinary(body)]);
      if (route === "updater/settings") return ordinary(stored);
      assert.equal(route, "updater/settings_update");
      assert.deepEqual(ordinary(body), {
        settings: { notify_pre_releases: true, update_check_interval: 0 },
        expected_revision: 7,
      });
      stored = {
        ok: true,
        revision: stored.revision + 1,
        settings: ordinary(body.settings),
      };
      return ordinary(stored);
    },
  };
  const saved = await updater.saveUpdaterSettings(api, stored, {
    notify_pre_releases: true,
    update_check_interval: 0,
  });
  assert.deepEqual(ordinary(saved), stored);
  assert.deepEqual(calls.map(([route]) => route), [
    "updater/settings_update",
    "updater/settings",
  ]);
});

test("updater settings reject stale writes and unconfirmed readback", async () => {
  await assert.rejects(
    updater.saveUpdaterSettings({
      async post(route) {
        assert.equal(route, "updater/settings_update");
        throw new Error("revision_conflict");
      },
    }, { ok: true, revision: 1, settings: defaultSettings }, {
      notify_pre_releases: true,
      update_check_interval: 60,
    }),
    /revision_conflict/,
  );
  for (const readback of [
    { ok: true, revision: 1, settings: { notify_pre_releases: true, update_check_interval: 60 } },
    { ok: true, revision: 3, settings: { notify_pre_releases: true, update_check_interval: 60 } },
    { ok: true, revision: 2, settings: { notify_pre_releases: false, update_check_interval: 60 } },
  ]) {
    await assert.rejects(
      updater.saveUpdaterSettings({
        async post(route) {
          if (route === "updater/settings_update")
            return { ok: true, revision: 2, settings: { notify_pre_releases: true, update_check_interval: 60 } };
          assert.equal(route, "updater/settings");
          return readback;
        },
      }, { ok: true, revision: 1, settings: defaultSettings }, {
        notify_pre_releases: true,
        update_check_interval: 60,
      }),
      /could not be confirmed/,
    );
  }
});

test("software updates settings editor loads persisted preferences and routes a manual check", async () => {
  const calls = [];
  const status = {
    ok: true,
    settings_ready: true,
    revision: 9,
    check_in_progress: false,
    last_check_unix_ms: null,
    last_error: null,
    latest_stable: null,
    latest_prerelease: null,
    available_update: null,
    notification: null,
  };
  const api = {
    allows: () => true,
    async post(route) {
      calls.push(route);
      if (route === "updater/settings")
        return { ok: true, revision: 9, settings: defaultSettings };
      if (route === "updater/status") return status;
      if (route === "updater/check")
        return { ok: true, started: true, in_progress: true };
      throw new Error(`unexpected route ${route}`);
    },
  };
  const container = new Element("div");
  updater.renderUpdaterSettings({ api, notify() {} }, container);
  await new Promise(setImmediate);
  const find = (node, predicate) => {
    if (predicate(node)) return node;
    for (const child of node.children) {
      if (typeof child === "string") continue;
      const found = find(child, predicate);
      if (found) return found;
    }
    return null;
  };
  const preReleases = find(container, (node) => node.name === "notify_pre_releases");
  const interval = find(container, (node) => node.name === "update_check_interval");
  assert.ok(preReleases);
  assert.equal(preReleases.checked, false);
  assert.ok(interval);
  assert.equal(interval.value, "86400");
  assert.equal(interval.min, "0");
  assert.equal(interval.step, "60");
  assert.equal(interval.required, true);
  const check = find(container, (node) => node.textContent === "Check for updates now");
  assert.ok(check);
  check.fire("click");
  await new Promise(setImmediate);
  assert.deepEqual(calls, [
    "updater/settings",
    "updater/status",
    "updater/check",
    "updater/status",
  ]);
});
