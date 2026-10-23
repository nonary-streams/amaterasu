import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ Error });
vm.runInContext(
  ts.transpile(readFileSync("src/updates.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);

const candidate = {
  slot: "amaterasu.ui",
  implementation: "publisher.console",
  version: "4",
  kind: "web_bundle",
  manifest_sha256: "a".repeat(64),
};
const repository = {
  id: "publisher",
  location: "https://updates.example.test",
  fingerprint: "b".repeat(64),
  revoked: false,
  candidates: [candidate],
};

test("repository choice pages preserve broad UI and native slot identities", () => {
  const page = context.Amaterasu.parseRepositoryPage({
    ok: true,
    repositories: [repository],
    next_offset: null,
  });
  assert.equal(page.repositories[0].candidates[0].slot, "amaterasu.ui");
  assert.equal(page.repositories[0].fingerprint, repository.fingerprint);
  assert.equal(page.nextOffset, null);
});

test("malformed repository choices and pagination fail closed", () => {
  for (const value of [
    { ok: true, repositories: [{ ...repository, candidates: [{ ...candidate, kind: "script" }] }], next_offset: null },
    { ok: true, repositories: [repository], next_offset: 4097 },
    { ok: true, repositories: [repository], next_offset: "1" },
    { ok: false, repositories: [repository], next_offset: null },
  ]) {
    assert.throws(() => context.Amaterasu.parseRepositoryPage(value));
  }
});
