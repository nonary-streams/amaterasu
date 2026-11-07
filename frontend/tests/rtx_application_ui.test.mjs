import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../../modules/rtx-hdr/ui/application.js', import.meta.url), 'utf8');
const descriptor = JSON.parse(readFileSync(new URL('../../modules/rtx-hdr/metadata/linux.json', import.meta.url), 'utf8')).descriptor;
const settle = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };

async function fixture({ deferred = false, fail = false, canWrite = true, mode = 'application', hostValues = {}, initialValues = {} } = {}) {
  let registration, revision = 7, values = structuredClone(initialValues), timerId = 0;
  const globalValues = { ...hostValues, rtx_hdr_scope: mode };
  const timers = new Map(), controls = [], buttons = [], updates = [], pending = [];
  const controller = new AbortController();
  const nodes = [];
  const element = (tag) => {
    const node = { tag, attributes: {}, children: [], append(...children) { this.children.push(...children); },
      replaceChildren(...children) { this.children = children; }, setAttribute(name, value) { this.attributes[name] = value; } };
    nodes.push(node);
    return node;
  };
  const document = { createElement: element };
  const context = vm.createContext({ document, window: { AmaterasuExtensions: { v1: { register(id, value) { assert.equal(id, 'rtx-hdr'); registration = value; } } } },
    setTimeout(callback) { timers.set(++timerId, callback); return timerId; }, clearTimeout(id) { timers.delete(id); } });
  vm.runInContext(source, context);
  let dirty = false, busy = false;
  const make = (options) => {
    let value = options.value;
    const control = { options, element: element(), disposed: false, value: () => value,
      setValue(next) { value = next; }, dispose() { this.disposed = true; },
      edit(next) { value = next; options.onChange(next); } };
    controls.push(control); return control;
  };
  const settings = { provider: 'amaterasu.rtx-hdr', canWrite,
    async read() { return { revision, values: structuredClone(values), effective: Object.fromEntries(descriptor.options.map(option => [option.id, { value: values[option.id] ?? globalValues[option.id] ?? option.definition.default, source: null }])) }; },
    async update(expected, next) {
      updates.push({ expected, values: structuredClone(next) });
      if (deferred) await new Promise(resolve => pending.push(resolve));
      if (fail) throw new Error('Save rejected');
      assert.equal(expected, revision);
      values = structuredClone(next); return { revision: ++revision };
    } };
  const sdk = { features: ['ui.application-settings/1'], settings, signal: controller.signal,
    setDirty(value) { dirty = value; }, setBusy(value) { busy = value; },
    ui: { toggle: make, slider: make, button(options) { const button = { options, element: element(), dispose() {} }; buttons.push(button); return button; } } };
  const container = element();
  registration.mountApplication(container, sdk);
  await settle();
  return { controls, buttons, updates, pending, container, controller, timers, nodes,
    dirty: () => dirty, busy: () => busy, values: () => values,
    async flush() { const callbacks = [...timers.values()]; timers.clear(); callbacks.forEach(callback => callback()); await settle(); } };
}

for (const mode of ['application', 'global']) {
  test(`RTX shows only the ${mode} enable control and saves to its setting`, async () => {
    const env = await fixture({ mode });
    const activeId = mode === 'application' ? 'rtx_hdr_application_enable' : 'rtx_hdr_enable';
    const inactiveId = mode === 'application' ? 'rtx_hdr_enable' : 'rtx_hdr_application_enable';
    assert.equal(env.nodes.some(node => node.attributes['data-setting-id'] === activeId), true);
    assert.equal(env.nodes.some(node => node.attributes['data-setting-id'] === inactiveId), false);
    assert.equal(env.controls.filter(control => control.options.label === 'Enable RTX HDR').length, 1);
    for (const option of descriptor.options.filter(option => option.allowed_scopes.includes('application') && option.id !== inactiveId)) {
      const control = env.controls.find(control => control.options.label === (option.id === activeId ? 'Enable RTX HDR' : option.label));
      assert.ok(control, option.id);
      if (option.definition.type === 'integer') {
        assert.equal(control.options.minimum, option.definition.minimum);
        assert.equal(control.options.maximum, option.definition.maximum);
      }
    }
    assert.equal(env.controls.length, 9, 'no separate inheritance controls');
    assert.equal(env.updates.length, 0);
    const enable = env.controls.find(control => control.options.label === 'Enable RTX HDR');
    enable.edit(true); await env.flush();
    enable.edit(false); await env.flush();
    assert.deepEqual(env.updates, [
      { expected: 7, values: { [activeId]: true } },
      { expected: 8, values: { [activeId]: false } },
    ]);
    assert.equal(env.dirty(), false);
  });
}

