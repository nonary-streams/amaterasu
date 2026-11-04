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
const transpile = source => ts.transpile(source, { target: ts.ScriptTarget.ES2022 });
const transpiledUi = transpile(await readFile("src/ui.ts", "utf8"));
const transpiledExtensionUi = transpile(await readFile("src/extension_ui.ts", "utf8"));
const transpiledFeedback = transpile(await readFile("src/extension_ui_feedback.ts", "utf8"));

function html() {
  return `<!doctype html><meta charset="utf-8"><main id="mount"></main>
<script>
window.onerror = (message,source,line,column,error) => { window.__loadError = error?.stack ?? String(message); };
window.Amaterasu = {
  localizedTextNode: source => document.createTextNode(window.Amaterasu.translateText(source)),
  localeFormattedTextNode: format => document.createTextNode(format(navigator.language)),
  literalText: value => ({kind:"literal", value}),
  textValueNode: value => document.createTextNode(typeof value === "string" ? value : value.value),
  translateText: value => value,
};
Object.assign(window, {
  el: (...args) => window.Amaterasu.el(...args),
  input: (...args) => window.Amaterasu.input(...args),
  createExtensionUiV1: (...args) => window.Amaterasu.createExtensionUiV1(...args),
  localizedTextNode: source => window.Amaterasu.localizedTextNode(source),
  localeFormattedTextNode: format => window.Amaterasu.localeFormattedTextNode(format),
  translateText: source => window.Amaterasu.translateText(source),
});
</script>
<script>${transpiledUi}</script>
<script>${transpiledExtensionUi}</script>
<script>${transpiledFeedback}</script>
<script>
window.__mount = document.querySelector("#mount");
window.__controller = new AbortController();
window.__ui = Amaterasu.createExtensionUiWithFeedbackV1(window.__controller.signal);
window.__reset = () => {
  window.__controller.abort();
  window.__controller = new AbortController();
  window.__ui = Amaterasu.createExtensionUiWithFeedbackV1(window.__controller.signal);
  window.__mount.replaceChildren();
};
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
  const profile = await mkdtemp(path.join(os.tmpdir(), "amaterasu-feedback-chromium-"));
  const process = spawn(chromiumPath, [
    "--headless", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
    `--user-data-dir=${profile}`, "--remote-debugging-port=0", url,
  ], {stdio:"ignore"});
  try {
    const activePortFile = path.join(profile, "DevToolsActivePort");
    for (let attempt = 0; attempt < 120 && !existsSync(activePortFile); attempt++) {
      if (process.exitCode !== null) throw new Error(`Chromium exited with ${process.exitCode}`);
      await tick();
    }
    const [rawPort] = (await readFile(activePortFile, "utf8")).split(/\r?\n/);
    const port = Number(rawPort);
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
      if (await cdp.evaluate("Boolean(window.__ui?.progress || window.__loadError)")) { const error = await cdp.evaluate("window.__loadError || null"); if (error) throw new Error(error); return {server, process, profile, cdp}; }
      await tick();
    }
    throw new Error("Feedback browser harness did not load.");
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

test("Chromium renders status and alert feedback as safe live text with their required semantics", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset();
    window.__status = window.__ui.status({message:"<img src=x onerror=window.__xss=true>"});
    window.__alert = window.__ui.alert({message:"Heads up",severity:"warning"});
    window.__mount.append(window.__status.element,window.__alert.element); true`);

  assert.deepEqual(await cdp.evaluate(`(()=>{const e=window.__status.element; return {role:e.getAttribute('role'),live:e.getAttribute('aria-live'),atomic:e.getAttribute('aria-atomic'),busy:e.getAttribute('aria-busy'),text:e.textContent,images:e.querySelectorAll('img').length}})()`), {
    role:"status", live:"polite", atomic:"true", busy:"false", text:"<img src=x onerror=window.__xss=true>", images:0,
  });
  assert.deepEqual(await cdp.evaluate(`(()=>{const e=window.__alert.element; return {role:e.getAttribute('role'),live:e.getAttribute('aria-live'),atomic:e.getAttribute('aria-atomic'),severity:e.dataset.severity}})()`), {
    role:"alert", live:"assertive", atomic:"true", severity:"warning",
  });
  assert.equal(await cdp.evaluate("window.__xss === true"), false);

  await cdp.evaluate(`window.__status.setMessage("Working"); window.__status.setBusy(true);
    window.__alert.setMessage("Connection lost. Retry required."); window.__alert.setSeverity("error"); true`);
  assert.deepEqual(await cdp.evaluate(`({status:window.__status.element.textContent,busy:window.__status.element.getAttribute('aria-busy'),alert:window.__alert.element.textContent,severity:window.__alert.element.dataset.severity})`), {
    status:"Working", busy:"true", alert:"Connection lost. Retry required.", severity:"error",
  });

  await cdp.evaluate(`window.__status.dispose(); window.__status.setMessage("Disposed update");
    window.__status.setMessage({source:""}); window.__status.setBusy("invalid");
    window.__alert.dispose(); window.__alert.setMessage(17); window.__alert.setSeverity("critical"); true`);
  assert.equal(await cdp.evaluate("window.__status.element.textContent"), "Working");
  assert.equal(await cdp.evaluate("window.__alert.element.textContent"), "Connection lost. Retry required.");
});

