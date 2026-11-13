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

// A host as the page sees it: four loaded modules from one repository, another
// publisher's repository beside it, repository changes that finish a moment
// after they are accepted, and the package-update routes when `host.pkg` is set.
// `host.pkg` also carries what a newer host adds to its status (`automatic`,
// `restoring`, `repository_behind`), and `host.versionsReply` the packages it kept.
const fakeHost = `
  window.host = {
    calls: [], notices: [], navigated: [], views: [],
    role: 'administrator', scopes: null, manageable: true, describe: 'ok', nextOperation: 1, operation: null,
    digest: 'd'.repeat(64),
    // null is a host from before the update routes existed.
    pkg: null, act: {}, refuse: {}, onStatus: null, offline: false, versionsReply: null,
    // A promise the next package-update reply waits for, to hold a request in flight.
    hold: null, release: null,
    modules: [
      { id: 'amaterasu.runtime', version: '0.1.0', sha256: '1'.repeat(64), runtime: true },
      { id: 'amaterasu.display', version: '0.2.0', sha256: '2'.repeat(64), runtime: false },
      { id: 'amaterasu.frame-limiter', version: '0.1.0', sha256: '3'.repeat(64), runtime: false },
      { id: 'example.overlay', version: '1.2.0', sha256: '4'.repeat(64), runtime: false },
    ],
    requires: { 'amaterasu.display': ['amaterasu.runtime'], 'example.overlay': ['amaterasu.display', 'amaterasu.runtime'] },
    repositories: [
      // The repository the running package came from. What it offers for the
      // slots the host runs is Amaterasu Core's business, not a list's.
      { id: 'house', location: 'https://house.example.test/repository', fingerprint: 'c'.repeat(64), revoked: false,
        catalog: { state: 'current', sequence: '9', expires_unix: 1900000000 },
        candidates: [
          { slot: 'example.overlay', implementation: 'example-overlay', version: '1.2.0', kind: 'native_set', manifest_sha256: 'd'.repeat(64) },
          { slot: 'amaterasu.frame-limiter', implementation: 'house-frame-pacer', version: '0.3.0', kind: 'native_set', manifest_sha256: 'e'.repeat(64) },
          { slot: 'amaterasu.rtx-hdr', implementation: 'amaterasu-linux-session-rtx-hdr', version: '0.1.0', kind: 'native_set', manifest_sha256: 'f'.repeat(64) },
        ] },
      { id: 'studio', location: 'https://studio.example.test/modules', fingerprint: '6'.repeat(64), revoked: false,
        catalog: { state: 'current', sequence: '4', expires_unix: 1900000000 },
        candidates: [
          { slot: 'example.overlay', implementation: 'studio-overlay', version: '1.2.0', kind: 'native_set', manifest_sha256: '8'.repeat(64) },
          { slot: 'amaterasu.frame-limiter', implementation: 'studio-frame-pacer', version: '0.3.0', kind: 'native_set', manifest_sha256: '9'.repeat(64) },
        ] },
      { id: 'stale', location: 'https://stale.example.test', fingerprint: '5'.repeat(64), revoked: false,
        catalog: { state: 'expired', expires_unix: 1700000000 }, candidates: [] },
    ],
  };
  const added = id => ({ id, from: null, to: '0.1.0', change: 'added', size: 100 });
  // Ten of its eleven modules change: more than the list shows at once.
  host.offer = {
    repository: 'house', slot: 'amaterasu.runtime', implementation: 'house-session', version: '0.2.0', digest: 'e'.repeat(64), sequence: '13',
    modules: [
      { id: 'amaterasu.runtime', from: '0.1.0', to: '0.2.0', change: 'updated', size: 100 },
      { id: 'amaterasu.display', from: '0.2.0', to: '0.2.0', change: 'rebuilt', size: 100 },
      { id: 'amaterasu.frame-limiter', from: '0.1.0', to: '0.1.0', change: 'unchanged', size: 100 },
      { id: 'example.overlay', from: '1.2.0', to: null, change: 'removed', size: 0 },
      ...['amaterasu.remote-session', 'amaterasu.input', 'amaterasu.audio', 'amaterasu.audio-encode', 'amaterasu.encode',
        'amaterasu.capture-kms', 'amaterasu.network'].map(added),
    ],
    download_bytes: 4194304, total_bytes: 8388608,
  };
  host.status = () => ({
    ok: true, supported: true, phase: 'current', checked_unix_ms: Date.now() - 5 * 60000,
    source: { repository: 'house', fingerprint: 'c'.repeat(64) }, running: { digest: host.digest, sequence: '12' },
    update: null, transferred_bytes: 0, error: null, ...host.pkg,
  });
  host.change = (kind, repository, finish) => {
    if (host.operation && host.operation.state === 'running') return { ok: false, code: 'repository_busy' };
    const id = host.nextOperation++;
    host.operation = { id, repository, kind, state: 'running', code: null };
    setTimeout(() => { host.operation = { id, repository, kind, ...finish() }; }, 150);
    return { ok: true, operation: id };
  };
  host.reply = (route, body) => {
    if (route === 'package-selection/status') return {
      ok: true, revision: 'a'.repeat(64), active: host.digest, previous: 'b'.repeat(64), pending: null, trial: null,
      highest_sequence: '12',
      running: { available: true, digest: host.digest, modules: host.modules, source: { repository: 'house', fingerprint: 'c'.repeat(64) } },
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
    if (route.startsWith('package-update/') && host.pkg) {
      const action = route.slice('package-update/'.length);
      if (action === 'versions') return host.versionsReply ? host.versionsReply() : { status: 404, ok: false, code: 'not_found' };
      if (host.pkg.supported === false) return { ok: true, supported: false };
      if (action === 'status') host.onStatus?.();
      else if (host.refuse[action]) return { ok: false, code: host.refuse[action] };
      else host.act[action]?.(body);
      return host.status();
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
      if (host.hold && route.startsWith('package-update/')) await host.hold;
      if (host.offline) throw new Error('The host could not be reached.');
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
  host.shown = selector => Array.from(document.querySelectorAll(selector)).filter(node => !node.closest('[hidden]')).map(node => node.textContent.trim());
  host.row = name => Array.from(document.querySelectorAll('.plugin-row')).find(row => row.querySelector('.plugin-row-name').textContent === name);
  host.card = id => Array.from(document.querySelectorAll('.repository-card')).find(card => card.querySelector('h3').textContent === id);
  host.family = name => Array.from(document.querySelectorAll('.plugin-family')).find(row => row.querySelector('h3').textContent === name);
  host.member = name => Array.from(document.querySelectorAll('.button.plugin-family-member')).find(row => row.querySelector('.plugin-family-member-name').textContent === name);
  host.button = (label, scope = document) => Array.from(scope.querySelectorAll('button')).find(control => control.textContent.trim() === label && !control.closest('[hidden]'));
  host.press = (label, scope = document) => host.button(label, scope).click();
  host.fill = (name, value) => { const control = document.querySelector('input[name="' + name + '"]'); control.value = value; control.dispatchEvent(new Event('input', { bubbles: true })); };
  host.headline = () => document.querySelector('.plugin-update-headline')?.textContent;
  host.summary = () => document.querySelector('.plugin-summary-headline')?.textContent;
  host.steps = () => Array.from(document.querySelectorAll('.plugin-update-step'), step => step.textContent + ':' + step.className.split('--')[1]);
  host.statusCalls = () => host.calls.filter(([route]) => route === 'package-update/status').length;
  host.holdNext = () => { host.hold = new Promise(resolve => { host.release = () => { host.hold = null; resolve(); }; }); };
  host.tabs = () => Array.from(document.querySelectorAll('.plugin-tab'), tab => tab.textContent + (tab.getAttribute('aria-selected') === 'true' ? '*' : ''));
`;

/// Runs a scenario against the page's own sources in headless Chromium.
async function withPage(prefix, run) {
  const profile = await mkdtemp(join(tmpdir(), prefix));
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
    await run({ command, evaluate, waitFor, viewport });
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
}

