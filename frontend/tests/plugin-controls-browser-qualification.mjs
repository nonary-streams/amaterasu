// Real Chromium check for reusable plugin settings controls with simulated APIs.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ts from "typescript";

const profile = mkdtempSync(join(tmpdir(), "amaterasu-plugin-controls-"));
const browser = spawn(process.env.AMATERASU_BROWSER ?? "chromium", [
  "--headless=new", "--disable-gpu", "--no-first-run",
  "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0",
  `--user-data-dir=${profile}`, "about:blank",
], { stdio: "ignore" });
let socket;
let sessionId;

try {
  let address;
  for (let attempt = 0; attempt < 200; attempt++) {
    try {
      const [port, path] = readFileSync(join(profile, "DevToolsActivePort"), "utf8").trim().split("\n");
      address = `ws://127.0.0.1:${port}${path}`;
      break;
    } catch {
      if (browser.exitCode !== null) throw new Error("Chromium exited before debugger startup");
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  assert.ok(address, "Chromium debugger did not start");
  socket = new WebSocket(address);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let sequence = 0;
  const pending = new Map();
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    message.error ? waiter.reject(new Error("CDP command failed")) : waiter.resolve(message.result);
  };
  const command = (method, params = {}, target = sessionId) => new Promise((resolve, reject) => {
    const id = ++sequence;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params, ...(target ? { sessionId: target } : {}) }));
  });
  const { targetId } = await command("Target.createTarget", { url: "about:blank" }, undefined);
  sessionId = (await command("Target.attachToTarget", { targetId, flatten: true }, undefined)).sessionId;
  await command("Page.enable");
  await command("Runtime.enable");
  const evaluate = async (expression) => {
    const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.result?.exception?.description ?? "Browser evaluation failed");
    return result.result.value;
  };
  const wait = async (expression, description) => {
    for (let attempt = 0; attempt < 200; attempt++) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    assert.fail(`timed out waiting for ${description}`);
  };
  const key = async (name, code, virtualKey) => {
    for (const type of ["keyDown", "keyUp"])
      await command("Input.dispatchKeyEvent", { type, key: name, code,
        windowsVirtualKeyCode: virtualKey, nativeVirtualKeyCode: virtualKey });
  };

  const source = [
    "locale-catalogs.ts", "locale.ts", "ui.ts", "extension_fields.ts",
    "session_extension_actions.ts", "session_extensions.ts",
  ].map((file) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8")).join("\n");
  await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
  await evaluate(`
    document.body.innerHTML = '<main id="page"><h1></h1><section id="controls"></section></main><aside id="locale"></aside>';
    Amaterasu.setLocalizedTextContent(document.querySelector('#page h1'),'Overview');
    window.savedSettings = null;
    window.currentSettings = {};
    window.currentLocale = 'en';
    const localeFields = {locale:{type:'enum',default:'en',options:[...Amaterasu.LOCALE_OPTIONS]}};
    const api = {
      allows:()=>true,
      async post(route,payload) {
        if (route === 'configuration/get') return {revision:1,fields:localeFields,values:{locale:window.currentLocale}};
        if (route === 'configuration/update') { window.currentLocale=payload.values.locale; return {revision:2,fields:localeFields,values:{locale:window.currentLocale}}; }
        if (route === 'session-extensions/list') return {revision:1,next:null,items:[{
          provider:'qualification.controls',display_name:'Browser fixture',actions:[],
          options:[
            {id:'strength',label:'Strength',help:'Provider-owned label',presentation:'slider',
              definition:{type:'integer',default:10,minimum:0,maximum:100},
              allowed_scopes:['host'],apply_policy:'next_stream'},
            {id:'pace',label:'Pacing',help:'Choice label belongs to the provider',
              definition:{type:'enum',default:'smooth',options:['smooth','low']},
              choice_labels:{smooth:'Smooth pacing',low:'Low latency'},
              allowed_scopes:['host'],apply_policy:'next_stream'}
          ]
        }]};
        if (route === 'session-extensions/settings_get') return {revision:7,values:{...window.currentSettings},
          effective:{strength:{value:window.currentSettings.strength ?? 10,source:null},
            pace:{value:window.currentSettings.pace ?? 'smooth',source:null}}};
        if (route === 'session-extensions/settings_update') {
          window.savedSettings = payload.values;
          window.currentSettings = payload.values;
          return {revision:8};
        }
        throw new Error('Unexpected route');
      }
    };
    Amaterasu.setLocalizedPageTitle('Overview');
    document.querySelector('#locale').append(Amaterasu.mountLocaleSelector(api));
    const area = document.querySelector('#controls');
    const context = {content:document.querySelector('#page'),api,notify(){},registerDraftGuard() { return () => {}; }};
    Amaterasu.renderSessionExtensionSettings(context,area,{kind:'host'});
  `);
  await wait("!!document.querySelector('#page input[type=range]') && !!document.querySelector('#global-locale:not(:disabled)')",
    "generic controls and locale selector");
  assert.equal(await evaluate("document.querySelector('#page h1').textContent"), "Overview");
  await evaluate("document.querySelector('#page input[type=range]').focus()");
  await key("ArrowRight", "ArrowRight", 39);
  assert.deepEqual(await evaluate(`({range:document.querySelector('#page input[type=range]').value,
    number:document.querySelector('#page input[type=number]').value,
    label:document.querySelector('#page input[type=range]').getAttribute('aria-label')})`),
  {range:"11",number:"11",label:"Strength slider"});

  await evaluate(`(() => { const select=document.querySelector('#global-locale'); select.value='de';
    select.dispatchEvent(new Event('change',{bubbles:true})); })()`);
  await wait(`document.querySelector('#page h1')?.textContent === 'Übersicht' &&
    document.querySelector('#page button[type=submit]')?.textContent.includes('Einstellungen speichern')`,
    "live locale translation");
  assert.equal(await evaluate("document.querySelector('#page legend').textContent"), "Strength");
  assert.equal(await evaluate("document.querySelector('#page select:not(#global-locale) option').textContent"), "Smooth pacing");

  await evaluate("document.querySelector('#page button[type=submit]').focus()");
  await key(" ", "Space", 32);
  await wait("window.savedSettings?.strength === 11 && window.savedSettings?.pace === 'smooth'",
    "keyboard form submission");

  await evaluate(`
    window.pendingList = {};
    window.pendingList.promise = new Promise(resolve => { window.pendingList.resolve = resolve; });
    const oldPage = document.createElement('main');
    oldPage.id='old-page';
    const staleArea = document.createElement('div');
    staleArea.id='stale-plugin-area';
    oldPage.append(staleArea);
    document.body.append(oldPage);
    const staleContext = {content:oldPage,notify(){},api:{allows:()=>true,post:async(route)=>{
      if (route === 'session-extensions/list') return await window.pendingList.promise;
      return {revision:1,values:{},effective:{}};
    }}};
    Amaterasu.renderSessionExtensionSettings(staleContext,staleArea,{kind:'host'});
    oldPage.remove();
    window.pendingList.resolve({revision:1,next:null,items:[]});
  `);
  await new Promise((resolve) => setTimeout(resolve, 80));
  assert.equal(await evaluate("document.querySelector('#stale-plugin-area')"), null);
  assert.equal(await evaluate("document.querySelector('#old-page')"), null);
  console.log("PASS: real Chromium keyboard editing, accessible labels, localized generic controls, and stale-view disposal");
} finally {
  socket?.close();
  if (browser.exitCode === null) {
    browser.kill();
    await new Promise((resolve) => browser.once("exit", resolve));
  }
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
