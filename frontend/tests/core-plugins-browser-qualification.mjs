// Real Chromium qualification of independently served core-plugin UIs.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import ts from "typescript";
import { createServer } from "node:http";
const sdkRoot = resolve("..");
const assets = { "display.js": readFileSync(join(sdkRoot, "modules/display/ui/display.js")), "settings.js": readFileSync(join(sdkRoot, "modules/frame-limiter/ui/settings.js")) };
const server = createServer((request, response) => {
    const name = request.url.split("/").at(-1);
    if (assets[name]) {
        response.setHeader("Content-Type", "text/javascript");
        response.end(assets[name]);
    }
    else if (name === "base.css") {
        response.setHeader("Content-Type", "text/css");
        response.end(readFileSync(join(sdkRoot, "frontend/dist/styles.css")));
    }
    else
        response.end('<!doctype html><meta charset=utf-8><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/base.css"><main style="padding:24px;max-width:1400px;margin:auto"><div id="sections"></div><div id="legacy-display" data-settings-owner="amaterasu.display">Legacy display controls</div><div id="host-plugins"></div><div id="app-plugins"></div></main>');
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const profile = mkdtempSync(join(tmpdir(), "amaterasu-stats-"));
const browser = spawn(process.env.AMATERASU_BROWSER ?? "chromium", ["--headless=new", "--disable-gpu", "--no-first-run", "--disable-extensions", "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: "ignore" });
let socket;
try {
    let address;
    for (let i = 0; i < 200; i++) {
        try {
            const [port, path] = readFileSync(join(profile, "DevToolsActivePort"), "utf8").trim().split("\n");
            address = `ws://127.0.0.1:${port}${path}`;
            break;
        }
        catch {
            await new Promise(resolve => setTimeout(resolve, 50));
        }
    }
    assert.ok(address, "Chromium debugger did not start");
    socket = new WebSocket(address);
    await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
    let id = 0;
    const pending = new Map();
    socket.onmessage = ({ data }) => {
        const message = JSON.parse(data), callback = pending.get(message.id);
        if (callback) {
            pending.delete(message.id);
            message.error ? callback.reject(new Error(message.error.message)) : callback.resolve(message.result);
        }
    };
    const command = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
        const requestId = ++id;
        pending.set(requestId, { resolve, reject });
        socket.send(JSON.stringify({ id: requestId, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
    const { targetId } = await command("Target.createTarget", { url: origin });
    const { sessionId } = await command("Target.attachToTarget", { targetId, flatten: true });
    await command("Emulation.setDeviceMetricsOverride", { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false }, sessionId);
    const evaluate = async (expression) => {
        const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
        if (result.exceptionDetails)
            throw new Error(result.exceptionDetails.exception?.description ?? "Browser evaluation failed");
        return result.result.value;
    };
    for (let i = 0; i < 100 && !await evaluate(`!!document.querySelector('#sections')`); i++)
        await new Promise(resolve => setTimeout(resolve, 20));
    const source = ['locale-catalogs.ts', 'locale.ts', 'model.ts', 'ui.ts', 'extension_fields.ts', 'extension_ui.ts', 'extension_ui_layout.ts', 'extension_ui_tabs.ts', 'extension_ui_feedback.ts', 'extension_ui_dialog.ts', 'extension_ui_charts.ts', 'extension_slots.ts', 'session_extensions.ts', 'updates.ts', 'extensions.ts'].map(file => readFileSync(join(sdkRoot, 'frontend/src', file), 'utf8')).join('\n');
    await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
    const frame = JSON.parse(readFileSync(join(sdkRoot, 'modules/frame-limiter/metadata/linux.json'), 'utf8')).descriptor;
    const displayUi = JSON.parse(readFileSync(join(sdkRoot, 'modules/display/ui/ui.json'), 'utf8'));
    await evaluate(`(async()=>{
    window.frameDescriptor=${JSON.stringify(frame)};window.readOnly=false;window.calls=[];window.writes=[];window.notifications=0;
    window.hostState={ok:true,revision:7,configured_output_name:'screen-a',configured_display_policy:{dd_configuration_option:'verify_only',adapter_pnp_id:'keep-adapter'}};
    window.entry={name:'Game',launch:{kind:'executable',program:'/game'},preparation:[{command:'keep'}],config_overrides:{fec_percentage:20,display_policy:{dd_configuration_option:'verify_only'}}};window.entryRevision=10;
    window.documents={host:{revision:1,values:{}},application:{revision:1,values:{}}};
    const descriptors=[{id:'display',name:'Display',digest:'a'.repeat(64),sdk_version:1,entrypoint:'display.js',settings_owner:'amaterasu.display',placements:['page','settings','application'],methods:${JSON.stringify(displayUi.methods.map(({ alias }) => ({ alias })))}},
      {id:'frame-limiter',name:'Frame limiting',digest:'b'.repeat(64),sdk_version:1,entrypoint:'settings.js',settings_provider:'amaterasu.frame-limiter',placements:['settings','application'],methods:[]}];
    window.api={allows:scope=>!readOnly||!scope.endsWith('.write'),async post(route,payload={}){
      calls.push([route,payload]);
      if(route==='extensions/list')return{extensions:descriptors.map(descriptor=>({...descriptor,methods:descriptor.methods.filter(method=>!readOnly||!method.alias.includes('update'))}))};
      if(route==='repositories/list')return{ok:true,repositories:[],next_offset:null};
      if(route==='package-selection/status')return{ok:true,revision:'c'.repeat(64),active:'d'.repeat(64),previous:null,pending:null,trial:null,highest_sequence:'1',running:{available:true,digest:'d'.repeat(64),modules:[{id:'amaterasu.display',version:'0.1.0',sha256:'e'.repeat(64),runtime:false},{id:'amaterasu.frame-limiter',version:'0.1.0',sha256:'f'.repeat(64),runtime:false}]}};
      if(route==='session-extensions/list')return{items:[frameDescriptor],next:null,revision:1};
      if(route==='session-extensions/settings_get'){const document=documents[payload.scope.kind];return{revision:document.revision,values:structuredClone(document.values),effective:Object.fromEntries(frameDescriptor.options.map(option=>[option.id,{value:document.values[option.id]??option.definition.default,source:null}]))};}
      if(route==='session-extensions/settings_update'){if(readOnly)throw Error('read-only');const document=documents[payload.scope.kind];if(document.revision!==payload.expected_revision)throw Error('revision conflict');document.values=structuredClone(payload.values);writes.push([route,payload]);return{revision:++document.revision};}
      if(route==='extensions/call'){
        const alias=payload.method,value=payload.payload;
        if(alias==='host_read')return{result:structuredClone(hostState)};
        if(alias==='app_read')return{result:{ok:true,revision:entryRevision,id:'app-0000000000000001',entry:structuredClone(entry)}};
        if(readOnly)throw Error('read-only');writes.push([alias,value]);
        if(alias==='host_update'){if(value.expected_revision!==hostState.revision)throw Error('revision conflict');hostState.configured_output_name=value.values.output_name??'';for(const[key,item]of Object.entries(value.values.display_policy)){if(item==null)delete hostState.configured_display_policy[key];else hostState.configured_display_policy[key]=item;}return{result:{ok:true,revision:++hostState.revision}};}
        if(alias==='app_update'){if(value.expected_revision!==entryRevision)throw Error('revision conflict');entry=structuredClone(value.entry);return{result:{ok:true,revision:++entryRevision}};}
        throw Error(alias);
      }
      throw Error(route);
    }};
    window.context={content:document.querySelector('main'),api,registerDraftGuard(){return()=>{};}};
    Amaterasu.renderUpdates({...context,content:document.querySelector('#sections')});
    Amaterasu.renderExtensionSlot(context,document.querySelector('#host-plugins'),'settings');
  })()`);
    const until = async (expression) => { for (let i = 0; i < 150; i++) {
        if (await evaluate(expression))
            return;
        await new Promise(resolve => setTimeout(resolve, 20));
    } throw Error(`Timed out: ${expression}; DOM: ${await evaluate("document.body.innerText")}`); };
    await until(`document.querySelector('#host-plugins input[type=range][max="1000"]')`);
    assert.equal(await evaluate(`getComputedStyle(document.querySelector('#legacy-display')).display==='none'`), true, 'signed ownership replaces legacy controls');
    assert.match(await evaluate(`document.querySelector('#sections').textContent`), /amaterasu.display/);
    assert.match(await evaluate(`document.querySelector('#sections').textContent`), /amaterasu.frame-limiter/);
    await evaluate(`{const control=document.querySelector('#host-plugins input[type=range][max="1000"]');control.value='59.94';control.dispatchEvent(new Event('input',{bubbles:true}));}`);
    await until(`documents.host.values.frame_limiter_fps_limit===59.94`);
    await until(`[...document.querySelectorAll('#host-plugins select')].some(node=>node.labels?.[0]?.textContent.includes('Display preparation'))`);
    await evaluate(`{const control=[...document.querySelectorAll('#host-plugins select')].find(node=>node.labels?.[0]?.textContent.includes('Display preparation'));control.value='ensure_primary';control.dispatchEvent(new Event('change',{bubbles:true}));}`);
    await until(`hostState.configured_display_policy.dd_configuration_option==='ensure_primary'`);
    assert.equal(await evaluate(`hostState.configured_display_policy.adapter_pnp_id`), 'keep-adapter');
    const directory = process.env.AMATERASU_QUALIFICATION_DIR;
    if (directory) {
        mkdirSync(directory, { recursive: true });
        const shot = await command('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true }, sessionId);
        writeFileSync(join(directory, 'core-module-settings-desktop.png'), Buffer.from(shot.data, 'base64'));
    }
    await evaluate(`document.querySelector('#host-plugins').remove();Amaterasu.renderApplicationPluginSections(context,document.querySelector('#app-plugins'),{id:'app-0000000000000001',name:'Game',onDocumentChange(){notifications++;}});`);
    await until(`document.querySelector('#app-plugins input[type=range][max="1000"]')`);
    await until(`[...document.querySelectorAll('#app-plugins select')].some(node=>node.labels?.[0]?.textContent.includes('Display preparation'))`);
    await evaluate(`{const control=[...document.querySelectorAll('#app-plugins select')].find(node=>node.labels?.[0]?.textContent.includes('Display preparation'));control.value='ensure_active';control.dispatchEvent(new Event('change',{bubbles:true}));}`);
    await until(`notifications===1`);
    assert.equal(await evaluate(`entry.config_overrides.display_policy.dd_configuration_option`), 'ensure_active');
    assert.equal(await evaluate(`entry.config_overrides.fec_percentage`), 20);
    await command('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true }, sessionId);
    await evaluate(`new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))`);
    assert.equal(await evaluate(`document.documentElement.scrollWidth<=document.documentElement.clientWidth+1`), true, JSON.stringify(await evaluate(`[...document.querySelectorAll('body *')].filter(node=>node.getBoundingClientRect().right>document.documentElement.clientWidth+1&&!node.closest('.table-scroll')).slice(0,25).map(node=>({tag:node.tagName,classes:node.className,width:node.getBoundingClientRect().width,right:node.getBoundingClientRect().right}))`)));
    if (directory) {
        const shot = await command('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true }, sessionId);
        writeFileSync(join(directory, 'core-module-settings-mobile.png'), Buffer.from(shot.data, 'base64'));
    }
    await evaluate(`Amaterasu.disposeApplicationPluginSections(document.querySelector('#app-plugins'));window.before=writes.length;`);
    await new Promise(resolve => setTimeout(resolve, 400));
    assert.equal(await evaluate(`writes.length===before`), true, 'closing retires queued writes');
    await evaluate(`readOnly=true;window.readonlyPages=new Amaterasu.ExtensionPages(api);(async()=>{await readonlyPages.discover();await readonlyPages.mountSlot('frame-limiter',document.querySelector('#app-plugins'),'settings');})()`);
    await until(`document.querySelector('#app-plugins input[type=range][max="1000"]')?.disabled`);
    assert.equal(await evaluate(`[...document.querySelectorAll('#app-plugins input,#app-plugins select,#app-plugins button')].every(control=>control.disabled)`), true, 'read-only host settings disable writes');
    await evaluate(`readonlyPages.dispose()`);
    console.log('PASS: module inventory, independent Display and Frame limiting UIs, global/app writes, notifications, read-only controls, mobile layout and disposal');
}
finally {
    socket?.close();
    browser.kill('SIGTERM');
    await new Promise(resolve => browser.once('exit', resolve));
    rmSync(profile, { recursive: true, force: true });
    await new Promise(resolve => server.close(resolve));
}
