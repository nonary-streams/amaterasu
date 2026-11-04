import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class Element {
  constructor(tagName) {
    this.tagName = tagName;
    this.children = [];
    this.listeners = new Map();
    this.value = "";
    this.dataset = {};
    this.isConnected = false;
  }
  setConnected(connected) {
    this.isConnected = connected;
    for (const child of this.children) child?.setConnected?.(connected);
  }
  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      if (child && typeof child === "object") { child.remove(); child.parentElement = this; }
      this.children.push(child);
      child?.setConnected?.(this.isConnected);
    }
  }
  replaceChildren(...children) {
    for (const child of this.children) child?.setConnected?.(false);
    this.children = [];
    this.append(...children);
  }
  addEventListener(name, callback) { this.listeners.set(name, callback); }
  setAttribute() {}
  removeAttribute() {}
  scrollIntoView() {}
  focus() {}
  reportValidity() { return true; }
  remove() {
    if (!this.parentElement) return;
    this.parentElement.children = this.parentElement.children.filter(child => child !== this);
    this.parentElement = null;
    this.setConnected(false);
  }
  get classList() { return { contains: name => (this.className ?? "").split(" ").includes(name) }; }
  get options() { return this.children; }
  querySelectorAll(selector) {
    const matches = node => selector.split(",").some(part => {
      part = part.trim();
      if (part === "summary strong") return node.tagName === "strong" && node.parentElement?.tagName === "summary";
      if (part === "button:not([hidden])") return node.tagName === "button" && !node.hidden;
      if (part.startsWith(".")) return node.classList.contains(part.slice(1));
      return node.tagName === part;
    });
    const found = [];
    const walk = node => { for (const child of node.children ?? []) if (typeof child !== "string") { if (matches(child)) found.push(child); walk(child); } };
    walk(this);
    return found;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] ?? null; }
  dispatchEvent(event) { this.fire(event.type); }
  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
  }
  set textContent(value) { this.replaceChildren(value); }
  fire(name) { this.listeners.get(name)?.({ preventDefault() {} }); }
}

class SelectElement extends Element {}

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children ?? []) {
    if (typeof child === "string") continue;
    const found = find(child, predicate);
    if (found) return found;
  }
  return null;
}

class EditorFormData extends FormData {
  constructor(form) {
    super();
    const collect = (node) => {
      if (node.name && !node.disabled && ["input", "select", "textarea"].includes(node.tagName) &&
          (node.type !== "checkbox" || node.checked))
        this.set(node.name, node.value);
      for (const child of node.children ?? []) if (typeof child !== "string") collect(child);
    };
    if (form) collect(form);
  }
}

const ordinary = (value) => JSON.parse(JSON.stringify(value));
const settle = () => new Promise(setImmediate);

