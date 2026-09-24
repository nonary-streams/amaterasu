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

test("input policy editor preserves booleans and reports pending cleanup honestly", () => {
  const parsePolicy = context.Amaterasu.parseInputPolicy;
  assert.deepEqual(ordinary(parsePolicy(false, true)), {
    keyboard: false,
    mouse: true,
  });
  assert.throws(() => parsePolicy("false", true));
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

test("Steam exclusions preserve valid ID and name objects without normalization", () => {
  const exclusions = [
    { id: "730", name: "Counter-Strike 2" },
    { name: "Portal 2" },
  ];
  assert.deepEqual(ordinary(parse(JSON.stringify(exclusions))), exclusions);
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
