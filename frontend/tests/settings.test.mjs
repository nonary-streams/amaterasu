import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ TextEncoder, URL });
vm.runInContext(
  ts.transpile(readFileSync("src/settings.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
vm.runInContext(
  ts.transpile(readFileSync("src/pages.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
const parse = context.Amaterasu.parseSteamExclusions;
const ordinary = (value) => JSON.parse(JSON.stringify(value));

test("authentication lifetime editor accepts bounded whole seconds", () => {
  const parseAuth = context.Amaterasu.parseAuthTokenLifetimes;
  assert.deepEqual(ordinary(parseAuth(7200, 604800)), {
    session_token_ttl_seconds: 7200,
    remember_me_refresh_token_ttl_seconds: 604800,
  });
  assert.deepEqual(ordinary(parseAuth(1, 0x7fff_ffff)), {
    session_token_ttl_seconds: 1,
    remember_me_refresh_token_ttl_seconds: 0x7fff_ffff,
  });
  for (const [session, refresh] of [
    [0, 60],
    [1.5, 60],
    [Number.NaN, 60],
    [0x8000_0000, 60],
    [60, 0],
    [60, 1.5],
    [60, 0x8000_0000],
  ]) {
    assert.throws(() => parseAuth(session, refresh));
  }
});

test("authentication editor signs a revisioned update and confirms persisted readback", async () => {
  const calls = [];
  let stored = {
    ok: true,
    revision: 7,
    values: {
      session_token_ttl_seconds: 7200,
      remember_me_refresh_token_ttl_seconds: 604800,
    },
  };
  const api = {
    async post(route, body = {}) {
      calls.push([route, ordinary(body)]);
      if (route === "auth/settings") return ordinary(stored);
      assert.equal(route, "auth/settings_update");
      assert.deepEqual(ordinary(body), {
        session_token_ttl_seconds: 1,
        remember_me_refresh_token_ttl_seconds: 2147483647,
        expected_revision: 7,
      });
      stored = {
        ok: true,
        revision: stored.revision + 1,
        values: {
          session_token_ttl_seconds: body.session_token_ttl_seconds,
          remember_me_refresh_token_ttl_seconds: body.remember_me_refresh_token_ttl_seconds,
        },
      };
      return ordinary(stored);
    },
  };
  const initial = await api.post("auth/settings");
  const saved = await context.Amaterasu.saveAuthTokenLifetimes(api, initial.revision, 1, 2147483647);
  assert.deepEqual(ordinary(saved), stored);
  assert.deepEqual(calls.map(([route]) => route), ["auth/settings", "auth/settings_update", "auth/settings"]);
  // A newly loaded editor requests the authoritative values again.
  assert.deepEqual(await api.post("auth/settings"), stored);
});

test("authentication editor reports conflicts and unconfirmed writes", async () => {
  const conflictCalls = [];
  const conflict = {
    async post(route) {
      conflictCalls.push(route);
      assert.equal(route, "auth/settings_update");
      throw new Error("revision_conflict");
    },
  };
  await assert.rejects(
    context.Amaterasu.saveAuthTokenLifetimes(conflict, 4, 60, 3600),
    /revision_conflict/,
  );
  assert.deepEqual(conflictCalls, ["auth/settings_update"]);
  const stale = {
    async post(route, body) {
      if (route === "auth/settings_update") {
        assert.equal(body.expected_revision, 4);
        return { ok: true, revision: 5,
          values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 } };
      }
      assert.equal(route, "auth/settings");
      return {
        ok: true,
        revision: 4,
        values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 },
      };
    },
  };
  await assert.rejects(
    context.Amaterasu.saveAuthTokenLifetimes(stale, 4, 60, 3600),
    /could not be confirmed/,
  );
  for (const readback of [
    { ok: true, revision: 5, values: { session_token_ttl_seconds: 61, remember_me_refresh_token_ttl_seconds: 3600 } },
    { ok: true, revision: 6, values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 } },
    { ok: true, revision: "5", values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 } },
    null,
  ]) {
    const api = {
      async post(route) {
        return route === "auth/settings_update" ? {
          ok: true, revision: 5,
          values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 },
        } : readback;
      },
    };
    await assert.rejects(
      context.Amaterasu.saveAuthTokenLifetimes(api, 4, 60, 3600),
      /could not be confirmed|reply is invalid/,
    );
  }
  for (const rejected of [
    { ok: false, error: { code: "revision_conflict" } },
    { ok: true, revision: 6, values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 } },
    { ok: true, revision: 5, values: { session_token_ttl_seconds: 61, remember_me_refresh_token_ttl_seconds: 3600 } },
    { ok: true, revision: 5, values: { session_token_ttl_seconds: 0, remember_me_refresh_token_ttl_seconds: 3600 } },
  ]) {
    const calls = [];
    await assert.rejects(context.Amaterasu.saveAuthTokenLifetimes({ async post(route) {
      calls.push(route);
      return rejected;
    } }, 4, 60, 3600));
    assert.deepEqual(calls, ["auth/settings_update"]);
  }
});

test("authentication editor discards old loads and failures after refresh or save starts", async () => {
  const gate = new context.Amaterasu.AuthTokenSettingsLoadGate();
  const pending = [];
  const api = { post(route) {
    assert.equal(route, "auth/settings");
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  } };
  const old = gate.read(api);
  const latest = gate.read(api);
  const settings = {
    ok: true, revision: 8,
    values: { session_token_ttl_seconds: 60, remember_me_refresh_token_ttl_seconds: 3600 },
  };
  pending[1].resolve(settings);
  assert.equal((await latest).revision, 8);
  pending[0].resolve({ ...settings, revision: 7 });
  assert.equal(await old, null);

  const duringSave = gate.read(api);
  gate.invalidate();
  pending[2].reject(new Error("old load failed"));
  assert.equal(await duringSave, null);
  const reloaded = gate.read(api);
  pending[3].resolve({ ...settings, revision: 9 });
  assert.equal((await reloaded).revision, 9);
});

function registeredSettings(revision = 4) {
  return {
    ok: true,
    revision,
    fields: {
      notify_pre_releases: { type: "boolean", default: false },
      update_check_interval: { type: "integer", default: 1440, minimum: 1 },
      unrelated: { type: "object", default: { state: "old" }, fields: {} },
    },
    values: {
      notify_pre_releases: false,
      update_check_interval: 1440,
      unrelated: { state: "kept", options: [1, 2] },
    },
  };
}

test("registered settings editor confirms signed update and durable readback without changing other fields", async () => {
  const original = registeredSettings();
  let stored = original;
  const routes = [];
  const api = { async post(route, payload = {}) {
    routes.push(route);
    if (route === "configuration/get") return ordinary(stored);
    assert.equal(route, "configuration/update");
    assert.deepEqual(ordinary(payload), {
      expected_revision: 4, values: { notify_pre_releases: true },
    });
    stored = {
      ...original,
      revision: 5,
      values: {
        unrelated: { options: [1, 2], state: "kept" },
        update_check_interval: 1440,
        notify_pre_releases: true,
      },
    };
    return ordinary(stored);
  } };
  const saved = await context.Amaterasu.saveRegisteredConfiguration(api, original, {
    notify_pre_releases: true,
  });
  assert.deepEqual(ordinary(saved), stored);
  assert.deepEqual(routes, ["configuration/update", "configuration/get"]);
});

test("registered settings editor rejects a denied or mismatched update and concurrent readback", async () => {
  const original = registeredSettings();
  const expected = { ...original, revision: 5,
    values: { ...original.values, notify_pre_releases: true } };
  for (const rejected of [
    { ok: false, error: { code: "revision_conflict" } },
    { ...expected, revision: 6 },
    { ...expected, fields: { ...expected.fields, update_check_interval: { type: "integer", default: 20 } } },
    { ...expected, values: { ...expected.values, update_check_interval: 20 } },
    { ...expected, values: { notify_pre_releases: true } },
  ]) {
    const routes = [];
    await assert.rejects(context.Amaterasu.saveRegisteredConfiguration({ async post(route) {
      routes.push(route);
      return rejected;
    } }, original, { notify_pre_releases: true }));
    assert.deepEqual(routes, ["configuration/update"]);
  }
  for (const stale of [
    original,
    { ...expected, revision: 6 },
    { ...expected, values: { ...expected.values, unrelated: { state: "changed" } } },
  ]) {
    const routes = [];
    await assert.rejects(context.Amaterasu.saveRegisteredConfiguration({ async post(route) {
      routes.push(route);
      return route === "configuration/update" ? expected : stale;
    } }, original, { notify_pre_releases: true }), /could not be confirmed/);
    assert.deepEqual(routes, ["configuration/update", "configuration/get"]);
  }
});

test("registered settings editor ignores older loads and failures after refresh or save", async () => {
  const gate = new context.Amaterasu.RegisteredConfigurationLoadGate();
  const pending = [];
  const api = { post(route) {
    assert.equal(route, "configuration/get");
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  } };
  const old = gate.read(api);
  const latest = gate.read(api);
  pending[1].resolve(registeredSettings(8));
  assert.equal((await latest).revision, 8);
  pending[0].resolve(registeredSettings(7));
  assert.equal(await old, null);
  const duringSave = gate.read(api);
  gate.invalidate();
  pending[2].reject(new Error("old request failed"));
  assert.equal(await duringSave, null);
  const reloaded = gate.read(api);
  pending[3].resolve(registeredSettings(9));
  assert.equal((await reloaded).revision, 9);
});

function hostConfiguration(revision = 6) {
  return {
    ok: true,
    ready: true,
    version: 1,
    revision,
    values: {
      file_state: "existing/state.json",
      vibeshine_file_state: "old/state.json",
      credentials_file: "",
      pkey: "credentials/key.pem",
      cert: "credentials/cert.pem",
      log_path: "amaterasu.log",
      min_log_level: "product_default",
    },
    effective: { log_path: "C:/state/amaterasu.log", min_log_level: "info" },
  };
}

test("host logging editor preserves other paths under CAS and confirms signed readback", async () => {
  const original = hostConfiguration();
  const calls = [];
  let stored = original;
  const api = {
    async post(route, body = {}) {
      calls.push([route, ordinary(body)]);
      if (route === "host-configuration/get") return ordinary(stored);
      assert.equal(route, "host-configuration/update");
      assert.deepEqual(ordinary(body), {
        ...original.values,
        expected_revision: 6,
        log_path: "logs/diagnostics.log",
        min_log_level: "warning",
      });
      stored = {
        ...original,
        revision: 7,
        values: { ...original.values, log_path: body.log_path, min_log_level: body.min_log_level },
        effective: { log_path: "C:/state/logs/diagnostics.log", min_log_level: "warning" },
      };
      return ordinary(stored);
    },
  };
  const current = context.Amaterasu.parseHostConfigurationReply(await api.post("host-configuration/get"));
  const saved = await context.Amaterasu.saveHostLoggingSettings(api, current, "warning", "logs/diagnostics.log");
  assert.deepEqual(ordinary(saved), stored);
  assert.deepEqual(calls.map(([route]) => route), [
    "host-configuration/get", "host-configuration/update", "host-configuration/get",
  ]);
  assert.deepEqual(await api.post("host-configuration/get"), stored);
});

