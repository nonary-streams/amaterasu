import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";
import test from "node:test";
import ts from "typescript";

const chromium = "/usr/bin/chromium";
test("Diagnostics compatibility alerts render, dismiss and restore in Chromium at mobile width", {skip: !existsSync(chromium), timeout: 30000}, async () => {
  const root = await mkdtemp(join(tmpdir(), "amaterasu-alert-browser-"));
  try {
    const source = (await Promise.all(["src/ui.ts", "src/diagnostic_alerts.ts"].map(file => readFile(file, "utf8")))).join("\n");
    const script = ts.transpile(source, {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None});
    const css = await readFile("public/styles.css", "utf8");
    const html = `<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1"><style>${css}</style><main class="content" id="alerts"></main><pre id="result"></pre><script>${script}</script><script>
      (async () => {try {
        const target = document.getElementById('alerts');
        const issue = {operation:'ticket-a', disposition:'advisory_warning', code:'optional_client_features', phase:'announce', requested:16, effective:0};
        const entry = {issue, publisher:'amaterasu.rtsp', at_ms:1};
        const reply = {ok:true,issues:[entry,entry],incomplete:false};
        const context = {api:{hostOrigin:'https://browser-alert.test',allows:s=>s==='session.read',
          post:async route=>{if(route!=='nvhttp/launch_status') throw Error('unexpected route'); return reply;}}};
        localStorage.clear();
        await Amaterasu.renderDiagnosticAlerts(context, target)();
        const check = (value, message) => {if(!value) throw Error(message);};
        const button = text => [...target.querySelectorAll('button')].find(node=>node.textContent===text);
        check(target.querySelectorAll('article').length===1, 'dedup');
        check(target.textContent.includes('Requested: 0x10'), 'requested flags');
        check(target.scrollWidth<=target.clientWidth, 'mobile horizontal overflow');
        button('Dismiss').click();
        await Amaterasu.renderDiagnosticAlerts(context, target)();
        check(target.querySelectorAll('article').length===0, 'stored dismissal');
        button('Restore dismissed alerts').click();
        check(target.querySelectorAll('article').length===1, 'restore');
        document.getElementById('result').textContent='ALERT_BROWSER_PASS';
      } catch (error) {document.getElementById('result').textContent='ALERT_BROWSER_FAIL: '+error.message;}})();
    </script>`;
    const file = join(root, "alerts.html");
    await writeFile(file, html);
    const {stdout} = await promisify(execFile)(chromium, ["--headless=new", "--disable-gpu", "--no-first-run", `--user-data-dir=${join(root, "profile")}`, "--window-size=390,844", "--dump-dom", pathToFileURL(file).href], {timeout:25000, maxBuffer:4*1024*1024});
    assert.match(stdout, /<pre id="result">ALERT_BROWSER_PASS<\/pre>/);
  } finally { await rm(root, {recursive:true,force:true,maxRetries:20,retryDelay:100}); }
});
