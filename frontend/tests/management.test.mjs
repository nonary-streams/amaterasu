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

  click() {
    this.listeners.get("click")?.();
  }

  get textContent() {
    return this.children
      .map((child) => typeof child === "string" ? child : child?.textContent ?? "")
      .join("");
  }
}

function loadManagement() {
  const storageOperations = [];
  const storage = {
    getItem(key) { storageOperations.push(["get", key]); return null; },
    setItem(key, value) { storageOperations.push(["set", key, value]); },
    removeItem(key) { storageOperations.push(["remove", key]); },
    clear() { storageOperations.push(["clear"]); },
  };
  const context = vm.createContext({
    document: { createElement: (tag) => new FakeElement(tag) },
    localStorage: storage,
    sessionStorage: storage,
    window: { localStorage: storage, sessionStorage: storage },
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
  return { module: context.Amaterasu, storageOperations };
}

function descendants(node, tagName) {
  return [
    ...(node.tagName === tagName ? [node] : []),
    ...node.children.flatMap((child) => child instanceof FakeElement ? descendants(child, tagName) : []),
  ];
}

function connectedContent() {
  const content = new FakeElement("main");
  content.setConnected(true);
  return content;
}

function clickButton(root, label) {
  const button = descendants(root, "button").find((candidate) => candidate.textContent === label);
  assert.ok(button, `expected button ${label}`);
  button.click();
  return button;
}

function defer() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function request(name, awaitingPin = true) {
  return {
    pairing_id: "a".repeat(32),
    client_id: `client-${name.toLowerCase().replaceAll(" ", "-")}`,
    name,
    expires_at: 100,
    awaiting_pin: awaitingPin,
  };
}

function pairingReply(...pending) {
  return { ok: true, pending };
}

function baseReply(route) {
  if (route === "nvhttp/settings") return {
    ok: true,
    hostname: "amaterasu-host",
    revision: 4,
    active_http_port: 47989,
    active_https_port: null,
    active_rtsp_port: null,
    listener_error: null,
    rtsp_listener_error: null,
  };
  if (route === "session/list") return { ok: true, sessions: [], next_offset: null };
  if (route === "history/history_status") return { ok: true, status: {
    available: true, configuration_ready: true, active_streams: 0, failed_operations: 0,
  } };
  return { ok: true };
}

async function flushUi() {
  await new Promise((resolve) => setImmediate(resolve));
}

test("pending pairing parser accepts the empty and awaiting-PIN response shapes", () => {
  const { parsePendingPairings } = loadManagement().module;
  assert.deepEqual(
    JSON.parse(JSON.stringify(parsePendingPairings({ ok: true, pending: [] }))),
    { ok: true, pending: [] },
  );
  const parsed = parsePendingPairings({ ok: true, pending: [{
    ...request("Living room"),
    certificate_id: "b".repeat(64),
  }] });
  assert.equal(parsed.pending[0].name, "Living room");
  assert.equal(parsed.pending[0].awaiting_pin, true);
  for (const value of [null, { ok: false, pending: [] },
    { ok: true, pending: [{ ...request("Device"), pairing_id: "bad" }] },
    { ok: true, pending: [{ ...request("\u0000") }] },
  ]) assert.throws(() => parsePendingPairings(value));
});

test("management page avoids routes for scopes the signed-in principal lacks", async () => {
  const { module } = loadManagement();
  const content = connectedContent();
  const calls = [];
  module.renderHostManagement({
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

test("double submit sends one approval and shows protocol progress instead of completion", async () => {
  const { module, storageOperations } = loadManagement();
  const content = connectedContent();
  const calls = [];
  const approval = defer();
  let pendingRead = 0;
  const notifications = [];
  const api = {
    allows: () => true,
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "pairing/pending")
        return pairingReply(request("Living room", pendingRead++ === 0));
      if (route === "pairing/approve_pin") return approval.promise;
      return baseReply(route);
    },
  };
  module.renderHostManagement({ content, api, notify: (message) => notifications.push(message), navigate() {} });
  await flushUi();

  const form = descendants(content, "form")[0];
  const pin = descendants(form, "input")[0];
  const approve = descendants(form, "button")[0];
  pin.value = "0427";
  form.listeners.get("submit")({ preventDefault() {} });
  assert.equal(approve.disabled, true);
  form.listeners.get("submit")({ preventDefault() {} });
  assert.equal(pin.value, "");
  assert.equal(calls.filter((call) => call.route === "pairing/approve_pin").length, 1);
  assert.doesNotMatch(content.textContent, /Enter the four-digit PIN shown on the device/);
  approval.resolve({ ok: true });
  await flushUi();

  const approvalCall = calls.find((call) => call.route === "pairing/approve_pin");
  assert.equal(JSON.stringify(approvalCall.payload), JSON.stringify({ pairing_id: "a".repeat(32), pin: "0427" }));
  assert.match(content.textContent, /Pairing in progress/);
  assert.match(content.textContent, /must complete its pairing exchange/);
  assert.doesNotMatch(content.textContent, /0427|paired successfully|pairing complete/i);
  assert.equal(notifications.includes("PIN approval accepted. The device must complete its pairing exchange."), true);
  assert.equal(storageOperations.length, 0);
});

test("interrupted approval refreshes read-only status without replaying the PIN", async () => {
  const { module, storageOperations } = loadManagement();
  const content = connectedContent();
  const calls = [];
  let pendingRead = 0;
  const api = {
    allows: () => true,
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "pairing/pending")
        return pairingReply(request("Den room", pendingRead++ === 0));
      if (route === "pairing/approve_pin") throw new Error("connection interrupted");
      return baseReply(route);
    },
  };
  module.renderHostManagement({ content, api, notify() {}, navigate() {} });
  await flushUi();
  const form = descendants(content, "form")[0];
  const pin = descendants(form, "input")[0];
  pin.value = "9132";
  form.listeners.get("submit")({ preventDefault() {} });
  assert.equal(pin.value, "");
  await flushUi();

  assert.equal(calls.filter((call) => call.route === "pairing/approve_pin").length, 1);
  assert.equal(calls.filter((call) => call.route === "pairing/pending").length, 2);
  assert.match(content.textContent, /Approval not confirmed/);
  assert.match(content.textContent, /Pairing in progress/);
  assert.match(content.textContent, /must complete its pairing exchange/);
  assert.doesNotMatch(content.textContent, /9132|paired successfully|pairing complete/i);
  assert.equal(descendants(content, "form").length, 0);
  assert.equal(storageOperations.length, 0);
});

test("failed approval keeps the request visible with a cleared PIN and no automatic retry", async () => {
  const { module } = loadManagement();
  const content = connectedContent();
  const calls = [];
  const api = {
    allows: () => true,
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "pairing/pending") return pairingReply(request("Office PC"));
      if (route === "pairing/approve_pin") throw new Error("rejected by host");
      return baseReply(route);
    },
  };
  module.renderHostManagement({ content, api, notify() {}, navigate() {} });
  await flushUi();
  const form = descendants(content, "form")[0];
  const pin = descendants(form, "input")[0];
  pin.value = "0001";
  form.listeners.get("submit")({ preventDefault() {} });
  await flushUi();

  assert.equal(calls.filter((call) => call.route === "pairing/approve_pin").length, 1);
  assert.equal(calls.filter((call) => call.route === "pairing/pending").length, 2);
  assert.match(content.textContent, /Approval not confirmed/);
  assert.match(content.textContent, /PIN was cleared/);
  assert.match(content.textContent, /PIN required/);
  assert.equal(descendants(content, "input")[0].value, "");
  assert.doesNotMatch(content.textContent, /0001/);
});

