import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag, text = "") { this.tagName=tag; this.children=[]; this.text=text; this.hidden=false; this.disabled=false; this.isConnected=true; }
  append(...children) { this.children.push(...children); }
  set textContent(value) { this.text=String(value); this.children=[]; }
  get textContent() { return this.text + this.children.map(child=>typeof child === "string" ? child : child.textContent).join(""); }
  click() { if (!this.disabled && !this.hidden) this.action?.(); }
}
const descendants = (node) => [node,...node.children.flatMap(child=>child instanceof Element ? descendants(child) : [])];
const find = (node, text) => descendants(node).find(child=>child.tagName === "button" && child.textContent === text);
const tick = () => new Promise(setImmediate);
const launch = (state="ready", revision="b".repeat(64)) => ({ok:true,launch:{reference:"a".repeat(64),revision,state,pending_resume:false}});
function fixture(api) {
  api.allowsLaunchAction ??= () => api.allows("application.terminate");
  const timers = new Map(); let next=0; let approval=()=>Promise.resolve(true);
  const context=vm.createContext({Error,document:{body:{},visibilityState:"visible",addEventListener(){},removeEventListener(){}},
    window:{setTimeout(callback){timers.set(++next,callback);return next;},clearTimeout(id){timers.delete(id);}},
    MutationObserver:class {observe(){} disconnect(){}},
    el:(tag,_class,...children)=>{const node=new Element(tag);node.append(...children);return node;},
    panel:(title,...children)=>{const node=new Element("section",title);node.append(...children);return node;},
    button:(text,action)=>{const node=new Element("button",text);node.action=action;return node;},
    confirmAction:()=>approval(),
  });
  vm.runInContext(ts.transpile(readFileSync("src/stream_controls.ts","utf8"),{target:ts.ScriptTarget.ES2022}),context);
  const container=new Element("main");
  context.Amaterasu.renderManagedStreamControls({api},container);
  return {container,timers,setApproval(fn){approval=fn;},async poll(){const [id,fn]=[...timers][0];timers.delete(id);fn();await tick();}};
}

test("managed stream controls require read and write scopes", async()=>{
  let calls=0; const api={allows:scope=>scope === "session.read",async post(){calls++;}};
  const {container}=fixture(api); await tick(); assert.equal(calls,0); assert.equal(container.children.length,0);
});

test("a confirmed stop uses the observed revision and waits for cleanup readback", async()=>{
  let state="ready"; const calls=[];
  const api={allows:()=>true,async post(route,payload={}){calls.push([route,payload]);
    if(route === "nvhttp/stop_launch") {state="stopping";return {ok:true,accepted:true,reference:payload.reference};}
    return launch(state);
  }};
  const f=fixture(api); await tick();
  find(f.container,"Stop managed stream").click(); await tick();
  assert.equal(calls[1][0],"nvhttp/stop_launch");
  assert.equal(calls[1][1].revision,"b".repeat(64));
  assert.match(f.container.textContent,/cleanup is still in progress/);
  assert.doesNotMatch(f.container.textContent,/confirmed stream and application cleanup/);
  state="ended"; await f.poll();
  assert.match(f.container.textContent,/host confirmed stream and application cleanup/);
});

test("a changed launch is not silently retried after a confirmation", async()=>{
  let approve; let revision="b".repeat(64); const writes=[];
  const api={allows:()=>true,async post(route,payload={}){
    if(route === "nvhttp/stop_launch") {writes.push(payload);const error=new Error("changed");error.code="launch_changed";throw error;}
    return launch("ready",revision);
  }};
  const f=fixture(api); f.setApproval(()=>new Promise(resolve=>approve=resolve)); await tick();
  find(f.container,"Stop managed stream").click(); revision="c".repeat(64); await f.poll(); approve(true); await tick();
  assert.equal(writes.length,1); assert.equal(writes[0].revision,"b".repeat(64));
  assert.match(f.container.textContent,/launch changed while confirmation was open/);
});

test("an uncertain stop is read back by reference before reporting a result", async()=>{
  let state="ready"; const calls=[];
  const api={allows:()=>true,async post(route,payload={}){calls.push([route,payload]);
    if(route === "nvhttp/stop_launch") {state="stopping";throw new Error("connection lost");}
    return launch(state);
  }};
  const f=fixture(api); await tick(); find(f.container,"Stop managed stream").click(); await tick();
  assert.equal(calls[2][1].reference,"a".repeat(64));
  assert.match(f.container.textContent,/outcome is unconfirmed/);
  state="cleanup_failed"; await f.poll();
  assert.match(f.container.textContent,/Cleanup is not confirmed/);
  assert.ok(find(f.container,"Retry stream cleanup"));
});
