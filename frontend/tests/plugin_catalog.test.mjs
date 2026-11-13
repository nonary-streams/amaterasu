import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ TextEncoder });
vm.runInContext(
  ts.transpile(readFileSync("src/plugin_catalog.ts", "utf8"), { target: ts.ScriptTarget.ES2022 }),
  context,
);
const plugins = context.Amaterasu;
const plain = (value) => JSON.parse(JSON.stringify(value));

const candidate = {
  slot: "amaterasu.ui",
  implementation: "publisher.console",
  version: "4",
  kind: "web_bundle",
  manifest_sha256: "a".repeat(64),
};
const nativeCandidate = {
  slot: "amaterasu.frame-limiter",
  implementation: "publisher-frame-pacer",
  version: "0.3.0",
  kind: "native_set",
  manifest_sha256: "b".repeat(64),
};
const repository = {
  id: "publisher",
  location: "https://updates.example.test",
  fingerprint: "c".repeat(64),
  revoked: false,
  candidates: [candidate],
  catalog: { state: "current", sequence: "18446744073709551615", expires_unix: 1_800_000_000 },
};
const selectionReply = {
  ok: true,
  revision: "f".repeat(64),
  active: "d".repeat(64),
  previous: null,
  pending: null,
  trial: null,
  highest_sequence: "7",
  running: {
    available: true,
    digest: "d".repeat(64),
    modules: [
      { id: "amaterasu.runtime", version: "0.1.0", sha256: "1".repeat(64), runtime: true },
      { id: "amaterasu.frame-limiter", version: "0.1.0", sha256: "2".repeat(64), runtime: false },
      { id: "example.overlay", version: "1.2.0", sha256: "3".repeat(64), runtime: false },
    ],
    source: { repository: "publisher", fingerprint: "c".repeat(64) },
  },
};

test("repository pages keep catalog status, the running change and management support", () => {
  const page = plugins.parseRepositoryPage({
    ok: true,
    repositories: [repository],
    next_offset: null,
    manageable: true,
    capacity: 16,
    operation: { id: 3, repository: "publisher", kind: "refresh", state: "failed", code: "catalog_expired" },
  });
  assert.equal(page.repositories[0].candidates[0].slot, "amaterasu.ui");
  assert.deepEqual(plain(page.repositories[0].catalog),
    { state: "current", sequence: "18446744073709551615", expiresUnix: 1_800_000_000 });
  assert.equal(page.manageable, true);
  assert.equal(page.capacity, 16);
  assert.deepEqual(plain(page.operation),
    { id: 3, repository: "publisher", kind: "refresh", state: "failed", code: "catalog_expired" });
  assert.equal(page.nextOffset, null);
});

test("a host that only lists repositories is read as unmanageable, not as an error", () => {
  const { catalog: _catalog, ...listed } = repository;
  const page = plugins.parseRepositoryPage({ ok: true, repositories: [listed], next_offset: 32 });
  assert.equal(page.manageable, false);
  assert.equal(page.capacity, null);
  assert.equal(page.operation, null);
  assert.equal(page.nextOffset, 32);
  assert.equal(page.repositories[0].catalog.state, "unknown");
});

test("malformed repository replies fail closed", () => {
  for (const value of [
    { ok: true, repositories: [{ ...repository, candidates: [{ ...candidate, kind: "script" }] }], next_offset: null },
    { ok: true, repositories: [repository], next_offset: 4097 },
    { ok: true, repositories: [repository], next_offset: "1" },
    { ok: false, repositories: [repository], next_offset: null },
    { ok: true, repositories: [{ ...repository, catalog: { state: "current", sequence: 7, expires_unix: 1 } }], next_offset: null },
    { ok: true, repositories: [{ ...repository, catalog: { state: "trusted" } }], next_offset: null },
    { ok: true, repositories: [repository], next_offset: null, operation: { id: 0, repository: "publisher", kind: "refresh", state: "running", code: null } },
    { ok: true, repositories: [repository], next_offset: null, operation: { id: 1, repository: "publisher", kind: "install", state: "running", code: null } },
  ]) {
    assert.throws(() => plugins.parseRepositoryPage(value));
  }
});

test("package selection keeps persisted phases, the running package and its source only", () => {
  const parsed = plugins.parsePackageSelectionStatus({ ...selectionReply, package_root: "/private/host/root" });
  assert.equal(parsed.active, "d".repeat(64));
  assert.equal(parsed.highestSequence, "7");
  assert.equal(parsed.running.modules.length, 3);
  assert.deepEqual(plain(parsed.running.source), { repository: "publisher", fingerprint: "c".repeat(64) });
  assert.equal(Object.hasOwn(parsed, "package_root"), false);

  const idle = plugins.parsePackageSelectionStatus({ ...selectionReply, running: { available: false } });
  assert.deepEqual(plain(idle.running), { available: false, digest: null });

  const unavailable = { available: false };
  for (const reply of [
    { ok: false, code: "package_selection_unavailable" },
    { ...selectionReply, active: "A".repeat(64) },
    { ...selectionReply, highest_sequence: "18446744073709551616" },
    { ...selectionReply, revision: "not-a-revision" },
    { ...selectionReply, running: { available: true, digest: "/unsafe/path" } },
    { ...selectionReply, running: { ...unavailable, digest: "d".repeat(64) } },
    { ...selectionReply, running: { ...selectionReply.running, source: { repository: "../escape", fingerprint: "c".repeat(64) } } },
    { ...selectionReply, running: { ...selectionReply.running, source: { repository: "publisher", fingerprint: "short" } } },
  ]) assert.throws(() => plugins.parsePackageSelectionStatus(reply));
});

test("the loaded module inventory is bounded and well formed", () => {
  const modules = selectionReply.running.modules;
  assert.equal(plugins.parseRunningModules(modules)[0].id, "amaterasu.runtime");
  for (const bad of [
    [],
    [...modules, modules[0]],
    Array(129).fill(modules[0]),
    [{ ...modules[0], sha256: "invalid" }],
    [{ ...modules[0], version: "bad\nversion" }],
    [{ ...modules[0], id: "../display" }],
  ]) assert.throws(() => plugins.parseRunningModules(bad));
});

test("a module description is read whole or not at all", () => {
  const reply = {
    ok: true,
    module: {
      id: "amaterasu.display", version: "0.2.0", sha256: "b".repeat(64), runtime: false,
      size: 2_971_272, abi_major: 1,
      requires: [{ id: "amaterasu.runtime", version: "0.1.0" }],
      required_by: ["example.overlay"],
      provides: ["amaterasu.display/1"], calls: ["amaterasu.state/1"],
      native_provides: [], native_uses: [], publishes: [], subscribes: [], hooks: [],
    },
  };
  const described = plugins.parseModuleDescription(reply);
  assert.equal(described.abiMajor, 1);
  assert.deepEqual(plain(described.requires), [{ id: "amaterasu.runtime", version: "0.1.0" }]);
  assert.deepEqual(plain(described.requiredBy), ["example.overlay"]);
  const broken = (change) => ({ ok: true, module: { ...reply.module, ...change } });
  for (const value of [
    { ok: false, code: "module_not_found" },
    broken({ size: -1 }),
    broken({ sha256: "nope" }),
    broken({ requires: [{ id: "Bad Id", version: "1" }] }),
    broken({ required_by: ["../escape"] }),
    broken({ provides: ["line\nbreak"] }),
    broken({ calls: undefined }),
  ]) assert.throws(() => plugins.parseModuleDescription(value));
});

