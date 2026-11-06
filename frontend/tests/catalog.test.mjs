import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ atob, Blob, Error, TextEncoder, AbortController, window: { setInterval, clearInterval } });
for (const source of ["src/model.ts", "src/catalog.ts"]) {
  vm.runInContext(
    ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    context,
  );
}
const readSteamArtwork = context.Amaterasu.readSteamArtwork;
const readLutrisArtwork = context.Amaterasu.readLutrisArtwork;
const reference = { provider: "steam", app_id: 730, revision: "42" };
const lutrisReference = { provider: "lutris", game_id: 42, revision: "a".repeat(64) };
const windowsCapabilities = {
  frame_limiter_provider: ["auto", "rtss", "nvidia-control-panel"],
  rtss_frame_limit_type: [
    "async",
    "front edge sync",
    "back edge sync",
    "nvidia reflex",
  ],
};

test("catalog output edits preserve raw preparation, keybindings, and opaque plugin settings", () => {
  const model = context.Amaterasu;
  const rawPreparation = [{ do: " prepare $(PROFILE) ", undo: " restore ", elevated: true }];
  const overrides = { keybindings: [65, 66, 65, 67], frame_limiter_enable: false,
    frame_limiter_fps_limit: 0, frame_limiter_provider: "auto", lossless_scaling_framegen: false };
  for (const output of [undefined, null, "", "null", " logs/$(APP).log "]) {
    const fields = new FormData();
    fields.set("name", "Output application");
    fields.set("description", "Keep this description");
    fields.set("tags", "arcade, streaming");
    fields.set("kind", "executable");
    fields.set("path", "/opt/game");
    fields.set("arguments", '["--fullscreen"]');
    fields.set("prep_cmd", JSON.stringify(rawPreparation));
    fields.set("exclude_global_prep_cmd", "on");
    fields.set("elevated", "on");
    fields.set("keybindings", JSON.stringify(overrides.keybindings));
    for (const [name, value] of Object.entries(model.applicationOutputFormValues(output)))
      fields.set(name, value);
    for (const [name, value] of Object.entries(model.catalogSessionPolicyOverrideFormValues(overrides)))
      fields.set(name, value);
    const saved = model.entryFromForm(fields);
    saved.config_overrides = model.mergeCatalogConfigOverrides(overrides, fields.keys(),
      saved.config_overrides, model.parseCatalogSessionPolicyOverrides(fields, {}, overrides));
    const ordinary = JSON.parse(JSON.stringify(saved));
    assert.deepEqual(ordinary.prep_cmd, rawPreparation);
    assert.deepEqual(ordinary.config_overrides.keybindings, overrides.keybindings);
    assert.equal(ordinary.config_overrides.frame_limiter_enable, false);
    assert.equal(ordinary.config_overrides.frame_limiter_fps_limit, 0);
    assert.equal(ordinary.config_overrides.frame_limiter_provider, "auto");
    assert.equal(ordinary.config_overrides.lossless_scaling_framegen, false);
    assert.equal(ordinary.description, "Keep this description");
    assert.deepEqual(ordinary.tags, ["arcade", "streaming"]);
    assert.deepEqual(ordinary.launch.arguments, ["--fullscreen"]);
    assert.equal(ordinary.exclude_global_prep_cmd, true);
    assert.equal(ordinary.elevated, true);
    if (output == null) assert.equal(Object.hasOwn(ordinary, "output"), false);
    else assert.equal(ordinary.output, output);
  }
});

