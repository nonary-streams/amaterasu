import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { createServer } from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import ts from "typescript";

const chromiumPath = "/usr/bin/chromium";
const tick = () => new Promise(resolve => setTimeout(resolve, 30));
const transpiledPicker = ts.transpile(
  await readFile("src/steam_exclusion_picker.ts", "utf8"),
  { target: ts.ScriptTarget.ES2022 },
);

function html() {
  return `<!doctype html><meta charset="utf-8"><form id="settings"><main id="mount"></main><button id="save" type="submit">Save settings</button></form><button id="outside">Outside</button>
<script>
window.__saveRequests = 0;
document.querySelector("#settings").addEventListener("submit", event => { event.preventDefault(); window.__saveRequests++; });
window.Amaterasu = {
  localizedMessage: value => ({kind:"localized", value}),
  literalText: value => ({kind:"literal", value}),
  textValueNode: value => document.createTextNode(typeof value === "string" ? value : value.value),
  translateText: value => window.__translations?.[value] ?? value,
  setLocalizedTextContent(target, value) { target.replaceChildren(document.createTextNode(this.translateText(value))); target.dataset.localeText=value; },
  setLocalizedAttribute(target, name, value) { target.setAttribute(name,value); target.setAttribute("data-locale-"+name,value); },
  setLocalizedAttributeTemplate(target, name, source, values) {
    target.setAttribute(name, source.replace(/\\{(\\w+)\\}/g, (_, key) => values[key].value));
    target.setAttribute("data-locale-"+name,source);
  },
  localeFormattedTextNode(format) { return document.createTextNode(format(navigator.language)); },
};
${transpiledPicker}
window.__pendingLists = [];
window.__api = {
  permissions: new Set(["steam.read"]),
  allows(scope) { return this.permissions.has(scope); },
  post(route, params) {
    if (route !== "steam/list") throw new Error("unexpected route: " + route);
    return new Promise((resolve, reject) => this && window.__pendingLists.push({params, resolve, reject}));
  },
};
window.__createPicker = initial => {
  const picker = Amaterasu.steamExclusionPicker(window.__api, initial);
  document.querySelector("#mount").replaceChildren(picker.node);
  window.__picker = picker;
  return picker;
};
window.__resolveList = (index, reply) => window.__pendingLists[index].resolve(reply);
</script>`;
}

class DevTools {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 0;
    this.pending = new Map();
    socket.addEventListener("message", event => {
      const message = JSON.parse(event.data);
      if (!message.id) return;
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
  }
  static async connect(url) {
    const socket = new WebSocket(url);
    await new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve, { once: true });
      socket.addEventListener("error", reject, { once: true });
    });
    return new DevTools(socket);
  }
  command(method, params = {}) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, {resolve, reject});
      this.socket.send(JSON.stringify({id, method, params}));
    });
  }
  async evaluate(expression) {
    const result = await this.command("Runtime.evaluate", {
      expression, awaitPromise: true, returnByValue: true,
    });
    if (result.exceptionDetails)
      throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result?.value;
  }
  close() { this.socket.close(); }
}

