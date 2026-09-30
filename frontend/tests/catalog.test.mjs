import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ atob, Blob, Error, TextEncoder });
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

test("catalog output edits preserve raw preparation, keybindings, and sparse frame limiter controls", () => {
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
    for (const [name, value] of Object.entries(model.catalogFrameLimiterOverrideFormValues(overrides)))
      fields.set(name, value);
    const saved = model.entryFromForm(fields);
    saved.config_overrides = { ...saved.config_overrides,
      ...model.parseCatalogFrameLimiterOverrides(fields, windowsCapabilities, overrides) };
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

test("Catalog frame limiter editor round-trips inherited values, explicit false, and zero", () => {
  const readback = context.Amaterasu.catalogFrameLimiterOverrideFormValues;
  const parse = context.Amaterasu.parseCatalogFrameLimiterOverrides;
  const inheritedValues = JSON.parse(JSON.stringify(readback(undefined)));
  assert.deepEqual(inheritedValues, {
    frame_limiter_enable: "",
    frame_limiter_fps_limit: "",
    frame_limiter_provider: "",
    frame_limiter_disable_vsync: "",
    frame_limiter_auto_virtual_framegen: "",
    lossless_scaling_framegen: "",
    rtss_frame_limit_type: "",
    ds4_back_as_touchpad_click: "",
  });

  const existingOverrides = {
    frame_limiter_enable: false,
    frame_limiter_fps_limit: 0,
    frame_limiter_provider: "auto",
    frame_limiter_disable_vsync: null,
    frame_limiter_auto_virtual_framegen: null,
    lossless_scaling_framegen: false,
    rtss_frame_limit_type: null,
    ds4_back_as_touchpad_click: false,
  };
  const editorValues = JSON.parse(JSON.stringify(readback(existingOverrides)));
  assert.deepEqual(editorValues, {
    frame_limiter_enable: "false",
    frame_limiter_fps_limit: "0",
    frame_limiter_provider: "auto",
    frame_limiter_disable_vsync: "",
    frame_limiter_auto_virtual_framegen: "",
    lossless_scaling_framegen: "false",
    rtss_frame_limit_type: "",
    ds4_back_as_touchpad_click: "false",
  });

  const form = new FormData();
  for (const [name, value] of Object.entries(editorValues)) form.set(name, value);
  const savedPayload = JSON.parse(JSON.stringify(parse(form, windowsCapabilities, existingOverrides)));
  assert.deepEqual(savedPayload, {
    frame_limiter_enable: false,
    frame_limiter_fps_limit: 0,
    frame_limiter_provider: "auto",
    frame_limiter_disable_vsync: null,
    frame_limiter_auto_virtual_framegen: null,
    lossless_scaling_framegen: false,
    rtss_frame_limit_type: null,
    ds4_back_as_touchpad_click: false,
  });
  assert.equal(Object.hasOwn(savedPayload, "dd_refresh_rate_option"), false);
  assert.equal(Object.hasOwn(savedPayload, "prefer_10bit_sdr"), false);
  assert.deepEqual(JSON.parse(JSON.stringify(readback(savedPayload))), editorValues);
});

test("Catalog frame limiter controls enforce the current override types and domains", () => {
  const parse = context.Amaterasu.parseCatalogFrameLimiterOverrides;
  const fields = new FormData();
  assert.deepEqual(JSON.parse(JSON.stringify(parse(fields, windowsCapabilities))), {
    frame_limiter_enable: null,
    frame_limiter_fps_limit: null,
    frame_limiter_provider: null,
    frame_limiter_disable_vsync: null,
    frame_limiter_auto_virtual_framegen: null,
    lossless_scaling_framegen: null,
    rtss_frame_limit_type: null,
    ds4_back_as_touchpad_click: null,
  });

  for (const provider of windowsCapabilities.frame_limiter_provider) {
    fields.set("frame_limiter_provider", provider);
    assert.equal(parse(fields, windowsCapabilities).frame_limiter_provider, provider);
  }
  for (const name of [
    "frame_limiter_enable",
    "frame_limiter_disable_vsync",
    "lossless_scaling_framegen",
    "ds4_back_as_touchpad_click",
  ]) {
    fields.set(name, "true");
    assert.equal(parse(fields, windowsCapabilities)[name], true);
  }
  for (const value of ["true", "false"]) {
    fields.set("lossless_scaling_framegen", value);
    assert.equal(
      parse(fields, windowsCapabilities).lossless_scaling_framegen,
      value === "true",
    );
  }
  for (const mode of ["enabled", "vrr", "legacy", "disabled"]) {
    fields.set("frame_limiter_auto_virtual_framegen", mode);
    assert.equal(parse(fields, windowsCapabilities).frame_limiter_auto_virtual_framegen, mode);
  }
  for (const limitType of windowsCapabilities.rtss_frame_limit_type) {
    fields.set("rtss_frame_limit_type", limitType);
    assert.equal(parse(fields, windowsCapabilities).rtss_frame_limit_type, limitType);
  }
  for (const fpsLimit of ["0", "59.94", "1000", "1.2300"]) {
    fields.set("frame_limiter_fps_limit", fpsLimit);
    assert.equal(parse(fields, windowsCapabilities).frame_limiter_fps_limit, Number(fpsLimit));
  }

  for (const [name, value] of [
    ["frame_limiter_enable", "yes"],
    ["frame_limiter_disable_vsync", "0"],
    ["lossless_scaling_framegen", "yes"],
    ["ds4_back_as_touchpad_click", "FALSE"],
    ["frame_limiter_fps_limit", "1.0001"],
    ["frame_limiter_fps_limit", "1.2345"],
    ["frame_limiter_fps_limit", "1000.001"],
    ["frame_limiter_fps_limit", "-0.001"],
    ["frame_limiter_fps_limit", "60hz"],
    ["frame_limiter_provider", "unknown"],
    ["frame_limiter_auto_virtual_framegen", "true"],
    ["frame_limiter_auto_virtual_framegen", "automatic"],
    ["rtss_frame_limit_type", "invalid"],
  ]) {
    const invalidFields = new FormData();
    invalidFields.set(name, value);
    assert.throws(
      () => parse(invalidFields, windowsCapabilities),
      /Frame limiter|Lossless Scaling|VSync|DS4 Back|Auto virtual|RTSS frame limit/,
    );
  }
});

test("Catalog frame limiter controls use populated Windows-like host capabilities", () => {
  const capabilities = {
    frame_limiter_provider: ["auto", "rtss", "nvidia-control-panel"],
    rtss_frame_limit_type: ["async", "front edge sync"],
  };
  const options = context.Amaterasu.catalogFrameLimiterOverrideOptionValues;
  assert.deepEqual(
    JSON.parse(JSON.stringify(options("frame_limiter_provider", capabilities, null))),
    ["", ...capabilities.frame_limiter_provider],
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(options("rtss_frame_limit_type", capabilities, null))),
    ["", ...capabilities.rtss_frame_limit_type],
  );

  const parse = context.Amaterasu.parseCatalogFrameLimiterOverrides;
  const fields = new FormData();
  const providerFromHost = capabilities.frame_limiter_provider.at(-1);
  const rtssFromHost = capabilities.rtss_frame_limit_type.at(-1);
  fields.set("frame_limiter_provider", providerFromHost);
  fields.set("rtss_frame_limit_type", rtssFromHost);
  const parsed = JSON.parse(JSON.stringify(parse(fields, capabilities)));
  assert.equal(parsed.frame_limiter_provider, providerFromHost);
  assert.equal(parsed.rtss_frame_limit_type, rtssFromHost);

  for (const [name, value] of [
    ["frame_limiter_provider", "provider-not-returned-by-host"],
    ["rtss_frame_limit_type", "rtss-mode-not-returned-by-host"],
  ]) {
    const unsupported = new FormData();
    unsupported.set(name, value);
    assert.throws(() => parse(unsupported, capabilities), /supported values/);
  }
});

test("Catalog disables unavailable RTSS choices and preserves saved unsupported values explicitly", () => {
  const capabilities = {
    frame_limiter_provider: ["host-provider"],
    rtss_frame_limit_type: [],
  };
  const isDisabled = context.Amaterasu.catalogFrameLimiterOverrideIsDisabled;
  const optionValues = context.Amaterasu.catalogFrameLimiterOverrideOptionValues;
  const parse = context.Amaterasu.parseCatalogFrameLimiterOverrides;
  assert.equal(isDisabled("rtss_frame_limit_type", capabilities), true);
  assert.equal(isDisabled("frame_limiter_provider", capabilities), false);
  assert.deepEqual(
    JSON.parse(JSON.stringify(optionValues("rtss_frame_limit_type", capabilities, null))),
    [""],
  );

  const emptyChoices = new FormData();
  assert.equal(parse(emptyChoices, capabilities).rtss_frame_limit_type, null);
  emptyChoices.set("rtss_frame_limit_type", "new-unsupported-rtss-value");
  assert.throws(() => parse(emptyChoices, capabilities), /supported values/);

  const savedProvider = "previous-provider-choice";
  const savedRtss = "previous-rtss-choice";
  const existingOverrides = {
    frame_limiter_provider: savedProvider,
    rtss_frame_limit_type: savedRtss,
  };
  const providerOptions = optionValues(
    "frame_limiter_provider",
    capabilities,
    savedProvider,
  );
  const rtssOptions = optionValues(
    "rtss_frame_limit_type",
    capabilities,
    savedRtss,
  );
  const providerPreserveToken = providerOptions.at(-1);
  const rtssPreserveToken = rtssOptions.at(-1);
  assert.notEqual(providerPreserveToken, savedProvider);
  assert.notEqual(rtssPreserveToken, savedRtss);
  assert.equal(providerOptions.includes(savedProvider), false);
  assert.equal(rtssOptions.includes(savedRtss), false);

  const unchanged = new FormData();
  unchanged.set("frame_limiter_provider", providerPreserveToken);
  unchanged.set("rtss_frame_limit_type", rtssPreserveToken);
  const preserved = JSON.parse(
    JSON.stringify(parse(unchanged, capabilities, existingOverrides)),
  );
  assert.equal(preserved.frame_limiter_provider, savedProvider);
  assert.equal(preserved.rtss_frame_limit_type, savedRtss);

  unchanged.set("frame_limiter_provider", "");
  unchanged.set("rtss_frame_limit_type", "");
  const inherited = JSON.parse(
    JSON.stringify(parse(unchanged, capabilities, existingOverrides)),
  );
  assert.equal(inherited.frame_limiter_provider, null);
  assert.equal(inherited.rtss_frame_limit_type, null);

  const newlySelectedUnsupported = new FormData();
  newlySelectedUnsupported.set("frame_limiter_provider", savedProvider);
  newlySelectedUnsupported.set("rtss_frame_limit_type", savedRtss);
  assert.throws(
    () => parse(newlySelectedUnsupported, capabilities, existingOverrides),
    /supported values/,
  );
});

test("Steam artwork chunks assemble into the expected image without a local path", async () => {
  const source = Buffer.alloc(8195, 0x5a);
  const offsets = [];
  const api = {
    async post(path, request) {
      assert.equal(path, "steam/artwork_read");
      assert.equal(request.app_id, 730);
      assert.equal(request.revision, "42");
      offsets.push(request.offset);
      const end = Math.min(request.offset + 8192, source.length);
      return {
        ok: true,
        app_id: 730,
        revision: "42",
        offset: request.offset,
        next_offset: end === source.length ? null : end,
        total_bytes: source.length,
        mime_type: "image/png",
        data: source.subarray(request.offset, end).toString("base64"),
      };
    },
  };
  const image = await readSteamArtwork(api, reference);
  assert.deepEqual(offsets, [0, 8192]);
  assert.equal(image.type, "image/png");
  assert.deepEqual(Buffer.from(await image.arrayBuffer()), source);
});

test("Steam artwork transfer stops on changed revision and invalid cursors", async () => {
  const api = {
    async post(_path, request) {
      return {
        ok: true,
        app_id: 730,
        revision: request.offset ? "43" : "42",
        offset: request.offset,
        next_offset: request.offset ? null : 8192,
        total_bytes: 8193,
        mime_type: "image/png",
        data: Buffer.alloc(request.offset ? 1 : 8192).toString("base64"),
      };
    },
  };
  await assert.rejects(readSteamArtwork(api, reference), /changed during transfer/);
  await assert.rejects(readSteamArtwork(api, { ...reference, revision: "../art" }), /Invalid Steam artwork reference/);
  const invalidCursor = {
    post: async () => ({
      ok: true, app_id: 730, revision: "42", offset: 0,
      next_offset: 1, total_bytes: 8193, mime_type: "image/png",
      data: Buffer.alloc(8192).toString("base64"),
    }),
  };
  await assert.rejects(readSteamArtwork(invalidCursor, reference), /Invalid Steam artwork cursor/);
});

test("Steam artwork accepts a 16 MiB transfer declaration but rejects larger images", async () => {
  let calls = 0;
  const api = {
    async post(_path, request) {
      calls++;
      return {
        ok: true, app_id: 730, revision: request.offset ? "43" : "42",
        offset: request.offset, next_offset: 8192,
        total_bytes: 16 * 1024 * 1024,
        mime_type: "image/jpeg",
        data: Buffer.alloc(8192).toString("base64"),
      };
    },
  };
  await assert.rejects(readSteamArtwork(api, reference), /changed during transfer/);
  assert.equal(calls, 2);
  calls = 0;
  const oversized = { post: async (path, request) => ({
    ...await api.post(path, request), total_bytes: 16 * 1024 * 1024 + 1,
  }) };
  await assert.rejects(readSteamArtwork(oversized, reference), /changed during transfer/);
  assert.equal(calls, 1);
});

test("Lutris artwork reads signed 8 KiB chunks for the catalog digest", async () => {
  const source = Buffer.alloc(8197, 0x51);
  const offsets = [];
  const api = { async post(route, request) {
    assert.equal(route, "lutris/artwork_read");
    assert.equal(request.game_id, lutrisReference.game_id);
    assert.equal(request.revision, lutrisReference.revision);
    offsets.push(request.offset);
    const end = Math.min(request.offset + 8192, source.length);
    return {
      ok: true, game_id: request.game_id, revision: request.revision,
      offset: request.offset, next_offset: end === source.length ? null : end,
      total_bytes: source.length, mime_type: "image/webp",
      data: source.subarray(request.offset, end).toString("base64"),
    };
  } };
  const image = await readLutrisArtwork(api, lutrisReference);
  assert.deepEqual(offsets, [0, 8192]);
  assert.equal(image.type, "image/webp");
  assert.deepEqual(Buffer.from(await image.arrayBuffer()), source);
});

test("Lutris artwork rejects malformed references, changed digests, cursors and size", async () => {
  const valid = {
    ok: true, game_id: 42, revision: lutrisReference.revision,
    offset: 0, next_offset: null, total_bytes: 1,
    mime_type: "image/png", data: Buffer.from([0x51]).toString("base64"),
  };
  for (const invalid of [
    { ...lutrisReference, game_id: 0 },
    { ...lutrisReference, game_id: Number.MAX_SAFE_INTEGER + 1 },
    { ...lutrisReference, revision: "A".repeat(64) },
    { ...lutrisReference, revision: "a".repeat(63) },
  ]) {
    await assert.rejects(readLutrisArtwork({ post: () => { throw new Error("request sent"); } }, invalid), /Invalid Lutris artwork reference/);
  }
  for (const reply of [
    { ...valid, ok: false, code: "artwork_changed" },
    { ...valid, game_id: 43 },
    { ...valid, revision: "b".repeat(64) },
    { ...valid, offset: 8192 },
    { ...valid, total_bytes: 16 * 1024 * 1024 + 1 },
    { ...valid, mime_type: "text/html" },
    { ...valid, data: "!!!" },
    { ...valid, data: Buffer.alloc(8193).toString("base64"), total_bytes: 8193 },
    { ...valid, next_offset: 1, total_bytes: 2 },
    { ...valid, next_offset: null, total_bytes: 2 },
  ]) {
    await assert.rejects(readLutrisArtwork({ post: async () => reply }, lutrisReference));
  }
  const changedSecondChunk = { async post(_route, request) {
    return request.offset === 0
      ? { ...valid, total_bytes: 8193, next_offset: 8192, data: Buffer.alloc(8192).toString("base64") }
      : { ...valid, offset: 8192, total_bytes: 8193, revision: "b".repeat(64) };
  } };
  await assert.rejects(readLutrisArtwork(changedSecondChunk, lutrisReference), /changed during transfer/);
});

test("Lutris card refresh ignores stale A and revokes B on load or removal", async () => {
  class Element {
    constructor() { this.isConnected = true; this.children = []; this.listeners = new Map(); }
    prepend(child) { this.children.unshift(child); }
    addEventListener(type, listener) { this.listeners.set(type, listener); }
    fire(type) { this.listeners.get(type)?.(); }
  }
  const created = [];
  const revoked = [];
  const observers = [];
  const artworkContext = vm.createContext({
    atob, Blob, Error,
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
      assert.equal(route, "lutris/artwork_read");
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
  pending.get(b.revision)({
    ok: true, game_id: 42, revision: b.revision, offset: 0,
    next_offset: null, total_bytes: 1, mime_type: "image/png", data: "UQ==",
  });
  await new Promise(setImmediate);
  pending.get(lutrisReference.revision)({
    ok: true, game_id: 42, revision: lutrisReference.revision, offset: 0,
    next_offset: null, total_bytes: 1, mime_type: "image/png", data: "UQ==",
  });
  await new Promise(setImmediate);
  assert.equal(oldArt.children.length, 0);
  assert.equal(newArt.children.length, 1);
  assert.equal(created.length, 1);
  newArt.children[0].fire("load");
  assert.deepEqual(revoked, ["blob:1"]);

  const removedArt = new Element();
  show(api, b, removedArt, content);
  pending.get(b.revision)({
    ok: true, game_id: 42, revision: b.revision, offset: 0,
    next_offset: null, total_bytes: 1, mime_type: "image/png", data: "UQ==",
  });
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
      assert.equal(route, "steam/artwork_read");
      return {
        ok: true, app_id: request.app_id, revision: request.revision,
        offset: 0, next_offset: null, total_bytes: 1,
        mime_type: "image/png", data: "UQ==",
      };
    },
  }, reference, steamArt, content);
  await new Promise(setImmediate);
  assert.equal(steamArt.children.length, 1);
  steamArt.children[0].fire("error");
  assert.deepEqual(revoked, ["blob:1", "blob:2", "blob:3"]);
});
