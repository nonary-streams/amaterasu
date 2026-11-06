import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.isConnected = true; }
  append(...children) { this.children.push(...children.filter((child) => child != null)); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute(name, value) { (this.attributes ??= {})[name] = String(value); }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
  click() { this.listeners.get("click")?.(); }
}
const descend = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? descend(child) : [])];
const find = (root, check) => descend(root).find(check);
async function wait(check) {
  for (let i = 0; i < 100; i++) { if (check()) return; await new Promise((resolve) => setTimeout(resolve, 1)); }
  assert.fail("timed out waiting for session UI");
}
function fixture(api) {
  const sandbox = vm.createContext({document:{createElement:(tag)=>new Element(tag)}, window:{confirm:()=>true}, Error});
  for (const source of ["src/ui.ts", "src/maintenance.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {target:ts.ScriptTarget.ES2022}), sandbox);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; globalThis.messageOf = error => error.message", sandbox);
  }
  const content = new Element("main");
  const notices = [];
  sandbox.Amaterasu.renderMaintenance({content, api, notify(value) { notices.push(value); }, navigate() {} });
  return {content, notices};
}
const current = {session_id:"a".repeat(64),subject:"admin",expires_at:200,refresh_expires_at:300,remember_me:true,current:true};
const peer = {session_id:"b".repeat(64),subject:"viewer",expires_at:200,refresh_expires_at:300,remember_me:true,current:false};

test("maintenance protects the current session and confirms peer revocation by readback", async () => {
  let sessions = [current, peer];
  const calls = [];
  const api = {allows:()=>true, async post(route, payload = {}) {
    calls.push({route,payload});
    if (route === "account-sessions/list") return {ok:true,sessions};
    sessions = [current]; return {ok:true,revoked:true};
  }};
  const {content,notices} = fixture(api);
  await wait(() => content.textContent.includes("This session") && content.textContent.includes("viewer"));
  const cards = descend(content).filter((el) => el.className === "browser-session-card");
  assert.equal(cards.length, 2);
  const peerRevoke = find(cards[1], (el) => el.tagName === "button" && el.textContent === "Revoke session");
  assert.match(peerRevoke.attributes["aria-label"], /viewer, refresh expires/);
  assert.equal(find(cards[0], (el) => el.tagName === "button" && el.textContent === "Revoke session"), undefined);
  find(cards[1], (el) => el.tagName === "button" && el.textContent === "Revoke session").click();
  await wait(() => notices.includes("Session revoked."));
  assert.equal(calls.filter((call) => call.route === "account-sessions/revoke").length, 1);
  assert.equal(calls.find((call) => call.route === "account-sessions/revoke").payload.session_id, peer.session_id);
  assert.equal(descend(content).filter((el) => el.className === "browser-session-card").length, 1);
});

test("an interrupted revoke is reconciled without claiming its cause or retrying", async () => {
  let sessions = [current, peer], attempts = 0;
  const api = {allows:()=>true, async post(route) {
    if (route === "account-sessions/list") return {ok:true,sessions};
    attempts++; sessions = [current]; throw new Error("request interrupted");
  }};
  const {content,notices} = fixture(api);
  await wait(() => content.textContent.includes("viewer"));
  find(content, (el) => el.tagName === "button" && el.textContent === "Revoke session").click();
  await wait(() => content.textContent.includes("cause could not be confirmed"));
  assert.equal(attempts, 1);
  assert.deepEqual(notices, []);
});

test("malformed session lists do not expose revoke controls", async () => {
  const api = {allows:()=>true, async post() { return {ok:true,sessions:[current,{...peer,current:true}]}; }};
  const {content} = fixture(api);
  await wait(() => content.textContent.includes("current session could not be identified"));
  assert.equal(find(content, (el) => el.tagName === "button" && el.textContent === "Revoke session"), undefined);
});
