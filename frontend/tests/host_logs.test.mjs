import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tagName) { this.tagName = tagName; this.children = []; this.listeners = new Map(); this.isConnected = false; }
  setConnected(value) { this.isConnected = value; for (const child of this.children) child?.setConnected?.(value); }
  append(...children) { for (const child of children) { if (child == null) continue; this.children.push(child); child?.setConnected?.(this.isConnected); } }
  replaceChildren(...children) { for (const child of this.children) child?.setConnected?.(false); this.children = []; this.append(...children); }
  setAttribute() {}
  addEventListener(event, callback) { this.listeners.set(event, callback); }
  click() { this.listeners.get("click")?.(); }
  get textContent() { return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
}
const timers = new Map();
let nextTimer = 1;
const documentListeners = new Map();
const document = { createElement: (tag) => new Element(tag), createTextNode: (text) => text,
  visibilityState:"visible", addEventListener:(name,callback)=>documentListeners.set(name,callback),
  removeEventListener:(name,callback)=>{if(documentListeners.get(name)===callback) documentListeners.delete(name);} };
const window = {setTimeout(callback, delay) { const id = nextTimer++; timers.set(id,{callback,delay}); return id; },
  clearTimeout(id) { timers.delete(id); }};
let downloadedBlob;
const blobUrls = {createObjectURL(blob) { downloadedBlob = blob; return "blob:fixture"; }, revokeObjectURL() {}};
const sandbox = vm.createContext({ document, window, TextDecoder, Uint8Array, atob, Blob, URL:blobUrls, Error });
for (const source of ["src/ui.ts", "src/host_logs.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), sandbox);
  if (source === "src/ui.ts") vm.runInContext("Object.assign(globalThis, Amaterasu); globalThis.literalText = (value) => value; globalThis.messageOf = (error) => error.message", sandbox);
}
const { renderHostLogs } = sandbox.Amaterasu;
const descendants = (element) => [element, ...element.children.flatMap((child) => child instanceof Element ? descendants(child) : [])];
const find = (element, condition) => descendants(element).find(condition);
async function wait(predicate) {
  for (let attempt = 0; attempt < 200; attempt++) { if (predicate()) return; await new Promise((resolve) => setTimeout(resolve, 1)); }
  assert.fail("timed out waiting for host log UI");
}
const id = "0123456789abcdef0123456789abcdef";
test.beforeEach(() => { timers.clear(); documentListeners.clear(); document.visibilityState = "visible"; downloadedBlob = undefined; });

function runTimer(delay) {
  const found = [...timers].find(([, timer])=>timer.delay === delay);
  assert.ok(found, `no ${delay} ms timer scheduled`);
  timers.delete(found[0]);
  found[1].callback();
}

test("host logs read every chunk, preserve UTF-8 and search the full snapshot", async () => {
  const text = `${"ordinary line\n".repeat(400)}needle 🐈\nlast needle\n`;
  const bytes = Buffer.from(text);
  const calls = [];
  const api = { async post(route, payload = {}) {
    calls.push({ route, payload });
    if (route === "host-logs/open") return {ok:true, snapshot:id, length:bytes.length, earlier_segments_discarded:2};
    if (route === "host-logs/close") return {ok:true};
    const data = bytes.subarray(payload.offset, Math.min(payload.offset + payload.limit, bytes.length));
    const next_offset = payload.offset + data.length;
    return {ok:true, snapshot:id, offset:payload.offset, next_offset, length:bytes.length, data:data.toString("base64"), eof:next_offset === bytes.length};
  } };
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api}, target);
  await wait(() => target.textContent.includes("older log segments were discarded"));
  assert.ok(calls.filter((call) => call.route === "host-logs/read").length > 1);
  assert.ok(calls.some((call) => call.route === "host-logs/close"));
  const search = find(target, (element) => element.name === "host_log_search");
  search.value = "needle"; search.listeners.get("input")();
  assert.match(target.textContent, /Match 1 of 2/);
  assert.match(target.textContent, /needle 🐈/);
  find(target, (element) => element.tagName === "button" && element.textContent === "Next match").click();
  assert.match(target.textContent, /Match 2 of 2/);
});

