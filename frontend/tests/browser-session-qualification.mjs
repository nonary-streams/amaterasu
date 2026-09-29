import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { join, resolve, sep } from "node:path";
import { promisify } from "node:util";
import ts from "typescript";

const browser = process.env.AMATERASU_BROWSER ?? (process.platform === "win32"
  ? "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe"
  : process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "chromium");
const apiScript = ts.transpile(readFileSync("src/api.ts", "utf8"), {
  target: ts.ScriptTarget.ES2022,
});
const browserScript = `
  const result = document.getElementById("result");
  const assert = (condition, message) => { if (!condition) throw new Error(message); };
  const nextFrame = () => new Promise((resolve) => {
    const frame = document.createElement("iframe");
    frame.src = "/frame";
    frame.onload = () => resolve(frame.contentWindow);
    document.body.append(frame);
  });
  const settleStorage = () => new Promise((resolve) => setTimeout(resolve, 100));
  (async () => {
    assert(navigator.locks, "Web Locks unavailable");
    const first = await nextFrame();
    const second = await nextFrame();
    const apiA = new first.Amaterasu.Api(location.origin);
    const apiB = new second.Amaterasu.Api(location.origin);
    await apiA.login("admin", "password", true);
    assert(await apiB.restoreSession(), "peer failed to restore remembered login");
    let lost = 0;
    apiB.onAuthenticationLost = () => lost += 1;
    first.localStorage.clear();
    await settleStorage();
    assert(apiB.subject === "" && lost === 1, "storage.clear did not sign out peer");
    let rejected = false;
    try { await apiB.post("catalog/list"); } catch { rejected = true; }
    assert(rejected, "cleared peer still sent protected request");

    await apiA.login("admin", "password", true);
    assert(await apiB.restoreSession(), "peer failed to restore second login");
    await apiA.logout();
    await settleStorage();
    assert(apiB.subject === "" && lost === 2, "peer logout did not sign out peer");
    result.textContent = "PASS: remembered reload, peer storage.clear, peer logout";
  })().catch((error) => result.textContent = "FAIL: " + error.stack)
    .finally(() => fetch("/done"));
`;
const frameHtml = `<!doctype html><script src="/api.js"></script>`;
const pageHtml = `<!doctype html><div id="result">PENDING</div><iframe src="/hold"></iframe><script>${browserScript}</script>`;

let heldResponse;

const server = createServer(async (request, response) => {
  const path = request.url ?? "/";
  if (path === "/api.js") {
    response.writeHead(200, { "Content-Type": "text/javascript" });
    response.end(apiScript);
  } else if (path === "/frame") {
    response.writeHead(200, { "Content-Type": "text/html" });
    response.end(frameHtml);
  } else if (path === "/") {
    response.writeHead(200, { "Content-Type": "text/html" });
    response.end(pageHtml);
  } else if (path === "/hold") {
    heldResponse = response;
  } else if (path === "/done") {
    heldResponse?.writeHead(200);
    heldResponse?.end();
    response.writeHead(200);
    response.end();
  } else if (path.startsWith("/v1/auth/")) {
    const body = await new Promise((resolve) => {
      let chunks = "";
      request.on("data", (chunk) => chunks += chunk);
      request.on("end", () => resolve(JSON.parse(chunks)));
    });
    response.writeHead(200, { "Content-Type": "application/json" });
    if (path === "/v1/auth/login") {
      response.end(JSON.stringify({
        ok: true,
        bearer: `ama_${"a".repeat(64)}`,
        refresh_token: `ama_${"b".repeat(64)}`,
        subject: "admin",
        scopes: ["catalog.read"],
        expires_at: Math.floor(Date.now() / 1000) + 3600,
        refresh_expires_at: Math.floor(Date.now() / 1000) + 7200,
        remember_me: body.remember_me,
      }));
    } else {
      response.end(JSON.stringify({ ok: true }));
    }
  } else {
    response.writeHead(404);
    response.end();
  }
});

const profile = resolve(mkdtempSync(join("tests", ".browser-session-profile-")));
try {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const { stdout, stderr } = await promisify(execFile)(browser, [
    "--headless=new", "--disable-gpu", "--no-first-run",
    "--no-default-browser-check", "--disable-extensions",
    `--user-data-dir=${profile}`, "--dump-dom",
    `http://127.0.0.1:${port}/`,
  ], { timeout: 30000 });
  const match = stdout.match(/<div id="result">([^<]*)<\/div>/);
  assert.ok(match, `No browser result in DOM. stderr: ${stderr.slice(-1000)}`);
  assert.match(match[1], /^PASS:/, match[1]);
  console.log(match[1]);
} finally {
  server.close();
  assert.ok(profile.startsWith(resolve("tests") + sep));
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
