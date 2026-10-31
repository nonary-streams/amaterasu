// CDP drives only the signed production page. The Python owner supplies the
// private debugger endpoint and coordinates native-host/session administration.
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";

const check = (condition, message) => { if (!condition) throw new Error(message); };
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

class Debugger {
  sequence = 0;
  pending = new Map();
  listeners = new Set();

  async connect(address) {
    const url = new URL(address);
    check(url.protocol === "ws:" && url.hostname === "127.0.0.1" &&
      Number(url.port) > 0 && Number(url.port) <= 65535 &&
      /^\/devtools\/browser\/[a-f0-9-]+$/.test(url.pathname), "unexpected debugger address");
    this.socket = new WebSocket(url);
    this.socket.addEventListener("message", ({ data }) => {
      const message = JSON.parse(data);
      if (message.id !== undefined) {
        const pending = this.pending.get(message.id);
        if (pending) {
          this.pending.delete(message.id);
          clearTimeout(pending.timeout);
          if (message.error) pending.reject(new Error(`debugger ${pending.method} failed`));
          else pending.resolve(message.result);
        }
      } else {
        for (const listener of this.listeners) listener(message);
      }
    });
    this.socket.addEventListener("close", () => {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timeout);
        pending.reject(new Error("owned browser debugger closed"));
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
        reject(new Error(`debugger ${method} timed out`));
      }, 10000);
      this.pending.set(id, { resolve, reject, timeout, method });
      this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }

  disconnect() { this.socket?.close(); }
}

let debuggerClient;
let origin;
let first;
let peer;
let followPage;
let rememberedDigest;
const observed = [];
const accessName = "__Secure-amaterasu-access";
const refreshName = "__Secure-amaterasu-refresh";
const digest = (value) => createHash("sha256").update(value).digest("hex");

async function page(address) {
  const { targetId } = await debuggerClient.command("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await debuggerClient.command("Target.attachToTarget", { targetId, flatten: true });
  const target = { targetId, sessionId };
  const requests = new Map();
  debuggerClient.listeners.add((event) => {
    if (event.sessionId !== sessionId) return;
    if (event.method === "Network.requestWillBeSent") {
      const request = event.params.request;
      const path = new URL(request.url).pathname;
      if (!path.startsWith("/v1/")) return;
      const headers = Object.fromEntries(Object.entries(request.headers).map(([name, value]) => [name.toLowerCase(), value]));
      // Never retain postData, Cookie, Authorization values or response headers.
      requests.set(event.params.requestId, { path, method: request.method,
        csrf: headers["x-amaterasu-csrf"] === "1", authorization: "authorization" in headers });
    } else if (event.method === "Network.responseReceived") {
      const request = requests.get(event.params.requestId);
      if (request) observed.push({ ...request, status: event.params.response.status });
    }
  });
  await debuggerClient.command("Page.enable", {}, sessionId);
  await debuggerClient.command("Runtime.enable", {}, sessionId);
  await debuggerClient.command("Network.enable", {}, sessionId);
  await debuggerClient.command("Page.navigate", { url: address }, sessionId);
  return target;
}

async function evaluate(target, expression) {
  const reply = await debuggerClient.command("Runtime.evaluate", {
    expression, awaitPromise: true, returnByValue: true,
  }, target.sessionId);
  check(!reply.exceptionDetails, "production-page evaluation failed");
  return reply.result.value;
}

async function wait(target, expression, description) {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    try {
      if (await evaluate(target, expression)) return;
    } catch (error) {
      // A navigation can destroy the previous execution context between polls.
      // Retry only protocol evaluation failure; a permanent failure still reaches
      // the same bounded deadline instead of satisfying a DOM assertion.
      if (error.message !== "debugger Runtime.evaluate failed") throw error;
    }
    await delay(40);
  }
  throw new Error(`browser timed out waiting for ${description}`);
}

async function reload(target) {
  await evaluate(target, "window.__signedQualificationPreviousDocument = true");
  await debuggerClient.command("Page.reload", {}, target.sessionId);
  await wait(target, "window.__signedQualificationPreviousDocument === undefined", "new production document after reload");
}

const signedIn = (target) => wait(target, "!!document.querySelector('.shell')", "signed production shell");
const signedOut = (target) => wait(target, "!!document.querySelector('input[name=username]')",
  target === peer ? "peer production sign-in form" : "first production sign-in form");
const policyReady = (target) => wait(target, "!!document.querySelector('select[name=origin_web_ui_allowed]:not(:disabled)')", "management policy editor");

async function login(target, username, password) {
  await signedOut(target);
  await evaluate(target, `(() => {
    const username = document.querySelector('input[name=username]');
    username.value = ${JSON.stringify(username)};
    document.querySelector('input[name=password]').value = ${JSON.stringify(password)};
    document.querySelector('input[name=remember_me]').checked = true;
    username.closest('form').requestSubmit();
  })()`);
  await signedIn(target);
}

async function button(target, text) {
  const clicked = await evaluate(target, `(() => {
    const button = [...document.querySelectorAll('button')].find(element => element.textContent.trim() === ${JSON.stringify(text)});
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`);
  check(clicked, `production ${text} control unavailable`);
}

async function policy(target) {
  return evaluate(target, `(async () => {
    const response = await fetch('/v1/management_settings/get', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:'{}'});
    const reply = await response.json();
    return {status:response.status, ok:reply.ok, revision:reply.revision, selection:reply.values?.origin_web_ui_allowed, code:reply.code};
  })()`);
}

async function save(target, selection) {
  await policyReady(target);
  await evaluate(target, `(() => {
    const select = document.querySelector('select[name=origin_web_ui_allowed]');
    select.value = ${JSON.stringify(selection)};
    select.closest('form').requestSubmit();
  })()`);
}

