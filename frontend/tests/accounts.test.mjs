import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; this.value = ""; }
  append(...children) { this.children.push(...children.filter((child) => child != null)); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute() {}
  reportValidity() { return true; }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
  click() { this.listeners.get("click")?.(); }
}
function descendants(node) {
  return [node, ...(node.children ?? []).flatMap((child) => child instanceof Element ? descendants(child) : [])];
}

test("account cards submit the selected role for the exact account identity", async () => {
  const context = vm.createContext({ document: { createElement: (tag) => new Element(tag) }, window: { confirm: () => true }, Error });
  for (const source of ["src/ui.ts", "src/accounts.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const calls = [];
  const content = new Element("main");
  context.Amaterasu.renderAccounts({ content, notify() {}, api: {
    allows: () => true,
    async post(route, body) {
      calls.push([route, body]);
      if (route === "accounts/list") return { accounts: [{ username: "viewer", role: "viewer", enabled: true }] };
      if (route === "accounts/update") return { ok: true };
      throw new Error(`Unexpected route ${route}`);
    },
  } });
  await new Promise(setImmediate);
  const card = descendants(content).find((node) => node.className === "account-card");
  assert.ok(card);
  const role = descendants(card).find((node) => node.tagName === "select");
  role.value = "operator";
  descendants(card).find((node) => node.tagName === "button" && node.textContent === "Save account").click();
  await new Promise(setImmediate);
  const update = calls.find(([route]) => route === "accounts/update");
  assert.equal(update[1].username, "viewer");
  assert.equal(update[1].role, "operator");
  assert.equal(update[1].enabled, true);
});
