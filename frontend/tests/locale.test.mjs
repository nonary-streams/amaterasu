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

  replaceChildren(...children) {
    this.children = [...children];
    this.textContent = children
      .map((child) =>
        typeof child === "string"
          ? child
          : child?.nodeValue ?? child?.textContent ?? "",
      )
      .join("");
  }

  setAttribute(name, value) {
    this.attributes.set(name, value);
  }

  getAttribute(name) {
    return this.attributes.has(name) ? this.attributes.get(name) : null;
  }

  querySelectorAll(selector) {
    const match = selector.match(/^\[([^\]]+)\]$/);
    if (!match) return [];
    const attribute = match[1];
    const matches = [];
    const visit = (element) => {
      if (element.attributes?.has(attribute)) matches.push(element);
      for (const child of element.children ?? []) child.querySelectorAll?.(selector).forEach((nested) => {
        if (!matches.includes(nested)) matches.push(nested);
      });
    };
    visit(this);
    return matches;
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

test("product, catalog, settings, updater, and notice messages cover every translated locale", () => {
  const requiredMessages = [
    "Host overview",
    "Curate your library and manage the services that keep this host running.",
    "Streaming is still being built",
    "Client pairing, capture, and stream connections are not available in this release. The management tools shown here are live.",
    "Your library",
    "Edit application",
    "Add application",
    "Delete entry",
    "Frame limiter",
    "Copy rendered log preview",
    "Copies every rendered line, including search context. Log content is raw and may contain identifying details.",
    "Rendered log preview copied to the clipboard.",
    "Could not copy the rendered log preview. Clipboard access may be unavailable in this browser context.",
    "Keyboard input",
    "Mouse input",
    "Enable Lutris discovery",
    "Enable Playnite synchronization",
    "Show the system tray icon and desktop notifications",
    "Sampling and dashboard interval (ms)",
    "Maximum retained samples",
    "A durable catalog of the applications on your host.",
    "Discover local Steam games and optionally sync a complete scan into the catalog.",
    "Edit host options and values registered for this management console.",
    "Inspect and stop sessions owned by this management console.",
    "Processes launched through this management console.",
    "Records and counters for this management console.",
    "Applications launched through the management API appear here. Catalog entries are metadata and do not launch automatically.",
    "Completed streams appear here when history recording is enabled.",
    "Sessions created through the management API appear here. Streaming clients and pairing are not available yet.",
    "Records appear when a service explicitly submits them. Automatic host log collection is not enabled.",
    "Register a schema for the management principal through the authenticated configuration API. This screen will then show its editable values.",
    "The registered schema is empty. Schema setup and migration are available through the management API.",
    "Controls automatic scans of the configured Steam library roots.",
    "Collect measured host metrics for the live statistics dashboard and while an active stream requests samples.",
    "The default is 86400 seconds (24 hours). Set 0 to disable periodic checks; a manual check is still available.",
    "Recording changes wait for active streams to end and pending launches to resolve. Check the runtime status for the effective policy.",
    "Stable updates are always considered. Pre-releases are included when enabled or when this build is already on a pre-release channel.",
    "Use a literal IPv4 or IPv6 address. An explicit blank uses the startup policy's wildcard/loopback behavior.",
    'A JSON array of literal arguments. Example: ["--fullscreen"]',
    "Separate tags with commas.",
    "{field} override for {client}",
    "{field} override",
    "{label} over recent measured samples",
    "DS4 Back as touchpad click",
    "Motion selects DS4",
    "Touchpad selects DS4",
    "DS5 random MAC",
    "Proton DualSense compatibility",
    "CPU utilization",
    "GPU utilization",
    "Memory use",
    "Network receive",
    "Your host, your library",
    "A home for your applications.",
    "Active streaming sessions",
    "Input policy",
    "Authentication session lifetimes",
    "Real-time statistics",
    "Software updates",
    "System tray",
    "No active streams",
    "No paired clients",
    "Search paired clients",
    "Sort by",
    "Recently paired",
    "No paired clients match this search.",
    "Showing {visible} of {total} paired client",
    "Showing {visible} of {total} paired clients",
    "No diagnostic records",
    "Search names, descriptions, tags",
    "Absolute application path",
    "Save input policy",
    "Refresh host metrics",
    "Check for updates now",
    "Application removed from the catalog.",
    "Session updated.",
    "Core console labels and selected notices are translated; some descriptions remain English.",
  ];
  for (const token of expectedLocales.filter((value) => !value.startsWith("en"))) {
    const messages = locale.localeMessagesFor(token);
    for (const source of requiredMessages) {
      assert.equal(typeof messages[source], "string", `${token}: ${source}`);
      assert.notEqual(messages[source], source, `${token}: ${source}`);
    }
  }
  for (const token of ["en", "en_GB", "en_US"]) {
    const messages = locale.localeMessagesFor(token);
    for (const source of requiredMessages) assert.equal(messages[source], source);
  }
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
    querySelectorAll: (selector) => root.querySelectorAll(selector),
  };
  const runtimeContext = vm.createContext({
    document,
    NodeFilter: { SHOW_TEXT: 4 },
  });
  vm.runInContext(localeSource, runtimeContext);
  vm.runInContext(uiSource, runtimeContext);
  vm.runInContext(
    ts.transpile(readFileSync("src/client_directory.ts", "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    runtimeContext,
  );
  runtimeContext.Amaterasu.setLocalizedPageTitle("Overview");
  const heading = runtimeContext.Amaterasu.el("h1", "", "Overview");
  const literalHeading = runtimeContext.Amaterasu.el(
    "p",
    "",
    runtimeContext.Amaterasu.literalText("Overview"),
  );
  const explicitNotice = runtimeContext.Amaterasu.el(
    "p",
    "",
    runtimeContext.Amaterasu.localizedMessage("Session updated."),
  );
  const asyncStatus = new FakeElement("p");
  runtimeContext.Amaterasu.setLocalizedTextContent(asyncStatus, "Session updated.");
  const menu = new FakeElement("button");
  runtimeContext.Amaterasu.setLocalizedAttribute(menu, "aria-label", "Toggle navigation");
  const clientOverride = new FakeElement("input");
  runtimeContext.Amaterasu.setLocalizedAttributeTemplate(
    clientOverride,
    "aria-label",
    "{field} override for {client}",
    {
      field: runtimeContext.Amaterasu.localizedMessage("FEC percentage"),
      client: runtimeContext.Amaterasu.literalText("Overview"),
    },
  );
  const clientDirectory = runtimeContext.Amaterasu.createClientDirectory([
    { key: "a", name: "Living room", searchText: "Living room living-room", pairedAt: 10, card: new FakeElement("article") },
    { key: "b", name: "Desk", searchText: "Desk desk", pairedAt: 20, card: new FakeElement("article") },
  ]);
  const clientDirectoryRoot = clientDirectory.element;
  const date = new Date(Date.UTC(2024, 0, 2, 15, 4));
  const localizedDate = runtimeContext.Amaterasu.localeFormattedTextNode(
    (locale) => date.toLocaleString(locale),
  );
  const dateLabel = new FakeElement("time");
  dateLabel.append(localizedDate);
  root.append(heading);
  root.append(literalHeading);
  root.append(explicitNotice);
  root.append(asyncStatus);
  root.append(menu);
  root.append(clientOverride);
  root.append(clientDirectoryRoot);
  root.append(dateLabel);
  const textNode = heading.children[0];
  const literalTextNode = literalHeading.children[0];
  const noticeTextNode = explicitNotice.children[0];
  const dateTextNode = dateLabel.children[0];

  assert.equal(textNode.nodeValue, "Overview");
  assert.equal(literalTextNode.nodeValue, "Overview");
  assert.equal(noticeTextNode.nodeValue, "Session updated.");
  assert.equal(asyncStatus.children[0].nodeValue, "Session updated.");
  assert.equal(menu.getAttribute("aria-label"), "Toggle navigation");
  assert.equal(clientOverride.getAttribute("aria-label"), "FEC percentage override for Overview");
  assert.equal(dateTextNode.nodeValue, date.toLocaleString("en"));
  const directoryText = (node) => (node.children ?? [])
    .map((child) => child.nodeValue ?? directoryText(child))
    .join("");
  const searchLabel = clientDirectoryRoot.findByClass("field-label");
  const directoryCount = clientDirectoryRoot.children[0].children[2];
  const directoryEmpty = clientDirectoryRoot.children[1];
  const sortOption = clientDirectoryRoot.children[0].children[1].children[1].children[0];
  assert.equal(directoryText(searchLabel), "Search paired clients");
  assert.equal(directoryText(sortOption), "Recently paired");
  assert.equal(directoryText(directoryCount), "Showing 2 of 2 paired clients");
  clientDirectory.search.value = "missing";
  clientDirectory.search.dispatch("input");
  assert.equal(directoryText(directoryEmpty), "No paired clients match this search.");
  runtimeContext.Amaterasu.applyLocale("de");
  assert.equal(textNode.nodeValue, "Übersicht");
  assert.equal(literalTextNode.nodeValue, "Overview");
  assert.equal(noticeTextNode.nodeValue, "Sitzung aktualisiert.");
  assert.equal(
    asyncStatus.children[0].nodeValue,
    locale.translateLocaleMessage("Session updated.", locale.localeMessagesFor("de")),
  );
  assert.equal(menu.getAttribute("aria-label"), "Navigation umschalten");
  assert.equal(
    clientOverride.getAttribute("aria-label"),
    `${locale.translateLocaleMessage("FEC percentage", locale.localeMessagesFor("de"))} für Overview überschreiben`,
  );
  assert.equal(dateTextNode.nodeValue, date.toLocaleString("de"));
  assert.equal(directoryText(searchLabel), "Gekoppelte Clients suchen");
  assert.equal(directoryText(sortOption), "Zuletzt gekoppelt");
  assert.equal(directoryCount.children[0].nodeValue, "0 von 2 gekoppelten Clients werden angezeigt");
  assert.equal(directoryText(directoryEmpty), "Keine gekoppelten Clients entsprechen dieser Suche.");
  assert.equal(clientDirectory.search.getAttribute("placeholder"), "Gekoppelte Clients suchen");
  assert.equal(document.documentElement.lang, "de");
  assert.equal(document.title, "Übersicht · Amaterasu");

  runtimeContext.Amaterasu.applyLocale("en");
  assert.equal(textNode.nodeValue, "Overview");
  assert.equal(literalTextNode.nodeValue, "Overview");
  assert.equal(noticeTextNode.nodeValue, "Session updated.");
  assert.equal(asyncStatus.children[0].nodeValue, "Session updated.");
  assert.equal(menu.getAttribute("aria-label"), "Toggle navigation");
  assert.equal(clientOverride.getAttribute("aria-label"), "FEC percentage override for Overview");
  assert.equal(dateTextNode.nodeValue, date.toLocaleString("en"));
  assert.equal(directoryText(searchLabel), "Search paired clients");
  assert.equal(directoryCount.children[0].nodeValue, "Showing 0 of 2 paired clients");
  assert.equal(directoryText(directoryEmpty), "No paired clients match this search.");
  assert.equal(clientDirectory.search.getAttribute("placeholder"), "Search paired clients");
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