async function cookieState() {
  const { cookies } = await debuggerClient.command("Storage.getCookies");
  const access = cookies.find((cookie) => cookie.name === accessName);
  const refresh = cookies.find((cookie) => cookie.name === refreshName);
  check(access && refresh, "native browser cookie pair missing");
  for (const cookie of [access, refresh]) {
    check(cookie.domain === "127.0.0.1" && cookie.secure && cookie.httpOnly &&
      cookie.sameSite === "Strict", "native cookie security attributes differ");
  }
  check(access.path === "/v1" && access.session, "access cookie is not scoped/session-only");
  check(refresh.path === "/v1/auth/browser" && !refresh.session &&
    refresh.expires > Date.now() / 1000, "remembered refresh cookie is not persistent/scoped");
  for (const target of [first, peer].filter(Boolean)) {
    const storage = await evaluate(target, `JSON.stringify({local:[...Object.keys(localStorage)].map(key=>localStorage.getItem(key)),
      session:[...Object.keys(sessionStorage)].map(key=>sessionStorage.getItem(key))})`);
    check(!/ama_[a-f0-9]{64}/.test(storage) && !storage.includes(access.value) &&
      !storage.includes(refresh.value), "browser storage contains token secrets");
  }
  // Root '/' is outside cookie paths. Use a top-level matching-path document;
  // signed assets prohibit framing and we do not weaken their CSP.
  const probe = await page(`${origin}/v1/auth/browser/script-cookie-probe`);
  await wait(probe, "location.pathname === '/v1/auth/browser/script-cookie-probe'", "matching cookie-path document");
  check(await evaluate(probe, "document.cookie === ''"), "HttpOnly cookies visible to matching-path script");
  await debuggerClient.command("Target.closeTarget", { targetId: probe.targetId });
  return { access: digest(access.value), refresh: digest(refresh.value) };
}

