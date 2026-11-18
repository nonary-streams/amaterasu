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
const tick = () => new Promise(resolve => setTimeout(resolve, 40));
const sourceFiles = ["ui.ts", "extension_ui.ts", "extension_ui_elements.ts", "extension_ui_layout.ts", "extension_ui_feedback.ts", "extension_ui_dialog.ts", "extension_ui_charts.ts"];
const sources = await Promise.all(sourceFiles.map(file => readFile(`src/${file}`, "utf8")));
const transpiled = sources.map(source => ts.transpile(source, { target: ts.ScriptTarget.ES2022 })).join("\n");
const stylesheet = await readFile("public/styles.css", "utf8");

function html() {
  return `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><style>${stylesheet}</style><main id="mount"></main>
<script>window.onerror=(message,source,line,column,error)=>{window.__loadError=error?.stack ?? String(message)};</script>
<script>
window.Amaterasu = {
  localizedTextNode: source => document.createTextNode(window.Amaterasu.translateText(source)),
  localeFormattedTextNode: format => document.createTextNode(format(navigator.language)),
  translateText: source => source,
};
</script>
<script>${transpiled}</script>
<script>
window.createExtensionUiV1 = (...args) => Amaterasu.createExtensionUiV1(...args);
window.createExtensionUiWithFeedbackV1 = (...args) => Amaterasu.createExtensionUiWithFeedbackV1(...args);
window.el = (...args) => Amaterasu.el(...args);
window.button = (...args) => Amaterasu.button(...args);
window.localizedTextNode = source => Amaterasu.localizedTextNode(source);
window.localeFormattedTextNode = format => Amaterasu.localeFormattedTextNode(format);
window.translateText = source => Amaterasu.translateText(source);
window.__mount = document.querySelector("#mount");
window.__controller = new AbortController();
window.__ui = Amaterasu.createExtensionUiWithDialogsV1(window.__controller.signal);
window.__reset = () => {
  window.__controller.abort();
  window.__controller = new AbortController();
  window.__ui = Amaterasu.createExtensionUiWithDialogsV1(window.__controller.signal);
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
    const result = await this.command("Runtime.evaluate", {expression, awaitPromise:true, returnByValue:true});
    if (result.exceptionDetails)
      throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result?.value;
  }
  close() { this.socket.close(); }
}

async function startBrowser() {
  const server = createServer((_request, response) => {
    response.writeHead(200, {"content-type":"text/html; charset=utf-8"});
    response.end(html());
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${server.address().port}/`;
  const profile = await mkdtemp(path.join(os.tmpdir(), "amaterasu-dialog-chromium-"));
  const process = spawn(chromiumPath, ["--headless", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage", `--user-data-dir=${profile}`, "--remote-debugging-port=0", url], {stdio:"ignore"});
  try {
    const portFile = path.join(profile, "DevToolsActivePort");
    for (let attempt = 0; attempt < 120 && !existsSync(portFile); attempt++) {
      if (process.exitCode !== null) throw new Error(`Chromium exited with ${process.exitCode}`);
      await tick();
    }
    const [rawPort] = (await readFile(portFile, "utf8")).split(/\r?\n/);
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
      if (await cdp.evaluate("Boolean(window.__ui?.dialog)")) return {server, process, profile, cdp};
      await tick();
    }
    throw new Error(`Dialog browser harness did not load: ${await cdp.evaluate("window.__loadError ?? 'no window error'")}`);
  } catch (error) {
    process.kill("SIGTERM");
    server.close();
    await rm(profile, {recursive:true, force:true, maxRetries:5, retryDelay:50});
    throw error;
  }
}

async function press(cdp, key, code, windowsVirtualKeyCode) {
  await cdp.command("Input.dispatchKeyEvent", {type:"keyDown", key, code, windowsVirtualKeyCode});
  await cdp.command("Input.dispatchKeyEvent", {type:"keyUp", key, code, windowsVirtualKeyCode});
  await tick();
}

async function waitForCloseCallbacks(cdp, minimum) {
  for (let attempt = 0; attempt < 60; attempt++) {
    const reasons = await cdp.evaluate("window.__reasons");
    if (reasons.length >= minimum) return reasons;
    await tick();
  }
  throw new Error("Chromium did not dispatch the expected dialog close events.");
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
  await rm(browser.profile, {recursive:true, force:true, maxRetries:5, retryDelay:50});
});

