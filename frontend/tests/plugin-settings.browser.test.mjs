import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm, mkdir, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const chromium = '/usr/bin/chromium';
const pause = () => new Promise(resolve => setTimeout(resolve, 30));

test('plugin sections honor SDK defaults and retain edits across keyboard collapse at desktop and mobile widths',
  { skip: !existsSync(chromium), timeout: 30000 }, async () => {
  const profile = await mkdtemp(join(tmpdir(), 'amaterasu-plugin-settings-'));
  const browser = spawn(chromium, ['--headless=new', '--disable-gpu', '--no-first-run',
    '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: 'ignore' });
  let socket;
  try {
    let address;
    for (let attempt = 0; attempt < 200; attempt++) {
      try {
        const [port, path] = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).trim().split('\n');
        address = `ws://127.0.0.1:${port}${path}`;
        break;
      } catch { await pause(); }
    }
    assert.ok(address, 'Chromium starts');
    socket = new WebSocket(address);
    await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
    let sequence = 0, sessionId;
    const pending = new Map();
    socket.onmessage = ({ data }) => {
      const message = JSON.parse(data), waiter = pending.get(message.id);
      if (!waiter) return;
      pending.delete(message.id);
      message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);
    };
    const command = (method, params = {}) => new Promise((resolve, reject) => {
      const id = ++sequence;
      pending.set(id, { resolve, reject });
      socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
    const { targetId } = await command('Target.createTarget', { url: 'about:blank' });
    sessionId = (await command('Target.attachToTarget', { targetId, flatten: true })).sessionId;
    await command('Page.enable');
    await command('Page.bringToFront');
    const evaluate = async expression => {
      const result = await command('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(result.result?.exception?.description ?? 'Browser evaluation failed');
      return result.result.value;
    };
    const waitFor = async expression => {
      for (let attempt = 0; attempt < 200; attempt++) {
        if (await evaluate(expression)) return;
        await pause();
      }
      assert.fail(`Timed out: ${expression}`);
    };
    const key = async () => {
      for (const type of ['keyDown', 'keyUp'])
        await command('Input.dispatchKeyEvent', { type, key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, nativeVirtualKeyCode: 13,
          ...(type === 'keyDown' ? { text: '\r' } : {}) });
    };
    const root = new URL('../../', import.meta.url);
    const css = await readFile(new URL('frontend/public/styles.css', root), 'utf8');
    const pluginCss = await readFile(new URL('modules/rtx-hdr/ui/application.css', root), 'utf8');
    const pluginJs = await readFile(new URL('modules/rtx-hdr/ui/application.js', root), 'utf8');
    const pluginConfig = JSON.parse(await readFile(new URL('modules/rtx-hdr/ui/application-ui.json', root), 'utf8'));
    const native = JSON.parse(await readFile(new URL('modules/rtx-hdr/metadata/linux.json', root), 'utf8')).descriptor;
    assert.equal(pluginConfig.default_expanded, true);
    assert.equal(native.default_expanded, true);
    await command('Emulation.setDeviceMetricsOverride', { width: 1060, height: 1200, deviceScaleFactor: 1, mobile: false });
    await evaluate(`document.documentElement.dataset.theme = 'dark';
      document.head.innerHTML = '<meta name="viewport" content="width=device-width, initial-scale=1">';
      const style = document.createElement('style'); style.textContent = ${JSON.stringify(css + pluginCss)}; document.head.append(style);
      document.body.innerHTML = '<main style="max-width:980px;margin:24px auto;padding:0 16px"><h1>Plugin settings</h1><p class="muted" style="margin:8px 0 20px">Application overrides · Example game</p><div id="plugins"></div></main>';`);
    const files = ['locale-catalogs.ts', 'locale.ts', 'ui.ts', 'extension_fields.ts', 'extension_ui.ts', 'session_extension_actions.ts', 'session_extensions.ts'];
    const source = (await Promise.all(files.map(file => readFile(new URL(`frontend/src/${file}`, root), 'utf8')))).join('\n');
    await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
    await evaluate(`Object.assign(window, Amaterasu);
      window.AmaterasuExtensions = {v1: {register(_id, page) {window.rtxPage = page;}}};`);
    await evaluate(pluginJs);
    await evaluate(`
      window.saved = []; window.assets = []; window.values = {}; window.revision = 1;
      window.rtxHost = {rtx_hdr_scope:'application',rtx_hdr_peak_brightness:1200};
      const native = ${JSON.stringify(native)};
      const generic = (provider, expanded) => ({provider, display_name: provider, default_expanded: expanded,
        options: [{id:'quality', label:'Quality', help:'An independent plugin setting.',
          definition:{type:'integer',default:50,minimum:0,maximum:100}, presentation:'slider',
          allowed_scopes:['application'],apply_policy:'next_stream'}]});
      window.api = {allows:()=>true, async post(route, payload) {
        if (route.endsWith('/list')) return {revision:1,next:null,items:[native,generic('Closed by default',false),generic('Expanded by default',true)]};
        return {revision:1,values:{},effective:{quality:{value:50,source:null}}};
      }};
      const settings = {provider:'amaterasu.rtx-hdr',canWrite:true,
        async read() {return {revision,values:{...values},effective:Object.fromEntries(native.options.map(option =>
          [option.id,{value:values[option.id] ?? rtxHost[option.id] ?? option.definition.default, source:null}]))};},
        async update(expected, next) {if(expected !== revision) throw Error('revision conflict');
          saved.push({...next});values={...next};return {revision:++revision};}};
      Amaterasu.ExtensionPages = class {
        async discover() {return [{id:'rtx-hdr',name:'RTX HDR',settings_provider:settings.provider,placements:['application'],default_expanded:${pluginConfig.default_expanded}},
          {id:'example-custom',name:'Custom plugin',placements:['application']}];}
        mountApplication(id, container) {
          const controller = new AbortController();
          const sdk = {features:['ui.application-settings/1'],signal:controller.signal,settings,
            ui:Amaterasu.createExtensionUiV1(controller.signal),assets:{stylesheet:async path=>{assets.push(path);}},setDirty(){},setBusy(){}};
          if(id === 'rtx-hdr') rtxPage.mountApplication(container,sdk);
          else container.textContent = 'Independent custom plugin';
          return {ready:Promise.resolve(),dispose(){controller.abort();},isDirty:()=>false,isBusy:()=>false};
        }
        reset() {}
      };
      window.ExtensionPages = Amaterasu.ExtensionPages;
      Amaterasu.renderApplicationPluginSections({api,content:document.body,notify(){},registerDraftGuard(){}},document.querySelector('#plugins'),{id:'app-0000000000000001',name:'Example game'});
    `);
    await waitFor(`document.querySelectorAll('.rtx-hdr-setting').length === 9 && document.querySelectorAll('.application-plugin-section').length === 4`);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.application-plugin-section'), node => [node.querySelector('summary').textContent,node.open])`),
      [['Closed by default', false], ['Expanded by default', true], ['RTX HDR', true], ['Custom plugin', false]]);
    assert.deepEqual(await evaluate('saved'), []);
    assert.deepEqual(await evaluate('assets'), ['application.css']);
    await evaluate(`window.rtx = Array.from(document.querySelectorAll('.application-plugin-section')).find(node=>node.querySelector('summary').textContent === 'RTX HDR');
      rtx.querySelector('summary').focus();`);
    assert.equal(await evaluate(`document.activeElement === rtx.querySelector('summary')`), true);
    await key();
    await waitFor('!rtx.open');
    assert.equal(await evaluate('rtx.open'), false);
    await key();
    await waitFor('rtx.open');
    assert.equal(await evaluate('rtx.open'), true);
    assert.equal(await evaluate(`rtx.querySelector('.rtx-hdr-advanced').open`), false);
    assert.equal(await evaluate(`Array.from(rtx.querySelectorAll('.rtx-hdr-setting')).filter(node=>node.checkVisibility()).length`), 5);
    assert.equal(await evaluate(`rtx.querySelectorAll('.rtx-hdr-inherit').length`), 0);
    assert.equal(await evaluate(`rtx.querySelectorAll('[data-setting-id="rtx_hdr_enable"]').length`), 0);
    assert.equal(await evaluate(`rtx.querySelector('[data-setting-id="rtx_hdr_peak_brightness"] input[type="number"]').value`), '1200');
    await evaluate(`const control = rtx.querySelector('[aria-label="Peak brightness (nits)"] input[type="number"]');
      control.value='1400';control.dispatchEvent(new Event('input',{bubbles:true}));`);
    await waitFor('saved.length === 1');
    assert.deepEqual(await evaluate('saved[0]'), { rtx_hdr_peak_brightness: 1400 });
    await evaluate(`rtx.querySelector('[data-setting-id="rtx_hdr_application_enable"] input').click();`);
    await waitFor('saved.length === 2');
    assert.deepEqual(await evaluate('saved[1]'), { rtx_hdr_peak_brightness: 1400, rtx_hdr_application_enable: true });
    await evaluate(`rtx.querySelector('.rtx-hdr-advanced > summary').focus();`);
    await key();
    await waitFor(`rtx.querySelector('.rtx-hdr-advanced').open`);
    assert.equal(await evaluate(`Array.from(rtx.querySelectorAll('.rtx-hdr-setting')).filter(node=>node.checkVisibility()).length`), 9);
    await evaluate(`rtx.querySelector('[data-setting-id="rtx_hdr_debanding"] input').click();`);
    await waitFor('saved.length === 3');
    assert.deepEqual(await evaluate('saved[2]'), { rtx_hdr_peak_brightness: 1400, rtx_hdr_application_enable: true, rtx_hdr_debanding: false });
    await evaluate(`rtx.querySelector('.rtx-hdr-advanced > summary').focus();`);
    await key();
    await waitFor(`!rtx.querySelector('.rtx-hdr-advanced').open`);
    // A refreshed host mode changes which enable control is shown, while the
    // application's existing value for the inactive mode remains preserved.
    await evaluate(`rtxHost.rtx_hdr_scope='global';
      const contrast = rtx.querySelector('[data-setting-id="rtx_hdr_contrast"] input[type="number"]');
      contrast.value='10';contrast.dispatchEvent(new Event('input',{bubbles:true}));`);
    await waitFor(`saved.length===4 && !!rtx.querySelector('[data-setting-id="rtx_hdr_enable"]')`);
    assert.equal(await evaluate(`rtx.querySelectorAll('[data-setting-id="rtx_hdr_application_enable"]').length`), 0);
    await evaluate(`rtx.querySelector('[data-setting-id="rtx_hdr_enable"] input').click();`);
    await waitFor('saved.length === 5');
    assert.deepEqual(await evaluate('saved[4]'), { rtx_hdr_peak_brightness: 1400, rtx_hdr_application_enable: true,
      rtx_hdr_debanding: false, rtx_hdr_contrast: 10, rtx_hdr_enable: true });
    // Only the RTX panel is shown in visual artifacts; the independent fixtures
    // above exercise the unchanged consumer with other providers and defaults.
    await evaluate(`document.activeElement.blur(); document.querySelectorAll('.application-plugin-section').forEach(node=>{if(node!==rtx)node.hidden=true;});`);
    for (const [width, theme] of [[1060, 'dark'], [1060, 'light'], [375, 'dark']]) {
      await command('Emulation.setDeviceMetricsOverride', { width, height: 1400, deviceScaleFactor: 1, mobile: false });
      await evaluate(`document.documentElement.dataset.theme = ${JSON.stringify(theme)}`);
      assert.equal(await evaluate('document.documentElement.scrollWidth <= window.innerWidth'), true, `${width}px ${theme} has no overflow`);
      assert.equal(await evaluate(`Array.from(rtx.querySelectorAll('input')).filter(node=>node.checkVisibility()).every(node=>{const box=node.getBoundingClientRect();return box.width>0 && box.left>=0 && box.right<=innerWidth;})`), true);
      assert.equal(await evaluate(`Array.from(rtx.querySelectorAll('.plugin-toggle')).every(node=>getComputedStyle(node).flexDirection==='row')`), true, 'toggle labels stay beside their controls');
      if (process.env.AMATERASU_SCREENSHOT_DIR) {
        await mkdir(process.env.AMATERASU_SCREENSHOT_DIR, { recursive: true });
        const { data } = await command('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
        await writeFile(join(process.env.AMATERASU_SCREENSHOT_DIR, `plugin-settings-${width}-${theme}.png`), Buffer.from(data, 'base64'));
      }
    }
  } finally {
    socket?.close();
    if (browser.exitCode === null) {
      const exited = new Promise(resolve => browser.once('exit', resolve));
      browser.kill('SIGTERM');
      await exited;
    }
    await rm(profile, { recursive: true, force: true });
  }
});