test('untouched RTX settings use global values and only edits create overrides', async () => {
  const env = await fixture({ hostValues: { rtx_hdr_peak_brightness: 1600, rtx_hdr_contrast: 15 },
    initialValues: { rtx_hdr_enable: false, rtx_hdr_debanding: false } });
  const peak = env.controls.find(control => control.options.label === 'Peak brightness (nits)');
  const contrast = env.controls.find(control => control.options.label === 'Contrast');
  assert.equal(peak.value(), 1600);
  assert.equal(contrast.value(), 15);
  assert.equal(env.updates.length, 0);
  assert.equal(env.dirty(), false);
  peak.edit(1400);
  assert.equal(env.dirty(), true);
  await env.flush();
  assert.deepEqual(env.values(), { rtx_hdr_enable: false, rtx_hdr_debanding: false, rtx_hdr_peak_brightness: 1400 });
  assert.equal(contrast.value(), 15);
  assert.equal(env.dirty(), false);
});

test('extra controls live inside a closed Advanced section', async () => {
  const env = await fixture();
  const advanced = env.nodes.find(node => node.tag === 'details');
  assert.ok(advanced);
  assert.notEqual(advanced.open, true);
  assert.equal(advanced.children[0].textContent, 'Advanced');
  assert.deepEqual(advanced.children[1].children.map(row => row.attributes['data-setting-id']),
    ['rtx_hdr_fullscreen_only', 'rtx_hdr_foreground_only', 'rtx_hdr_sdr_brightness', 'rtx_hdr_debanding']);
});

test('edits made during an RTX save are serialized with the next revision', async () => {
  const env = await fixture({ deferred: true });
  const peak = env.controls.find(control => control.options.label === 'Peak brightness (nits)');
  peak.edit(1400); await env.flush();
  assert.equal(env.busy(), true);
  peak.edit(1500);
  env.pending.shift()(); await settle();
  assert.equal(env.dirty(), true);
  await env.flush();
  assert.equal(env.updates[1].expected, 8);
  env.pending.shift()(); await settle();
  assert.equal(env.values().rtx_hdr_peak_brightness, 1500);
  assert.equal(env.dirty(), false);
});

test('unknown host modes fail without guessing an enable setting', async () => {
  const env = await fixture({ mode: 'unknown' });
  assert.equal(env.controls.length, 0);
  assert.equal(env.updates.length, 0);
  assert.ok(env.nodes.some(node => node.textContent?.includes('Invalid RTX HDR settings response')));
});

test('read-only RTX controls cannot create application overrides', async () => {
  const env = await fixture({ canWrite: false });
  assert.equal(env.controls.every(control => control.options.disabled), true);
  env.controls.find(control => control.options.label === 'Contrast').edit(10);
  await env.flush();
  assert.equal(env.updates.length, 0);
  assert.equal(env.dirty(), false);
});

test('failed RTX saves retain drafts and closing cancels queued saves', async () => {
  const failed = await fixture({ fail: true });
  failed.controls.find(control => control.options.label === 'Contrast').edit(10);
  await failed.flush();
  assert.equal(failed.dirty(), true);
  assert.equal(failed.busy(), false);
  const closing = await fixture();
  closing.controls.find(control => control.options.label === 'Contrast').edit(10);
  closing.controller.abort(); await closing.flush();
  assert.equal(closing.updates.length, 0);
  assert.equal(closing.controls.every(control => control.disposed), true);
});
