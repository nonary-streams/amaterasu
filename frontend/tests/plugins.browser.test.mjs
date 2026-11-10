import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const chromium = '/usr/bin/chromium';
const pause = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));

// A host as the page sees it: three loaded modules from one repository, and
// repository changes that finish a moment after they are accepted.
const fakeHost = `
  window.host = {
    calls: [], notices: [], navigated: [], views: [],
    role: 'administrator', scopes: null, manageable: true, describe: 'ok', nextOperation: 1, operation: null,
    modules: [
      { id: 'amaterasu.runtime', version: '0.1.0', sha256: '1'.repeat(64), runtime: true },
      { id: 'amaterasu.display', version: '0.2.0', sha256: '2'.repeat(64), runtime: false },
      { id: 'amaterasu.frame-limiter', version: '0.1.0', sha256: '3'.repeat(64), runtime: false },
      { id: 'example.overlay', version: '1.2.0', sha256: '4'.repeat(64), runtime: false },
    ],
    requires: { 'amaterasu.display': ['amaterasu.runtime'], 'example.overlay': ['amaterasu.display', 'amaterasu.runtime'] },
    repositories: [
      { id: 'house', location: 'https://house.example.test/repository', fingerprint: 'c'.repeat(64), revoked: false,
        catalog: { state: 'current', sequence: '9', expires_unix: 1900000000 },
        candidates: [
          { slot: 'example.overlay', implementation: 'example-overlay', version: '1.2.0', kind: 'native_set', manifest_sha256: 'd'.repeat(64) },
          { slot: 'amaterasu.frame-limiter', implementation: 'house-frame-pacer', version: '0.3.0', kind: 'native_set', manifest_sha256: 'e'.repeat(64) },
          { slot: 'amaterasu.rtx-hdr', implementation: 'amaterasu-linux-session-rtx-hdr', version: '0.1.0', kind: 'native_set', manifest_sha256: 'f'.repeat(64) },
        ] },
      { id: 'stale', location: 'https://stale.example.test', fingerprint: '5'.repeat(64), revoked: false,
        catalog: { state: 'expired', expires_unix: 1700000000 }, candidates: [] },
    ],
  };
  host.change = (kind, repository, finish) => {
    if (host.operation && host.operation.state === 'running') return { ok: false, code: 'repository_busy' };
    const id = host.nextOperation++;
    host.operation = { id, repository, kind, state: 'running', code: null };
    setTimeout(() => { host.operation = { id, repository, kind, ...finish() }; }, 150);
    return { ok: true, operation: id };
  };
  host.reply = (route, body) => {
    if (route === 'package-selection/status') return {
      ok: true, revision: 'a'.repeat(64), active: 'd'.repeat(64), previous: 'b'.repeat(64), pending: null, trial: null,
      highest_sequence: '12',
      running: { available: true, digest: 'd'.repeat(64), modules: host.modules, source: { repository: 'house', fingerprint: 'c'.repeat(64) } },
    };
    if (route === 'repositories/list') return {
      ok: true, repositories: host.repositories, next_offset: null,
      ...(host.manageable ? { manageable: true, capacity: 16, operation: host.operation } : {}),
    };
    if (route === 'modules/describe') {
      if (host.describe === 'missing') return { status: 404, ok: false, code: 'not_found' };
      const module = host.modules.find(entry => entry.id === body.id);
      return { ok: true, module: { ...module, size: 2971272, abi_major: 1,
        requires: (host.requires[body.id] ?? []).map(id => ({ id, version: host.modules.find(entry => entry.id === id).version })),
        required_by: Object.entries(host.requires).filter(([, needs]) => needs.includes(body.id)).map(([id]) => id),
        provides: [body.id + '/1'], calls: Array.from({ length: 14 }, (_, index) => 'amaterasu.service-' + index + '/1'),
        native_provides: [], native_uses: [], publishes: [], subscribes: [], hooks: [] } };
    }
    if (route === 'repositories/add') {
      if (body.fingerprint !== 'ab'.repeat(32)) return { ok: false, code: 'fingerprint_mismatch' };
      return host.change('add', body.id, () => {
        if (body.location.includes('unsigned')) return { state: 'failed', code: 'catalog_rejected' };
        host.repositories.push({ id: body.id, location: body.location, fingerprint: body.fingerprint, revoked: false,
          catalog: { state: 'current', sequence: '1', expires_unix: 1900000000 },
          candidates: [{ slot: 'new.recorder', implementation: 'new-recorder', version: '2.0.0', kind: 'native_set', manifest_sha256: '7'.repeat(64) }] });
        return { state: 'succeeded', code: null };
      });
    }
    if (route === 'repositories/refresh') return host.change('refresh', body.id, () =>
      body.id === 'stale' ? { state: 'failed', code: 'catalog_expired' } : { state: 'succeeded', code: null });
    if (route === 'repositories/remove') return host.change('remove', body.id, () => {
      Object.assign(host.repositories.find(entry => entry.id === body.id), { revoked: true, candidates: [], catalog: { state: 'missing' } });
      return { state: 'succeeded', code: null };
    });
    return { status: 404, ok: false, code: 'not_found' };
  };
  host.api = {
    get role() { return host.role; },
    allows: scope => !host.scopes || host.scopes.includes(scope),
    async post(route, body) {
      host.calls.push([route, body]);
      const reply = JSON.parse(JSON.stringify(host.reply(route, body)));
      if (reply.ok !== true) throw new Amaterasu.ApiError(reply.code.replaceAll('_', ' '), reply.code, reply.status ?? 200);
      return reply;
    },
  };
  host.open = (view) => {
    host.pageGuard?.();
    document.querySelectorAll('dialog').forEach(dialog => dialog.remove());
    const content = document.querySelector('#page');
    content.replaceChildren();
    host.calls.length = 0;
    Amaterasu.renderPlugins({
      api: host.api, content,
      notify: message => host.notices.push(typeof message === 'string' ? message : message.value),
      navigate: page => host.navigated.push(page),
      ...(view ? { initialPluginsView: view } : {}),
      replacePluginsView: next => host.views.push(next),
      registerDraftGuard: (_dirty, dispose) => { host.pageGuard = dispose; return dispose; },
    });
  };
  host.text = selector => Array.from(document.querySelectorAll(selector), node => node.textContent.trim());
  host.row = name => Array.from(document.querySelectorAll('.plugin-row')).find(row => row.querySelector('.plugin-row-name').textContent === name);
  host.card = id => Array.from(document.querySelectorAll('.repository-card')).find(card => card.querySelector('h3').textContent === id);
  host.press = (label, scope = document) => Array.from(scope.querySelectorAll('button')).find(control => control.textContent.trim() === label).click();
  host.fill = (name, value) => { const control = document.querySelector('input[name="' + name + '"]'); control.value = value; control.dispatchEvent(new Event('input', { bubbles: true })); };
`;