test("catalog NVENC overrides preserve inheritance, false booleans, zero, and bounds", () => {
  const model = context.Amaterasu;
  const ordinary = (value) => JSON.parse(JSON.stringify(value));
  const defaults = model.catalogNvencSettingsOverrideFormValues({});
  assert.deepEqual(ordinary(defaults), {
    nvenc_preset: "", nvenc_h264_cavlc: "", nvenc_twopass: "",
    nvenc_vbv_increase: "", nvenc_split_encode: "", nvenc_realtime_hags: "",
    nvenc_opengl_vulkan_on_dxgi: "", nvenc_latency_over_power: "",
    vaapi_strict_rc_buffer: "",
  });
  const fields = new FormData();
  for (const [name, value] of Object.entries({
    nvenc_preset: "7", nvenc_h264_cavlc: "false", nvenc_twopass: "full_res",
    nvenc_vbv_increase: "0", nvenc_split_encode: "disabled", nvenc_realtime_hags: "false",
    nvenc_opengl_vulkan_on_dxgi: "true", nvenc_latency_over_power: "false",
    vaapi_strict_rc_buffer: "true",
  })) fields.set(name, value);
  assert.deepEqual(ordinary(model.parseCatalogNvencSettingsOverrides(fields)), {
    nvenc_preset: 7, nvenc_h264_cavlc: false, nvenc_twopass: "full_res",
    nvenc_vbv_increase: 0, nvenc_split_encode: "disabled", nvenc_realtime_hags: false,
    nvenc_opengl_vulkan_on_dxgi: true, nvenc_latency_over_power: false,
    vaapi_strict_rc_buffer: true,
  });
  const inherited = model.parseCatalogNvencSettingsOverrides(new FormData());
  assert.deepEqual(ordinary(inherited), {
    nvenc_preset: null, nvenc_h264_cavlc: null, nvenc_twopass: null,
    nvenc_vbv_increase: null, nvenc_split_encode: null, nvenc_realtime_hags: null,
    nvenc_opengl_vulkan_on_dxgi: null, nvenc_latency_over_power: null,
    vaapi_strict_rc_buffer: null,
  });
  for (const [name, value] of [["nvenc_preset", "0"], ["nvenc_preset", "8"], ["nvenc_vbv_increase", "401"], ["nvenc_vbv_increase", "1.5"], ["nvenc_twopass", "fast"], ["nvenc_split_encode", "force"], ["nvenc_h264_cavlc", "0"]]) {
    const invalid = new FormData();
    invalid.set(name, value);
    assert.throws(() => model.parseCatalogNvencSettingsOverrides(invalid));
  }
});

test("catalog app and provider saves retain unrendered overrides while applying sparse edits", () => {
  const merge = context.Amaterasu.mergeCatalogConfigOverrides;
  const saved = {
    nvenc_spatial_aq: false,
    vaapi_quality: 5,
    custom_unrendered_flag: true,
    nvenc_h264_cavlc: true,
  };
  const edits = {
    nvenc_h264_cavlc: false,
    nvenc_vbv_increase: 0,
  };

  // The ordinary and Playnite-provider submit handlers both seed the same
  // merge with the stored overrides and then apply their rendered edit fields.
  for (const result of [
    merge(saved, ["nvenc_h264_cavlc", "nvenc_vbv_increase"], edits),
    merge(saved, ["nvenc_h264_cavlc", "nvenc_vbv_increase"], {}, edits),
  ]) {
    const persisted = JSON.parse(JSON.stringify(result));
    assert.equal(persisted.nvenc_spatial_aq, false);
    assert.equal(persisted.vaapi_quality, 5);
    assert.equal(persisted.custom_unrendered_flag, true);
    assert.equal(persisted.nvenc_h264_cavlc, false);
    assert.equal(persisted.nvenc_vbv_increase, 0);
  }
  assert.equal(merge(saved, ["keybindings"], edits).nvenc_spatial_aq, false);
  assert.equal(Object.hasOwn(merge({ keybindings: [65] }, ["keybindings"], {}), "keybindings"), false);
});

