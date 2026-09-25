import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ atob, Blob, Error });
for (const source of ["src/model.ts", "src/catalog.ts"]) {
  vm.runInContext(
    ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    context,
  );
}
const readSteamArtwork = context.Amaterasu.readSteamArtwork;
const reference = { provider: "steam", app_id: 730, revision: "42" };
const windowsCapabilities = {
  frame_limiter_provider: ["auto", "rtss", "nvidia-control-panel"],
  rtss_frame_limit_type: [
    "async",
    "front edge sync",
    "back edge sync",
    "nvidia reflex",
  ],
};

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
    rtss_frame_limit_type: "",
    ds4_back_as_touchpad_click: "",
  });

  const existingOverrides = {
    frame_limiter_enable: false,
    frame_limiter_fps_limit: 0,
    frame_limiter_provider: "auto",
    frame_limiter_disable_vsync: null,
    frame_limiter_auto_virtual_framegen: null,
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
    "ds4_back_as_touchpad_click",
  ]) {
    fields.set(name, "true");
    assert.equal(parse(fields, windowsCapabilities)[name], true);
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
      /Frame limiter|VSync|DS4 Back|Auto virtual|RTSS frame limit/,
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
