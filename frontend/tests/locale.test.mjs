import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({});
const localeSource = ts.transpile(
  `${readFileSync("src/locale-catalogs.ts", "utf8")}\n${readFileSync("src/locale.ts", "utf8")}`,
  {
    target: ts.ScriptTarget.ES2022,
  },
);
const uiSource = ts.transpile(readFileSync("src/ui.ts", "utf8"), {
  target: ts.ScriptTarget.ES2022,
});
vm.runInContext(localeSource, context);
const locale = context.Amaterasu;
const ordinary = (value) => JSON.parse(JSON.stringify(value));
const expectedLocales = [
  "bg",
  "cs",
  "de",
  "en",
  "en_GB",
  "en_US",
  "es",
  "fr",
  "hu",
  "it",
  "ja",
  "ko",
  "pl",
  "pt",
  "pt_BR",
  "ru",
  "sv",
  "tr",
  "uk",
  "vi",
  "zh",
  "zh_TW",
];

function configuration(values = {}) {
  return {
    revision: 12,
    fields: {
      locale: {
        type: "enum",
        default: "en",
        options: expectedLocales,
      },
    },
    values,
  };
}

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName;
    this.children = [];
    this.attributes = new Map();
    this.listeners = new Map();
    this.className = "";
    this.textContent = "";
    this.value = "";
    this.disabled = false;
    this.id = "";
    this.htmlFor = "";
  }

  append(...children) {
    this.children.push(...children);
  }

  setAttribute(name, value) {
    this.attributes.set(name, value);
  }

  addEventListener(name, listener) {
    const listeners = this.listeners.get(name) ?? [];
    listeners.push(listener);
    this.listeners.set(name, listeners);
  }

  dispatch(name) {
    for (const listener of this.listeners.get(name) ?? [])
      listener({ target: this });
  }

  findById(id) {
    if (this.id === id) return this;
    for (const child of this.children) {
      const match = child.findById?.(id);
      if (match) return match;
    }
    return null;
  }

  findByClass(className) {
    if (this.className === className) return this;
    for (const child of this.children) {
      const match = child.findByClass?.(className);
      if (match) return match;
    }
    return null;
  }
}

class FakeTextNode {
  constructor(text) {
    this.nodeType = 3;
    this.nodeValue = text;
  }

  get textContent() {
    return this.nodeValue;
  }

  set textContent(value) {
    this.nodeValue = value;
  }
}

function mountLocaleSelector(api) {
  const document = {
    documentElement: { lang: "en" },
    createElement: (tagName) => new FakeElement(tagName),
  };
  const selectorContext = vm.createContext({ document });
  vm.runInContext(localeSource, selectorContext);
  const container = selectorContext.Amaterasu.mountLocaleSelector(api);
  return { container, document };
}

async function flushMicrotasks() {
  await new Promise((resolve) => setImmediate(resolve));
}

test("locale validation accepts every exact supported token", () => {
  assert.deepEqual(ordinary(locale.LOCALE_OPTIONS), expectedLocales);
  for (const token of expectedLocales) {
    assert.equal(locale.validateLocale(token), token);
    assert.equal(
      locale.localeFromConfiguration(configuration({ locale: token })),
      token,
    );
  }
});

test("locale catalogs translate consumed console text and fall back to English", () => {
  for (const token of expectedLocales) {
    const messages = locale.localeMessagesFor(token);
    assert.equal(typeof messages.Overview, "string");
    assert.equal(typeof messages["Sign out"], "string");
    assert.equal(typeof messages["Toggle navigation"], "string");
    assert.equal(
      locale.translateLocaleMessage("A page-specific sentence", messages),
      "A page-specific sentence",
    );
  }

  const german = locale.localeMessagesFor("de");
  assert.equal(locale.translateLocaleMessage("Overview", german), "Übersicht");
  assert.equal(locale.translateLocaleMessage("Sign out", german), "Abmelden");
  assert.equal(
    locale.translateLocaleMessage("Streaming features are in progress.", german),
    "Streamingfunktionen sind in Arbeit.",
  );
  assert.equal(
    locale.translateLocaleMessage("Overview", locale.localeMessagesFor("en_GB")),
    "Overview",
  );
});

test("locale UI overlays merge over base messages without losing untranslated entries", () => {
  const merged = locale.mergeLocaleMessages(
    {
      labels: { save: "Save", cancel: "Cancel" },
      shared: "Base value",
    },
    {
      labels: { save: "Guardar" },
      shared: "Overlay value",
    },
  );

  assert.deepEqual(ordinary(merged), {
    labels: { save: "Guardar", cancel: "Cancel" },
    shared: "Overlay value",
  });
});

