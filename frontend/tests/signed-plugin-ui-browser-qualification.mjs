// Drives an owned Chromium against an isolated signed host and packaged web extension.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { extname } from "node:path";

const options = JSON.parse(readFileSync(0, "utf8"));
const check = (condition, message) => { if (!condition) throw new Error(message); };
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const assetBytes = readFileSync(options.assetPath);
check(sha256(assetBytes) === options.assetSha256, "fixture digest mismatch before browser run");
for (const asset of options.themeAssets) check(sha256(readFileSync(asset.path)) === asset.sha256, `theme fixture digest mismatch: ${asset.name}`);
const themeAssetNames = new Set(options.themeAssets.map((asset) => asset.name));
const themeAssetTypes = new Map([
  [".css", "text/css"],
  [".svg", "image/svg+xml"],
  [".webp", "image/webp"],
  [".woff2", "font/woff2"],
]);

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
const themeRequests = new Map();
const themeResponses = new Map();
const loadedThemeRequests = new Set();
const failedThemeAssets = [];
const showcaseRequests = new Map();
const showcaseResponses = new Map();
const failedShowcaseAssets = [];
const primitivesRequests = new Map();
const primitivesResponses = new Map();
const failedPrimitivesAssets = [];
let documentCsp = "";
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
      const primitivesMatch = url.pathname.match(/^\/v1\/extensions\/signed-ui-primitives\/([a-f0-9]{64})\/assets\/primitives\.js$/);
      if (primitivesMatch) primitivesRequests.set(event.params.requestId, {
        digest: primitivesMatch[1], name: "primitives.js", path: url.pathname, origin: url.origin,
      });
      const showcaseMatch = url.pathname.match(/^\/v1\/extensions\/showcase\/([a-f0-9]{64})\/assets\/(showcase\.js|showcase\.css)$/);
      if (showcaseMatch) showcaseRequests.set(event.params.requestId, {
        digest: showcaseMatch[1], name: showcaseMatch[2], path: url.pathname, origin: url.origin,
      });
      const match = url.pathname.match(/^\/web\/([a-f0-9]{64})\/([^/]+)$/);
      if (match && themeAssetNames.has(match[2])) themeRequests.set(event.params.requestId, {
        digest: match[1], name: match[2], path: url.pathname, origin: url.origin,
      });
    } else if (event.method === "Network.responseReceived") {
      const path = networkRequests.get(event.params.requestId);
      if (path) assetResponses.push({ requestId: event.params.requestId, path, status: event.params.response.status });
      const themeRequest = themeRequests.get(event.params.requestId);
      const showcaseRequest = showcaseRequests.get(event.params.requestId);
      const primitivesRequest = primitivesRequests.get(event.params.requestId);
      const headers = event.params.response.headers;
      const header = (name) => Object.entries(headers).find(([key]) => key.toLowerCase() === name)?.[1] ?? "";
      if (event.params.type === "Document") documentCsp = header("content-security-policy");
      if (themeRequest) themeResponses.set(themeRequest.name, {
        ...themeRequest, requestId: event.params.requestId, status: event.params.response.status,
        contentType: header("content-type"), csp: header("content-security-policy"),
      });
      if (showcaseRequest) showcaseResponses.set(showcaseRequest.name, {
        ...showcaseRequest, requestId: event.params.requestId, status: event.params.response.status,
        contentType: header("content-type"), csp: header("content-security-policy"),
      });
      if (primitivesRequest) primitivesResponses.set(primitivesRequest.name, {
        ...primitivesRequest, requestId: event.params.requestId, status: event.params.response.status,
        contentType: header("content-type"), csp: header("content-security-policy"),
      });
    } else if (event.method === "Network.loadingFinished" && themeRequests.has(event.params.requestId)) {
      loadedThemeRequests.add(event.params.requestId);
    } else if (event.method === "Network.loadingFailed") {
      const themeRequest = themeRequests.get(event.params.requestId);
      if (themeRequest) failedThemeAssets.push(themeRequest.name);
      const showcaseRequest = showcaseRequests.get(event.params.requestId);
      if (showcaseRequest) failedShowcaseAssets.push(showcaseRequest.name);
      const primitivesRequest = primitivesRequests.get(event.params.requestId);
      if (primitivesRequest) failedPrimitivesAssets.push(primitivesRequest.name);
    }
  });
  await debuggerClient.command("Page.enable", {}, sessionId);
  await debuggerClient.command("Runtime.enable", {}, sessionId);
  await debuggerClient.command("Network.enable", {}, sessionId);
  const setDesktopViewport = (width, height, deviceScaleFactor) => debuggerClient.command("Emulation.setDeviceMetricsOverride", {
    width, height, mobile: false, deviceScaleFactor,
  }, sessionId);
  await setDesktopViewport(1360, 900, 1);
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
      const event = {
        type, key: name, code, windowsVirtualKeyCode: virtualKey, nativeVirtualKeyCode: virtualKey,
      };
      if (type === "keyDown" && name === "Enter") {
        event.text = "\r";
        event.unmodifiedText = "\r";
      }
      if (type === "keyDown" && name === " ") {
        event.text = name;
        event.unmodifiedText = name;
      }
      await debuggerClient.command("Input.dispatchKeyEvent", event, sessionId);
    }
  };

  await wait("!!document.querySelector('input[name=username]')", "sign-in form");
  check(await evaluate(`getComputedStyle(document.querySelector('.login-intro'), '::after')
    .backgroundImage.includes('scroll-masthead.webp') &&
    getComputedStyle(document.querySelector('.login-side'), '::before').backgroundImage.includes('scroll-foothills.svg')`),
  "sign-in artwork was not applied by the signed stylesheet");
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
  const baseHref = await evaluate("document.querySelector('base')?.getAttribute('href') ?? ''");
  const selectedDigest = baseHref.match(/^\/web\/([a-f0-9]{64})\/$/)?.[1];
  check(selectedDigest, "signed page did not expose its selected web bundle digest");
  check(documentCsp.includes("default-src 'self'") && documentCsp.includes("img-src 'self' blob:") &&
    documentCsp.includes("script-src 'self'") && documentCsp.includes("style-src 'self'") &&
    documentCsp.includes("object-src 'none'"),
    "signed document CSP did not retain its restrictive policy");

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
      area.querySelector('input[type=range]').getAttribute('aria-label') === 'Strength slider' &&
      area.querySelector('input[type=number]').getAttribute('aria-label') === 'Strength value';
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
  await wait("!!document.querySelector('.overview-status')", "overview theme panel");
  await wait(`(() => {
    const banner = document.querySelector('.console-masthead');
    const mountains = getComputedStyle(banner, '::before');
    const crest = getComputedStyle(banner.querySelector('.brand-symbol'));
    const foothills = getComputedStyle(document.querySelector('.overview-status'), '::after');
    const blossoms = getComputedStyle(document.querySelector('.overview-activity-panel'), '::after');
    const brush = getComputedStyle(document.querySelector('.navigation .nav-item.selected'), '::before');
    return mountains.content !== 'none' && mountains.backgroundImage.includes('scroll-masthead.webp') &&
      crest.backgroundImage.includes('sun-crest.svg') &&
      foothills.content !== 'none' && foothills.backgroundImage.includes('scroll-foothills.svg') &&
      blossoms.content !== 'none' && blossoms.backgroundImage.includes('scroll-blossoms.svg') &&
      brush.content !== 'none' && brush.maskImage.includes('ink-swipe.svg');
  })()`,
    "painted masthead, crest, foothills, blossoms, and selected navigation brush applied under signed CSS");
  await wait(`[...document.fonts].some((font) => font.family.includes('Kaushan Script') && font.status === 'loaded')`,
    "signed brush font decoded by Chromium");
  const verifyThemeAssets = async (assets) => {
    const names = new Set(assets.map(({ name }) => name));
    const failed = () => failedThemeAssets.filter((name) => names.has(name));
    const allThemeResponsesReceived = () => assets.every(({ name }) =>
      loadedThemeRequests.has(themeResponses.get(name)?.requestId));
    const themeDeadline = Date.now() + 10000;
    while (Date.now() < themeDeadline && failed().length === 0 && !allThemeResponsesReceived()) {
      await delay(40);
    }
    check(failed().length === 0, `signed theme asset requests failed: ${failed().join(', ')}`);
    check(allThemeResponsesReceived(), `browser did not finish loading ${assets.filter(({ name }) =>
      !loadedThemeRequests.has(themeResponses.get(name)?.requestId)).map(({ name }) => name).join(', ')}`);
    for (const asset of assets) {
      const response = themeResponses.get(asset.name);
      check(response && response.status === 200, `signed ${asset.name} response was not HTTP 200`);
      check(response.origin === options.origin && response.digest === selectedDigest &&
        response.path === `/web/${selectedDigest}/${asset.name}`,
        `${asset.name} did not use the selected signed web bundle digest route`);
      const extension = extname(asset.name);
      const mimeType = themeAssetTypes.get(extension);
      check(mimeType && response.contentType.toLowerCase().split(";", 1)[0].trim() === mimeType,
        `${asset.name} returned unexpected content type: ${response.contentType}`);
      check(response.csp.includes("img-src 'self' blob:") && response.csp.includes("style-src 'self'"),
        `${asset.name} response did not retain the actual signed asset CSP`);
      const served = await debuggerClient.command("Network.getResponseBody", { requestId: response.requestId }, sessionId);
      const bytes = served.base64Encoded ? Buffer.from(served.body, "base64") : Buffer.from(served.body);
      check(sha256(bytes) === asset.sha256, `served signed ${asset.name} bytes differ from packaged bytes`);
      if (extension === ".svg") check(/^(?:\s*<\?xml[^>]*\?>)?\s*<svg\b/.test(bytes.toString("utf8")),
        `served signed ${asset.name} is not an SVG`);
      if (extension === ".webp") check(bytes.toString("ascii", 0, 4) === "RIFF" && bytes.toString("ascii", 8, 12) === "WEBP",
        `served signed ${asset.name} is not a WebP`);
      if (extension === ".woff2") check(bytes.toString("ascii", 0, 4) === "wOF2",
        `served signed ${asset.name} is not a WOFF2 font`);
    }
  };
  const appliedStylesheets = await evaluate("[...document.styleSheets].map((sheet) => sheet.href)");
  for (const { name } of options.themeAssets.filter((asset) => asset.name.endsWith(".css"))) {
    check(appliedStylesheets.includes(`${options.origin}/web/${selectedDigest}/${name}`),
      `signed stylesheet ${name} did not attach to the document`);
  }
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

  await evaluate(`(() => {
    const locale = document.querySelector('#global-locale');
    locale.value = 'en';
    locale.dispatchEvent(new Event('change', { bubbles: true }));
  })()`);
  await wait(`document.querySelector('#global-locale')?.value === 'en' &&
    [...document.querySelectorAll('.nav-item')].some((item) => item.textContent.includes('Overview'))`,
    "English labels restored before showcase qualification");
  await evaluate("location.hash = 'extension/showcase'");
  await wait("document.querySelector('.okami-showcase')?.dataset.stylesheetState === 'loaded'",
    "signed showcase and same-origin stylesheet load");
  await wait(`getComputedStyle(document.querySelector('.okami-showcase')).display === 'grid' &&
    getComputedStyle(document.querySelector('.showcase-theme-grid')).display === 'grid'`,
    "showcase CSS rendering under the host CSP");
  check(await evaluate('!document.querySelector(".okami-showcase style")'),
    "showcase inserted inline CSS despite the host CSP");
  const showcaseCss = showcaseResponses.get("showcase.css");
  check(showcaseCss && showcaseCss.status === 200, "signed showcase CSS was not delivered");
  check(showcaseCss.digest && showcaseCss.path ===
    `/v1/extensions/showcase/${showcaseCss.digest}/assets/showcase.css` &&
    showcaseCss.origin === options.origin,
    "showcase CSS did not use its same-origin signed digest path");
  check(showcaseCss.contentType.toLowerCase().startsWith("text/css"),
    `showcase CSS returned unexpected content type: ${showcaseCss.contentType}`);
  check(showcaseCss.csp.includes("style-src 'self'") && showcaseCss.csp.includes("default-src 'self'"),
    "showcase CSS response did not retain the actual restrictive CSP");
  check(failedShowcaseAssets.length === 0,
    `signed showcase assets failed to load: ${failedShowcaseAssets.join(", ")}`);
  for (const asset of options.showcaseAssets) {
    const response = showcaseResponses.get(asset.name);
    check(response && response.status === 200,
      `signed showcase ${asset.name} was not delivered successfully`);
    const served = await debuggerClient.command("Network.getResponseBody", { requestId: response.requestId }, sessionId);
    const bytes = served.base64Encoded ? Buffer.from(served.body, "base64") : Buffer.from(served.body);
    check(sha256(bytes) === asset.sha256,
      `signed showcase ${asset.name} differs from the packaged bytes`);
  }

  const lightShowcase = '.okami-showcase .showcase-theme[data-theme="light"]';
  const darkShowcase = '.okami-showcase .showcase-theme[data-theme="dark"]';
  const showcaseThemeTabs = '.okami-showcase .showcase-theme-grid [role="tablist"]';
  const showcaseSelector = (selector) => JSON.stringify(lightShowcase + " " + selector);
  await wait(`(() => {
    const light = document.querySelector(${JSON.stringify(lightShowcase)});
    const dark = document.querySelector(${JSON.stringify(darkShowcase)});
    return light?.getClientRects().length > 0 && dark?.getClientRects().length === 0;
  })()`, "visible light and hidden dark showcase boards");
  await evaluate(`document.querySelector(${JSON.stringify(showcaseThemeTabs + ' [role="tab"]')}).focus()`);
  await key("ArrowRight", "ArrowRight", 39);
  await wait(`document.querySelector(${JSON.stringify(darkShowcase)})?.getClientRects().length > 0 &&
    document.querySelector(${JSON.stringify(lightShowcase)})?.getClientRects().length === 0`,
    "keyboard-selected dark showcase board");
  await key("ArrowLeft", "ArrowLeft", 37);
  await wait(`document.querySelector(${JSON.stringify(lightShowcase)})?.getClientRects().length > 0`,
    "keyboard-selected light showcase board");

  const checkbox = showcaseSelector('.plugin-checkbox input[type="checkbox"]');
  const search = showcaseSelector('.plugin-search-field input[type="search"]');
  const initialCheckbox = await evaluate(`(() => {
    const checkbox = document.querySelector(${checkbox});
    const search = document.querySelector(${search});
    const labelText = (input) => input?.getAttribute('aria-label') ||
      [...(input?.labels || [])].map((label) => label.textContent).join(' ');
    return {
      checked: checkbox?.checked,
      associatedCheckboxLabel: labelText(checkbox).trim().length > 0,
      associatedSearchLabel: labelText(search).includes('Find a device'),
      placeholder: search?.placeholder,
    };
  })()`);
  check(typeof initialCheckbox.checked === "boolean" && initialCheckbox.associatedCheckboxLabel &&
    initialCheckbox.associatedSearchLabel && initialCheckbox.placeholder === "Search by name",
    "signed showcase did not render shared native checkbox and search controls with accessible labels");
  await evaluate(`document.querySelector(${checkbox}).focus()`);
  await key(" ", "Space", 32);
  check(await evaluate(`document.querySelector(${checkbox}).checked === ${!initialCheckbox.checked}`),
    "keyboard did not toggle the shared showcase checkbox");
  await evaluate(`document.querySelector(${search}).focus()`);
  await debuggerClient.command("Input.insertText", { text: "Living room" }, sessionId);
  check(await evaluate(`document.querySelector(${search}).value === 'Living room'`),
    "shared showcase search input did not accept keyboard text");

  const iconButton = showcaseSelector('.plugin-tooltip-anchor button[aria-label="Edit profile"]');
  await evaluate(`document.querySelector(${iconButton}).focus()`);
  await wait(`(() => {
    const button = document.querySelector(${iconButton});
    const tooltip = button?.closest('.plugin-tooltip-anchor')?.querySelector('.plugin-tooltip[role="tooltip"]');
    return document.activeElement === button && tooltip && !tooltip.hidden &&
      button.getAttribute('aria-describedby')?.split(/\\s+/).includes(tooltip.id) &&
      !!button.querySelector('svg[aria-hidden="true"]');
  })()`, "focused shared icon action with an accessible tooltip");
  await key("Escape", "Escape", 27);
  check(await evaluate(`(() => {
    const button = document.querySelector(${iconButton});
    return document.activeElement === button &&
      button.closest('.plugin-tooltip-anchor').querySelector('[role="tooltip"]').hidden;
  })()`), "Escape did not dismiss the shared icon tooltip and preserve focus");

  check(await evaluate(`(() => {
    const light = document.querySelector(${JSON.stringify(lightShowcase)});
    return !!light.querySelector('.plugin-badge[data-tone]') && [...light.querySelectorAll('table')].some((table) =>
      table.caption?.textContent.trim() === 'Sample paired devices' &&
      [...table.querySelectorAll('th[scope="col"]')].map((cell) => cell.textContent.trim()).join(',') ===
        'Name,Status,Type,Last seen');
  })()`), "shared showcase badges or semantic table missing");
  check(await evaluate(`(() => {
    const light = document.querySelector(${JSON.stringify(lightShowcase)});
    const seal = light.querySelector('.ink-status-card[data-summary-tone="success"] .ink-status-seal');
    return seal && getComputedStyle(seal, '::before').maskImage.includes('ink-seal.svg') &&
      light.querySelectorAll('.ink-info-card').length === 3;
  })()`), "shared showcase painted status seal or hardware info cards missing");
  check(await evaluate(`(() => {
    const light = document.querySelector(${JSON.stringify(lightShowcase)});
    const sparklines = [...light.querySelectorAll('.plugin-metric-chart-sparkline')];
    return sparklines.map((chart) => chart.querySelector('.plugin-chart-latest').textContent).join(',') ===
      '4%,3%,0%,0 Mbps' && sparklines.every((chart) => chart.querySelector('svg[tabindex="0"]'));
  })()`), "showcase did not render four keyboard-accessible sparklines with the sample values");
  const cpuPlot = showcaseSelector('.plugin-metric-chart-sparkline svg');
  const cpuReadout = showcaseSelector('.plugin-metric-chart-sparkline .plugin-chart-readout');
  const cpuCursor = showcaseSelector('.plugin-metric-chart-sparkline .plugin-chart-cursor');
  await evaluate(`document.querySelector(${cpuPlot}).focus()`);
  await key("Home", "Home", 36);
  const firstCpuCursor = await evaluate(`document.querySelector(${cpuCursor})?.getAttribute('d')`);
  check(firstCpuCursor && await evaluate(`document.querySelector(${cpuReadout}).textContent.includes('CPU:')`),
    "Home did not inspect the first signed showcase CPU sample");
  await key("End", "End", 35);
  const lastCpuCursor = await evaluate(`document.querySelector(${cpuCursor})?.getAttribute('d')`);
  check(lastCpuCursor && lastCpuCursor !== firstCpuCursor &&
    await evaluate(`document.querySelector(${cpuReadout}).textContent.includes('CPU: 4%')`),
    "End did not inspect the final signed showcase CPU sample");
  const showcaseButtons = showcaseSelector('button');
  await evaluate(`([...document.querySelectorAll(${showcaseButtons})].find((button) =>
    button.textContent.trim() === 'Open dialog')).focus()`);
  await key("Enter", "Enter", 13);
  const showcaseDialogFooter = showcaseSelector('dialog[open] .plugin-dialog-actions');
  await wait(`(() => {
    const footer = document.querySelector(${showcaseDialogFooter});
    const actions = [...(footer?.querySelectorAll('button') ?? [])].map((button) => button.textContent.trim());
    return actions.includes('Cancel') && actions.includes('Confirm');
  })()`, "shared showcase dialog with built-in Cancel and plugin Confirm in its footer");
  await evaluate(`([...document.querySelector(${showcaseDialogFooter}).querySelectorAll('button')].find((button) =>
    button.textContent.trim() === 'Confirm')).focus()`);
  await key("Enter", "Enter", 13);
  await wait(`!document.querySelector(${showcaseSelector('dialog[open]')}) &&
    document.activeElement?.textContent.trim() === 'Open dialog'`,
    "keyboard-confirmed showcase dialog and restored trigger focus");
  await verifyThemeAssets(options.themeAssets);

  await evaluate("location.hash = 'extension/signed-ui-primitives'");
  await wait("!!document.querySelector('[data-signed-ui-primitives]')",
    "signed UI primitives consumer mount");
  const primitives = primitivesResponses.get("primitives.js");
  check(primitives && primitives.status === 200, "signed UI primitives JavaScript was not delivered");
  check(primitives.path === `/v1/extensions/signed-ui-primitives/${primitives.digest}/assets/primitives.js` &&
    primitives.origin === options.origin, "UI primitives asset did not use its same-origin signed digest route");
  check(primitives.contentType.toLowerCase().includes("javascript") &&
    primitives.csp.includes("default-src 'self'") && primitives.csp.includes("script-src 'self'") &&
    primitives.csp.includes("object-src 'none'"),
    "signed UI primitives response did not retain JavaScript type and restrictive host CSP");
  check(failedPrimitivesAssets.length === 0,
    `signed UI primitives assets failed to load: ${failedPrimitivesAssets.join(", ")}`);
  const servedPrimitives = await debuggerClient.command("Network.getResponseBody",
    { requestId: primitives.requestId }, sessionId);
  const primitivesBytes = servedPrimitives.base64Encoded
    ? Buffer.from(servedPrimitives.body, "base64") : Buffer.from(servedPrimitives.body);
  check(sha256(primitivesBytes) === options.primitivesSha256 &&
    primitivesBytes.equals(readFileSync(options.primitivesPath)),
    "served UI primitives asset differs from the signed packaged fixture bytes");

  const featureQualification = await evaluate(`(() => {
    const section = document.querySelector('[data-signed-ui-primitives]');
    const expected = ['ui.dialog/1', 'ui.tabs/1', 'ui.layout/1', 'ui.feedback/1'];
    const fallbackRoot = document.createElement('div');
    const fallbackCleanup = window.__signedUiPrimitiveQualification.mount(fallbackRoot,
      {version: 1, ui: {version: 1}, features: ['ui.dialog/1']});
    const fallbackWorked = !!fallbackRoot.querySelector('[data-ui-primitive-fallback]') &&
      !fallbackRoot.querySelector('[data-signed-ui-primitives]');
    fallbackCleanup();
    return section.dataset.detectedFeatures === expected.join(',') && fallbackWorked;
  })()`);
  check(featureQualification, "feature detection or missing-feature fallback did not work");
  const semanticState = await evaluate(`(() => {
    const section = document.querySelector('[data-signed-ui-primitives]');
    const table = section.querySelector('table');
    const cells = [...table.querySelectorAll('tbody td')];
    return {
      dialogLabel: section.querySelector('dialog[aria-modal="true"]')?.getAttribute('aria-labelledby'),
      dialogDescription: section.querySelector('dialog')?.getAttribute('aria-describedby'),
      tablistLabel: section.querySelector('[role=tablist]')?.getAttribute('aria-labelledby'),
      tablistLabelText: section.querySelector('#' + CSS.escape(section.querySelector('[role=tablist]')?.getAttribute('aria-labelledby') ?? ''))?.textContent,
      selectedPanel: section.querySelector('[role=tabpanel]:not([hidden])')?.textContent,
      cardTitle: section.querySelector('section[aria-labelledby] h2')?.textContent,
      caption: table.caption.textContent,
      header: table.querySelector('th[scope=col]')?.textContent,
      cells: cells.map((cell) => cell.textContent),
      unsafeMarkup: !!table.querySelector('img'),
      status: section.querySelector('[role=status][aria-live=polite]')?.textContent,
      alert: section.querySelector('[role=alert][data-severity=warning]')?.textContent,
      progressDescription: section.querySelector('progress')?.getAttribute('aria-describedby'),
    };
  })()`);
  check(semanticState.dialogLabel && semanticState.dialogDescription && semanticState.tablistLabel &&
    semanticState.tablistLabelText === 'Qualification sections' &&
    semanticState.selectedPanel?.includes('Summary panel content') && semanticState.cardTitle === 'Metrics card' &&
    semanticState.caption === 'Qualification results' && semanticState.header === 'Name' &&
    semanticState.cells.length === 2 && semanticState.cells[1] === '<img src=x onerror=alert(1)>' &&
    !semanticState.unsafeMarkup && semanticState.status === 'Ready' &&
    semanticState.alert === 'Qualification warning' && semanticState.progressDescription,
    `semantic dialog, tabs, layout, feedback, or text-only table rendering failed: ${JSON.stringify(semanticState)}`);

  await evaluate(`document.querySelector('[data-signed-ui-primitives] button').focus()`);
  await key("Enter", "Enter", 13);
  await wait(`document.querySelector('[data-signed-ui-primitives] dialog')?.open === true &&
    document.activeElement?.getAttribute('aria-label') === 'Qualification value'`,
    "keyboard-opened modal dialog and initial focus");
  check(await evaluate(`(() => {
    const dialog = document.querySelector('[data-signed-ui-primitives] dialog');
    return dialog.querySelector('#' + CSS.escape(dialog.getAttribute('aria-labelledby')))?.textContent === 'Qualification dialog' &&
      dialog.querySelector('#' + CSS.escape(dialog.getAttribute('aria-describedby')))?.textContent.includes('Dialog description');
  })()`), "dialog accessible title or description association failed");
  await key("Escape", "Escape", 27);
  await wait(`!document.querySelector('[data-signed-ui-primitives] dialog')?.open &&
    window.__signedUiDialogCloseReason === 'cancel' && document.activeElement?.textContent === 'Open qualification dialog'`,
    "Escape close and focus restoration");
  await key("Enter", "Enter", 13);
  await wait("document.querySelector('[data-signed-ui-primitives] dialog')?.open === true",
    "reopened modal dialog");
  await key("Tab", "Tab", 9);
  check(await evaluate("document.querySelector('[data-signed-ui-primitives] dialog')?.contains(document.activeElement)"),
    "modal dialog did not keep keyboard focus within its content");
  await key("Escape", "Escape", 27);
  await wait("!document.querySelector('[data-signed-ui-primitives] dialog')?.open", "second dialog close");

  await evaluate(`document.querySelector('[data-signed-ui-primitives] [role=tab][data-tab-id=summary]').focus()`);
  await key("ArrowRight", "ArrowRight", 39);
  const tabState = await evaluate(`(() => {
    const section = document.querySelector('[data-signed-ui-primitives]');
    return {
      focused: document.activeElement?.dataset.tabId,
      selected: section.querySelector('[role=tab][aria-selected=true]')?.dataset.tabId,
      direction: getComputedStyle(section.querySelector('[role=tablist]')).direction,
      panels: [...section.querySelectorAll('[role=tabpanel]')].map((panel) => ({hidden: panel.hidden, text: panel.textContent})),
    };
  })()`);
  check(tabState.focused === 'details' && tabState.selected === 'details' &&
    tabState.panels.some((panel) => panel.hidden && panel.text.includes('Summary panel content')) &&
    tabState.panels.some((panel) => !panel.hidden && panel.text.includes('Details panel content')),
    `arrow-key tab activation or linked panel state failed: ${JSON.stringify(tabState)}`);
  await key("Home", "Home", 36);
  check(await evaluate("document.activeElement?.dataset.tabId === 'summary'"), "Home did not focus the first enabled tab");
  await key("End", "End", 35);
  check(await evaluate("document.activeElement?.dataset.tabId === 'details'"), "End did not focus the last enabled tab");
  await evaluate(`(() => {
    const h = window.__signedUiPrimitiveHandles;
    h.table.setRows([["updated", "still text"]]);
    h.status.setBusy(true);
    h.status.setMessage("Updating");
    h.alert.setSeverity("error");
    h.progress.setValue(2);
  })()`);
  check(await evaluate(`(() => {
    const section = document.querySelector('[data-signed-ui-primitives]');
    return section.querySelector('tbody td')?.textContent === 'updated' &&
      section.querySelector('[role=status]')?.getAttribute('aria-busy') === 'true' &&
      section.querySelector('[role=status]')?.textContent === 'Updating' &&
      section.querySelector('[role=alert]')?.dataset.severity === 'error' &&
      section.querySelector('progress')?.value === 2;
  })()`), "feedback state updates or table row updates failed");

  await evaluate(`(() => {
    window.__signedUiPrimitiveHandles.status.setMessage({
      source: "Ready after update", translations: { fr: "Prêt après mise à jour" },
    });
    const locale = document.querySelector('#global-locale');
    locale.value = 'fr';
    locale.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  await wait(`document.querySelector('#global-locale')?.value === 'fr' &&
    document.querySelector('[data-signed-ui-primitives] [role=tablist] [data-tab-id=summary]')?.textContent === 'Résumé' &&
    document.querySelector('[data-signed-ui-primitives] .plugin-ui-card h2')?.textContent === 'Carte des mesures' &&
    document.querySelector('[data-signed-ui-primitives] table caption')?.textContent === 'Résultats de qualification' &&
    document.querySelector('[data-signed-ui-primitives] table th')?.textContent === 'Nom' &&
    document.querySelector('[data-signed-ui-primitives] dialog h2')?.textContent === 'Dialogue de qualification' &&
    document.querySelector('[data-signed-ui-primitives] [role=status]')?.textContent === 'Prêt après mise à jour'`,
    "locale refresh for tabs, layout, and feedback");
  await evaluate("location.hash = 'overview'");
  await wait(`window.__signedUiPrimitiveLifecycle?.aborted === true &&
    !document.querySelector('[data-signed-ui-primitives]') &&
    !document.querySelector('dialog[aria-modal=true]')`,
    "page abort cleanup of dialog, tabs, layout, and feedback");
  check(await evaluate(`(() => {
    const h = window.__signedUiPrimitiveHandles;
    h.table.setRows([["after abort", "must stay removed"]]);
    h.status.setMessage("after abort");
    h.progress.setValue(99);
    h.card.append(document.createElement('p'));
    return !document.querySelector('[data-signed-ui-primitives]') &&
      !h.table.element.isConnected && !h.card.element.isConnected && !h.status.element.isConnected &&
      !h.alert.element.isConnected && !h.progress.element.isConnected && !h.dialog.element.isConnected &&
      !h.tabs.element.isConnected;
  })()`), "aborted page controls accepted a late update or remained connected");

  await debuggerClient.command("Page.navigate", { url: "about:blank" }, sessionId);
  await wait("location.href === 'about:blank'", "blank page before changing pixel density");
  themeResponses.delete("scroll-masthead.webp");
  await setDesktopViewport(1920, 1080, 2);
  await debuggerClient.command("Page.navigate", { url: `${options.origin}/#overview` }, sessionId);
  await wait(`(() => {
    const hero = document.querySelector('.console-masthead');
    return window.devicePixelRatio === 2 && window.innerWidth === 1920 && window.innerHeight === 1080 &&
      hero && getComputedStyle(hero, '::before').backgroundImage.includes('scroll-masthead.webp');
  })()`, "signed illustrated masthead at 3840 by 2160 physical pixels");
  await verifyThemeAssets(options.themeAssets.filter(({ name }) => name === "scroll-masthead.webp"));

  check(await evaluate(`(async () => {
    const header = document.querySelector('.console-masthead');
    const art = getComputedStyle(header, '::before');
    const image = new Image();
    image.src = new URL('scroll-masthead.webp', document.baseURI);
    await image.decode();
    return image.naturalWidth >= parseFloat(art.width) * devicePixelRatio &&
      image.naturalHeight >= parseFloat(art.height) * devicePixelRatio;
  })()`), "masthead image has enough source pixels for the high-density header");

  console.log(`Verified signed UI primitives asset at ${primitives.path}: same-origin digest route, matching signed bytes, JavaScript content type, and restrictive host CSP.`);
  console.log("Verified dialog keyboard focus/escape, tabs keyboard navigation, semantic layout, live feedback, locale updates, feature fallback, and page-abort cleanup.");
  console.log(`Verified signed showcase CSS at ${showcaseCss.path}: same-origin, matching bytes, text/css, rendered under the host CSP.`);
  console.log("Verified shared showcase tabs, checkbox, search, tooltip, badges, table, painted summary cards, sparklines, and dialog footer actions under the signed CSP.");
  console.log(`Verified selected signed web digest ${selectedDigest}: theme stylesheets and artwork matched packaged bytes and loaded under the signed CSP.`);
  console.log("PASS: signed web asset delivery, packaged UI primitives under CSP, extension keyboard use and host call, generic control keyboard/save, locale update, and navigation cleanup/remount");
} finally {
  if (target) {
    try { await debuggerClient.command("Target.closeTarget", { targetId: target.targetId }); } catch {}
  }
  debuggerClient.close();
}