test("host logging editor rejects invalid values, stale revisions and changed unrelated paths", async () => {
  const parse = context.Amaterasu.parseHostLoggingSettings;
  assert.deepEqual(ordinary(parse("none", "logs/service.log")), {
    min_log_level: "none", log_path: "logs/service.log",
  });
  assert.equal(parse("info", "/").log_path, "/");
  for (const [level, path] of [
    ["trace", "service.log"], ["info", ""], ["info", "../escape.log"],
    ["info", "bad\npath"], ["info", "é".repeat(2049)],
    ["info", "./"], ["info", ".\\."], ["info", "C:relative.log"],
  ]) assert.throws(() => parse(level, path));
  for (const malformed of [
    null, { ...hostConfiguration(), ready: false },
    { ...hostConfiguration(), version: 2 },
    { ...hostConfiguration(), revision: "6" },
    { ...hostConfiguration(), values: { ...hostConfiguration().values, log_path: 5 } },
  ]) assert.throws(() => context.Amaterasu.parseHostConfigurationReply(malformed));
  for (const changed of [
    hostConfiguration(),
    { ...hostConfiguration(7), values: { ...hostConfiguration().values, log_path: "other.log" } },
    { ...hostConfiguration(7), values: { ...hostConfiguration().values, cert: "changed.pem", log_path: "new.log" } },
  ]) {
    const updated = {
      ...hostConfiguration(7),
      values: { ...hostConfiguration().values, log_path: "new.log" },
    };
    const api = {
      async post(route) {
        return route === "host-configuration/get" ? changed : updated;
      },
    };
    await assert.rejects(
      context.Amaterasu.saveHostLoggingSettings(api, hostConfiguration(), "product_default", "new.log"),
      /could not be confirmed/,
    );
  }
  const confirmed = {
    ...hostConfiguration(7),
    values: { ...hostConfiguration().values, log_path: "new.log" },
  };
  for (const rejected of [
    { ok: false, error: { code: "revision_conflict" } },
    hostConfiguration(6),
    { ...confirmed, revision: 8 },
    { ...confirmed, values: { ...confirmed.values, cert: "other/key.pem" } },
    { ...confirmed, values: { ...confirmed.values, log_path: "different.log" } },
  ]) {
    const routes = [];
    await assert.rejects(context.Amaterasu.saveHostLoggingSettings({ async post(route) {
      routes.push(route);
      return rejected;
    } }, hostConfiguration(), "product_default", "new.log"));
    assert.deepEqual(routes, ["host-configuration/update"]);
  }
});

test("host logging editor discards delayed loads and failures after refresh or save", async () => {
  const gate = new context.Amaterasu.HostLoggingLoadGate();
  const pending = [];
  const api = { post(route) {
    if (route === "diagnostics/status")
      return Promise.resolve({ ok: true, ready: true, logging_configuration_revision: 8 });
    assert.equal(route, "host-configuration/get");
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  } };
  const old = gate.read(api);
  const latest = gate.read(api);
  pending[1].resolve(hostConfiguration(8));
  assert.equal((await latest).settings.revision, 8);
  pending[0].resolve(hostConfiguration(7));
  assert.equal(await old, null);

  const duringSave = gate.read(api);
  gate.invalidate();
  pending[2].reject(new Error("old load failed"));
  assert.equal(await duringSave, null);
  const reloaded = gate.read(api);
  pending[3].resolve(hostConfiguration(9));
  assert.equal((await reloaded).settings.revision, 9);
});

test("diagnostics status distinguishes saved, pending, unavailable and newer logging revisions", () => {
  const parseStatus = context.Amaterasu.parseDiagnosticsLoggingStatus;
  const describe = context.Amaterasu.diagnosticsLoggingDescription;
  assert.match(describe(7, parseStatus({ ok: true, ready: true, logging_configuration_revision: 7 })), /using saved host logging revision 7/);
  assert.match(describe(7, parseStatus({ ok: true, ready: true, logging_configuration_revision: 6 })), /pending/);
  assert.match(describe(7, parseStatus({ ok: true, ready: true, logging_configuration_revision: null })), /pending/);
  assert.match(describe(7, parseStatus({ ok: true, ready: false, logging_configuration_revision: null })), /not ready/);
  assert.match(describe(7, parseStatus({ ok: true, ready: true, logging_configuration_revision: 8 })), /newer/);
  assert.match(describe(7, null), /unavailable/);
  for (const value of [null, {}, { ok: false, ready: true, logging_configuration_revision: 1 },
    { ok: true, ready: "yes", logging_configuration_revision: 1 },
    { ok: true, ready: true, logging_configuration_revision: 0 },
    { ok: true, ready: true, logging_configuration_revision: 1.5 },
    { ok: true, ready: true }]) assert.throws(() => parseStatus(value));
});

test("host logging save confirms signed readback and reports disk sink failure and recovery", async () => {
  const describe = context.Amaterasu.diagnosticsLoggingDescription;
  let stored = hostConfiguration();
  let sink = { state: "failed", failure: "open" };
  const calls = [];
  const api = { async post(route, body) {
    calls.push(route);
    if (route === "host-configuration/update") {
      assert.equal(body.expected_revision, stored.revision);
      stored = hostConfiguration(stored.revision + 1);
      stored.values = { ...stored.values, min_log_level: body.min_log_level, log_path: body.log_path };
      return stored;
    }
    if (route === "host-configuration/get") return stored;
    return { ok: true, ready: true, logging_configuration_revision: stored.revision, logging_sink: sink };
  } };
  const loaded = await context.Amaterasu.readHostLoggingState(api);
  assert.match(describe(loaded.settings.revision, loaded.diagnostics), /opening the disk log failed/);
  const saved = await context.Amaterasu.saveHostLoggingState(api, loaded.settings, "info", "logs/new.log");
  assert.equal(saved.settings.revision, 7);
  assert.match(describe(saved.settings.revision, saved.diagnostics), /opening the disk log failed/);
  assert.deepEqual(calls, ["host-configuration/get", "diagnostics/status", "host-configuration/update", "host-configuration/get", "diagnostics/status"]);
  sink = { state: "failed", failure: "write" };
  const writeFailure = await context.Amaterasu.readHostLoggingState(api);
  assert.match(describe(writeFailure.settings.revision, writeFailure.diagnostics), /writing the disk log failed/);
  sink = { state: "healthy", failure: null };
  const recovered = await context.Amaterasu.readHostLoggingState(api);
  assert.match(describe(recovered.settings.revision, recovered.diagnostics), /disk logging is healthy/);
  sink = { state: "disabled", failure: null };
  const disabled = await context.Amaterasu.readHostLoggingState(api);
  assert.match(describe(disabled.settings.revision, disabled.diagnostics), /disk logging is disabled/);
  assert.doesNotMatch([loaded, saved, writeFailure, recovered, disabled]
    .map(({ settings, diagnostics }) => describe(settings.revision, diagnostics)).join(" "), /logs\/new\.log|secret|password/);
});

test("diagnostics sink parser rejects malformed health and preserves legacy or unavailable status", async () => {
  const parse = context.Amaterasu.parseDiagnosticsLoggingStatus;
  const describe = context.Amaterasu.diagnosticsLoggingDescription;
  const base = { ok: true, ready: true, logging_configuration_revision: 7 };
  assert.match(describe(7, parse(base)), /disk delivery is unverified/);
  for (const logging_sink of [null, [], {}, { state: "failed", failure: null },
    { state: "failed", failure: "secret/path" }, { state: "healthy", failure: "write" },
    { state: "disabled", failure: "open" }, { state: "unknown", failure: null }]) {
    assert.throws(() => parse({ ...base, logging_sink }));
  }
  const settings = hostConfiguration();
  for (const status of [{ ...base, logging_sink: { state: "failed", failure: "secret/path" } }, null]) {
    const state = await context.Amaterasu.readHostLoggingState({ async post(route) {
      if (route === "host-configuration/get") return settings;
      if (status === null) throw new Error("scope denied");
      return status;
    } });
    assert.deepEqual(ordinary(state.settings), settings);
    assert.equal(state.diagnostics, null);
    assert.match(describe(settings.revision, state.diagnostics), /status is unavailable/);
  }
});

test("history editor confirms exact signed revision and settings readback", async () => {
  const current = { ok: true, revision: 4, settings: { enabled: true, ttl_days: 30, db_size_limit_mb: 50 }, effective_enabled: true };
  const calls = [];
  const saved = { ...current, revision: 5, settings: { enabled: false, ttl_days: 14, db_size_limit_mb: 20 }, effective_enabled: true };
  const api = { async post(route, body) {
    calls.push([route, ordinary(body ?? {})]);
    return saved;
  } };
  assert.deepEqual(ordinary(await context.Amaterasu.saveHistorySettings(api, current, false, 14, 20)), saved);
  assert.deepEqual(calls, [
    ["history/settings_update", { settings: saved.settings, expected_revision: 4 }],
    ["history/settings_get", {}],
  ]);
  for (const stale of [current, { ...saved, revision: 6 }, { ...saved, settings: { ...saved.settings, ttl_days: 15 } }]) {
    const staleApi = { async post(route) { return route === "history/settings_get" ? stale : saved; } };
    await assert.rejects(context.Amaterasu.saveHistorySettings(staleApi, current, false, 14, 20), /could not be confirmed/);
  }
  for (const rejected of [
    { ok: false, code: "revision_conflict", revision: 5 },
    { ok: false, code: "write_unresolved" },
    { ...saved, revision: 6 },
    { ...saved, settings: { ...saved.settings, ttl_days: 15 } },
  ]) {
    const routes = [];
    const rejectedApi = { async post(route) {
      routes.push(route);
      return rejected;
    } };
    await assert.rejects(context.Amaterasu.saveHistorySettings(rejectedApi, current, false, 14, 20));
    assert.deepEqual(routes, ["history/settings_update"]);
  }
});

test("history editor ignores stale loads and errors after a newer load or save begins", async () => {
  const gate = new context.Amaterasu.HistorySettingsLoadGate();
  const replies = [];
  const api = { post(route) {
    assert.equal(route, "history/settings_get");
    return new Promise((resolve, reject) => replies.push({ resolve, reject }));
  } };
  const old = gate.read(api);
  const latest = gate.read(api);
  const current = { ok: true, revision: 8, settings: { enabled: true, ttl_days: 14, db_size_limit_mb: 20 }, effective_enabled: true };
  replies[1].resolve(current);
  assert.equal((await latest).settings.revision, 8);
  replies[0].resolve({ ...current, revision: 7 });
  assert.equal(await old, null);

  const duringSave = gate.read(api);
  gate.invalidate();
  replies[2].reject(new Error("outdated request failed"));
  assert.equal(await duringSave, null);

  const reloaded = gate.read(api);
  replies[3].resolve({ ...current, revision: 9 });
  assert.equal((await reloaded).settings.revision, 9);
});

test("history editor confirms durable save and separately reads signed runtime on save and reload", async () => {
  let stored = { ok: true, revision: 4, settings: { enabled: false, ttl_days: 30, db_size_limit_mb: 50 }, effective_enabled: false };
  let runtime = {
    ok: true,
    status: {
      available: true, configuration_ready: true, enabled: false, effective_enabled: false,
      degraded: false, failed_operations: 0, active_streams: 1, sessions: 0, samples: 0,
      events: 0, live_bytes: 0,
      last_prune: { sessions_deleted: 0, live_bytes: 0, size_limit_bytes: 0, size_limit_unmet: false },
    },
  };
  const calls = [];
  const api = { async post(route, body) {
    calls.push([route, ordinary(body ?? {})]);
    if (route === "history/settings_update") {
      assert.equal(body.expected_revision, stored.revision);
      stored = { ...stored, revision: stored.revision + 1, settings: body.settings, effective_enabled: false };
      runtime = { ...runtime, status: { ...runtime.status, enabled: true } };
      return stored;
    }
    return route === "history/settings_get" ? stored : runtime;
  } };
  assert.equal(context.Amaterasu.parseHistoryRuntimeStatus(runtime).available, true);
  const initial = await context.Amaterasu.readHistorySettingsState(api);
  assert.deepEqual(calls, [["history/settings_get", {}], ["history/history_status", {}]]);
  assert.equal(initial.runtime?.available, true);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(initial.settings, initial.runtime), /recording is disabled/);
  const saved = await context.Amaterasu.saveHistorySettingsState(api, initial.settings, true, 14, 20);
  assert.equal(saved.settings.revision, 5);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(saved.settings, saved.runtime), /deferred until active streams end/);
  assert.deepEqual(calls.map(([route]) => route), [
    "history/settings_get", "history/history_status", "history/settings_update",
    "history/settings_get", "history/history_status",
  ]);
  runtime = { ...runtime, status: { ...runtime.status, effective_enabled: true, active_streams: 0 } };
  const reloaded = await context.Amaterasu.readHistorySettingsState(api);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(reloaded.settings, reloaded.runtime), /recording is active/);
  runtime = { ...runtime, status: { ...runtime.status, enabled: false } };
  const pending = await context.Amaterasu.readHistorySettingsState(api);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(pending.settings, pending.runtime), /has not yet reported/);
  runtime = { ...runtime, status: { ...runtime.status, enabled: true } };
  const notReady = context.Amaterasu.historySettingsRuntimeDescription(reloaded.settings, {
    ...reloaded.runtime, configuration_ready: false,
  });
  assert.match(notReady, /configuration is not ready/);
  runtime = { ...runtime, status: { ...runtime.status, available: false, effective_enabled: false } };
  const unavailable = await context.Amaterasu.readHistorySettingsState(api);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(unavailable.settings, unavailable.runtime), /storage is unavailable/);
  assert.doesNotMatch(context.Amaterasu.historySettingsRuntimeDescription(unavailable.settings, unavailable.runtime), /recording is active/);
  runtime = { ...runtime, status: { ...runtime.status, available: true, degraded: true } };
  const degraded = await context.Amaterasu.readHistorySettingsState(api);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(degraded.settings, degraded.runtime), /degraded/);
  runtime = { ...runtime, status: { ...runtime.status, available: "true" } };
  const malformed = await context.Amaterasu.readHistorySettingsState(api);
  assert.equal(malformed.runtime, null);
  assert.match(context.Amaterasu.historySettingsRuntimeDescription(malformed.settings, malformed.runtime), /status is unavailable/);
  const rejected = await context.Amaterasu.readHistorySettingsState({ async post(route) {
    if (route === "history/history_status") throw new Error("service unavailable");
    return stored;
  } });
  assert.equal(rejected.runtime, null);
});

