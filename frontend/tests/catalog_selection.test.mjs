import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.attributes = {}; this.dataset = {}; this.isConnected = false; this.value = ""; this.hidden = false; this.disabled = false; }
  setConnected(value) { this.isConnected = value; for (const child of this.children) child?.setConnected?.(value); }
  append(...children) { for (const child of children) { if (child == null) continue; this.children.push(child); child?.setConnected?.(this.isConnected); } }
  replaceChildren(...children) { for (const child of this.children) child?.setConnected?.(false); this.children = []; this.append(...children); }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  removeAttribute(name) { delete this.attributes[name]; }
  querySelectorAll(selector) { return selector === "button.catalog-open:not(:disabled)" ? descend(this).filter((node) => node.tagName === "button" && node.className.includes("catalog-open") && !node.disabled) : []; }
  getBoundingClientRect() { const position = Number(this.dataset.catalogId?.slice(-1) ?? 0); return {left:position * 100,top:0,width:80,height:100}; }
  focus() { this.focused = true; }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
  click() { this.listeners.get("click")?.(); }
  change() { this.listeners.get("change")?.(); }
  key(key, shiftKey = false) { const event = {key, shiftKey, isComposing:false, preventDefault(){}, stopPropagation(){}}; this.listeners.get("keydown")?.(event); }
}
const descend = (node) => [node, ...node.children.flatMap((child) => child instanceof Element ? descend(child) : [])];
const find = (root, condition) => descend(root).find(condition);
async function wait(check) { for (let i = 0; i < 100; i++) { if (check()) return; await new Promise((resolve) => setTimeout(resolve, 1)); } assert.fail("timed out waiting for catalog"); }
const row = (id, name) => ({id,entry:{name,description:"",tags:[],launch:{kind:"application",path:"/game"}}});
function fixture(api) {
  const sandbox = vm.createContext({document:{createElement:(tag)=>new Element(tag)}, window:{setTimeout,clearTimeout}, Error, TextEncoder, URL});
  for (const source of ["src/model.ts","src/ui.ts","src/catalog.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source,"utf8"),{target:ts.ScriptTarget.ES2022}), sandbox);
    vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = value => value; globalThis.localizedMessage = value => value; globalThis.messageOf = error => error.message", sandbox);
  }
  const content = new Element("main"); content.setConnected(true);
  const notices = [];
  sandbox.Amaterasu.renderCatalog({content, api, notify(value){notices.push(value);}, navigate() {}});
  return {content,notices};
}

test("library links preserve the search and sort while leaving other URL state intact", () => {
  const sandbox = vm.createContext({URL, URLSearchParams});
  vm.runInContext(ts.transpile(readFileSync("src/catalog.ts", "utf8"), {target:ts.ScriptTarget.ES2022}), sandbox);
  const {libraryLocation, updateLibraryLocation} = sandbox.Amaterasu;
  const original = new URL("https://example.test/manage?other=keep#catalog");
  const linked = updateLibraryLocation(original, "some game", "name-desc");
  assert.equal(original.search, "?other=keep");
  assert.equal(linked.searchParams.get("other"), "keep");
  assert.equal(linked.hash, "#catalog?q=some+game&sort=name-desc");
  assert.equal(libraryLocation(linked).query, "some game");
  assert.equal(libraryLocation(linked).sort, "name-desc");
  const defaults = updateLibraryLocation(linked, "", "name");
  assert.equal(defaults.search, "?other=keep");
  assert.equal(defaults.hash, "#catalog");
  defaults.hash = "catalog?sort=unexpected";
  assert.equal(libraryLocation(defaults).sort, "name");
});

test("catalog selection supports keyboard ranges, paging and revision changes", async () => {
  let revision = 1;
  const caps = {frame_limiter_provider:["auto"],rtss_frame_limit_type:[]};
  const rows = Array.from({length:14},(_,index)=>row(`app-${index+1}`,`Game ${String(index+1).padStart(2,"0")}`));
  const api = {allows:()=>true, async post(_route,payload) {
    const start = payload.cursor?.offset ?? 0;
    const end = payload.limit === 1 ? 1 : Math.min(start + 7, rows.length);
    return {ok:true,revision,config_override_capabilities:caps,next_cursor:end < rows.length ? {offset:end} : null,
      entries:rows.slice(start,end)};
  }};
  const {content,notices} = fixture(api);
  await wait(() => !!find(content,(node)=>node.attributes["aria-label"] === "Edit Game 01"));
  const first = find(content,(node)=>node.attributes["aria-label"] === "Edit Game 01");
  first.key(" ");
  assert.match(content.textContent,/1 selected/);
  assert.equal(find(content,(node)=>node.attributes["aria-label"] === "Deselect Game 01").attributes["aria-pressed"],"true");
  first.key("ArrowRight",true);
  assert.match(content.textContent,/2 selected/);
  assert.equal(find(content,(node)=>node.attributes["aria-label"] === "Edit Game 02").focused,true);
  find(content,(node)=>node.tagName === "button" && node.textContent === "Next →").click();
  await wait(() => !!find(content,(node)=>node.attributes["aria-label"] === "Select Game 13"));
  assert.match(content.textContent,/2 selected/);
  find(content,(node)=>node.attributes["aria-label"] === "Select Game 13").click();
  assert.match(content.textContent,/3 selected/);
  revision = 2;
  find(content,(node)=>node.tagName === "button" && node.textContent === "← Previous").click();
  await wait(() => notices.length === 1);
  assert.match(notices[0],/catalog changed/i);
  assert.equal(find(content,(node)=>node.className === "library-selection").hidden,true);
});

test("library sorts the entire revision across catalog pages and fetches visible uncached entries", async () => {
  const caps = {frame_limiter_provider:["auto"],rtss_frame_limit_type:[]};
  const rows = Array.from({length:40},(_,index)=>row(`app-${String(index+1).padStart(2,"0")}`,`Game ${String(40-index).padStart(2,"0")}`));
  rows[39].entry.steam_id = "7";
  const requests = [];
  const api = {allows:()=>true, async post(route,payload) {
    requests.push({route,payload});
    if (route === "catalog/get") return {ok:true,revision:5,...rows.find((entry)=>entry.id === payload.id)};
    const start = payload.cursor?.offset ?? 0;
    const end = Math.min(start + payload.limit, rows.length);
    return {ok:true,revision:5,config_override_capabilities:caps,next_cursor:end < rows.length ? {offset:end} : null,entries:rows.slice(start,end)};
  }};
  const {content} = fixture(api);
  await wait(()=>!!find(content,(node)=>node.attributes["aria-label"] === "Edit Game 01"));
  const names = ()=>descend(content).filter((node)=>node.attributes["aria-label"]?.startsWith("Edit Game ")).map((node)=>node.attributes["aria-label"]);
  assert.deepEqual(names().slice(0,3),["Edit Game 01","Edit Game 02","Edit Game 03"]);
  assert.equal(names().length,12);
  assert.ok(requests.some(({route})=>route === "catalog/get"));
  const sort = find(content,(node)=>node.name === "sort");
  sort.value = "name-desc"; sort.change();
  await wait(()=>names()[0] === "Edit Game 40");
  assert.deepEqual(names().slice(0,3),["Edit Game 40","Edit Game 39","Edit Game 38"]);
  sort.value = "source"; sort.change();
  await wait(()=>names()[0] === "Edit Game 02");
  assert.equal(names()[0],"Edit Game 02");
});

test("a new sort cancels an older library scan before it can replace the results", async () => {
  const caps = {frame_limiter_provider:["auto"],rtss_frame_limit_type:[]};
  let finishFirst;
  let calls = 0;
  const delayed = new Promise((resolve)=>{ finishFirst = resolve; });
  const api = {allows:()=>true, async post(_route,payload) {
    if (payload.limit === 32 && calls++ === 0) return delayed;
    return {ok:true,revision:2,config_override_capabilities:caps,next_cursor:null,entries:[row("app-2","Current")]};
  }};
  const {content} = fixture(api);
  await wait(()=>!!finishFirst);
  const sort = find(content,(node)=>node.name === "sort");
  sort.value = "name-desc"; sort.change();
  await wait(()=>!!find(content,(node)=>node.attributes["aria-label"] === "Edit Current"));
  finishFirst({ok:true,revision:1,config_override_capabilities:caps,next_cursor:null,entries:[row("app-1","Stale")]});
  await new Promise((resolve)=>setTimeout(resolve,5));
  assert.ok(find(content,(node)=>node.attributes["aria-label"] === "Edit Current"));
  assert.equal(find(content,(node)=>node.attributes["aria-label"] === "Edit Stale"),undefined);
});

test("a repeating catalog continuation fails visibly instead of looping", async () => {
  const api = {allows:()=>true, async post(){return {ok:true,revision:1,
    config_override_capabilities:{frame_limiter_provider:["auto"],rtss_frame_limit_type:[]},
    next_cursor:{offset:1},entries:[]};}};
  const {content} = fixture(api);
  await wait(()=>content.textContent.includes("repeated continuation"));
  assert.ok(find(content,(node)=>node.tagName === "button" && node.textContent === "Try again"));
});

test("selection remains available to catalog readers without edit permission", async () => {
  const api = {allows:(scope)=>scope !== "catalog.write", async post(){return {ok:true,revision:1,next_cursor:null,
    config_override_capabilities:{frame_limiter_provider:["auto"],rtss_frame_limit_type:[]},entries:[row("app-1","One")]};}};
  const {content} = fixture(api);
  await wait(()=>!!find(content,(node)=>node.attributes["aria-label"] === "Select One"));
  assert.equal(find(content,(node)=>node.attributes["aria-label"] === "Edit One").disabled,true);
  find(content,(node)=>node.attributes["aria-label"] === "Select One").click();
  assert.match(content.textContent,/1 selected/);
  find(content,(node)=>node.tagName === "button" && node.textContent === "Clear selection").click();
  assert.equal(find(content,(node)=>node.className === "library-selection").hidden,true);
});
