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
