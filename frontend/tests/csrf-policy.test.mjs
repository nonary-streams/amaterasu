import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ URL });
vm.runInContext(ts.transpile(readFileSync("src/csrf-policy.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
const plain = value => JSON.parse(JSON.stringify(value));
const reply = (revision, selection) => ({ ok: true, revision,
  values: { csrf_allowed_origins: selection }, csrf_allowed_origins: selection ?? [] });

test("CSRF origins reject wildcard, non-HTTPS, URL extras and ambiguous numeric authorities", () => {
  for (const origin of ["https://localhost", "https://example.com:47990", "https://127.0.0.1:443", "https://[::1]", "https://[fd00::1]:47990"])
    assert.equal(context.Amaterasu.validCsrfOrigin(origin), true, origin);
  for (const origin of [null, "", "*", "https://*", "http://localhost", "HTTPS://localhost", "https://", "https://user@host", "https://host/", "https://host?q=1", "https://host#fragment", "https://host:", "https://host:0", "https://host:65536", "https://host:443:1", "https://[::1", "https://[::1]/", "https://[::1]:", "https://[not-an-ip]", "https://host..invalid", "https://-host", "https://host-", "https://127.1", "https://2130706433", "https://host\r\n", "https://0x7f000001", "https://0x7f.0.0.1", "https://host.123", "https://0x", "https://1.0X"])
    assert.equal(context.Amaterasu.validCsrfOrigin(origin), false, String(origin));
});

test("CSRF reply preserves null and explicit empty and checks effective values", () => {
  for (const selection of [null, [], ["https://trusted.example:47990"]])
    assert.deepEqual(plain(context.Amaterasu.parseCsrfPolicyReply(reply(0, selection))), reply(0, selection));
  for (const value of [null, [], { ok: false }, reply(-1, []), reply(1.5, []), reply(Number.MAX_SAFE_INTEGER + 1, []),
    reply(1, ["http://host"]), reply(1, Array(17).fill("https://host")), reply(1, ["https://host", "https://host"]),
    { ...reply(1, []), values: {} }, { ...reply(1, ["https://host"]), csrf_allowed_origins: [] }])
    assert.throws(() => context.Amaterasu.parseCsrfPolicyReply(value));
});

test("CSRF save updates only its field with CAS and confirms readback", async () => {
  for (const selection of [null, [], ["https://trusted.example:47990"]]) {
    const calls = [];
    const api = { async post(path, payload = {}) { calls.push([path, plain(payload)]); return reply(5, selection); } };
    assert.deepEqual(plain(await context.Amaterasu.saveCsrfPolicy(api, reply(4, null), selection)), reply(5, selection));
    assert.deepEqual(calls, [["management_settings/update", { expected_revision: 4, csrf_allowed_origins: selection }], ["management_settings/get", {}]]);
  }
});

test("CSRF save rejects stale, unconfirmed and collapsed null/empty writes", async () => {
  const conflict = { async post() { return { ok: false, code: "revision_conflict" }; } };
  await assert.rejects(context.Amaterasu.saveCsrfPolicy(conflict, reply(4, null), []), /another editor/);
  for (const response of [reply(4, []), reply(6, []), reply(5, null)]) {
    let reads = 0;
    const api = { async post(path) { if (path.endsWith("get")) ++reads; return response; } };
    await assert.rejects(context.Amaterasu.saveCsrfPolicy(api, reply(4, null), []), /could not be confirmed/);
    assert.equal(reads, 0);
  }
  const stale = { async post(path) { return path.endsWith("update") ? reply(5, []) : reply(6, null); } };
  await assert.rejects(context.Amaterasu.saveCsrfPolicy(stale, reply(4, null), []), /after saving/);
});

class Element {
  constructor(tagName) { this.tagName = tagName; this.children = []; this.listeners = new Map(); this.isConnected = true; this.value = ""; }
  append(...children) { this.children.push(...children.filter(value => value != null)); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute() {}
  get textContent() { return this.children.map(child => typeof child === "string" ? child : child.textContent).join(""); }
  set textContent(value) { this.replaceChildren(value); }
}
const find = (node, predicate) => {
  if (predicate(node)) return node;
  for (const child of node.children ?? []) {
    if (typeof child === "string") continue;
    const result = find(child, predicate);
    if (result) return result;
  }
};

test("CSRF form saves explicit empty, restores null, and reloads durable revisions", async () => {
  const dom = vm.createContext({ URL, document: { createElement: name => new Element(name) } });
  for (const source of ["src/ui.ts", "src/csrf-policy.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", dom);
  }
  let stored = reply(0, null);
  const writes = [];
  const notices = [];
  const content = new Element("main");
  dom.Amaterasu.renderCsrfPolicy({ content, notify: value => notices.push(value), api: {
    allows: () => true,
    async post(path, payload) {
      if (path.endsWith("update")) {
        writes.push(plain(payload));
        stored = reply(stored.revision + 1, payload.csrf_allowed_origins);
      }
      return stored;
    },
  }});
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(find(content, node => node.tagName === "select").value, "default");
  assert.equal(find(content, node => node.tagName === "textarea").disabled, true);
  for (const [mode, origins, selection] of [["custom", "https://trusted.example:47990", ["https://trusted.example:47990"]], ["custom", "", []], ["default", "", null]]) {
    const select = find(content, node => node.tagName === "select");
    select.value = mode;
    select.listeners.get("change")();
    find(content, node => node.tagName === "textarea").value = origins;
    find(content, node => node.tagName === "form").listeners.get("submit")({ preventDefault() {} });
    await new Promise(resolve => setImmediate(resolve));
    assert.deepEqual(writes.at(-1), { expected_revision: writes.length - 1, csrf_allowed_origins: selection });
    assert.equal(find(content, node => node.tagName === "select").value, mode);
  }
  assert.equal(notices.length, 3);
});