test("locale changes translate existing UI text nodes and the browser title", () => {
  const root = new FakeElement("html");
  const document = {
    documentElement: root,
    title: "Overview · Amaterasu",
    createElement: (tagName) => new FakeElement(tagName),
    createTextNode: (text) => new FakeTextNode(text),
    createTreeWalker(node) {
      const textNodes = [];
      const collect = (candidate) => {
        if (candidate instanceof FakeTextNode) {
          textNodes.push(candidate);
          return;
        }
        for (const child of candidate.children ?? []) collect(child);
      };
      collect(node);
      let index = 0;
      return { nextNode: () => textNodes[index++] ?? null };
    },
  };
  const runtimeContext = vm.createContext({
    document,
    NodeFilter: { SHOW_TEXT: 4 },
  });
  vm.runInContext(localeSource, runtimeContext);
  vm.runInContext(uiSource, runtimeContext);
  runtimeContext.Amaterasu.setLocalizedPageTitle("Overview");
  const heading = runtimeContext.Amaterasu.el("h1", "", "Overview");
  const date = new Date(Date.UTC(2024, 0, 2, 15, 4));
  const localizedDate = runtimeContext.Amaterasu.localeFormattedTextNode(
    (locale) => date.toLocaleString(locale),
  );
  const dateLabel = new FakeElement("time");
  dateLabel.append(localizedDate);
  root.append(heading);
  root.append(dateLabel);
  const textNode = heading.children[0];
  const dateTextNode = dateLabel.children[0];

  assert.equal(textNode.nodeValue, "Overview");
  assert.equal(dateTextNode.nodeValue, date.toLocaleString("en"));
  runtimeContext.Amaterasu.applyLocale("de");
  assert.equal(textNode.nodeValue, "Übersicht");
  assert.equal(dateTextNode.nodeValue, date.toLocaleString("de"));
  assert.equal(document.documentElement.lang, "de");
  assert.equal(document.title, "Übersicht · Amaterasu");

  runtimeContext.Amaterasu.applyLocale("en");
  assert.equal(textNode.nodeValue, "Overview");
  assert.equal(dateTextNode.nodeValue, date.toLocaleString("en"));
  assert.equal(document.title, "Overview · Amaterasu");
});

test("locale validation rejects unsupported and normalized spellings", () => {
  for (const value of [
    "en-GB",
    "EN",
    "en_gb",
    "pt-BR",
    "zh-TW",
    "en ",
    "",
    null,
    1,
  ]) {
    assert.throws(() => locale.validateLocale(value));
  }
  assert.throws(() =>
    locale.localeFromConfiguration(configuration({ locale: "en-GB" })),
  );
});

test("locale readback uses the en default and validates the registered definition", () => {
  assert.equal(locale.localeFromConfiguration(configuration()), "en");
  assert.equal(
    locale.localeFromConfiguration(configuration({ locale: "en_GB" })),
    "en_GB",
  );

  for (const definition of [
    { type: "string", default: "en", options: expectedLocales },
    { type: "enum", default: "en_GB", options: expectedLocales },
    { type: "enum", default: "en", options: expectedLocales.slice(1) },
    { type: "enum", default: "en", options: [...expectedLocales].reverse() },
  ]) {
    assert.throws(() =>
      locale.localeFromConfiguration({
        ...configuration(),
        fields: { locale: definition },
      }),
    );
  }
});

test("document language maps underscores without changing stored locale tokens", () => {
  for (const [token, documentLanguage] of [
    ["en_GB", "en-GB"],
    ["en_US", "en-US"],
    ["pt_BR", "pt-BR"],
    ["zh_TW", "zh-TW"],
  ]) {
    assert.equal(locale.validateLocale(token), token);
    assert.equal(locale.documentLanguage(token), documentLanguage);
  }
});

test("locale migration adds its field while preserving unrelated definitions and values", () => {
  const existingDefinition = {
    type: "string",
    default: "Host",
    min_length: 1,
  };
  const existingValue = "Saved host name";
  const original = {
    revision: 73,
    version: 1,
    fields: {
      host_name: existingDefinition,
      logging: { type: "boolean", default: true },
    },
    values: { host_name: existingValue, logging: false },
  };
  const before = ordinary(original);
  const extended = locale.extendLocaleConfiguration(original);

  assert.equal(extended.revision, 73);
  assert.strictEqual(extended.fields.host_name, existingDefinition);
  assert.strictEqual(extended.values.host_name, existingValue);
  assert.deepEqual(ordinary(extended.fields), {
    ...before.fields,
    locale: {
      type: "enum",
      default: "en",
      options: expectedLocales,
    },
  });
  assert.deepEqual(ordinary(extended.values), before.values);
  assert.deepEqual(ordinary(original), before);
  assert.throws(() => locale.extendLocaleConfiguration(configuration()));
});

