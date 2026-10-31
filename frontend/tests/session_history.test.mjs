import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.listeners = new Map(); this.attributes = {}; }
  append(...children) { this.children.push(...children.filter((child)=>child != null)); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
  addEventListener(event, callback) { this.listeners.set(event, callback); }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  click() { this.listeners.get("click")?.(); }
  get textContent() { return this.children.map((child)=>typeof child === "string" ? child : child?.textContent ?? "").join(""); }
  set textContent(value) { this.replaceChildren(String(value)); }
}
const descend = (node) => [node, ...node.children.flatMap((child)=>child instanceof Element ? descend(child) : [])];
const find = (node, predicate) => descend(node).find(predicate);
let downloaded;
let anchor;
const sandbox = vm.createContext({
  document:{createElement(tag) { const element = new Element(tag); if (tag === "a") anchor = element; return element; },
    createElementNS(_namespace,tag) {return new Element(tag);},createTextNode(value){return value;}},
  window:{setTimeout(callback){callback();}},
  URL:{createObjectURL(blob){downloaded=blob; return "blob:fixture";},revokeObjectURL(){}},
  Blob, Error, TextEncoder,
});
for (const source of ["src/ui.ts", "src/session_history.ts", "src/pages.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source,"utf8"),{target:ts.ScriptTarget.ES2022}),sandbox);
  vm.runInContext("Object.assign(globalThis, Amaterasu)",sandbox);
}
function detail() {
  return {session:{session_id:"s1",app_name:"Example",client_name:"Client",device_name:"Device",protocol:"sunshine-modern",
    width:1920,height:1080,target_fps:60,target_bitrate_kbps:18000,codec:"hevc",started_at_unix:90,ended_at_unix:450,duration_seconds:360},
    sample_count:5,event_count:2,samples_truncated:true,events_truncated:true,
    samples:[{session_id:"s1",recorded_at_unix:100,frames_sent:10,bytes_sent:100,dropped_frames:0,actual_fps:30,actual_bitrate_kbps:1000,encode_latency_ms:null},
      {session_id:"s1",recorded_at_unix:160,frames_sent:20,bytes_sent:200,dropped_frames:1,actual_fps:45,actual_bitrate_kbps:2000,encode_latency_ms:0},
      {session_id:"s1",recorded_at_unix:400,frames_sent:40,bytes_sent:400,dropped_frames:3,actual_fps:60,actual_bitrate_kbps:3000,encode_latency_ms:4.5}],
    events:[{session_id:"s1",recorded_at_unix:401,event_type:"stream_stopped",payload:{reason:"client"}}]};
}

test("history charts preserve measured gaps, scale by time and export the complete returned subset", async () => {
  const source = detail();
  let closed = false;
  const view = sandbox.Amaterasu.renderRecordedSessionDetail(source,()=>{closed=true;});
  assert.match(view.textContent,/3 of 5 samples/);
  assert.match(view.textContent,/Only the latest returned samples and events/);
  assert.match(view.textContent,/client delivery is not confirmed/i);
  assert.match(view.textContent,/time-proportional spacing/);
  const charts = descend(view).filter((element)=>element.tagName === "svg");
  assert.equal(charts.length,4);
  const video = charts.find((chart)=>chart.attributes["aria-label"].startsWith("Accepted video rate"));
  const videoLine = find(video,(element)=>element.tagName === "polyline");
  assert.match(videoLine.attributes.points,/^0,.* 64,.* 320,/);
  const latency = charts.find((chart)=>chart.attributes["aria-label"].startsWith("Encoder latency"));
  assert.match(latency.attributes["aria-label"],/2 recorded values, latest 4.5 ms/);
  find(view,(element)=>element.tagName === "button" && element.textContent === "3 min").click();
  assert.match(view.textContent,/1 of 3 returned samples/);
  assert.equal(find(view,(element)=>element.tagName === "button" && element.textContent === "3 min").attributes["aria-pressed"],"true");
  find(view,(element)=>element.tagName === "button" && element.textContent === "Download returned history (JSON)").click();
  assert.equal(anchor.download,"amaterasu-session-history.json");
  const exported = JSON.parse(await downloaded.text());
  assert.equal(exported.detail.samples.length,3);
  assert.equal(exported.detail.samples_truncated,true);
  assert.equal(exported.detail.events[0].payload.reason,"client");
  find(view,(element)=>element.tagName === "button" && element.textContent === "Close details").click();
  assert.equal(closed,true);
});

test("history charts do not infer missing latency or reorder repeated and backward timestamps", () => {
  const source = detail();
  source.samples[1].recorded_at_unix = 100;
  source.samples[2].recorded_at_unix = 99;
  source.samples.forEach((sample)=>{sample.encode_latency_ms=null;});
  const view = sandbox.Amaterasu.renderRecordedSessionDetail(source,()=>{});
  assert.match(view.textContent,/evenly spaced in recorded order/);
  const latency = descend(view).find((element)=>element.tagName === "svg" && element.attributes["aria-label"].startsWith("Encoder latency"));
  assert.match(latency.attributes["aria-label"],/0 recorded values, latest Unavailable/);
  assert.equal(descend(latency).filter((element)=>element.tagName === "polyline" || element.tagName === "circle").length,0);
});

test("live cards preserve the selected range and clear charts when a refresh fails", () => {
  const source = detail();
  source.session.ended_at_unix = null;
  const live = {...source,samples:[],sample_count:0,events:[],event_count:0,samples_truncated:false,events_truncated:false};
  const card = sandbox.Amaterasu.createLiveSessionCard();
  card.update(source.session, live, source, "", false, 1000, 300);
  const choice = find(card.element,(element)=>element.tagName === "button" && element.textContent === "3 min");
  choice.click();
  assert.match(card.element.textContent,/1 of 3 samples shown/);
  assert.match(card.element.textContent,/client delivery is not confirmed/);
  const newer = structuredClone(source);
  newer.samples[2].actual_fps = 55;
  card.update(newer.session, live, newer, "", false, 1000, 300);
  assert.equal(find(card.element,(element)=>element.tagName === "button" && element.textContent === "3 min"),choice);
  assert.equal(choice.attributes["aria-pressed"],"true");
  assert.match(card.element.textContent,/55\.0 fps/);
  card.update(newer.session, live, null, "Recorded history is unavailable.", false, 1000, 300);
  assert.match(card.element.textContent,/Recorded history is unavailable/);
  assert.equal(descend(card.element).filter((element)=>element.tagName === "svg").length,0);
  card.update(newer.session, live, newer, "", false, 1000, 300);
  assert.equal(choice.attributes["aria-pressed"],"true");
  assert.equal(descend(card.element).filter((element)=>element.tagName === "svg").length,4);
});

test("history detail rejects malformed metadata, unsafe counters and latency", () => {
  const valid = detail();
  assert.ok(sandbox.Amaterasu.parseHistoryDetailReply({ok:true,detail:valid},"s1"));
  for (const mutation of [
    (value)=>{value.session.width="bad";},
    (value)=>{value.samples[0].bytes_sent=Number.MAX_SAFE_INTEGER+1;},
    (value)=>{value.samples[0].encode_latency_ms=-1;},
    (value)=>{value.samples[0].encode_latency_ms=Number.NaN;},
    (value)=>{value.samples_truncated=false;},
    (value)=>{value.samples=Array.from({length:33},()=>value.samples[0]); value.sample_count=33; value.samples_truncated=false;},
  ]) {
    const invalid=structuredClone(valid); mutation(invalid);
    assert.throws(()=>sandbox.Amaterasu.parseHistoryDetailReply({ok:true,detail:invalid},"s1"));
  }
});

test("nearby sessions group only when names match and the non-overlapping gap is at most two minutes", () => {
  const base = {session_id:"new",protocol:"rtsp",client_name:"Client",device_name:"Device",app_name:"Game",
    width:1920,height:1080,target_fps:60,target_bitrate_kbps:18000,codec:"hevc",started_at_unix:300,ended_at_unix:400,duration_seconds:100};
  const older = {...base,session_id:"old",started_at_unix:100,ended_at_unix:180,duration_seconds:80};
  assert.equal(sandbox.Amaterasu.groupRecordedSessions([base,older])[0].length,2);
  for (const changed of [
    {...older,ended_at_unix:179}, {...older,ended_at_unix:301}, {...older,client_name:"Other"},
    {...older,device_name:"Other"}, {...older,app_name:"Other"}, {...older,protocol:"Other"},
    {...older,client_name:"",device_name:""}, {...older,ended_at_unix:null},
    {...older,started_at_unix:200,ended_at_unix:180},
  ]) assert.equal(sandbox.Amaterasu.groupRecordedSessions([base,changed]).length,2);
  assert.equal(sandbox.Amaterasu.groupRecordedSessions([{...base,app_name:""},{...older,app_name:""}])[0].length,2);
  assert.equal(sandbox.Amaterasu.groupRecordedSessions([{...base,client_name:" ",device_name:""},{...older,client_name:" ",device_name:""}]).length,2);
});

test("grouped history preserves member identities, breaks chart lines and exports separate snapshots", async () => {
  const old = detail();
  old.session = {...old.session,session_id:"old",started_at_unix:100,ended_at_unix:200,duration_seconds:100};
  old.sample_count=2; old.event_count=1; old.samples_truncated=false; old.events_truncated=false;
  old.samples=old.samples.slice(0,2).map((sample,index)=>({...sample,session_id:"old",recorded_at_unix:100+index*50}));
  old.events=old.events.map((event)=>({...event,session_id:"old",recorded_at_unix:190}));
  const recent = structuredClone(old);
  recent.session={...recent.session,session_id:"new",started_at_unix:250,ended_at_unix:350,duration_seconds:100,codec:"av1"};
  recent.samples=recent.samples.map((sample,index)=>({...sample,session_id:"new",recorded_at_unix:250+index*50}));
  recent.events=recent.events.map((event)=>({...event,session_id:"new",recorded_at_unix:340}));
  const merged=sandbox.Amaterasu.mergeRecordedSessionDetails([recent,old]);
  assert.equal(merged.sample_count,4); assert.equal(merged.session.duration_seconds,200);
  assert.equal(merged.session.codec,"");
  assert.deepEqual(Array.from(merged.samples,(sample)=>sample.session_id),["old","old","new","new"]);
  const view=sandbox.Amaterasu.renderRecordedSessionDetail(merged,()=>{},true,[recent,old]);
  assert.match(view.textContent,/2 nearby streams/);
  assert.match(view.textContent,/reconnect is not confirmed/);
  const video=descend(view).find((element)=>element.tagName === "svg" && element.attributes["aria-label"].startsWith("Accepted video rate"));
  assert.equal(descend(video).filter((element)=>element.tagName === "polyline").length,2);
  assert.match(view.textContent,/Stream old/); assert.match(view.textContent,/Stream new/);
  find(view,(element)=>element.tagName === "button" && element.textContent === "Download retained history (JSON)").click();
  assert.equal(anchor.download,"amaterasu-session-history-group.json");
  const exported=JSON.parse(await downloaded.text());
  assert.equal(exported.format,"amaterasu-session-history-group/1");
  assert.deepEqual(exported.members.map((member)=>member.session.session_id),["new","old"]);
  assert.equal(exported.members[0].samples[0].session_id,"new");
});

test("live charts join only non-overlapping host-time intervals and keep current rates visible",()=>{
  const recorded=detail();recorded.session.ended_at_unix=null;
  recorded.samples=recorded.samples.slice(0,2).map((sample,index)=>({...sample,recorded_at_unix:100+index}));
  const live={...recorded,sample_count:3,samples_truncated:false,events:[],event_count:0,events_truncated:false,
    samples:[{start:100500,end:101.5},{start:101500,end:102.5},{start:102500,end:103.5}].map(({start,end},index)=>({
      ...recorded.samples[0],recorded_at_unix:end,actual_fps:70+index,interval_start_unix_ms:start,chart_source:"live"}))};
  const combined=sandbox.Amaterasu.combineActiveSamples(recorded,live,1000,20);
  assert.equal(combined.recordedCount,2);assert.equal(combined.liveCount,1);
  assert.deepEqual(Array.from(combined.samples,(sample)=>sample.recorded_at_unix),[100,101,103.5]);
  const card=sandbox.Amaterasu.createLiveSessionCard();
  card.update(recorded.session,live,recorded,"",false,1000,20);
  assert.match(card.element.textContent,/Latest live rate: 72\.0 fps/);
  assert.match(card.element.textContent,/2 recorded · 1 live/);
  assert.match(card.element.textContent,/Recorded: solid \/ filled · Live: dashed \/ hollow/);
  const video=descend(card.element).find((element)=>element.tagName === "svg" && element.attributes["aria-label"].startsWith("Accepted video rate"));
  assert.equal(descend(video).filter((element)=>element.tagName === "polyline").length,1);
  assert.equal(descend(video).filter((element)=>element.tagName === "circle" && element.attributes.class.includes("host-metric-chart-live")).length,1);
  const liveOnly=find(card.element,(element)=>element.tagName === "button" && element.textContent === "Live only");
  liveOnly.click();
  assert.match(card.element.textContent,/3 of 3 samples shown/);
  assert.equal(liveOnly.attributes["aria-pressed"],"true");
  card.update(recorded.session,live,null,"History recording is disabled.",false,1000,20);
  assert.equal(liveOnly.attributes["aria-pressed"],"true");
  assert.match(card.element.textContent,/History recording is disabled/);
  const recordedOnly=find(card.element,(element)=>element.tagName === "button" && element.textContent === "Recorded only");
  recordedOnly.click();
  assert.match(card.element.textContent,/History recording is disabled/);
  assert.equal(descend(card.element).filter((element)=>element.tagName === "svg").length,0);
  card.update(recorded.session,live,recorded,"",false,1000,20);
  assert.equal(recordedOnly.attributes["aria-pressed"],"true");
  assert.equal(descend(card.element).filter((element)=>element.tagName === "svg").length,4);
  const backwards=structuredClone(recorded);backwards.samples[1].recorded_at_unix=99;
  const unsafe=sandbox.Amaterasu.combineActiveSamples(backwards,live,1000,20);
  assert.equal(unsafe.liveCount,0);
  assert.match(unsafe.note,/cannot be safely joined/);
});