test("virtual sink selector editor preserves unset and explicit empty distinctly", () => {
  const parseSelection = context.Amaterasu.parseVirtualSinkSelection;
  assert.equal(parseSelection(false, "unused value"), null);
  assert.equal(parseSelection(true, ""), "");
  assert.equal(parseSelection(true, "virtual-device-id"), "virtual-device-id");
  assert.throws(() => parseSelection(true, "bad\nselector"));
  assert.throws(() => parseSelection(true, "x".repeat(4097)));
});

test("input policy editor validates key repeat settings and reports cleanup honestly", () => {
  const parsePolicy = context.Amaterasu.parseInputPolicy;
  assert.deepEqual(ordinary(parsePolicy(false, true, false, false, true, 0, 0)), {
    keyboard: false,
    mouse: true,
    high_resolution_scrolling: false,
    always_send_scancodes: false,
    key_rightalt_to_key_win: true,
    key_repeat_delay: 0,
    key_repeat_frequency: 0,
    back_button_timeout: -1,
    gamepad: "auto",
    controller: true,
    native_pen_touch: true,
  });
  assert.deepEqual(
    ordinary(parsePolicy(true, true, true, true, false, 2147483647, 31.25, 2147483647)),
    {
      keyboard: true,
      mouse: true,
      high_resolution_scrolling: true,
      always_send_scancodes: true,
      key_rightalt_to_key_win: false,
      key_repeat_delay: 2147483647,
      key_repeat_frequency: 31.25,
      back_button_timeout: 2147483647,
      gamepad: "auto",
      controller: true,
      native_pen_touch: true,
    },
  );
  const parseNativePenTouch = (nativePenTouch) =>
    parsePolicy(true, true, true, true, false, 500, 24.9, -1, "auto", ["auto"], true, nativePenTouch);
  assert.equal(ordinary(parseNativePenTouch(true)).native_pen_touch, true);
  assert.equal(ordinary(parseNativePenTouch(false)).native_pen_touch, false);
  for (const invalidNativePenTouch of [null, 0, 1, "false", [], {}]) {
    assert.throws(() => parseNativePenTouch(invalidNativePenTouch));
  }
  assert.equal(
    ordinary(parsePolicy(true, true, true, true, false, 0, 0, -2147483648))
      .back_button_timeout,
    -2147483648,
  );
  assert.equal(
    ordinary(parsePolicy(true, true, true, true, false, 0, 0, 0)).back_button_timeout,
    0,
  );
  assert.equal(
    ordinary(parsePolicy(true, true, true, true, false, 500, 24.9, -1, "auto", ["auto"], false))
      .controller,
    false,
  );
  assert.throws(() => parsePolicy(true, true, true, true, false, 500, 24.9, -1, "auto", ["auto"], "false"));
  for (const values of [
    ["false", true, true, true, true, 500, 24.9],
    [true, true, "false", true, true, 500, 24.9],
    [true, true, true, "false", true, 500, 24.9],
    [true, true, true, true, "false", 500, 24.9],
    [true, true, true, true, true, -1, 24.9],
    [true, true, true, true, true, 1.5, 24.9],
    [true, true, true, true, true, 2147483648, 24.9],
    [true, true, true, true, true, 500, -0.1],
    [true, true, true, true, true, 500, Number.NaN],
    [true, true, true, true, true, 500, Number.POSITIVE_INFINITY],
    [true, true, true, true, true, 500, 24.9, -2147483649],
    [true, true, true, true, true, 500, 24.9, 2147483648],
    [true, true, true, true, true, 500, 24.9, 1.5],
    [true, true, true, true, true, 500, 24.9, "100"],
    [true, true, true, true, true, 500, 24.9, Number.NaN],
  ]) {
    assert.throws(() => parsePolicy(...values));
  }
  const describe = context.Amaterasu.inputPolicyStatusDescription;
  const settings = {
    persisted: true,
    applied: true,
    cleanup_required: false,
    revision: 4,
    values: { keyboard: false, mouse: true },
  };
  assert.match(describe({ ...settings, persisted: false }), /not been confirmed/);
  assert.match(describe({ ...settings, applied: false, cleanup_required: true }), /cleanup.*pending/);
  assert.match(describe(settings), /policy is active/);
});

test("gamepad editor uses backend profiles and labels a selected unavailable profile", () => {
  const choices = context.Amaterasu.inputGamepadProfileChoices;
  assert.deepEqual(ordinary(choices("ds4", ["auto", "x360"])), [
    { value: "auto", label: "auto", available: true },
    { value: "x360", label: "x360", available: true },
    {
      value: "ds4",
      label: "ds4 (recognized, unavailable on this backend)",
      available: false,
    },
  ]);
  assert.deepEqual(ordinary(choices("xone", ["auto", "xone"])), [
    { value: "auto", label: "auto", available: true },
    { value: "xone", label: "xone", available: true },
  ]);
  assert.deepEqual(ordinary(choices("xone", ["auto"], ["xone"])), [
    { value: "xone", label: "xone (recognized, unavailable on this backend)", available: false },
    { value: "auto", label: "auto", available: true },
  ]);

  const parsePolicy = context.Amaterasu.parseInputPolicy;
  assert.equal(
    ordinary(parsePolicy(true, true, true, true, false, 500, 24.9, -1, "ds4", ["auto", "x360", "ds4"]))
      .gamepad,
    "ds4",
  );
  assert.throws(() => parsePolicy(true, true, true, true, false, 500, 24.9, -1, "switch", ["auto", "x360", "ds4"]));
  assert.deepEqual(ordinary(choices("", ["auto", "x360"], ["auto", "x360", "vhf_ds4"])), [
    { value: "auto", label: "auto", available: true },
    { value: "x360", label: "x360", available: true },
    { value: "vhf_ds4", label: "vhf_ds4 (recognized, unavailable on this backend)", available: false },
  ]);

  const describe = context.Amaterasu.inputGamepadStatusDescription;
  assert.match(
    describe("ds4", {
      gamepad: "ds4",
      supported_profiles: ["auto", "x360"],
      recognized_profiles: ["auto", "x360", "ds4"],
      effective_gamepad_profile: null,
      gamepad_unavailable_reason: "ViGEm profile is not implemented",
      controller_input: false,
      controller_enabled: true,
      controller_cleanup_required: false,
      lease: {},
    }),
    /recognized but unavailable.*not implemented/,
  );
  assert.match(
    describe("auto", {
      gamepad: "auto",
      supported_profiles: ["auto", "x360"],
      recognized_profiles: ["auto", "x360", "ds4"],
      effective_gamepad_profile: null,
      gamepad_unavailable_reason: null,
      controller_input: false,
      controller_enabled: true,
      controller_cleanup_required: false,
      lease: null,
    }),
    /availability is checked when a stream starts/,
  );
  assert.match(
    describe("auto", {
      gamepad: "auto",
      supported_profiles: ["auto", "x360"],
      recognized_profiles: ["auto", "x360", "ds4"],
      effective_gamepad_profile: null,
      gamepad_unavailable_reason: null,
      controller_input: false,
      controller_enabled: false,
      controller_cleanup_required: true,
      lease: {},
    }),
    /disabled; virtual gamepad removal is retrying.*keyboard and mouse input remain available/i,
  );
});

test("history settings preserve disabled pruning and enforce host integer bounds", () => {
  const parseHistory = context.Amaterasu.parseHistorySettings;
  assert.deepEqual(ordinary(parseHistory(true, 0, 0)), {
    enabled: true,
    ttl_days: 0,
    db_size_limit_mb: 0,
  });
  assert.deepEqual(ordinary(parseHistory(false, 0x7fff_ffff, 1)), {
    enabled: false,
    ttl_days: 0x7fff_ffff,
    db_size_limit_mb: 1,
  });
  for (const values of [
    ["true", 0, 0],
    [true, -1, 0],
    [true, 1.5, 0],
    [true, Number.NaN, 0],
    [true, 0x8000_0000, 0],
    [true, 0, -1],
    [true, 0, 1.5],
    [true, 0, 0x8000_0000],
  ]) {
    assert.throws(() => parseHistory(...values));
  }
});

test("session history readback validates metadata and preserves configured and effective state", () => {
  const parseReply = context.Amaterasu.parseHistorySettingsReply;
  const deferredDisable = {
    ok: true,
    settings: { enabled: false, ttl_days: 0, db_size_limit_mb: 0 },
    revision: 0,
    effective_enabled: true,
  };
  assert.deepEqual(ordinary(parseReply(deferredDisable)), deferredDisable);

  const disabled = {
    ok: true,
    settings: { enabled: false, ttl_days: 2147483647, db_size_limit_mb: 0 },
    revision: 42,
    effective_enabled: false,
  };
  assert.deepEqual(ordinary(parseReply(disabled)), disabled);

  for (const invalid of [
    null,
    undefined,
    [],
    false,
    { ...deferredDisable, ok: false },
    { ...deferredDisable, ok: "true" },
    { ...deferredDisable, settings: null },
    { ...deferredDisable, settings: [] },
    { ...deferredDisable, settings: { ...deferredDisable.settings, enabled: "false" } },
    { ...deferredDisable, settings: { ...deferredDisable.settings, ttl_days: -1 } },
    { ...deferredDisable, settings: { ...deferredDisable.settings, ttl_days: 1.5 } },
    { ...deferredDisable, settings: { ...deferredDisable.settings, ttl_days: 2147483648 } },
    { ...deferredDisable, settings: { ...deferredDisable.settings, db_size_limit_mb: "0" } },
    { ...deferredDisable, settings: { ...deferredDisable.settings, db_size_limit_mb: -1 } },
    { ...deferredDisable, settings: { ...deferredDisable.settings, db_size_limit_mb: 2147483648 } },
    { ...deferredDisable, revision: -1 },
    { ...deferredDisable, revision: 1.5 },
    { ...deferredDisable, revision: Number.MAX_SAFE_INTEGER + 1 },
    { ...deferredDisable, revision: "0" },
    { ...deferredDisable, effective_enabled: "true" },
  ]) {
    assert.throws(() => parseReply(invalid));
  }
});

