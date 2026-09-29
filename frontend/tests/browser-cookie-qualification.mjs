import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:https";
import { join, resolve, sep } from "node:path";
import { promisify } from "node:util";
import ts from "typescript";

const browser = process.env.AMATERASU_BROWSER ?? (process.platform === "win32"
  ? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"
  : process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "chromium");
const certificate = readFileSync("../modules/pairing/tests/fixtures/host-cert.pem");
const key = readFileSync("../modules/pairing/tests/fixtures/host-key.pem");
const apiScript = ts.transpile(readFileSync("src/api.ts", "utf8"), {
  target: ts.ScriptTarget.ES2022,
});
const browserScript = `
  const result = document.getElementById("result");
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const frame = () => new Promise((resolve) => {
    const element = document.createElement("iframe");
    element.src = "/v1/test/frame";
    element.onload = () => resolve(element.contentWindow);
    document.body.append(element);
  });
  const noScriptSecrets = (api) => {
    assert(document.cookie === "", "HttpOnly cookies leaked to script");
    const stored = Array.from({length: localStorage.length}, (_, index) =>
      localStorage.getItem(localStorage.key(index))).join(" ");
    assert(!stored.includes("access-") && !stored.includes("refresh-"),
      "cookie token leaked to storage");
    assert(api.bearer === "" && api.refreshToken === "", "token entered API script state");
  };
  (async () => {
    assert(isSecureContext && navigator.locks, "secure Web Locks unavailable");
    const first = await frame();
    const second = await frame();
    const apiA = new first.Amaterasu.Api(location.origin);
    const apiB = new second.Amaterasu.Api(location.origin);
    assert(apiA.usesSecureCookies, "HTTPS cookie mode was not selected");
    await apiA.login("admin", "fixture-password", true);
    noScriptSecrets(apiA);
    await apiA.post("catalog/list");
    assert(await apiB.restoreSession(), "peer failed to restore access cookie");

    await fetch("/test/expire-access", { method: "POST" });
    await apiB.post("catalog/list");
    noScriptSecrets(apiB);
    const reloaded = await frame();
    const apiC = new reloaded.Amaterasu.Api(location.origin);
    assert(await apiC.restoreSession(), "reload failed after cookie rotation");
    assert(apiC.subject === "admin", "reload metadata missing");

    await apiA.logout();
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert(apiB.subject === "" && apiC.subject === "", "peer sign-out was missed");
    let denied = false;
    try { await apiB.post("catalog/list"); } catch { denied = true; }
    assert(denied, "post-logout protected request succeeded");
    const afterLogout = await frame();
    assert(!(await new afterLogout.Amaterasu.Api(location.origin).restoreSession()),
      "logout cookies restored after reload");
    noScriptSecrets(apiA);
    result.textContent = "PASS: HTTPS HttpOnly login, CSRF, reload, refresh, peer logout";
  })().catch((error) => result.textContent = "FAIL: " + error.stack)
    .finally(() => fetch("/done"));
`;
const pageHtml = `<!doctype html><div id="result">PENDING</div><iframe src="/hold"></iframe><script>${browserScript}</script>`;

let heldResponse;
let origin;
let access = null;
let refresh = null;
let generation = 0;
let loginCount = 0;
let refreshCount = 0;
let protectedCount = 0;
let violations = 0;
const cookies = (request) => Object.fromEntries(
  (request.headers.cookie ?? "").split("; ").filter(Boolean).map((part) => {
    const separator = part.indexOf("=");
    return [part.slice(0, separator), part.slice(separator + 1)];
  }),
);
const session = () => ({
  ok: true,
  subject: "admin",
  scopes: ["catalog.read"],
  expires_at: Math.floor(Date.now() / 1000) + 3600,
});
const setCookies = (response) => response.setHeader("Set-Cookie", [
  `ama_access=${access}; Secure; HttpOnly; SameSite=Strict; Path=/v1`,
  `ama_refresh=${refresh}; Secure; HttpOnly; SameSite=Strict; Path=/v1`,
]);
const clearCookies = (response) => response.setHeader("Set-Cookie", [
  "ama_access=; Max-Age=0; Secure; HttpOnly; SameSite=Strict; Path=/v1",
  "ama_refresh=; Max-Age=0; Secure; HttpOnly; SameSite=Strict; Path=/v1",
]);
const json = (response, status, body) => {
  response.writeHead(status, { "Content-Type": "application/json", "Cache-Control": "no-store" });
  response.end(JSON.stringify(body));
};