test("application DOM saves output and keybindings while preserving raw preparation and sparse controls", async () => {
  const context = vm.createContext({
    FormData: EditorFormData, TextEncoder, URL, Error, AbortController,
    HTMLElement: Element, HTMLSelectElement: SelectElement, Event,
    document: { createElement: (tagName) => tagName === "select" ? new SelectElement(tagName) : new Element(tagName) },
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts", "src/catalog.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  const content = new Element("main");
  content.setConnected(true);
  let revision = 7;
  const rawPreparation = [{ do: " prepare $(PROFILE) ", undo: " restore ", elevated: true }];
  let entry = {
    name: "Mapped application", description: "", tags: [],
    launch: { kind: "application", path: "/opt/game" },
    output: " logs/$(APP).log ",
    prep_cmd: rawPreparation,
    config_overrides: { encoder: "software", keybindings: [65, 66, 65, 67], frame_limiter_enable: false, frame_limiter_fps_limit: 0, nvenc_spatial_aq: false },
  };
  const updates = [];
  context.Amaterasu.renderCatalog({
    content, notify() {}, api: {
      allows: () => true,
      async post(route, body) {
        if (route === "catalog/list") return {
          ok: true, revision, next_cursor: null,
          config_override_capabilities: {
            frame_limiter_provider: ["auto"], rtss_frame_limit_type: ["async"],
          },
          entries: [{ id: "app-1", revision, entry }],
        };
        if (route === "input/status") throw new Error("unavailable");
        assert.equal(route, "catalog/update");
        assert.equal(body.id, "app-1");
        assert.equal(body.expected_revision, revision);
        updates.push(ordinary(body));
        entry = ordinary(body.entry);
        revision += 1;
        return { ok: true, revision };
      },
    },
  });
  await settle();
  let expectedUpdates = 0;
  for (const [text, expected, output] of [["[0x41,0x44,0x41,0x45]", [65, 68, 65, 69], ""], ["[]", [], "null"], ["", undefined, undefined]]) {
    find(content, (node) => node.tagName === "button" && node.textContent === "Edit").fire("click");
    await settle();
    const form = find(content, (node) => node.className === "editor-form");
    if (updates.length === 0) {
      const selected = find(form, node => node.className === "catalog-override-row" && find(node, child => child.name === "keybindings"));
      const inherited = find(form, node => node.className === "catalog-override-row" && find(node, child => child.name === "controller"));
      assert.equal(selected.hidden, false);
      const encoder = find(form, node => node.className === "catalog-override-row" && find(node, child => child.name === "encoder_backend"));
      assert.equal(encoder.hidden, false);
      find(encoder, node => node.tagName === "button" && node.textContent === "Remove").fire("click");
      assert.equal(encoder.hidden, true);
      assert.equal(inherited.hidden, true);
      const add = find(form, node => node.className === "button secondary catalog-override-choice" && node.textContent === "Controller input");
      add.fire("click");
      assert.equal(inherited.hidden, false);
      find(inherited, node => node.tagName === "button" && node.textContent === "Remove").fire("click");
      assert.equal(inherited.hidden, true);
    }
    const control = find(form, (node) => node.name === "keybindings");
    const outputMode = find(form, (node) => node.name === "output_mode");
    const outputValue = find(form, (node) => node.name === "output_value");
    assert.equal(outputMode.value, entry.output == null ? "unset" : "value");
    assert.equal(outputValue.value, entry.output ?? "");
    assert.equal(find(form, (node) => node.name === "prep_cmd").value, JSON.stringify(rawPreparation));
    assert.equal(control.value, JSON.stringify(entry.config_overrides?.keybindings) ?? "");
    if (updates.length === 0) {
      control.value = "[0,256]";
      form.fire("submit");
      await settle();
      assert.equal(updates.length, 0, "invalid byte map reached a catalog mutation");
    }
    control.value = text;
    outputMode.value = output === undefined ? "unset" : "value";
    outputMode.fire("change");
    assert.equal(outputValue.disabled, output === undefined);
    outputValue.value = output ?? "";
    form.fire("submit");
    await settle();
    expectedUpdates += 1;
    assert.equal(updates.length, expectedUpdates, form.textContent);
    assert.equal(form.isConnected, false, form.textContent);
    assert.deepEqual(entry.config_overrides.keybindings, expected);
    assert.equal(Object.hasOwn(entry.config_overrides, "keybindings"), expected !== undefined);
    assert.equal(entry.output, output);
    assert.equal(Object.hasOwn(entry, "output"), output !== undefined);
    assert.deepEqual(entry.prep_cmd, rawPreparation);
    assert.equal(entry.config_overrides.frame_limiter_enable, false);
    assert.equal(entry.config_overrides.frame_limiter_fps_limit, 0);
    assert.equal(entry.config_overrides.nvenc_spatial_aq, false);
    assert.equal(Object.hasOwn(entry.config_overrides, "encoder"), false);
  }
  assert.deepEqual(updates.map((update) => update.expected_revision), [7, 8, 9]);
});

test("desktop editor saves its process-less target, preparation and display settings", async () => {
  const context = vm.createContext({ FormData:EditorFormData, TextEncoder, URL, Error, AbortController,
    HTMLElement:Element, HTMLSelectElement:SelectElement, Event,
    document:{createElement:(tag)=>tag === "select" ? new SelectElement(tag) : new Element(tag)} });
  for (const source of ["src/model.ts","src/ui.ts","src/pages.ts","src/settings.ts","src/catalog.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source,"utf8"),{target:ts.ScriptTarget.ES2022}),context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)",context);
  }
  const content = new Element("main"); content.setConnected(true);
  const preparation = [{run:{executable:"/usr/bin/setup",args:["enable"]}}];
  const desktop = {name:"Desktop",description:"",tags:[],"display-output":"Virtual-1","virtual-screen":false,
    launch:{kind:"desktop",exit_timeout_seconds:30,working_directory:"/home/player",preparation}};
  let saved;
  context.Amaterasu.renderCatalog({content,notify(){},navigate(){},api:{allows:()=>true, async post(route,payload){
    if (route === "catalog/list") return {ok:true,revision:1,next_cursor:null,config_override_capabilities:{frame_limiter_provider:["auto"],rtss_frame_limit_type:[]},entries:[{id:"app-1",entry:desktop}]};
    if (route === "input/status") throw new Error("unavailable");
    if (route === "catalog/update") { saved=ordinary(payload); return {ok:true,revision:2}; }
    throw new Error(`unexpected route ${route}`);
  }}});
  await settle();
  find(content,(node)=>node.tagName === "button" && node.textContent === "Edit").fire("click");
  await settle();
  const form = find(content,(node)=>node.className === "editor-form");
  assert.equal(find(form,(node)=>node.name === "kind").value,"desktop");
  assert.equal(find(form,(node)=>node.name === "path").disabled,true);
  assert.equal(find(form,(node)=>node.name === "working_directory").value,"/home/player");
  assert.equal(find(form,(node)=>node.name === "fullscreen_exit_timeout_seconds").value,"30");
  form.fire("submit");
  await settle();
  assert.deepEqual(saved.entry.launch,{kind:"desktop",exit_timeout_seconds:30,working_directory:"/home/player",preparation});
  assert.equal(saved.entry["display-output"],"Virtual-1");
  assert.equal(saved.entry["virtual-screen"],false);
  assert.equal(Object.hasOwn(saved.entry,"steam_id"),false);
});

test("configuration-only users can open application plugin overrides", async () => {
  const context = vm.createContext({ FormData: EditorFormData, TextEncoder, URL, Error, AbortController,
    HTMLElement: Element, HTMLSelectElement: SelectElement, Event,
    document: { createElement: tag => tag === "select" ? new SelectElement(tag) : new Element(tag) } });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts", "src/catalog.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), { target: ts.ScriptTarget.ES2022 }), context);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
  }
  let opened = null;
  context.renderApplicationPluginOverrides = (_context, _area, id) => { opened = id; };
  const content = new Element("main");
  content.setConnected(true);
  context.Amaterasu.renderCatalog({ content, notify() {}, api: {
    allows: scope => scope === "configuration.read" || scope === "configuration.write",
    async post(route) {
      if (route === "catalog/list") return { ok: true, revision: 1, next_cursor: null, config_override_capabilities: {}, entries: [{ id: "game", revision: 1, entry: { name: "Game", launch: { kind: "application", path: "/game" } } }] };
      if (route === "input/status") throw new Error("unavailable");
      throw new Error(`Unexpected ${route}`);
    },
  } });
  await settle();
  const settings = find(content, node => node.tagName === "button" && node.textContent === "Settings");
  assert.equal(settings.disabled, false);
  settings.fire("click");
  await settle();
  assert.equal(opened, "game");
  assert.equal(find(content, node => node.className === "editor-form").hidden, true);
  assert.ok(find(content, node => node.tagName === "button" && node.textContent === "Close"));
});