async function startBrowser() {
  const server = createServer((request, response) => {
    response.writeHead(200, {"content-type":"text/html; charset=utf-8"});
    response.end(html());
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}/`;
  const profile = await mkdtemp(path.join(os.tmpdir(), "amaterasu-chromium-"));
  const process = spawn(chromiumPath, [
    "--headless", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
    `--user-data-dir=${profile}`, "--remote-debugging-port=0", url,
  ], {stdio:"ignore"});
  let port;
  try {
    const activePortFile = path.join(profile, "DevToolsActivePort");
    for (let attempt = 0; attempt < 120 && !existsSync(activePortFile); attempt++) {
      if (process.exitCode !== null) throw new Error(`Chromium exited with ${process.exitCode}`);
      await tick();
    }
    const [rawPort] = (await readFile(activePortFile, "utf8")).split(/\r?\n/);
    port = Number(rawPort);
    let targets = [];
    for (let attempt = 0; attempt < 60; attempt++) {
      const response = await fetch(`http://127.0.0.1:${port}/json`);
      targets = await response.json();
      if (targets.some(target => target.type === "page")) break;
      await tick();
    }
    const page = targets.find(target => target.type === "page");
    if (!page) throw new Error("Chromium did not create a page target.");
    const cdp = await DevTools.connect(page.webSocketDebuggerUrl);
    await cdp.command("Page.enable");
    await cdp.command("Runtime.enable");
    await cdp.command("Page.navigate", {url});
    for (let attempt = 0; attempt < 100; attempt++) {
      if (await cdp.evaluate("Boolean(window.__createPicker)")) return {server, process, profile, cdp};
      await tick();
    }
    throw new Error("Picker browser harness did not load.");
  } catch (error) {
    process.kill("SIGTERM");
    for (let attempt = 0; attempt < 40 && process.exitCode === null; attempt++) await tick();
    server.close();
    await rm(profile, {recursive:true, force:true, maxRetries: 5, retryDelay: 50});
    throw error;
  }
}

let browser;
test.before(async t => {
  if (!existsSync(chromiumPath)) {
    t.skip(`Chromium is required for real-browser tests (${chromiumPath} not found).`);
    return;
  }
  browser = await startBrowser();
});
test.after(async () => {
  if (!browser) return;
  browser.cdp.close();
  browser.process.kill("SIGTERM");
  for (let attempt = 0; attempt < 40 && browser.process.exitCode === null; attempt++) await tick();
  browser.server.close();
  await rm(browser.profile, {recursive:true, force:true, maxRetries: 5, retryDelay: 50});
});