test("application display settings preserve inheritance, explicit false, bounds, and command output separation", () => {
  const model = context.Amaterasu;
  const empty = model.catalogApplicationDisplayFormValues({});
  assert.equal(empty.display_output, "");
  assert.equal(empty.virtual_screen, "");
  assert.equal(empty.virtual_display_mode, "");
  assert.equal(empty.virtual_display_layout, "");
  const falseValue = model.catalogApplicationDisplayFormValues({
    "display-output": "Virtual-1",
    "virtual-screen": false,
    "virtual-display-mode": "disabled",
    "virtual-display-layout": "extended_primary_isolated",
    output: "command.log",
  });
  assert.equal(falseValue.display_output, "Virtual-1");
  assert.equal(falseValue.virtual_screen, "false");
  assert.equal(falseValue.virtual_display_mode, "disabled");
  assert.equal(falseValue.virtual_display_layout, "extended_primary_isolated");

  const inherited = JSON.parse(JSON.stringify(model.parseCatalogApplicationDisplaySettings(new FormData())));
  assert.deepEqual(inherited, {});
  const fields = new FormData();
  fields.set("name", "Display settings application");
  fields.set("kind", "executable");
  fields.set("path", "/opt/display-game");
  fields.set("output_mode", "value");
  fields.set("output_value", "logs/game.log");
  fields.set("display_output", "  DISPLAY-1  ");
  fields.set("virtual_screen", "false");
  fields.set("virtual_display_mode", "disabled");
  fields.set("virtual_display_layout", "extended_primary_isolated");
  const saved = model.entryFromForm(fields);
  Object.assign(saved, model.parseCatalogApplicationDisplaySettings(fields));
  assert.equal(saved.output, "logs/game.log");
  assert.equal(saved["display-output"], "DISPLAY-1");
  assert.equal(saved["virtual-screen"], false);
  assert.equal(saved["virtual-display-mode"], "disabled");
  assert.equal(saved["virtual-display-layout"], "extended_primary_isolated");
  assert.deepEqual(JSON.parse(JSON.stringify({
    "display-output": saved["display-output"],
    "virtual-screen": saved["virtual-screen"],
    "virtual-display-mode": saved["virtual-display-mode"],
    "virtual-display-layout": saved["virtual-display-layout"],
  })), {
    "display-output": "DISPLAY-1",
    "virtual-screen": false,
    "virtual-display-mode": "disabled",
    "virtual-display-layout": "extended_primary_isolated",
  });
  fields.set("virtual_screen", "true");
  assert.deepEqual(
    JSON.parse(JSON.stringify(model.parseCatalogApplicationDisplaySettings(fields))),
    {
      "display-output": "DISPLAY-1",
      "virtual-screen": true,
      "virtual-display-mode": "disabled",
      "virtual-display-layout": "extended_primary_isolated",
    },
  );
  fields.set("virtual_display_mode", "per_client");
  fields.set("virtual_display_layout", "extended");
  assert.deepEqual(
    JSON.parse(JSON.stringify(model.parseCatalogApplicationDisplaySettings(fields))),
    {
      "display-output": "DISPLAY-1",
      "virtual-screen": true,
      "virtual-display-mode": "per_client",
      "virtual-display-layout": "extended",
    },
  );
  fields.set("virtual_display_mode", "");
  fields.set("virtual_display_layout", "");
  assert.deepEqual(
    JSON.parse(JSON.stringify(model.parseCatalogApplicationDisplaySettings(fields))),
    { "display-output": "DISPLAY-1", "virtual-screen": true },
  );
  fields.set("virtual_display_mode", "inherit");
  assert.throws(() => model.parseCatalogApplicationDisplaySettings(fields), /Virtual display mode/);
  fields.set("virtual_display_mode", "");
  fields.set("virtual_display_layout", "auto");
  assert.throws(() => model.parseCatalogApplicationDisplaySettings(fields), /Virtual display layout/);
  fields.set("virtual_screen", "no");
  assert.throws(() => model.parseCatalogApplicationDisplaySettings(fields), /Virtual screen/);

  for (const displayOutput of ["x".repeat(1_025), "é".repeat(513), "display\nname"]) {
    const invalid = new FormData();
    invalid.set("display_output", displayOutput);
    assert.throws(() => model.parseCatalogApplicationDisplaySettings(invalid), /Display output/);
  }
  for (const entry of [
    { "display-output": true },
    { "display-output": "x".repeat(1_025) },
    { "virtual-screen": "false" },
    { "virtual-display-mode": true },
    { "virtual-display-mode": "inherit" },
    { "virtual-display-layout": "extended_primary_isolated_extra" },
  ]) {
    assert.throws(() => model.catalogApplicationDisplayFormValues(entry), /Existing/);
  }
});