test("known modules get a name and a place; unknown ones are named from their identity", () => {
  assert.deepEqual(plain(plugins.pluginPresentation("amaterasu.rtx-hdr")),
    { name: "RTX HDR", summary: "Converts SDR streams to HDR with NVIDIA RTX.", category: "Video" });
  assert.deepEqual(plain(plugins.pluginPresentation("okami.discord-presence")),
    { name: "Discord presence", summary: "", category: "Other" });
  assert.equal(plugins.pluginPresentation("solo").name, "Solo");

  const installed = plugins.installedPlugins(plugins.parsePackageSelectionStatus(selectionReply));
  assert.deepEqual(installed.map((plugin) => [plugin.category, plugin.name]),
    [["Core", "Runtime"], ["Video", "Frame limiter"], ["Other", "Overlay"]]);
  assert.deepEqual(plain(plugins.installedPlugins(null)), []);
});

test("offers are compared with what the host runs", () => {
  const selection = plugins.parsePackageSelectionStatus(selectionReply);
  const listed = plugins.parseRepositoryList({
    ok: true,
    repositories: [
      { ...repository, candidates: [
        candidate,
        nativeCandidate,
        { ...nativeCandidate, version: "0.10.0", manifest_sha256: "e".repeat(64) },
        { ...nativeCandidate, version: "0.2.0", manifest_sha256: "9".repeat(64) },
        { slot: "example.overlay", implementation: "example-overlay", version: "1.2.0", kind: "native_set", manifest_sha256: "d".repeat(64) },
        { slot: "example.recorder", implementation: "example-recorder", version: "2.0.0", kind: "native_set", manifest_sha256: "7".repeat(64) },
      ] },
      { ...repository, id: "revoked", revoked: true, candidates: [nativeCandidate] },
    ],
  });
  const offers = plugins.availablePlugins(listed, selection, "a".repeat(64));
  const bySlot = Object.fromEntries(offers.map((offer) => [offer.slot, offer]));
  assert.equal(offers.length, 4, "a revoked repository offers nothing and versions collapse into one offer");

  // The newest version leads; 0.10.0 is newer than 0.3.0.
  assert.equal(bySlot["amaterasu.frame-limiter"].version, "0.10.0");
  assert.deepEqual(plain(bySlot["amaterasu.frame-limiter"].otherVersions).sort(), ["0.2.0", "0.3.0"]);
  assert.equal(bySlot["amaterasu.frame-limiter"].status, "update");
  assert.equal(bySlot["amaterasu.frame-limiter"].installedVersion, "0.1.0");
  assert.equal(plugins.availabilityLabel(bySlot["amaterasu.frame-limiter"]), "Other version installed");

  assert.equal(bySlot["example.overlay"].status, "installed");
  assert.equal(bySlot["example.overlay"].runningThisPackage, true);
  assert.equal(bySlot["example.recorder"].status, "available");
  assert.equal(plugins.availabilityLabel(bySlot["example.recorder"]), "Not installed");

  // The console interface is in use only when this page was loaded from it.
  assert.equal(bySlot["amaterasu.ui"].status, "installed");
  assert.equal(plugins.availablePlugins(listed, selection, null)[0].status, "available");
  assert.equal(plugins.availablePlugins(listed, selection, "0".repeat(64))[0].status, "available");
  assert.equal(plugins.consoleDigestFromAssets([
    "https://host.example/styles.css",
    `https://host.example/web/${"a".repeat(64)}/app.js`,
  ]), "a".repeat(64));
  assert.equal(plugins.consoleDigestFromAssets(["https://host.example/app.js"]), null);
});

test("search matches every word against the name, summary, identity and source", () => {
  const overlay = { ...plugins.pluginPresentation("example.overlay"), id: "example.overlay" };
  const hdr = { ...plugins.pluginPresentation("amaterasu.rtx-hdr"), slot: "amaterasu.rtx-hdr", repository: { id: "publisher" } };
  const matches = (plugin, query, category = "") => plugins.matchesPluginFilter(plugin, { query, category });
  assert.equal(matches(hdr, ""), true);
  assert.equal(matches(hdr, "  NVIDIA hdr "), true);
  assert.equal(matches(hdr, "publisher"), true);
  assert.equal(matches(hdr, "nvidia audio"), false);
  assert.equal(matches(hdr, "", "Video"), true);
  assert.equal(matches(hdr, "", "Core"), false);
  assert.equal(matches(overlay, "example.over"), true);
  assert.equal(matches(overlay, "hdr"), false);
});

test("versions order number by number", () => {
  const sorted = ["0.10.0", "0.9.0", "1.0.0-beta", "1.0.0", "0.9"].sort(plugins.compareVersions);
  assert.deepEqual(sorted, ["0.9", "0.9.0", "0.10.0", "1.0.0-beta", "1.0.0"]);
  assert.equal(plugins.compareVersions("2", "2"), 0);
});

test("digests, fingerprints and sizes are shown in a readable form", () => {
  assert.equal(plugins.shortDigest("3f700dfd124f4e6139ed32e8c8dfedd7"), "3f700dfd124f");
  assert.equal(plugins.groupedFingerprint("c412da5820e3"), "c412 da58 20e3");
  assert.equal(plugins.formatBytes(512), "512 B");
  assert.equal(plugins.formatBytes(4_577_712), "4.4 MB");
  assert.equal(plugins.formatBytes(268_435_456), "256 MB");
});

test("a repository's line says what it offers or what it needs", () => {
  const date = (unix) => `day ${unix}`;
  const listed = (change) => plugins.parseRepositoryList({ ok: true, repositories: [{ ...repository, ...change }] })[0];
  const summary = (change) => plain(plugins.summarizeRepository(listed(change), date));
  assert.deepEqual(summary({}), { status: "1 plugin · catalog valid until day 1800000000", attention: false });
  assert.deepEqual(summary({ candidates: [candidate, nativeCandidate] }).status, "2 plugins · catalog valid until day 1800000000");
  assert.equal(summary({ candidates: [], catalog: { state: "expired", expires_unix: 9 } }).attention, true);
  assert.match(summary({ candidates: [], catalog: { state: "expired", expires_unix: 9 } }).status, /expired day 9/);
  assert.match(summary({ candidates: [], catalog: { state: "missing" } }).status, /No catalog yet/);
  assert.match(summary({ candidates: [], catalog: { state: "unavailable" } }).status, /no longer verifies/);
  assert.deepEqual(summary({ revoked: true, candidates: [] }),
    { status: "Removed. This ID cannot be added again.", attention: false });
  assert.deepEqual(summary({ catalog: undefined }), { status: "1 plugin", attention: false });
});

