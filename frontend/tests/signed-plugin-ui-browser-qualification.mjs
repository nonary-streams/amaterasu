// Drives an owned Chromium against an isolated signed host and packaged web extension.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const options = JSON.parse(readFileSync(0, "utf8"));
const check = (condition, message) => { if (!condition) throw new Error(message); };
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const assetBytes = readFileSync(options.assetPath);
check(sha256(assetBytes) === options.assetSha256, "fixture digest mismatch before browser run");

class Debugger {
  sequence = 0;
  pending = new Map();
  listeners = new Set();

  async connect(address) {
    const url = new URL(address);
    check(url.protocol === "ws:" && url.hostname === "127.0.0.1" &&
      Number(url.port) > 0 && /^\/devtools\/browser\/[a-f0-9-]+$/.test(url.pathname),
    "unexpected owned Chromium debugger address");
    this.socket = new WebSocket(url);
    this.socket.addEventListener("message", ({ data }) => {
      const message = JSON.parse(data);
      if (message.id !== undefined) {
        const waiter = this.pending.get(message.id);
        if (!waiter) return;
        this.pending.delete(message.id);
        clearTimeout(waiter.timeout);
        if (message.error) waiter.reject(new Error("CDP command failed"));
        else waiter.resolve(message.result);
      } else {
        for (const listener of this.listeners) listener(message);
      }
    });
    this.socket.addEventListener("close", () => {
      for (const waiter of this.pending.values()) {
        clearTimeout(waiter.timeout);
        waiter.reject(new Error("owned browser debugger closed"));
      }
      this.pending.clear();
    });
    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("debugger admission timed out")), 10000);
      this.socket.addEventListener("open", () => { clearTimeout(timeout); resolve(); }, { once: true });
      this.socket.addEventListener("error", () => {
        clearTimeout(timeout);
        reject(new Error("debugger admission failed"));
      }, { once: true });
    });
  }

  command(method, params = {}, sessionId) {
    const id = ++this.sequence;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error("CDP command timed out"));
      }, 10000);
      this.pending.set(id, { resolve, reject, timeout });
      this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }

  close() { this.socket?.close(); }
}