test("severity filtering uses the product log level and search keeps surrounding context", async () => {
  const text = "[2026-01-01]: Info: host: startup\n[2026-01-01]: Warning: capture: warning marker\n[2026-01-01]: Error: encode: error marker\n";
  const bytes = Buffer.from(text);
  const api = { async post(route) {
    if (route === "host-logs/open") return {ok:true,snapshot:id,length:bytes.length,earlier_segments_discarded:0};
    if (route === "host-logs/close") return {ok:true};
    return {ok:true,snapshot:id,offset:0,next_offset:bytes.length,length:bytes.length,data:bytes.toString("base64"),eof:true};
  } };
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api}, target);
  await wait(() => target.textContent.includes("Snapshot loaded"));
  const level = find(target, (element) => element.name === "host_log_level");
  level.value = "Error"; level.listeners.get("change")();
  const output = find(target, (element) => element.className === "host-log-output");
  assert.match(output.textContent, /error marker/);
  assert.doesNotMatch(output.textContent, /warning marker/);
  const search = find(target, (element) => element.name === "host_log_search");
  search.value = "marker"; search.listeners.get("input")();
  assert.match(target.textContent, /Match 1 of 1/);
  assert.match(output.textContent, /warning marker/); // Search includes adjacent context.
  assert.match(output.textContent, /error marker/);
});

test("a newer refresh replaces stale results and closes both snapshots", async () => {
  const pending = [];
  const closed = [];
  const secondId = "fedcba9876543210fedcba9876543210";
  const api = { async post(route, payload = {}) {
    if (route === "host-logs/open") return new Promise((resolve) => pending.push(resolve));
    if (route === "host-logs/close") { closed.push(payload.snapshot); return {ok:true}; }
    const data = Buffer.from(payload.snapshot === id ? "stale" : "new");
    return {ok:true, snapshot:payload.snapshot, offset:0, next_offset:data.length, length:data.length, data:data.toString("base64"), eof:true};
  } };
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api}, target);
  find(target, (element) => element.tagName === "button" && element.textContent === "Refresh host log").click();
  pending[1]({ok:true,snapshot:secondId,length:3,earlier_segments_discarded:0});
  await wait(() => target.textContent.includes("new"));
  pending[0]({ok:true,snapshot:id,length:5,earlier_segments_discarded:0});
  await wait(() => closed.length === 2);
  assert.doesNotMatch(target.textContent, /stale/);
  assert.deepEqual(new Set(closed), new Set([id, secondId]));
});

test("malformed log chunks are rejected and the snapshot is closed", async () => {
  const calls = [];
  const api = { async post(route) {
    calls.push(route);
    if (route === "host-logs/open") return {ok:true,snapshot:id,length:4,earlier_segments_discarded:0};
    if (route === "host-logs/close") return {ok:true};
    return {ok:true,snapshot:id,offset:0,next_offset:4,length:4,data:"YQ==",eof:true};
  } };
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api}, target);
  await wait(() => target.textContent.includes("invalid log response"));
  assert.ok(calls.includes("host-logs/close"));
});

test("following reads only appended bytes, preserves search, and resets on rotation or a new writer", async () => {
  let bytes = Buffer.from([0x61,0x20,0xe2,0x82]);
  let stream = "a".repeat(32);
  let discarded = 0;
  let serial = 0;
  const snapshots = new Map();
  const reads = [];
  const api = {async post(route,payload={}) {
    if(route === "host-logs/open") {
      const snapshot = (++serial).toString(16).padStart(32,"0");
      snapshots.set(snapshot,Buffer.from(bytes));
      return {ok:true,snapshot,stream_id:stream,length:bytes.length,earlier_segments_discarded:discarded};
    }
    if(route === "host-logs/close") return {ok:true};
    const data = snapshots.get(payload.snapshot);
    reads.push({snapshot:payload.snapshot,offset:payload.offset});
    const chunk = data.subarray(payload.offset,Math.min(payload.offset+payload.limit,data.length));
    const next_offset = payload.offset+chunk.length;
    return {ok:true,snapshot:payload.snapshot,offset:payload.offset,next_offset,length:data.length,data:chunk.toString("base64"),eof:next_offset===data.length};
  }};
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api},target);
  await wait(()=>target.textContent.includes("Snapshot loaded") && timers.size);
  bytes = Buffer.concat([bytes,Buffer.from([0xac,0x0a])]);
  runTimer(5000);
  await wait(()=>target.textContent.includes("a €") && timers.size);
  assert.deepEqual(reads.map((read)=>read.offset),[0,4]);
  const search = find(target,(element)=>element.name === "host_log_search");
  search.value = "€"; search.listeners.get("input")();
  bytes = Buffer.concat([bytes,Buffer.from("another €\n")]);
  runTimer(5000);
  await wait(()=>target.textContent.includes("new log data available") && timers.size);
  assert.match(target.textContent,/Match 1 of 1/);
  const showLatest = find(target,(element)=>element.tagName === "button" && element.textContent === "Show latest");
  assert.equal(showLatest.hidden,false);
  showLatest.click();
  assert.match(target.textContent,/Match 1 of 2/);
  discarded = 1; bytes = Buffer.from("after rotation\n");
  find(target,(element)=>element.tagName === "button" && element.textContent === "Refresh host log").click();
  await wait(()=>reads.length === 4 && timers.size);
  assert.equal(reads.at(-1).offset,0);
  stream = "b".repeat(32); bytes = Buffer.from("replacement writer\n");
  find(target,(element)=>element.tagName === "button" && element.textContent === "Refresh host log").click();
  await wait(()=>reads.length === 5 && timers.size);
  assert.equal(reads.at(-1).offset,0);
});