test("every refusal the host can send has its own words and points at a field when one is wrong", () => {
  const fallback = plugins.repositoryChangeMessage(null);
  const source = readFileSync("../modules/management/src/repository_admin.rs", "utf8");
  const production = source.slice(0, source.indexOf("#[cfg(test)]"));
  const codes = new Set(Array.from(production.matchAll(/"((?:repository|catalog|invalid|fingerprint|shutting)_[a-z_]+)"/g), (match) => match[1]));
  for (const action of ["repository_add", "repository_refresh", "repository_remove"]) codes.delete(action);
  assert.ok(codes.size >= 15, `expected the host's refusal codes, found ${[...codes].join(", ")}`);
  for (const code of codes) {
    assert.notEqual(plugins.repositoryChangeMessage(code), fallback, `${code} has no message`);
  }
  assert.equal(plugins.repositoryChangeMessage("not_a_known_code"), fallback);
  assert.equal(plugins.repositoryChangeField("fingerprint_mismatch"), "fingerprint");
  assert.equal(plugins.repositoryChangeField("invalid_publisher_key"), "publicKey");
  assert.equal(plugins.repositoryChangeField("catalog_identity_mismatch"), "id");
  assert.equal(plugins.repositoryChangeField("catalog_download_failed"), "location");
  assert.equal(plugins.repositoryChangeField("repository_busy"), null);
});

test("pasted repository details fill the form but never supply the fingerprint", () => {
  const key = "3D4017C3E843895A92B70AA74D1B7EBC9C982CCF2EC4968CC0CD55F12AF4660C";
  assert.deepEqual(plain(plugins.parseRepositoryDescriptor(JSON.stringify({
    id: "example-plugins",
    location: "https://plugins.example.org/amaterasu",
    public_key: key,
    fingerprint: "f".repeat(64),
  }))), { id: "example-plugins", location: "https://plugins.example.org/amaterasu", publicKey: key.toLowerCase() });
  assert.deepEqual(plain(plugins.parseRepositoryDescriptor(' {"repository":"r","url":"https://r.example"} ')),
    { id: "r", location: "https://r.example" });
  for (const text of ["https://plugins.example.org", "{not json", "[]", "{}", '{"id":7}', `{"id":"${"x".repeat(5000)}"}`])
    assert.equal(plugins.parseRepositoryDescriptor(text), null);
});

// --- The Modules page: views, families, updates ------------------------------

const digest = (character) => character.repeat(64);
const updateModule = (id, change, from = "0.1.0", to = "0.2.0") => ({
  id,
  from: change === "added" ? null : from,
  to: change === "removed" ? null : change === "updated" || change === "added" ? to : from,
  change,
  size: 3_774_568,
});
const updateOffer = {
  repository: "amaterasu",
  slot: "amaterasu.runtime",
  implementation: "amaterasu-linux-session",
  version: "0.2.0",
  digest: digest("e"),
  sequence: "1794200000",
  modules: [
    updateModule("amaterasu.runtime", "unchanged"),
    updateModule("amaterasu.input", "updated"),
    updateModule("amaterasu.display", "rebuilt"),
    updateModule("amaterasu.remote-session", "added", null, "0.1.0"),
    updateModule("amaterasu.upnp", "removed"),
    updateModule("amaterasu.audio", "updated", "0.1.0", "0.1.1"),
  ],
  download_bytes: 18_200_000,
  total_bytes: 131_000_000,
};
const updateReply = {
  ok: true,
  supported: true,
  phase: "available",
  checked_unix_ms: 1_794_170_000_000,
  source: { repository: "amaterasu", fingerprint: digest("c") },
  running: { digest: digest("d"), sequence: "1794169005" },
  update: updateOffer,
  transferred_bytes: 0,
  error: null,
};
const reading = (change = {}) => ({ state: "ready", status: plugins.parsePackageUpdateStatus({ ...updateReply, ...change }) });

test("addresses saved under a view's former name open the view that replaced it", () => {
  assert.deepEqual(plain(plugins.PLUGINS_VIEWS), ["overview", "modules", "repositories"]);
  assert.equal(plugins.DEFAULT_PLUGINS_VIEW, "overview");
  for (const view of plugins.PLUGINS_VIEWS) assert.equal(plugins.pluginsViewFromAddress(view), view);
  assert.equal(plugins.pluginsViewFromAddress("installed"), "modules");
  // Core modules and other publishers' offers now share the overview.
  for (const former of ["core", "publishers", "available"]) assert.equal(plugins.pluginsViewFromAddress(former), "overview", former);
  for (const name of ["", "Installed", "updates", "constructor", "__proto__", "modules/extra"])
    assert.equal(plugins.pluginsViewFromAddress(name), null, name);
});

test("an update status reply is read whole", () => {
  const status = plugins.parsePackageUpdateStatus({ ...updateReply, package_root: "/private/host/root" });
  assert.deepEqual(plain(status), {
    supported: true,
    phase: "available",
    checkedUnixMs: 1_794_170_000_000,
    source: { repository: "amaterasu", fingerprint: digest("c") },
    running: { digest: digest("d"), sequence: "1794169005" },
    update: {
      repository: "amaterasu",
      slot: "amaterasu.runtime",
      implementation: "amaterasu-linux-session",
      version: "0.2.0",
      digest: digest("e"),
      sequence: "1794200000",
      modules: updateOffer.modules,
      downloadBytes: 18_200_000,
      totalBytes: 131_000_000,
    },
    transferredBytes: 0,
    error: null,
    automatic: null,
    repositoryBehind: false,
    versions: false,
    restoring: null,
  });

  // A host that keeps itself up to date and can switch packages says so.
  const kept = plugins.parsePackageUpdateStatus({
    ...updateReply, phase: "current", update: null, repository_behind: true,
    automatic: { mode: "install", next_check_unix_ms: 1_794_190_000_000 }, restoring: null,
  });
  assert.deepEqual(plain([kept.automatic, kept.repositoryBehind, kept.versions, kept.restoring]),
    [{ mode: "install", nextCheckUnixMs: 1_794_190_000_000 }, true, true, null]);
  assert.deepEqual(plain(plugins.parsePackageUpdateStatus({ ...updateReply, automatic: { mode: "off", next_check_unix_ms: null } }).automatic),
    { mode: "off", nextCheckUnixMs: null });
  const switching = plugins.parsePackageUpdateStatus(
    { ...updateReply, phase: "waiting_stream", update: null, restoring: { digest: digest("b"), sequence: "1794169001" } });
  assert.deepEqual(plain([switching.versions, switching.restoring]), [true, { digest: digest("b"), sequence: "1794169001" }]);
  for (const bad of [
    { automatic: { mode: "sometimes", next_check_unix_ms: null } },
    { automatic: { mode: "off", next_check_unix_ms: -1 } },
    { automatic: "install" },
    { repository_behind: "yes" },
    { restoring: { digest: "short", sequence: "1" } },
    { restoring: { digest: digest("b"), sequence: "01" } },
  ]) assert.throws(() => plugins.parsePackageUpdateStatus({ ...updateReply, ...bad }), JSON.stringify(bad));

  // Everything the contract allows to be null, and every phase.
  const idle = plugins.parsePackageUpdateStatus(
    { ...updateReply, phase: "idle", checked_unix_ms: null, source: null, update: null });
  assert.equal(idle.checkedUnixMs, null);
  assert.equal(idle.source, null);
  assert.equal(idle.update, null);
  for (const phase of ["idle", "checking", "current", "available", "downloading", "ready", "waiting_stream", "activating", "failed", "cancelled"])
    assert.equal(plugins.parsePackageUpdateStatus({ ...updateReply, phase }).phase, phase);
  assert.equal(plugins.parsePackageUpdateStatus({ ...updateReply, phase: "failed", error: "download_failed" }).error, "download_failed");
  assert.equal(plugins.parsePackageUpdateStatus({ ...updateReply, running: { digest: digest("d"), sequence: "18446744073709551615" } })
    .running.sequence, "18446744073709551615");

  // A host that has the routes but cannot install says only that.
  assert.deepEqual(plain(plugins.parsePackageUpdateStatus({ ok: true, supported: false })), { supported: false });
  assert.deepEqual(plain(plugins.parsePackageUpdateStatus({ ...updateReply, supported: false, phase: "nonsense" })), { supported: false });
});