test('the Modules page lists, explains and changes what a host can run', { skip: !existsSync(chromium), timeout: 60000 }, async () => {
  await withPage('amaterasu-modules-', async ({ command, evaluate, waitFor, viewport }) => {
    // The overview is where the page opens: Amaterasu Core with a row per
    // family in a fixed order, then what belongs to none.
    await evaluate(`host.open()`);
    await waitFor(`document.querySelectorAll('.plugin-family').length === 11`);
    assert.equal(await evaluate(`document.querySelector('h1').textContent`), 'Modules');
    assert.equal(await evaluate(`document.querySelector('.page-heading p').textContent`),
      'What this host runs and how it stays up to date.');
    assert.deepEqual(await evaluate(`host.tabs()`), ['Overview*', 'All modules4', 'Repositories3']);
    assert.deepEqual(await evaluate(`host.text('.plugin-family h3')`), ['Input', 'Audio', 'Screen capture', 'Video encoding', 'Display',
      'Remote session', 'RTX HDR', 'Frame limiter', 'Statistics', 'Runtime and SDK', 'From other developers']);
    // A family the host runs needs no label; one it does not run says so.
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-family'), row => row.querySelector('.plugin-family-state')?.textContent ?? '')`),
      ['Not installed', 'Not installed', 'Not installed', 'Not installed', '', 'Not installed', 'Not installed', '', 'Not installed', '', '']);
    assert.match(await evaluate(`host.family('Display').querySelector('.plugin-family-summary').textContent`), /^Lists displays and prepares the right one/);
    assert.deepEqual(await evaluate(`Array.from(host.family('Display').querySelectorAll('.plugin-family-member'), row => row.textContent)`), ['Displays']);
    assert.deepEqual(await evaluate(`Array.from(host.family('Display').querySelectorAll('.plugin-family-sources a'), link => [link.textContent, link.href, link.target, link.rel].join(' '))`), [
      'nonary-streams/amaterasu-display https://github.com/nonary-streams/amaterasu-display _blank noopener noreferrer',
      'nonary-streams/amaterasu https://github.com/nonary-streams/amaterasu _blank noopener noreferrer',
    ]);
    assert.deepEqual(await evaluate(`Array.from(host.family('Runtime and SDK').querySelectorAll('.plugin-family-sources a'), link => link.textContent)`),
      ['nonary-streams/amaterasu', 'nonary-streams/amaterasu-sdk']);
    // A module another developer wrote that ships in the package is named, with no source claimed for it.
    assert.deepEqual(await evaluate(`Array.from(host.family('From other developers').querySelectorAll('.plugin-family-member'), row => row.textContent)`), ['Overlay']);
    assert.equal(await evaluate(`host.family('From other developers').querySelectorAll('.plugin-family-sources a').length`), 0);
    // The rows are a table of three columns on a wide screen.
    assert.equal(await evaluate(`getComputedStyle(host.family('Display')).gridTemplateColumns.split(' ').length`), 3);
    assert.equal(await evaluate(`document.querySelector('.plugin-included-head').textContent`), 'IncludedAll 4 modules');
    // RTX HDR points at where it is set up.
    assert.equal(await evaluate(`host.family('Remote session').querySelector('.plugin-family-members')`), null);
    assert.equal(await evaluate(`host.family('Remote session').classList.contains('plugin-family--absent')`), true);
    await evaluate(`host.press('Set up RTX HDR', host.family('RTX HDR'))`);
    assert.deepEqual(await evaluate(`host.navigated`), ['integrations']);
    // Headings descend in order: the page, Amaterasu Core and its families, then Plugins and its repositories.
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('h1, h2, h3'), heading => heading.tagName).join('')`),
      'H1H2' + 'H3'.repeat(11) + 'H2' + 'H3H3');

    // A host from before the update routes says so once, and the rest of the page works.
    assert.equal(await evaluate(`host.summary()`), 'This host is updated outside the console');
    assert.equal(await evaluate(`document.querySelector('.plugin-summary').dataset.tone`), 'neutral');
    assert.equal(await evaluate(`document.querySelector('.plugin-summary-detail').hidden`), true);
    assert.deepEqual(await evaluate(`host.text('.plugin-update-name')`), ['Amaterasu Core', 'Plugins']);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-state').hidden`), true);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), 'This host does not install updates from the console yet.');
    // The version is the one the repository gives the package the host runs.
    assert.match(await evaluate(`document.querySelector('.plugin-update-facts').textContent`), /^Version 1\.2\.0Package d{12}Copyfrom house$/);
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), true);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button, .plugin-update-step, .plugin-update-bar, .plugin-update-summary, .plugin-setting, .plugin-versions')`), []);
    assert.equal(await evaluate(`host.statusCalls()`), 1);

    // A member opens its details in All modules, with the keyboard on its row.
    await evaluate(`host.member('Displays').click()`);
    assert.deepEqual(await evaluate(`host.views`), ['modules']);
    assert.deepEqual(await evaluate(`host.tabs()`), ['Overview', 'All modules4*', 'Repositories3']);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail-pane h2').textContent`), 'Displays');
    assert.equal(await evaluate(`document.activeElement === host.row('Displays')`), true);
    await waitFor(`host.text('.plugin-detail-section .plugin-label').includes('Depends on')`);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Supplied byhousePart ofDisplaySourcenonary-streams\/amaterasu-displaySize/);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail .plugin-source-link').href`), 'https://github.com/nonary-streams/amaterasu-display');
    await evaluate(`host.row('Frame limiter').click()`);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Part ofFrame limiterSourcenonary-streams\/amaterasu-frame-limiter/);
    // A module of Amaterasu's outside every family is built into Amaterasu itself.
    await evaluate(`host.modules.push({ id: 'amaterasu.network', version: '0.1.0', sha256: '5'.repeat(64), runtime: false }); host.open('modules')`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 5`);
    await evaluate(`host.row('Network').click()`);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Supplied byhouseSourcenonary-streams\/amaterasuBuilt into Amaterasu/);
    assert.doesNotMatch(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Part of/);
    // Nothing is claimed about a module another publisher wrote.
    await evaluate(`host.row('Overlay').click()`);
    assert.doesNotMatch(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Part of|Source/);
    // The overview accounts for it in a row of its own, by name while there are few.
    await evaluate(`document.querySelector('#plugin-tab-overview').click()`);
    assert.deepEqual(await evaluate(`host.text('.plugin-family h3').slice(-2)`), ['Built into Amaterasu', 'From other developers']);
    assert.deepEqual(await evaluate(`Array.from(host.family('Built into Amaterasu').querySelectorAll('.plugin-family-member'), row => row.textContent)`), ['Network']);
    assert.deepEqual(await evaluate(`Array.from(host.family('Built into Amaterasu').querySelectorAll('.plugin-family-sources a'), link => link.textContent)`), ['nonary-streams/amaterasu']);
    // Many are counted, and the count opens the list of them all.
    await evaluate(`host.modules.push(...['nvhttp', 'pairing', 'rtsp', 'catalog'].map((name, index) => ({ id: 'amaterasu.' + name, version: '0.1.0', sha256: String(6 + index).repeat(64), runtime: false })));
      host.views.length = 0; host.open()`);
    await waitFor(`host.family('Built into Amaterasu') !== undefined`);
    assert.deepEqual(await evaluate(`Array.from(host.family('Built into Amaterasu').querySelectorAll('.plugin-family-member'), row => row.textContent)`), ['5 modules']);
    await evaluate(`host.press('5 modules', host.family('Built into Amaterasu'))`);
    assert.deepEqual(await evaluate(`host.views`), ['modules']);
    await evaluate(`host.modules.splice(4); host.views.length = 0`);

    // An address saved when the view was called Installed opens All modules:
    // grouped by category, first module selected beside the list.
    await evaluate(`host.open('installed')`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 4`);
    assert.deepEqual(await evaluate(`host.tabs()`), ['Overview', 'All modules4*', 'Repositories3']);
    assert.deepEqual(await evaluate(`host.text('.plugin-group-heading')`), ['Core', 'Video', 'Other']);
    assert.deepEqual(await evaluate(`host.text('.plugin-row-name')`), ['Runtime', 'Displays', 'Frame limiter', 'Overlay']);
    assert.equal(await evaluate(`document.querySelector('.plugin-count').textContent`), '4 installed');
    assert.equal(await evaluate(`document.querySelector('.plugin-detail-pane h2').textContent`), 'Runtime');
    assert.equal(await evaluate(`document.querySelector('.plugin-row.selected').getAttribute('aria-current')`), 'true');
    await waitFor(`host.text('.plugin-detail-section .plugin-label').includes('Required by')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-detail-section .plugin-label')`), ['Required by', 'Provides', 'Uses']);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Part ofRuntime and SDKSourcenonary-streams\/amaterasuSize2\.8 MB/);
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
    assert.deepEqual(await evaluate(`host.text('.plugin-quiet-state h3')`), ['No modules match']);
    assert.equal(await evaluate(`getComputedStyle(document.querySelector('.plugin-detail-pane')).display`), 'none');
    // A family member hidden by the search is still opened from its row.
    await evaluate(`document.querySelector('#plugin-tab-overview').click(); host.member('Runtime').click()`);
    assert.equal(await evaluate(`document.querySelector('input[name="plugin_search"]').value`), '');
    assert.equal(await evaluate(`document.querySelector('.plugin-detail-pane h2').textContent`), 'Runtime');

    // Arrow keys move through the list and the details follow.
    await evaluate(`host.row('Runtime').focus();`);
    await command('Input.dispatchKeyEvent', { type: 'keyDown', key: 'ArrowDown', code: 'ArrowDown', windowsVirtualKeyCode: 40 });
    await waitFor(`document.querySelector('.plugin-detail-pane h2').textContent === 'Displays'`);
    assert.equal(await evaluate(`document.activeElement === host.row('Displays')`), true);

    // The tabs keep one stop for the keyboard, and the arrows walk them in order.
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('[role="tablist"] [role="tab"]'), tab => tab.tabIndex)`), [-1, 0, -1]);
    await evaluate(`document.querySelector('#plugin-tab-modules').focus()`);
    for (const expected of ['repositories', 'overview', 'modules']) {
      await command('Input.dispatchKeyEvent', { type: 'keyDown', key: 'ArrowRight', code: 'ArrowRight', windowsVirtualKeyCode: 39 });
      assert.equal(await evaluate(`document.activeElement.id`), `plugin-tab-${expected}`);
      assert.equal(await evaluate(`document.querySelector('[role="tabpanel"]').getAttribute('aria-labelledby')`), `plugin-tab-${expected}`);
    }
    await command('Input.dispatchKeyEvent', { type: 'keyDown', key: 'ArrowLeft', code: 'ArrowLeft', windowsVirtualKeyCode: 37 });
    assert.equal(await evaluate(`document.activeElement.id`), 'plugin-tab-overview');
    assert.deepEqual(await evaluate(`host.views.slice(-4)`), ['repositories', 'overview', 'modules', 'overview']);

    // The package record sits under Amaterasu Core, closed until asked for,
    // and names where each recorded package is listed.
    assert.equal(await evaluate(`document.querySelector('.plugin-update-source .plugin-package-record').open`), false);
    assert.equal(await evaluate(`document.querySelector('.plugin-package-record summary').textContent`), 'Package record');
    await evaluate(`document.querySelector('.plugin-package-record').open = true`);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-package-record tbody tr'), row => row.cells[0].textContent + '|' + row.cells[2].textContent)`),
      ['Running now|house', 'Active|house', 'Previous|Not in a current catalog', 'Pending|—', 'On trial|—']);

    // A host without the description route still lists its modules, and is asked once.
    await evaluate(`host.describe = 'missing'; host.open('modules')`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 4`);
    await waitFor(`!document.querySelector('.plugin-detail').textContent.includes('Loading details')`);
    await evaluate(`host.row('Overlay').click(); host.row('Displays').click();`);
    assert.equal(await evaluate(`host.calls.filter(([route]) => route === 'modules/describe').length`), 1);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Version0\.2\.0/);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-detail-section').length`), 0);
    await evaluate(`host.describe = 'ok'`);

    // Plugins: what the repositories offer beyond the host's own package,
    // compared with what the host runs. An address saved when this was a
    // view of its own opens the overview, where it now lives.
    for (const former of ['available', 'publishers', 'core']) {
      await evaluate(`host.views.length = 0; host.open('${former}')`);
      await waitFor(`document.querySelectorAll('.plugin-others .plugin-row').length === 3`);
      assert.deepEqual(await evaluate(`host.tabs()`), ['Overview*', 'All modules4', 'Repositories3']);
    }
    assert.equal(await evaluate(`document.querySelector('.plugin-others h2').textContent`), 'Plugins');
    assert.deepEqual(await evaluate(`host.text('.plugin-others .plugin-group-heading')`), ['house', 'studio']);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-others .plugin-row'), row => row.querySelector('.plugin-row-name').textContent + ': ' + row.querySelector('.plugin-row-meta').textContent)`), [
      'RTX HDR: Not installed · 0.1.0',
      'Frame limiter: Other version installed · 0.3.0',
      'Overlay: Installed · 1.2.0',
    ]);
    // A handful of plugins needs no search, and none is opened until it is chosen.
    assert.equal(await evaluate(`document.querySelector('.plugin-others input[name="plugin_search"]')`), null);
    assert.equal(await evaluate(`document.querySelector('.plugin-others .plugin-detail')`), null);
    assert.equal(await evaluate(`getComputedStyle(document.querySelector('.plugin-others .plugin-detail-pane')).display`), 'none');
    // Its details open under its row, at any width.
    await evaluate(`host.row('Frame limiter').click()`);
    assert.equal(await evaluate(`host.row('Frame limiter').getAttribute('aria-expanded')`), 'true');
    assert.equal(await evaluate(`host.row('Frame limiter').parentElement.querySelector('.plugin-detail h4').textContent`), 'Frame limiter');
    assert.match(await evaluate(`document.querySelector('.plugin-detail-state').textContent`), /Version 0\.1\.0 is installed\. This repository offers 0\.3\.0\./);
    assert.match(await evaluate(`document.querySelector('.plugin-detail-section').textContent`), /cannot install a module set yet/);
    assert.match(await evaluate(`document.querySelector('.plugin-detail').textContent`), /Repositorystudio/);
    await evaluate(`host.row('Overlay').click()`);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-others .plugin-detail').length`), 1, 'one plugin is open at a time');
    assert.match(await evaluate(`document.querySelector('.plugin-detail-state').textContent`), /loaded from a different package than the one listed here/);
    await evaluate(`host.row('Overlay').click()`);
    assert.equal(await evaluate(`document.querySelector('.plugin-others .plugin-detail')`), null, 'choosing the open row closes it');
    await evaluate(`host.navigated.length = 0; host.row('RTX HDR').click(); host.press('Set up RTX HDR', document.querySelector('.plugin-detail'))`);
    assert.deepEqual(await evaluate(`host.navigated`), ['integrations']);
    // A catalog's worth of plugins gets a search.
    await evaluate(`host.repositories[1].candidates.push(...['recorder', 'overlay-two', 'mixer', 'relay'].map((name, index) =>
        ({ slot: 'studio.' + name, implementation: 'studio-' + name, version: '1.0.0', kind: 'native_set', manifest_sha256: String(index).repeat(64) })));
      host.open()`);
    await waitFor(`document.querySelectorAll('.plugin-others .plugin-row').length === 7`);
    assert.equal(await evaluate(`document.querySelector('.plugin-others .plugin-count').textContent`), '7 offered');
    await evaluate(`host.fill('plugin_search', 'mixer')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-others .plugin-row-name')`), ['Mixer']);
    assert.equal(await evaluate(`document.querySelector('.plugin-others .plugin-count').textContent`), '1 of 7');
    await evaluate(`host.fill('plugin_search', 'nothing matches this')`);
    assert.deepEqual(await evaluate(`host.text('.plugin-quiet-state h3')`), ['No plugins match']);
    await evaluate(`host.fill('plugin_search', ''); host.repositories[1].candidates.splice(2)`);
    // A host whose repositories offer nothing more says so in a line.
    await evaluate(`host.kept = host.repositories.splice(1); host.spare = host.repositories[0].candidates.pop(); host.open()`);
    await waitFor(`document.querySelector('.plugin-others-empty') !== null`);
    assert.equal(await evaluate(`document.querySelector('.plugin-others-empty').textContent`), "No plugins yet. Add a developer's repository to see what it offers.");
    assert.equal(await evaluate(`host.button('Add repository', document.querySelector('.plugin-others')) !== undefined`), true);
    await evaluate(`host.repositories[0].candidates.push(host.spare); host.repositories.push(...host.kept); host.views.length = 0; host.open()`);
    await waitFor(`document.querySelectorAll('.plugin-others .plugin-row').length === 3`);

    // Repositories: status, and no way to remove the running package's source.
    await evaluate(`document.querySelector('#plugin-tab-repositories').click()`);
    assert.deepEqual(await evaluate(`host.views`), ['repositories']);
    assert.deepEqual(await evaluate(`host.text('.repository-card h3')`), ['house', 'studio', 'stale']);
    assert.match(await evaluate(`host.card('house').textContent`), /3 plugins · catalog valid until/);
    assert.match(await evaluate(`host.card('house').textContent`), /cannot be removed/);
    assert.equal(await evaluate(`Array.from(host.card('house').querySelectorAll('button')).find(control => control.textContent === 'Remove').disabled`), true);
    assert.equal(await evaluate(`host.card('stale').querySelector('.repository-status--attention') !== null`), true);

    // A refused check is explained on the repository it concerns.
    await evaluate(`host.press('Check for updates', host.card('stale'))`);
    await waitFor(`host.card('stale')?.querySelector('[role="alert"]') !== null`, 'the refused check is reported');
    assert.match(await evaluate(`host.card('stale').querySelector('[role="alert"]').textContent`), /catalog has expired/);

    // Adding: pasted details fill the form, a wrong fingerprint is caught, a good one enrolls.
    await evaluate(`host.press('Add repository', document.querySelector('#plugin-view'))`);
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
    assert.equal(await evaluate(`host.repositories.length`), 3);
    await evaluate(`host.fill('repository_location', 'https://new.example.test/repository'); document.querySelector('.plugin-form').requestSubmit();`);
    await waitFor(`document.querySelector('dialog.plugin-form-dialog') === null`, 'the dialog closes after enrollment');
    await waitFor(`host.card('new-plugins') !== undefined`);
    assert.equal(await evaluate(`host.notices.at(-1)`), 'Added new-plugins. 1 plugin is available.');
    assert.deepEqual(await evaluate(`host.tabs()`), ['Overview', 'All modules4', 'Repositories4*']);

    // Removing asks first, then files the repository under removed ones.
    await evaluate(`host.press('Remove', host.card('new-plugins'))`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    assert.match(await evaluate(`document.querySelector('dialog.confirmation').textContent`), /Remove new-plugins\?.*can never be added again/);
    await evaluate(`host.press('Remove repository', document.querySelector('dialog.confirmation'))`);
    await waitFor(`document.querySelector('.repository-removed') !== null`, 'the repository is filed as removed');
    assert.equal(await evaluate(`host.notices.at(-1)`), 'Removed new-plugins.');
    assert.deepEqual(await evaluate(`host.text('.repository-removed .repository-card h3')`), ['new-plugins']);
    assert.equal(await evaluate(`document.querySelectorAll('.repository-removed .repository-card button:not(.plugin-copy)').length`), 0);
    assert.deepEqual(await evaluate(`host.tabs()`), ['Overview', 'All modules4', 'Repositories3*']);
    // Adding from the overview's Plugins section opens the same form.
    await evaluate(`document.querySelector('#plugin-tab-overview').click(); host.press('Add repository', document.querySelector('.plugin-others'))`);
    await waitFor(`document.querySelector('dialog.plugin-form-dialog')?.open === true`);
    await evaluate(`host.press('Cancel', document.querySelector('dialog.plugin-form-dialog'))`);
    assert.equal(await evaluate(`document.querySelector('dialog.plugin-form-dialog')`), null);

    // A host that only lists repositories offers no changes, and neither does a viewer.
    const changes = `Array.from(document.querySelectorAll('#plugin-view button')).some(control => /Add repository|Remove|Check for updates/.test(control.textContent))`;
    await evaluate(`host.manageable = false; host.open('repositories')`);
    await waitFor(`document.querySelectorAll('.repository-card').length >= 3`);
    assert.equal(await evaluate(changes), false);
    assert.match(await evaluate(`document.querySelector('.repository-note').textContent`), /changes them with the release tool/);
    await evaluate(`document.querySelector('#plugin-tab-overview').click()`);
    assert.equal(await evaluate(changes), false, 'the overview offers no repository to add either');
    await evaluate(`host.manageable = true; host.role = 'viewer'; host.open('repositories')`);
    await waitFor(`document.querySelectorAll('.repository-card').length >= 3`);
    assert.equal(await evaluate(changes), false);
    assert.match(await evaluate(`document.querySelector('.repository-note').textContent`), /Only an administrator/);
    await evaluate(`host.role = 'administrator'; host.scopes = []; host.open()`);
    await waitFor(`document.querySelector('.plugin-quiet-state') !== null`);
    assert.deepEqual(await evaluate(`host.text('.plugin-quiet-state h3')`), ['Modules are unavailable']);
    assert.equal(await evaluate(`document.querySelector('.plugin-summary')`), null);
    assert.equal(await evaluate(`host.calls.length`), 0, 'nothing is requested without permission to read');
    await evaluate(`host.scopes = null`);

    // On a narrow screen the details open under the row and nothing scrolls sideways.
    await viewport(380, 800);
    await evaluate(`host.open('modules')`);
    await waitFor(`document.querySelectorAll('.plugin-row').length === 4`);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail')`), null, 'nothing is opened until a row is chosen');
    await evaluate(`host.row('Displays').click()`);
    assert.equal(await evaluate(`host.row('Displays').parentElement.querySelector('.plugin-detail h2').textContent`), 'Displays');
    assert.equal(await evaluate(`host.row('Displays').getAttribute('aria-expanded')`), 'true');
    assert.equal(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), true);
    await evaluate(`host.row('Displays').click()`);
    assert.equal(await evaluate(`document.querySelector('.plugin-detail')`), null, 'choosing the open row closes it');
    for (const view of ['overview', 'repositories']) {
      await evaluate(`document.querySelector('#plugin-tab-${view}').click()`);
      assert.equal(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), true, `${view} fits a narrow screen`);
    }
    // The families stack into one column there.
    await evaluate(`document.querySelector('#plugin-tab-overview').click()`);
    assert.equal(await evaluate(`getComputedStyle(host.family('Display')).gridTemplateColumns.split(' ').length`), 1);
    // A member chosen from its row opens under its row in All modules there too.
    await evaluate(`host.member('Frame limiter').click()`);
    assert.equal(await evaluate(`host.row('Frame limiter').parentElement.querySelector('.plugin-detail h2').textContent`), 'Frame limiter');
    // Widening the window moves an open row's details back beside the list.
    await evaluate(`host.row('Overlay').click()`);
    await viewport(1400);
    await waitFor(`document.querySelector('.plugin-detail-pane h2')?.textContent === 'Overlay'`);
    assert.equal(await evaluate(`host.row('Overlay').hasAttribute('aria-expanded')`), false);
  });
});