test("Chromium provides accessible modal semantics, safe content, focus containment and cancel behavior", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__opener=document.createElement("button"); window.__opener.textContent="Open dialog"; window.__mount.append(window.__opener);
    window.__input=document.createElement("input"); window.__input.setAttribute("aria-label","Project name");
    window.__dialog=window.__ui.dialog({title:"Rename project",description:"Enter a new name.",content:["<img src=x onerror=window.__xss=true>",window.__input],initialFocus:window.__input,onClose:reason=>window.__reasons.push(reason)});
    window.__dialog.element.id="rename-dialog"; window.__mount.append(window.__dialog.element);
    window.__opener.focus(); window.__dialog.open(); true`);

  assert.deepEqual(await cdp.evaluate(`(()=>{const d=window.__dialog.element; return {tag:d.tagName,open:d.open,role:d.getAttribute('role'),modal:d.getAttribute('aria-modal'),label:d.getAttribute('aria-labelledby'),title:d.querySelector('h2').textContent,description:d.getAttribute('aria-describedby'),descriptionText:d.querySelector('p').textContent,focus:document.activeElement===window.__input,maliciousText:d.textContent.includes('<img src=x onerror=window.__xss=true>'),imageCount:d.querySelectorAll('img').length}})()`), {
    tag:"DIALOG", open:true, role:null, modal:"true", label:await cdp.evaluate("window.__dialog.element.querySelector('h2').id"), title:"Rename project", description:await cdp.evaluate("window.__dialog.element.querySelector('p').id"), descriptionText:"Enter a new name.", focus:true, maliciousText:true, imageCount:0,
  });
  assert.equal(await cdp.evaluate("document.getElementById(window.__dialog.element.getAttribute('aria-labelledby')).textContent"), "Rename project");
  assert.equal(await cdp.evaluate("document.getElementById(window.__dialog.element.getAttribute('aria-describedby')).textContent"), "Enter a new name.");
  assert.equal(await cdp.evaluate("window.__xss === true"), false);

  await press(cdp, "Tab", "Tab", 9);
  assert.equal(await cdp.evaluate("window.__dialog.element.contains(document.activeElement)"), true);
  await press(cdp, "Tab", "Tab", 9);
  assert.equal(await cdp.evaluate("window.__dialog.element.contains(document.activeElement)"), true, await cdp.evaluate("document.activeElement.outerHTML"));
  await cdp.evaluate(`window.__dialog.element.querySelector('.plugin-dialog-actions button').click(); true`);
  await tick();
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), false);
  assert.equal(await cdp.evaluate("document.activeElement === window.__opener"), true);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["cancel"]);

  await cdp.evaluate("window.__dialog.open(); true");
  await press(cdp, "Escape", "Escape", 27);
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), false);
  assert.equal(await cdp.evaluate("document.activeElement === window.__opener"), true);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["cancel", "cancel"]);
});

test("Chromium focuses the safe default, restores focus on close/dispose, and lifecycle abort cleans up", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__opener=document.createElement("button"); window.__opener.textContent="Open"; window.__mount.append(window.__opener);
    window.__dialog=window.__ui.dialog({title:"Default focus",content:"Plain text",onClose:reason=>window.__reasons.push(reason)});
    window.__mount.append(window.__dialog.element); window.__opener.focus(); window.__dialog.open(); true`);
  assert.equal(await cdp.evaluate("document.activeElement === window.__dialog.element.querySelector('.plugin-dialog-actions button')"), true);
  await cdp.evaluate("window.__dialog.close(); true");
  await tick();
  assert.equal(await cdp.evaluate("document.activeElement === window.__opener"), true);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["close"]);

  await cdp.evaluate("window.__dialog.open(); window.__dialog.dispose(); true");
  assert.equal(await cdp.evaluate("window.__dialog.element.isConnected"), false);
  assert.equal(await cdp.evaluate("document.activeElement === window.__opener"), true);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["close", "dispose"]);
  await cdp.evaluate("window.__dialog.dispose(); window.__dialog.open(); window.__dialog.close(); true");

  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__opener=document.createElement("button"); window.__mount.append(window.__opener);
    window.__dialog=window.__ui.dialog({title:"Abort focus",content:"Work",onClose:reason=>window.__reasons.push(reason)});
    window.__mount.append(window.__dialog.element); window.__opener.focus(); window.__dialog.open();
    window.__opener.remove(); window.__controller.abort(); true`);
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), false);
  assert.equal(await cdp.evaluate("window.__dialog.element.isConnected"), false);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), []);
  assert.equal(await cdp.evaluate("document.activeElement === document.body"), true);
  await cdp.evaluate("window.__dialog.open(); window.__dialog.close(); window.__dialog.dispose(); true");
});

test("Chromium rejects initial focus outside content and nodes from another document", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate("window.__reset(); true");
  const result = await cdp.evaluate(`(()=>{
    const outside=document.createElement('button');
    let focusError=''; let documentError='';
    try { window.__ui.dialog({title:'Bad focus',content:'Text',initialFocus:outside}); } catch (error) { focusError=error.message; }
    const foreign=document.implementation.createHTMLDocument('foreign').createElement('p');
    try { window.__ui.dialog({title:'Foreign node',content:foreign}); } catch (error) { documentError=error.message; }
    return {focusError,documentError};
  })()`);
  assert.match(result.focusError, /inside its content/);
  assert.match(result.documentError, /owned by this document/);
});

test("Chromium mounts inline footer actions with native keyboard behavior and keeps Cancel as the default", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.command("Emulation.setDeviceMetricsOverride", {width:320,height:640,deviceScaleFactor:1,mobile:false});
  const footer = await cdp.evaluate(`(() => {
    window.__reset();
    window.__footerCloseReasons = [];
    window.__footerOpener = document.createElement("button");
    window.__footerOpener.textContent = "Edit profile";
    const confirm = window.__ui.button({ label:"Confirm", variant:"primary", onClick:() => dialog.close() });
    const dialog = window.__ui.dialog({
      title:"Confirm profile",
      content:"Apply your changes?",
      actions:[confirm.element],
      onClose:reason => window.__footerCloseReasons.push(reason),
    });
    window.__footerDialog = dialog;
    window.__mount.append(window.__footerOpener, dialog.element);
    window.__footerOpener.focus();
    dialog.open();
    const actions = dialog.element.querySelector(".plugin-dialog-actions");
    const [cancel, accept] = actions.querySelectorAll("button");
    return {
      labels:[cancel.textContent,accept.textContent],
      inline:Math.abs(cancel.getBoundingClientRect().top - accept.getBoundingClientRect().top) < 1,
      cancelFocused:document.activeElement === cancel,
      inBody:Boolean(dialog.element.querySelector(".plugin-dialog-content button")),
      footerFits:actions.scrollWidth <= actions.clientWidth,
    };
  })()`);
  assert.deepEqual(footer, {
    labels: ["Cancel", "Confirm"], inline: true, cancelFocused: true, inBody: false, footerFits: true,
  });
  await press(cdp, "Tab", "Tab", 9);
  assert.equal(await cdp.evaluate("document.activeElement.textContent"), "Confirm");
  await press(cdp, " ", "Space", 32);
  assert.equal(await cdp.evaluate("window.__footerDialog.element.open"), false);
  assert.equal(await cdp.evaluate("document.activeElement === window.__footerOpener"), true);
  assert.deepEqual(await cdp.evaluate("window.__footerCloseReasons"), ["close"]);

  const nativeFocus = await cdp.evaluate(`(() => {
    const action = document.createElement("button");
    action.type = "button";
    const wrapper = document.createElement("div");
    wrapper.append(action);
    const dialog = window.__ui.dialog({title:"Native action", actions:[wrapper], initialFocus:action});
    window.__mount.append(dialog.element);
    dialog.open();
    const focused = document.activeElement === action;
    dialog.dispose();
    return focused;
  })()`);
  assert.equal(nativeFocus, true);
  await cdp.command("Emulation.clearDeviceMetricsOverride");
});

test("Chromium rejects foreign or overlapping footer actions before moving supplied DOM", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  const result = await cdp.evaluate(`(() => {
    window.__reset();
    const content = document.createElement("div");
    const action = document.createElement("button");
    content.append(action);
    window.__mount.append(content);
    const foreign = document.implementation.createHTMLDocument("foreign").createElement("button");
    const errorFor = options => {
      try { window.__ui.dialog({ title:"Invalid actions", ...options }); }
      catch (error) { return error.message; }
      return "";
    };
    return {
      foreign:errorFor({actions:[foreign]}),
      overlap:errorFor({content,actions:[action]}),
      duplicate:errorFor({actions:[action,action]}),
      stillMounted:action.parentElement === content && content.parentElement === window.__mount,
    };
  })()`);
  assert.match(result.foreign, /actions must be elements owned by this document/);
  assert.match(result.overlap, /actions must be separate/);
  assert.match(result.duplicate, /actions must be separate/);
  assert.equal(result.stillMounted, true);
});

test("Chromium supports fragment initial focus and concurrent dialog focus restoration", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__outerOpener=document.createElement("button"); window.__outerOpener.textContent="Open first"; window.__mount.append(window.__outerOpener);
    window.__fragment=document.createDocumentFragment();
    window.__openSecond=document.createElement("button"); window.__openSecond.textContent="Open second"; window.__fragment.append(window.__openSecond);
    window.__first=window.__ui.dialog({title:"First modal",description:"First description",content:window.__fragment,initialFocus:window.__openSecond,onClose:reason=>window.__reasons.push("first:"+reason)});
    window.__secondInput=document.createElement("input"); window.__secondInput.setAttribute("aria-label","Second dialog input");
    window.__second=window.__ui.dialog({title:"Second modal",description:"Second description",content:window.__secondInput,initialFocus:window.__secondInput,onClose:reason=>window.__reasons.push("second:"+reason)});
    window.__mount.append(window.__first.element,window.__second.element);
    window.__openSecond.addEventListener("click",()=>window.__second.open());
    window.__outerOpener.focus(); window.__first.open(); true`);
  assert.equal(await cdp.evaluate("document.activeElement === window.__openSecond"), true);
  assert.equal(await cdp.evaluate("window.__first.element.contains(window.__openSecond)"), true);
  await cdp.evaluate("window.__openSecond.click(); true");
  assert.equal(await cdp.evaluate("window.__second.element.open"), true);
  assert.equal(await cdp.evaluate("document.activeElement === window.__secondInput"), true);
  const ids = await cdp.evaluate(`({first:window.__first.element.getAttribute('aria-labelledby'),second:window.__second.element.getAttribute('aria-labelledby')})`);
  assert.notEqual(ids.first, ids.second);
  await cdp.evaluate("window.__second.close(); true");
  assert.deepEqual(await waitForCloseCallbacks(cdp, 1), ["second:close"]);
  assert.equal(await cdp.evaluate("document.activeElement === window.__openSecond"), true);
  await cdp.evaluate("window.__first.close(); true");
  assert.deepEqual(await waitForCloseCallbacks(cdp, 2), ["second:close", "first:close"]);
  assert.equal(await cdp.evaluate("document.activeElement === window.__outerOpener"), true);
});