test("a host that cannot say which package it loaded still reports update status", () => {
  const status = plugins.parsePackageUpdateStatus({ ...updateReply, running: { digest: null, sequence: null } });
  assert.deepEqual(plain(status.running), { digest: null, sequence: null });
  // The row then keeps the digest the package selection reported.
  const row = plugins.coreUpdateSource({ state: "ready", status }, { digest: digest("e"), repository: "amaterasu" });
  assert.equal(row.running, digest("e"));
});

test("each malformed field of an update status reply is rejected", () => {
  const update = (change) => ({ ...updateReply, update: { ...updateOffer, ...change } });
  const modules = (...rows) => update({ modules: rows });
  const first = updateOffer.modules[1];
  const malformed = {
    "not an object": [],
    "refusal": { ok: false, code: "busy" },
    "ok missing": { ...updateReply, ok: undefined },
    "supported missing": { ...updateReply, supported: undefined },
    "supported not a boolean": { ...updateReply, supported: "true" },
    "unknown phase": { ...updateReply, phase: "installing" },
    "phase not a string": { ...updateReply, phase: 3 },
    "check time missing": { ...updateReply, checked_unix_ms: undefined },
    "check time a string": { ...updateReply, checked_unix_ms: "1794170000000" },
    "check time negative": { ...updateReply, checked_unix_ms: -1 },
    "check time fractional": { ...updateReply, checked_unix_ms: 1.5 },
    "check time beyond any date": { ...updateReply, checked_unix_ms: 8_640_000_000_000_001 },
    "source missing": { ...updateReply, source: undefined },
    "source repository": { ...updateReply, source: { repository: "../escape", fingerprint: digest("c") } },
    "source fingerprint": { ...updateReply, source: { repository: "amaterasu", fingerprint: "short" } },
    "running missing": { ...updateReply, running: null },
    "running digest": { ...updateReply, running: { digest: "/unsafe/path", sequence: "1" } },
    "running sequence a number": { ...updateReply, running: { digest: digest("d"), sequence: 1794169005 } },
    "running sequence padded": { ...updateReply, running: { digest: digest("d"), sequence: "01" } },
    "running digest without its sequence": { ...updateReply, running: { digest: digest("d"), sequence: null } },
    "running sequence without its digest": { ...updateReply, running: { digest: null, sequence: "1" } },
    "running sequence too large": { ...updateReply, running: { digest: digest("d"), sequence: "18446744073709551616" } },
    "update missing": { ...updateReply, update: undefined },
    "update not an object": { ...updateReply, update: "0.2.0" },
    "update repository": update({ repository: "Bad Id" }),
    "update slot": update({ slot: "" }),
    "update implementation": update({ implementation: "line\nbreak" }),
    "update version": update({ version: 2 }),
    "update digest": update({ digest: digest("E") }),
    "update sequence": update({ sequence: "-1" }),
    "download size": update({ download_bytes: -1 }),
    "download size a string": update({ download_bytes: "18200000" }),
    "total size": update({ total_bytes: 1.5 }),
    "modules missing": update({ modules: undefined }),
    "modules empty": modules(),
    "too many modules": modules(...Array.from({ length: 129 }, (_, index) => updateModule(`amaterasu.module-${index}`, "unchanged"))),
    "module not an object": modules("amaterasu.input"),
    "module id": modules({ ...first, id: "../input" }),
    "module listed twice": modules(first, first),
    "module version from": modules({ ...first, from: 1 }),
    "module version to": modules({ ...first, to: "bad\u0000version" }),
    "unknown change": modules({ ...first, change: "replaced" }),
    "module size": modules({ ...first, size: -5 }),
    "module size missing": modules({ ...first, size: undefined }),
    "added with a previous version": modules({ ...first, change: "added" }),
    "added without a new version": modules({ ...first, change: "added", from: null, to: null }),
    "removed with a new version": modules({ ...first, change: "removed" }),
    "updated without a previous version": modules({ ...first, from: null }),
    "unchanged without a version": modules({ ...first, change: "unchanged", to: null }),
    "progress missing": { ...updateReply, transferred_bytes: undefined },
    "progress negative": { ...updateReply, transferred_bytes: -1 },
    "progress a string": { ...updateReply, transferred_bytes: "0" },
    "error missing": { ...updateReply, error: undefined },
    "error an object": { ...updateReply, error: { code: "download_failed" } },
    "error not a code": { ...updateReply, error: "The download failed." },
  };
  // The parser's own refusal, not an accident further in.
  for (const [what, reply] of Object.entries(malformed))
    assert.throws(() => plugins.parsePackageUpdateStatus(reply), (error) => /is (invalid|unavailable)\.$/.test(error.message), what);
});