async function initial(options) {
  origin = options.origin;
  check(new URL(origin).hostname === "127.0.0.1" && new URL(origin).protocol === "https:", "native origin must be loopback HTTPS");
  debuggerClient = new Debugger();
  await debuggerClient.connect(options.debugger);
  const command = await debuggerClient.command("Browser.getBrowserCommandLine");
  check(command.arguments.includes(`--user-data-dir=${options.profile}`) &&
    command.arguments.includes(`--ignore-certificate-errors-spki-list=${options.spki}`), "owned browser profile/SPKI selection differs");
  check(!command.arguments.some((argument) => ["--ignore-certificate-errors", "--disable-web-security", "--no-sandbox"].includes(argument.split("=", 1)[0])),
    "owned browser enabled a broad security bypass");
  const rejected = await page("about:blank");
  const navigation = await debuggerClient.command("Page.navigate", { url: options.rejected_origin }, rejected.sessionId);
  check(navigation.errorText === "net::ERR_CERT_AUTHORITY_INVALID", "SPKI exception admitted an unrelated untrusted TLS key");
  await debuggerClient.command("Target.closeTarget", { targetId: rejected.targetId });
  first = await page(`${origin}/#settings`);
  await login(first, options.username, options.password);
  await policyReady(first);
  await evaluate(first, `(() => {
    const category = document.querySelector('select[aria-label="Settings category"]');
    category.value = 'access';
    category.dispatchEvent(new Event('change', { bubbles: true }));
    const select = document.querySelector('select[name=origin_web_ui_allowed]');
    window.__draftOriginal = select.value;
    select.value = select.value === 'pc' ? 'lan' : 'pc';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    const search = document.querySelector('input[name=settings_search]');
    search.value = 'origin_web_ui_allowed';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    window.__searchPreservedDraft = select.value !== window.__draftOriginal &&
      select.getClientRects().length > 0 &&
      document.querySelector('.settings-area-management button[type=submit]').getClientRects().length > 0 &&
      document.querySelector('.settings-area-csrf').hidden;
    search.value = '';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    window.__searchPreservedDraft &&= select.value !== window.__draftOriginal && select.getClientRects().length > 0;
    window.__draftConfirm = window.confirm;
    window.__draftPromptCount = 0;
    window.confirm = () => { window.__draftPromptCount++; return false; };
    location.hash = 'overview';
  })()`);
  await wait(first, "window.__draftPromptCount === 1 && location.hash === '#settings/access'", "declined draft navigation");
  check(await evaluate(first, `(() => {
    const select = document.querySelector('select[name=origin_web_ui_allowed]');
    const refresh = [...document.querySelectorAll('.page-heading button')].find(element => element.textContent.trim() === 'Refresh');
    const unload = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(unload);
    return window.__searchPreservedDraft && select.value !== window.__draftOriginal && refresh.disabled && unload.defaultPrevented;
  })()`), "settings draft was not protected from navigation, refresh and reload");
  await evaluate(first, `(() => {
    const select = document.querySelector('select[name=origin_web_ui_allowed]');
    select.value = window.__draftOriginal;
    select.dispatchEvent(new Event('change', { bubbles: true }));
    window.confirm = window.__draftConfirm;
  })()`);
  await wait(first, "![...document.querySelectorAll('.page-heading button')].find(element => element.textContent.trim() === 'Refresh').disabled", "clean settings after reverting draft");
  check((await policy(first)).revision === 0, "initial native policy revision differs");
  const overview = await page(`${origin}/#overview`);
  await signedIn(overview);
  await wait(overview, `(() => {
    const activity = document.querySelector('.overview-activity-area');
    return activity && !activity.textContent.includes('Loading host stream activity');
  })()`, "native overview activity");
  await wait(overview, `(() => {
    const telemetry = document.querySelector('.overview-host-metrics');
    return telemetry && !telemetry.textContent.includes('Loading live host metrics');
  })()`, "native overview telemetry");
  check(await evaluate(overview, "document.querySelector('.overview-activity-area').textContent.includes('No active streams reported')"),
    "native overview did not report the stream owner's empty status");
  check(observed.some((request) => request.path === "/v1/live-streams/status" && request.status === 200),
    "native overview did not read live stream status");
  await debuggerClient.command("Target.closeTarget", { targetId: overview.targetId });
  const library = await page(`${origin}/#catalog`);
  await signedIn(library);
  const catalogCreated = await evaluate(library, `(async () => {
    const headers = {'Content-Type':'application/json','X-Amaterasu-CSRF':'1'};
    const request = async (route, body) => {
      const response = await fetch('/v1/catalog/' + route, {method:'POST',credentials:'same-origin',headers,body:JSON.stringify(body)});
      return response.json();
    };
    let current = await request('list',{limit:1});
    if (!current.ok) return false;
    for (const suffix of ['alpha','bravo']) {
      current = await request('create',{expected_revision:current.revision,entry:{name:'UI selection ' + suffix,description:'',tags:[],launch:{kind:'application',path:'/fixture/' + suffix}}});
      if (!current.ok) return false;
    }
    current = await request('create',{expected_revision:current.revision,entry:{name:'UI desktop fixture',description:'',tags:[],
      'display-output':'Virtual-1','virtual-screen':false,launch:{kind:'desktop',exit_timeout_seconds:30,working_directory:'/tmp',
        preparation:[{run:{executable:'/usr/bin/true'}}]}}});
    if (!current.ok || !current.id) return false;
    window.__desktopFixtureId = current.id;
    for (let number = 35; number >= 1; number--) {
      const name = 'UI sorting ' + String(number).padStart(2,'0');
      current = await request('create',{expected_revision:current.revision,entry:{name,description:'',tags:[],launch:{kind:'application',path:'/fixture/sort/' + number}}});
      if (!current.ok) return false;
    }
    return true;
  })()`);
  check(catalogCreated, "native selection fixture could not create catalog entries");
  const steamConfigured = await evaluate(library, `(async () => {
    const post = async (route, body = {}) => {
      const response = await fetch('/v1/steam/' + route, {method:'POST',credentials:'same-origin',
        headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'},body:JSON.stringify(body)});
      return response.json();
    };
    const current = await post('settings');
    if (!current.ok) return false;
    const saved = await post('settings_update', {enabled:true,auto_sync:false,exclude_games:current.values.exclude_games,
      sync_all_installed:true,include_tools:true,expected_revision:current.revision});
    if (!saved.ok) return false;
    return (await post('refresh')).ok;
  })()`);
  check(steamConfigured, "native artwork fixture could not start Steam discovery");
  await wait(library, `(async () => {
    const response = await fetch('/v1/steam/list',{method:'POST',credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'},body:JSON.stringify({limit:16})});
    const result = await response.json();
    return result.ok && result.games?.some(game => game.app_id === 730 && game.artwork_revision);
  })()`, "native Steam artwork discovery");
  check(await evaluate(library, `(async () => {
    const response = await fetch('/v1/steam/force_sync',{method:'POST',credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'},body:'{}'});
    return (await response.json()).ok;
  })()`), "native Steam artwork fixture sync was not accepted");
  await wait(library, `(async () => {
    const response = await fetch('/v1/catalog/search',{method:'POST',credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'},body:JSON.stringify({query:'Example',limit:16})});
    const result = await response.json();
    return result.ok && result.entries?.some(row => row.entry.name === 'Example' && row.entry.artwork?.provider === 'steam');
  })()`, "native Steam artwork catalog sync");
  await evaluate(library, `(() => {
    const search = document.querySelector('input[name=search]');
    search.value = 'Example'; search.dispatchEvent(new Event('input',{bubbles:true}));
  })()`);
  await wait(library, `!!document.querySelector('[aria-label="Edit Example"]')`, "native Steam cover entry");
  await evaluate(library, `document.querySelector('[aria-label="Edit Example"]').click()`);
  await wait(library, `(() => { const cover = document.querySelector('.catalog-editor-cover');
    return cover?.querySelector('img')?.naturalWidth === 600 && cover.querySelector('.catalog-editor-artwork-fallback')?.hidden; })()`, "native editor provider cover");
  await button(library, "Cancel");
  await evaluate(library, `(() => {
    const search = document.querySelector('input[name=search]');
    search.value = 'UI sorting';
    search.dispatchEvent(new Event('input',{bubbles:true}));
  })()`);
  await wait(library, `document.querySelector('.catalog-open')?.getAttribute('aria-label') === 'Edit UI sorting 01'`, "native global ascending sort");
  check(await evaluate(library, `!!document.querySelector('[aria-label="Edit UI sorting 12"]') && !document.querySelector('[aria-label="Edit UI sorting 13"]')`), "native ascending sort did not page the whole library");
  await evaluate(library, `(() => {
    const sort = document.querySelector('select[name=sort]');
    sort.value = 'name-desc';
    sort.dispatchEvent(new Event('change',{bubbles:true}));
  })()`);
  await wait(library, `document.querySelector('.catalog-open')?.getAttribute('aria-label') === 'Edit UI sorting 35'`, "native global descending sort");
  check(await evaluate(library, `(() => { const params = new URLSearchParams(location.hash.split('?')[1]); return params.get('q') === 'UI sorting' && params.get('sort') === 'name-desc'; })()`), "library link did not retain search and sort");
  const sharedLibrary = await page(await evaluate(library, "location.href"));
  await signedIn(sharedLibrary);
  await wait(sharedLibrary, `document.querySelector('input[name=search]')?.value === 'UI sorting' && document.querySelector('select[name=sort]')?.value === 'name-desc' && document.querySelector('.catalog-open')?.getAttribute('aria-label') === 'Edit UI sorting 35'`, "shared library link restores search and sort");
  await debuggerClient.command("Target.closeTarget", { targetId: sharedLibrary.targetId });
  await evaluate(library, `(() => {
    const address = new URL(location.href);
    address.hash = 'catalog?q=UI+selection';
    history.pushState({}, '', address);
    dispatchEvent(new PopStateEvent('popstate'));
  })()`);
  await wait(library, `document.querySelector('input[name=search]')?.value === 'UI selection' && !!document.querySelector('[aria-label="Edit UI selection alpha"]')`, "library history restores a different search");
  await evaluate(library, "history.back()");
  await wait(library, `document.querySelector('input[name=search]')?.value === 'UI sorting' && document.querySelector('select[name=sort]')?.value === 'name-desc' && document.querySelector('.catalog-open')?.getAttribute('aria-label') === 'Edit UI sorting 35'`, "library history restores search and sort");
  await button(library, "Next →");
  await wait(library, `document.querySelector('.catalog-open')?.getAttribute('aria-label') === 'Edit UI sorting 23'`, "native sorted second page");
  await evaluate(library, `(() => {
    const sort = document.querySelector('select[name=sort]');
    sort.value = 'name';
    sort.dispatchEvent(new Event('change',{bubbles:true}));
  })()`);
  await evaluate(library, `(() => {
    const search = document.querySelector('input[name=search]');
    search.value = '/fixture/alpha';
    search.dispatchEvent(new Event('input',{bubbles:true}));
  })()`);
  await wait(library, `!!document.querySelector('[aria-label="Edit UI selection alpha"]') && !document.querySelector('[aria-label="Edit UI selection bravo"]')`, "native library command-path search");
  await evaluate(library, `(() => {
    const search = document.querySelector('input[name=search]');
    search.value = 'UI selection';
    search.dispatchEvent(new Event('input',{bubbles:true}));
  })()`);
  await wait(library, `!!document.querySelector('[aria-label="Edit UI selection alpha"]') && !!document.querySelector('[aria-label="Edit UI selection bravo"]')`, "native library search results");
  check(await evaluate(library, `(() => {
    const first = document.querySelector('[aria-label="Edit UI selection alpha"]');
    first.focus();
    first.dispatchEvent(new KeyboardEvent('keydown',{key:'F10',shiftKey:true,bubbles:true,cancelable:true}));
    const menu = first.closest('.app-card').querySelector('details');
    if (!menu.open || !menu.contains(document.activeElement)) return false;
    document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true,cancelable:true}));
    if (menu.open || document.activeElement !== first) return false;
    first.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,button:2}));
    if (!menu.open) return false;
    document.body.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true}));
    return !menu.open;
  })()`), "native library context actions or focus restoration failed");
  const selected = await evaluate(library, `(() => {
    const first = document.querySelector('[aria-label="Edit UI selection alpha"]');
    first.focus();
    first.dispatchEvent(new KeyboardEvent('keydown',{key:' ',bubbles:true,cancelable:true}));
    first.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',shiftKey:true,bubbles:true,cancelable:true}));
    return document.body.textContent.includes('2 selected') &&
      !!document.querySelector('[aria-label="Deselect UI selection alpha"][aria-pressed="true"]') &&
      !!document.querySelector('[aria-label="Deselect UI selection bravo"][aria-pressed="true"]');
  })()`);
  check(selected, "native library keyboard selection or range extension failed");
  await evaluate(library, `(() => {
    const search = document.querySelector('input[name=search]');
    search.value = 'UI selection bravo';
    search.dispatchEvent(new Event('input',{bubbles:true}));
  })()`);
  await wait(library, `!!document.querySelector('[aria-label="Deselect UI selection bravo"]') && !document.querySelector('[aria-label="Edit UI selection alpha"]')`, "native library selection after filtering");
  check(await evaluate(library, "document.body.textContent.includes('2 selected')"), "library search lost selection");
  await button(library, "Clear selection");
  check(await evaluate(library, "document.querySelector('.library-selection').hidden"), "library selection did not clear");
  await evaluate(library, `(() => {
    const search = document.querySelector('input[name=search]');
    search.value = 'UI desktop fixture';
    search.dispatchEvent(new Event('input',{bubbles:true}));
  })()`);
  await wait(library, `!!document.querySelector('[aria-label="Edit UI desktop fixture"]')`, "native desktop entry");
  await evaluate(library, `document.querySelector('[aria-label="Edit UI desktop fixture"]').click()`);
  await wait(library, `document.querySelector('select[name=kind]')?.value === 'desktop'`, "native desktop editor");
  check(await evaluate(library, `!!document.querySelector('.catalog-editor-cover .catalog-editor-artwork') && document.querySelector('.catalog-editor-cover').textContent.includes('Cover unavailable')`), "application editor did not show the cover fallback");
  await evaluate(library, `(() => {
    const description = document.querySelector('.editor-form textarea[name=description]');
    description.value = 'Edited desktop description';
    description.dispatchEvent(new Event('input', {bubbles:true}));
    window.__libraryOriginalHash = location.hash;
    window.__libraryConfirm = window.confirm;
    window.__libraryPromptCount = 0;
    window.confirm = () => { window.__libraryPromptCount++; return false; };
    location.hash = 'overview';
  })()`);
  await wait(library, `window.__libraryPromptCount === 1 && location.hash === window.__libraryOriginalHash`, "declined library draft navigation retains its link");
  check(await evaluate(library, `document.querySelector('.editor-form textarea[name=description]')?.value === 'Edited desktop description'`), "declined library navigation lost the application draft");
  await evaluate(library, `(() => {
    window.confirm = window.__libraryConfirm;
    const form = document.querySelector('.editor-form');
    form.requestSubmit();
  })()`);
  await wait(library, "document.body.textContent.includes('Application updated.')", "native desktop edit confirmation");
  const desktopReadback = await evaluate(library, `(async () => {
    const id = window.__desktopFixtureId; delete window.__desktopFixtureId;
    const response = await fetch('/v1/catalog/get',{method:'POST',credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'},body:JSON.stringify({id})});
    const result = await response.json();
    const entry = result.entry;
    return result.ok && entry?.description === 'Edited desktop description' && entry.launch?.kind === 'desktop' &&
      entry.launch?.exit_timeout_seconds === 30 && entry.launch?.working_directory === '/tmp' &&
      entry.launch?.preparation?.[0]?.run?.executable === '/usr/bin/true' &&
      entry['display-output'] === 'Virtual-1' && entry['virtual-screen'] === false;
  })()`);
  check(desktopReadback, "desktop editor changed its process-less launch or display settings");
  await debuggerClient.command("Target.closeTarget", { targetId: library.targetId });
  const history = await page(`${origin}/#sessions`);
  await signedIn(history);
  await wait(history, "document.body.textContent.includes('No active streams reported.')", "native active stream status");
  const liveWindow = await evaluate(history, `(async () => {
    let pages = 0;
    const api = { async post(route, payload) {
      if (route === 'history/timeline_page') pages++;
      const response = await fetch('/v1/' + route, { method:'POST', credentials:'same-origin',
        headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:JSON.stringify(payload) });
      const body = await response.json();
      if (!response.ok || body.ok !== true) throw new Error('Live history request failed');
      return body;
    }};
    const first = await Amaterasu.loadLiveSampleSnapshot(api, 'signed-history-fixture', 30, null, () => true);
    const initialPages = pages;
    const second = await Amaterasu.loadLiveSampleSnapshot(api, 'signed-history-fixture', 30, first, () => true);
    return { count:second.samples.length, first:second.samples[0].frames_sent,
      last:second.samples.at(-1).frames_sent, initialPages, extraPages:pages-initialPages };
  })()`);
  check(liveWindow.count === 30 && liveWindow.first === 180 && liveWindow.last === 1920 &&
    liveWindow.initialPages === 1 && liveWindow.extraPages === 0, "native bounded live history or snapshot reuse failed");
  await wait(history, `!![...document.querySelectorAll('.history-card')].find(row => row.textContent.includes('Fixture game') && [...row.querySelectorAll('button')].some(button => button.textContent.trim() === 'Details'))`, "native recorded session fixture");
  await evaluate(history, `(() => {
    const row = [...document.querySelectorAll('.history-card')].find(element => element.textContent.includes('Fixture game'));
    [...row.querySelectorAll('button')].find(button => button.textContent.trim() === 'Details').click();
  })()`);
  await wait(history, `document.body.textContent.includes('33 of 33 samples') && document.querySelectorAll('.recorded-session-chart-grid svg').length === 4`, "native complete retained performance charts");
  check(await evaluate(history, `!![...document.querySelectorAll('.recorded-session-chart-grid svg')].find(svg => svg.getAttribute('aria-label').includes('Encoder latency; 1 recorded value, latest 4.2 ms')) && document.body.textContent.includes('client delivery is not confirmed')`), "native recorded charts misrepresented latency or delivery");
  await button(history, "3 min");
  check(await evaluate(history, "document.body.textContent.includes('19 of 33 returned samples')"), "native history range did not use returned timestamps");
  await debuggerClient.command("Browser.setDownloadBehavior", {behavior:"allow",downloadPath:options.download_directory});
  await button(history, "Download retained history (JSON)");
  await button(history, "Close details");
  check(await evaluate(history, "!document.querySelector('.recorded-session-detail')"), "native history detail did not close");
  await wait(history, `!![...document.querySelectorAll('.history-card')].find(card => card.textContent.includes('Group client') && card.textContent.includes('2 nearby streams'))`, "native grouped history card");
  await evaluate(history, `(() => {
    const card = [...document.querySelectorAll('.history-card')].find(element => element.textContent.includes('Group client'));
    [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Details').click();
  })()`);
  await wait(history, `document.body.textContent.includes('4 of 4 samples') && document.body.textContent.includes('reconnect is not confirmed')`, "native grouped history details");
  check(await evaluate(history, `(() => {
    const chart = [...document.querySelectorAll('.recorded-session-chart-grid svg')].find(svg => svg.getAttribute('aria-label').startsWith('Accepted video rate'));
    return chart && chart.querySelectorAll('polyline').length === 2 && document.body.textContent.includes('signed-group-old') && document.body.textContent.includes('signed-group-new');
  })()`), "native grouped chart merged stream identities");
  await button(history, "Close details");
  await debuggerClient.command("Target.closeTarget", { targetId: history.targetId });
  const logs = await page(`${origin}/#diagnostics`);
  await signedIn(logs);
  await wait(logs, "document.querySelector('.host-log-output')?.textContent.includes('signed browser host log marker')", "native host log snapshot");
  check(await evaluate(logs, `(() => {
    const search = document.querySelector('input[name=host_log_search]');
    search.value = 'signed browser host log marker';
    search.dispatchEvent(new Event('input', {bubbles:true}));
    return document.body.textContent.includes('Match 1 of 1') &&
      document.querySelector('.host-log-output').textContent.includes('signed browser host log marker');
  })()`), "native host log search did not find the recorded marker");
  await debuggerClient.command("Browser.setDownloadBehavior", {behavior:"allow",downloadPath:options.download_directory});
  await button(logs, "Download support bundle");
  await wait(logs, "/[0-9]+ retained sessions? included/.test(document.body.textContent)", "native retained support bundle download");
  const forged = await evaluate(logs, `(async () => {
    const response = await fetch('/v1/host-logs/open', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:JSON.stringify({owner:'forged',path:'/etc/passwd'})});
    return response.status;
  })()`);
  check(forged === 400, "native host log route accepted a forged owner or path");
  const forgedExport = await evaluate(logs, `(async () => {
    const response = await fetch('/v1/host-logs/export_start', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:JSON.stringify({path:'/etc/passwd'})});
    return response.status;
  })()`);
  check(forgedExport === 400, "native support export accepted a caller-supplied path");
  check(observed.some((request) => request.path === "/v1/host-logs/open" && request.status === 200) &&
    observed.some((request) => request.path === "/v1/host-logs/read" && request.status === 200),
    "native host log snapshot requests missing");
  await debuggerClient.command("Target.closeTarget", { targetId: logs.targetId });
  const tokenPage = await page(`${origin}/#tokens`);
  await signedIn(tokenPage);
  await wait(tokenPage, `!!document.querySelector('input[name="token_scope_catalog.read"]')`, "native token permission form");
  await evaluate(tokenPage, `(() => {
    for (const name of ['token_scope_catalog.read','token_scope_configuration.write']) {
      document.querySelector('input[name="' + name + '"]').checked = true;
    }
    [...document.querySelectorAll('form')].find(form => form.querySelector('input[name="token_scope_catalog.read"]')).requestSubmit();
  })()`);
  await wait(tokenPage, "!!document.querySelector('textarea[aria-label=\"New API token\"]') && !![...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Revoke')", "native token issuance and list");
  const tokenChecks = await evaluate(tokenPage, `(async () => {
    const secret = document.querySelector('textarea[aria-label="New API token"]').value;
    window.__qualifiedApiToken = secret;
    const headers = {'Content-Type':'application/json','Authorization':'Bearer ' + secret};
    const read = await fetch('/v1/catalog/list', {method:'POST',headers,body:JSON.stringify({limit:1})});
    const mint = await fetch('/v1/tokens/issue', {method:'POST',headers,body:JSON.stringify({scopes:['catalog.read'],ttl_seconds:60})});
    const sessions = await fetch('/v1/account-sessions/list', {method:'POST',headers,body:'{}'});
    const storage = JSON.stringify({local:Object.values(localStorage),session:Object.values(sessionStorage)});
    return {format:/^ama_[a-f0-9]{64}$/.test(secret),read:read.status,mint:mint.status,sessions:sessions.status,stored:storage.includes(secret)};
  })()`);
  check(tokenChecks.format && tokenChecks.read === 200 && tokenChecks.mint === 403 && tokenChecks.sessions === 403 && !tokenChecks.stored,
    "native token scope, session-only issuance, or secret storage boundary failed");
  await evaluate(tokenPage, `(() => {
    window.confirm = () => true;
    [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Revoke').click();
  })()`);
  await wait(tokenPage, "document.body.textContent.includes('No issued tokens')", "native token revocation readback");
  const revoked = await evaluate(tokenPage, `(async () => {
    const secret = window.__qualifiedApiToken; delete window.__qualifiedApiToken;
    const response = await fetch('/v1/catalog/list', {method:'POST',headers:{'Content-Type':'application/json','Authorization':'Bearer ' + secret},body:JSON.stringify({limit:1})});
    [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Done').click();
    return response.status;
  })()`);
  check(revoked === 403 || revoked === 401, "revoked native token remained authorized");
  const delegated = await evaluate(tokenPage, `(async () => {
    const cookieHeaders = {'Content-Type':'application/json','X-Amaterasu-CSRF':'1'};
    const issue = await fetch('/v1/tokens/issue', {method:'POST',credentials:'same-origin',headers:cookieHeaders,
      body:JSON.stringify({scopes:['tokens.manage'],ttl_seconds:60})});
    const value = await issue.json();
    if (!value.ok || !value.bearer || !value.token_id) return {issued:false};
    const denied = await fetch('/v1/tokens/list', {method:'POST',headers:{'Content-Type':'application/json','Authorization':'Bearer ' + value.bearer},body:'{}'});
    const cleanup = await fetch('/v1/tokens/revoke', {method:'POST',credentials:'same-origin',headers:cookieHeaders,body:JSON.stringify({token_id:value.token_id})});
    return {issued:true,denied:denied.status,cleanup:cleanup.status};
  })()`);
  check(delegated.issued && delegated.denied === 403 && delegated.cleanup === 200,
    "token with management scope bypassed the administrator-session requirement");
  await debuggerClient.command("Target.closeTarget", { targetId: tokenPage.targetId });
  const maintenance = await page(`${origin}/#maintenance`);
  await signedIn(maintenance);
  await wait(maintenance, "document.body.textContent.includes('This session')", "current native browser session");
  const peerCreated = await evaluate(maintenance, `(async () => {
    const response = await fetch('/v1/auth/login', {method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({username:${JSON.stringify(options.username)},password:${JSON.stringify(options.password)},remember_me:true})});
    const result = await response.json();
    if (!result.ok || !result.bearer || !result.refresh_token) return false;
    window.__qualifiedPeerSession = {bearer:result.bearer,refresh:result.refresh_token};
    return true;
  })()`);
  check(peerCreated, "second native browser session could not be created");
  await button(maintenance, "Refresh sessions");
  await wait(maintenance, "document.querySelectorAll('.browser-session-card').length === 2", "both native browser sessions");
  const selfProtection = await evaluate(maintenance, `(async () => {
    const headers = {'Content-Type':'application/json','X-Amaterasu-CSRF':'1'};
    const response = await fetch('/v1/account-sessions/list', {method:'POST',credentials:'same-origin',headers,body:'{}'});
    const sessions = (await response.json()).sessions;
    const current = sessions.find(session => session.current);
    const denied = await fetch('/v1/account-sessions/revoke', {method:'POST',credentials:'same-origin',headers,body:JSON.stringify({session_id:current.session_id})});
    return (await denied.json()).code;
  })()`);
  check(selfProtection === 'current_session', "current native browser session could be revoked through maintenance");
  const peerRotated = await evaluate(maintenance, `(async () => {
    const peer = window.__qualifiedPeerSession;
    const response = await fetch('/v1/auth/refresh', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({refresh_token:peer.refresh})});
    const result = await response.json();
    if (!result.ok || !result.bearer || !result.refresh_token) return false;
    window.__qualifiedPeerSession = {bearer:result.bearer,refresh:result.refresh_token};
    return true;
  })()`);
  check(peerRotated, "native peer session refresh failed before revocation");
  await evaluate(maintenance, `(() => {
    window.confirm = () => true;
    [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Revoke session').click();
  })()`);
  await wait(maintenance, "document.querySelectorAll('.browser-session-card').length === 1 && document.body.textContent.includes('Session revoked.')", "native peer session revocation readback");
  const peerRevoked = await evaluate(maintenance, `(async () => {
    const peer = window.__qualifiedPeerSession; delete window.__qualifiedPeerSession;
    const access = await fetch('/v1/catalog/list', {method:'POST',headers:{'Content-Type':'application/json','Authorization':'Bearer ' + peer.bearer},body:JSON.stringify({limit:1})});
    const refresh = await fetch('/v1/auth/refresh', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({refresh_token:peer.refresh})});
    return {access:access.status,refresh:refresh.status};
  })()`);
  check((peerRevoked.access === 401 || peerRevoked.access === 403) && peerRevoked.refresh === 401,
    "revoked native browser session retained access or refresh authority");
  await debuggerClient.command("Target.closeTarget", { targetId: maintenance.targetId });
  const devices = await page(`${origin}/#host`);
  await signedIn(devices);
  await wait(devices, `(() => {
    const card = document.querySelector('.client-settings-card');
    return card && card.textContent.includes('Browser test device');
  })()`, "native paired-device editor");
  const revision = await evaluate(devices, `(async () => {
    const response = await fetch('/v1/pairing/list_clients', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:'{}'});
    const value = await response.json();
    const card = document.querySelector('.client-settings-card');
    card.open = true;
    const field = card.querySelector('input[name^=fec_]');
    field.value = '35';
    field.dispatchEvent(new Event('input', {bubbles:true}));
    const update = await fetch('/v1/pairing/update_client_config_overrides', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'},
      body:JSON.stringify({certificate_id:value.clients[0].certificate_id,expected_revision:value.revision,
        config_overrides:{...value.clients[0].config_overrides,back_button_timeout:-1}})});
    return {ok:update.ok, revision:value.revision};
  })()`);
  check(revision.ok, "native competing device change failed");
  await evaluate(devices, `(() => {
    const card = document.querySelector('.client-settings-card');
    [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Save').click();
  })()`);
  await wait(devices, "document.body.textContent.includes('Paired clients changed elsewhere')", "native stale device editor");
  await evaluate(devices, `(() => {
    [...document.querySelectorAll('button')].find(button => button.textContent.trim() === 'Review latest and keep drafts').click();
  })()`);
  await wait(devices, `(() => {
    const card = document.querySelector('.client-settings-card');
    return card && card.querySelector('input[name^=fec_]').value === '35' &&
      card.querySelector('input[name^=back_button_timeout_]').value === '-1' &&
      ![...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Save').disabled;
  })()`, "native device draft reconciliation");
  check(await evaluate(devices, `(() => {
    const event = new Event('beforeunload', {cancelable:true});
    window.dispatchEvent(event);
    return event.defaultPrevented;
  })()`), "reconciled device draft was not protected");
  await evaluate(devices, `(() => {
    const card = document.querySelector('.client-settings-card');
    [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Save').click();
  })()`);
  await wait(devices, "document.body.textContent.includes('Stored overrides confirmed for Browser test device.')", "native merged device save");
  const confirmed = await evaluate(devices, `(async () => {
    const response = await fetch('/v1/pairing/list_clients', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:'{}'});
    return (await response.json()).clients[0].config_overrides;
  })()`);
  check(confirmed.fec_percentage === 35 && confirmed.back_button_timeout === -1, "native merged device values differ");
  await evaluate(devices, `(() => {
    const card = document.querySelector('.client-settings-card');
    window.confirm = () => true;
    [...card.querySelectorAll('button')].find(button => button.textContent.trim() === 'Remove device').click();
  })()`);
  await wait(devices, "!document.querySelector('.client-settings-card') && document.body.textContent.includes('No paired clients remain.')", "native device removal readback");
  check(observed.some((request) => request.path === "/v1/pairing/unpair" && request.status === 200),
    "native browser device removal request missing");
  await debuggerClient.command("Target.closeTarget", { targetId: devices.targetId });
  rememberedDigest = await cookieState();
  const csrf = await evaluate(first, `(async () => {
    const response = await fetch('/v1/management_settings/get', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json'}, body:'{}'});
    return {status:response.status, code:(await response.json()).code};
  })()`);
  check(csrf.status === 403 && csrf.code === "csrf_denied", "native missing-CSRF request was not denied");
  const metadata = await evaluate(first, `(async () => {
    const response = await fetch('/v1/auth/browser/session', {method:'POST', credentials:'same-origin',
      headers:{'Content-Type':'application/json','X-Amaterasu-CSRF':'1'}, body:'{}'});
    const reply = await response.json();
    return {status:response.status, ok:reply.ok, bearer:Object.hasOwn(reply,'bearer'), refresh:Object.hasOwn(reply,'refresh_token')};
  })()`);
  check(metadata.status === 200 && metadata.ok && !metadata.bearer && !metadata.refresh, "browser authentication JSON exposed tokens or failed");
  await reload(first);
  await signedIn(first);
  await policyReady(first);
  peer = await page(`${origin}/#settings`);
  await signedIn(peer);
  await policyReady(peer);
  await save(first, "pc");
  await wait(first, "document.body.textContent.includes('Management access policy saved.')", "native UI policy confirmation");
  const saved = await policy(first);
  check(saved.status === 200 && saved.ok && saved.revision === 1 && saved.selection === "pc", "UI save was not durably confirmed");
  await save(peer, "wan");
  await wait(peer, "document.body.textContent.includes('This data changed elsewhere. Refresh before saving again.')", "native stale-editor conflict");
  const unchanged = await policy(peer);
  check(unchanged.revision === 1 && unchanged.selection === "pc", "stale browser CAS changed native policy");
  check(observed.some((request) => request.path === "/v1/auth/browser/login" && request.status === 200), "native browser login request missing");
  return { revision: saved.revision, selection: saved.selection };
}