test('the plugin manager lists, explains and changes what a host can run', { skip: !existsSync(chromium), timeout: 60000 }, async () => {
  const profile = await mkdtemp(join(tmpdir(), 'amaterasu-plugins-'));
  const browser = spawn(chromium, ['--headless=new', '--disable-gpu', '--no-first-run',
    '--remote-debugging-address=127.0.0.1', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: 'ignore' });
  let socket;
  try {
    let address;
    for (let attempt = 0; attempt < 200 && !address; attempt++) {
      try {
        const [port, path] = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).trim().split('\n');
        address = `ws://127.0.0.1:${port}${path}`;
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
    const evaluate = async expression => {
      const result = await command('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? 'Browser evaluation failed');
      return result.result.value;
    };
    const waitFor = async (expression, description = expression) => {
      for (let attempt = 0; attempt < 300; attempt++) {
        if (await evaluate(expression)) return;
        await pause();
      }
      assert.fail(`Timed out: ${description}`);
    };
    const viewport = (width, height = 900) =>
      command('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });

    const root = new URL('../../', import.meta.url);
    const css = await readFile(new URL('frontend/public/styles.css', root), 'utf8');
    await viewport(1400);
    await evaluate(`document.documentElement.dataset.theme = 'dark';
      document.head.innerHTML = '<meta name="viewport" content="width=device-width, initial-scale=1">';
      const style = document.createElement('style'); style.textContent = ${JSON.stringify(css)}; document.head.append(style);
      document.body.innerHTML = '<main id="page" class="content"></main>';`);
    const files = ['model.ts', 'api.ts', 'locale-catalogs.ts', 'locale.ts', 'ui.ts', 'plugin_catalog.ts', 'plugins.ts'];
    const source = (await Promise.all(files.map(file => readFile(new URL(`frontend/src/${file}`, root), 'utf8')))).join('\n');
    await evaluate(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }));
    await evaluate(fakeHost);

    // Installed: grouped by category, first plugin selected beside the list.
    await evaluate(`host.open()`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 4`);
    assert.deepEqual(await evaluate(`host.text('.plugin-tab')`), ['Installed4', 'Available3', 'Repositories2']);
    assert.deepEqual(await evaluate(`host.text('.plugin-group-heading')`), ['Core', 'Video', 'Other']);
    assert.deepEqual(await evaluate(`host.text('.plugin-row-name')`), ['Runtime', 'Displays', 'Frame limiter', 'Overlay']);
    assert.equal(await evaluate(`document.querySelector('.plugin-count').textContent`), '4 installed');
    assert.match(await evaluate(`document.querySelector('.plugin-package-line').textContent`), /Running package d{12}.*4 modules.*from house/);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail-pane h2').textContent`), 'Runtime');
    assert.equal(await evaluate(`document.querySelector('.plugin-row.selected').getAttribute('aria-current')`), 'true');
    await waitFor(`host.text('.plugin-detail-section .plugin-label').includes('Required by')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-detail-section .plugin-label')`), ['Required by', 'Provides', 'Uses']);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Size2\.8 MB/);
    // A long list of services stays short until it is asked for.
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-detail-section:last-of-type li').length`), 8);
    await evaluate(`host.press('Show all 14', document.querySelector('.plugin-detail'))`);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-detail-section:last-of-type li').length`), 14);

    // Search narrows the list; following a dependency clears a filter that hides it.
    await evaluate(`host.fill('plugin_search', 'overlay')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-row-name')`), ['Overlay']);
    assert.equal(await evaluate(`document.querySelector('.plugin-count').textContent`), '1 of 4');
    await waitFor(`host.text('.plugin-detail-section .plugin-label').includes('Depends on')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-detail-section:first-of-type li')`), ['Displays 0.2.0', 'Runtime 0.1.0']);
    await evaluate(`host.press('Displays', document.querySelector('.plugin-detail'))`);
    await waitFor(`document.querySelector('.plugin-detail-pane h2')?.textContent === 'Displays'`);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-row').length`), 4);
    assert.equal(await evaluate(`document.querySelector('input[name="plugin_search"]').value`), '');
    await evaluate(`host.fill('plugin_search', 'nothing matches this')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-quiet-state h3')`), ['No plugins match']);
    assert.equal(await evaluate(`getComputedStyle(document.querySelector('.plugin-detail-pane')).display`), 'none');

    // Arrow keys move through the list and the details follow.
    await evaluate(`host.fill('plugin_search', ''); host.row('Runtime').focus();`);
    await command('Input.dispatchKeyEvent', { type: 'keyDown', key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 });
    await waitFor(`document.querySelector('.plugin-detail-pane h2').textContent === 'Displays'`);
    assert.equal(await evaluate(`document.activeElement === host.row('Displays')`), true);

    // The package history names where each recorded package is listed.
    await evaluate(`document.querySelector('.plugin-package-details').open = true`);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-package-details tbody tr'), row => row.cells[0].textContent + '|' + row.cells[2].textContent)`),
      ['Running now|house', 'Active|house', 'Previous|Not in a current catalog', 'Pending|—', 'On trial|—']);

    // A host without the description route still lists its modules, and is asked once.
    await evaluate(`host.describe = 'missing'; host.open()`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 4`);
    await waitFor(`!document.querySelector('.plugin-detail').textContent.includes('Loading details')`);
    await evaluate(`host.row('Overlay').click(); host.row('Displays').click();`);
    assert.equal(await evaluate(`host.calls.filter(([route]) => route === 'modules/describe').length`), 1);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Version0\.2\.0/);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-detail-section').length`), 0);
    await evaluate(`host.describe = 'ok'`);

    // Available: each offer is compared with what the host runs.
    await evaluate(`document.querySelector('#plugin-tab-available').click()`);
    assert.deepEqual(await evaluate(`host.views`), ['available']);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-row'), row => row.querySelector('.plugin-row-name').textContent + ': ' + row.querySelector('.plugin-row-meta').textContent)`), [
      'Frame limiter: Other version installed · 0.3.0',
      'Overlay: Installed · 1.2.0',
      'RTX HDR: Not installed · 0.1.0',
    ]);
    assert.match(await evaluate(`document.querySelector('.plugin-detail-state').textContent`), /Version 0\.1\.0 is installed\. This repository offers 0\.3\.0\./);
    assert.match(await evaluate(`document.querySelector('.plugin-detail-section').textContent`), /cannot install a module set yet/);
    await evaluate(`host.row('Overlay').click()`);
    assert.match(await evaluate(`document.querySelector('.plugin-detail-state').textContent`), /running the package this repository lists/);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-detail-section').length`), 0);
    await evaluate(`host.row('RTX HDR').click(); host.press('Set up RTX HDR', document.querySelector('.plugin-detail'))`);
    assert.deepEqual(await evaluate(`host.navigated`), ['integrations']);

    // Repositories: status, and no way to remove the running package's source.
    await evaluate(`document.querySelector('#plugin-tab-repositories').click()`);
    assert.deepEqual(await evaluate(`host.text('.repository-card h3')`), ['house', 'stale']);
    assert.match(await evaluate(`host.card('house').textContent`), /3 plugins · catalog valid until/);
    assert.match(await evaluate(`host.card('house').textContent`), /cannot be removed/);
    assert.equal(await evaluate(`Array.from(host.card('house').querySelectorAll('button')).find(control => control.textContent === 'Remove').disabled`), true);
    assert.equal(await evaluate(`host.card('stale').querySelector('.repository-status--attention') !== null`), true);

    // A refused check is explained on the repository it concerns.
    await evaluate(`host.press('Check for updates', host.card('stale'))`);
    await waitFor(`host.card('stale')?.querySelector('[role="alert"]') !== null`, 'the refused check is reported');
    assert.match(await evaluate(`host.card('stale').querySelector('[role="alert"]').textContent`), /catalog has expired/);

    // Adding: pasted details fill the form, a wrong fingerprint is caught, a good one enrolls.
    await evaluate(`host.press('Add repository')`);
    await waitFor(`document.querySelector('dialog.plugin-form-dialog')?.open === true`);
    await evaluate(`(() => {
      const paste = new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: new DataTransfer() });
      paste.clipboardData.setData('text', JSON.stringify({ id: 'new-plugins', location: 'https://new.example.test/unsigned', public_key: 'AB'.repeat(32), fingerprint: 'ab'.repeat(32) }));
      document.querySelector('input[name="repository_location"]').dispatchEvent(paste);
    })()`);
    assert.deepEqual(await evaluate(`['repository_location', 'repository_id', 'repository_public_key', 'repository_fingerprint'].map(name => document.querySelector('input[name="' + name + '"]').value)`),
      ['https://new.example.test/unsigned', 'new-plugins', 'ab'.repeat(32), '']);
    assert.equal(await evaluate(`document.activeElement.name`), 'repository_fingerprint');
    await evaluate(`host.fill('repository_fingerprint', '1234 '.repeat(16)); document.querySelector('.plugin-form').requestSubmit();`);
    assert.equal(await evaluate(`host.calls.some(([route]) => route === 'repositories/add')`), false, 'the trust acknowledgement is required');
    await evaluate(`document.querySelector('input[name="repository_trust"]').click(); document.querySelector('.plugin-form').requestSubmit();`);
    await waitFor(`document.querySelector('.plugin-form [role="alert"]') !== null`);
    assert.match(await evaluate(`document.querySelector('.plugin-form [role="alert"]').textContent`), /fingerprint does not belong to this publisher key/);
    assert.equal(await evaluate(`document.querySelector('input[name="repository_fingerprint"]').getAttribute('aria-invalid')`), 'true');
    assert.deepEqual(await evaluate(`host.calls.find(([route]) => route === 'repositories/add')[1]`),
      { id: 'new-plugins', location: 'https://new.example.test/unsigned', public_key: 'ab'.repeat(32), fingerprint: '1234'.repeat(16) });

    // The host checks the catalog before it enrolls anything.
    await evaluate(`host.fill('repository_fingerprint', 'abab '.repeat(16)); document.querySelector('.plugin-form').requestSubmit();`);
    await waitFor(`document.querySelector('.plugin-form [role="alert"]')?.textContent.includes('not signed by this publisher key')`);
    assert.equal(await evaluate(`document.querySelector('input[name="repository_public_key"]').getAttribute('aria-invalid')`), 'true');
    assert.equal(await evaluate(`host.repositories.length`), 2);
    await evaluate(`host.fill('repository_location', 'https://new.example.test/repository'); document.querySelector('.plugin-form').requestSubmit();`);
    await waitFor(`document.querySelector('dialog.plugin-form-dialog') === null`, 'the dialog closes after enrollment');
    await waitFor(`host.card('new-plugins') !== undefined`);
    assert.equal(await evaluate(`host.notices.at(-1)`), 'Added new-plugins. 1 plugin is available.');
    assert.deepEqual(await evaluate(`host.text('.plugin-tab')`), ['Installed4', 'Available4', 'Repositories3']);

    // Removing asks first, then files the repository under removed ones.
    await evaluate(`host.press('Remove', host.card('new-plugins'))`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    assert.match(await evaluate(`document.querySelector('dialog.confirmation').textContent`), /Remove new-plugins\?.*can never be added again/);
    await evaluate(`host.press('Remove repository', document.querySelector('dialog.confirmation'))`);
    await waitFor(`document.querySelector('.repository-removed') !== null`, 'the repository is filed as removed');
    assert.equal(await evaluate(`host.notices.at(-1)`), 'Removed new-plugins.');
    assert.deepEqual(await evaluate(`host.text('.repository-removed .repository-card h3')`), ['new-plugins']);
    assert.equal(await evaluate(`document.querySelectorAll('.repository-removed .repository-card button:not(.plugin-copy)').length`), 0);
    assert.deepEqual(await evaluate(`host.text('.plugin-tab')`), ['Installed4', 'Available3', 'Repositories2']);

    // A host that only lists repositories offers no changes, and neither does a viewer.
    await evaluate(`host.manageable = false; host.open('repositories')`);
    await waitFor(`document.querySelectorAll('.repository-card').length >= 2`);
    assert.equal(await evaluate(`Array.from(document.querySelectorAll('#page button')).some(control => /Add repository|Remove|Check for updates/.test(control.textContent))`), false);
    assert.match(await evaluate(`document.querySelector('.repository-note').textContent`), /changes them with the release tool/);
    await evaluate(`host.manageable = true; host.role = 'viewer'; host.open('repositories')`);
    await waitFor(`document.querySelectorAll('.repository-card').length >= 2`);
    assert.equal(await evaluate(`Array.from(document.querySelectorAll('#page button')).some(control => /Add repository|Remove|Check for updates/.test(control.textContent))`), false);
    assert.match(await evaluate(`document.querySelector('.repository-note').textContent`), /Only an administrator/);
    await evaluate(`host.role = 'administrator'; host.scopes = []; host.open()`);
    await waitFor(`document.querySelector('.plugin-quiet-state') !== null`);
    assert.equal(await evaluate(`host.calls.length`), 0, 'nothing is requested without permission to read');
    await evaluate(`host.scopes = null`);

    // On a narrow screen the details open under the row and nothing scrolls sideways.
    await viewport(380, 800);
    await evaluate(`host.open()`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 4`);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail')`), null, 'nothing is opened until a row is chosen');
    await evaluate(`host.row('Displays').click()`);
    assert.equal(await evaluate(`host.row('Displays').parentElement.querySelector('.plugin-detail h2').textContent`), 'Displays');
    assert.equal(await evaluate(`host.row('Displays').getAttribute('aria-expanded')`), 'true');
    assert.equal(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), true);
    await evaluate(`host.row('Displays').click()`);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail')`), null, 'choosing the open row closes it');
    for (const view of ['available', 'repositories']) {
      await evaluate(`document.querySelector('#plugin-tab-${view}').click()`);
      assert.equal(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), true, `${view} fits a narrow screen`);
    }
    // Widening the window moves an open row's details back beside the list.
    await evaluate(`document.querySelector('#plugin-tab-installed').click(); host.row('Overlay').click()`);
    await viewport(1400);
    await waitFor(`document.querySelector('.plugin-detail-pane h2')?.textContent === 'Overlay'`);
    assert.equal(await evaluate(`host.row('Overlay').hasAttribute('aria-expanded')`), false);
  } finally {
    socket?.close();
    if (browser.exitCode === null) {
      const exited = new Promise(resolve => browser.once('exit', resolve));
      browser.kill('SIGTERM');
      await exited;
    }
    // Chromium's helpers can still be writing to the profile as it exits.
    await rm(profile, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
});