test('the page follows Amaterasu Core from a check to a restart', { skip: !existsSync(chromium), timeout: 120000 }, async () => {
  await withPage('amaterasu-updates-', async ({ evaluate, waitFor, viewport }) => {
    const settle = `document.querySelectorAll('.plugin-family').length === 11`;
    const offer = `{ update: host.offer, transferred_bytes: 4194304 }`;

    // A platform the host cannot install on is told apart from an older host.
    await evaluate(`host.pkg = { supported: false }; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.summary()`), 'This host is updated outside the console');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), 'Updates from the console are not available on this platform yet.');
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), true);

    // Up to date: the page answers in one line, the row in two words, and nothing is asked again.
    await evaluate(`host.pkg = {}; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.summary()`), 'Everything is up to date');
    assert.equal(await evaluate(`document.querySelector('.plugin-summary').dataset.tone`), 'good');
    assert.equal(await evaluate(`document.querySelector('.plugin-summary-headline').getAttribute('role')`), 'status');
    assert.equal(await evaluate(`document.querySelector('.plugin-summary-detail').textContent`), 'Last checked 5 minutes ago');
    // The exact time is there for whoever wants it.
    assert.equal(await evaluate(`(() => { const time = document.querySelector('.plugin-summary-detail time');
      return !Number.isNaN(Date.parse(time.dateTime)) && /\\d{4}/.test(time.title) && Math.abs(Date.now() - 5 * 60000 - Date.parse(time.dateTime)) < 60000; })()`), true);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-name').textContent + '|' + host.headline()`), 'Amaterasu Core|Up to date');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-source').dataset.tone`), 'good');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-state').getAttribute('aria-live')`), 'polite');
    assert.match(await evaluate(`document.querySelector('.plugin-update-facts').textContent`), /^Version 1\.2\.0Package d{12}Copyfrom house$/);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-about').textContent`),
      'Every module Amaterasu ships. They update together as one signed package, and the host goes back to the package it had if a new one does not start.');
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button, .plugin-update-step, .plugin-update-bar, .plugin-update-summary, .plugin-update-detail')`), []);
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), false);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-source .plugin-package-record').open`), false);
    assert.deepEqual(await evaluate(`host.text('.plugin-family-state--changes')`), []);
    // A host that says nothing about keeping itself up to date is offered no setting for it.
    assert.deepEqual(await evaluate(`host.shown('.plugin-setting, .plugin-versions')`), []);
    await pause(1800);
    assert.equal(await evaluate(`host.statusCalls()`), 1, 'a settled host is not polled');

    // A host ahead of its repository is up to date, and says why nothing newer was found.
    await evaluate(`host.pkg = { repository_behind: true }; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.summary() + '|' + host.headline()`), 'Everything is up to date|Up to date');
    assert.match(await evaluate(`host.shown('.plugin-update-detail')[0]`), /^Nothing newer is published\. This host runs a later package than its repository lists/);

    // Checking is followed until the host answers. The update it finds is
    // listed by what it changes, and the families it touches say so without
    // taking the keyboard from the row it was on.
    await evaluate(`host.pkg = {}; host.open()`);
    await waitFor(settle);
    await evaluate(`host.act.check = () => {
        host.pkg = { phase: 'checking' };
        host.onStatus = () => { host.pkg = { phase: 'available', update: host.offer, checked_unix_ms: Date.now() }; host.onStatus = null; };
      };
      host.press('Check for updates'); host.member('Runtime').focus();`);
    await waitFor(`host.headline() === 'Checking for updates'`);
    assert.equal(await evaluate(`host.summary()`), 'Checking for updates…');
    assert.equal(await evaluate(`document.querySelector('.plugin-summary button').textContent + '|' + document.querySelector('.plugin-summary button').getAttribute('aria-disabled')`), 'Checking…|true');
    await waitFor(`host.headline() === 'Update available'`);
    assert.equal(await evaluate(`host.summary() + '|' + document.querySelector('.plugin-summary').dataset.tone`), '1 update available|attention');
    assert.deepEqual(await evaluate(`host.calls.filter(([route]) => route.startsWith('package-update/')).map(([route, body]) => route + JSON.stringify(body))`),
      ['package-update/status{}', 'package-update/check{}', 'package-update/status{}']);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-source').dataset.tone`), 'attention');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-summary').textContent`), '10 of 11 modules change · 4.0 MB to download');
    assert.deepEqual(await evaluate(`host.text('.plugin-update-list li')`),
      ['Runtime0.1.0 → 0.2.0', 'Displaysrebuilt', 'Audio captureadded', 'Audio encoderadded', 'Inputadded', 'Networkadded']);
    assert.equal(await evaluate(`document.querySelector('.plugin-summary-detail').textContent`), 'Last checked just now');
    assert.deepEqual(await evaluate(`host.steps()`), ['Download:upcoming', 'Verify:upcoming', 'Wait for idle:upcoming', 'Restart:upcoming']);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), ['Download update']);
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), false);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-family'), row => row.querySelector('.plugin-family-state')?.textContent ?? '')`),
      ['Changes in this update', 'Changes in this update', 'Changes in this update', 'Changes in this update', 'Changes in this update',
        'Changes in this update', 'Not installed', '', 'Not installed', 'Changes in this update', 'Changes in this update']);
    // A module's own version is shown where the update changes it.
    assert.equal(await evaluate(`host.member('Runtime').textContent`), 'Runtime0.1.0 → 0.2.0');
    assert.equal(await evaluate(`host.member('Displays').textContent`), 'Displaysrebuilt');
    assert.equal(await evaluate(`host.member('Frame limiter').textContent`), 'Frame limiter');
    assert.equal(await evaluate(`host.member('Overlay').textContent`), 'Overlayremoved');
    assert.equal(await evaluate(`host.family('Input').querySelector('li.plugin-family-member').textContent`), 'Inputadded');
    assert.equal(await evaluate(`host.family('Input').classList.contains('plugin-family--absent')`), false);
    assert.equal(await evaluate(`document.activeElement === host.member('Runtime')`), true, 'the keyboard stays on the row it was on');
    await evaluate(`host.press('Show all 10', document.querySelector('.plugin-update-source'))`);
    assert.deepEqual(await evaluate(`host.text('.plugin-update-list li').slice(6)`),
      ['Remote sessionadded', 'Screen capture (KMS)added', 'Video encoderadded', 'Overlayremoved']);

    // Downloading shows real progress. The control that started it is gone, so
    // the keyboard moves to the row rather than falling off the page.
    await evaluate(`host.act.download = () => {
        host.pkg = { phase: 'downloading', update: host.offer, transferred_bytes: 1048576 };
        host.onStatus = () => { host.pkg = { ...host.pkg, transferred_bytes: 3145728 }; host.onStatus = null; };
      };
      document.querySelector('.plugin-package-record').open = true;
      host.holdNext(); host.button('Download update').focus(); host.press('Download update');`);
    // While the host has not answered, nothing else can be asked of it.
    await pause(200);
    assert.equal(await evaluate(`host.headline()`), 'Update available');
    assert.equal(await evaluate(`host.button('Download update').disabled && host.button('Check for updates').disabled`), true);
    await evaluate(`host.release()`);
    await waitFor(`host.headline() === 'Downloading update'`);
    assert.equal(await evaluate(`host.summary()`), '1 update available');
    assert.deepEqual(await evaluate(`(() => { const bar = document.querySelector('progress.plugin-update-bar'); return [bar.max, bar.value, bar.getAttribute('aria-label')]; })()`),
      [4194304, 1048576, 'Download progress']);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-received').textContent`), '1.0 MB of 4.0 MB');
    assert.deepEqual(await evaluate(`host.steps()`), ['Download:current', 'Verify:upcoming', 'Wait for idle:upcoming', 'Restart:upcoming']);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-step[aria-current="step"]').textContent`), 'Download');
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), ['Cancel']);
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), true);
    assert.equal(await evaluate(`document.activeElement.textContent`), 'Amaterasu Core');
    // Reading the host again changes the numbers and nothing else: the list
    // stays open, the record stays open, the keyboard stays put.
    await evaluate(`host.button('Cancel').focus()`);
    await waitFor(`document.querySelector('progress.plugin-update-bar').value === 3145728`);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-received').textContent`), '3.0 MB of 4.0 MB');
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-update-list li').length`), 10);
    assert.equal(await evaluate(`document.querySelector('.plugin-package-record').open`), true);
    assert.equal(await evaluate(`document.activeElement === host.button('Cancel')`), true);
    // Everything received but not yet called ready: the host is verifying.
    await evaluate(`host.onStatus = () => {
      host.pkg = { ...host.pkg, transferred_bytes: 4194304 };
      host.onStatus = () => { host.pkg = { phase: 'ready', ...${offer} }; host.onStatus = null; };
    }`);
    await waitFor(`host.steps()[1] === 'Verify:current'`);
    assert.equal(await evaluate(`host.steps()[0]`), 'Download:done');
    assert.equal(await evaluate(`document.activeElement === host.button('Cancel')`), true);

    // Ready: the download is over, and installing is one confirmed request away.
    await waitFor(`host.headline() === 'Ready to install'`);
    assert.deepEqual(await evaluate(`host.steps()`), ['Download:done', 'Verify:done', 'Wait for idle:upcoming', 'Restart:upcoming']);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-summary').textContent`), '10 of 11 modules change · 4.0 MB downloaded');
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button, .plugin-update-bar')`), ['Install when idle']);
    assert.equal(await evaluate(`document.activeElement.textContent`), 'Amaterasu Core', 'the control that went away hands the keyboard to the row');
    const polled = await evaluate(`host.statusCalls()`);
    // A refusal is said in plain words on the row, and nothing is installed.
    await evaluate(`host.refuse.activate = 'not_ready'; host.press('Install when idle')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    assert.match(await evaluate(`document.querySelector('dialog.confirmation').textContent`), /Install the Amaterasu Core update\?.*restarts with the new package as soon as no stream is running/);
    await evaluate(`host.press('Cancel', document.querySelector('dialog.confirmation'))`);
    assert.equal(await evaluate(`host.calls.some(([route]) => route === 'package-update/activate')`), false, 'declining sends nothing');
    await evaluate(`host.press('Install when idle')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    await evaluate(`host.press('Install when idle', document.querySelector('dialog.confirmation'))`);
    await waitFor(`document.querySelector('.plugin-update-refusal:not([hidden])') !== null`);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-refusal').textContent + '|' + document.querySelector('.plugin-update-refusal').getAttribute('role')`),
      'The update has not finished downloading, so it cannot be installed yet.|alert');
    assert.equal(await evaluate(`host.headline()`), 'Ready to install');

    // Installing waits for the stream that is running; waiting can be called off.
    await evaluate(`host.refuse = {}; host.act.activate = () => { host.pkg = { phase: 'waiting_stream', ...${offer} }; };
      host.act.cancel = () => { host.pkg = { phase: 'cancelled', update: host.offer }; };
      host.press('Install when idle')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    await evaluate(`host.press('Install when idle', document.querySelector('dialog.confirmation'))`);
    await waitFor(`host.headline() === 'Waiting for the stream to end'`);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-refusal').hidden`), true);
    assert.deepEqual(await evaluate(`host.steps()`), ['Download:done', 'Verify:done', 'Wait for idle:current', 'Restart:upcoming']);
    assert.match(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), /as soon as the stream that is running ends/);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), ['Cancel']);
    await waitFor(`host.statusCalls() > ${polled}`, 'a waiting host is followed');
    await evaluate(`host.press('Cancel')`);
    await waitFor(`host.headline() === 'Update cancelled'`);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button, .plugin-update-step')`), ['Download update']);
    assert.match(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), /^Nothing was installed\./);
    assert.equal(await evaluate(`host.summary()`), '1 update available', 'a cancelled update is still there to be had');

    // The host restarts to install. It is silent while it does, then comes
    // back running the new package, and the whole page is read again.
    await evaluate(`host.pkg = { phase: 'ready', ...${offer} }; host.notices.length = 0; host.open();`);
    await waitFor(settle);
    await evaluate(`host.act.activate = () => { host.pkg = { phase: 'activating', ...${offer} }; setTimeout(() => { host.offline = true; }, 0); };
      host.press('Install when idle')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    await evaluate(`host.press('Install when idle', document.querySelector('dialog.confirmation'))`);
    await waitFor(`host.headline() === 'Installing'`);
    assert.deepEqual(await evaluate(`host.steps()`), ['Download:done', 'Verify:done', 'Wait for idle:done', 'Restart:current']);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), []);
    const silent = await evaluate(`host.statusCalls()`);
    await waitFor(`host.statusCalls() >= ${silent} + 2`, 'a restarting host is asked again');
    assert.equal(await evaluate(`host.headline()`), 'Installing', 'silence during a restart is not a failure');
    await evaluate(`host.digest = 'e'.repeat(64); host.pkg = { checked_unix_ms: Date.now() }; host.offline = false;
      host.modules = [...host.modules, { id: 'amaterasu.input', version: '0.1.0', sha256: '9'.repeat(64), runtime: false }];`);
    await waitFor(`host.headline() === 'Up to date' && host.member('Input') !== undefined`);
    assert.equal(await evaluate(`host.summary()`), 'Everything is up to date');
    // The version follows the package: it is the one the repository gives the new one.
    assert.match(await evaluate(`document.querySelector('.plugin-update-facts').textContent`), /^Version 0\.3\.0Package e{12}Copyfrom house$/);
    assert.deepEqual(await evaluate(`host.notices`), ['Update installed. The host is running package eeeeeeeeeeee.']);
    assert.deepEqual(await evaluate(`host.text('.plugin-family-state--changes')`), []);
    assert.equal(await evaluate(`host.calls.filter(([route]) => route === 'package-selection/status').length`), 2);
    await evaluate(`host.digest = 'd'.repeat(64); host.modules.pop()`);

    // A failure names its cause in words someone can act on.
    await evaluate(`host.pkg = { phase: 'failed', error: 'verification_failed', update: host.offer }; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.headline()`), 'Update failed');
    assert.equal(await evaluate(`host.summary() + '|' + document.querySelector('.plugin-summary').dataset.tone`), 'An update needs attention|danger');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-source').dataset.tone`), 'danger');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-detail').textContent`),
      'The downloaded files did not match what the publisher signed, so the host discarded them. Download the update again.');
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button, .plugin-update-step')`), ['Download update']);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-summary').textContent`), '10 of 11 modules change · 4.0 MB to download');
    await evaluate(`host.pkg = { phase: 'failed', error: 'something_new' }; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), 'The update did not complete. Check for updates to try again.');
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button, .plugin-update-summary')`), []);

    // A status that cannot be read is neither an older host nor a failed update, and can be asked for again.
    await evaluate(`host.pkg = { phase: 'what' }; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.headline()`), 'Status unavailable');
    assert.equal(await evaluate(`host.summary()`), 'Update status is unavailable');
    assert.equal(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), 'Package update phase is invalid.');
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), true);
    await evaluate(`host.pkg = {}; host.press('Try again')`);
    await waitFor(`host.headline() === 'Up to date'`);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), []);

    // An account is shown what it may not do instead of being refused after the fact.
    await evaluate(`host.pkg = { phase: 'available', update: host.offer }; host.role = 'viewer'; host.scopes = ['configuration.read']; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.button('Check for updates').disabled && host.button('Download update').disabled`), true);
    assert.deepEqual(await evaluate(`host.shown('.plugin-summary-note')`), ['This account can see updates but cannot change them.']);
    // A token can fetch an update; installing takes an administrator who is signed in.
    await evaluate(`host.pkg = { phase: 'ready', ...${offer} }; host.role = 'token'; host.scopes = null; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`host.button('Check for updates').disabled`), false);
    assert.equal(await evaluate(`host.button('Install when idle').disabled`), true);
    assert.deepEqual(await evaluate(`host.shown('.plugin-summary-note, .plugin-update-permission')`),
      ['Installing an update needs an administrator who is signed in to the console.']);
    await evaluate(`host.role = 'administrator'`);

    // Leaving the page ends the polling with it, even with a request under way.
    await evaluate(`host.pkg = { phase: 'downloading', update: host.offer, transferred_bytes: 1 }; host.open()`);
    await waitFor(`host.statusCalls() >= 2`, 'a download is followed');
    await evaluate(`host.pageGuard(); host.left = host.statusCalls()`);
    await pause(1800);
    assert.equal(await evaluate(`host.statusCalls() === host.left`), true, 'nothing is asked after the page is left');
    await evaluate(`host.open()`);
    await waitFor(settle);
    await evaluate(`host.holdNext(); host.left = host.statusCalls() + 1`);
    await waitFor(`host.statusCalls() === host.left`, 'a request is under way');
    await evaluate(`host.pageGuard(); host.release()`);
    await pause(1800);
    assert.equal(await evaluate(`host.statusCalls() === host.left`), true, 'an answer that arrives after the page is left starts nothing');

    // An update in progress fits a narrow screen in every view.
    await viewport(380, 800);
    await evaluate(`host.open()`);
    await waitFor(settle);
    for (const view of ['overview', 'modules', 'repositories']) {
      await evaluate(`document.querySelector('#plugin-tab-${view}').click()`);
      assert.equal(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), true, `${view} fits a narrow screen`);
    }
    assert.equal(await evaluate(`document.querySelector('.plugin-summary').getBoundingClientRect().right <= innerWidth`), true);
  });
});

test('Amaterasu Core keeps itself up to date and can go back to an earlier package', { skip: !existsSync(chromium), timeout: 120000 }, async () => {
  await withPage('amaterasu-versions-', async ({ evaluate, waitFor, viewport }) => {
    const settle = `document.querySelectorAll('.plugin-family').length === 11`;
    const mode = `document.querySelector('select[name="plugin_automatic"]')`;
    const choose = value => `(() => { const control = ${mode}; control.focus(); control.value = '${value}'; control.dispatchEvent(new Event('change', { bubbles: true })); })()`;
    const automaticCalls = `host.calls.filter(([route]) => route === 'package-update/automatic').map(([, body]) => body.mode)`;

    // A host that says how it keeps itself up to date offers the choice, with what each one means.
    await evaluate(`host.pkg = { automatic: { mode: 'off', next_check_unix_ms: null }, restoring: null }; host.open()`);
    await waitFor(settle);
    assert.deepEqual(await evaluate(`Array.from(${mode}.options, option => option.value + ':' + option.textContent)`),
      ['off:Off', 'download:Download, then ask', 'install:Install when idle']);
    assert.equal(await evaluate(`${mode}.value + '|' + ${mode}.disabled`), 'off|false');
    assert.equal(await evaluate(`document.querySelector('label[for="' + ${mode}.id + '"]').textContent`), 'Automatic updates');
    assert.equal(await evaluate(`document.getElementById(${mode}.getAttribute('aria-describedby')).textContent`),
      'Updates are downloaded and installed only when you ask.');

    // Choosing sends the choice, and the host's answer is what the page then shows.
    await evaluate(`host.act.automatic = body => { host.pkg = { ...host.pkg, automatic: { mode: body.mode, next_check_unix_ms: Date.now() + 6 * 3600000 - 30000 } }; };
      ${choose('download')}`);
    await waitFor(`document.querySelector('.plugin-setting-hint').textContent.includes('downloads what it finds')`);
    assert.deepEqual(await evaluate(automaticCalls), ['download']);
    assert.equal(await evaluate(`${mode}.value`), 'download');
    assert.equal(await evaluate(`document.querySelector('.plugin-setting-hint').textContent`),
      'The host looks for updates every six hours and downloads what it finds. You choose when to install. Next check in 6 hours.');
    assert.equal(await evaluate(`document.activeElement === ${mode}`), true, 'the keyboard stays on the setting');
    await evaluate(choose('install'));
    await waitFor(`document.querySelector('.plugin-setting-hint').textContent.includes('installs them once no stream is running')`);
    assert.deepEqual(await evaluate(automaticCalls), ['download', 'install']);
    // Turning it off drops the schedule from the line.
    await evaluate(`host.act.automatic = body => { host.pkg = { ...host.pkg, automatic: { mode: body.mode, next_check_unix_ms: null } }; }; ${choose('off')}`);
    await waitFor(`document.querySelector('.plugin-setting-hint').textContent === 'Updates are downloaded and installed only when you ask.'`);

    // A refusal leaves the setting where the host has it, and says why.
    await evaluate(`host.refuse.automatic = 'permission_denied'; ${choose('install')}`);
    await waitFor(`document.querySelector('.plugin-update-refusal:not([hidden])') !== null`);
    assert.match(await evaluate(`document.querySelector('.plugin-update-refusal').textContent`), /^This account is not allowed to do that\./);
    assert.equal(await evaluate(`${mode}.value`), 'off');
    await evaluate(`host.refuse = {}`);

    // Only an administrator who is signed in may change it; everyone else is told so.
    await evaluate(`host.role = 'token'; host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`${mode}.disabled`), true);
    assert.equal(await evaluate(`document.querySelector('.plugin-setting-hint').textContent`), 'Only an administrator who is signed in to the console can change this.');
    await evaluate(`host.role = 'administrator'`);

    // Earlier versions: closed until asked for, and read only then.
    await evaluate(`host.kept = [
        { digest: host.digest, sequence: '12', state: 'running', installed_unix_ms: Date.UTC(2026, 10, 12, 18, 27), modules: 4, differs: 0, restorable: true, reason: null },
        { digest: 'b'.repeat(64), sequence: '11', state: 'previous', installed_unix_ms: Date.UTC(2026, 10, 11, 18, 46), modules: 4, differs: 1, restorable: true, reason: null, switches_back: true },
        { digest: 'a'.repeat(64), sequence: '10', state: 'retained', installed_unix_ms: null, modules: 3, differs: 4, restorable: false, reason: 'expired' },
      ];
      host.versionsReply = () => ({ ok: true, supported: true, total: 41, versions: host.kept });
      host.versionCalls = () => host.calls.filter(([route]) => route === 'package-update/versions').length;
      host.open()`);
    await waitFor(settle);
    assert.equal(await evaluate(`document.querySelector('.plugin-versions summary').textContent + '|' + document.querySelector('.plugin-versions').open`), 'Earlier versions|false');
    assert.equal(await evaluate(`host.versionCalls()`), 0);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelectorAll('.plugin-versions tbody tr').length === 3`);
    assert.equal(await evaluate(`host.versionCalls()`), 1);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-versions thead th'), cell => cell.textContent)`),
      ['Package', 'Release', 'First installed', 'Compared with now', '']);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-versions tbody tr'), row => [0, 1, 3, 4].map(cell => row.cells[cell].textContent).join('|'))`), [
      'ddddddddddddCopy|12|4 modules|Running now',
      'bbbbbbbbbbbbCopyPrevious|11|1 of 4 modules differs|Switch to this version',
      'aaaaaaaaaaaaCopy|10|4 of 3 modules differ|Its signature has expired',
    ]);
    assert.match(await evaluate(`document.querySelector('.plugin-versions tbody tr').cells[2].textContent`), /2026/);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-versions tbody tr')[2].cells[2].textContent`), '—');
    assert.match(await evaluate(`document.querySelector('.plugin-versions-body > p').textContent`),
      /^The 3 most recent of the 41 packages this host has kept\. The host tries a package before it switches/);
    assert.equal(await evaluate(`host.button('Switch to this version').getAttribute('aria-label')`), 'Switch to package bbbbbbbbbbbb');
    // Every package that can be switched to here can be left again, so nothing warns of a one-way trip.
    assert.equal(await evaluate(`document.querySelector('.plugin-versions-oneway')`), null);

    // Switching asks first, then waits for the stream like an install does.
    await evaluate(`host.press('Switch to this version')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    assert.match(await evaluate(`document.querySelector('dialog.confirmation').textContent`),
      /Switch to package bbbbbbbbbbbb\?.*restarts with this earlier package as soon as no stream is running.*will not bring back the package you are leaving/);
    await evaluate(`host.press('Cancel', document.querySelector('dialog.confirmation'))`);
    await pause(100);
    assert.equal(await evaluate(`host.calls.some(([route]) => route === 'package-update/restore')`), false, 'declining sends nothing');
    await evaluate(`host.act.restore = body => { host.pkg = { ...host.pkg, phase: 'waiting_stream', restoring: { digest: body.digest, sequence: '11' } }; };
      host.act.cancel = () => { host.pkg = { ...host.pkg, phase: 'current', restoring: null }; };
      host.press('Switch to this version')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    await evaluate(`host.press('Switch version', document.querySelector('dialog.confirmation'))`);
    await waitFor(`host.headline() === 'Waiting for the stream to end'`);
    assert.deepEqual(await evaluate(`host.calls.find(([route]) => route === 'package-update/restore')[1]`), { digest: 'b'.repeat(64) });
    assert.equal(await evaluate(`host.summary() + '|' + document.querySelector('.plugin-summary').dataset.tone`), 'Switching to an earlier version|attention');
    assert.match(await evaluate(`document.querySelector('.plugin-update-detail').textContent`), /^The host switches to the earlier package/);
    assert.deepEqual(await evaluate(`host.steps()`), ['Wait for idle:current', 'Restart:upcoming']);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), ['Cancel']);
    // Nothing else can be asked of the host meanwhile.
    assert.equal(await evaluate(`host.button('Switch to this version').disabled && host.button('Check for updates').disabled`), true);
    await evaluate(`host.press('Cancel', document.querySelector('.plugin-update-actions'))`);
    await waitFor(`host.headline() === 'Up to date'`);
    assert.equal(await evaluate(`host.button('Switch to this version').disabled`), false);
    assert.deepEqual(await evaluate(`host.steps()`), []);

    // A refused switch says why and changes nothing.
    await evaluate(`host.refuse.restore = 'not_restorable'; host.press('Switch to this version')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    await evaluate(`host.press('Switch version', document.querySelector('dialog.confirmation'))`);
    await waitFor(`document.querySelector('.plugin-update-refusal:not([hidden])') !== null`);
    assert.match(await evaluate(`document.querySelector('.plugin-update-refusal').textContent`), /^The host can no longer switch to that package\./);
    assert.equal(await evaluate(`host.headline()`), 'Up to date');
    await evaluate(`host.refuse = {}`);

    // The host restarts into the earlier package; the page says which, and reads its list again.
    await evaluate(`host.notices.length = 0;
      host.act.restore = body => { host.pkg = { ...host.pkg, phase: 'activating', restoring: { digest: body.digest, sequence: '11' } }; setTimeout(() => { host.offline = true; }, 0); };
      host.press('Switch to this version')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    await evaluate(`host.press('Switch version', document.querySelector('dialog.confirmation'))`);
    await waitFor(`host.headline() === 'Switching version'`);
    assert.deepEqual(await evaluate(`host.steps()`), ['Wait for idle:done', 'Restart:current']);
    assert.deepEqual(await evaluate(`host.shown('.plugin-update-actions button')`), []);
    const listed = await evaluate(`host.versionCalls()`);
    await evaluate(`host.digest = 'b'.repeat(64);
      host.kept = [{ ...host.kept[1], state: 'running', differs: 0 }, { ...host.kept[0], state: 'previous', differs: 1 }, host.kept[2]];
      host.pkg = { automatic: host.pkg.automatic, restoring: null, phase: 'idle', checked_unix_ms: null }; host.offline = false;`);
    await waitFor(`host.notices.length === 1`);
    assert.deepEqual(await evaluate(`host.notices`), ['Version switched. The host is running package bbbbbbbbbbbb.']);
    await waitFor(`host.versionCalls() === ${listed} + 1 && document.querySelector('.plugin-versions tbody tr')?.cells[0].textContent.startsWith('bbbbbbbbbbbb')`);
    assert.equal(await evaluate(`document.querySelector('.plugin-versions').open`), true, 'the list the reader had open stays open');
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-versions tbody tr'), row => row.cells[4].textContent)`),
      ['Running now', 'Switch to this version', 'Its signature has expired']);
    assert.equal(await evaluate(`host.summary()`), 'Updates have not been checked yet');
    await evaluate(`host.digest = 'd'.repeat(64)`);

    // A long history shows its most recent packages until the rest are asked for.
    await evaluate(`host.kept = Array.from({ length: 12 }, (_, index) => ({
        digest: index === 0 ? host.digest : index.toString(16).repeat(64), sequence: String(40 - index), state: index === 0 ? 'running' : 'retained',
        installed_unix_ms: null, modules: 4, differs: index === 0 ? 0 : 4, restorable: true, reason: null }));
      host.pkg = { automatic: { mode: 'off', next_check_unix_ms: null }, restoring: null }; host.open()`);
    await waitFor(settle);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelectorAll('.plugin-versions tbody tr').length === 6`);
    await evaluate(`host.press('Show all 12', document.querySelector('.plugin-versions'))`);
    assert.equal(await evaluate(`document.querySelectorAll('.plugin-versions tbody tr').length`), 12);

    // A package from before version switching is one way, and says so before and at the choice.
    assert.match(await evaluate(`document.querySelector('.plugin-versions-oneway').textContent`), /^Packages from before version switching are one way/);
    await evaluate(`host.press('Switch to this version')`);
    await waitFor(`document.querySelector('dialog.confirmation')?.open === true`);
    assert.match(await evaluate(`document.querySelector('dialog.confirmation').textContent`),
      /This package is from before version switching: once it is running, this console can no longer switch packages/);
    assert.doesNotMatch(await evaluate(`document.querySelector('dialog.confirmation').textContent`), /will not bring back/);
    await evaluate(`host.press('Cancel', document.querySelector('dialog.confirmation'))`);

    // A host that has only just started is still reading its packages, and is asked again.
    await evaluate(`host.reading = 2; host.listedPackages = host.versionsReply;
      host.versionsReply = () => host.reading-- > 0 ? { ok: true, supported: true, loading: true, total: 0, versions: [] } : host.listedPackages();
      host.open()`);
    await waitFor(settle);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelector('.plugin-versions-body').textContent === 'Loading earlier versions…'`);
    await waitFor(`document.querySelectorAll('.plugin-versions tbody tr').length === 6`, 'the list arrives once the host has read it');
    assert.equal(await evaluate(`host.versionCalls()`), 3);
    await evaluate(`host.versionsReply = host.listedPackages`);

    // A viewer sees the list and is told who may switch.
    await evaluate(`host.role = 'viewer'; host.scopes = ['configuration.read']; host.open()`);
    await waitFor(settle);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelectorAll('.plugin-versions tbody tr').length === 6`);
    assert.deepEqual(await evaluate(`Array.from(document.querySelectorAll('.plugin-versions tbody button')).filter(control => control.textContent === 'Switch to this version').map(control => control.disabled)`),
      [true, true, true, true, true]);
    assert.match(await evaluate(`document.querySelector('.plugin-versions-body > p').textContent`), /Switching takes an administrator who is signed in to the console\.$/);
    await evaluate(`host.role = 'administrator'; host.scopes = null`);

    // A host that kept nothing earlier, and a list that cannot be read, each say so.
    await evaluate(`host.kept = host.kept.slice(0, 1); host.open()`);
    await waitFor(settle);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelector('.plugin-versions-body').textContent === 'The host has not kept an earlier package to switch to.'`);
    await evaluate(`host.versionsReply = () => ({ ok: true, supported: true, total: 1, versions: 'none' }); host.open()`);
    await waitFor(settle);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelector('.plugin-versions-body .plugin-update-refusal') !== null`);
    assert.equal(await evaluate(`document.querySelector('.plugin-versions-body .plugin-update-refusal').textContent`), 'Package version list is invalid.');
    await evaluate(`host.versionsReply = () => ({ ok: true, supported: true, total: 1, versions: host.kept }); host.press('Try again', document.querySelector('.plugin-versions'))`);
    await waitFor(`document.querySelector('.plugin-versions-body').textContent === 'The host has not kept an earlier package to switch to.'`);

    // The setting and the list fit a narrow screen.
    await viewport(380, 800);
    await evaluate(`host.kept = Array.from({ length: 3 }, (_, index) => ({
        digest: index === 0 ? host.digest : index.toString(16).repeat(64), sequence: String(40 - index), state: index === 0 ? 'running' : 'retained',
        installed_unix_ms: Date.UTC(2026, 10, 12), modules: 4, differs: index === 0 ? 0 : 4, restorable: true, reason: null }));
      host.open()`);
    await waitFor(settle);
    await evaluate(`document.querySelector('.plugin-versions').open = true`);
    await waitFor(`document.querySelectorAll('.plugin-versions tbody tr').length === 3`);
    assert.equal(await evaluate(`document.documentElement.scrollWidth <= innerWidth`), true);
    assert.equal(await evaluate(`${mode}.getBoundingClientRect().right <= innerWidth`), true);
  });
});
