import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ TextEncoder });
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

test("Steam exclusions preserve valid ID and name objects without normalization", () => {
  const exclusions = [
    { id: "730", name: "Counter-Strike 2" },
    { name: "Portal 2" },
  ];
  assert.deepEqual(ordinary(parse(JSON.stringify(exclusions))), exclusions);
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
