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
  check((await policy(first)).revision === 0, "initial native policy revision differs");
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
      const handlers = { initial, refresh, after_restart: afterRestart, prepare_cold_restart: prepareColdRestart,
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
