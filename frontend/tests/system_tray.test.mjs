import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.listeners = new Map();
    this.isConnected = true;
  }

  append(...children) {
    this.children.push(...children.filter((child) => child != null));
  }

  replaceChildren(...children) {
    this.children = [];
    this.append(...children);
  }

  addEventListener(event, listener) {
    this.listeners.set(event, listener);
  }

  setAttribute() {}

  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join("");
  }
}

function descendants(element) {
  return [element, ...element.children.flatMap((child) => child instanceof FakeElement ? descendants(child) : [])];
}

const context = vm.createContext({
  document: { createElement: (tag) => new FakeElement(tag) },
  console,
});
for (const source of ["src/ui.ts", "src/system_tray.ts"]) {
  vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
  if (source === "src/ui.ts") vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
}
const tray = context.Amaterasu;
const ordinary = (value) => JSON.parse(JSON.stringify(value));

test("system tray defaults enabled and rejects malformed settings", () => {
  assert.deepEqual(ordinary(tray.defaultSystemTraySettings()), { system_tray: true });
  assert.deepEqual(ordinary(tray.parseSystemTraySettings({ system_tray: false })), { system_tray: false });
  for (const settings of [{}, { system_tray: "true" }, { system_tray: true, extra: false }])
    assert.throws(() => tray.parseSystemTraySettings(settings));
  assert.throws(() => tray.parseSystemTraySettingsReply({
    ok: true, settings: { system_tray: true }, revision: -1,
  }));
});

test("system tray editor saves and reads back the revisioned default-enabled setting", async () => {
  const calls = [];
  let stored = { ok: true, settings: { system_tray: true }, revision: 3, availability: "available" };
  const container = new FakeElement("div");
  tray.renderSystemTraySettings({
    api: {
      allows: (scope) => scope === "configuration.write",
      async post(route, body = {}) {
        calls.push([route, body]);
        if (route === "system-tray/settings") return stored;
        assert.equal(route, "system-tray/settings_update");
        assert.equal(body.expected_revision, 3);
        stored = { ok: true, settings: body.settings, revision: 4, availability: "disabled" };
        return stored;
      },
    },
    notify() {},
  }, container);
  await new Promise((resolve) => setImmediate(resolve));
  const checkbox = descendants(container).find((element) => element.name === "system_tray");
  assert.equal(checkbox.checked, true);
  assert.match(container.textContent, /Saved settings revision 3/);
  checkbox.checked = false;
  const form = descendants(container).find((element) => element.tagName === "form");
  form.listeners.get("submit")({ preventDefault() {} });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls.map(([route]) => route), [
    "system-tray/settings",
    "system-tray/settings_update",
    "system-tray/settings",
    "system-tray/settings",
  ]);
  assert.deepEqual(ordinary(calls[1][1].settings), { system_tray: false });
  assert.match(container.textContent, /Saved settings revision 4/);
});
