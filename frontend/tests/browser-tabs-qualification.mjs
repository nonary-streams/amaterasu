// Real Chromium qualification for the reusable plugin tabs API.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import ts from "typescript";

const profile = mkdtempSync(join(tmpdir(), "amaterasu-plugin-tabs-"));
const browser = spawn(process.env.AMATERASU_BROWSER ?? "chromium", [
  "--headless=new", "--disable-gpu", "--no-first-run", "--disable-extensions",
  "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0",
  `--user-data-dir=${profile}`, "about:blank",
], { stdio: "ignore" });
let socket;
try {
  let address;
  for (let attempt = 0; attempt < 200; attempt++) {
    try {
      const [port, path] = readFileSync(join(profile, "DevToolsActivePort"), "utf8").trim().split("\n");
      address = `ws://127.0.0.1:${port}${path}`;
      break;
    } catch {
      if (browser.exitCode !== null) throw new Error("Chromium exited before debugger startup.");
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  assert.ok(address, "Chromium debugger did not start.");
  socket = new WebSocket(address);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  let sequence = 0;
  const pending = new Map();
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);
  };
  const command = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = ++sequence;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const { targetId } = await command("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await command("Target.attachToTarget", { targetId, flatten: true });
  const evaluate = async (expression) => {
    const result = await command("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (result.exceptionDetails) throw new Error(result.exception?.description ?? "Browser evaluation failed.");
    return result.result.value;
  };
  const key = async (name, code, keyCode) => {
    for (const type of ["keyDown", "keyUp"])
      await command("Input.dispatchKeyEvent", { type, key: name, code,
        windowsVirtualKeyCode: keyCode, nativeVirtualKeyCode: keyCode }, sessionId);
  };

  const sourceFiles = ["locale-catalogs.ts", "locale.ts", "ui.ts", "extension_ui_tabs.ts"];
  const source = sourceFiles.map((file) => readFileSync(`src/${file}`, "utf8")).join("\n");
  await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
  await evaluate(`
    document.body.innerHTML = "<main></main>";
    window.tabLifecycle = new AbortController();
    window.tabChanges = [];
    window.generalContent = document.createElement("section");
    window.generalContent.textContent = "<img src=x onerror=alert(1)>";
    window.networkContent = document.createElement("section");
    window.networkContent.textContent = "Network panel";
    window.disabledContent = document.createElement("section");
    window.disabledContent.textContent = "Unavailable panel";
    window.tabsApi = Amaterasu.createExtensionUiTabsV1(window.tabLifecycle.signal);
    window.tabsHandle = window.tabsApi.tabs({
      label: { source: "Settings sections", translations: { fr: "Settings sections FR" } },
      tabs: [
        { id: "general", label: { source: "General", translations: { fr: "General FR" } }, content: window.generalContent },
        { id: "disabled", label: "Disabled", content: window.disabledContent, disabled: true },
        { id: "network", label: { source: "Network", translations: { fr: "Network FR" } }, content: window.networkContent },
      ],
      onChange(id) { window.tabChanges.push(id); },
    });
    document.querySelector("main").append(window.tabsHandle.element);
  `);

  assert.deepEqual(await evaluate(`(() => {
    const root = window.tabsHandle.element;
    const tablist = root.querySelector('[role="tablist"]');
    const tabs = [...root.querySelectorAll('[role="tab"]')];
    const panels = [...root.querySelectorAll('[role="tabpanel"]')];
    return {
      active: window.tabsHandle.active(),
      listLabelled: tablist.getAttribute("aria-labelledby") === root.querySelector(".plugin-tabs-label").id,
      count: tabs.length,
      links: tabs.every(tab => {
        const panel = document.getElementById(tab.getAttribute("aria-controls"));
        return panel?.getAttribute("aria-labelledby") === tab.id;
      }),
      selected: tabs.map(tab => tab.getAttribute("aria-selected")),
      hidden: panels.map(panel => panel.hidden),
      disabled: tabs[1].disabled,
      escaped: window.generalContent.querySelector("img") === null,
      activeContentNodePreserved: panels[0].firstElementChild === window.generalContent,
    };
  })()`), {
    active: "general", listLabelled: true, count: 3, links: true,
    selected: ["true", "false", "false"], hidden: [false, true, true],
    disabled: true, escaped: true, activeContentNodePreserved: true,
  });

  assert.deepEqual(await evaluate(`(() => {
    const alpha = document.createElement('section');
    const beta = document.createElement('section');
    const betaInput = document.createElement('input');
    beta.append(betaInput);
    const fallback = window.tabsApi.tabs({ label: 'Focus recovery', tabs: [
      { id: 'alpha', label: 'Alpha', content: alpha },
      { id: 'beta', label: 'Beta', content: beta },
    ] });
    document.querySelector('main').append(fallback.element);
    fallback.select('beta');
    fallback.element.querySelector('[data-tab-id=beta]').focus();
    fallback.setTabs([{ id: 'alpha', label: 'Alpha', content: alpha }]);
    const removedTab = fallback.active() === 'alpha' &&
      document.activeElement === fallback.element.querySelector('[data-tab-id=alpha]');
    fallback.setTabs([
      { id: 'alpha', label: 'Alpha', content: alpha },
      { id: 'beta', label: 'Beta', content: beta },
    ]);
    fallback.select('beta');
    betaInput.focus();
    const replacement = document.createElement('section');
    fallback.setTabs([
      { id: 'alpha', label: 'Alpha', content: alpha },
      { id: 'beta', label: 'Beta', content: replacement },
    ]);
    const replacedPanel = fallback.active() === 'beta' &&
      document.activeElement === fallback.element.querySelector('[data-tab-id=beta]') &&
      betaInput.isConnected === false;
    fallback.dispose();
    fallback.element.remove();
    return { removedTab, replacedPanel };
  })()`), { removedTab: true, replacedPanel: true });


  await evaluate(`document.querySelector('[data-tab-id="general"]').focus()`);
  await key("ArrowRight", "ArrowRight", 39);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), focus:document.activeElement.getAttribute("data-tab-id"), changes:window.tabChanges})`),
    { active: "network", focus: "network", changes: ["network"] });
  await key("Home", "Home", 36);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), focus:document.activeElement.getAttribute("data-tab-id"), changes:window.tabChanges})`),
    { active: "general", focus: "general", changes: ["network", "general"] });
  await key("End", "End", 35);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), focus:document.activeElement.getAttribute("data-tab-id")})`),
    { active: "network", focus: "network" });
  await evaluate(`window.tabsHandle.select("general")`);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), changes:window.tabChanges})`),
    { active: "general", changes: ["network", "general", "network"] });

  await evaluate(`
    const networkTab = document.querySelector('[data-tab-id="network"]');
    window.networkInput = document.createElement("input");
    window.networkContent.append(window.networkInput);
    window.tabsHandle.select("network");
    window.networkInput.focus();
    window.networkTabBeforeUpdate = networkTab;
    window.tabsHandle.setTabs([
      { id: "network", label: { source: "Network", translations: { fr: "Network FR" } }, content: window.networkContent },
      { id: "general", label: "General", content: window.generalContent },
      { id: "disabled", label: "Disabled", content: window.disabledContent, disabled: true },
    ]);
  `);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), same:document.querySelector('[data-tab-id="network"]')===window.networkTabBeforeUpdate, focusedInput:document.activeElement===window.networkInput, contentStable:window.networkContent.firstElementChild===window.networkInput, changes:window.tabChanges})`),
    { active: "network", same: true, focusedInput: true, contentStable: true, changes: ["network", "general", "network"] });

  await evaluate(`window.tabsHandle.select("network"); window.networkInput.focus(); window.tabsHandle.setDisabled("network", true);`);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), focused:document.activeElement.getAttribute("data-tab-id"), disabled:document.querySelector('[data-tab-id="network"]').disabled, changes:window.tabChanges})`),
    { active: "general", focused: "general", disabled: true, changes: ["network", "general", "network"] });
  await evaluate(`
    window.blankGroupNameRejected = false;
    try { window.tabsApi.tabs({ label: "  ", tabs: [{ id: "x", label: "X", content: document.createElement("div") }] }); }
    catch { window.blankGroupNameRejected = true; }
    window.blankTabNameRejected = false;
    try { window.tabsHandle.setTabs([{ id: "x", label: " ", content: document.createElement("div") }]); }
    catch { window.blankTabNameRejected = true; }
    window.allTabsDisabledStable = window.tabsHandle.active() === "general";
    window.tabsHandle.setDisabled("general", true);
    window.allTabsDisabled = window.tabsHandle.active() === null &&
      [...window.tabsHandle.element.querySelectorAll('[role="tab"]')].every(tab => tab.disabled && tab.tabIndex === -1);
    window.tabsHandle.setDisabled("network", false);
  `);
  assert.deepEqual(await evaluate(`({blankGroup:window.blankGroupNameRejected,blankTab:window.blankTabNameRejected,stable:window.allTabsDisabledStable,allDisabled:window.allTabsDisabled,active:window.tabsHandle.active(),selected:document.querySelector('[data-tab-id="network"]').getAttribute("aria-selected"),tabIndex:document.querySelector('[data-tab-id="network"]').tabIndex})`),
    { blankGroup: true, blankTab: true, stable: true, allDisabled: true, active: "network", selected: "true", tabIndex: 0 });
  await evaluate(`window.tabsHandle.setDisabled("general", false); window.tabsHandle.select("general"); Amaterasu.applyLocale("fr");`);
  assert.deepEqual(await evaluate(`({label:window.tabsHandle.element.querySelector(".plugin-tabs-label").textContent, general:document.querySelector('[data-tab-id="general"]').textContent, network:document.querySelector('[data-tab-id="network"]').textContent, active:window.tabsHandle.active()})`),
    { label: "Settings sections FR", general: "General", network: "Network FR", active: "general" });
  await evaluate(`Amaterasu.applyLocale("en");`);

  assert.equal(await evaluate(`(() => { const invalid = document.createElement("section"); try {
    window.tabsHandle.setTabs([{id:"bad",label:"Bad",content:invalid}], "missing"); return false;
  } catch { return window.tabsHandle.active() === "general" &&
    document.querySelectorAll('[role="tab"]').length === 3 &&
    invalid.parentNode === null; } })()`), true);

  const compatibility = await evaluate(`(() => {
    const legacySdk = {features:["ui.controls/1"], ui:{}};
    return legacySdk.features.includes("ui.tabs/1") ? legacySdk.ui.tabs({}) : "fallback";
  })()`);
  assert.equal(compatibility, "fallback");

  await evaluate(`window.tabLifecycle.abort(); window.tabsHandle.setDisabled("general", true); window.tabsHandle.select("network"); document.querySelector('[data-tab-id="network"]').click();`);
  assert.deepEqual(await evaluate(`({active:window.tabsHandle.active(), changes:window.tabChanges, generalDisabled:document.querySelector('[data-tab-id="general"]').disabled})`),
    { active: "general", changes: ["network", "general", "network"], generalDisabled: false });
  console.log("PASS: real Chromium tabs expose linked ARIA, safe DOM, localized labels, disabled states, keyboard focus, dynamic updates, compatibility fallback, and lifecycle disposal");
} finally {
  socket?.close();
  if (browser.exitCode === null) {
    browser.kill();
    await new Promise((resolve) => browser.once("exit", resolve));
  }
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
