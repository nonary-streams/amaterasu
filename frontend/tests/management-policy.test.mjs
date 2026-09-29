import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({});
vm.runInContext(ts.transpile(readFileSync("src/management-policy.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
const plain = value => JSON.parse(JSON.stringify(value));
const reply = (revision, selection) => ({ ok: true, revision,
  values: { origin_web_ui_allowed: selection }, origin_web_ui_allowed: selection ?? "lan" });

test("management policy reply checks effective and stored selections independently", () => {
  for (const selection of [null, "pc", "lan", "wan"])
    assert.deepEqual(plain(context.Amaterasu.parseManagementPolicyReply(reply(0, selection))), reply(0, selection));
  for (const value of [null, [], { ok: false }, reply(-1, "pc"), reply(1.5, "pc"),
    reply(Number.MAX_SAFE_INTEGER + 1, "pc"), reply(1, "WAN"),
    { ...reply(1, "pc"), origin_web_ui_allowed: "wan" },
    { ...reply(1, null), origin_web_ui_allowed: "pc" },
    { ...reply(1, "pc"), values: {} }])
    assert.throws(() => context.Amaterasu.parseManagementPolicyReply(value));
});

test("management policy save uses CAS and confirms durable readback including null reset", async () => {
  for (const selection of [null, "pc", "lan", "wan"]) {
    const calls = [];
    const api = { async post(path, payload = {}) {
      calls.push([path, plain(payload)]);
      return reply(5, selection);
    }};
    assert.deepEqual(plain(await context.Amaterasu.saveManagementPolicy(api, reply(4, "pc"), selection)), reply(5, selection));
    assert.deepEqual(calls, [["management_settings/update", { expected_revision: 4, origin_web_ui_allowed: selection }], ["management_settings/get", {}]]);
  }
});

test("management policy save rejects conflicts and never reports an unconfirmed write saved", async () => {
  let reads = 0;
  const conflict = { async post() { return { ok: false, code: "revision_conflict" }; } };
  await assert.rejects(context.Amaterasu.saveManagementPolicy(conflict, reply(4, "pc"), "wan"), /another editor/);
  for (const changed of [reply(4, "wan"), reply(5, "lan"), reply(6, "wan")]) {
    const api = { async post(path) { if (path.endsWith("update")) return changed; ++reads; return reply(5, "wan"); } };
    await assert.rejects(context.Amaterasu.saveManagementPolicy(api, reply(4, "pc"), "wan"), /could not be confirmed/);
  }
  assert.equal(reads, 0);
  const stale = { async post(path) { return path.endsWith("update") ? reply(5, "wan") : reply(6, "pc"); } };
  await assert.rejects(context.Amaterasu.saveManagementPolicy(stale, reply(4, "pc"), "wan"), /after saving/);
});

test("management policy form loads and saves the selected tier then reloads its revision", async () => {
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
  const dom = vm.createContext({ document: { createElement: name => new Element(name) } });
  for (const source of ["src/ui.ts", "src/management-policy.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", dom);
  }
  let stored = reply(0, null);
  const writes = [];
  const notices = [];
  const content = new Element("main");
  dom.Amaterasu.renderManagementPolicy({ content, notify: value => notices.push(value), api: {
    allows: () => true,
    async post(path, payload) {
      if (path.endsWith("update")) {
        writes.push(plain(payload));
        stored = reply(stored.revision + 1, payload.origin_web_ui_allowed);
      }
      return stored;
    },
  }});
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(find(content, node => node.tagName === "select").value, "");
  find(content, node => node.tagName === "select").value = "pc";
  find(content, node => node.tagName === "form").listeners.get("submit")({ preventDefault() {} });
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(writes, [{ expected_revision: 0, origin_web_ui_allowed: "pc" }]);
  assert.equal(find(content, node => node.tagName === "select").value, "pc");
  assert.deepEqual(notices, ["Management access policy saved."]);
});