test("every core module belongs to its family and names the repository it is built from", () => {
  const github = (name) => ({ name: `nonary-streams/${name}`, url: `https://github.com/nonary-streams/${name}` });
  const expected = [
    ["Input", { "amaterasu.input": "amaterasu-input" }, ["amaterasu-input"]],
    ["Audio", { "amaterasu.audio": "amaterasu-audio", "amaterasu.audio-encode": "amaterasu-audio-encode" },
      ["amaterasu-audio", "amaterasu-audio-encode"]],
    ["Screen capture", {
      "amaterasu.capture-kms": "amaterasu-capture", "amaterasu.capture-pipewire": "amaterasu-capture",
      "amaterasu.capture-x11": "amaterasu-capture", "amaterasu.capture-dxgi": "amaterasu-capture",
      "amaterasu.capture-wgc": "amaterasu-capture", "amaterasu.capture-black": "amaterasu-capture",
      "amaterasu.capture-vkms": "amaterasu-capture-vkms",
    }, ["amaterasu-capture", "amaterasu-capture-vkms"]],
    ["Video encoding", {
      "amaterasu.video": "amaterasu-encode-video", "amaterasu.linux-session-video": "amaterasu-encode-video",
      "amaterasu.encode": "amaterasu-encode-video",
    }, ["amaterasu-encode-video"]],
    // Two of the display modules are built in Amaterasu's own repository.
    ["Display", {
      "amaterasu.display": "amaterasu-display", "amaterasu.display-settings": "amaterasu",
      "amaterasu.virtual-display": "amaterasu",
    }, ["amaterasu-display", "amaterasu"]],
    ["Remote session", { "amaterasu.remote-session": "amaterasu-remote-session" }, ["amaterasu-remote-session"]],
    ["RTX HDR", { "amaterasu.rtx-hdr": "amaterasu-rtx-hdr" }, ["amaterasu-rtx-hdr"]],
    ["Frame limiter", { "amaterasu.frame-limiter": "amaterasu-frame-limiter" }, ["amaterasu-frame-limiter"]],
    ["Statistics", { "amaterasu.host-metrics": "amaterasu-stats", "amaterasu.session-history": "amaterasu-stats" },
      ["amaterasu-stats"]],
    // The SDK builds no module of its own; the runtime is written against it.
    ["Runtime and SDK", { "amaterasu.runtime": "amaterasu" }, ["amaterasu", "amaterasu-sdk"]],
  ];
  assert.deepEqual(plain(plugins.MODULE_FAMILIES.map((family) => family.name)), expected.map(([name]) => name));
  for (const [index, [name, members, sources]] of expected.entries()) {
    const family = plugins.MODULE_FAMILIES[index];
    assert.deepEqual(plain(family.members), Object.keys(members), name);
    assert.deepEqual(plain(family.sources), sources.map(github), name);
    assert.ok(family.summary.endsWith("."), name);
    for (const [id, repository] of Object.entries(members)) {
      const origin = plugins.moduleOrigin(id);
      assert.equal(origin.family, family, id);
      assert.equal(origin.label, name, id);
      assert.deepEqual(plain(origin.source), github(repository), id);
    }
  }
  const members = plain(plugins.MODULE_FAMILIES.flatMap((family) => family.members));
  assert.equal(new Set(members).size, members.length, "no module is in two families");

  // A module of Amaterasu's outside every family is built into Amaterasu.
  for (const id of ["amaterasu.network", "amaterasu.management", "amaterasu.not-yet-named"])
    assert.deepEqual(plain(plugins.moduleOrigin(id)),
      { family: null, label: "Built into Amaterasu", source: github("amaterasu") }, id);
  assert.equal(plugins.AMATERASU_SOURCE.url, "https://github.com/nonary-streams/amaterasu");
  // Nothing is claimed about a module somebody else publishes.
  for (const id of ["example.overlay", "amaterasu", "notamaterasu.input"])
    assert.deepEqual(plain(plugins.moduleOrigin(id)), { family: null, label: null, source: null }, id);
});

test("an update is summed up as the modules it changes", () => {
  const { update } = plugins.parsePackageUpdateStatus(updateReply);
  assert.deepEqual(plain(plugins.moduleChanges(update)), [
    { id: "amaterasu.audio", name: "Audio capture", change: "updated", detail: "0.1.0 → 0.1.1" },
    { id: "amaterasu.input", name: "Input", change: "updated", detail: "0.1.0 → 0.2.0" },
    { id: "amaterasu.display", name: "Displays", change: "rebuilt", detail: "rebuilt" },
    { id: "amaterasu.remote-session", name: "Remote session", change: "added", detail: "added" },
    { id: "amaterasu.upnp", name: "Port mapping", change: "removed", detail: "removed" },
  ]);
  assert.equal(plugins.packageUpdateSummary(update), "5 of 6 modules change · 17.4 MB to download");
  assert.equal(plugins.packageUpdateSummary(update, true), "5 of 6 modules change · 17.4 MB downloaded");

  const one = { ...update, modules: update.modules.slice(0, 2), downloadBytes: 512 };
  assert.equal(plugins.packageUpdateSummary(one), "1 of 2 modules changes · 512 B to download");
  const alone = { ...update, modules: update.modules.slice(1, 2) };
  assert.equal(plugins.packageUpdateSummary(alone), "1 of 1 module changes · 17.4 MB to download");
  const untouched = { ...update, modules: update.modules.slice(0, 1) };
  assert.deepEqual(plain(plugins.moduleChanges(untouched)), []);
  assert.equal(plugins.packageUpdateSummary(untouched), "0 of 1 module change · 17.4 MB to download");
});

