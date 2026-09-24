import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ FormData, Error, TextEncoder });
vm.runInContext(
  ts.transpile(readFileSync("src/model.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
const model = context.Amaterasu;
const ordinary = (value) => JSON.parse(JSON.stringify(value));

test("executable arguments stay literal and application metadata is not executed", () => {
  const form = new FormData();
  form.set("name", "  Example  ");
  form.set("kind", "executable");
  form.set("path", "/opt/example");
  form.set("arguments", '["--flag", "$(do-not-run)", "a b"]');
  form.set("tags", "arcade, co-op, ");
  form.set("auto_detach", "on");
  form.set("wait_all", "on");
  form.set("exit_timeout_seconds", "5");
  const entry = ordinary(model.entryFromForm(form));
  assert.deepEqual(entry.launch, {
    kind: "executable",
    program: "/opt/example",
    arguments: ["--flag", "$(do-not-run)", "a b"],
    working_directory: null,
    auto_detach: true,
    wait_all: true,
    exit_timeout_seconds: 5,
  });
  assert.equal(entry.name, "Example");
  assert.deepEqual(entry.tags, ["arcade", "co-op"]);
});

test("invalid argument arrays fail before sending mutations", () => {
  for (const value of ["{}", "[1]", "[null]", '["ok", {}]', "not JSON"]) {
    assert.throws(() => model.parseArguments(value));
  }
});

test("global preparation editor validates bounded native command steps", () => {
  const parse = model.parsePreparationSteps;
  assert.deepEqual(ordinary(parse("")), []);
  const valid = [
    {
      run: {
        executable: "/opt/tools/prepare",
        args: ["--safe", "é"],
        environment: { MODE: "safe" },
        cwd: "/opt/tools",
        timeout_ms: 30000,
      },
      undo: null,
    },
  ];
  assert.deepEqual(ordinary(parse(JSON.stringify(valid))), valid);
  for (const value of [
    "{",
    "{}",
    JSON.stringify(Array(9).fill({ run: { executable: "/bin/true" } })),
    JSON.stringify([{ run: { executable: "relative.exe" } }]),
    JSON.stringify([{ run: { executable: "/bin/tool.cmd" } }]),
    JSON.stringify([{ run: { executable: "/bin/tool", cwd: "relative" } }]),
    JSON.stringify([{ run: { executable: "/bin/tool", timeout_ms: 0 } }]),
    JSON.stringify([{ run: { executable: "/bin/tool", unsupported: true } }]),
    JSON.stringify([{ run: { executable: "/bin/tool", args: Array(65).fill("x") } }]),
    JSON.stringify([{ run: { executable: "/bin/tool", environment: { PATH: "a", Path: "b" } } }]),
    JSON.stringify([{ run: { executable: "/bin/tool", environment: { "BAD-KEY": "x" } } }]),
  ]) assert.throws(() => parse(value));
});

test("manual Playnite Fullscreen entries use a typed bounded launch", () => {
  const form = new FormData();
  form.set("name", "Fullscreen");
  form.set("kind", "playnite_fullscreen");
  form.set("path", "C:\\Ignored\\Playnite.exe");
  form.set("steam_id", "123");
  form.set("steam_install_dir", "C:\\Ignored");
  form.set("exit_timeout_seconds", "invalid inactive value");
  form.set("detached", "bad\tcommand");
  form.set("preparation", "invalid inactive value");
  form.set("fullscreen_exit_timeout_seconds", "0");
  const entry = ordinary(model.entryFromForm(form));
  assert.deepEqual(entry.launch, { kind: "playnite_fullscreen", exit_timeout_seconds: 0 });
  assert.equal(Object.hasOwn(entry, "steam_id"), false);
  assert.equal(Object.hasOwn(entry, "steam_install_dir"), false);
  form.delete("fullscreen_exit_timeout_seconds");
  assert.equal(model.entryFromForm(form).launch.exit_timeout_seconds, 10);
  for (const timeout of ["-1", "301", "1.5", "NaN"]) {
    form.set("fullscreen_exit_timeout_seconds", timeout);
    assert.throws(() => model.entryFromForm(form));
  }
});

test("audio sink overrides distinguish inheritance from explicit automatic", () => {
  assert.equal(model.parseAudioSinkOverride("inherit", "ignored"), null);
  assert.equal(model.parseAudioSinkOverride("automatic", "ignored"), "");
  assert.equal(model.parseAudioSinkOverride("specific", "  exact endpoint  "), "  exact endpoint  ");
  for (const [mode, endpoint] of [["specific", ""], ["specific", "bad\nendpoint"], ["specific", "x".repeat(4097)], ["unknown", "id"]])
    assert.throws(() => model.parseAudioSinkOverride(mode, endpoint));
  const form = new FormData();
  form.set("name", "Audio app");
  form.set("kind", "application");
  form.set("path", "/opt/audio");
  form.set("audio_sink_mode", "automatic");
  assert.equal(model.entryFromForm(form).config_overrides.audio_sink, "");
  form.set("audio_sink_mode", "inherit");
  assert.equal(Object.hasOwn(model.entryFromForm(form), "config_overrides"), false);
});

test("Back button timeout overrides preserve inheritance and signed i32 values", () => {
  const parse = model.parseBackButtonTimeoutOverride;
  assert.equal(parse(""), null);
  assert.equal(parse("  "), null);
  assert.equal(parse("-1"), -1);
  assert.equal(parse("0"), 0);
  assert.equal(parse("-2147483648"), -2147483648);
  assert.equal(parse("2147483647"), 2147483647);
  for (const value of ["-2147483649", "2147483648", "1.5", "1e2", "+1", "abc"])
    assert.throws(() => parse(value));
});

test("application editor saves sparse FEC and executable lifecycle fields", () => {
  const form = new FormData();
  form.set("name", "Policy app");
  form.set("kind", "executable");
  form.set("path", "C:\\Games\\policy.exe");
  form.set("arguments", '["--fullscreen"]');
  form.set("working_directory", "");
  form.set("wait_all", "on");
  form.set("exit_timeout_seconds", "0");
  form.set("fec_percentage", "37");
  form.set("stream_audio", "false");
  form.set("back_button_timeout", "-1");
  form.set("exclude_global_prep_cmd", "on");
  form.set("detached", "first --ordered\n\nsecond");
  form.set(
    "preparation",
    JSON.stringify([{ run: { executable: "C:\\Tools\\prepare.exe" } }]),
  );
  const entry = ordinary(model.entryFromForm(form));
  assert.equal(entry.exclude_global_prep_cmd, true);
  assert.deepEqual(entry.config_overrides, { fec_percentage: 37, stream_audio: false, back_button_timeout: -1 });
  assert.deepEqual(entry.launch, {
    kind: "executable",
    program: "C:\\Games\\policy.exe",
    arguments: ["--fullscreen"],
    working_directory: null,
    auto_detach: false,
    wait_all: true,
    exit_timeout_seconds: 0,
    detached: ["first --ordered", "second"],
    preparation: [{ run: { executable: "C:\\Tools\\prepare.exe" } }],
  });
  form.delete("exclude_global_prep_cmd");
  assert.equal(model.entryFromForm(form).exclude_global_prep_cmd, false);
});

test("blank FEC and timeout retain sparse inheritance and explicit bounds", () => {
  assert.equal(model.parseFecOverride("  "), null);
  assert.deepEqual(ordinary(model.providerConfigOverrides("")), {
    fec_percentage: null,
    stream_audio: null,
    audio_sink: null,
    virtual_sink: null,
    audio_sink_capture_only: null,
    back_button_timeout: null,
  });
  assert.deepEqual(ordinary(model.providerConfigOverrides("37", "false")), {
    fec_percentage: 37,
    stream_audio: false,
    audio_sink: null,
    virtual_sink: null,
    audio_sink_capture_only: null,
    back_button_timeout: null,
  });
  assert.deepEqual(ordinary(model.providerConfigOverrides("", "", "inherit", "", "false")), {
    fec_percentage: null,
    stream_audio: null,
    audio_sink: null,
    virtual_sink: null,
    audio_sink_capture_only: false,
    back_button_timeout: null,
  });
  assert.deepEqual(ordinary(model.providerConfigOverrides("", "", "inherit", "", "", "-1")), {
    fec_percentage: null,
    stream_audio: null,
    audio_sink: null,
    virtual_sink: null,
    audio_sink_capture_only: null,
    back_button_timeout: -1,
  });
  assert.equal(model.parseAudioSinkCaptureOnlyOverride(""), null);
  assert.equal(model.parseAudioSinkCaptureOnlyOverride("true"), true);
  assert.equal(model.parseAudioSinkCaptureOnlyOverride("false"), false);
  assert.throws(() => model.parseAudioSinkCaptureOnlyOverride("False"));
  assert.equal(model.parseVirtualSinkOverride("inherit", ""), null);
  assert.equal(model.parseVirtualSinkOverride("automatic", "ignored"), "");
  assert.equal(model.parseVirtualSinkOverride("specific", "Steam Virtual Speakers"), "Steam Virtual Speakers");
  assert.throws(() => model.parseVirtualSinkOverride("specific", "bad\nselector"));
  assert.throws(() => model.parseVirtualSinkOverride("specific", "x".repeat(4097)));
  assert.equal(model.parseStreamAudioOverride(""), null);
  assert.equal(model.parseStreamAudioOverride("true"), true);
  assert.equal(model.parseStreamAudioOverride("false"), false);
  for (const value of ["TRUE", "0", "off", " false "])
    assert.throws(() => model.parseStreamAudioOverride(value));
  const audioOnly = new FormData();
  audioOnly.set("name", "Silent app");
  audioOnly.set("kind", "application");
  audioOnly.set("path", "/opt/silent");
  audioOnly.set("stream_audio", "false");
  assert.deepEqual(ordinary(model.entryFromForm(audioOnly).config_overrides), {
    stream_audio: false,
  });
  audioOnly.set("audio_sink_capture_only", "false");
  assert.deepEqual(ordinary(model.entryFromForm(audioOnly).config_overrides), {
    stream_audio: false,
    audio_sink_capture_only: false,
  });
  audioOnly.set("virtual_sink_mode", "automatic");
  assert.deepEqual(ordinary(model.entryFromForm(audioOnly).config_overrides), {
    stream_audio: false,
    audio_sink_capture_only: false,
    virtual_sink: "",
  });
  assert.equal(model.parseExitTimeout(""), null);
  assert.equal(model.parseExitTimeout("0"), 0);
  assert.equal(model.parseExitTimeout("300"), 300);
  assert.deepEqual(ordinary(model.parseDetachedCommands("one\n\n two ")), ["one", "two"]);
  for (const value of ["0", "256", "1.5", "-1", "NaN"]) {
    assert.throws(() => model.parseFecOverride(value));
  }
  for (const value of ["-1", "301", "1.5", "NaN"]) {
    assert.throws(() => model.parseExitTimeout(value));
  }
  assert.throws(() => model.parseDetachedCommands(Array(17).fill("run").join("\n")));
  assert.throws(() => model.parseDetachedCommands("bad\tcommand"));
  assert.throws(() => model.parseDetachedCommands("é".repeat(1025)));
});

test("Steam metadata is sparse and rejects ambiguous IDs or directory roots", () => {
  const form = new FormData();
  form.set("name", "Steam game");
  form.set("kind", "executable");
  form.set("path", "/games/game");
  form.set("steam_id", "4294967295");
  form.set("steam_install_dir", "/opt/games/steam-game");
  const entry = ordinary(model.entryFromForm(form));
  assert.equal(entry.steam_id, "4294967295");
  assert.equal(entry.steam_install_dir, "/opt/games/steam-game");
  form.set("steam_id", "");
  form.set("steam_install_dir", "");
  const sparse = ordinary(model.entryFromForm(form));
  assert.equal(Object.hasOwn(sparse, "steam_id"), false);
  assert.equal(Object.hasOwn(sparse, "steam_install_dir"), false);
  for (const value of ["0", "01", "4294967296", "-1", "1.0", " 1 "])
    assert.throws(() => model.parseSteamId(value));
  for (const value of ["/", "C:\\", "\\foo", "\\\\server\\share", "relative/game", "/opt/../game", " /opt/game", "/opt/game\n"])
    assert.throws(() => model.parseSteamInstallDir(value));
  assert.equal(model.parseSteamInstallDir("C:\\Games\\Game"), "C:\\Games\\Game");
  assert.equal(model.parseSteamInstallDir("\\\\server\\share\\game"), "\\\\server\\share\\game");
});

test("integer settings reject blank, fractional and unsafe integer values", () => {
  for (const value of ["", "  ", "1.5", "9007199254740992", "NaN"]) {
    assert.throws(() => model.fieldValue({ type: "integer" }, { value }));
  }
  assert.equal(model.fieldValue({ type: "integer" }, { value: "-42" }), -42);
  assert.equal(
    model.fieldValue({ type: "boolean" }, { checked: false }),
    false,
  );
  assert.equal(
    model.fieldValue({ type: "string" }, { value: "<img src=x>" }),
    "<img src=x>",
  );
});

test("structured settings reject malformed or mismatched JSON before saving", () => {
  const list = { type: "list" };
  const object = { type: "object" };
  for (const value of ["[", "{}", "null", "42"]) {
    assert.throws(() => model.fieldValue(list, { value }));
  }
  for (const value of ["{", "[]", "null", '"text"']) {
    assert.throws(() => model.fieldValue(object, { value }));
  }
  assert.deepEqual(ordinary(model.fieldValue(list, { value: '["left", "right"]' })), ["left", "right"]);
  assert.deepEqual(ordinary(model.fieldValue(object, { value: '{"enabled":true}' })), { enabled: true });
});

test("decimal and enum settings validate numeric bounds and listed choices", () => {
  const decimal = { type: "decimal", minimum: 0, maximum: 2 };
  for (const value of ["", "NaN", "Infinity", "-0.1", "2.1"]) {
    assert.throws(() => model.fieldValue(decimal, { value }));
  }
  assert.equal(model.fieldValue(decimal, { value: "1.25" }), 1.25);
  const choice = { type: "enum", options: ["fast", "balanced"] };
  assert.equal(model.fieldValue(choice, { value: "balanced" }), "balanced");
  assert.throws(() => model.fieldValue(choice, { value: "unsupported" }));
});

test("page counts never claim an exact total while continuation exists", () => {
  assert.equal(model.countLabel(12, { after: "next" }), "12+");
  assert.equal(model.countLabel(0, { after: "continue-search" }), "0+");
  assert.equal(model.countLabel(12, null), "12");
});