async function refresh() {
  await debuggerClient.command("Network.deleteCookies", { name: accessName, url: `${origin}/v1` }, first.sessionId);
  await button(first, "Refresh policy");
  await policyReady(first);
  await wait(first, "!!document.querySelector('select[name=origin_web_ui_allowed]') && !document.querySelector('select[name=origin_web_ui_allowed]').disabled", "refreshed policy editor");
  const current = await cookieState();
  check(current.access !== rememberedDigest.access && current.refresh !== rememberedDigest.refresh,
    "native refresh did not rotate both cookie secrets");
  rememberedDigest = current;
  check(observed.some((request) => request.path === "/v1/auth/browser/refresh" && request.status === 200), "native refresh request missing");
  return { rotated: true };
}

async function afterRestart() {
  for (const target of [first, peer]) {
    await reload(target);
    await signedIn(target);
    await policyReady(target);
  }
  const restored = await policy(first);
  check(restored.ok && restored.revision === 1 && restored.selection === "pc", "native restart lost saved policy/session");
  await cookieState();
  return { restored: true };
}

async function prepareColdRestart() {
  // Remove the session-only access cookie before closing Chromium. A later
  // process can restore authority only from the persistent remembered cookie.
  await debuggerClient.command("Network.deleteCookies", { name: accessName, url: `${origin}/v1` }, first.sessionId);
  const { cookies } = await debuggerClient.command("Storage.getCookies");
  check(!cookies.some((cookie) => cookie.name === accessName) &&
    cookies.some((cookie) => cookie.name === refreshName && !cookie.session),
  "cold restart did not leave only the persistent remembered cookie");
  return { refresh_cookie_retained: true };
}