test("application refresh rate option editor reads back and writes sparse exact tokens", () => {
  const parse = context.Amaterasu.parseCatalogRefreshRateOptionOverride;
  const fields = new FormData();
  assert.equal(parse(fields), null);
  fields.set("dd_refresh_rate_option", "");
  assert.equal(parse(fields), null);
  for (const value of ["auto", "disabled", "manual", "prefer_highest"]) {
    fields.set("dd_refresh_rate_option", value);
    assert.equal(parse(fields), value);
    assert.equal(context.Amaterasu.catalogRefreshRateOptionText(value), value);
  }
  for (const value of ["AUTO", "auto ", "unknown", "60"]) {
    fields.set("dd_refresh_rate_option", value);
    assert.throws(() => parse(fields), /Refresh rate option/);
  }

  assert.equal(context.Amaterasu.catalogRefreshRateOptionText(null), "");
  assert.equal(context.Amaterasu.catalogRefreshRateOptionText(undefined), "");
  assert.equal(context.Amaterasu.catalogManualRefreshRateCompanionVisible("manual"), true);
  for (const value of [null, undefined, "auto", "disabled", "prefer_highest"])
    assert.equal(context.Amaterasu.catalogManualRefreshRateCompanionVisible(value), false);
});

test("catalog input compatibility overrides preserve inheritance and explicit false", () => {
  const parse = context.Amaterasu.parseCatalogInputCompatibilityOverrides;
  const fields = new FormData();
  assert.deepEqual(JSON.parse(JSON.stringify(parse(fields))), {
    motion_as_ds4: null,
    touchpad_as_ds4: null,
    ds5_inputtino_randomize_mac: null,
    proton_dualsense_compatibility: null,
  });
  for (const key of [
    "motion_as_ds4",
    "touchpad_as_ds4",
    "ds5_inputtino_randomize_mac",
    "proton_dualsense_compatibility",
  ]) {
    fields.set(key, "false");
    assert.equal(parse(fields)[key], false);
    fields.set(key, "true");
    assert.equal(parse(fields)[key], true);
    fields.set(key, "invalid");
    assert.throws(() => parse(fields));
    fields.delete(key);
  }
});

test("catalog reads back 10-bit SDR overrides without collapsing false or accepting malformed values", () => {
  const read = context.Amaterasu.catalogPrefer10BitSdrOverrideValue;
  assert.equal(read(undefined), null);
  assert.equal(read(null), null);
  assert.equal(read(true), true);
  assert.equal(read(false), false);
  for (const value of ["false", 0, 1, {}, []]) {
    assert.throws(() => read(value), /boolean or null/);
  }
});

test("providers fetch one binary image with pinned identity and cancellation", async () => {
  for (const [read, ref, route, key] of [[readSteamArtwork, reference, "steam/artwork", "app_id"], [readLutrisArtwork, lutrisReference, "lutris/artwork", "game_id"]]) {
    let calls = 0;
    const bytes = Buffer.alloc(1024 * 1024 + 31, 0x51);
    const api = { async post(path, payload, protectedRoute, signal) {
      calls++;
      assert.equal(path, route);
      assert.equal(payload[key], ref[key]);
      assert.equal(payload.revision, ref.revision);
      assert.equal(Object.hasOwn(payload, "offset"), false);
      assert.equal(protectedRoute, true);
      assert.equal(signal.aborted, false);
      return new Blob([bytes], { type: "image/png" });
    } };
    const image = await read(api, ref);
    assert.deepEqual(Buffer.from(await image.arrayBuffer()), bytes);
    assert.equal(calls, 1);
    await assert.rejects(read(api, ref, () => false), /cancelled/);
    assert.equal(calls, 1);
  }
});
test("provider references reject malformed IDs before requesting bytes", async () => {
  const api = { post() { throw new Error("unexpected request"); } };
  await assert.rejects(readSteamArtwork(api, { ...reference, revision: "../art" }), /Invalid Steam/);
  await assert.rejects(readLutrisArtwork(api, { ...lutrisReference, game_id: 0 }), /Invalid Lutris/);
  await assert.rejects(readLutrisArtwork(api, { ...lutrisReference, revision: "x" }), /Invalid Lutris/);
});
test("removing a view aborts an in-flight binary response", async () => {
  let active = true;
  const api = { post(_route, _payload, _protected, signal) {
    active = false;
    return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("cancelled")), { once: true }));
  } };
  await assert.rejects(readSteamArtwork(api, reference, () => active), /cancelled/);
});

