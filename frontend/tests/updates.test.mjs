import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

class FakeElement {
  constructor(tagName) {
    this.tagName = tagName.toLowerCase();
    this.children = [];
    this.listeners = new Map();
    this.isConnected = false;
  }

  setConnected(connected) {
    this.isConnected = connected;
    for (const child of this.children) child?.setConnected?.(connected);
  }

  append(...children) {
    for (const child of children) {
      if (child == null) continue;
      this.children.push(child);
      if (child && typeof child === "object") {
        if (child.setConnected) child.setConnected(this.isConnected);
        else child.isConnected = this.isConnected;
      }
    }
  }

  replaceChildren(...children) {
    this.children = [];
    this.append(...children);
  }

  addEventListener(event, listener) {
    this.listeners.set(event, listener);
  }

  setAttribute() {}

  get textContent() {
    return this.children.map((child) => typeof child === "string" ? child : child?.textContent ?? "").join("");
  }
}

const context = vm.createContext({
  Error,
  document: { createElement: (tagName) => new FakeElement(tagName) },
});
for (const source of ["src/ui.ts", "src/updates.ts"]) {
  vm.runInContext(
    ts.transpile(readFileSync(source, "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    context,
  );
  if (source === "src/ui.ts") vm.runInContext("Object.assign(globalThis, Amaterasu)", context);
}

function waitFor(predicate, description) {
  return new Promise((resolve, reject) => {
    let attempts = 0;
    const poll = () => {
      if (predicate()) return resolve();
      if (++attempts > 50) return reject(new Error(`Timed out waiting for ${description}`));
      setTimeout(poll, 0);
    };
    poll();
  });
}

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

test("repository status distinguishes verified choices, empty catalogs and revoked publishers", () => {
  const summary = context.Amaterasu.summarizeRepositoryState([
    repository,
    { ...repository, id: "empty", candidates: [] },
    { ...repository, id: "revoked", revoked: true, candidates: [candidate] },
  ]);
  assert.equal(summary.approved, 2);
  assert.equal(summary.revoked, 1);
  assert.equal(summary.verifiedCandidates, 1);
  assert.equal(summary.uiCandidates, 1);
  assert.equal(summary.nativeSetCandidates, 0);
  assert.match(summary.repositories.find((entry) => entry.id === "publisher").status, /Verified catalog choices/);
  assert.match(summary.repositories.find((entry) => entry.id === "empty").status, /no verified choices/);
  assert.equal(summary.repositories.find((entry) => entry.id === "revoked").candidates, 0);
  assert.equal(summary.repositories.find((entry) => entry.id === "revoked").status, "Revoked");
});

test("repository inventory is configuration.read-only and discloses missing selection state", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  const calls = [];
  context.Amaterasu.renderUpdates({ content, api: {
    allows: (scope) => scope === "configuration.read",
    async post(route, payload) {
      calls.push({ route, payload });
      assert.equal(route, "repositories/list");
      return { ok: true, repositories: [repository], next_offset: null };
    },
  } });
  await waitFor(() => content.textContent.includes("Verified catalog choices available for this host"), "verified repository inventory");
  assert.match(content.textContent, /1 approved/);
  assert.match(content.textContent, /1 UI bundles · 0 native sets/);
  assert.match(content.textContent, /does not report the selected or pending UI\/native package/);
  assert.match(content.textContent, /web-recovery-status ROOT/);
  assert.match(content.textContent, /web-recover ROOT DIGEST/);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].route, "repositories/list");
  assert.equal(calls[0].payload.offset, 0);
});

test("repository inventory shows a scoped permission state without requesting data", async () => {
  const content = new FakeElement("main");
  content.setConnected(true);
  let requests = 0;
  context.Amaterasu.renderUpdates({ content, api: {
    allows: () => false,
    async post() { requests += 1; throw new Error("must not request without read permission"); },
  } });
  await waitFor(() => content.textContent.includes("needs configuration.read"), "configuration.read permission message");
  assert.equal(requests, 0);
});