async function coldRestart(options) {
  origin = options.origin;
  check(new URL(origin).hostname === "127.0.0.1" && new URL(origin).protocol === "https:",
    "native origin must be loopback HTTPS");
  debuggerClient = new Debugger();
  await debuggerClient.connect(options.debugger);
  const command = await debuggerClient.command("Browser.getBrowserCommandLine");
  check(command.arguments.includes(`--user-data-dir=${options.profile}`) &&
    command.arguments.includes(`--ignore-certificate-errors-spki-list=${options.spki}`),
  "cold browser profile/SPKI selection differs");
  check(!command.arguments.some((argument) => ["--ignore-certificate-errors", "--disable-web-security", "--no-sandbox"].includes(argument.split("=", 1)[0])),
    "cold browser enabled a broad security bypass");
  first = await page(`${origin}/#settings`);
  await signedIn(first);
  await policyReady(first);
  peer = await page(`${origin}/#settings`);
  await signedIn(peer);
  await policyReady(peer);
  const restored = await policy(first);
  check(restored.ok && restored.revision === 1 && restored.selection === "pc",
    "persistent remembered cookie did not restore the signed management session");
  await cookieState();
  check(observed.some((request) => request.path === "/v1/auth/browser/refresh" && request.status === 200),
    "cold browser process did not restore through native remembered-cookie refresh");
  return { restored: true, remembered_cookie_refresh: true };
}