test("the Amaterasu Core row says where its update stands", () => {
  const running = { digest: digest("9"), repository: "house", version: "0.1.0" };
  const row = (change, fallback = running) => plain(plugins.coreUpdateSource(reading(change), fallback));
  const steps = (view) => view.steps.map((step) => `${step.label}:${step.state}`);

  const current = row({ phase: "current", update: null });
  assert.equal(current.id, plugins.CORE_UPDATE_SOURCE);
  assert.equal(current.name, "Amaterasu Core");
  assert.match(current.about, /^Every module Amaterasu ships\. They update together as one signed package/);
  assert.deepEqual([current.headline, current.tone, current.detail], ["Up to date", "good", null]);
  assert.deepEqual([current.version, current.automatic, current.versions, current.restoring], ["0.1.0", null, false, null]);
  // A host ahead of its repository is up to date, and says why nothing newer was found.
  const ahead = row({ phase: "current", update: null, repository_behind: true });
  assert.deepEqual([ahead.headline, ahead.tone], ["Up to date", "good"]);
  assert.match(ahead.detail, /^Nothing newer is published\. This host runs a later package than its repository lists/);
  // What the host says about keeping itself up to date is carried through every phase.
  const kept = row({ automatic: { mode: "download", next_check_unix_ms: 1_794_190_000_000 }, restoring: null });
  assert.deepEqual([kept.automatic, kept.versions], [{ mode: "download", nextCheckUnixMs: 1_794_190_000_000 }, true]);
  // The host's own answer names what it runs; the selection is only a fallback.
  assert.deepEqual([current.running, current.repository, current.checkedUnixMs],
    [digest("d"), "amaterasu", 1_794_170_000_000]);
  assert.deepEqual([current.actions, current.busy, current.steps, current.transfer, current.summary, current.changes, current.update],
    [["check"], false, [], null, null, [], null]);
  assert.equal(row({ phase: "current", update: null, source: null }).repository, "house");
  // A host that still carries the last update once nothing is newer shows none.
  assert.equal(row({ phase: "current" }).summary, null);

  const idle = row({ phase: "idle", checked_unix_ms: null, update: null });
  assert.deepEqual([idle.headline, idle.tone, idle.actions, idle.busy], ["Not checked yet", "neutral", ["check"], false]);
  const checking = row({ phase: "checking", update: null });
  assert.deepEqual([checking.headline, checking.actions, checking.busy], ["Checking for updates", [], true]);

  const available = row({});
  assert.deepEqual([available.headline, available.tone, available.detail], ["Update available", "attention", null]);
  assert.deepEqual([available.actions, available.busy, available.update], [["download", "check"], false, digest("e")]);
  assert.equal(available.summary, "5 of 6 modules change · 17.4 MB to download");
  assert.equal(available.changes.length, 5);
  assert.deepEqual(steps(available), ["Download:upcoming", "Verify:upcoming", "Wait for idle:upcoming", "Restart:upcoming"]);
  assert.equal(available.transfer, null);

  const downloading = row({ phase: "downloading", transferred_bytes: 4_550_000 });
  assert.deepEqual([downloading.headline, downloading.actions, downloading.busy], ["Downloading update", ["cancel"], true]);
  assert.deepEqual(downloading.transfer, { received: 4_550_000, total: 18_200_000 });
  assert.deepEqual(steps(downloading), ["Download:current", "Verify:upcoming", "Wait for idle:upcoming", "Restart:upcoming"]);
  // Everything has arrived but the host has not called it ready: it is verifying.
  const verifying = row({ phase: "downloading", transferred_bytes: 18_200_001 });
  assert.deepEqual(verifying.transfer, { received: 18_200_000, total: 18_200_000 });
  assert.deepEqual(steps(verifying), ["Download:done", "Verify:current", "Wait for idle:upcoming", "Restart:upcoming"]);
  // A download the host gave no size for has no end to measure against.
  const unsized = row({ phase: "downloading", transferred_bytes: 7, update: { ...updateOffer, download_bytes: 0 } });
  assert.deepEqual(unsized.transfer, { received: 7, total: null });
  assert.equal(steps(unsized)[0], "Download:current");
  assert.deepEqual(row({ phase: "downloading", update: null, transferred_bytes: 7 }).transfer, { received: 7, total: null });

  const ready = row({ phase: "ready", transferred_bytes: 18_200_000 });
  assert.deepEqual([ready.headline, ready.actions, ready.busy], ["Ready to install", ["activate", "check"], false]);
  assert.match(ready.detail, /^Downloaded and verified\./);
  assert.equal(ready.summary, "5 of 6 modules change · 17.4 MB downloaded");
  assert.deepEqual(steps(ready), ["Download:done", "Verify:done", "Wait for idle:upcoming", "Restart:upcoming"]);

  const waiting = row({ phase: "waiting_stream" });
  assert.deepEqual([waiting.headline, waiting.tone, waiting.actions, waiting.busy],
    ["Waiting for the stream to end", "attention", ["cancel"], true]);
  assert.deepEqual(steps(waiting), ["Download:done", "Verify:done", "Wait for idle:current", "Restart:upcoming"]);
  const activating = row({ phase: "activating" });
  assert.deepEqual([activating.headline, activating.actions, activating.busy], ["Installing", [], true]);
  assert.deepEqual(steps(activating), ["Download:done", "Verify:done", "Wait for idle:done", "Restart:current"]);

  // Switching to an earlier package borrows the last two phases of an install.
  const target = { digest: digest("b"), sequence: "1794169001" };
  const switchWaiting = row({ phase: "waiting_stream", update: null, restoring: target });
  assert.deepEqual([switchWaiting.headline, switchWaiting.tone, switchWaiting.actions, switchWaiting.busy, switchWaiting.restoring],
    ["Waiting for the stream to end", "attention", ["cancel"], true, digest("b")]);
  assert.match(switchWaiting.detail, /^The host switches to the earlier package/);
  assert.deepEqual(steps(switchWaiting), ["Wait for idle:current", "Restart:upcoming"]);
  const switching = row({ phase: "activating", update: null, restoring: target });
  assert.deepEqual([switching.headline, switching.actions, switching.busy, switching.summary],
    ["Switching version", [], true, null]);
  assert.deepEqual(steps(switching), ["Wait for idle:done", "Restart:current"]);
  // A switch the host has finished with is an ordinary status again.
  assert.equal(row({ phase: "idle", update: null, restoring: null }).restoring, null);

  const failed = row({ phase: "failed", error: "verification_failed" });
  assert.deepEqual([failed.headline, failed.tone, failed.busy], ["Update failed", "danger", false]);
  assert.equal(failed.detail, plugins.packageUpdateFailureMessage("verification_failed"));
  assert.deepEqual([failed.actions, failed.steps, failed.changes.length], [["download", "check"], [], 5]);
  assert.deepEqual(row({ phase: "failed", error: "no_source", update: null }).actions, ["check"]);
  const cancelled = row({ phase: "cancelled" });
  assert.deepEqual([cancelled.headline, cancelled.tone, cancelled.actions], ["Update cancelled", "neutral", ["download", "check"]]);

  // What the page shows when the host has no answer to give.
  const loading = plain(plugins.coreUpdateSource({ state: "loading" }, null));
  assert.deepEqual([loading.state, loading.running, loading.repository, loading.actions], ["loading", null, null, []]);
  const absent = plain(plugins.coreUpdateSource({ state: "absent" }, running));
  assert.deepEqual([absent.state, absent.headline, absent.detail, absent.actions, absent.busy],
    ["unavailable", "Updated outside the console", "This host does not install updates from the console yet.", [], false]);
  assert.deepEqual([absent.running, absent.repository], [digest("9"), "house"]);
  const unsupported = plain(plugins.coreUpdateSource(
    { state: "ready", status: plugins.parsePackageUpdateStatus({ ok: true, supported: false }) }, running));
  assert.deepEqual([unsupported.state, unsupported.detail, unsupported.actions],
    ["unavailable", "Updates from the console are not available on this platform yet.", []]);
  const unreadable = plain(plugins.coreUpdateSource({ state: "failed", message: "Request timed out." }, running));
  assert.deepEqual([unreadable.state, unreadable.headline, unreadable.detail, unreadable.actions],
    ["unreadable", "Status unavailable", { kind: "literal", value: "Request timed out." }, []]);
});

test("every reason an update can stop, and every refusal, has its own plain sentence", () => {
  const failures = [
    "unsupported_platform", "no_source", "repository_unavailable", "catalog_download_failed", "catalog_rejected",
    "candidate_ambiguous", "manifest_download_failed", "manifest_rejected", "download_failed", "verification_failed",
    "activation_unavailable", "activation_failed", "activation_interrupted", "shutting_down",
  ];
  const generic = plugins.packageUpdateFailureMessage(null);
  const sentences = failures.map((code) => plugins.packageUpdateFailureMessage(code));
  assert.equal(new Set(sentences).size, failures.length, "no two failures share a sentence");
  for (const sentence of [...sentences, generic]) {
    assert.match(sentence, /^[A-Z].*\.$/);
    assert.doesNotMatch(sentence, /_|manifest|catalog|sha|digest/i, "written for someone who is not a developer");
  }
  assert.ok(!sentences.includes(generic));
  assert.equal(plugins.packageUpdateFailureMessage("not_a_known_code"), generic);
  assert.equal(plugins.packageUpdateFailureMessage("constructor"), generic);

  const refusals = ["busy", "nothing_to_download", "not_ready", "activation_in_progress", "shutting_down",
    "unsupported_platform", "permission_denied"];
  const refused = refusals.map((code) => plugins.packageUpdateRefusalMessage(code));
  assert.equal(new Set(refused).size, refusals.length);
  for (const sentence of refused) assert.match(sentence, /^[A-Z].*\.$/);
  assert.equal(plugins.packageUpdateRefusalMessage("not_a_known_code"), null);
  assert.equal(plugins.packageUpdateRefusalMessage(null), null);
});

