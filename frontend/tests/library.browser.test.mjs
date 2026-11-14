import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";

const browser = process.env.AMATERASU_BROWSER ?? "chromium";
let available = false;
try { execFileSync(browser, ["--version"], { stdio: "ignore" }); available = true; } catch {}

test("Library owns integration navigation, preserves drafts and scopes, and retires Applications", {
  skip: !available && `Browser executable unavailable: ${browser}`,
}, () => {
  const config = JSON.parse(readFileSync("tsconfig.json", "utf8"));
  const compile = (source) => ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None });
  const source = config.files.filter((file) => !file.endsWith(".d.ts") && file !== "src/app.ts")
    .map((file) => readFileSync(file, "utf8")).join("\n");
  const host = `
    window.calls = [];
    window.scopes = null;
    window.confirmed = false;
    window.confirm = () => window.confirmed;
    Amaterasu.Api = class {
      subject = "Library tester";
      role = "administrator";
      allows(scope) { return window.scopes === null || window.scopes.includes(scope); }
      allowsLaunchAction() { return false; }
      async restoreSession() { return true; }
      suspend() {}
      async post(route) {
        calls.push(route);
        if (route === "catalog/list") return { ok: true, revision: 1, entries: [], next_cursor: null,
          config_override_capabilities: { frame_limiter_provider: ["auto"], rtss_frame_limit_type: [] } };
        if (route === "steam/status") return { ok: true, settings_ready: true, enabled: true,
          game_count: 0, warning_count: 0, scanning: false, complete: true,
          auto_sync_catalog: true, force_sync_pending: false,
          catalog_sync: { success: true, pending: false, error: null } };
        if (route === "steam/list") return { ok: true, games: [], next_cursor: null, revision: "1" };
        if (route === "steam/force_sync" || route === "steam/refresh") return { ok: true, accepted: true };
        if (route === "lutris/status") return { ok: true, settings_ready: true, enabled: true,
          platform_supported: true, database_available: true, executable_available: true, available: true,
          game_count: 0, refresh_pending: false, last_error: null };
        if (route === "playnite/status") return { ok: true, settings_ready: true, enabled: false,
          playnite_running: false, snapshot_game_count: 0, snapshot_pending: false, last_error: null };
        throw new Error("Unexpected mocked route: " + route);
      }
    };
    Amaterasu.ExtensionPages = class {
      reset() {} dispose() {} isDirty() { return false; } async discover() { return []; }
    };
    Object.assign(globalThis, Amaterasu);
    location.hash = "catalog";
  `;
  const checks = `
    const assert = (ok, message) => { if (!ok) throw new Error(message); };
    const wait = async (check, label) => {
      for (let attempt = 0; attempt < 300; attempt++) {
        if (check()) return;
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      throw new Error("Timed out: " + label + " | " + document.querySelector("#main")?.innerText?.slice(-1200) + " | calls: " + calls.join(","));
    };
    const selected = id => document.querySelector('#library-tab-' + id + '[aria-selected="true"]');
    const findButton = text => [...document.querySelectorAll('#main button')].find(node => node.textContent === text);
    const navigate = async (route, check) => {
      location.hash = route;
      await wait(check, route);
    };
    (async () => {
      await wait(() => selected('games') && findButton('+ Add application')?.disabled === false, 'initial library');
      const nav = [...document.querySelectorAll('.nav-item')].map(node => node.textContent);
      assert(nav.includes('Library') && !nav.includes('Applications') && !nav.includes('Steam'), 'retired sidebar entries');
      assert(!calls.includes('application/list') && !calls.includes('launch/list'), 'removed Applications polling');
      assert(document.querySelectorAll('#main h1').length === 1, 'single library heading');

      document.querySelector('#library-tab-games').focus();
      document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
      await wait(() => selected('integrations') && document.activeElement.id === 'library-tab-integrations', 'arrow key focus');
      assert(location.hash === '#catalog/integrations', 'integration deep link');
      assert(document.querySelector('.nav-item[aria-current="page"]').textContent === 'Library', 'Library remains selected');
      assert(document.querySelectorAll('.integration-card').length === 3, 'Steam, Lutris and Playnite inside Library');
      assert(!document.querySelector('#main').textContent.includes('RTX HDR'), 'library excludes unrelated integrations');
      findButton('Manage Steam library').click();
      await wait(() => findButton('Back to integrations') && calls.includes('steam/list'), 'Steam discovery inside Library');
      assert(selected('integrations') && location.hash === '#catalog/integrations/steam', 'Steam stays inside Integrations');
      assert(findButton('Force catalog sync'), 'Steam sync preserved');
      findButton('Back to integrations').click();
      await wait(() => selected('integrations') && !findButton('Back to integrations'), 'return to integration cards');
      document.querySelector('#library-tab-integrations').focus();
      document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true }));
      await wait(() => selected('games') && document.activeElement.id === 'library-tab-games', 'Home key');
      document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true }));
      await wait(() => selected('integrations') && document.activeElement.id === 'library-tab-integrations', 'End key');
      history.back();
      await wait(() => selected('games'), 'browser Back');
      history.forward();
      await wait(() => selected('integrations'), 'browser Forward');

      await navigate('steam', () => !!findButton('Back to integrations'));
      assert(selected('integrations'), 'legacy Steam link');
      await navigate('applications', () => !!selected('games'));
      assert(!findButton('Terminate') && !calls.includes('application/list'), 'legacy Applications link resolves to Library');
      await wait(() => findButton('+ Add application')?.disabled === false, 'catalog ready');
      findButton('+ Add application').click();
      await wait(() => document.querySelector('.editor-form input[name="name"]'), 'application editor');
      const name = document.querySelector('.editor-form input[name="name"]');
      name.value = 'Keep my draft'; name.dispatchEvent(new Event('input', { bubbles: true }));
      document.querySelector('#library-tab-integrations').click();
      await new Promise(resolve => setTimeout(resolve, 30));
      assert(selected('games') && name.isConnected && name.value === 'Keep my draft', 'tab navigation protects draft');
      window.confirmed = true;
      document.querySelector('#library-tab-integrations').click();
      await wait(() => selected('integrations'), 'confirmed draft discard');

      window.scopes = ['catalog.read'];
      await navigate('catalog', () => !!selected('games'));
      assert(!document.querySelector('#library-tab-integrations'), 'catalog reader has only Games');
      assert(!calls.includes('application/list'), 'removed section never requested');
      window.scopes = ['steam.read', 'steam.refresh'];
      await navigate('catalog/integrations', () => !!selected('integrations'));
      assert(!document.querySelector('#library-tab-games'), 'integration access does not require catalog access');
      await wait(() => findButton('Manage Steam library'), 'Steam access');
      findButton('Manage Steam library').click();
      await wait(() => findButton('Back to integrations'), 'scoped Steam discovery');
      window.scopes = ['steam.refresh'];
      await navigate('catalog/integrations', () => !findButton('Back to integrations'));
      await navigate('catalog/integrations/steam', () => !!findButton('Force catalog sync'));
      assert(document.querySelector('#main').textContent.includes('Steam status unavailable'), 'refresh-only session stays read-restricted');
      document.body.dataset.qualification = 'passed';
    })().catch(error => {
      document.body.dataset.qualification = 'failed';
      const result = document.createElement('pre'); result.id = 'failure'; result.textContent = error.stack; document.body.append(result);
    });
  `;
  const directory = mkdtempSync(join(tmpdir(), "amaterasu-library-browser-"));
  const html = join(directory, "library.html");
  writeFileSync(join(directory, "app.js"), `${compile(source)}\n${host}\n${compile(readFileSync("src/app.ts", "utf8"))}\n${checks}`);
  writeFileSync(html, `<!doctype html><html><head><meta charset="utf-8">
    <link rel="stylesheet" href="${pathToFileURL(resolve("public/styles.css"))}"></head><body><div id="app"></div>
    <script>window.addEventListener('error', event => {
      document.body.dataset.qualification = 'failed';
      const result = document.createElement('pre'); result.id = 'failure'; result.textContent = event.message; document.body.append(result);
    });
    window.addEventListener('unhandledrejection', event => {
      const result = document.createElement('pre'); result.id = 'failure'; result.textContent = event.reason?.stack ?? event.reason; document.body.append(result);
    });</script><script src="./app.js"></script></body></html>`);
  try {
    const output = execFileSync(browser, ["--headless", "--no-sandbox", "--disable-gpu", "--no-first-run",
      "--allow-file-access-from-files", "--virtual-time-budget=15000", "--dump-dom",
      `--user-data-dir=${join(directory, "profile")}`, pathToFileURL(html).href],
    { encoding: "utf8", timeout: 30000, maxBuffer: 8 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
    assert.equal(output.includes('data-qualification="passed"'), true, output.match(/<pre id="failure">[\s\S]*?<\/pre>/)?.[0] ?? output.slice(-4000));
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
