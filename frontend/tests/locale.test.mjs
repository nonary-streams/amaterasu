import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({});
vm.runInContext(
  ts.transpile(readFileSync("src/locale.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
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