test("newer pending-list results win over older requests", async () => {
  const { module } = loadManagement();
  const content = connectedContent();
  const first = defer();
  const second = defer();
  let pairingRead = 0;
  const api = {
    allows: () => true,
    async post(route) {
      if (route === "pairing/pending") return [first.promise, second.promise][pairingRead++];
      return baseReply(route);
    },
  };
  module.renderHostManagement({ content, api, notify() {}, navigate() {} });
  await Promise.resolve();
  clickButton(content, "Refresh all");
  await Promise.resolve();
  second.resolve(pairingReply(request("Current device")));
  await flushUi();
  first.resolve(pairingReply(request("Stale device")));
  await flushUi();

  assert.match(content.textContent, /Current device/);
  assert.doesNotMatch(content.textContent, /Stale device/);
});

test("unmounted pairing reads cannot repopulate the page after a reload", async () => {
  const { module } = loadManagement();
  const oldContent = connectedContent();
  const oldRead = defer();
  let pairingRead = 0;
  const api = {
    allows: () => true,
    async post(route) {
      if (route === "pairing/pending")
        return pairingRead++ === 0 ? oldRead.promise : pairingReply(request("After reload"));
      return baseReply(route);
    },
  };
  module.renderHostManagement({ content: oldContent, api, notify() {}, navigate() {} });
  await Promise.resolve();
  oldContent.setConnected(false);

  const newContent = connectedContent();
  module.renderHostManagement({ content: newContent, api, notify() {}, navigate() {} });
  await flushUi();
  oldRead.resolve(pairingReply(request("Before unmount")));
  await flushUi();

  assert.match(newContent.textContent, /After reload/);
  assert.doesNotMatch(newContent.textContent, /Before unmount/);
  assert.match(oldContent.textContent, /Loading pending pairing requests/);
  assert.doesNotMatch(oldContent.textContent, /Before unmount/);
});

test("an approval finishing after unmount does not refresh or notify the old page", async () => {
  const { module } = loadManagement();
  const content = connectedContent();
  const approval = defer();
  const calls = [];
  const notifications = [];
  const api = {
    allows: () => true,
    async post(route, payload = {}) {
      calls.push({ route, payload });
      if (route === "pairing/pending") return pairingReply(request("Bedroom PC"));
      if (route === "pairing/approve_pin") return approval.promise;
      return baseReply(route);
    },
  };
  module.renderHostManagement({ content, api, notify: (message) => notifications.push(message), navigate() {} });
  await flushUi();
  const form = descendants(content, "form")[0];
  const pin = descendants(form, "input")[0];
  pin.value = "7824";
  form.listeners.get("submit")({ preventDefault() {} });
  assert.equal(pin.value, "");
  content.setConnected(false);
  approval.resolve({ ok: true });
  await flushUi();

  assert.equal(calls.filter((call) => call.route === "pairing/approve_pin").length, 1);
  assert.equal(calls.filter((call) => call.route === "pairing/pending").length, 1);
  assert.deepEqual(notifications, []);
  assert.doesNotMatch(content.textContent, /7824/);
});