test("Playnite editor validates complete sparse settings and bounded filter lists", () => {
  const parseSettings = context.Amaterasu.parsePlayniteSettings;
  const values = {
    enabled: true,
    auto_sync: false,
    sync_all_installed: true,
    autosync_remove_uninstalled: false,
    autosync_delete_after_days: 14,
    autosync_require_replacement: true,
    focus_attempts: 3,
    focus_timeout_secs: 15,
    focus_exit_on_first: false,
    fullscreen_entry_enabled: true,
    recent_games: 0,
    recent_max_age_days: 36500,
    sync_categories: ["Action"],
    exclude_categories: [],
    sync_plugins: ["plugin.example"],
    exclude_plugins: [],
    exclude_games: ["game-id"],
  };
  assert.deepEqual(ordinary(parseSettings(values)), values);
  assert.deepEqual(
    ordinary(context.Amaterasu.parsePlayniteFilterList('["Action","RPG"]', "Categories")),
    ["Action", "RPG"],
  );
  assert.deepEqual(
    ordinary(context.Amaterasu.parsePlayniteFilterList(JSON.stringify(["é".repeat(256)]), "Filters")),
    ["é".repeat(256)],
  );
  assert.deepEqual(
    ordinary(context.Amaterasu.parsePlayniteFilterList('["Action","action"]', "Filters")),
    ["Action", "action"],
  );
  for (const source of [
    "not JSON",
    "{}",
    JSON.stringify(["  "]),
    JSON.stringify(["bad\nname"]),
    JSON.stringify(["é".repeat(257)]),
    JSON.stringify([1]),
    JSON.stringify(Array.from({ length: 257 }, (_, index) => `filter-${index}`)),
  ]) {
    assert.throws(() => context.Amaterasu.parsePlayniteFilterList(source, "Filters"));
  }
  for (const invalid of [
    { ...values, recent_games: -1 },
    { ...values, recent_games: 10001 },
    { ...values, recent_max_age_days: 36501 },
    { ...values, enabled: "true" },
    { ...values, autosync_remove_uninstalled: "false" },
    { ...values, autosync_delete_after_days: -1 },
    { ...values, autosync_delete_after_days: 2_147_483_648 },
    { ...values, autosync_delete_after_days: 1.5 },
    { ...values, autosync_require_replacement: "true" },
    { ...values, focus_attempts: -1 },
    { ...values, focus_attempts: 2_147_483_648 },
    { ...values, focus_timeout_secs: 1.5 },
    { ...values, focus_exit_on_first: "false" },
    { ...values, fullscreen_entry_enabled: "true" },
    { ...values, exclude_games: Array.from({ length: 256 }, (_, index) => `game-${index}-${"x".repeat(500)}`) },
  ]) {
    assert.throws(() => parseSettings(invalid));
  }
});

test("Playnite save confirms the signed update and exact durable settings", async () => {
  const values = {
    enabled: true, auto_sync: true, sync_all_installed: false,
    autosync_remove_uninstalled: true, autosync_delete_after_days: 14,
    autosync_require_replacement: true, focus_attempts: 3,
    focus_timeout_secs: 15, focus_exit_on_first: false,
    fullscreen_entry_enabled: false, recent_games: 10,
    recent_max_age_days: 30, sync_categories: [], exclude_categories: [],
    sync_plugins: [], exclude_plugins: [], exclude_games: [],
  };
  const current = { ok: true, revision: 7, values };
  const requested = { ...values, recent_games: 120, exclude_games: ["first", "FIRST"] };
  const next = { ok: true, revision: 8, values: requested };
  const calls = [];
  const api = { async post(route, body) {
    calls.push(route);
    if (route === "playnite/settings_update") {
      assert.deepEqual(ordinary(body), { ...requested, expected_revision: 7 });
      return next;
    }
    assert.equal(route, "playnite/settings");
    return next;
  } };
  assert.deepEqual(ordinary(await context.Amaterasu.savePlayniteSettings(api, current, requested)), next);
  assert.deepEqual(calls, ["playnite/settings_update", "playnite/settings"]);
});

const inputSettingsValues = () => ({
  keyboard: true, mouse: true, controller: true,
  high_resolution_scrolling: true, always_send_scancodes: true,
  key_rightalt_to_key_win: false, key_repeat_delay: 500,
  key_repeat_frequency: 24.9, back_button_timeout: -1,
  gamepad: "auto", ds4_back_as_touchpad_click: false, native_pen_touch: true,
});

test("input policy confirms complete signed update and durable GET at revision plus one", async () => {
  const current = {
    ok: true, persisted: true, applied: true, cleanup_required: false,
    revision: 7, values: inputSettingsValues(),
  };
  const requested = { ...current.values, keyboard: false, key_repeat_delay: 0 };
  const confirmed = { ...current, revision: 8, values: requested };
  const routes = [];
  const api = { async post(route, body) {
    routes.push(route);
    if (route === "input/settings_update")
      assert.deepEqual(ordinary(body), { ...requested, expected_revision: 7 });
    else assert.equal(route, "input/settings_get");
    return confirmed;
  } };
  assert.deepEqual(ordinary(await context.Amaterasu.saveInputPolicySettings(api, current, requested)), confirmed);
  assert.deepEqual(routes, ["input/settings_update", "input/settings_get"]);
});

test("input policy rejects changed unrelated values, malformed replies and concurrent readback", async () => {
  const current = {
    ok: true, persisted: true, applied: true, cleanup_required: false,
    revision: 7, values: inputSettingsValues(),
  };
  const requested = { ...current.values, keyboard: false };
  const confirmed = { ...current, revision: 8, values: requested };
  for (const update of [
    { ...confirmed, revision: 9 },
    { ...confirmed, values: { ...requested, ds4_back_as_touchpad_click: true } },
    { ...confirmed, values: { ...requested, unknown: true } },
    { ...confirmed, ok: false },
  ]) {
    const routes = [];
    await assert.rejects(context.Amaterasu.saveInputPolicySettings({ async post(route) {
      routes.push(route);
      return update;
    } }, current, requested));
    assert.deepEqual(routes, ["input/settings_update"]);
  }
  for (const readback of [
    { ...confirmed, revision: 9 },
    { ...confirmed, values: { ...requested, controller: false } },
    { ...confirmed, values: { ...requested, ds4_back_as_touchpad_click: true } },
  ]) {
    await assert.rejects(context.Amaterasu.saveInputPolicySettings({ async post(route) {
      return route === "input/settings_update" ? confirmed : readback;
    } }, current, requested));
  }
});

test("input cleanup_pending requires exact durable GET before reporting saved policy", async () => {
  class ApiError extends Error { constructor(code) { super(code); this.code = code; } }
  context.ApiError = ApiError;
  const current = {
    ok: true, persisted: true, applied: true, cleanup_required: false,
    revision: 7, values: inputSettingsValues(),
  };
  const requested = { ...current.values, controller: false };
  const pending = { ...current, revision: 8, applied: false,
    cleanup_required: true, values: requested };
  const api = (readback) => ({ async post(route) {
    if (route === "input/settings_update") throw new ApiError("cleanup_pending");
    assert.equal(route, "input/settings_get");
    return readback;
  } });
  assert.deepEqual(ordinary(await context.Amaterasu.saveInputPolicySettings(
    api(pending), current, requested)), pending);
  assert.match(context.Amaterasu.inputPolicyRuntimeDescription(pending, {
    gamepad: "auto", controller_enabled: false, controller_cleanup_required: true,
  }), /pending/);
  for (const readback of [
    { ...pending, revision: 7 },
    { ...pending, values: { ...requested, ds4_back_as_touchpad_click: true } },
    { ...pending, persisted: false },
  ]) {
    await assert.rejects(context.Amaterasu.saveInputPolicySettings(
      api(readback), current, requested));
  }
});

test("input settings load discards old GET/status and permits status failure", async () => {
  const gate = new context.Amaterasu.InputPolicyLoadGate();
  const settings = { ok: true, persisted: true, applied: true, cleanup_required: false,
    revision: 7, values: inputSettingsValues() };
  let resolveOld;
  const old = gate.read({ post: () => new Promise((resolve) => { resolveOld = resolve; }) });
  const fresh = gate.read({ async post(route) {
    if (route === "input/settings_get") return { ...settings, revision: 8 };
    throw new Error("status unavailable");
  } });
  resolveOld(settings);
  assert.equal(await old, null);
  const current = await fresh;
  assert.equal(current.settings.revision, 8);
  assert.equal(current.runtime, null);
  const malformed = await gate.read({ async post(route) {
    return route === "input/settings_get" ? settings : { ok: true, supported_profiles: "auto" };
  } });
  assert.equal(malformed.runtime, null);
  let resolveStatus;
  const waiting = gate.read({ async post(route) {
    if (route === "input/settings_get") return settings;
    return new Promise((resolve) => { resolveStatus = resolve; });
  } });
  await new Promise(setImmediate);
  gate.invalidate();
  resolveStatus({ gamepad: "auto" });
  assert.equal(await waiting, null);
});