test("Chromium exposes determinate and indeterminate native progress and blocks updates after abort", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset();
    window.__status = window.__ui.status({message:"Before abort"});
    window.__alert = window.__ui.alert({message:"Before abort"});
    window.__determinate = window.__ui.progress({label:"Upload",value:-2,maximum:8,message:"Starting"});
    window.__indeterminate = window.__ui.progress({label:"Scanning",value:null});
    window.__mount.append(window.__status.element,window.__alert.element,window.__determinate.element,window.__indeterminate.element); true`);

  assert.deepEqual(await cdp.evaluate(`(()=>{const p=window.__determinate.element.querySelector('progress'); return {tag:p.tagName,value:p.value,max:p.max,valueAttribute:p.getAttribute('value'),label:window.__determinate.element.querySelector('label').textContent,description:p.getAttribute('aria-describedby'),message:window.__determinate.element.querySelector('small').textContent}})()`), {
    tag:"PROGRESS", value:0, max:8, valueAttribute:"0", label:"Upload", description:await cdp.evaluate("window.__determinate.element.querySelector('progress').getAttribute('aria-describedby')"), message:"Starting",
  });
  assert.deepEqual(await cdp.evaluate(`(()=>{const p=window.__indeterminate.element.querySelector('progress'); return {tag:p.tagName,value:p.value,max:p.max,valueAttribute:p.getAttribute('value')}})()`), {
    tag:"PROGRESS", value:0, max:100, valueAttribute:null,
  });
  assert.equal(await cdp.evaluate("window.__determinate.value()"), 0);
  assert.equal(await cdp.evaluate("window.__indeterminate.value()"), null);

  await cdp.evaluate(`window.__determinate.setValue(99); window.__determinate.setMessage("Nearly finished");
    window.__indeterminate.setValue(30); window.__indeterminate.setMessage("30 percent"); true`);
  assert.deepEqual(await cdp.evaluate(`(()=>{const a=window.__determinate.element.querySelector('progress'); const b=window.__indeterminate.element.querySelector('progress'); return {a:a.value,aAttr:a.getAttribute('value'),aMessage:window.__determinate.element.querySelector('small').textContent,b:b.value,bAttr:b.getAttribute('value'),bMessage:window.__indeterminate.element.querySelector('small').textContent}})()`), {
    a:8, aAttr:"8", aMessage:"Nearly finished", b:30, bAttr:"30", bMessage:"30 percent",
  });
  await cdp.evaluate(`window.__indeterminate.setValue(null); window.__indeterminate.setMessage(null); true`);
  assert.deepEqual(await cdp.evaluate(`(()=>{const p=window.__indeterminate.element.querySelector('progress'); return {value:p.value,valueAttribute:p.getAttribute('value'),description:p.getAttribute('aria-describedby'),messageCount:window.__indeterminate.element.querySelectorAll('small').length}})()`), {
    value:0, valueAttribute:null, description:null, messageCount:0,
  });

  await cdp.evaluate(`window.__controller.abort();
    window.__status.setMessage("After abort"); window.__status.setMessage({source:""}); window.__status.setBusy("invalid");
    window.__alert.setMessage("After abort"); window.__alert.setSeverity("critical");
    window.__determinate.setValue(3); window.__determinate.setValue(Infinity);
    window.__determinate.setMessage("After abort"); window.__determinate.setMessage({source:""}); true`);
  assert.deepEqual(await cdp.evaluate(`({status:window.__status.element.textContent,alert:window.__alert.element.textContent,value:window.__determinate.value(),renderedValue:window.__determinate.element.querySelector('progress').value,message:window.__determinate.element.querySelector('small').textContent})`), {
    status:"Before abort", alert:"Before abort", value:8, renderedValue:8, message:"Nearly finished",
  });

  await cdp.evaluate(`window.__reset(); window.__changes=0;
    window.__toggle=window.__ui.toggle({label:"Lifecycle",value:false,onChange:()=>window.__changes++});
    window.__mount.append(window.__toggle.element); true`);
  await cdp.evaluate(`(()=>{const input=window.__toggle.element.querySelector('input'); input.checked=true; input.dispatchEvent(new Event('change',{bubbles:true})); return true})()`);
  assert.equal(await cdp.evaluate("window.__changes"), 1);
  await cdp.evaluate("window.__controller.abort(); true");
  await cdp.evaluate(`(()=>{const input=window.__toggle.element.querySelector('input'); input.checked=false; input.dispatchEvent(new Event('change',{bubbles:true})); return true})()`);
  assert.equal(await cdp.evaluate("window.__changes"), 1);
});


test("Chromium allocates stable unique progress description IDs in creation order", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset();
    window.__firstProgress=window.__ui.progress({label:"First task",value:1});
    window.__secondProgress=window.__ui.progress({label:"Second task",value:2});
    window.__mount.append(window.__firstProgress.element,window.__secondProgress.element);
    window.__firstProgress.setMessage("First description");
    window.__secondProgress.setMessage("Second description"); true`);
  const ids = await cdp.evaluate(`(()=>{
    const first=window.__firstProgress.element.querySelector("progress");
    const second=window.__secondProgress.element.querySelector("progress");
    const firstId=first.getAttribute("aria-describedby");
    const secondId=second.getAttribute("aria-describedby");
    return {firstId,secondId,firstDescription:document.getElementById(firstId)?.textContent,secondDescription:document.getElementById(secondId)?.textContent,firstCount:document.querySelectorAll('[id="'+firstId+'"]').length,secondCount:document.querySelectorAll('[id="'+secondId+'"]').length};
  })()`);
  assert.notEqual(ids.firstId, ids.secondId);
  assert.equal(ids.firstCount, 1);
  assert.equal(ids.secondCount, 1);
  assert.equal(ids.firstDescription, "First description");
  assert.equal(ids.secondDescription, "Second description");
  const before = await cdp.evaluate("window.__firstProgress.element.querySelector('progress').getAttribute('aria-describedby')");
  await cdp.evaluate(`window.__ui.progress({label:"Third task",value:0,message:"Third description"}); window.__firstProgress.setMessage("Updated first description"); true`);
  assert.equal(await cdp.evaluate("window.__firstProgress.element.querySelector('progress').getAttribute('aria-describedby')"), before);
  assert.equal(await cdp.evaluate(`document.getElementById("${ids.firstId}").textContent`), "Updated first description");
});
