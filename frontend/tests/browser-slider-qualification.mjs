// Optional real-browser qualification: node tests/browser-slider-qualification.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ts from "typescript";

const profile = mkdtempSync(join(tmpdir(), "amaterasu-slider-browser-"));
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
  const { targetId } = await command("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await command("Target.attachToTarget", { targetId, flatten: true });
  const evaluate = async expression => {
    const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? "Browser evaluation failed");
    return result.result.value;
  };
  const source = ["ui.ts", "extension_ui.ts", "extension_fields.ts", "session_extensions.ts"].map(file => readFileSync(`src/${file}`, "utf8")).join("\n");
  await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
  await evaluate(`
    window.onunhandledrejection = e => { window.failure = String(e.reason?.stack || e.reason); }; globalThis.localizedTextNode = source => document.createTextNode(source); globalThis.localeFormattedTextNode = format => document.createTextNode(format(document.documentElement.lang || "en")); globalThis.translateText = source => source; Amaterasu.errorPanel = e => { window.failure = String(e.stack || e); return document.createElement("div"); }; globalThis.literalText = Amaterasu.literalText = x => x;
    Amaterasu.runAction = async (_button, _status, work) => { await work(); };
    document.body.innerHTML = '<main></main>'; window.saved = null;
    const descriptor = { provider: 'test', options: [{ id: 'strength', label: 'Strength', help: '', presentation: 'slider', definition: { type: 'integer', default: 10, minimum: 0, maximum: 100 }, allowed_scopes: ['application'], apply_policy: 'next_stream' }], actions: [] };
    const area = document.querySelector('main');
    const context = { content: area, notify() {}, api: { allows: () => true, post: async (route, payload) => {
      if (route.endsWith('/list')) return { items: [descriptor], next: null, revision: 1 };
      if (route.endsWith('/settings_get')) return { revision: 1, values: {}, effective: { strength: { value: 10, source: null } } };
      window.saved = payload.values; return {};
    } } };
    Amaterasu.renderSessionExtensionSettings(context, area, { kind: 'application', id: 'game' });
  `);
  for (let attempt = 0; attempt < 100 && !await evaluate(`!!document.querySelector("input[type=checkbox]")`); attempt++)
    await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(await evaluate(`window.failure ?? null`), null);
  assert.equal(await evaluate(`document.querySelector('input[type=checkbox]').checked`), true);
  await evaluate(`document.querySelector('input[type=range]').focus()`);
  for (const type of ["keyDown", "keyUp"]) await command("Input.dispatchKeyEvent", { type, key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39, nativeVirtualKeyCode: 39 }, sessionId);
  assert.deepEqual(await evaluate(`({ slider: document.querySelector('input[type=range]').value, number: document.querySelector('input[type=number]').value, inherit: document.querySelector('input[type=checkbox]').checked })`), { slider: "11", number: "11", inherit: false });
  await evaluate(`document.querySelector('form').requestSubmit()`);
  assert.deepEqual(await evaluate(`window.saved`), { strength: 11 });
  await evaluate(`
    window.saved = null;
    const inlineArea = document.querySelector('main'); inlineArea.replaceChildren();
    const inlineContext = { content: inlineArea, notify() {}, api: { allows: () => true, post: async (route, payload) => {
      if (route.endsWith('/list')) return { items: [descriptor], next: null, revision: 1 };
      if (route.endsWith('/settings_get')) return { revision: window.saved ? 2 : 1, values: window.saved ?? { strength: 10 }, effective: { strength: { value: window.saved?.strength ?? 10, source: {kind:'application', id:'game'} } } };
      window.saved = payload.values; return { revision: 2 };
    } } };
    Amaterasu.renderApplicationPluginOverrides(inlineContext, inlineArea, 'game');
  `);
  for (let attempt = 0; attempt < 100 && !await evaluate(`!!document.querySelector("input[type=range]")`); attempt++)
    await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(await evaluate(`window.failure ?? null`), null);
  await evaluate(`document.querySelector('input[type=range]').focus()`);
  for (const type of ["keyDown", "keyUp"]) await command("Input.dispatchKeyEvent", { type, key: "ArrowRight", code: "ArrowRight", windowsVirtualKeyCode: 39, nativeVirtualKeyCode: 39 }, sessionId);
  assert.deepEqual(await evaluate(`({ slider: document.querySelector('input[type=range]').value, number: document.querySelector('input[type=number]').value })`), { slider: "11", number: "11" });
  await evaluate(`document.querySelector('form').requestSubmit()`);
  for (let attempt = 0; attempt < 100 && !await evaluate(`window.saved !== null`); attempt++) await new Promise(resolve => setTimeout(resolve, 20));
  assert.deepEqual(await evaluate(`window.saved`), { strength: 11 });
  assert.equal(await evaluate(`window.failure ?? null`), null);
  await evaluate(`
    window.callbackFailure = null;
    window.onerror = message => { window.callbackFailure = String(message); return true; };
    window.extensionController = new AbortController();
    window.extensionSlider = Amaterasu.createExtensionUiV1(window.extensionController.signal).slider({
      label: "Plugin scale", minimum: 0, maximum: 1, step: 0.1, value: 0.3,
      onChange(value) { if (value === 0.8) throw new Error("plugin callback failure is visible"); }
    });
    document.body.replaceChildren(window.extensionSlider.element);
    window.stepSetRejected = false;
    try { window.extensionSlider.setValue(0.35); } catch { window.stepSetRejected = true; }
  `);
  assert.equal(await evaluate(`window.stepSetRejected`), true);
  assert.deepEqual(await evaluate(`({ range: document.querySelector('input[type=range]').value, number: document.querySelector('input[type=number]').value, value: window.extensionSlider.value() })`), { range: "0.3", number: "0.3", value: 0.3 });
  await evaluate(`document.querySelector('input[type=number]').value = '0.8'; document.querySelector('input[type=number]').dispatchEvent(new Event('input', { bubbles: true }));`);
  for (let attempt = 0; attempt < 100 && !await evaluate(`window.callbackFailure`); attempt++) await new Promise(resolve => setTimeout(resolve, 10));
  assert.match(await evaluate(`window.callbackFailure`), /plugin callback failure is visible/);
  await evaluate(`window.extensionController.abort(); window.extensionSlider.dispose(); window.callbackFailure = null; document.querySelector('input[type=number]').value = '0.9'; document.querySelector('input[type=number]').dispatchEvent(new Event('input', { bubbles: true }));`);
  assert.equal(await evaluate(`window.callbackFailure`), null);
  await evaluate(`
    window.fieldUi = Amaterasu.createExtensionUiV1(new AbortController().signal);
    window.fieldChanges = 0;
    window.fieldToggle = window.fieldUi.toggle({ label: "Locked toggle", value: true, readOnly: true, help: "Toggle help.", required: true, onChange() { window.fieldChanges++; } });
    window.fieldSelect = window.fieldUi.select({ label: "Dynamic mode", choices: [{value:"a",label:"Alpha"},{value:"b",label:"Beta"}], value: "a", readOnly: true, error: "Mode error." });
    window.fieldRadio = window.fieldUi.radioGroup({ label: "Preset", choices: [{value:"a",label:"Alpha"},{value:"b",label:"Beta"}], value: "a", readOnly: true, required: true, help: "Preset help.", error: "Preset error." });
    window.fieldSlider = window.fieldUi.slider({ label: "Locked scale", minimum: 0, maximum: 10, step: 1, value: 5, readOnly: true, required: true });
    document.body.replaceChildren(window.fieldToggle.element, window.fieldSelect.element, window.fieldRadio.element, window.fieldSlider.element);
    window.fieldToggle.element.querySelector('input').click();
    const selectInput = window.fieldSelect.element.querySelector('select'); selectInput.value = 'b'; selectInput.dispatchEvent(new Event('change', {bubbles:true}));
    window.fieldRadio.setChoices([{value:'a',label:'Alpha'},{value:'b',label:'Beta'},{value:'c',label:'Gamma'}]);
    const radioInput = window.fieldRadio.element.querySelectorAll('input[type=radio]')[1]; radioInput.checked = true; radioInput.dispatchEvent(new Event('change', {bubbles:true}));
  `);
  assert.deepEqual(await evaluate(`({ toggle: window.fieldToggle.value(), select: window.fieldSelect.value(), radio: window.fieldRadio.value(), disabled: window.fieldToggle.element.querySelector('input').disabled, required: window.fieldToggle.element.querySelector('input').required, readonly: window.fieldSelect.element.querySelector('select').getAttribute('aria-readonly'), radioCount: window.fieldRadio.element.querySelectorAll('input[type=radio]').length, radioDescription: !!window.fieldRadio.element.querySelectorAll('input[type=radio]')[1].getAttribute('aria-describedby'), descriptionTargets: [...window.fieldRadio.element.querySelectorAll('input[type=radio]')[1].getAttribute('aria-describedby').split(' ')].every(id => !!document.getElementById(id)), fieldChanges: window.fieldChanges })`), { toggle: true, select: "a", radio: "a", disabled: false, required: true, readonly: "true", radioCount: 3, radioDescription: true, descriptionTargets: true, fieldChanges: 0 });
  console.log("PASS: plugin forms retain accessible states and descriptions across dynamic choices");
  console.log("PASS: shared and plugin sliders synchronize, enforce steps, expose callback errors, and dispose listeners");
} finally {
  socket?.close();
  browser.kill();
  if (browser.exitCode === null) await new Promise(resolve => browser.once("exit", resolve));
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