test("locale selector registers a missing caller configuration and reads it back", async () => {
  const calls = [];
  let registered = false;
  const api = {
    allows: (scope) =>
      ["configuration.read", "configuration.write"].includes(scope),
    async post(path, payload = {}) {
      calls.push({ path, payload: ordinary(payload) });
      if (path === "configuration/get") {
        if (!registered) {
          throw Object.assign(new Error("not registered"), {
            code: "not_registered",
          });
        }
        return configuration();
      }
      if (path === "configuration/register") {
        registered = true;
        return { ok: true, revision: 1 };
      }
      throw new Error(`Unexpected configuration route: ${path}`);
    },
  };

  const { container, document } = mountLocaleSelector(api);
  await flushMicrotasks();

  assert.deepEqual(calls.map(({ path }) => path), [
    "configuration/get",
    "configuration/register",
    "configuration/get",
  ]);
  assert.deepEqual(calls[1].payload, {
    fields: {
      locale: { type: "enum", default: "en", options: expectedLocales },
    },
  });
  assert.equal(container.findById("global-locale").value, "en");
  assert.equal(container.findById("global-locale").disabled, false);
  assert.equal(document.documentElement.lang, "en");
});

test("locale selector migrates the existing schema without replacing unrelated values", async () => {
  const calls = [];
  let serverDocument = {
    revision: 7,
    fields: {
      host_name: { type: "string", default: "Host", min_length: 1 },
      logging: { type: "boolean", default: true },
    },
    values: { host_name: "Saved host", logging: false },
  };
  const api = {
    allows: () => true,
    async post(path, payload = {}) {
      calls.push({ path, payload: ordinary(payload) });
      if (path === "configuration/get") return ordinary(serverDocument);
      if (path === "configuration/migrate") {
        assert.equal(payload.expected_revision, 7);
        assert.equal(Object.hasOwn(payload, "values"), false);
        assert.equal(Object.hasOwn(payload, "renames"), false);
        assert.equal(Object.hasOwn(payload, "removed_fields"), false);
        serverDocument = {
          revision: 8,
          fields: payload.fields,
          values: { ...serverDocument.values, locale: "en" },
        };
        return { ok: true, revision: 8 };
      }
      throw new Error(`Unexpected configuration route: ${path}`);
    },
  };

  const { container, document } = mountLocaleSelector(api);
  await flushMicrotasks();

  assert.deepEqual(calls.map(({ path }) => path), [
    "configuration/get",
    "configuration/migrate",
    "configuration/get",
  ]);
  assert.deepEqual(ordinary(serverDocument.fields.host_name), {
    type: "string",
    default: "Host",
    min_length: 1,
  });
  assert.deepEqual(serverDocument.values, {
    host_name: "Saved host",
    logging: false,
    locale: "en",
  });
  assert.equal(container.findById("global-locale").value, "en");
  assert.equal(document.documentElement.lang, "en");
});

test("locale selector saves with the loaded revision and applies authoritative readback", async () => {
  const calls = [];
  let serverDocument = configuration({ locale: "en" });
  let updateSucceeded = false;
  const api = {
    allows: () => true,
    async post(path, payload = {}) {
      calls.push({ path, payload: ordinary(payload) });
      if (path === "configuration/get") {
        if (updateSucceeded) {
          serverDocument = {
            ...serverDocument,
            revision: 14,
            values: { ...serverDocument.values, locale: "en_GB" },
          };
          updateSucceeded = false;
        }
        return ordinary(serverDocument);
      }
      if (path === "configuration/update") {
        assert.deepEqual(ordinary(payload), {
          values: { locale: "pt_BR" },
          expected_revision: 12,
        });
        serverDocument = {
          ...serverDocument,
          revision: 13,
          values: { ...serverDocument.values, ...payload.values },
        };
        updateSucceeded = true;
        return { ok: true, revision: 13 };
      }
      throw new Error(`Unexpected configuration route: ${path}`);
    },
  };

  const { container, document } = mountLocaleSelector(api);
  await flushMicrotasks();
  const select = container.findById("global-locale");
  select.value = "pt_BR";
  select.dispatch("change");
  await flushMicrotasks();

  assert.deepEqual(calls.map(({ path }) => path), [
    "configuration/get",
    "configuration/update",
    "configuration/get",
  ]);
  assert.equal(calls[1].payload.expected_revision, 12);
  assert.deepEqual(calls[1].payload.values, { locale: "pt_BR" });
  assert.equal(select.value, "en_GB");
  assert.equal(document.documentElement.lang, "en-GB");
  assert.equal(serverDocument.revision, 14);
});

test("locale selector keeps service and missing-scope failures nonfatal", async () => {
  const unavailableApi = {
    allows: (scope) => scope === "configuration.read",
    async post() {
      throw new Error("service unavailable");
    },
  };
  const unavailable = mountLocaleSelector(unavailableApi);
  await flushMicrotasks();
  assert.ok(unavailable.container.findById("global-locale"));
  assert.match(
    unavailable.container.findByClass("locale-error").textContent,
    /service unavailable/,
  );

  let calls = 0;
  const noReadScopeApi = {
    allows: () => false,
    async post() {
      calls += 1;
      throw new Error("must not make an unsigned/unscoped request");
    },
  };
  const readOnly = mountLocaleSelector(noReadScopeApi);
  await flushMicrotasks();
  assert.equal(calls, 0);
  assert.equal(readOnly.container.findById("global-locale").disabled, true);
  assert.match(
    readOnly.container.findByClass("locale-error").textContent,
    /configuration.read/,
  );
});