test("Chromium preserves focus and disabled state through picker rerenders and delayed discovery", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__createPicker([{id:"999",name:"Unavailable"}]); true`);
  await cdp.evaluate(`window.__picker.setDisabled(true); true`);
  assert.equal(await cdp.evaluate(`document.querySelector(".steam-exclusion-manual-add button").disabled`), true);
  await cdp.evaluate(`window.__resolveList(0,{revision:1,games:[{app_id:22,name:"Portal",installed:true}],next_after:32,scanning:false,complete:false}); true`);
  await tick();
  assert.equal(await cdp.evaluate(`document.querySelector(".steam-exclusion-option input").disabled`), true);
  assert.equal(await cdp.evaluate(`document.querySelector(".steam-exclusion-saved input").disabled`), true);
  await cdp.evaluate(`window.__picker.setDisabled(false); true`);

  assert.equal(await cdp.evaluate(`(()=>{const input=document.querySelector('.steam-exclusion-option input'); input.focus(); input.click(); return document.activeElement.dataset.focusKey})()`), "game-22");
  assert.equal(await cdp.evaluate(`document.querySelector('.steam-exclusion-warning').textContent.includes('incomplete')`), true);

  const savedNameFocus = `(()=>{const name=document.querySelector('.steam-exclusion-saved input[aria-label="Edit excluded Steam game name"]'); name.focus(); name.setSelectionRange(1,3); document.querySelector('.steam-exclusion-picker > button').click(); return true})()`;
  await cdp.evaluate(savedNameFocus);
  await cdp.evaluate(`window.__resolveList(1,{revision:1,games:[{app_id:33,name:"Hades",installed:true}],next_after:64,scanning:false,complete:false}); true`);
  await tick();
  assert.equal(await cdp.evaluate(`document.activeElement.dataset.focusKey`), "saved-0-name");
  assert.deepEqual(await cdp.evaluate(`({start:document.activeElement.selectionStart,end:document.activeElement.selectionEnd})`), {start:1,end:3});

  // A second page remains pending while focus moves outside. Completion must not steal it.
  await cdp.evaluate(`document.querySelector('.steam-exclusion-picker > button').click(); document.querySelector('#outside').focus(); true`);
  await cdp.evaluate(`window.__resolveList(2,{revision:1,games:[],next_after:null,scanning:false,complete:true}); true`);
  await tick();
  assert.equal(await cdp.evaluate(`document.activeElement.id`), "outside");
  assert.equal(await cdp.evaluate(`document.querySelector('.steam-exclusion-warning').hidden`), true);
  assert.equal(await cdp.evaluate(`document.querySelector('.steam-exclusion-warning').textContent`), "");
});

test("Chromium permits manual exclusions without discovery and omits cleared optional names", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__picker.dispose(); window.__pendingLists=[]; window.__api.permissions.clear(); window.__createPicker([]); true`);
  assert.equal(await cdp.evaluate(`document.querySelector('.steam-exclusion-picker [role=status]').textContent.includes('steam.read')`), true);
  await cdp.evaluate(`document.querySelector('[aria-label="Steam app ID to exclude"]').value='42'; document.querySelector('[aria-label="Steam game name to exclude"]').value='Optional'; document.querySelector('.steam-exclusion-manual-add button').click(); true`);
  assert.deepEqual(await cdp.evaluate(`window.__picker.value()`), [{id:"42",name:"Optional"}]);
  assert.equal(await cdp.evaluate(`window.__saveRequests`), 0);
  await cdp.evaluate(`(()=>{const name=document.querySelector('.steam-exclusion-saved input[aria-label="Edit excluded Steam game name"]'); name.value=''; name.dispatchEvent(new Event('input',{bubbles:true})); return true})()`);
  assert.deepEqual(await cdp.evaluate(`window.__picker.value()`), [{id:"42"}]);
  await cdp.evaluate(`(()=>{const name=document.querySelector('[aria-label="Steam game name to exclude"]'); name.value='Name from Enter'; name.focus(); return true})()`);
  await cdp.command("Input.dispatchKeyEvent", {type:"keyDown", key:"Enter", code:"Enter", windowsVirtualKeyCode:13});
  await cdp.command("Input.dispatchKeyEvent", {type:"keyUp", key:"Enter", code:"Enter", windowsVirtualKeyCode:13});
  await tick();
  assert.deepEqual(await cdp.evaluate(`window.__picker.value()`), [{id:"42"},{name:"Name from Enter"}]);
  assert.equal(await cdp.evaluate(`window.__saveRequests`), 0);
  assert.equal(await cdp.evaluate(`window.__pendingLists.length`), 0);
});


test("Chromium focuses manual entry after removing the last row and translates combined warnings", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__picker.dispose(); window.__pendingLists=[]; window.__api.permissions.add("steam.read"); window.__translations={"Steam scan is incomplete; existing exclusions are preserved.":"FR: analyse incomplète", "The Steam list changed while paging; restarted at the latest revision.":"FR: reprise après changement"}; window.__createPicker([{id:"91",name:"Only row"}]); true`);
  await cdp.evaluate(`window.__resolveList(0,{revision:1,games:[],next_after:32,scanning:false,complete:false}); true`);
  await tick();
  await cdp.evaluate(`document.querySelector('.steam-exclusion-saved button').focus(); document.querySelector('.steam-exclusion-saved button').click(); true`);
  assert.equal(await cdp.evaluate(`document.activeElement.dataset.focusKey`), "manual-id");
  await cdp.evaluate(`document.querySelector('.steam-exclusion-picker > button').click(); true`);
  await cdp.evaluate(`window.__resolveList(1,{revision:2,games:[],next_after:64,scanning:false,complete:false}); true`);
  await tick();
  assert.equal(await cdp.evaluate(`window.__pendingLists.length`), 3);
  await cdp.evaluate(`window.__resolveList(2,{revision:2,games:[],next_after:null,scanning:false,complete:false}); true`);
  await tick();
  assert.equal(await cdp.evaluate(`document.querySelector('.steam-exclusion-warning').textContent`), "FR: analyse incomplète FR: reprise après changement");
});
