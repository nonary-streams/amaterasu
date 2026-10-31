import assert from "node:assert/strict";
import {readFileSync} from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const sandbox=vm.createContext({Error,TextEncoder});
vm.runInContext(ts.transpile(readFileSync("src/live_streams.ts","utf8"),{target:ts.ScriptTarget.ES2022}),sandbox);
const stream=(overrides={})=>({session_id:"stream-1",started_at_unix:100,device_name:"Living room",width:1920,height:1080,
  target_fps:60,target_bitrate_kbps:10000,codec:"hevc",frames_sent:"100",bytes_sent:"1000",sequence_gaps:"1",...overrides});
const snapshot=(time,rows=[stream()],epoch="a".repeat(32),wall=99000+time)=>sandbox.Amaterasu.parseLiveStreamSnapshot({ok:true,epoch,sampled_at_ms:String(time),sampled_at_unix_ms:String(wall),sessions:rows});

test("live status validates identity, bounded records and full precision counters",()=>{
  const parsed=snapshot(1000,[stream({frames_sent:"9007199254740993"})]);
  assert.equal(parsed.streams[0].counters.frames,9007199254740993n);
  for(const bad of [
    {ok:true,sampled_at_ms:"01",sessions:[]},
    {ok:true,sampled_at_ms:"-1",sessions:[]},
    {ok:true,sampled_at_ms:"18446744073709551616",sessions:[]},
    {ok:true,sampled_at_ms:"1",sessions:[stream(),stream()]},
    {ok:true,sampled_at_ms:"1",sessions:[stream(),stream({session_id:"2"}),stream({session_id:"3"})]},
    {ok:true,sampled_at_ms:"1",sessions:[stream({frames_sent:"1.5"})]},
    {ok:true,sampled_at_ms:"1",sessions:[stream({device_name:"bad\nname"})]},
    {ok:true,sampled_at_ms:"1",sessions:[stream({codec:"unknown"})]},
  ]) assert.throws(()=>sandbox.Amaterasu.parseLiveStreamSnapshot({epoch:"a".repeat(32),sampled_at_unix_ms:"100000",...bad}));
  assert.throws(()=>snapshot(1,[],"invalid"));
  assert.throws(()=>snapshot(1,[],"a".repeat(32),"-0"));
  assert.throws(()=>snapshot(1,[],"a".repeat(32),"8640000000000001"));
});

test("host clock jumps reset the rate baseline instead of joining incompatible intervals",()=>{
  const tracker=new sandbox.Amaterasu.LiveStreamTracker();
  const read=(time,wall,frames)=>tracker.update(snapshot(time,[stream({frames_sent:String(frames)})],"a".repeat(32),wall),30,30).get("stream-1");
  assert.equal(read(1000,100000,100).detail.samples.length,0);
  const stable=read(3000,102000,220);
  assert.equal(stable.detail.samples.length,1);
  assert.equal(stable.detail.samples[0].interval_start_unix_ms,100000);
  assert.equal(stable.detail.samples[0].recorded_at_unix,102);
  assert.equal(read(4000,90000,280).detail.samples.length,0);
  assert.equal(read(5000,200000,340).detail.samples.length,0);
});

test("live rates need two monotonic snapshots and reset when identity or counters change",()=>{
  const tracker=new sandbox.Amaterasu.LiveStreamTracker();
  const read=(value)=>tracker.update(value,30,3).get("stream-1");
  assert.equal(read(snapshot(1000)).detail.samples.length,0);
  let output=read(snapshot(3000,[stream({frames_sent:"220",bytes_sent:"21000",sequence_gaps:"3"})]));
  assert.equal(output.detail.samples.length,1);
  assert.equal(output.detail.samples[0].actual_fps,60);
  assert.equal(output.detail.samples[0].actual_bitrate_kbps,80);
  assert.equal(output.detail.samples[0].dropped_frames,3);
  assert.equal(output.detail.samples[0].encode_latency_ms,null);
  output=read(snapshot(3000,[stream({frames_sent:"230",bytes_sent:"22000",sequence_gaps:"3"})]));
  assert.equal(output.detail.samples.length,1);
  output=read(snapshot(4000,[stream({frames_sent:"1",bytes_sent:"2",sequence_gaps:"0"})]));
  assert.equal(output.detail.samples.length,0);
  output=read(snapshot(5000,[stream({started_at_unix:101,frames_sent:"10",bytes_sent:"200",sequence_gaps:"0"})]));
  assert.equal(output.detail.samples.length,0);
  output=read(snapshot(100,[stream({started_at_unix:101,frames_sent:"20",bytes_sent:"300",sequence_gaps:"0"})]));
  assert.equal(output.detail.samples.length,0);
  tracker.update(snapshot(200,[]),30,3);
  assert.equal(read(snapshot(300,[stream({started_at_unix:101,frames_sent:"30",bytes_sent:"400",sequence_gaps:"0"})])).detail.samples.length,0);
  assert.equal(read(snapshot(400,[stream({started_at_unix:101,frames_sent:"40",bytes_sent:"500",sequence_gaps:"0"})],"b".repeat(32))).detail.samples.length,0);
});

test("live samples obey time and count limits and flag precision rounding",()=>{
  const tracker=new sandbox.Amaterasu.LiveStreamTracker();
  for(let index=0;index<7;index++) {
    const count=(9007199254740993n+BigInt(index)).toString();
    const output=tracker.update(snapshot(1000+index*1000,[stream({frames_sent:count,bytes_sent:String(1000+index),sequence_gaps:"0"})]),30,3).get("stream-1");
    assert.equal(output.rounded,true);
    assert.ok(output.detail.samples.length<=3);
  }
  const expired=tracker.update(snapshot(50000,[stream({frames_sent:"9007199254741000",bytes_sent:"1100",sequence_gaps:"0"})]),30,3).get("stream-1");
  assert.equal(expired.detail.samples.length,0);
});
