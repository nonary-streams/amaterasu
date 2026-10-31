import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.listeners = new Map();
    this.isConnected = true;
    this.classList = { add: (name) => { this.className = name; } };
    this.attributes = {};
    this.parentElement = null;
  }

  append(...children) {
    for (const child of children.filter((item) => item != null)) {
      if (child instanceof FakeElement) { child.remove(); child.parentElement = this; }
      this.children.push(child);
    }
  }

  replaceChildren(...children) {
    for (const child of this.children) if (child instanceof FakeElement) child.parentElement = null;
    this.children = [];
    this.append(...children);
  }

  insertBefore(child, before) {
    child.remove(); child.parentElement = this;
    const index = before === null ? -1 : this.children.indexOf(before);
    if (index < 0) this.children.push(child); else this.children.splice(index, 0, child);
  }

  remove() {
    if (!this.parentElement) return;
    const children = this.parentElement.children;
    children.splice(children.indexOf(this), 1);
    this.parentElement = null;
  }

  get firstElementChild() { return this.children.find((child) => child instanceof FakeElement) ?? null; }

  addEventListener(event, listener) {
    this.listeners.set(event, listener);
  }

  setAttribute(name, value) { this.attributes[name] = value; }

  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join("");
  }

  set textContent(value) {
    this.replaceChildren(String(value));
  }
}

function descendants(element) {
  return [element, ...element.children.flatMap((child) => child instanceof FakeElement ? descendants(child) : [])];
}

const settings = {
  enabled: true,
  poll_interval_ms: 2000,
  history_retention_seconds: 300,
  max_history_points: 300,
  pause_when_hidden: true,
  show_active_sessions: true,
  show_host_stats: true,
  show_host_charts: true,
  show_session_history: true,
};