const server = createServer({ cert: certificate, key }, async (request, response) => {
  const path = request.url ?? "/";
  if (path === "/v1/test/page") {
    response.writeHead(200, { "Content-Type": "text/html" });
    response.end(pageHtml);
  } else if (path === "/api.js") {
    response.writeHead(200, { "Content-Type": "text/javascript" });
    response.end(apiScript);
  } else if (path === "/v1/test/frame") {
    response.writeHead(200, { "Content-Type": "text/html" });
    response.end('<!doctype html><script src="/api.js"></script>');
  } else if (path === "/hold") {
    heldResponse = response;
  } else if (path === "/done") {
    heldResponse?.writeHead(200);
    heldResponse?.end();
    response.writeHead(200);
    response.end();
  } else if (path === "/test/expire-access") {
    access = "expired";
    response.writeHead(200);
    response.end();
  } else if (path.startsWith("/v1/")) {
    if (
      request.method !== "POST" || request.headers.origin !== origin ||
      request.headers["x-amaterasu-csrf"] !== "1" ||
      request.headers.authorization !== undefined
    ) {
      violations += 1;
      json(response, 403, { ok: false, code: "forbidden" });
      return;
    }
    const sent = cookies(request);
    if (path === "/v1/auth/browser/logout") {
      const existed = sent.ama_refresh === refresh || sent.ama_access === access;
      access = null;
      refresh = null;
      clearCookies(response);
      json(response, existed ? 200 : 401,
        existed ? { ok: true } : { ok: false, code: "session_missing" });
    } else if (path === "/v1/auth/browser/login") {
      loginCount += 1;
      generation += 1;
      access = `access-${generation}`;
      refresh = `refresh-${generation}`;
      setCookies(response);
      json(response, 200, session());
    } else if (path === "/v1/auth/browser/session") {
      json(response, sent.ama_access === access && access !== null ? 200 : 401,
        sent.ama_access === access && access !== null
          ? session() : { ok: false, code: "unauthorized" });
    } else if (path === "/v1/auth/browser/refresh") {
      if (sent.ama_refresh !== refresh || refresh === null) {
        json(response, 401, { ok: false, code: "token_not_found" });
      } else {
        refreshCount += 1;
        generation += 1;
        access = `access-${generation}`;
        refresh = `refresh-${generation}`;
        setCookies(response);
        json(response, 200, session());
      }
    } else if (path === "/v1/catalog/list") {
      if (sent.ama_access !== access || access === null)
        json(response, 401, { ok: false, code: "unauthorized" });
      else {
        protectedCount += 1;
        json(response, 200, { ok: true, entries: [] });
      }
    } else {
      json(response, 404, { ok: false, code: "not_found" });
    }
  } else {
    response.writeHead(404);
    response.end();
  }
});

const profile = resolve(mkdtempSync(join("tests", ".browser-cookie-profile-")));
try {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  origin = `https://127.0.0.1:${server.address().port}`;
  const { stdout, stderr } = await promisify(execFile)(browser, [
    "--headless=new", "--disable-gpu", "--no-first-run",
    "--no-default-browser-check", "--disable-extensions",
    // Only this isolated fixture uses the repository's untrusted test cert.
    "--ignore-certificate-errors", `--user-data-dir=${profile}`, "--dump-dom",
    `${origin}/v1/test/page`,
  ], { timeout: 30000 });
  const match = stdout.match(/<div id="result">([^<]*)<\/div>/);
  assert.ok(match, `No browser result in DOM. stderr: ${stderr.slice(-1000)}`);
  assert.match(match[1], /^PASS:/, match[1]);
  assert.equal(violations, 0);
  assert.equal(loginCount, 1);
  assert.equal(refreshCount, 1);
  assert.ok(protectedCount >= 2);
  assert.equal(access, null);
  assert.equal(refresh, null);
  console.log(match[1]);
} finally {
  server.close();
  assert.ok(profile.startsWith(resolve("tests") + sep));
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
