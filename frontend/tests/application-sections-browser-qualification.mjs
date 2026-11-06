// Optional real-browser qualification: node tests/browser-slider-qualification.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ts from "typescript";
import { createServer } from "node:http";
const plugin = readFileSync("../examples/web-extension/application.js", "utf8");
const rtxPlugin = readFileSync("../modules/rtx-hdr/ui/application.js", "utf8");
const server = createServer((request, response) => {
  response.setHeader("Content-Type", request.url.endsWith(".js") ? "text/javascript" : "text/html");
  response.end(request.url.endsWith(".js") ? (request.url.endsWith("rtx-application.js") ? rtxPlugin : plugin) : "<main><div id=sections></div></main>");
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const profile = mkdtempSync(join(tmpdir(), "amaterasu-application-sections-"));
const browser = spawn(process.env.AMATERASU_BROWSER ?? "chromium", ["--headless=new", "--disable-gpu", "--no-first-run", "--disable-extensions", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: "ignore" });
let socket;
try {
  let address;
  for (let i = 0; i < 200; i++) {
    try {
      const [port, path] = readFileSync(join(profile, "DevToolsActivePort"), "utf8").trim().split("\n");
      address = `ws://127.0.0.1:${port}${path}`;
      break;
    } catch { await new Promise(resolve => setTimeout(resolve, 50)); }
  }
  assert.ok(address, "Chromium debugger did not start");
  socket = new WebSocket(address);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let id = 0;
  const pending = new Map();
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data), callback = pending.get(message.id);
    if (callback) { pending.delete(message.id); message.error ? callback.reject(new Error(message.error.message)) : callback.resolve(message.result); }
  };
  const command = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const requestId = ++id;
    pending.set(requestId, { resolve, reject });
    socket.send(JSON.stringify({ id: requestId, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const { targetId } = await command("Target.createTarget", { url: origin });
  const { sessionId } = await command("Target.attachToTarget", { targetId, flatten: true });
  const evaluate = async expression => {
    const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? "Browser evaluation failed");
    return result.result.value;
  };
  for (let i = 0; i < 100 && !await evaluate(`!!document.querySelector('#sections')`); i++) await new Promise(resolve => setTimeout(resolve, 20));
  const source = ['locale-catalogs.ts','locale.ts','ui.ts','extension_ui.ts','extension_ui_layout.ts','extension_ui_tabs.ts','extension_ui_feedback.ts','extension_ui_dialog.ts','extension_ui_charts.ts','extension_slots.ts','extensions.ts','extension_fields.ts','session_extensions.ts'].map(file => readFileSync(`src/${file}`, 'utf8')).join('\n');
  await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
  const descriptor = JSON.parse(readFileSync('../modules/rtx-hdr/metadata/linux.json', 'utf8')).descriptor;
  await evaluate(`
    window.failures=[]; window.saved=null; window.greetings=0; window.values={}; window.revision=1;
    window.onunhandledrejection=e=>failures.push(String(e.reason));
    const descriptor=${JSON.stringify(descriptor)};
    window.context={content:document.querySelector('main'),notify(){},api:{allows:()=>true,async post(route,payload){
      if(route==='extensions/list')return {extensions:[{id:'hello',name:'Application tools',sdk_version:1,digest:'a'.repeat(64),entrypoint:'application.js',placements:['application'],methods:[{alias:'greet'}]}]};
      if(route==='extensions/call'){window.greetings++;await new Promise(resolve=>window.releaseGreeting=resolve);return {result:{message:payload.payload.name}};}
      if(route==='session-extensions/list')return {items:[descriptor],next:null,revision:1};
      if(route==='session-extensions/settings_get')return {revision:window.revision,values:window.values,effective:Object.fromEntries(descriptor.options.map(option=>[option.id,{value:window.values[option.id]??option.definition.default,source:null}]))};
      if(route==='session-extensions/settings_update'){window.saved=payload;window.values=payload.values;window.revision++;return {revision:window.revision};}
      throw new Error(route);
    }}};
    Amaterasu.renderApplicationPluginSections(window.context,document.querySelector('#sections'),{id:'app-0000000000000001',name:'Example game'});
  `);
  for(let i=0;i<150&&!await evaluate(`!!document.querySelector('.application-plugin-section input')`);i++)await new Promise(resolve=>setTimeout(resolve,20));
  assert.deepEqual(await evaluate(`window.failures`),[]);
  assert.equal(await evaluate(`document.querySelectorAll('.catalog-plugin-override-row').length`),descriptor.options.filter(option=>option.allowed_scopes.includes('application')).length);
  assert.equal(await evaluate(`document.querySelector('.application-plugin-section').textContent.includes('Tools for Example game')`),true);
  assert.equal(await evaluate(`Amaterasu.applicationPluginOverridesDirty(document.querySelector('#sections'))`),false);
  await evaluate(`const slider=document.querySelector('.catalog-plugin-overrides-form input[type=range]');slider.value='1500';slider.dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('.catalog-plugin-overrides-form').requestSubmit();`);
  for(let i=0;i<100&&!await evaluate(`!!window.saved`);i++)await new Promise(resolve=>setTimeout(resolve,20));
  assert.deepEqual(await evaluate(`window.saved.values`),{rtx_hdr_peak_brightness:1500});
  assert.equal(await evaluate(`window.saved.scope.id`),'app-0000000000000001');
  await evaluate(`const message=document.querySelector('.application-plugin-section input');message.value='Application action';message.dispatchEvent(new Event('input',{bubbles:true}));`);
  assert.equal(await evaluate(`Amaterasu.applicationPluginOverridesDirty(document.querySelector('#sections'))`),true);
  await evaluate(`window.detachedButton=document.querySelector('.application-plugin-section button');window.detachedButton.click();`);
  assert.equal(await evaluate(`Amaterasu.applicationPluginOverridesSaving(document.querySelector('#sections'))`),true);
  await evaluate(`window.releaseGreeting()`);
  for(let i=0;i<100&&await evaluate(`Amaterasu.applicationPluginOverridesSaving(document.querySelector('#sections'))`);i++)await new Promise(resolve=>setTimeout(resolve,20));
  assert.equal(await evaluate(`Amaterasu.applicationPluginOverridesDirty(document.querySelector('#sections'))`),false);
  await evaluate(`document.querySelector('#sections').remove()`);
  await new Promise(resolve=>setTimeout(resolve,20));
  await evaluate(`window.detachedButton.click()`);
  assert.equal(await evaluate(`window.greetings`),1);
  assert.deepEqual(await evaluate(`window.failures`),[]);
  console.log('PASS: full RTX HDR sliders, independent application UI, draft and busy guards, and cleanup in Chromium');
  await evaluate(`
    document.querySelector('main').innerHTML='<div id="sections"></div>';
    window.values={};window.revision=1;window.saved=null;
    const previous=window.context.api.post;
    window.context.api.post=async (route,payload)=>route==='extensions/list'
      ? {extensions:[{id:'rtx-hdr',name:'RTX HDR',sdk_version:1,digest:'b'.repeat(64),entrypoint:'rtx-application.js',placements:['application'],settings_provider:'amaterasu.rtx-hdr',methods:[]}]}
      : previous(route,payload);
    Amaterasu.renderApplicationPluginSections(window.context,document.querySelector('#sections'),{id:'app-0000000000000002',name:'RTX game'});
  `);
  for(let i=0;i<150&&!await evaluate(`document.querySelectorAll('.rtx-hdr-setting').length===9`);i++)await new Promise(resolve=>setTimeout(resolve,20));
  assert.equal(await evaluate(`document.querySelectorAll('.rtx-hdr-setting').length`),9);
  assert.equal(await evaluate(`document.querySelectorAll('.catalog-plugin-overrides-form').length`),0,'a selected plugin UI replaces the generic form');
  await evaluate(`{const peak=document.querySelector('.application-plugin-section input[type=range]');peak.value='1400';peak.dispatchEvent(new Event('input',{bubbles:true}));}`);
  assert.equal(await evaluate(`Amaterasu.applicationPluginOverridesDirty(document.querySelector('#sections'))`),true);
  for(let i=0;i<150&&!await evaluate(`window.saved?.values?.rtx_hdr_peak_brightness===1400`);i++)await new Promise(resolve=>setTimeout(resolve,20));
  assert.deepEqual(await evaluate(`window.saved.values`),{rtx_hdr_peak_brightness:1400});
  assert.equal(await evaluate(`window.saved.scope.id`),'app-0000000000000002');
  await evaluate(`{const peak=document.querySelector('.application-plugin-section input[type=range]');peak.value='1600';peak.dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('#sections').remove();}`);
  await new Promise(resolve=>setTimeout(resolve,300));
  assert.equal(await evaluate(`window.saved.values.rtx_hdr_peak_brightness`),1400,'closing the app cancels pending autosave');
  assert.deepEqual(await evaluate(`window.failures`),[]);
  console.log('PASS: plugin-owned RTX HDR section replaces generated controls and autosaves only its application');

} finally {
  socket?.close();browser.kill();
  await new Promise(resolve=>browser.once('exit',resolve));
  rmSync(profile,{recursive:true,force:true});
  await new Promise(resolve=>server.close(resolve));
}
