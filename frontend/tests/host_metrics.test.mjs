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
    this.isConnected = true;
  }

  append(...children) {
    this.children.push(...children.filter((child) => child != null));
  }

  replaceChildren(...children) {
    this.children = [];
    this.append(...children);
  }

  addEventListener(event, listener) {
    this.listeners.set(event, listener);
  }

  setAttribute() {}

  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join("");
  }
}

function descendants(element) {
  return [element, ...element.children.flatMap((child) => child instanceof FakeElement ? descendants(child) : [])];
}

const settings = {
  enabled: true,
  poll_interval_ms: 2000,
  history_retention_seconds: 300,
  max_history_points: 300,
  pause_when_hidden: true,
  show_active_sessions: true,
  show_host_stats: true,
  show_host_charts: true,
  show_session_history: true,
};

test("host metrics settings editor reads, bounds and confirms revisioned preferences", async () => {
  const calls = [];
  let stored = { ok: true, settings, revision: 4 };
  const context = vm.createContext({
    document: { createElement: (tag) => new FakeElement(tag) },
    Error,
    console,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    if (source === "src/model.ts" || source === "src/ui.ts")
      vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }

  const container = new FakeElement("div");
  const page = {
    api: {
      allows: (scope) => scope === "configuration.write",
      async post(route, body = {}) {
        calls.push([route, body]);
        if (route === "host-metrics/settings") return stored;
        assert.equal(route, "host-metrics/settings_update");
        assert.equal(body.expected_revision, 4);
        assert.equal(body.settings.poll_interval_ms, 2500);
        stored = { ok: true, settings: body.settings, revision: 5 };
        return stored;
      },
    },
    notify() {},
  };
  context.Amaterasu.renderHostMetricsSettings(page, container);
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(calls.map(([route]) => route), ["host-metrics/settings"]);
  assert.match(container.textContent, /Saved settings revision 4/);
  const controls = descendants(container).filter((element) => element.tagName === "input");
  const byName = new Map(controls.map((control) => [control.name, control]));
  assert.equal(byName.get("realtime_stats_poll_interval_ms").min, "250");
  assert.equal(byName.get("realtime_stats_poll_interval_ms").max, "60000");
  assert.equal(byName.get("realtime_stats_history_retention_seconds").min, "30");
  assert.equal(byName.get("realtime_stats_history_retention_seconds").max, "3600");
  assert.equal(byName.get("realtime_stats_max_history_points").min, "30");
  assert.equal(byName.get("realtime_stats_max_history_points").max, "2000");
  assert.equal(byName.get("realtime_stats_enabled").checked, true);
  assert.equal(controls.length, 9);

  byName.get("realtime_stats_poll_interval_ms").valueAsNumber = 2500;
  byName.get("realtime_stats_history_retention_seconds").valueAsNumber = 300;
  byName.get("realtime_stats_max_history_points").valueAsNumber = 300;
  const form = descendants(container).find((element) => element.tagName === "form");
  form.listeners.get("submit")({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls.map(([route]) => route), [
    "host-metrics/settings",
    "host-metrics/settings_update",
    "host-metrics/settings",
    "host-metrics/settings",
  ]);
  assert.match(container.textContent, /Saved settings revision 5/);
});