test("other publishers' offers leave out what the Updates panel stands for", () => {
  const selection = plugins.parsePackageSelectionStatus(selectionReply);
  const native = (slot, implementation, version, manifest) =>
    ({ slot, implementation, version, kind: "native_set", manifest_sha256: manifest });
  const listed = plugins.parseRepositoryList({
    ok: true,
    repositories: [
      // The repository that supplied the running package.
      { ...repository, candidates: [
        candidate,
        native("amaterasu.runtime", "amaterasu-linux-session", "0.1.0", digest("d")),
        native("amaterasu.runtime", "amaterasu-linux-session", "0.2.0", digest("e")),
        native("amaterasu.rtx-hdr", "amaterasu-rtx-hdr", "0.1.0", digest("5")),
      ] },
      { ...repository, id: "studio", candidates: [
        native("amaterasu.frame-limiter", "studio-frame-pacer", "0.3.0", digest("6")),
        native("example.overlay", "example-overlay", "1.2.0", digest("7")),
        native("studio.mirror", "studio-mirror", "1.0.0", digest("d")),
      ] },
    ],
  });
  const offers = plugins.availablePlugins(listed, selection, null);
  assert.equal(offers.length, 6);
  const kept = plain(plugins.otherPublisherOffers(offers, selection).map((offer) => `${offer.repository.id}:${offer.slot}`)).sort();
  assert.deepEqual(kept, [
    // A module the host does not run is still an offer, whoever lists it.
    "publisher:amaterasu.rtx-hdr",
    // A console interface is not part of the package the panel updates.
    "publisher:amaterasu.ui",
    // Another publisher's module is an offer even for a slot the host fills.
    "studio:amaterasu.frame-limiter",
    "studio:example.overlay",
  ]);
  // Without a recorded source, only an offer of the running package itself is left out.
  const { source: _source, ...unsourcedRunning } = selectionReply.running;
  const unsourced = plugins.parsePackageSelectionStatus({ ...selectionReply, running: unsourcedRunning });
  assert.deepEqual(plain(plugins.otherPublisherOffers(plugins.availablePlugins(listed, unsourced, null), unsourced)
    .map((offer) => offer.slot)).sort(),
    ["amaterasu.frame-limiter", "amaterasu.rtx-hdr", "amaterasu.runtime", "amaterasu.ui", "example.overlay"]);
  assert.equal(plugins.otherPublisherOffers(plugins.availablePlugins(listed, null, null), null).length, 6);
});

test("how long ago is counted in the largest whole unit", () => {
  const now = 1_800_000_000_000;
  const ago = (ms) => plain(plugins.elapsedSince(now, now - ms));
  assert.equal(ago(0), null);
  assert.equal(ago(59_999), null);
  assert.equal(ago(-5_000), null, "a host clock ahead of the browser's is not a negative age");
  assert.deepEqual(ago(60_000), { value: 1, unit: "minute" });
  assert.deepEqual(ago(59 * 60_000 + 59_000), { value: 59, unit: "minute" });
  assert.deepEqual(ago(60 * 60_000), { value: 1, unit: "hour" });
  assert.deepEqual(ago(23 * 3_600_000), { value: 23, unit: "hour" });
  assert.deepEqual(ago(49 * 3_600_000), { value: 2, unit: "day" });
});

test("the page's one line adds its sources up", () => {
  const running = { digest: digest("9"), repository: "house", version: null };
  const source = (change) => plugins.coreUpdateSource(reading({ update: null, ...change }), running);
  const answer = (...sources) => { const summary = plugins.updatesSummary(sources); return [summary.headline, summary.tone]; };

  assert.deepEqual(answer(), ["Loading…", "neutral"]);
  assert.deepEqual(answer(plugins.coreUpdateSource({ state: "loading" }, null)), ["Loading…", "neutral"]);
  assert.deepEqual(answer(source({ phase: "current" })), ["Everything is up to date", "good"]);
  assert.deepEqual(answer(source({ phase: "current" }), source({ phase: "current" })), ["Everything is up to date", "good"]);
  assert.deepEqual(answer(source({ phase: "idle" })), ["Updates have not been checked yet", "neutral"]);
  assert.deepEqual(answer(source({ phase: "checking" })), ["Checking for updates…", "neutral"]);
  for (const phase of ["available", "downloading", "ready", "waiting_stream", "activating"])
    assert.deepEqual(answer(source({ phase, update: updateOffer })), ["1 update available", "attention"], phase);
  assert.deepEqual(answer(source({ phase: "available", update: updateOffer }), source({ phase: "ready", update: updateOffer })),
    ["2 updates available", "attention"]);
  // One source up to date does not hide another's update, and trouble comes first.
  assert.deepEqual(answer(source({ phase: "current" }), source({ phase: "available", update: updateOffer })), ["1 update available", "attention"]);
  assert.deepEqual(answer(source({ phase: "available", update: updateOffer }), source({ phase: "failed", error: "download_failed" })),
    ["An update needs attention", "danger"]);
  // A cancelled update is still there to be had; a cancelled check is not one.
  assert.deepEqual(answer(source({ phase: "cancelled", update: updateOffer })), ["1 update available", "attention"]);
  assert.deepEqual(answer(source({ phase: "cancelled" })), ["Updates have not been checked yet", "neutral"]);
  assert.deepEqual(answer(source({ phase: "waiting_stream", restoring: { digest: digest("b"), sequence: "3" } })),
    ["Switching to an earlier version", "attention"]);
  assert.deepEqual(answer(plugins.coreUpdateSource({ state: "absent" }, running)), ["This host is updated outside the console", "neutral"]);
  assert.deepEqual(answer(plugins.coreUpdateSource({ state: "failed", message: "Request timed out." }, running)),
    ["Update status is unavailable", "neutral"]);

  // The newest check among the sources is the one the line reports.
  const earlier = source({ phase: "current", checked_unix_ms: 1_794_000_000_000 });
  const later = source({ phase: "current", checked_unix_ms: 1_794_170_000_000 });
  assert.equal(plugins.updatesSummary([earlier, later]).checkedUnixMs, 1_794_170_000_000);
  assert.equal(plugins.updatesSummary([source({ phase: "idle", checked_unix_ms: null })]).checkedUnixMs, null);
});

