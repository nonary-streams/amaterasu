import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag, text = "") { this.tag = tag; this.text = text; this.children = []; this.listeners = {}; this.isConnected = true; this.checked = false; this.disabled = false; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  addEventListener(type, fn) { this.listeners[type] = fn; }
  setAttribute(name, value) { this[name] = value; }
  querySelectorAll() { return this.children.flatMap((child) => child instanceof Element ? [...(["input", "button"].includes(child.tag) ? [child] : []), ...child.querySelectorAll()] : []); }
  get textContent() { return this.text + this.children.map((c) => c.textContent ?? c).join(""); }
  set textContent(value) { this.text = value; this.children = []; }
  click() { if (!this.disabled) this.listeners.click?.(); }
}
const context = vm.createContext({ window: { confirm: () => true }, Object, Array, Set, Map, Number, Error, RegExp });
vm.runInContext(`var Amaterasu = {};
function el(tag, cls, ...children) { const node = new Element(tag); node.append(...children); return node; }
function input(name, value, type) { const node = new Element('input'); node.name = name; node.type = type; node.value = value; return node; }
function field(label, control, help) { return el('label', '', label, control, help || ''); }
function button(label, click) { const node = new Element('button', label); node.addEventListener('click', click); return node; }
function panel(label, ...children) { return el('section', '', label, ...children); }
function errorPanel(error) { return el('div', '', String(error)); }`, Object.assign(context, { Element }));
vm.runInContext(ts.transpile(readFileSync("src/permission-management.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
const render = context.Amaterasu.renderClientPermissions;
const id = "a".repeat(64);
const perms = { list_apps: false, view_streams: true, launch_apps: false, terminate_streams: true };
const reply = (enabled = true) => ({ ok: true, revision: 4, permission_mode_enabled: enabled, clients: [{ certificate_id: id, name: "Living room", permissions: { ...perms } }] });
const tick = () => new Promise((resolve) => setImmediate(resolve));
const find = (area, tag, name) => area.querySelectorAll().find((node) => node.tag === tag && (node.name === name || node.text === name));

test("disabled mode hides per-client controls; missing permission scope makes no request", async () => {
  const area = new Element("div"), content = new Element("main"); let calls = 0;
  render({ content, api: { allows: () => false, post: async () => { calls++; } } }, area);
  assert.equal(calls, 0);
  render({ content, api: { allows: () => true, post: async () => reply(false) } }, area);
  await tick();
  assert.ok(find(area, "input", "permission_mode_enabled"));
  assert.equal(find(area, "input", `list_apps_${id}`), undefined);
});

test("saving grants preserves termination and confirms revision via readback", async () => {
  const area = new Element("div"), content = new Element("main"), calls = []; let saved;
  const api = { allows: () => true, async post(path, payload) {
    calls.push([path, payload]);
    if (path === "pairing/update_client_permissions") { saved = payload.permissions; return { ok: true }; }
    if (saved) { const result = reply(); result.revision = 5; result.clients[0].permissions = saved; return result; }
    return reply();
  } };
  render({ content, api, notify() {} }, area); await tick();
  assert.match(area.textContent, /Effective listing: allowed/);
  find(area, "input", `list_apps_${id}`).checked = true;
  find(area, "button", "Save permissions").click(); await tick(); await tick();
  assert.equal(calls[1][0], "pairing/update_client_permissions");
  assert.equal(calls[1][1].expected_revision, 4);
  assert.equal(saved.terminate_streams, true);
  assert.equal(saved.view_streams, true);
  assert.equal(saved.list_apps, true);
  assert.equal(calls[2][0], "pairing/list_clients");
});

test("invalid or unknown grants fail closed", () => {
  const result = reply(); result.clients[0].permissions.future = true;
  assert.throws(() => context.Amaterasu.parseClientPermissionState(result), /cannot safely preserve/);
});


test("paired-client permission search filters by name or fingerprint without discarding hidden drafts", async () => {
  const area = new Element("div"), content = new Element("main");
  const state = reply();
  state.clients.push({ certificate_id: "b".repeat(64), name: "Desk", permissions: { ...perms } });
  render({ content, api: { allows: () => true, post: async () => state } }, area);
  await tick();
  const search = find(area, "input", "client_permission_search");
  const allNodes = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? allNodes(child) : [])];
  const cards = allNodes(area).filter((node) => node.tag === "section" && node.children.some((child) => child instanceof Element && child.tag === "h3"));
  assert.equal(cards.length, 2);
  const hiddenDraft = find(cards[1], "input", `list_apps_${"b".repeat(64)}`);
  hiddenDraft.checked = true;
  search.value = "living room";
  search.listeners.input();
  assert.equal(cards[0].hidden, false);
  assert.equal(cards[1].hidden, true);
  assert.equal(hiddenDraft.checked, true);
  assert.match(area.textContent, /Showing 1 of 2 paired clients/);
  search.value = "bbbbbbbbbbbb";
  search.listeners.input();
  assert.equal(cards[0].hidden, true);
  assert.equal(cards[1].hidden, false);
  search.value = "not a device";
  search.listeners.input();
  assert.equal(cards.every((card) => card.hidden), true);
  assert.equal(area.textContent.includes("No paired clients match this search."), true);
});