test("host metrics settings editor reads, bounds and confirms revisioned preferences", async () => {
  const calls = [];
  let stored = { ok: true, settings, revision: 4 };
  const context = vm.createContext({
    document: { createElement: (tag) => new FakeElement(tag) },
    Error,
    console,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    if (source === "src/model.ts" || source === "src/ui.ts")
      vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }

  const container = new FakeElement("div");
  const page = {
    api: {
      allows: (scope) => scope === "configuration.write",
      async post(route, body = {}) {
        calls.push([route, body]);
        if (route === "host-metrics/settings") return stored;
        assert.equal(route, "host-metrics/settings_update");
        assert.equal(body.expected_revision, 4);
        assert.equal(body.settings.poll_interval_ms, 2500);
        stored = { ok: true, settings: body.settings, revision: 5 };
        return stored;
      },
    },
    notify() {},
  };
  context.Amaterasu.renderHostMetricsSettings(page, container);
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(calls.map(([route]) => route), ["host-metrics/settings"]);
  assert.match(container.textContent, /Saved settings revision 4/);
  const controls = descendants(container).filter((element) => element.tagName === "input");
  const byName = new Map(controls.map((control) => [control.name, control]));
  assert.equal(byName.get("realtime_stats_poll_interval_ms").min, "250");
  assert.equal(byName.get("realtime_stats_poll_interval_ms").max, "60000");
  assert.equal(byName.get("realtime_stats_history_retention_seconds").min, "30");
  assert.equal(byName.get("realtime_stats_history_retention_seconds").max, "3600");
  assert.equal(byName.get("realtime_stats_max_history_points").min, "30");
  assert.equal(byName.get("realtime_stats_max_history_points").max, "2000");
  assert.equal(byName.get("realtime_stats_enabled").checked, true);
  assert.equal(controls.length, 9);

  byName.get("realtime_stats_poll_interval_ms").valueAsNumber = 2500;
  byName.get("realtime_stats_history_retention_seconds").valueAsNumber = 300;
  byName.get("realtime_stats_max_history_points").valueAsNumber = 300;
  const form = descendants(container).find((element) => element.tagName === "form");
  form.listeners.get("submit")({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls.map(([route]) => route), [
    "host-metrics/settings",
    "host-metrics/settings_update",
    "host-metrics/settings",
    "host-metrics/settings",
  ]);
  assert.match(container.textContent, /Saved settings revision 5/);
});

test("disabled dashboard renders its saved state without polling stats", async () => {
  const calls = [];
  const context = vm.createContext({
    document: {
      body: new FakeElement("body"),
      visibilityState: "visible",
      createElement: (tag) => new FakeElement(tag),
      addEventListener() {},
      removeEventListener() {},
    },
    window: { setTimeout: () => 1, clearTimeout() {} },
    MutationObserver: class {
      observe() {}
      disconnect() {}
    },
    Error,
    console,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    if (source === "src/model.ts" || source === "src/ui.ts")
      vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }

  const container = new FakeElement("section");
  const savedHistory = new FakeElement("section");
  const page = {
    api: {
      async post(route) {
        calls.push(route);
        assert.equal(route, "host-metrics/settings");
        return { ok: true, settings: { ...settings, enabled: false }, revision: 8 };
      },
    },
  };
  context.Amaterasu.renderHostMetricsDashboard(page, container, savedHistory);
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(calls, ["host-metrics/settings"]);
  assert.match(container.textContent, /Real-time statistics are disabled/);
  assert.match(container.textContent, /Sampling is disabled/);
});


test("live metrics show measured vitals, missing sensors, and both network histories", async () => {
  const timers = [];
  const context = vm.createContext({
    document: {
      body: new FakeElement("body"), visibilityState: "visible",
      createElement: (tag) => new FakeElement(tag),
      createElementNS: (_namespace, tag) => new FakeElement(tag),
      addEventListener() {}, removeEventListener() {},
    },
    window: { setTimeout(callback) { timers.push(callback); return timers.length; }, clearTimeout() {} },
    MutationObserver: class { observe() {} disconnect() {} },
    Date, Error, console,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const host = {
    cpu_percent: 25, cpu_temp_c: 55, ram_used_bytes: 4, ram_total_bytes: 8,
    gpu_percent: null, gpu_encoder_percent: null, gpu_temp_c: null,
    vram_used_bytes: null, vram_total_bytes: null, net_rx_bps: 2_000_000, net_tx_bps: 1_000_000,
  };
  const container = new FakeElement("section");
  context.Amaterasu.renderHostMetricsDashboard({ api: { async post(route) {
    if (route === "host-metrics/settings") return { ok: true, revision: 1, settings: { ...settings, show_active_sessions: false } };
    assert.equal(route, "host-metrics/stats");
    return { ok: true, available: true, platform: "linux", sampled_at_unix_ms: 1,
      host, samples: [{ recorded_at_unix_ms: 1, host }] };
  } } }, container);
  await new Promise(setImmediate);
  timers.shift()();
  await new Promise(setImmediate);
  const vitals = descendants(container).filter((item) => item.className === "host-vital");
  assert.equal(vitals.length, 4);
  assert.match(vitals[0].textContent, /CPU25\.0%55\.0 °C/);
  assert.match(vitals[1].textContent, /GPUUnavailable/);
  assert.match(vitals[2].textContent, /Memory50\.0%/);
  assert.match(container.textContent, /Network receive2\.00 Mbit\/s/);
  assert.match(container.textContent, /Network transmit1\.00 Mbit\/s/);
  const charts = descendants(container).filter((item) => item.className === "host-metric-chart-card");
  assert.equal(charts.length, 6);
  assert.match(charts[1].textContent, /Unavailable.*0 measured points/);
});


test("overview host vitals respect hidden settings and never start background sampling", async () => {
  const timers = [];
  const listeners = new Map();
  const calls = [];
  const context = vm.createContext({
    document: {
      body: new FakeElement("body"), visibilityState: "visible",
      createElement: (tag) => new FakeElement(tag),
      addEventListener(name, callback) { listeners.set(name, callback); }, removeEventListener() {},
    },
    window: { setTimeout(callback) { timers.push(callback); return timers.length; }, clearTimeout() {} },
    MutationObserver: class { observe() {} disconnect() {} }, Error,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const container = new FakeElement("section");
  context.Amaterasu.renderHostMetricsDashboard({ api: { async post(route) {
    calls.push(route);
    assert.equal(route, "host-metrics/settings");
    return { ok: true, revision: 1, settings: { ...settings, show_host_stats: false } };
  } } }, container, undefined, "overview");
  await new Promise(setImmediate);
  listeners.get("visibilitychange")();
  descendants(container).find((item) => item.tagName === "button").listeners.get("click")();
  await new Promise(setImmediate);
  assert.deepEqual(calls, ["host-metrics/settings"]);
  assert.equal(timers.length, 0);
  assert.match(container.textContent, /Host statistics are hidden/);
});

test("host vitals never sample before saved preferences have loaded", async () => {
  const timers = [];
  const listeners = new Map();
  const calls = [];
  let finishSettings;
  const context = vm.createContext({
    document: {
      body: new FakeElement("body"), visibilityState: "visible", createElement: (tag) => new FakeElement(tag),
      addEventListener(name, callback) { listeners.set(name, callback); }, removeEventListener() {},
    },
    window: { setTimeout(callback) { timers.push(callback); return timers.length; }, clearTimeout() {} },
    MutationObserver: class { observe() {} disconnect() {} }, Error,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const container = new FakeElement("section");
  context.Amaterasu.renderHostMetricsDashboard({ api: { post(route) {
    calls.push(route);
    return new Promise((resolve) => { finishSettings = resolve; });
  } } }, container, undefined, "overview");
  listeners.get("visibilitychange")();
  descendants(container).find((item) => item.tagName === "button").listeners.get("click")();
  assert.deepEqual(calls, ["host-metrics/settings"]);
  assert.equal(timers.length, 0);
  finishSettings({ ok: true, revision: 1, settings: { ...settings, enabled: false } });
  await new Promise(setImmediate);
  assert.match(container.textContent, /Real-time statistics are disabled/);
  assert.equal(timers.length, 0);
});

test("host vitals report a settings failure and can retry loading preferences", async () => {
  const timers = [];
  let calls = 0;
  const context = vm.createContext({
    document: { body: new FakeElement("body"), visibilityState: "visible", createElement: (tag) => new FakeElement(tag),
      addEventListener() {}, removeEventListener() {} },
    window: { setTimeout(callback) { timers.push(callback); return timers.length; }, clearTimeout() {} },
    MutationObserver: class { observe() {} disconnect() {} }, Error,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const container = new FakeElement("section");
  context.Amaterasu.renderHostMetricsDashboard({ api: { async post(route) {
    assert.equal(route, "host-metrics/settings");
    if (++calls === 1) throw new Error("Settings offline");
    return { ok: true, revision: 1, settings: { ...settings, enabled: false } };
  } } }, container, undefined, "overview");
  await new Promise(setImmediate);
  assert.match(container.textContent, /Host metrics settings are unavailable/);
  assert.doesNotMatch(container.textContent, /Loading live host metrics/);
  descendants(container).find((item) => item.tagName === "button" && item.textContent === "Try again").listeners.get("click")();
  await new Promise(setImmediate);
  assert.equal(calls, 2);
  assert.match(container.textContent, /Real-time statistics are disabled/);
  assert.equal(timers.length, 0);
});

test("overview host vitals do not display invented values when a sample is unavailable", async () => {
  const timers = [];
  const context = vm.createContext({
    document: {
      body: new FakeElement("body"), visibilityState: "visible",
      createElement: (tag) => new FakeElement(tag), addEventListener() {}, removeEventListener() {},
    },
    window: { setTimeout(callback) { timers.push(callback); return timers.length; }, clearTimeout() {} },
    MutationObserver: class { observe() {} disconnect() {} }, Date, Error,
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/live_streams.ts", "src/host_metrics.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const host = { cpu_percent: 25, cpu_temp_c: 55, ram_used_bytes: 4, ram_total_bytes: 8,
    gpu_percent: null, gpu_encoder_percent: null, gpu_temp_c: null,
    vram_used_bytes: null, vram_total_bytes: null, net_rx_bps: null, net_tx_bps: null };
  const container = new FakeElement("section");
  const calls = [];
  context.Amaterasu.renderHostMetricsDashboard({ api: { async post(route) {
    calls.push(route);
    if (route === "host-metrics/settings") return { ok: true, revision: 1, settings };
    assert.equal(route, "host-metrics/stats");
    return { ok: true, available: false, platform: "test", sampled_at_unix_ms: null, host, samples: [] };
  } } }, container, undefined, "overview");
  await new Promise(setImmediate);
  timers.shift()();
  await new Promise(setImmediate);
  assert.match(container.textContent, /Host telemetry is unavailable/);
  assert.doesNotMatch(container.textContent, /25\.0%/);
  assert.doesNotMatch(container.textContent, /Recent host charts|Active streaming sessions/);
  assert.deepEqual(calls, ["host-metrics/settings", "host-metrics/stats"]);
});


test("active cards use live counters without recording and safely enrich recorded history", async () => {
  const timers=new Map(); let nextTimer=0, round=0, clockMs=1_000_000; const calls=[]; const finishSlow=[];
  class TestDate extends Date { static now() { return clockMs; } }
  const context=vm.createContext({
    document:{body:new FakeElement("body"),visibilityState:"visible",createElement:(tag)=>new FakeElement(tag),
      createElementNS:(_namespace,tag)=>new FakeElement(tag),addEventListener(){},removeEventListener(){}},
    window:{setTimeout(callback){const id=++nextTimer;timers.set(id,callback);return id;},clearTimeout(id){timers.delete(id);}},
    MutationObserver:class {observe(){} disconnect(){}}, Date:TestDate, Error, TextEncoder, console,
  });
  for(const source of ["src/model.ts","src/ui.ts","src/live_streams.ts","src/host_metrics.ts","src/session_history.ts","src/session_timeline.ts","src/pages.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source,"utf8"),{target:ts.ScriptTarget.ES2022}),context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)",context);
  }
  const session=(id)=>({session_id:id,protocol:"sunshine-modern",client_name:"",device_name:`Client ${id}`,app_name:"",
    width:1920,height:1080,target_fps:60,target_bitrate_kbps:10000,codec:"hevc",started_at_unix:1,ended_at_unix:null,duration_seconds:null});
  const live=(id)=>({session_id:id,started_at_unix:1,device_name:`Client ${id}`,width:1920,height:1080,
    target_fps:60,target_bitrate_kbps:10000,codec:"hevc",frames_sent:String(round===2.5 ? 340 : 100+round*120),
    bytes_sent:String(round===2.5 ? 41000 : 1000+round*20000),sequence_gaps:String(Math.floor(round))});
  const detail=(id)=>({ok:true,detail:{session:session(id),sample_count:60,event_count:0,samples_truncated:true,events_truncated:false,events:[],
    samples:[100,400].map((time,index)=>({session_id:id,recorded_at_unix:time,frames_sent:index,bytes_sent:index,dropped_frames:0,actual_fps:59,actual_bitrate_kbps:2000,encode_latency_ms:null}))}});
  const host={cpu_percent:null,cpu_temp_c:null,ram_used_bytes:null,ram_total_bytes:null,gpu_percent:null,gpu_encoder_percent:null,
    gpu_temp_c:null,vram_used_bytes:null,vram_total_bytes:null,net_rx_bps:null,net_tx_bps:null};
  const container=new FakeElement("section");
  context.Amaterasu.renderHostMetricsDashboard({api:{async post(route,payload) {
    calls.push([route,payload]);
    if(route === "host-metrics/settings") return {ok:true,revision:1,settings:{...settings,show_host_stats:false,show_host_charts:false}};
    if(route === "host-metrics/stats") {if(round===3) throw new Error("host offline");return {ok:true,available:true,platform:"test",sampled_at_unix_ms:1,host,samples:[]};}
    if(route === "live-streams/status") return {ok:true,epoch:(round>=3 ? "b" : "a").repeat(32),sampled_at_ms:String(1000+round*2000),
      sampled_at_unix_ms:String(100000+round*2000),sessions:round===5 ? [] : [live("one"),live("two")]};
    if(route === "history/history_status") return {ok:true,status:{available:true,configuration_ready:true,enabled:round>=2,effective_enabled:round>=2,
      degraded:false,applied_revision:1,failed_operations:0,active_streams:2,pending_activities:0,sessions:2,samples:4,events:0,live_bytes:0,
      last_prune:{sessions_deleted:0,live_bytes:0,size_limit_bytes:0,size_limit_unmet:false}}};
    if(route === "history/session_detail") {
      if(round===4) return new Promise((resolve)=>finishSlow.push(()=>resolve(detail(payload.session_id))));
      if(payload.session_id === "two") throw new Error("history missing");
      return detail(payload.session_id);
    }
    if(route === "history/timeline_open") return {ok:true,session:session(payload.session_id),samples:{total:60,cursor:{
      session_id:payload.session_id,kind:"samples",first_id:"1",last_id:"60",after_id:null,total:60}}};
    if(route === "history/timeline_page") {
      const start=payload.cursor.after_id===null ? 0 : Number(payload.cursor.after_id);const end=Math.min(60,start+payload.limit);
      const entries=Array.from({length:end-start},(_,i)=>({session_id:payload.session_id,recorded_at_unix:100+(start+i)*10,
        frames_sent:start+i,bytes_sent:start+i,dropped_frames:0,actual_fps:59,actual_bitrate_kbps:2000,encode_latency_ms:null}));
      return {ok:true,session_id:payload.session_id,kind:"samples",total:60,entries,entry_ids:entries.map((_,i)=>String(start+i+1)),
        next_cursor:end===60 ? null : {...payload.cursor,after_id:String(end)}};
    }
    throw new Error(`Unexpected route ${route}`);
  }}},container);
  await new Promise(setImmediate);
  const poll=async()=>{const [id,callback]=timers.entries().next().value;timers.delete(id);callback();await new Promise(setImmediate);};
  await poll();
  let cards=descendants(container).filter((node)=>node.className === "live-session-card");
  assert.equal(cards.length,2);assert.match(cards[0].textContent,/History recording is disabled/);
  assert.match(cards[0].textContent,/Waiting for another live counter reading/);
  const choice=descendants(cards[0]).find((node)=>node.tagName === "button" && node.textContent === "3 min");
  choice.listeners.get("click")();
  round=1;clockMs+=2000;await poll();
  assert.match(cards[0].textContent,/60\.0 fps/);assert.match(cards[0].textContent,/0\.1 Mbit\/s/);
  assert.equal(choice.attributes["aria-pressed"],"true");
  round=2;clockMs+=2000;await poll();
  cards=descendants(container).filter((node)=>node.className === "live-session-card");
  assert.equal(descendants(cards[0]).find((node)=>node.tagName === "button" && node.textContent === "3 min"),choice);
  assert.match(cards[0].textContent,/19 of 31 samples shown/);
  assert.match(cards[0].textContent,/Latest live rate: 60\.0 fps/);
  round=2.5;clockMs+=1000;await poll();
  assert.match(cards[0].textContent,/Latest live rate: 0\.0 fps/);
  assert.match(cards[0].textContent,/19 of 31 samples shown/);
  const liveOnly=descendants(cards[0]).find((node)=>node.tagName === "button" && node.textContent === "Live only");
  liveOnly.listeners.get("click")();
  assert.equal(liveOnly.attributes["aria-pressed"],"true");
  assert.match(cards[0].textContent,/Live observations stay in this browser/);
  assert.match(cards[1].textContent,/Recorded history is unavailable; charts use live observations/);
  round=3;clockMs+=2000;await poll();
  assert.match(container.textContent,/Host metrics could not be refreshed/);
  assert.equal(descendants(container).filter((node)=>node.className === "live-session-card").length,2);
  assert.notEqual(descendants(container).filter((node)=>node.className === "live-session-card")[0],cards[0]);
  round=4;clockMs+=2000;await poll();
  assert.equal(finishSlow.length,2);
  round=5;clockMs+=2000;await poll();
  assert.equal(descendants(container).filter((node)=>node.className === "live-session-card").length,0);
  assert.match(container.textContent,/No active streams reported/);
  finishSlow.forEach((finish)=>finish());await new Promise(setImmediate);
  assert.equal(descendants(container).filter((node)=>node.className === "live-session-card").length,0);
  assert.equal(calls.some(([route])=>route === "history/active_sessions"),false);
});

test("session-only accounts see live streams without host telemetry or configuration grants",async()=>{
  const timers=[];const calls=[];
  const context=vm.createContext({document:{body:new FakeElement("body"),visibilityState:"visible",createElement:(tag)=>new FakeElement(tag),
    createElementNS:(_namespace,tag)=>new FakeElement(tag),addEventListener(){},removeEventListener(){}},
    window:{setTimeout(callback){timers.push(callback);return timers.length;},clearTimeout(){}},
    MutationObserver:class {observe(){} disconnect(){}}, Date, Error, TextEncoder, console});
  for(const source of ["src/model.ts","src/ui.ts","src/live_streams.ts","src/host_metrics.ts","src/session_history.ts","src/session_timeline.ts","src/pages.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source,"utf8"),{target:ts.ScriptTarget.ES2022}),context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)",context);
  }
  const container=new FakeElement("section");
  context.Amaterasu.renderHostMetricsDashboard({api:{allows:(scope)=>scope === "session.read",async post(route){
    calls.push(route);
    if(route === "live-streams/status") return {ok:true,epoch:"a".repeat(32),sampled_at_ms:"1000",sampled_at_unix_ms:"100000",sessions:[{session_id:"s1",started_at_unix:1,device_name:null,
      width:1920,height:1080,target_fps:60,target_bitrate_kbps:10000,codec:"hevc",frames_sent:"10",bytes_sent:"100",sequence_gaps:"0"}]};
    if(route === "history/history_status") throw new Error("unavailable");
    throw new Error(`Unexpected route ${route}`);
  }}},container);
  await new Promise(setImmediate);timers.shift()();await new Promise(setImmediate);
  assert.match(container.textContent,/Display preferences are unavailable/);
  assert.match(container.textContent,/Host telemetry is unavailable for this account/);
  assert.match(container.textContent,/1 active stream reported/);
  assert.match(container.textContent,/Recorded history is unavailable; charts use live observations/);
  assert.equal(calls.includes("host-metrics/settings"),false);
  assert.equal(calls.includes("host-metrics/stats"),false);
});