const debuggerClient = new Debugger();
let target;
const networkRequests = new Map();
const assetResponses = [];
try {
  await debuggerClient.connect(options.debuggerUrl);
  const browser = await debuggerClient.command("Browser.getBrowserCommandLine");
  check(browser.arguments.includes(`--user-data-dir=${options.profile}`) &&
    browser.arguments.includes(`--ignore-certificate-errors-spki-list=${options.spki}`),
  "Chromium profile or pinned test certificate differs");
  check(!browser.arguments.some((argument) =>
    ["--ignore-certificate-errors", "--disable-web-security", "--no-sandbox"].includes(argument.split("=", 1)[0])),
  "owned Chromium enabled a broad security bypass");

  const { targetId } = await debuggerClient.command("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await debuggerClient.command("Target.attachToTarget", { targetId, flatten: true });
  target = { targetId, sessionId };
  debuggerClient.listeners.add((event) => {
    if (event.sessionId !== sessionId) return;
    if (event.method === "Network.requestWillBeSent") {
      const url = new URL(event.params.request.url);
      if (/^\/v1\/extensions\/[^/]+\/[a-f0-9]{64}\/assets\/example\.js$/.test(url.pathname)) {
        networkRequests.set(event.params.requestId, url.pathname);
      }
    } else if (event.method === "Network.responseReceived") {
      const path = networkRequests.get(event.params.requestId);
      if (path) assetResponses.push({ requestId: event.params.requestId, path, status: event.params.response.status });
    }
  });
  await debuggerClient.command("Page.enable", {}, sessionId);
  await debuggerClient.command("Runtime.enable", {}, sessionId);
  await debuggerClient.command("Network.enable", {}, sessionId);
  await debuggerClient.command("Page.navigate", { url: `${options.origin}/#extension/plugin-ui-qualification` }, sessionId);

  const evaluate = async (expression) => {
    const reply = await debuggerClient.command("Runtime.evaluate", {
      expression, awaitPromise: true, returnByValue: true,
    }, sessionId);
    check(!reply.exceptionDetails, "browser expression failed");
    return reply.result.value;
  };
  const wait = async (expression, description) => {
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline) {
      try {
        if (await evaluate(expression)) return;
      } catch (error) {
        if (error.message !== "CDP command failed") throw error;
      }
      await delay(40);
    }
    const pageState = await evaluate(`JSON.stringify({href: location.href, title: document.title,
      readyState: document.readyState, body: document.body?.innerText.slice(0, 1200),
      markup: document.body?.innerHTML.slice(0, 240)})`).catch(() => "page state unavailable");
    throw new Error(`browser timed out waiting for ${description}; page state: ${pageState}`);
  };
  const key = async (name, code, virtualKey) => {
    for (const type of ["keyDown", "keyUp"]) {
      await debuggerClient.command("Input.dispatchKeyEvent", {
        type, key: name, code, windowsVirtualKeyCode: virtualKey, nativeVirtualKeyCode: virtualKey,
      }, sessionId);
    }
  };

  await wait("!!document.querySelector('input[name=username]')", "sign-in form");
  await evaluate(`(() => {
    const username = document.querySelector('input[name=username]');
    username.value = ${JSON.stringify(options.username)};
    document.querySelector('input[name=password]').value = ${JSON.stringify(options.password)};
    document.querySelector('input[name=remember_me]').checked = false;
    username.closest('form').requestSubmit();
  })()`);
  await wait("!!document.querySelector('.shell')", "signed-in shell");
  await wait("!!document.querySelector('[data-plugin-ui-qualification] button[aria-label=\"Say hello\"]') && window.__pluginUiQualificationMounts === 1",
    "mounted signed extension");
  await wait("window.__pluginUiQualificationMounts === 1 && !!document.querySelector('#global-locale:not(:disabled)')",
    "extension and locale preference controls");

  const asset = assetResponses.find((entry) => entry.status === 200);
  check(asset, "browser did not receive the signed extension asset with HTTP 200");
  const served = await debuggerClient.command("Network.getResponseBody", { requestId: asset.requestId }, sessionId);
  const servedBytes = served.base64Encoded ? Buffer.from(served.body, "base64") : Buffer.from(served.body);
  check(sha256(servedBytes) === options.assetSha256, "served signed extension asset differs from the packaged fixture");
  check(asset.path.startsWith("/v1/extensions/plugin-ui-qualification/") &&
    asset.path.endsWith("/assets/example.js"), "asset did not use the selected extension identity and digest route");

  const keyboardControl = await evaluate(`(() => {
    const panel = document.querySelector('[data-plugin-ui-qualification]');
    const slider = panel.querySelector('input[aria-label="Strength"]');
    const button = panel.querySelector('button[aria-label="Say hello"]');
    slider.focus();
    return document.activeElement === slider && button.textContent === 'Say hello';
  })()`);
  check(keyboardControl, "extension control lacks a usable accessible name or focus target");
  await key("ArrowRight", "ArrowRight", 39);
  check(await evaluate(`document.querySelector('[data-plugin-ui-qualification] input[type=range]').value === '11'`),
    "keyboard arrow did not operate the packaged extension range control");
  await evaluate(`document.querySelector('[data-plugin-ui-qualification] button[aria-label="Say hello"]').focus()`);
  await key("Enter", "Enter", 13);
  await wait("document.querySelector('[data-plugin-ui-qualification] [role=status]')?.textContent === 'Hello, keyboard!'",
    "keyboard-activated signed provider call");

  // Mount the reusable schema-driven controls in the real page runtime. The
  // control API is stubbed here: this proves browser UI behavior, not provider
  // persistence or the native settings broker.
  await evaluate(`(() => {
    window.__genericControlSave = null;
    window.__genericControlValues = {};
    const area = document.createElement('section');
    area.id = 'plugin-ui-generic-controls';
    document.querySelector('.content').append(area);
    const descriptor = {provider:'qualification.browser-controls', display_name:'Browser control fixture',
      options:[{id:'strength',label:'Strength',help:'Provider-owned range label',
        presentation:'slider',definition:{type:'integer',default:10,minimum:0,maximum:100},
        allowed_scopes:['host'],apply_policy:'next_stream'}], actions:[]};
    const context = {content:area,notify(){},api:{allows:()=>true,post:async(route,payload)=>{
      if (route === 'session-extensions/list') return {items:[descriptor],next:null,revision:1};
      if (route === 'session-extensions/settings_get') return {revision:7,values:{...window.__genericControlValues},
        effective:{strength:{value:window.__genericControlValues.strength ?? 10,source:null}}};
      if (route === 'session-extensions/settings_update') {
        window.__genericControlSave = payload.values;
        window.__genericControlValues = payload.values;
        return {revision:8};
      }
      throw new Error('Unexpected generic control route');
    }}};
    Amaterasu.renderSessionExtensionSettings(context,area,{kind:'host'});
  })()`);
  await wait("!!document.querySelector('#plugin-ui-generic-controls input[type=range]')",
    "reusable schema-driven browser control");
  await evaluate(`document.querySelector('#plugin-ui-generic-controls input[type=range]').focus()`);
  await key("ArrowRight", "ArrowRight", 39);
  check(await evaluate(`(() => {
    const area = document.querySelector('#plugin-ui-generic-controls');
    return area.querySelector('input[type=range]').value === '11' &&
      area.querySelector('input[type=number]').value === '11' &&
      area.querySelector('input[type=range]').getAttribute('aria-label') === 'Strength';
  })()`), "reusable control keyboard editing or accessible labeling failed");

  await evaluate(`(() => {
    const locale = document.querySelector('#global-locale');
    locale.value = 'de';
    locale.dispatchEvent(new Event('change',{bubbles:true}));
  })()`);
  await wait(`document.querySelector('#global-locale')?.value === 'de' &&
    [...document.querySelectorAll('.nav-item')].some((item)=>item.textContent.includes('Übersicht')) &&
    document.querySelector('#plugin-ui-generic-controls button[type=submit]')?.textContent.includes('Einstellungen speichern')`,
    "locale change translated shell and generic controls");
  check(await evaluate(`document.querySelector('#plugin-ui-generic-controls legend')?.textContent === 'Strength' &&
    document.querySelector('[data-plugin-ui-qualification] button[aria-label="Say hello"]') !== null`),
    "locale change overwrote provider-owned label or removed the live extension panel");

  await evaluate(`document.querySelector('#plugin-ui-generic-controls button[type=submit]').focus()`);
  await key("Enter", "Enter", 13);
  await wait("window.__genericControlSave?.strength === 11", "keyboard-saved generic plugin setting");

  await evaluate("location.hash = 'overview'");
  await wait("location.hash === '#overview' && window.__pluginUiQualificationCleanups === 1",
    "navigation disposal hook");
  check(await evaluate(`!document.querySelector('[data-plugin-ui-qualification]') &&
    !document.querySelector('#plugin-ui-generic-controls')`),
  "navigation left extension or generic-control DOM mounted");

  await evaluate("location.hash = 'extension/plugin-ui-qualification'");
  await wait("window.__pluginUiQualificationMounts === 2 && !!document.querySelector('[data-plugin-ui-qualification]')",
    "extension remount after navigation");
  await evaluate(`document.querySelector('[data-plugin-ui-qualification] button[aria-label="Say hello"]').focus()`);
  await key("Enter", "Enter", 13);
  await wait("document.querySelector('[data-plugin-ui-qualification] [role=status]')?.textContent === 'Hello, keyboard!'",
    "remounted extension keyboard action");
  await evaluate("location.hash = 'overview'");
  await wait("window.__pluginUiQualificationCleanups === 2", "second extension disposal");

  console.log("PASS: signed web asset delivery, extension keyboard use and host call, generic control keyboard/save, locale update, navigation cleanup/remount");
} finally {
  if (target) {
    try { await debuggerClient.command("Target.closeTarget", { targetId: target.targetId }); } catch {}
  }
  debuggerClient.close();
}
