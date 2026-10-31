import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; this.value = ""; this.checked = false; }
  append(...children) { this.children.push(...children.filter((child) => child != null)); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute() {}
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
  click() { this.listeners.get("click")?.(); }
}
const descend = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? descend(child) : [])];
const find = (root, check) => descend(root).find(check);
const wait = async (check) => {
  for (let i = 0; i < 100; i++) { if (check()) return; await new Promise((resolve) => setTimeout(resolve, 1)); }
  assert.fail("timed out waiting for token UI");
};
function fixture(api) {
  const sandbox = vm.createContext({document:{createElement:(tag)=>new Element(tag)}, window:{confirm:()=>true}, navigator:{}, Error});
  for (const source of ["src/ui.ts", "src/tokens.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {target:ts.ScriptTarget.ES2022}), sandbox);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; globalThis.messageOf = error => error.message", sandbox);
  }
  const content = new Element("main");
  let guard = null;
  sandbox.Amaterasu.renderTokens({content, api, notify() {}, registerDraftGuard(isDirty, dispose) { guard = {isDirty, dispose}; } });
  content.guard = guard;
  return content;
}

test("token creation uses selected scopes, shows the secret once and supports revocation", async () => {
  const calls = [];
  let issued = false;
  const tokenId = "b".repeat(64), secret = `ama_${"a".repeat(64)}`;
  const api = { scopes:["catalog.read","configuration.write","tokens.manage"], async post(route, body = {}) {
    calls.push({route,body});
    if (route === "tokens/list") return {ok:true,tokens:issued ? [{token_id:tokenId,scopes:["catalog.read"],issued_at:10,expires_at:100}] : []};
    if (route === "tokens/issue") { issued = true; return {ok:true,bearer:secret,token_id:tokenId,scopes:["catalog.read"],expires_at:100}; }
    if (route === "tokens/revoke") { issued = false; return {ok:true}; }
    throw new Error("unexpected route");
  } };
  const content = fixture(api);
  await wait(() => content.textContent.includes("No issued tokens"));
  assert.equal(find(content, (el) => el.name === "token_scope_tokens.manage"), undefined);
  find(content, (el) => el.name === "token_scope_catalog.read").checked = true;
  assert.equal(content.guard.isDirty(), true);
  const form = find(content, (el) => el.tagName === "form");
  form.listeners.get("submit")({preventDefault(){}});
  await wait(() => content.textContent.includes("Copy your new token"));
  assert.equal(calls.find((call) => call.route === "tokens/issue").body.scopes[0], "catalog.read");
  assert.equal(find(content, (el) => el.tagName === "textarea").value, secret);
  assert.equal(content.guard.isDirty(), true);
  assert.doesNotMatch(content.textContent, /ama_aaaaaaaa/);
  await wait(() => !!find(content, (el) => el.tagName === "button" && el.textContent === "Revoke"));
  find(content, (el) => el.tagName === "button" && el.textContent === "Revoke").click();
  await wait(() => content.textContent.includes("No issued tokens"));
  assert.equal(calls.find((call) => call.route === "tokens/revoke").body.token_id, tokenId);
  find(content, (el) => el.tagName === "button" && el.textContent === "Done").click();
  assert.equal(find(content, (el) => el.tagName === "textarea"), undefined);
  assert.equal(content.guard.isDirty(), false);
});

test("an uncertain issuance is not silently retried", async () => {
  let issues = 0;
  const api = {scopes:["catalog.read"], async post(route) {
    if (route === "tokens/list") return {ok:true,tokens:[]};
    issues++; throw new Error("The request timed out. A change may have completed; refresh before retrying.");
  }};
  const content = fixture(api);
  await wait(() => content.textContent.includes("No issued tokens"));
  find(content, (el) => el.name === "token_scope_catalog.read").checked = true;
  find(content, (el) => el.tagName === "form").listeners.get("submit")({preventDefault(){}});
  await wait(() => content.textContent.includes("A change may have completed"));
  assert.equal(issues, 1);
  assert.equal(find(content, (el) => el.tagName === "textarea"), undefined);
});