test("Chromium correlates native and method=dialog closes before allowing immediate reopen", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__opener=document.createElement("button"); window.__mount.append(window.__opener);
    window.__native=window.__ui.dialog({title:"Native close",content:"Text",onClose:reason=>window.__reasons.push("native:"+reason)});
    window.__mount.append(window.__native.element); window.__opener.focus(); window.__native.open();
    window.__native.element.close("native-return"); window.__native.open(); true`);
  assert.equal(await cdp.evaluate("window.__native.element.open"), false);
  await tick();
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["native:close"]);
  await cdp.evaluate("window.__native.open(); true");
  assert.equal(await cdp.evaluate("window.__native.element.open"), true);
  await cdp.evaluate("window.__native.close(); true");
  await tick();
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["native:close", "native:close"]);

  await cdp.evaluate(`window.__form=document.createElement("form"); window.__form.method="dialog";
    window.__submit=document.createElement("button"); window.__submit.type="submit"; window.__submit.value="accept"; window.__form.append(window.__submit);
    window.__formDialog=window.__ui.dialog({title:"Form close",content:window.__form,onClose:reason=>window.__reasons.push("form:"+reason)});
    window.__mount.append(window.__formDialog.element); window.__opener.focus(); window.__formDialog.open();
    window.__form.requestSubmit(window.__submit); window.__formDialog.open(); true`);
  assert.equal(await cdp.evaluate("window.__formDialog.element.open"), false);
  await tick();
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["native:close", "native:close", "form:close"]);
  assert.equal(await cdp.evaluate("window.__formDialog.element.returnValue"), "accept");
  await cdp.evaluate("window.__formDialog.open(); true");
  assert.equal(await cdp.evaluate("window.__formDialog.element.open"), true);
  await cdp.evaluate("window.__formDialog.dispose(); true");
});



test("Chromium preserves submit handlers, cancellation, and submitter formmethod overrides", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[]; window.__submits=0;
    window.__opener=document.createElement("button"); window.__mount.append(window.__opener);
    window.__form=document.createElement("form"); window.__form.method="dialog";
    window.__submit=document.createElement("button"); window.__submit.type="submit"; window.__submit.value="keep-open";
    window.__form.append(window.__submit);
    window.__cancelHandler=event=>{ window.__submits++; event.preventDefault(); };
    window.__form.addEventListener("submit", window.__cancelHandler);
    window.__dialog=window.__ui.dialog({title:"Canceled submit",content:window.__form,onClose:reason=>window.__reasons.push(reason)});
    window.__mount.append(window.__dialog.element); window.__opener.focus(); window.__dialog.open();
    window.__form.requestSubmit(window.__submit); true`);
  await tick();
  assert.equal(await cdp.evaluate("window.__submits"), 1);
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), true);
  assert.equal(await cdp.evaluate("window.__reasons.length"), 0);

  await cdp.evaluate(`window.__form.onsubmit=null;
    window.__form.addEventListener("submit", event=>event.preventDefault(), {once:true});
    window.__submit.setAttribute("formmethod","get"); window.__form.requestSubmit(window.__submit); true`);
  await tick();
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), true);
  assert.equal(await cdp.evaluate("window.__reasons.length"), 0);

  await cdp.evaluate(`window.__form.removeEventListener("submit", window.__cancelHandler);
    window.__form.method="get"; window.__submit.setAttribute("formmethod","dialog");
    window.__submit.value="override-return"; window.__form.requestSubmit(window.__submit); true`);
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), false);
  await tick();
  assert.equal(await cdp.evaluate("window.__dialog.element.returnValue"), "override-return");
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["close"]);
  await cdp.evaluate("window.__dialog.dispose(); true");
});

