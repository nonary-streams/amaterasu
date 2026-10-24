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
    this.value = "";
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

  get textContent() {
    return this.children
      .map((child) => typeof child === "string" ? child : child?.textContent ?? "")
      .join("");
  }
}

function loadManagement() {
  const context = vm.createContext({
    document: { createElement: (tag) => new FakeElement(tag) },
    Date,
    Error,
    Number,
    Object,
    Promise,
    RegExp,
    String,
    TextEncoder,
  });
  for (const source of ["src/types.ts", "src/ui.ts", "src/management.ts"]) {
    vm.runInContext(
      ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }),
      context,
    );
    if (source === "src/ui.ts")
      vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  return context.Amaterasu;
}

function descendants(node, tagName) {
  return [
    ...(node.tagName === tagName ? [node] : []),
    ...node.children.flatMap((child) => child instanceof FakeElement ? descendants(child, tagName) : []),
  ];
}

function hostReply() {
  return {
    ok: true,
    hostname: "amaterasu-host",
    revision: 4,
    active_http_port: 47989,
    active_https_port: null,
    active_rtsp_port: null,
    listener_error: null,
    rtsp_listener_error: null,
  };
}

test("pending pairing parser accepts the verified empty and awaiting-PIN shapes", () => {
  const { parsePendingPairings } = loadManagement();
  assert.deepEqual(
    JSON.parse(JSON.stringify(parsePendingPairings({ ok: true, pending: [] }))),
    { ok: true, pending: [] },
  );
  const parsed = parsePendingPairings({ ok: true, pending: [{
    pairing_id: "a".repeat(32),
    client_id: "client-7",
    name: "Living room",
    certificate_id: "b".repeat(64),
    expires_at: 100,
    awaiting_pin: true,
  }] });
  assert.equal(parsed.pending[0].name, "Living room");
  assert.equal(parsed.pending[0].awaiting_pin, true);
  for (const value of [null, { ok: false, pending: [] },
    { ok: true, pending: [{ pairing_id: "bad", client_id: "c", name: "Device", expires_at: 1, awaiting_pin: true }] },
    { ok: true, pending: [{ pairing_id: "a".repeat(32), client_id: "c", name: "\u0000", expires_at: 1, awaiting_pin: true }] },
  ]) assert.throws(() => parsePendingPairings(value));
});

test("management page does not call routes outside the signed-in scopes", async () => {
  const { renderHostManagement } = loadManagement();
  const content = new FakeElement("main");
  content.setConnected(true);
  const calls = [];
  renderHostManagement({
    content,
    api: {
      allows: () => false,
      async post(route) { calls.push(route); throw new Error("must not be called"); },
    },
    notify() {},
    navigate() {},
  });
  await Promise.resolve();
  assert.deepEqual(calls, []);
  assert.match(content.textContent, /does not have permission/);
  assert.match(content.textContent, /requires configuration.read/);
  assert.match(content.textContent, /requires session.read/);
});

test("pairing approval clears the PIN field before sending the scoped request", async () => {
  const { renderHostManagement } = loadManagement();
  const content = new FakeElement("main");
  content.setConnected(true);
  const calls = [];
  let pendingRead = 0;
  const api = {
    allows: () => true,
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "pairing/pending") {
        pendingRead += 1;
        return { ok: true, pending: pendingRead === 1 ? [{
          pairing_id: "a".repeat(32),
          client_id: "client-7",
          name: "Living room",
          expires_at: 100,
          awaiting_pin: true,
        }] : [] };
      }
      if (route === "nvhttp/settings") return hostReply();
      if (route === "session/list") return { ok: true, sessions: [], next_offset: null };
      if (route === "history/history_status") return { ok: true, status: {
        available: true, configuration_ready: true, active_streams: 0, failed_operations: 0,
      } };
      if (route === "pairing/approve_pin") return { ok: true };
      throw new Error(`unexpected route ${route}`);
    },
  };
  renderHostManagement({ content, api, notify() {}, navigate() {} });
  await new Promise((resolve) => setImmediate(resolve));

  const form = descendants(content, "form")[0];
  const pin = descendants(form, "input")[0];
  pin.value = "0427";
  form.listeners.get("submit")({ preventDefault() {} });
  assert.equal(pin.value, "");
  await new Promise((resolve) => setImmediate(resolve));

  const approval = calls.find((call) => call.route === "pairing/approve_pin");
  assert.equal(
    JSON.stringify(approval.payload),
    JSON.stringify({ pairing_id: "a".repeat(32), pin: "0427" }),
  );
  assert.equal(pin.value, "");
  assert.doesNotMatch(content.textContent, /0427/);
  assert.deepEqual(calls.map((call) => call.route).filter((route) => route === "pairing/approve_pin"), ["pairing/approve_pin"]);
});