test("input form confirms cleanup-pending save, then reloads after a revision conflict", async () => {
  class Element {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.listeners = new Map();
      this.isConnected = false;
      this.value = "";
    }
    setConnected(value) {
      this.isConnected = value;
      for (const child of this.children) child?.setConnected?.(value);
    }
    append(...children) {
      for (const child of children) {
        if (child == null) continue;
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
    reportValidity() { return true; }
    setCustomValidity() {}
    get options() { return this.children.filter((child) => child?.tagName === "option"); }
    get valueAsNumber() { return Number(this.value); }
    get textContent() {
      return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
    }
    set textContent(value) { this.replaceChildren(value); }
    fire(name, event) { this.listeners.get(name)?.(event); }
  }
  const find = (node, predicate) => {
    if (predicate(node)) return node;
    for (const child of node.children ?? []) {
      if (typeof child === "string") continue;
      const result = find(child, predicate);
      if (result) return result;
    }
    return null;
  };
  const dom = vm.createContext({
    TextEncoder, URL, Error,
    document: { createElement: (name) => new Element(name) },
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", dom);
  }
  class ApiError extends Error { constructor(code) { super(code); this.code = code; } }
  dom.ApiError = ApiError;
  const content = new Element("main");
  content.setConnected(true);
  const notices = [];
  let stored = { ok: true, persisted: true, applied: true, cleanup_required: false,
    revision: 7, values: inputSettingsValues() };
  let updates = 0;
  let secondSubmittedRevision;
  dom.Amaterasu.renderSettings({
    content,
    notify: (message) => notices.push(message),
    api: {
      allows: (scope) => scope === "configuration.write",
      async post(route, body) {
        if (route === "input/settings_get") return stored;
        if (route === "input/status") throw new Error("dynamic status unavailable");
        if (route === "input/settings_update") {
          updates += 1;
          if (updates === 2) {
            secondSubmittedRevision = body.expected_revision;
            stored = { ...stored, revision: 9 };
            throw new ApiError("revision_conflict");
          }
          stored = { ...stored, revision: 8, applied: false,
            cleanup_required: true, values: { ...body } };
          delete stored.values.expected_revision;
          throw new ApiError("cleanup_pending");
        }
        throw new Error(`unavailable: ${route}`);
      },
    },
  });
  await new Promise(setImmediate);
  const inputArea = content.children[8];
  const oldForm = find(inputArea, (node) => node.tagName === "form");
  assert.ok(oldForm);
  assert.equal(find(oldForm, (node) => node.name === "input_ds4_back_as_touchpad_click").checked, false);
  const gamepadProfile = find(oldForm, (node) => node.name === "input_gamepad_profile");
  assert.deepEqual(gamepadProfile.options.map((option) => option.value), ["auto"]);
  gamepadProfile.value = "xone";
  oldForm.fire("submit", { preventDefault() {} });
  await new Promise(setImmediate);
  assert.equal(updates, 0);
  assert.equal(oldForm.isConnected, true);
  gamepadProfile.value = "auto";
  oldForm.fire("submit", { preventDefault() {} });
  await new Promise(setImmediate);
  assert.equal(oldForm.isConnected, false);
  assert.match(inputArea.textContent, /Revision 8/);
  assert.match(notices.at(-1), /saved and confirmed.*pending/);
  assert.match(inputArea.textContent, /Runtime status is unavailable/);
  const freshForm = find(inputArea, (node) => node.tagName === "form");
  freshForm.fire("submit", { preventDefault() {} });
  await new Promise(setImmediate);
  assert.equal(secondSubmittedRevision, 8);
  assert.match(inputArea.textContent, /Revision 9/);
  assert.match(notices.at(-1), /changed elsewhere/);
});

test("Playnite save rejects mismatched replies, unrelated values and revision drift", async () => {
  const values = {
    enabled: true, auto_sync: true, sync_all_installed: false,
    autosync_remove_uninstalled: true, autosync_delete_after_days: 14,
    autosync_require_replacement: true, focus_attempts: 3,
    focus_timeout_secs: 15, focus_exit_on_first: false,
    fullscreen_entry_enabled: false, recent_games: 10,
    recent_max_age_days: 30, sync_categories: [], exclude_categories: [],
    sync_plugins: [], exclude_plugins: [], exclude_games: [],
  };
  const current = { ok: true, revision: 7, values };
  const requested = { ...values, recent_games: 12 };
  const confirmed = { ok: true, revision: 8, values: requested };
  for (const update of [
    { ...confirmed, revision: 9 },
    { ...confirmed, values: { ...requested, focus_attempts: 4 } },
    { ...confirmed, ok: false },
    { ...confirmed, values: { ...requested, unexpected: true } },
  ]) {
    const calls = [];
    await assert.rejects(context.Amaterasu.savePlayniteSettings({ async post(route) {
      calls.push(route);
      return update;
    } }, current, requested));
    assert.deepEqual(calls, ["playnite/settings_update"]);
  }
  for (const readback of [
    { ...confirmed, revision: 9 },
    { ...confirmed, values: { ...requested, auto_sync: false } },
    { ...confirmed, values: { ...requested, exclude_games: ["changed"] } },
    { ...confirmed, values: { ...requested, unexpected: true } },
  ]) {
    await assert.rejects(context.Amaterasu.savePlayniteSettings({ async post(route) {
      return route === "playnite/settings_update" ? confirmed : readback;
    } }, current, requested));
  }
});

test("Playnite reconciles an unknown update outcome only with exact durable readback", async () => {
  class ApiError extends Error {
    constructor(code) { super(code); this.code = code; }
  }
  context.ApiError = ApiError;
  const values = {
    enabled: true, auto_sync: true, sync_all_installed: false,
    autosync_remove_uninstalled: true, autosync_delete_after_days: 14,
    autosync_require_replacement: true, focus_attempts: 3,
    focus_timeout_secs: 15, focus_exit_on_first: false,
    fullscreen_entry_enabled: false, recent_games: 10,
    recent_max_age_days: 30, sync_categories: [], exclude_categories: [],
    sync_plugins: [], exclude_plugins: [], exclude_games: [],
  };
  const current = { ok: true, revision: 7, values };
  const requested = { ...values, recent_games: 11 };
  const committed = { ok: true, revision: 8, values: requested };
  const api = (readback) => ({ async post(route) {
    if (route === "playnite/settings_update") throw new ApiError("update_outcome_unknown");
    assert.equal(route, "playnite/settings");
    return readback;
  } });
  assert.deepEqual(ordinary(await context.Amaterasu.savePlayniteSettings(
    api(committed), current, requested)), committed);
  await assert.rejects(context.Amaterasu.savePlayniteSettings(
    api({ ...committed, values: { ...requested, focus_attempts: 4 } }), current, requested),
  /uncertain/);
});

test("Playnite settings and runtime load discards stale success and failure", async () => {
  const gate = new context.Amaterasu.PlayniteSettingsLoadGate();
  let resolveOld;
  const old = gate.read(() => new Promise((resolve) => { resolveOld = resolve; }), async () => ({}));
  const latest = gate.read(async () => ({ revision: 9 }), async () => ({ revision: 9 }));
  resolveOld({ revision: 8 });
  assert.equal(await old, null);
  assert.equal((await latest).settings.revision, 9);
  let rejectOld;
  const failing = gate.read(() => new Promise((_, reject) => { rejectOld = reject; }), async () => ({}));
  gate.invalidate();
  rejectOld(new Error("old load failed"));
  assert.equal(await failing, null);
  let resolveStatus;
  const statusPending = gate.read(async () => ({ revision: 9 }),
    () => new Promise((resolve) => { resolveStatus = resolve; }));
  await new Promise(setImmediate);
  gate.invalidate();
  resolveStatus({ revision: 9 });
  assert.equal(await statusPending, null);
});

test("recent-games UI preserves the provider's 10000 Playnite range", () => {
  const settingsSource = readFileSync("src/settings.ts", "utf8");
  const steamSettingsUi = settingsSource.slice(
    settingsSource.indexOf("async function loadSteam(): Promise<void>"),
    settingsSource.indexOf("async function loadLutris(): Promise<void>"),
  );
  const playniteSettingsUi = settingsSource.slice(
    settingsSource.indexOf("async function loadPlaynite(): Promise<void>"),
    settingsSource.indexOf(
      "async function loadPairing(): Promise<void>",
    ),
  );

  assert.match(steamSettingsUi, /recentGames\.max = "10000";/);
  assert.match(
    steamSettingsUi,
    /Maximum number of recently played games \(0–10000\); zero imports none in recent-only mode\./,
  );
  assert.match(playniteSettingsUi, /recentGames\.min = "0";/);
  assert.match(playniteSettingsUi, /recentGames\.max = "10000";/);
  assert.match(
    playniteSettingsUi,
    /Maximum number of recent games \(0–10000\); zero disables recent-game selection\./,
  );
});

test("Playnite status explains disabled, unavailable, and active scan states", () => {
  const describe = context.Amaterasu.playniteStatusDescription;
  const status = {
    ok: true,
    settings_ready: true,
    enabled: true,
    auto_sync: true,
    playnite_running: true,
    playnite_paths: [],
    snapshot_pending: false,
    snapshot_game_count: 12,
    launchable_game_count: 3,
    active_provider_launches: 0,
    catalog_sync: null,
    last_error: null,
    revision: 4,
  };
  assert.match(describe({ ...status, settings_ready: false }), /still loading/);
  assert.match(describe({ ...status, enabled: false }), /disabled/);
  assert.match(describe({ ...status, playnite_running: false }), /discovery is currently available on Windows/);
  assert.match(describe({ ...status, snapshot_pending: true }), /snapshot is in progress/);
  assert.match(describe({ ...status, last_error: "pipe unavailable" }), /pipe unavailable/);
});

test("client FEC override editor preserves sparse inherit and validates bounded integers", () => {
  const parseFec = context.Amaterasu.parseClientFecOverride;
  assert.deepEqual(ordinary(parseFec("")), { fec_percentage: null });
  assert.deepEqual(ordinary(parseFec("  ")), { fec_percentage: null });
  assert.deepEqual(ordinary(parseFec("1")), { fec_percentage: 1 });
  assert.deepEqual(ordinary(parseFec("255")), { fec_percentage: 255 });
  for (const value of ["0", "256", "1.5", "1e2", "01", "-1", "abc"])
    assert.throws(() => parseFec(value));
});

test("NVHTTP network settings preserve startup null and explicit blank overrides", () => {
  const parse = context.Amaterasu.parseNvhttpNetworkOverrides;
  assert.deepEqual(ordinary(parse("startup", true, "")), {
    address_family: null,
    bind_address: null,
  });
  assert.deepEqual(ordinary(parse("ipv4", false, "")), {
    address_family: "ipv4",
    bind_address: "",
  });
  assert.deepEqual(ordinary(parse("both", false, "::1")), {
    address_family: "both",
    bind_address: "::1",
  });
  assert.deepEqual(ordinary(parse("both", false, "192.168.1.20")), {
    address_family: "both",
    bind_address: "192.168.1.20",
  });
  assert.deepEqual(ordinary(parse("startup", false, "::1")), {
    address_family: null,
    bind_address: "::1",
  });
  for (const [family, useStartup, bind] of [
    ["ipv6", false, "127.0.0.1"],
    ["ipv4", false, "::1"],
    ["both", false, "localhost"],
    ["both", false, " "],
    ["both", false, "127.0.0.1/24"],
    ["both", false, "127.0.0.1:47989"],
    ["both", false, "224.0.0.1"],
    ["both", false, "255.255.255.255"],
    ["both", false, "ff02::1"],
    ["both", false, "::ffff:127.0.0.1"],
    ["both", false, " ::1"],
  ]) {
    assert.throws(() => parse(family, useStartup, bind));
  }
});

const pairedClients = () => ({
  ok: true,
  revision: 7,
  clients: [
    { certificate_id: "a".repeat(64), client_id: "living-room", name: "Living room",
      paired_at: 100, config_overrides: {
        fec_percentage: 20, native_pen_touch: false,
        max_bitrate: 15000, frame_limiter_enable: false,
      } },
    { certificate_id: "b".repeat(64), client_id: "desk", name: "Desk",
      paired_at: 200, config_overrides: { audio_sink: "headphones" } },
  ],
});

test("paired-client editor preserves hidden overrides and confirms complete signed readback", async () => {
  const current = pairedClients();
  const values = { fec_percentage: 35, native_pen_touch: true, stream_audio: false };
  const expected = { ...current.clients[0].config_overrides, ...values };
  const saved = { ...current, revision: 8, clients: [
    { ...current.clients[0], config_overrides: expected }, current.clients[1],
  ] };
  const routes = [];
  const api = { async post(route, body) {
    routes.push(route);
    if (route === "pairing/update_client_config_overrides") {
      assert.deepEqual(ordinary(body), {
        certificate_id: "a".repeat(64), expected_revision: 7, config_overrides: expected,
      });
      return { ok: true, certificate_id: "a".repeat(64) };
    }
    assert.equal(route, "pairing/list_clients");
    return saved;
  } };
  assert.deepEqual(ordinary(await context.Amaterasu.savePairingClientOverrides(
    api, current, "a".repeat(64), values)), saved);
  assert.deepEqual(routes, ["pairing/update_client_config_overrides", "pairing/list_clients"]);
});

test("paired-client editor rejects unknown fields, mismatched identity and stale readback", async () => {
  const current = pairedClients();
  const id = "a".repeat(64);
  assert.deepEqual(ordinary(context.Amaterasu.unsupportedPairingOverrideFields(current.clients[0])), []);
  const unsupported = pairedClients();
  unsupported.clients[0].config_overrides.future_policy = true;
  assert.deepEqual(ordinary(context.Amaterasu.unsupportedPairingOverrideFields(unsupported.clients[0])), ["future_policy"]);
  const malformed = pairedClients();
  malformed.clients[0].config_overrides.max_bitrate = "fast";
  assert.deepEqual(ordinary(context.Amaterasu.unsupportedPairingOverrideFields(malformed.clients[0])), ["max_bitrate"]);
  malformed.clients[0].config_overrides.max_bitrate = 15000;
  malformed.clients[0].config_overrides.frame_limiter_fps_limit = 59.9401;
  assert.deepEqual(ordinary(context.Amaterasu.unsupportedPairingOverrideFields(malformed.clients[0])), ["frame_limiter_fps_limit"]);
  await assert.rejects(context.Amaterasu.savePairingClientOverrides({ async post() {
    throw new Error("unexpected write");
  } }, unsupported, id, { fec_percentage: 30 }), /cannot preserve/);
  const desired = { ...current.clients[0].config_overrides, fec_percentage: 30 };
  const committed = { ...current, revision: 8, clients: [
    { ...current.clients[0], config_overrides: desired }, current.clients[1],
  ] };
  for (const readback of [
    { ...committed, revision: 9 },
    { ...committed, clients: [{ ...committed.clients[0], client_id: "changed" }, committed.clients[1]] },
    { ...committed, clients: [{ ...committed.clients[0], config_overrides: { ...desired, max_bitrate: 14000 } }, committed.clients[1]] },
    { ...committed, clients: [committed.clients[0], { ...committed.clients[1], config_overrides: {} }] },
  ]) {
    await assert.rejects(context.Amaterasu.savePairingClientOverrides({ async post(route) {
      return route === "pairing/update_client_config_overrides"
        ? { ok: true, certificate_id: id } : readback;
    } }, current, id, { fec_percentage: 30 }));
  }
  await assert.rejects(context.Amaterasu.savePairingClientOverrides({ async post() {
    return { ok: true, certificate_id: "b".repeat(64) };
  } }, current, id, { fec_percentage: 30 }), /selected certificate/);
});

test("paired-client editor leaves uncertain writes unconfirmed for a fresh UI reload", async () => {
  class ApiError extends Error { constructor(code) { super(code); this.code = code; } }
  context.ApiError = ApiError;
  const current = pairedClients();
  const id = "a".repeat(64);
  const routes = [];
  const api = { async post(route) {
    routes.push(route);
    if (route === "pairing/update_client_config_overrides") throw new ApiError("state_unavailable");
    return current;
  } };
  await assert.rejects(context.Amaterasu.savePairingClientOverrides(
    api, current, id, { fec_percentage: 30 }), /state_unavailable/);
  assert.deepEqual(routes, ["pairing/update_client_config_overrides"]);
});

test("paired-client loads discard old list or input status after refresh", async () => {
  const gate = new context.Amaterasu.PairingClientsLoadGate();
  let resolveOld;
  const old = gate.read({ post: () => new Promise((resolve) => { resolveOld = resolve; }) });
  const latest = gate.read({ async post(route) {
    if (route === "pairing/list_clients") return pairedClients();
    throw new Error("input unavailable");
  } });
  resolveOld(pairedClients());
  assert.equal(await old, null);
  assert.equal((await latest).settings.revision, 7);
  let resolveStatus;
  const pending = gate.read({ async post(route) {
    if (route === "pairing/list_clients") return pairedClients();
    return new Promise((resolve) => { resolveStatus = resolve; });
  } });
  await new Promise(setImmediate);
  gate.invalidate();
  resolveStatus({ ok: true });
  assert.equal(await pending, null);
});

test("paired-client form preserves hidden fields, reloads conflict, and blocks unknown fields", async () => {
  class Element {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.listeners = new Map();
      this.isConnected = false;
      this.value = "";
      this.attributes = new Map();
    }
    setConnected(value) {
      this.isConnected = value;
      for (const child of this.children) child?.setConnected?.(value);
    }
    append(...children) {
      for (const child of children) {
        if (child == null) continue;
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
    setAttribute(name, value) { this.attributes.set(name, value); }
    reportValidity() { return true; }
    setCustomValidity() {}
    get options() { return this.children.filter((child) => child?.tagName === "option"); }
    get valueAsNumber() { return Number(this.value); }
    get textContent() {
      return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
    }
    set textContent(value) { this.replaceChildren(value); }
    fire(name, event = {}) { this.listeners.get(name)?.(event); }
  }
  const find = (node, predicate) => {
    if (predicate(node)) return node;
    for (const child of node.children ?? []) {
      if (typeof child === "string") continue;
      const result = find(child, predicate);
      if (result) return result;
    }
    return null;
  };
  const dom = vm.createContext({
    TextEncoder, URL, Error,
    document: { createElement: (name) => new Element(name) },
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", dom);
  }
  class ApiError extends Error { constructor(code) { super(code); this.code = code; } }
  dom.ApiError = ApiError;
  const content = new Element("main");
  content.setConnected(true);
  const notices = [];
  let stored = pairedClients();
  let updates = 0;
  let secondSubmittedRevision;
  dom.Amaterasu.renderSettings({
    content,
    notify: (message) => notices.push(message),
    api: {
      allows: (scope) => scope === "pairing.admin",
      async post(route, body) {
        if (route === "pairing/list_clients") return stored;
        if (route === "input/status") throw new Error("input offline");
        if (route === "pairing/update_client_config_overrides") {
          updates += 1;
          if (updates === 2) {
            secondSubmittedRevision = body.expected_revision;
            stored = { ...stored, revision: 9, clients: [
              { ...stored.clients[0], config_overrides: {
                ...stored.clients[0].config_overrides, future_policy: true,
              } }, stored.clients[1],
            ] };
            throw new ApiError("revision_conflict");
          }
          assert.equal(body.expected_revision, 7);
          assert.equal(body.config_overrides.max_bitrate, 15000);
          assert.equal(body.config_overrides.frame_limiter_enable, false);
          assert.equal(body.config_overrides.native_pen_touch, true);
          stored = { ...stored, revision: 8, clients: [
            { ...stored.clients[0], config_overrides: body.config_overrides }, stored.clients[1],
          ] };
          return { ok: true, certificate_id: body.certificate_id };
        }
        throw new Error(`unavailable: ${route}`);
      },
    },
  });
  await new Promise(setImmediate);
  const pairingArea = content.children[10];
  const fec = find(pairingArea, (node) => node.name === `fec_${"a".repeat(64)}`);
  const save = find(pairingArea, (node) => node.tagName === "button" && node.textContent === "Save");
  assert.ok(fec);
  assert.ok(save);
  fec.value = "35";
  find(pairingArea, (node) => node.name === "native_pen_touch").value = "true";
  save.fire("click");
  await new Promise(setImmediate);
  assert.equal(updates, 1);
  assert.match(pairingArea.textContent, /Revision 8/);
  assert.match(notices.at(-1), /Stored overrides confirmed.*Active stream application is not confirmed/);
  const freshSave = find(pairingArea, (node) => node.tagName === "button" && node.textContent === "Save");
  freshSave.fire("click");
  await new Promise(setImmediate);
  assert.equal(secondSubmittedRevision, 8);
  assert.match(pairingArea.textContent, /Revision 9/);
  assert.match(notices.at(-1), /changed elsewhere/);
  const blocked = find(pairingArea, (node) => node.tagName === "button" && node.textContent === "Save");
  assert.equal(blocked.disabled, true);
  assert.match(pairingArea.textContent, /cannot preserve \(future_policy\)/);
});

function nvhttpHostSettings(revision = 8) {
  return {
    ok: true, revision,
    hostname: "Computer Name", configured_hostname: "",
    port: 47989, configured_address_family: null, configured_bind_address: null,
    configured_capture: "auto", configured_output_name: "DISPLAY1",
    stream_audio: false, configured_video_max_batch_size_kb: null,
    configured_max_bitrate: null, configured_minimum_fps_target: null,
    configured_ping_timeout_ms: null, configured_audio_sink: null,
    configured_virtual_sink: null, audio_sink_capture_only: false,
    configured_external_ip: null, configured_wgc_pacing_smoothing: null,
    configured_global_preparation: [],
    configured_legacy_global_preparation: [{ do: "old command", elevated: false }],
    configured_frame_limiter: { frame_limiter_enable: false, frame_limiter_fps_limit_millihz: null },
    nvenc_spatial_aq: true, lan_encryption_mode: 1, wan_encryption_mode: 2,
    active_http_port: 47989, active_rtsp_port: 47989,
    listener_error: null, rtsp_listener_error: null,
    effective_output_name: "DISPLAY1",
  };
}

function nvhttpHostUpdate() {
  return {
    hostname: "Living Room", port: 47989,
    address_family: null, bind_address: null,
    capture: "auto", output_name: "", stream_audio: true,
    video_max_batch_size_kb: null, max_bitrate: null,
    minimum_fps_target: null, ping_timeout_ms: -1,
    audio_sink: null, virtual_sink: "",
    audio_sink_capture_only: false, external_ip: null,
    wgc_pacing_smoothing: null,
    global_preparation: [{ run: { executable: "C:/tools/prepare.exe" } }],
  };
}

function nvhttpUpdatedHost() {
  return {
    ...nvhttpHostSettings(9),
    configured_hostname: "Living Room", configured_output_name: null,
    stream_audio: true, configured_virtual_sink: "",
    configured_global_preparation: [{
      run: {
        executable: "C:/tools/prepare.exe", args: [], environment: {},
        cwd: null, timeout_ms: 10000,
      }, undo: null,
    }],
  };
}

test("Moonlight host editor confirms signed update and GET with canonical preparation and dynamic listeners", async () => {
  const current = nvhttpHostSettings();
  const values = nvhttpHostUpdate();
  const updated = nvhttpUpdatedHost();
  const readback = { ...updated, active_http_port: null, active_rtsp_port: null,
    rtsp_listener_error: "listener apply pending", effective_output_name: "Startup output" };
  const calls = [];
  const api = { async post(route, body = {}) {
    calls.push([route, ordinary(body)]);
    return route === "nvhttp/settings_update" ? updated : readback;
  } };
  const saved = await context.Amaterasu.saveNvhttpHostSettings(api, current, values);
  assert.deepEqual(ordinary(saved), readback);
  assert.deepEqual(calls, [
    ["nvhttp/settings_update", { ...values, expected_revision: 8 }],
    ["nvhttp/settings", {}],
  ]);
});

test("Moonlight host editor rejects mismatched configured or unrelated fields and concurrent readback", async () => {
  const current = nvhttpHostSettings();
  const values = nvhttpHostUpdate();
  const updated = nvhttpUpdatedHost();
  for (const rejected of [
    { ok: false, code: "revision_conflict" },
    { ...updated, revision: 10 },
    { ...updated, configured_virtual_sink: null },
    { ...updated, configured_ping_timeout_ms: -1 },
    { ...updated, configured_frame_limiter: { frame_limiter_enable: true } },
    { ...updated, configured_legacy_global_preparation: [] },
    { ...updated, lan_encryption_mode: 0 },
  ]) {
    const routes = [];
    await assert.rejects(context.Amaterasu.saveNvhttpHostSettings({ async post(route) {
      routes.push(route);
      return rejected;
    } }, current, values));
    assert.deepEqual(routes, ["nvhttp/settings_update"]);
  }
  for (const stale of [
    current,
    { ...updated, revision: 10 },
    { ...updated, configured_audio_sink: "other device" },
    { ...updated, wan_encryption_mode: 0 },
  ]) {
    const routes = [];
    await assert.rejects(context.Amaterasu.saveNvhttpHostSettings({ async post(route) {
      routes.push(route);
      return route === "nvhttp/settings_update" ? updated : stale;
    } }, current, values), /could not be confirmed/);
    assert.deepEqual(routes, ["nvhttp/settings_update", "nvhttp/settings"]);
  }
});

test("Moonlight host editor ignores delayed loads and errors after a newer refresh or save", async () => {
  const gate = new context.Amaterasu.NvhttpSettingsLoadGate();
  const pending = [];
  const api = { post(route) {
    assert.equal(route, "nvhttp/settings");
    return new Promise((resolve, reject) => pending.push({ resolve, reject }));
  } };
  const old = gate.read(api);
  const latest = gate.read(api);
  pending[1].resolve(nvhttpHostSettings(9));
  const current = await latest;
  assert.equal(current.settings.revision, 9);
  assert.equal(gate.isCurrent(current.generation), true);
  pending[0].resolve(nvhttpHostSettings(8));
  assert.equal(await old, null);
  const duringSave = gate.read(api);
  gate.invalidate();
  assert.equal(gate.isCurrent(current.generation), false);
  pending[2].reject(new Error("old load failed"));
  assert.equal(await duringSave, null);
  const reloaded = gate.read(api);
  pending[3].resolve(nvhttpHostSettings(10));
  assert.equal((await reloaded).settings.revision, 10);
});

test("Moonlight host editor reloads a durable update when the listener is not active", async () => {
  const gate = new context.Amaterasu.NvhttpSettingsLoadGate();
  let stored = nvhttpHostSettings();
  let visible = (await gate.read({ post: async () => stored })).settings;
  const routes = [];
  const api = { async post(route) {
    routes.push(route);
    if (route === "nvhttp/settings_update") {
      stored = { ...nvhttpUpdatedHost(), active_http_port: null, active_rtsp_port: null,
        listener_error: "listener disabled" };
      return { ok: false, code: "listener_not_active", persisted: true };
    }
    assert.equal(route, "nvhttp/settings");
    return stored;
  } };
  const notices = [];
  await assert.rejects(context.Amaterasu.saveNvhttpHostSettings(api, visible, nvhttpHostUpdate()));
  const handled = await context.Amaterasu.recoverNvhttpSaveFailure(
    "listener_not_active",
    async () => { visible = (await gate.read(api)).settings; },
    (message) => notices.push(message),
  );
  assert.equal(handled, true);
  assert.deepEqual(routes, ["nvhttp/settings_update", "nvhttp/settings"]);
  assert.equal(visible.revision, 9);
  assert.equal(visible.active_http_port, null);
  assert.match(notices[0], /were stored, but listener application is not confirmed/);
  assert.equal(await context.Amaterasu.recoverNvhttpSaveFailure("invalid_request", async () => {
    throw new Error("unexpected reload");
  }, () => {}), false);
});

test("Moonlight host form reloads its revision and listener status after a persisted error", async () => {
  class Element {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.listeners = new Map();
      this.isConnected = false;
      this.value = "";
    }
    setConnected(connected) {
      this.isConnected = connected;
      for (const child of this.children) child?.setConnected?.(connected);
    }
    append(...children) {
      for (const child of children) {
        if (child == null) continue;
        this.children.push(child);
        child?.setConnected?.(this.isConnected);
      }
    }
    replaceChildren(...children) {
      for (const child of this.children) child?.setConnected?.(false);
      this.children = [];
      this.append(...children);
    }
    addEventListener(name, callback) {
      this.listeners.set(name, callback);
    }
    setAttribute() {}
    reportValidity() { return true; }
    setCustomValidity() {}
    get options() { return this.children.filter((child) => child?.tagName === "option"); }
    get valueAsNumber() { return Number(this.value); }
    get textContent() {
      return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
    }
    set textContent(value) { this.replaceChildren(value); }
    fire(name, event) { this.listeners.get(name)?.(event); }
  }
  const find = (node, predicate) => {
    if (predicate(node)) return node;
    for (const child of node.children ?? []) {
      if (typeof child === "string") continue;
      const result = find(child, predicate);
      if (result) return result;
    }
    return null;
  };
  const dom = vm.createContext({
    TextEncoder, URL, Error,
    document: { createElement: (tagName) => new Element(tagName) },
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", dom);
  }
  class ApiError extends Error {
    constructor(code) { super(code); this.code = code; }
  }
  dom.ApiError = ApiError;
  const content = new Element("main");
  content.setConnected(true);
  const notices = [];
  let stored = nvhttpHostSettings();
  const routes = [];
  let updates = 0;
  let secondSubmittedRevision;
  dom.Amaterasu.renderSettings({
    content,
    notify: (message) => notices.push(message),
    api: {
      allows: (scope) => scope === "configuration.write",
      async post(route, body) {
        routes.push(route);
        if (route === "nvhttp/settings") return stored;
        if (route === "nvhttp/settings_update") {
          updates += 1;
          if (updates === 2) {
            secondSubmittedRevision = body.expected_revision;
            throw new Error("second update intentionally rejected");
          }
          stored = { ...nvhttpUpdatedHost(), active_http_port: null, active_rtsp_port: null,
            listener_error: "listener disabled" };
          throw new ApiError("listener_not_active");
        }
        throw new Error(`unavailable: ${route}`);
      },
    },
  });
  await new Promise(setImmediate);
  const hostArea = content.children[4];
  const oldForm = find(hostArea, (node) => node.tagName === "form");
  assert.ok(oldForm);
  assert.match(hostArea.textContent, /Revision 8/);
  oldForm.fire("submit", { preventDefault() {} });
  await new Promise(setImmediate);
  assert.equal(oldForm.isConnected, false);
  assert.match(hostArea.textContent, /Revision 9/);
  assert.match(hostArea.textContent, /Active ports: HTTP off, HTTPS off, RTSP off/);
  assert.match(hostArea.textContent, /listener disabled/);
  assert.match(notices.at(-1), /were stored, but listener application is not confirmed/);
  assert.deepEqual(routes.filter((route) => route.startsWith("nvhttp/")), [
    "nvhttp/settings", "nvhttp/settings_update", "nvhttp/settings",
  ]);
  const freshForm = find(hostArea, (node) => node.tagName === "form");
  freshForm.fire("submit", { preventDefault() {} });
  await new Promise(setImmediate);
  assert.equal(updates, 2);
  assert.equal(secondSubmittedRevision, 9);
});

test("capture output selection preserves exact IDs and automatic reset", () => {
  const parse = context.Amaterasu.parseOutputNameSelection;
  assert.equal(parse(""), "");
  assert.equal(parse("DISPLAY1"), "DISPLAY1");
  assert.equal(parse("é".repeat(256)), "é".repeat(256));
  for (const invalid of [" ", " DISPLAY1", "DISPLAY1 ", "bad\nname", "é".repeat(257)])
    assert.throws(() => parse(invalid));
});

test("capture backend selection accepts only the four supported modes", () => {
  const parse = context.Amaterasu.parseCaptureSelection;
  for (const mode of ["auto", "ddx", "wgc", "wgcc"])
    assert.equal(parse(mode), mode);
  for (const invalid of ["", "dxgi", "WGC", "wgcc ", "other"])
    assert.throws(() => parse(invalid));
});

test("video batch size editor preserves default and bounded reference values", () => {
  const parse = context.Amaterasu.parseVideoMaxBatchSize;
  assert.equal(parse(""), null);
  assert.equal(parse("  "), null);
  for (const size of [0, 16, 20, 32, 64])
    assert.equal(parse(String(size)), size);
  for (const invalid of ["-1", "65", "16.5", "016", "16 KB", "NaN"])
    assert.throws(() => parse(invalid));
});

test("maximum bitrate editor preserves signed source values and reset", () => {
  const parse = context.Amaterasu.parseMaxBitrate;
  assert.equal(parse(""), null);
  assert.equal(parse("0"), 0);
  assert.equal(parse("-2147483648"), -2147483648);
  assert.equal(parse("2147483647"), 2147483647);
  for (const value of ["+1", "01", "-0", "1.5", "2147483648", "-2147483649", "Infinity"])
    assert.throws(() => parse(value));
});

test("minimum frame rate editor preserves default, zero, and fractional targets", () => {
  const parse = context.Amaterasu.parseMinimumFpsTarget;
  assert.equal(parse(""), null);
  assert.equal(parse("  "), null);
  for (const [input, expected] of [["0", 0], ["20", 20], ["59.94", 59.94], ["1e3", 1000]])
    assert.equal(parse(input), expected);
  for (const invalid of ["-1", "1000.01", "Infinity", "NaN", "0x10", "20 FPS", "1e309"])
    assert.throws(() => parse(invalid));
});

test("global ping timeout editor maps blank to reset and validates bounded milliseconds", () => {
  const parse = context.Amaterasu.parsePingTimeout;
  assert.equal(parse(""), -1);
  assert.equal(parse("  "), -1);
  assert.equal(parse("0"), 0);
  assert.equal(parse("10000"), 10000);
  assert.equal(parse("2147483647"), 2147483647);
  for (const invalid of ["-1", "+1", "01", "1.5", "2147483648", "Infinity", "10 ms"])
    assert.throws(() => parse(invalid));
});

test("audio sink editor preserves exact IDs and sparse automatic selection", () => {
  const parse = context.Amaterasu.parseAudioSinkSelection;
  assert.equal(parse(""), null);
  assert.equal(parse("  Endpoint ID  "), "  Endpoint ID  ");
  assert.equal(parse("é".repeat(2048)), "é".repeat(2048));
  for (const invalid of ["bad\nendpoint", "x".repeat(4097), "é".repeat(2049)])
    assert.throws(() => parse(invalid));
});

test("external IP editor preserves exact text and automatic reset", () => {
  const parse = context.Amaterasu.parseExternalIp;
  assert.equal(parse(""), null);
  assert.equal(parse("  address  "), "  address  ");
  assert.equal(parse("é".repeat(2048)), "é".repeat(2048));
  for (const invalid of ["bad\naddress", "x".repeat(4097), "é".repeat(2049)])
    assert.throws(() => parse(invalid));
});

test("WGC pacing editor preserves default and both explicit policies", () => {
  const parse = context.Amaterasu.parseWgcPacingSmoothing;
  assert.equal(parse(""), null);
  assert.equal(parse("true"), true);
  assert.equal(parse("false"), false);
  for (const invalid of ["0", "TRUE", "on", " false "])
    assert.throws(() => parse(invalid));
});

test("Steam exclusions preserve valid ID and name objects without normalization", () => {
  const exclusions = [
    { id: "730", name: "Counter-Strike 2" },
    { name: "Portal 2" },
  ];
  assert.deepEqual(ordinary(parse(JSON.stringify(exclusions))), exclusions);
});

const steamSettingsValues = () => ({
  enabled: true, auto_sync: false,
  exclude_games: [{ id: "730", name: "Counter-Strike 2" }, { name: "Portal 2" }],
  include_tools: false, sync_all_installed: false, recent_games: 10,
  recent_max_age_days: 30, autosync_remove_uninstalled: true,
});

const lutrisSettingsValues = () => ({
  enabled: true, auto_sync: true, autosync_remove_uninstalled: true,
  include_steam: false,
  exclude_games: ["0007", { id: "42", name: "Old Title" }, { name: "Portal 2" }],
});

test("Steam and Lutris confirm full signed updates and durable readback", async () => {
  for (const [provider, save, initial] of [
    ["steam", context.Amaterasu.saveSteamSettings, steamSettingsValues()],
    ["lutris", context.Amaterasu.saveLutrisSettings, lutrisSettingsValues()],
  ]) {
    const current = { ok: true, revision: 7, values: initial };
    const requested = provider === "steam"
      ? { ...initial, auto_sync: true, recent_games: 12 }
      : { ...initial, include_steam: true };
    const confirmed = { ok: true, revision: 8, values: requested };
    const routes = [];
    const api = { async post(route, body) {
      routes.push(route);
      if (route === `${provider}/settings_update`)
        assert.deepEqual(ordinary(body), { ...requested, expected_revision: 7 });
      else assert.equal(route, `${provider}/settings`);
      return ordinary(confirmed);
    } };
    assert.deepEqual(ordinary(await save(api, current, requested)), confirmed);
    assert.deepEqual(routes, [`${provider}/settings_update`, `${provider}/settings`]);
  }
});

test("Steam accepts only Linux forced-enabled normalization and returns durable values", async () => {
  const current = { ok: true, revision: 7, values: steamSettingsValues() };
  const requested = { ...current.values, enabled: false, recent_games: 12 };
  const normalized = { ok: true, revision: 8, values: { ...requested, enabled: true } };
  const routes = [];
  const api = { async post(route, body) {
    routes.push(route);
    if (route === "steam/settings_update") {
      assert.deepEqual(ordinary(body), { ...requested, expected_revision: 7 });
      return normalized;
    }
    assert.equal(route, "steam/settings");
    return normalized;
  } };
  assert.deepEqual(ordinary(await context.Amaterasu.saveSteamSettings(api, current, requested)), normalized);
  assert.deepEqual(routes, ["steam/settings_update", "steam/settings"]);
  for (const update of [
    { ...normalized, values: { ...normalized.values, recent_games: 13 } },
    { ...normalized, revision: 9 },
  ]) {
    await assert.rejects(context.Amaterasu.saveSteamSettings({ async post() { return update; } },
      current, requested));
  }
  await assert.rejects(context.Amaterasu.saveSteamSettings({ async post(route) {
    return route === "steam/settings_update" ? normalized : { ...normalized, values: requested };
  } }, current, requested));
  await assert.rejects(context.Amaterasu.saveLutrisSettings({ async post() {
    return { ok: true, revision: 8, values: { ...lutrisSettingsValues(), enabled: true } };
  } }, { ok: true, revision: 7, values: lutrisSettingsValues() },
  { ...lutrisSettingsValues(), enabled: false }));
});

test("Steam form reloads normalized Linux settings and explains the enabled policy", async () => {
  class Element {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.listeners = new Map();
      this.isConnected = false;
      this.value = "";
    }
    setConnected(value) {
      this.isConnected = value;
      for (const child of this.children) child?.setConnected?.(value);
    }
    append(...children) {
      for (const child of children) {
        if (child == null) continue;
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
    reportValidity() { return true; }
    setCustomValidity() {}
    get options() { return this.children.filter((child) => child?.tagName === "option"); }
    get valueAsNumber() { return Number(this.value); }
    get textContent() {
      return this.children.map((child) => typeof child === "string" ? child : child.textContent).join("");
    }
    set textContent(value) { this.replaceChildren(value); }
    fire(name, event) { this.listeners.get(name)?.(event); }
  }
  const find = (node, predicate) => {
    if (predicate(node)) return node;
    for (const child of node.children ?? []) {
      if (typeof child === "string") continue;
      const result = find(child, predicate);
      if (result) return result;
    }
    return null;
  };
  const dom = vm.createContext({
    TextEncoder, URL, Error,
    document: { createElement: (name) => new Element(name) },
  });
  for (const source of ["src/model.ts", "src/ui.ts", "src/pages.ts", "src/settings.ts"]) {
    vm.runInContext(ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }), dom);
    vm.runInContext("Object.assign(globalThis, Amaterasu)", dom);
  }
  dom.ApiError = class ApiError extends Error {};
  const content = new Element("main");
  content.setConnected(true);
  const notices = [];
  let stored = { ok: true, revision: 7, values: steamSettingsValues() };
  const routes = [];
  dom.Amaterasu.renderSettings({
    content,
    notify: (message) => notices.push(message),
    api: {
      allows: (scope) => scope === "configuration.write",
      async post(route, body) {
        routes.push(route);
        if (route === "steam/settings") return stored;
        if (route === "steam/status") return {
          settings_ready: true, enabled: true, scanning: false, complete: false,
          game_count: 0, warning_count: 0,
        };
        if (route === "steam/settings_update") {
          assert.equal(body.expected_revision, 7);
          assert.equal(body.enabled, false);
          stored = { ok: true, revision: 8, values: { ...body, enabled: true } };
          delete stored.values.expected_revision;
          return stored;
        }
        throw new Error(`unavailable: ${route}`);
      },
    },
  });
  await new Promise(setImmediate);
  const steamArea = content.children[5];
  const oldForm = find(steamArea, (node) => node.tagName === "form");
  assert.ok(oldForm);
  find(oldForm, (node) => node.name === "steam_enabled").checked = false;
  oldForm.fire("submit", { preventDefault() {} });
  await new Promise(setImmediate);
  assert.equal(oldForm.isConnected, false);
  assert.match(steamArea.textContent, /Revision 8/);
  assert.equal(find(steamArea, (node) => node.name === "steam_enabled").checked, true);
  assert.match(notices.at(-1), /remains enabled by Linux host policy/);
  assert.deepEqual(routes.filter((route) => route.startsWith("steam/")), [
    "steam/settings", "steam/status", "steam/settings_update", "steam/settings", "steam/settings", "steam/status",
  ]);
});

test("Steam and Lutris reject mismatched configured values and concurrent readback", async () => {
  for (const [provider, save, values] of [
    ["steam", context.Amaterasu.saveSteamSettings, steamSettingsValues()],
    ["lutris", context.Amaterasu.saveLutrisSettings, lutrisSettingsValues()],
  ]) {
    const current = { ok: true, revision: 7, values };
    const confirmed = { ok: true, revision: 8, values };
    for (const update of [
      { ...confirmed, revision: 9 },
      { ...confirmed, ok: false },
      { ...confirmed, values: { ...values, extra: true } },
      { ...confirmed, values: { ...values, auto_sync: !values.auto_sync } },
    ]) {
      const routes = [];
      await assert.rejects(save({ async post(route) {
        routes.push(route);
        return update;
      } }, current, values));
      assert.deepEqual(routes, [`${provider}/settings_update`]);
    }
    for (const readback of [
      { ...confirmed, revision: 9 },
      { ...confirmed, values: { ...values, auto_sync: !values.auto_sync } },
      { ...confirmed, values: { ...values, exclude_games: [] } },
    ]) {
      await assert.rejects(save({ async post(route) {
        return route === `${provider}/settings_update` ? confirmed : readback;
      } }, current, values));
    }
  }
});

test("Steam and Lutris reconcile uncertain outcomes using their distinct provider codes", async () => {
  class ApiError extends Error {
    constructor(code) { super(code); this.code = code; }
  }
  context.ApiError = ApiError;
  for (const [provider, code, save, values] of [
    ["steam", "settings_outcome_unknown", context.Amaterasu.saveSteamSettings, steamSettingsValues()],
    ["lutris", "update_outcome_unknown", context.Amaterasu.saveLutrisSettings, lutrisSettingsValues()],
  ]) {
    const current = { ok: true, revision: 7, values };
    const committed = { ok: true, revision: 8, values };
    const api = (readback) => ({ async post(route) {
      if (route === `${provider}/settings_update`) throw new ApiError(code);
      assert.equal(route, `${provider}/settings`);
      return readback;
    } });
    assert.deepEqual(ordinary(await save(api(committed), current, values)), committed);
    await assert.rejects(save(api({ ...committed, values: { ...values, auto_sync: !values.auto_sync } }),
      current, values), /uncertain/);
  }
});

test("Import settings load gate discards old settings and status after refresh or save", async () => {
  const gate = new context.Amaterasu.ImportSettingsLoadGate();
  let resolveOld;
  const old = gate.read(() => new Promise((resolve) => { resolveOld = resolve; }));
  const latest = gate.read(async () => ({ revision: 9 }));
  resolveOld({ revision: 8 });
  assert.equal(await old, null);
  assert.equal((await latest).value.revision, 9);
  let rejectOld;
  const failing = gate.read(() => new Promise((_, reject) => { rejectOld = reject; }));
  gate.invalidate();
  rejectOld(new Error("stale status failed"));
  assert.equal(await failing, null);
});

test("Lutris exclusion editor preserves ID precedence and bounds entries", () => {
  const parse = context.Amaterasu.parseLutrisExclusions;
  const exclusions = ["0007", { id: "42", name: "Old title" }, { name: "Portal 2" }];
  assert.deepEqual(ordinary(parse(JSON.stringify(exclusions))), exclusions);
  assert.deepEqual(ordinary(parse("[]")), []);
  for (const value of [
    "not JSON",
    "{}",
    "null",
    JSON.stringify([0]),
    JSON.stringify(["0"]),
    JSON.stringify(["18446744073709551616"]),
    JSON.stringify([{}]),
    JSON.stringify([{ id: "42", owner: "other" }]),
    JSON.stringify([{ name: "  " }]),
    JSON.stringify([{ name: "bad\nname" }]),
    JSON.stringify([{ id: "42", name: "x".repeat(513) }]),
    JSON.stringify(["42", { id: "42", name: "Duplicate ID" }]),
    JSON.stringify([{ name: "Portal" }, { name: "portal" }]),
    JSON.stringify(Array.from({ length: 257 }, (_, index) => String(index + 1))),
    JSON.stringify(Array.from({ length: 24 }, (_, index) => ({
      name: `${index}-${"x".repeat(500)}`,
    }))),
  ]) {
    assert.throws(() => parse(value));
  }
});

test("Lutris status identifies unsupported platforms and discovery blockers", () => {
  const describe = context.Amaterasu.lutrisStatusDescription;
  const base = {
    ok: true,
    settings_ready: true,
    enabled: true,
    auto_sync: true,
    autosync_remove_uninstalled: true,
    include_steam: false,
    available: true,
    platform_supported: true,
    database_available: true,
    executable_available: true,
    settings_revision: 1,
    game_count: 3,
    refresh_pending: false,
    last_error: null,
  };
  assert.match(describe({ ...base, settings_ready: false }), /still loading/);
  assert.match(describe({ ...base, platform_supported: false }), /only on Linux/);
  assert.match(describe({ ...base, enabled: false }), /disabled/);
  assert.match(describe({ ...base, database_available: false }), /database is unavailable/);
  assert.match(describe({ ...base, executable_available: false }), /executable was not found/);
  assert.match(describe({ ...base, refresh_pending: true }), /3 games.*in progress/);
  assert.match(describe({ ...base, last_error: "scan failed" }), /Last scan issue: scan failed/);
});

test("Steam import controls preserve booleans and bounded recent-game values", () => {
  const parseImport = context.Amaterasu.parseSteamImportSettings;
  assert.deepEqual(ordinary(parseImport(false, true, true, 10, 30)), {
    include_tools: false,
    sync_all_installed: true,
    autosync_remove_uninstalled: true,
    recent_games: 10,
    recent_max_age_days: 30,
  });
  assert.deepEqual(ordinary(parseImport(true, false, false, 0, 0)), {
    include_tools: true,
    sync_all_installed: false,
    autosync_remove_uninstalled: false,
    recent_games: 0,
    recent_max_age_days: 0,
  });
  assert.deepEqual(ordinary(parseImport(false, false, true, 10_000, 36_500)), {
    include_tools: false,
    sync_all_installed: false,
    autosync_remove_uninstalled: true,
    recent_games: 10_000,
    recent_max_age_days: 36_500,
  });
  for (const [include, all, remove, recent, age] of [
    [0, false, true, 10, 30],
    [false, "true", true, 10, 30],
    [false, false, "true", 10, 30],
    [false, false, true, -1, 30],
    [false, false, true, 10_001, 30],
    [false, false, true, 1.5, 30],
    [false, false, true, 10, -1],
    [false, false, true, 10, 36_501],
    [false, false, true, 10, 30.5],
  ]) {
    assert.throws(() => parseImport(include, all, remove, recent, age));
  }
});

test("Steam exclusion editor rejects malformed or ambiguous entries", () => {
  const invalid = [
    "not JSON",
    "{}",
    "null",
    JSON.stringify([{}]),
    JSON.stringify([{ id: 730 }]),
    JSON.stringify([{ id: "0" }]),
    JSON.stringify([{ id: "0730" }]),
    JSON.stringify([{ id: "4294967296" }]),
    JSON.stringify([{ name: "  " }]),
    JSON.stringify([{ name: "bad\nname" }]),
    JSON.stringify([{ name: "x".repeat(513) }]),
    JSON.stringify([{ id: "730", owner: "other" }]),
    JSON.stringify([{ id: "730" }, { id: "730", name: "Renamed" }]),
    JSON.stringify([{ name: "Portal 2" }, { name: "portal 2" }]),
  ];
  for (const value of invalid) assert.throws(() => parse(value));
});

test("Steam exclusion editor accepts 256 entries and rejects the next one", () => {
  const exclusions = Array.from({ length: 256 }, (_, index) => ({
    id: String(index + 1),
  }));
  assert.equal(parse(JSON.stringify(exclusions)).length, 256);
  assert.throws(() => parse(JSON.stringify([...exclusions, { id: "257" }])), /256/);
});

test("Steam settings show provider loading, disabled and scan states honestly", () => {
  const describe = context.Amaterasu.steamStatusDescription;
  const base = {
    settings_ready: true,
    enabled: true,
    scanning: false,
    complete: false,
    game_count: 4,
    warning_count: 1,
  };
  assert.match(
    describe({ ...base, settings_ready: false }),
    /not confirmed/,
  );
  assert.match(describe({ ...base, enabled: false }), /disabled/);
  assert.match(describe({ ...base, scanning: true }), /scan in progress/);
  assert.match(describe({ ...base, complete: true }), /last scan complete/);
  assert.match(describe(base), /no complete scan/);
  assert.match(describe(base), /4 games · 1 warning/);
});