test("pause and hidden tabs stop polling, and an interrupted snapshot cannot replace the view", async () => {
  let opens = 0;
  let finish;
  let closed = 0;
  const api = {async post(route) {
    if(route === "host-logs/open") {
      opens++;
      if(opens === 2) return new Promise((resolve)=>{finish=resolve;});
      return {ok:true,snapshot:id,stream_id:id,length:3,earlier_segments_discarded:0};
    }
    if(route === "host-logs/close") {closed++; return {ok:true};}
    return {ok:true,snapshot:id,offset:0,next_offset:3,length:3,data:Buffer.from("old").toString("base64"),eof:true};
  }};
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api},target);
  await wait(()=>target.textContent.includes("old") && timers.size);
  document.visibilityState = "hidden"; documentListeners.get("visibilitychange")();
  assert.equal(timers.size,0);
  document.visibilityState = "visible"; documentListeners.get("visibilitychange")();
  runTimer(0);
  await wait(()=>!!finish);
  find(target,(element)=>element.tagName === "button" && element.textContent === "Pause").click();
  assert.match(target.textContent,/Paused/);
  finish({ok:true,snapshot:id,stream_id:id,length:3,earlier_segments_discarded:0});
  await wait(()=>closed === 2);
  assert.equal(timers.size,0);
  assert.match(target.textContent,/old/);
  find(target,(element)=>element.tagName === "button" && element.textContent === "Resume").click();
  runTimer(0);
  await wait(()=>opens === 3 && timers.size);
});

test("support bundle download reads bounded archive chunks and closes its handle", async () => {
  const archive = Buffer.alloc(9000,42);
  const reads = [];
  const closed = [];
  const api = {async post(route,payload={}) {
    if(route === "host-logs/open") return {ok:true,snapshot:id,length:0,earlier_segments_discarded:0};
    if(route === "host-logs/export_start") return {ok:true,snapshot:id,status:"building"};
    if(route === "host-logs/export_status") return {ok:true,snapshot:id,status:"ready",length:archive.length,sessions:2,truncated:true};
    if(route === "host-logs/close") {closed.push(payload.snapshot); return {ok:true};}
    if(route === "host-logs/export_read") {
      reads.push(payload.offset);
      const chunk=archive.subarray(payload.offset,Math.min(payload.offset+payload.limit,archive.length));
      const next_offset=payload.offset+chunk.length;
      return {ok:true,snapshot:id,offset:payload.offset,next_offset,length:archive.length,data:chunk.toString("base64"),eof:next_offset===archive.length};
    }
    throw new Error(`unexpected route ${route}`);
  }};
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api},target);
  await wait(()=>target.textContent.includes("Snapshot loaded"));
  find(target,(element)=>element.tagName === "button" && element.textContent === "Download support bundle").click();
  await wait(()=>target.textContent.includes("2 retained sessions included"));
  assert.deepEqual(reads,[0,8192]);
  assert.match(target.textContent,/Some older data was omitted/);
  assert.equal(downloadedBlob.type,"application/zip");
  assert.deepEqual(Buffer.from(await downloadedBlob.arrayBuffer()),archive);
  assert.equal(closed.filter((value)=>value===id).length,2);
});

test("cancelling a pending support export closes its eventual handle without downloading", async () => {
  let finish;
  const closed = [];
  const api = {async post(route,payload={}) {
    if(route === "host-logs/open") return {ok:true,snapshot:id,length:0,earlier_segments_discarded:0};
    if(route === "host-logs/export_start") return new Promise((resolve)=>{finish=resolve;});
    if(route === "host-logs/close") {closed.push(payload.snapshot); return {ok:true};}
    throw new Error(`unexpected route ${route}`);
  }};
  const target = new Element("div"); target.setConnected(true);
  renderHostLogs({api},target);
  await wait(()=>target.textContent.includes("Snapshot loaded"));
  find(target,(element)=>element.tagName === "button" && element.textContent === "Download support bundle").click();
  await wait(()=>!!finish);
  find(target,(element)=>element.tagName === "button" && element.textContent === "Cancel export").click();
  finish({ok:true,snapshot:id,status:"building"});
  await wait(()=>target.textContent.includes("Support bundle cancelled"));
  assert.equal(downloadedBlob,undefined);
  assert.equal(closed.filter((value)=>value===id).length,2);
});