test("the packages a host has kept are read whole or not at all", () => {
  const version = {
    digest: digest("d"), sequence: "1794169006", state: "running", installed_unix_ms: 1_794_529_635_000,
    modules: 37, differs: 0, restorable: true, reason: null,
  };
  const earlier = { ...version, digest: digest("b"), sequence: "1794169005", state: "previous", differs: 1 };
  const expired = { ...version, digest: digest("a"), sequence: "18446744073709551615", state: "retained", installed_unix_ms: null, differs: 37, restorable: false, reason: "expired" };
  const reply = { ok: true, supported: true, total: 156, versions: [version, earlier, expired] };
  const listed = plain(plugins.parsePackageVersions(reply));
  assert.deepEqual([listed.supported, listed.total, listed.versions.length], [true, 156, 3]);
  assert.deepEqual(listed.versions[1], {
    digest: digest("b"), sequence: "1794169005", state: "previous", installedUnixMs: 1_794_529_635_000,
    modules: 37, differs: 1, restorable: true, reason: null, switchesBack: false,
  });
  assert.deepEqual([listed.versions[2].installedUnixMs, listed.versions[2].restorable, listed.versions[2].reason], [null, false, "expired"]);
  // A package that has run here with version switching can be left again; one that has not is one way.
  assert.equal(plugins.parsePackageVersions({ ...reply, versions: [{ ...version, switches_back: true }] }).versions[0].switchesBack, true);
  assert.equal(listed.loading, false);
  // A host still reading its packages says so with an empty list.
  assert.deepEqual(plain(plugins.parsePackageVersions({ ok: true, supported: true, loading: true, total: 0, versions: [] })),
    { supported: true, loading: true, total: 0, versions: [] });
  // A total smaller than the list is not believed.
  assert.equal(plugins.parsePackageVersions({ ...reply, total: 1 }).total, 3);
  assert.deepEqual(plain(plugins.parsePackageVersions({ ok: true, supported: false, total: 0, versions: [] })),
    { supported: false, loading: false, total: 0, versions: [] });

  for (const bad of [
    { ok: false },
    { versions: "none" },
    { total: -1 },
    { versions: [version, version] },
    { versions: [{ ...version, digest: "D".repeat(64) }] },
    { versions: [{ ...version, sequence: 7 }] },
    { versions: [{ ...version, state: "trial" }] },
    { versions: [{ ...version, installed_unix_ms: "yesterday" }] },
    { versions: [{ ...version, modules: 1.5 }] },
    { versions: [{ ...version, differs: -1 }] },
    { versions: [{ ...version, restorable: "yes" }] },
    { versions: [{ ...version, switches_back: 1 }] },
    { loading: "yes" },
    { versions: [{ ...version, reason: "Not Allowed" }] },
    { versions: Array.from({ length: 65 }, (_, index) => ({ ...version, digest: index.toString(16).padStart(64, "0") })) },
  ]) assert.throws(() => plugins.parsePackageVersions({ ...reply, ...bad }), JSON.stringify(bad).slice(0, 80));

  assert.equal(plugins.packageVersionDifference(listed.versions[0]), "37 modules");
  assert.equal(plugins.packageVersionDifference(listed.versions[1]), "1 of 37 modules differs");
  assert.equal(plugins.packageVersionDifference(listed.versions[2]), "37 of 37 modules differ");
  assert.equal(plugins.packageVersionDifference({ ...listed.versions[1], differs: 0 }), "Same modules");
  assert.equal(plugins.packageVersionReason("expired"), "Its signature has expired");
  assert.equal(plugins.packageVersionReason("publisher_untrusted"), "Its publisher is no longer trusted");
  assert.equal(plugins.packageVersionReason("unavailable"), "Its files are no longer on this host");
  for (const unknown of [null, "constructor", "something_new"]) assert.equal(plugins.packageVersionReason(unknown), "Cannot be switched to");
  assert.match(plugins.packageUpdateRefusalMessage("not_restorable"), /can no longer switch to that package/);
  assert.match(plugins.packageUpdateRefusalMessage("invalid_mode"), /did not recognise that setting/);
});

test("the modules no family accounts for are Amaterasu's own or another developer's", () => {
  const installed = plugins.installedPlugins(plugins.parsePackageSelectionStatus({
    ...selectionReply,
    running: { ...selectionReply.running, modules: [
      ...selectionReply.running.modules,
      { id: "amaterasu.network", version: "0.1.0", sha256: "4".repeat(64), runtime: false },
      { id: "amaterasu.nvhttp", version: "0.1.0", sha256: "5".repeat(64), runtime: false },
    ] },
  }));
  const loose = plugins.modulesOutsideFamilies(installed);
  assert.deepEqual(loose.builtIn.map((plugin) => plugin.id).sort(), ["amaterasu.network", "amaterasu.nvhttp"]);
  assert.deepEqual(loose.foreign.map((plugin) => plugin.id), ["example.overlay"]);
  // Every module is in a family, built in, or another developer's: none is left out or counted twice.
  const inFamilies = installed.filter((plugin) => plugins.moduleOrigin(plugin.id).family !== null);
  assert.equal(inFamilies.length + loose.builtIn.length + loose.foreign.length, installed.length);
});

test("the running package takes the version its repository gives it", () => {
  const listing = [
    { ...repository, candidates: [nativeCandidate, { ...nativeCandidate, version: "0.4.0", manifest_sha256: "d".repeat(64) }] },
    { ...repository, id: "revoked", revoked: true, candidates: [{ ...nativeCandidate, version: "9.9.9", manifest_sha256: "e".repeat(64) }] },
    { ...repository, id: "interface", candidates: [{ ...candidate, manifest_sha256: "f".repeat(64) }] },
  ];
  assert.equal(plugins.runningPackageVersion(listing, "d".repeat(64)), "0.4.0");
  assert.equal(plugins.runningPackageVersion(listing, "b".repeat(64)), "0.3.0");
  // A revoked repository, an interface bundle and an unlisted package name no version.
  assert.equal(plugins.runningPackageVersion(listing, "e".repeat(64)), null);
  assert.equal(plugins.runningPackageVersion(listing, "f".repeat(64)), null);
  assert.equal(plugins.runningPackageVersion(listing, "0".repeat(64)), null);
  assert.equal(plugins.runningPackageVersion(listing, null), null);
});

test("how long until is counted in the largest whole unit", () => {
  const now = 1_794_170_000_000;
  assert.equal(plugins.timeUntil(now, now), null);
  assert.equal(plugins.timeUntil(now, now - 60_000), null);
  assert.deepEqual(plain(plugins.timeUntil(now, now + 1)), { value: 1, unit: "minute" });
  assert.deepEqual(plain(plugins.timeUntil(now, now + 59 * 60_000)), { value: 59, unit: "minute" });
  assert.deepEqual(plain(plugins.timeUntil(now, now + 6 * 3_600_000)), { value: 6, unit: "hour" });
  assert.deepEqual(plain(plugins.timeUntil(now, now + 3 * 86_400_000)), { value: 3, unit: "day" });
});
