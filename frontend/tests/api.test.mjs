import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function apiHarness(responder) {
  const calls = [];
  const context = vm.createContext({
    Error,
    URL,
    AbortController,
    clearTimeout,
    location: { origin: "http://127.0.0.1:5000" },
    window: { setTimeout },
    fetch: async (url, options) => {
      calls.push({ url, options });
      return responder(url, options);
    },
  });
  vm.runInContext(
    ts.transpile(readFileSync("src/api.ts", "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    context,
  );
  return { api: new context.Amaterasu.Api(""), calls };
}

test("login sends no bearer and subsequent requests keep auth in the header only", async () => {
  const secret = "ama_" + "a".repeat(64);
  const { api, calls } = apiHarness((url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? {
            ok: true,
            bearer: secret,
            subject: "admin",
            scopes: ["catalog.read"],
          }
        : { ok: true, entries: [] },
    ),
  );
  await api.login("admin", "password");
  await api.post("catalog/list");
  assert.equal(calls[0].options.headers.Authorization, undefined);
  assert.equal(calls[1].options.headers.Authorization, `Bearer ${secret}`);
  assert.equal(calls[1].options.credentials, "omit");
  assert.equal(calls[1].options.cache, "no-store");
  assert.equal(calls[1].url, "http://127.0.0.1:5000/v1/catalog/list");
  assert.equal(calls[1].options.body, "{}");
  assert.equal(api.allows("catalog.write"), false);
  api.clear();
  await api.post("catalog/list");
  assert.equal(calls[2].options.headers.Authorization, "Bearer ");
});

test("domain conflicts surface despite a successful HTTP status and are never retried", async () => {
  const { api, calls } = apiHarness(() =>
    Response.json({ ok: false, error: { code: "revision_conflict" } }),
  );
  api.useToken("ama_" + "b".repeat(64));
  await assert.rejects(
    api.post("catalog/update", { expected_revision: 1 }),
    /changed elsewhere/,
  );
  assert.equal(calls.length, 1);
});
