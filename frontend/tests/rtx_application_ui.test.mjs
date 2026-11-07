import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../../modules/rtx-hdr/ui/application.js', import.meta.url), 'utf8');
const descriptor = JSON.parse(readFileSync(new URL('../../modules/rtx-hdr/metadata/linux.json', import.meta.url), 'utf8')).descriptor;
const settle = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };

async function fixture({ deferred = false, fail = false, canWrite = true } = {}) {
  let registration, revision = 7, values = {}, timerId = 0;
  const timers = new Map(), controls = [], buttons = [], updates = [], pending = [];
  const controller = new AbortController();
  const element = () => ({ children: [], append(...nodes) { this.children.push(...nodes); }, replaceChildren(...nodes) { this.children = nodes; }, setAttribute() {} });
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
    async read() { return { revision, values: structuredClone(values), effective: Object.fromEntries(descriptor.options.map(option => [option.id, { value: values[option.id] ?? option.definition.default, source: null }])) }; },
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
  return { controls, buttons, updates, pending, container, controller, timers,
    dirty: () => dirty, busy: () => busy, values: () => values,
    async flush() { const callbacks = [...timers.values()]; timers.clear(); callbacks.forEach(callback => callback()); await settle(); } };
}

test('RTX plugin owns controls matching its native descriptor and saves slider edits', async () => {
  const env = await fixture();
  for (const option of descriptor.options.filter(option => option.allowed_scopes.includes('application'))) {
    const label = option.id === 'rtx_hdr_enable' ? 'Override RTX HDR in global mode' : option.label;
    const control = env.controls.find(control => control.options.label === label);
    assert.ok(control, option.id);
    if (option.definition.type === 'integer') {
      assert.equal(control.options.minimum, option.definition.minimum);
      assert.equal(control.options.maximum, option.definition.maximum);
    }
  }
  assert.equal(env.updates.length, 0);
  env.controls.find(control => control.options.label === 'Peak brightness (nits)').edit(1400);
  assert.equal(env.dirty(), true);
  await env.flush();
  assert.deepEqual(env.updates, [{ expected: 7, values: { rtx_hdr_peak_brightness: 1400 } }]);
  assert.equal(env.dirty(), false);
  assert.equal(env.busy(), false);
});

test('an application can override the global RTX enable setting', async () => {
  const env = await fixture();
  env.controls.find(control => control.options.label === 'Override RTX HDR in global mode').edit(false);
  await env.flush();
  assert.deepEqual(env.values(), { rtx_hdr_enable: false });
  env.controls.find(control => control.options.label === 'Use inherited override rtx hdr in global mode').edit(true);
  await env.flush();
  assert.deepEqual(env.values(), {});
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

test('RTX enable toggle saves both directions through the same scoped settings API', async () => {
  const env = await fixture();
  const enable = env.controls.find(control => control.options.label === 'Enable RTX HDR for this application');
  assert.equal(descriptor.options.find(option => option.id === 'rtx_hdr_application_enable').apply_policy, 'next_stream');
  enable.edit(true);
  await env.flush();
  enable.edit(false);
  await env.flush();
  assert.deepEqual(env.updates, [
    { expected: 7, values: { rtx_hdr_application_enable: true } },
    { expected: 8, values: { rtx_hdr_application_enable: false } },
  ]);
  assert.equal(env.dirty(), false);
});

test('RTX inheritance removes only the application value', async () => {
  const env = await fixture();
  env.controls.find(control => control.options.label === 'Contrast').edit(10);
  await env.flush();
  env.controls.find(control => control.options.label === 'Use inherited contrast').edit(true);
  await env.flush();
  assert.deepEqual(env.values(), {});
  assert.equal(env.controls.find(control => control.options.label === 'Contrast').value(), 0);
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