test("Lutris card refresh ignores stale A and revokes B on load or removal", async () => {
  class Element {
    constructor() { this.isConnected = true; this.children = []; this.listeners = new Map(); }
    prepend(child) { child.parent = this; this.children.unshift(child); }
    remove() { if (this.parent) this.parent.children = this.parent.children.filter((child) => child !== this); }
    addEventListener(type, listener) { this.listeners.set(type, listener); }
    fire(type) { this.listeners.get(type)?.(); }
  }
  const created = [];
  const revoked = [];
  const observers = [];
  const artworkContext = vm.createContext({
    atob, Blob, Error, AbortController, queueMicrotask, window: { setInterval, clearInterval },
    URL: {
      createObjectURL(blob) { created.push(blob); return `blob:${created.length}`; },
      revokeObjectURL(url) { revoked.push(url); },
    },
    document: { createElement: () => new Element() },
    MutationObserver: class {
      constructor(callback) { this.callback = callback; observers.push(this); }
      observe() {}
      disconnect() { this.disconnected = true; }
      notify() { if (!this.disconnected) this.callback(); }
    },
  });
  vm.runInContext(ts.transpile(readFileSync("src/ui.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }), artworkContext);
  vm.runInContext("Object.assign(globalThis, Amaterasu)", artworkContext);
  vm.runInContext(ts.transpile(readFileSync("src/catalog.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }), artworkContext);
  const show = artworkContext.Amaterasu.showCatalogArtwork;
  const pending = new Map();
  const api = {
    allows: (scope) => scope === "lutris.read",
    post(route, request) {
      assert.equal(route, "lutris/artwork");
      return new Promise((resolve) => pending.set(request.revision, resolve));
    },
  };
  const content = new Element();
  const oldArt = new Element();
  const newArt = new Element();
  const b = { ...lutrisReference, revision: "b".repeat(64) };
  show(api, lutrisReference, oldArt, content);
  oldArt.isConnected = false;
  show(api, b, newArt, content);
  await new Promise(setImmediate);
  pending.get(b.revision)(new Blob(["Q"], { type: "image/png" }));
  await new Promise(setImmediate);
  assert.equal(oldArt.children.length, 0);
  assert.equal(newArt.children.length, 1);
  assert.equal(created.length, 1);
  newArt.children[0].fire("load");
  assert.deepEqual(revoked, ["blob:1"]);

  const removedArt = new Element();
  show(api, b, removedArt, content);
  await new Promise(setImmediate);
  pending.get(b.revision)(new Blob(["Q"], { type: "image/png" }));
  await new Promise(setImmediate);
  removedArt.isConnected = false;
  for (const observer of observers) observer.notify();
  assert.deepEqual(revoked, ["blob:1", "blob:2"]);
  assert.equal(observers.every((observer) => observer.disconnected), true);

  show(api, undefined, new Element(), content);
  show({ ...api, allows: () => false }, b, new Element(), content);
  assert.equal(created.length, 2);

  const steamArt = new Element();
  show({
    allows: (scope) => scope === "steam.read",
    async post(route, request) {
      assert.equal(route, "steam/artwork");
      return new Blob(["Q"], { type: "image/png" });
    },
  }, reference, steamArt, content);
  await new Promise(setImmediate);
  assert.equal(steamArt.children.length, 1);
  steamArt.children[0].fire("error");
  assert.equal(steamArt.children.length, 0);
  assert.deepEqual(revoked, ["blob:1", "blob:2", "blob:3"]);
});

test("catalog artwork uses only the catalog identity and expected revision", async () => {
  const calls = [];
  const api = { async post(route, request) {
    calls.push({ route, request: { ...request } });
    return new Blob(["abc"], { type: "image/png" });
  } };
  const image = await context.Amaterasu.readCatalogArtwork(api, "app-0000000000000001", "vendor revision");
  assert.equal(await image.text(), "abc");
  assert.deepEqual(calls, [{ route: "library/artwork", request: { id: "app-0000000000000001", revision: "vendor revision" } }]);
});

test("catalog covers wait for attachment, bound concurrent transfers, and skip removed cards", async () => {
  class Element {
    constructor(connected = false) { this.isConnected = connected; this.children = []; this.listeners = new Map(); }
    prepend(child) { this.children.unshift(child); }
    addEventListener(type, listener) { this.listeners.set(type, listener); }
    fire(type) { this.listeners.get(type)?.(); }
    remove() {}
  }
  const revoked = [];
  let nextUrl = 0;
  const artworkContext = vm.createContext({
    atob, Blob, Error, queueMicrotask, AbortController, window: { setInterval, clearInterval },
    crypto: { getRandomValues(bytes) { bytes.fill(++nextUrl); return bytes; } },
    URL: { createObjectURL: () => `blob:${nextUrl}`, revokeObjectURL: (url) => revoked.push(url) },
    document: { createElement: () => new Element() },
    MutationObserver: class { constructor(callback) { this.callback = callback; } observe() {} disconnect() {} },
  });
  vm.runInContext(ts.transpile(readFileSync("src/ui.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), artworkContext);
  vm.runInContext("Object.assign(globalThis, Amaterasu)", artworkContext);
  vm.runInContext(ts.transpile(readFileSync("src/catalog.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }), artworkContext);
  const pending = [];
  let reads = 0;
  const api = {
    allows: (scope) => scope === "catalog.read",
    post(route, request) {
      assert.equal(route, "library/artwork");
      reads++;
      return new Promise((resolve) => pending.push(() => resolve(new Blob(["Q"], { type: "image/png" }))));
    },
  };
  const content = new Element(true);
  const arts = Array.from({ length: 7 }, () => new Element());
  for (const [index, art] of arts.entries()) {
    artworkContext.Amaterasu.showCatalogArtwork(api, reference, art, content, undefined,
      `app-${(index + 1).toString(16).padStart(16, "0")}`);
  }
  assert.equal(reads, 0);
  arts.forEach((art) => { art.isConnected = true; });
  await new Promise(setImmediate);
  assert.equal(reads, 4);
  arts[4].isConnected = false;
  pending.shift()();
  await new Promise(setImmediate);
  assert.equal(reads, 5);
  assert.equal(arts[0].children.length, 1);
  assert.equal(arts[4].children.length, 0);
  while (pending.length) pending.shift()();
  await new Promise(setImmediate);
  while (pending.length) pending.shift()();
  await new Promise(setImmediate);
  assert.equal(reads, 6);
  for (const art of arts) art.children[0]?.fire("load");
  assert.equal(revoked.length, 6);
});

test("unchanged unknown encoder and frame generation options survive a save", () => {
  const model = context.Amaterasu;
  const saved = { nvenc_twopass: "future-pass", nvenc_split_encode: "future-split", frame_generation_provider: "future-provider", frame_generation_mode: "future-mode" };
  const fields = new FormData();
  for (const [key, value] of Object.entries(saved)) fields.set(key, value);
  const encoder = model.parseCatalogNvencSettingsOverrides(fields, saved);
  const session = model.parseCatalogSessionPolicyOverrides(fields, {}, saved);
  for (const key of ["nvenc_twopass", "nvenc_split_encode"]) assert.equal(encoder[key], saved[key]);
  for (const key of ["frame_generation_provider", "frame_generation_mode"]) assert.equal(session[key], saved[key]);
  fields.set("nvenc_twopass", "different-invalid-value");
  assert.throws(() => model.parseCatalogNvencSettingsOverrides(fields, saved));
});