async function revoked() {
  await button(first, "Refresh policy");
  await signedOut(first);
  await signedOut(peer);
  const result = await policy(first);
  check(result.status === 401 && result.code === "unauthorized", "administratively revoked cookie authorized a request");
  return { denied: true };
}

async function logFollowStart() {
  followPage = await page(`${origin}/#diagnostics`);
  await signedIn(followPage);
  await wait(followPage, "document.querySelector('.host-log-output')?.textContent.includes('signed browser host log marker')", "host log follow baseline");
  await evaluate(followPage, `(() => {
    window.__logOffsets = [];
    const original = window.fetch;
    window.fetch = function(resource, options) {
      const path = new URL(typeof resource === 'string' ? resource : resource.url, location.origin).pathname;
      if (path === '/v1/host-logs/read') window.__logOffsets.push(JSON.parse(options.body).offset);
      return original.apply(this, arguments);
    };
  })()`);
  return {ready:true};
}

async function logFollowUpdated() {
  await wait(followPage, "document.querySelector('.host-log-output')?.textContent.includes('signed live follow marker')", "new host log entry without manual refresh");
  check(await evaluate(followPage, "window.__logOffsets.some(offset => offset > 0) && !window.__logOffsets.includes(0)"), "host log following reread the full snapshot");
  await button(followPage, "Pause");
  return {following:true};
}

