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
  const source = ["ui.ts", "extension_fields.ts", "session_extensions.ts"].map(file => readFileSync(`src/${file}`, "utf8")).join("\n");
  await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
  await evaluate(`
    window.onunhandledrejection = e => { window.failure = String(e.reason?.stack || e.reason); }; Amaterasu.errorPanel = e => { window.failure = String(e.stack || e); return document.createElement("div"); }; globalThis.literalText = Amaterasu.literalText = x => x;
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
  console.log("PASS: native slider keyboard input synchronizes value and saves scoped override");
} finally {
  socket?.close();
  browser.kill();
  if (browser.exitCode === null) await new Promise(resolve => browser.once("exit", resolve));
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