test("Chromium scopes method=dialog to the owning nested dialog and closes children with parents", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__outerOpener=document.createElement("button"); window.__mount.append(window.__outerOpener);
    window.__outerContent=document.createElement("div");
    window.__outer=window.__ui.dialog({title:"Outer dialog",content:window.__outerContent,onClose:reason=>window.__reasons.push("outer:"+reason)});
    window.__innerForm=document.createElement("form"); window.__innerForm.method="dialog";
    window.__innerSubmit=document.createElement("button"); window.__innerSubmit.type="submit"; window.__innerSubmit.value="inner-value";
    window.__innerForm.append(window.__innerSubmit);
    window.__inner=window.__ui.dialog({title:"Inner dialog",content:window.__innerForm,onClose:reason=>window.__reasons.push("inner:"+reason)});
    window.__outerContent.append(window.__inner.element); window.__mount.append(window.__outer.element);
    window.__outerOpener.focus(); window.__outer.open(); window.__inner.open();
    window.__innerForm.requestSubmit(window.__innerSubmit); true`);
  assert.equal(await cdp.evaluate("window.__outer.element.open"), true);
  assert.equal(await cdp.evaluate("window.__inner.element.open"), false);
  await tick();
  assert.equal(await cdp.evaluate("window.__inner.element.returnValue"), "inner-value");
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["inner:close"]);

  await cdp.evaluate("window.__inner.open(); window.__outer.close(); true");
  assert.equal(await cdp.evaluate("window.__outer.element.open"), false);
  assert.equal(await cdp.evaluate("window.__inner.element.open"), false);
  await tick();
  assert.equal(await cdp.evaluate("document.activeElement === window.__outerOpener"), true);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["inner:close", "inner:close", "outer:close"]);
  await cdp.evaluate("window.__inner.dispose(); window.__outer.dispose(); true");
});



test("Chromium keeps explicit closes pending when a canceled submit closes the dialog", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__opener=document.createElement("button"); window.__mount.append(window.__opener);
    window.__form=document.createElement("form"); window.__form.method="dialog";
    window.__submit=document.createElement("button"); window.__submit.type="submit"; window.__submit.value="submit-value";
    window.__form.append(window.__submit);
    window.__dialog=window.__ui.dialog({title:"Close during submit",content:window.__form,onClose:reason=>window.__reasons.push(reason)});
    window.__mount.append(window.__dialog.element); window.__opener.focus(); window.__dialog.open();
    window.__form.addEventListener("submit", event=>{event.preventDefault(); window.__dialog.element.close("plugin-value");});
    window.__form.requestSubmit(window.__submit); window.__dialog.open(); true`);
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), false);
  await tick();
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["close"]);
  assert.equal(await cdp.evaluate("window.__dialog.element.returnValue"), "plugin-value");
  assert.equal(await cdp.evaluate("document.activeElement === window.__opener"), true);
  await cdp.evaluate("window.__dialog.open(); true");
  assert.equal(await cdp.evaluate("window.__dialog.element.open"), true);
  await cdp.evaluate("window.__dialog.dispose(); true");
});

