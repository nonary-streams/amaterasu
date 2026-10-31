import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const sandbox = vm.createContext({Error, TextEncoder});
for (const source of ["src/session_timeline.ts", "src/pages.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source,"utf8"),{target:ts.ScriptTarget.ES2022}),sandbox);
  vm.runInContext("Object.assign(globalThis, Amaterasu)",sandbox);
}
const summary={session_id:"s1",protocol:"sunshine-modern",client_name:"Client",device_name:"Device",app_name:"Game",
  width:1920,height:1080,target_fps:60,target_bitrate_kbps:18000,codec:"hevc",started_at_unix:10,ended_at_unix:100,duration_seconds:90};
const sample = (index)=>({session_id:"s1",recorded_at_unix:100-index,frames_sent:index,bytes_sent:index*100,
  dropped_frames:0,actual_fps:60,actual_bitrate_kbps:1000,encode_latency_ms:null});
const event = (index)=>({session_id:"s1",recorded_at_unix:index,event_type:"marker",payload:{index}});
const windowFor=(first,total,kind)=>({total,cursor:total ? {session_id:"s1",kind,first_id:String(first),last_id:String(first+total-1),after_id:null,total} : null});
function makeApi(sampleCount,eventCount) {
  const data={samples:Array.from({length:sampleCount},(_,i)=>sample(i)),events:Array.from({length:eventCount},(_,i)=>event(i))};
  const starts={samples:1,events:1001}; const calls=[];
  const api={calls, async post(route,payload) {
    calls.push({route,payload});
    if(route === "history/timeline_open") return {ok:true,session:summary,samples:windowFor(1,sampleCount,"samples"),events:windowFor(1001,eventCount,"events")};
    const {kind,cursor,limit}=payload;
    const start=cursor.after_id === null ? 0 : Number(cursor.after_id)-starts[kind]+1;
    const entries=data[kind].slice(start,start+limit);
    const end=start+entries.length;
    const next_cursor=end===data[kind].length ? null : {...cursor,after_id:String(starts[kind]+end-1)};
    return {ok:true,session_id:"s1",kind,total:data[kind].length,entries,next_cursor};
  }};
  return api;
}

test("retained timeline loads all pages in admission order and preserves snapshot counts",async()=>{
  const api=makeApi(70,13); const progress=[];
  const detail=await sandbox.Amaterasu.loadRetainedTimeline(api,"s1",()=>true,(...values)=>progress.push(values));
  assert.equal(detail.samples.length,70); assert.equal(detail.events.length,13);
  assert.equal(detail.samples[0].recorded_at_unix,100); assert.equal(detail.samples[69].recorded_at_unix,31);
  assert.equal(detail.samples_truncated,false); assert.equal(detail.events_truncated,false);
  assert.equal(api.calls.filter(({route})=>route === "history/timeline_page").length,5);
  assert.deepEqual(progress.at(-1),[70,70,13,13]);
});

test("a stale cursor or partial page fails without returning a partial full history",async()=>{
  const api=makeApi(70,0); const original=api.post;
  api.post=async(route,payload)=>{
    const reply=await original(route,payload);
    if(route === "history/timeline_page" && payload.cursor.after_id) reply.next_cursor={...payload.cursor};
    return reply;
  };
  await assert.rejects(sandbox.Amaterasu.loadRetainedTimeline(api,"s1",()=>true,()=>{}),/cursor did not advance/);
});

test("a cancelled load does not accept a late page or start another",async()=>{
  let alive=true; let finish; let pages=0;
  const api=makeApi(40,0); const original=api.post;
  api.post=async(route,payload)=>{
    if(route === "history/timeline_page") { pages++; return new Promise((resolve)=>{finish=async()=>resolve(await original(route,payload));}); }
    return original(route,payload);
  };
  const pending=sandbox.Amaterasu.loadRetainedTimeline(api,"s1",()=>alive,()=>{});
  while(!finish) await new Promise((resolve)=>setTimeout(resolve,0));
  alive=false; await finish();
  assert.equal(await pending,null); assert.equal(pages,1);
});

test("a pruning error or excessive browser snapshot is surfaced without a fabricated complete result",async()=>{
  const api=makeApi(2,0);
  api.post=async(route)=>{
    if(route === "history/timeline_open") return {ok:true,session:summary,samples:windowFor(1,2,"samples"),events:windowFor(1001,0,"events")};
    throw new Error("history_changed");
  };
  await assert.rejects(sandbox.Amaterasu.loadRetainedTimeline(api,"s1",()=>true,()=>{}),/history_changed/);
  const enormous=makeApi(0,0);
  enormous.post=async()=>({ok:true,session:summary,samples:windowFor(1,25001,"samples"),events:windowFor(1001,0,"events")});
  await assert.rejects(sandbox.Amaterasu.loadRetainedTimeline(enormous,"s1",()=>true,()=>{}),/browser limit/);
});

test("snapshot cursors must belong to the requested session and record kind",async()=>{
  for (const field of ["session_id", "kind"]) {
    const api=makeApi(1,0); const original=api.post;
    api.post=async(route,payload)=>{
      const reply=await original(route,payload);
      if(route === "history/timeline_open") reply.samples.cursor[field]=field === "kind" ? "events" : "other";
      return reply;
    };
    await assert.rejects(sandbox.Amaterasu.loadRetainedTimeline(api,"s1",()=>true,()=>{}),/window is inconsistent/);
  }
});

function liveApi(count) {
  const rows = Array.from({length:count}, (_,index)=>({id:(9_007_199_254_740_993n + BigInt(index)*2n).toString(),sample:sample(index)}));
  const calls=[];
  let captured=[];
  const api={rows,calls,async post(route,payload) {
    calls.push({route,payload});
    if(route === "history/timeline_open") {
      captured=rows.slice(-payload.sample_limit);
      const first=captured[0]?.id, last=captured.at(-1)?.id;
      return {ok:true,session:summary,samples:{total:captured.length,cursor:captured.length ?
        {session_id:"s1",kind:"samples",first_id:first,last_id:last,after_id:null,total:captured.length}:null}};
    }
    const {cursor,limit}=payload;
    if(captured.some((entry)=>!rows.some((current)=>current.id===entry.id))) {
      const error=new Error("history_changed"); error.code="history_changed"; throw error;
    }
    const start=cursor.after_id === null ? 0 : captured.findIndex((entry)=>entry.id===cursor.after_id)+1;
    const page=captured.slice(start,start+limit);
    const next=start+page.length===captured.length ? null : {...cursor,after_id:page.at(-1).id};
    return {ok:true,session_id:"s1",kind:"samples",total:captured.length,
      entries:page.map((entry)=>entry.sample),entry_ids:page.map((entry)=>entry.id),next_cursor:next};
  }};
  return api;
}

test("live history reuses an exact bounded window and advances sparse 64-bit IDs",async()=>{
  const api=liveApi(100);
  const first=await sandbox.Amaterasu.loadLiveSampleSnapshot(api,"s1",50,null,()=>true);
  assert.equal(first.samples.length,50);
  assert.equal(first.samples[0].frames_sent,50);
  assert.equal(api.calls.filter(({route})=>route === "history/timeline_page").length,2);
  api.calls.length=0;
  for(let index=100;index<105;index++) api.rows.push({id:(9_007_199_254_740_993n+BigInt(index)*2n).toString(),sample:sample(index)});
  const second=await sandbox.Amaterasu.loadLiveSampleSnapshot(api,"s1",50,first,()=>true);
  assert.equal(second.samples.length,50);
  assert.equal(second.samples[0].frames_sent,55);
  assert.equal(second.samples.at(-1).frames_sent,104);
  const pages=api.calls.filter(({route})=>route === "history/timeline_page");
  assert.equal(pages.length,1);
  assert.equal(pages[0].payload.cursor.after_id,first.ids.at(-1));
  api.calls.length=0;
  const stable=await sandbox.Amaterasu.loadLiveSampleSnapshot(api,"s1",50,second,()=>true);
  assert.equal(stable.samples.length,50);
  assert.equal(api.calls.filter(({route})=>route === "history/timeline_page").length,0);
  api.rows.splice(api.rows.findIndex(({sample:row})=>row.frames_sent===70),1);
  api.calls.length=0;
  const pruned=await sandbox.Amaterasu.loadLiveSampleSnapshot(api,"s1",50,stable,()=>true);
  assert.equal(pruned.samples.length,50);
  assert.equal(pruned.samples[0].frames_sent,54);
  assert.equal(pruned.samples.some((row)=>row.frames_sent===70),false);
  assert.equal(api.calls.filter(({route})=>route === "history/timeline_page").length,2);
});

test("live history rejects invalid row identities and returns no partial snapshot after cancellation or pruning",async()=>{
  const malformed=liveApi(40); const original=malformed.post;
  malformed.post=async(route,payload)=>{
    const reply=await original(route,payload);
    if(route === "history/timeline_page") reply.entry_ids[1]=reply.entry_ids[0];
    return reply;
  };
  await assert.rejects(sandbox.Amaterasu.loadLiveSampleSnapshot(malformed,"s1",40,null,()=>true),/IDs are invalid/);
  const cancelled=liveApi(40); const originalCancelled=cancelled.post;
  let alive=true, release;
  cancelled.post=async(route,payload)=>{
    if(route === "history/timeline_page") return new Promise((resolve)=>{release=async()=>resolve(await originalCancelled(route,payload));});
    return originalCancelled(route,payload);
  };
  const pending=sandbox.Amaterasu.loadLiveSampleSnapshot(cancelled,"s1",40,null,()=>alive);
  while(!release) await new Promise((resolve)=>setTimeout(resolve,0));
  alive=false; await release();
  assert.equal(await pending,null);
  const changed=liveApi(40); const originalChanged=changed.post;
  changed.post=async(route,payload)=>{
    if(route === "history/timeline_page") changed.rows.splice(0,1);
    return originalChanged(route,payload);
  };
  await assert.rejects(sandbox.Amaterasu.loadLiveSampleSnapshot(changed,"s1",40,null,()=>true),/history_changed/);
});