async function logFollowResume() {
  await delay(5500);
  check(await evaluate(followPage, "!document.querySelector('.host-log-output')?.textContent.includes('signed paused follow marker') && document.body.textContent.includes('Paused')"), "paused host log changed");
  await button(followPage, "Resume");
  await wait(followPage, "document.querySelector('.host-log-output')?.textContent.includes('signed paused follow marker')", "resumed host log follow");
  await debuggerClient.command("Target.closeTarget", {targetId:followPage.targetId});
  followPage = undefined;
  return {resumed:true};
}

async function logout(options) {
  await login(first, options.username, options.password);
  await policyReady(first);
  await reload(peer);
  await signedIn(peer);
  await policyReady(peer);
  await button(first, "Sign out");
  await signedOut(first);
  await signedOut(peer);
  check((await policy(peer)).status === 401, "peer retained native authority after logout");
  const { cookies } = await debuggerClient.command("Storage.getCookies");
  check(!cookies.some((cookie) => cookie.name === accessName || cookie.name === refreshName), "native logout retained session cookies");
  await reload(first);
  await signedOut(first);
  const cookieRequests = observed.filter((request) => request.method === "POST" && request.path.startsWith("/v1/auth/browser/"));
  check(cookieRequests.every((request) => request.csrf && !request.authorization), "production browser auth omitted CSRF or used a bearer");
  check(observed.filter((request) => request.path === "/v1/auth/browser/login" && request.status === 200).length === 1,
    "cold restore or post-logout reload unexpectedly required credential login");
  return { peer_signed_out: true };
}

const commands = createInterface({ input: process.stdin, crlfDelay: Infinity });
try {
  for await (const line of commands) {
    const request = JSON.parse(line);
    try {
      const handlers = { initial, refresh, log_follow_start: logFollowStart, log_follow_updated: logFollowUpdated,
        log_follow_resume: logFollowResume, after_restart: afterRestart, prepare_cold_restart: prepareColdRestart,
        cold_restart: coldRestart, revoked, logout };
      check(Object.hasOwn(handlers, request.action), "unknown browser qualification phase");
      const result = await handlers[request.action](request);
      process.stdout.write(JSON.stringify({ ok: true, result }) + "\n");
    } catch (error) {
      // Fixed assertions/protocol errors contain no cookie or credential values.
      process.stdout.write(JSON.stringify({ ok: false, error: error.message }) + "\n");
    }
  }
} finally {
  if (debuggerClient?.socket?.readyState === WebSocket.OPEN) {
    await debuggerClient.command("Browser.close").catch(() => undefined);
  }
  debuggerClient?.disconnect();
}