test("Chromium closes nested dialogs on native parent form close without stale focus suppression", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset(); window.__reasons=[];
    window.__outerOpener=document.createElement("button"); window.__mount.append(window.__outerOpener);
    window.__form=document.createElement("form"); window.__form.method="dialog";
    window.__submit=document.createElement("button"); window.__submit.type="submit"; window.__submit.value="parent-submit";
    window.__trigger=document.createElement("button"); window.__trigger.type="button"; window.__trigger.textContent="Open child";
    window.__form.append(window.__submit,window.__trigger);
    window.__outer=window.__ui.dialog({title:"Form parent",content:window.__form,onClose:reason=>window.__reasons.push("outer:"+reason)});
    window.__inner=window.__ui.dialog({title:"Child",content:"Child",onClose:reason=>window.__reasons.push("inner:"+reason)});
    window.__form.append(window.__inner.element); window.__mount.append(window.__outer.element);
    window.__trigger.addEventListener("click",()=>window.__inner.open());
    window.__outerOpener.focus(); window.__outer.open(); window.__inner.open();
    window.__form.requestSubmit(window.__submit); true`);
  assert.equal(await cdp.evaluate("window.__outer.element.open"), false);
  assert.equal(await cdp.evaluate("window.__inner.element.open"), false);
  await tick();
  assert.equal(await cdp.evaluate("window.__outer.element.returnValue"), "parent-submit");
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["outer:close", "inner:close"]);
  assert.equal(await cdp.evaluate("document.activeElement === window.__outerOpener"), true);

  // A later parent close must not leave a suppression flag on an already closed child.
  await cdp.evaluate("window.__outer.open(); window.__trigger.focus(); window.__inner.open(); window.__inner.close(); true");
  await tick();
  assert.equal(await cdp.evaluate("document.activeElement === window.__trigger"), true);
  await cdp.evaluate("window.__outer.close(); true");
  await tick();
  assert.equal(await cdp.evaluate("document.activeElement === window.__outerOpener"), true);
  await cdp.evaluate("window.__outer.open(); window.__trigger.focus(); window.__inner.open(); window.__inner.close(); true");
  await tick();
  assert.equal(await cdp.evaluate("document.activeElement === window.__trigger"), true);
  assert.deepEqual(await cdp.evaluate("window.__reasons"), ["outer:close", "inner:close", "inner:close", "outer:close", "inner:close"]);
  await cdp.evaluate("window.__inner.dispose(); window.__outer.dispose(); true");
});

test("Chromium always completes disposal when an onClose callback throws", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.evaluate(`window.__reset();
    window.__opener=document.createElement("button"); window.__mount.append(window.__opener);
    window.__throwing=window.__ui.dialog({title:"Throw on close",content:"Text",onClose(){throw new Error("close callback failed")}});
    window.__mount.append(window.__throwing.element); window.__opener.focus(); window.__throwing.open();
    window.__disposeError=""; try { window.__throwing.dispose(); } catch(error) { window.__disposeError=error.message; } true`);
  assert.equal(await cdp.evaluate("window.__disposeError"), "close callback failed");
  assert.equal(await cdp.evaluate("window.__throwing.element.isConnected"), false);
  assert.equal(await cdp.evaluate("window.__throwing.element.open"), false);
  assert.equal(await cdp.evaluate("document.activeElement === window.__opener"), true);
  await cdp.evaluate("window.__throwing.open(); window.__throwing.close(); true");
  assert.equal(await cdp.evaluate("window.__throwing.element.open"), false);
});


test("Chromium themes modal backdrops and keeps tall dialog actions reachable on narrow screens", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.command("Emulation.setDeviceMetricsOverride", {width:360,height:640,deviceScaleFactor:1,mobile:true});
  await cdp.evaluate(`window.__reset();
    const content=document.createElement("div");
    window.__visualRange=document.createElement("input"); window.__visualRange.type="range";
    window.__visualRange.style.color="var(--accent)"; content.append(window.__visualRange);
    for(let index=0;index<24;index++){const paragraph=document.createElement("p");paragraph.textContent="Scrollable plugin setting "+(index+1);content.append(paragraph);}
    window.__visualDialog=window.__ui.dialog({title:"Narrow plugin settings",description:"Long content should remain scrollable.",content});
    window.__mount.append(window.__visualDialog.element); window.__visualDialog.open(); true`);
  const darkBackdrop = await cdp.evaluate("getComputedStyle(window.__visualDialog.element, '::backdrop').backgroundColor");
  const darkRangeAccent = await cdp.evaluate("getComputedStyle(window.__visualRange).accentColor");
  const narrow = await cdp.evaluate(`(() => {
    const dialog=window.__visualDialog.element;
    const rect=dialog.getBoundingClientRect();
    return {width:rect.width,viewport:innerWidth,maxHeight:getComputedStyle(dialog).maxHeight,
      scrollable:dialog.scrollHeight>dialog.clientHeight,horizontalOverflow:dialog.scrollWidth>dialog.clientWidth};
  })()`);
  assert.equal(narrow.width <= narrow.viewport - 32, true);
  assert.equal(narrow.width >= 280, true);
  assert.equal(narrow.maxHeight, "608px");
  assert.equal(narrow.scrollable, true);
  assert.equal(narrow.horizontalOverflow, false);
  assert.equal(darkBackdrop, "rgba(13, 10, 7, 0.76)");
  assert.equal(darkRangeAccent, await cdp.evaluate("getComputedStyle(window.__visualRange).color"));
  await cdp.evaluate("window.__visualDialog.element.tabIndex=-1; window.__visualDialog.element.scrollTop=0; window.__visualDialog.element.focus(); true");
  await press(cdp, "PageDown", "PageDown", 34);
  assert.equal(await cdp.evaluate("window.__visualDialog.element.scrollTop>0"), true);
  await cdp.evaluate("window.__visualDialog.element.scrollTop=window.__visualDialog.element.scrollHeight; document.documentElement.dataset.theme='light'; true");
  const reachedActions = await cdp.evaluate(`(() => {
    const dialog=window.__visualDialog.element;
    const actions=dialog.querySelector(".plugin-dialog-actions").getBoundingClientRect();
    const rect=dialog.getBoundingClientRect();
    return dialog.scrollTop>0 && actions.bottom<=rect.bottom && actions.bottom>rect.top;
  })()`);
  const lightBackdrop = await cdp.evaluate("getComputedStyle(window.__visualDialog.element, '::backdrop').backgroundColor");
  const lightRangeAccent = await cdp.evaluate("getComputedStyle(window.__visualRange).accentColor");
  assert.equal(reachedActions, true);
  assert.equal(lightBackdrop, "rgba(60, 42, 28, 0.56)");
  assert.equal(lightRangeAccent, await cdp.evaluate("getComputedStyle(window.__visualRange).color"));
  assert.notEqual(lightRangeAccent, darkRangeAccent);
  await cdp.command("Emulation.clearDeviceMetricsOverride");
  await cdp.evaluate("window.__visualDialog.dispose(); document.documentElement.removeAttribute('data-theme'); true");
});

test("Chromium renders localized text badges with explicit tone and disposes them with the plugin", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  const badges = await cdp.evaluate(`(() => {
    window.__reset();
    const ui = Amaterasu.createExtensionUiElementsV1(window.__controller.signal);
    const tones = [undefined, "success", "warning", "error", "info"];
    const components = tones.map(tone => ui.badge({
      label: { source: "Warning", translations: { [navigator.language]: "<img src=x>" } }, tone,
    }));
    window.__mount.append(...components.map(component => component.element));
    return {
      tones: components.map(component => component.element.dataset.tone),
      text: components[0].element.textContent,
      image: Boolean(components[0].element.querySelector("img")),
      frozen: components.every(Object.isFrozen),
      invalidRejected: (() => {
        try { ui.badge({label: "Invalid", tone: "rainbow"}); } catch { return true; }
        return false;
      })(),
    };
  })()`);
  assert.deepEqual(badges, {
    tones: ["neutral", "success", "warning", "error", "info"],
    text: "<img src=x>",
    image: false,
    frozen: true,
    invalidRejected: true,
  });
  assert.equal(await cdp.evaluate(`window.__controller.abort();
    document.querySelectorAll(".plugin-badge").length`), 0);
});

test("Chromium tooltips keep action descriptions and support focus, Escape, pointer handoff and viewport edges", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.command("Emulation.setDeviceMetricsOverride", {width:360,height:640,deviceScaleFactor:1,mobile:false});
  await cdp.evaluate(`window.__reset();
    const ui = Amaterasu.createExtensionUiElementsV1(window.__controller.signal);
    window.__tipAction = window.__ui.button({label: "Edit profile", help: "Edit settings", onClick() {}});
    window.__tipHandle = ui.tooltip({trigger: window.__tipAction.element, message: "Change the name, preferences, or display picture for this profile."});
    window.__tipButton = window.__tipAction.element.querySelector("button");
    window.__tip = window.__tipHandle.element.querySelector('[role="tooltip"]');
    Object.assign(window.__tipHandle.element.style, {position:"absolute", right:"8px", bottom:"8px"});
    window.__mount.append(window.__tipHandle.element);
    true`);
  assert.equal(await cdp.evaluate("window.__tip.hidden"), true);
  assert.deepEqual(await cdp.evaluate(`(async () => {
    window.__tipAction.setState({help: null, error: "Invalid profile"});
    await new Promise(resolve => requestAnimationFrame(resolve));
    return window.__tipButton.getAttribute("aria-describedby").split(" ").map(id => document.getElementById(id).textContent);
  })()`), [
    "Invalid profile", "Change the name, preferences, or display picture for this profile.",
  ]);
  await cdp.evaluate("window.__tipButton.focus(); true");
  const edges = await cdp.evaluate(`(() => {
    const {left, right, top, bottom} = window.__tip.getBoundingClientRect();
    return {
      visible: !window.__tip.hidden,
      fits: left >= 7 && right <= innerWidth - 7 && top > 0 && bottom <= window.__tipHandle.element.getBoundingClientRect().top + 1,
      placement: window.__tip.dataset.placement,
    };
  })()`);
  assert.deepEqual(edges, { visible: true, fits: true, placement: "above" });
  await press(cdp, "Escape", "Escape", 27);
  assert.equal(await cdp.evaluate("window.__tip.hidden && document.activeElement === window.__tipButton"), true);
  await cdp.evaluate("window.__tipButton.blur(); true");

  const moveTo = async (selector) => {
    const {x, y} = await cdp.evaluate(`(() => {
      const {left, top, width, height} = ${selector}.getBoundingClientRect();
      return {x: left + width / 2, y: top + height / 2};
    })()`);
    await cdp.command("Input.dispatchMouseEvent", {type:"mouseMoved", x, y});
  };
  await moveTo("window.__tipButton");
  assert.equal(await cdp.evaluate("window.__tip.hidden"), false);
  await moveTo("window.__tip");
  assert.equal(await cdp.evaluate("window.__tip.hidden"), false);
  await cdp.command("Input.dispatchMouseEvent", {type:"mouseMoved", x:0, y:0});
  assert.equal(await cdp.evaluate("window.__tip.hidden"), true);
  assert.deepEqual(await cdp.evaluate(`window.__tipHandle.dispose();
    [window.__tipHandle.element.isConnected, window.__tipHandle.element.childElementCount,
      window.__tipButton.getAttribute("aria-describedby") === window.__tipAction.element.querySelector('[role="alert"]').id]`), [
    false, 0, true,
  ]);
  await cdp.command("Emulation.clearDeviceMetricsOverride");
});

test("Chromium sparkline charts wrap in a narrow panel and expose latest and inspected samples", async t => {
  if (!browser) return t.skip("Chromium browser setup unavailable.");
  const {cdp} = browser;
  await cdp.command("Emulation.setDeviceMetricsOverride", {width:320,height:640,deviceScaleFactor:1,mobile:false});
  const initial = await cdp.evaluate(`(() => {
    window.__reset();
    const ui = Amaterasu.createExtensionUiChartsV1(window.__controller.signal);
    const series = [{id:"cpu", label:"CPU", points:[
      {time:0,value:4}, {time:500,value:null}, {time:1000,value:0},
    ]}];
    const options = {label:"CPU usage", unit:"%", series, minimum:0, maximum:10};
    window.__sparkline = ui.metricChart({...options, appearance:"sparkline"});
    const legacy = ui.metricChart(options);
    const chart = window.__sparkline.element;
    chart.style.width = "155px";
    window.__mount.append(chart, legacy.element);
    window.__sparklinePlot = chart.querySelector("svg");
    const plot = window.__sparklinePlot;
    const plotRect = plot.getBoundingClientRect();
    return {
      latest: chart.querySelector(".plugin-chart-latest").textContent,
      accessible: plot.getAttribute("aria-label"),
      wraps: plotRect.top >= chart.querySelector("h3").getBoundingClientRect().bottom,
      overflow: chart.scrollWidth > chart.clientWidth,
      nullGaps: (plot.querySelector("path").getAttribute("d").match(/M/g) ?? []).length,
      graphHeight: plotRect.height,
      legacyControls: legacy.element.querySelectorAll(".plugin-chart-controls button").length,
      legacySeries: legacy.element.querySelectorAll(".plugin-chart-legend button").length,
    };
  })()`);
  assert.equal(initial.latest, "0%");
  assert.match(initial.accessible, /CPU usage.*Latest: CPU: 0%.*2 measured values/);
  assert.equal(initial.wraps, true);
  assert.equal(initial.overflow, false);
  assert.equal(initial.nullGaps, 2);
  assert.ok(initial.graphHeight < 28);
  assert.equal(initial.legacyControls, 3);
  assert.equal(initial.legacySeries, 1);

  await cdp.evaluate("window.__sparklinePlot.focus(); true");
  await press(cdp, "Home", "Home", 36);
  assert.match(await cdp.evaluate('window.__sparkline.element.querySelector(".plugin-chart-readout").textContent'), /CPU: 4%/);
  assert.equal(await cdp.evaluate('window.__sparkline.element.querySelector(".plugin-chart-latest").textContent'), "0%");
  const missing = await cdp.evaluate(`window.__sparkline.setData([{id:"cpu",label:"CPU",points:[
    {time:0,value:4}, {time:1000,value:null},
  ]}]);
  window.__sparkline.element.querySelector(".plugin-chart-latest").textContent`);
  assert.equal(missing, "Unavailable");
  await cdp.command("Emulation.clearDeviceMetricsOverride");
});
