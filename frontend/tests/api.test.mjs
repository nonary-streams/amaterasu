import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const secret = (letter) => `ama_${letter.repeat(64)}`;
const future = (seconds) => Math.floor(Date.now() / 1000) + seconds;

function sessionReply(bearer, refreshToken, expiry, refreshExpiry, overrides = {}) {
  return {
    ok: true,
    bearer,
    refresh_token: refreshToken,
    subject: "admin",
    scopes: ["catalog.read"],
    expires_at: expiry,
    refresh_expires_at: refreshExpiry,
    remember_me: false,
    ...overrides,
  };
}

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

test("login carries remember choice and protected requests use memory-only bearer auth", async () => {
  const bearer = secret("a");
  const refreshToken = secret("b");
  const { api, calls } = apiHarness((url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: true, entries: [] },
    ),
  );
  await api.login("admin", "password", true);
  await api.post("catalog/list");
  assert.equal(calls[0].options.headers.Authorization, undefined);
  assert.deepEqual(JSON.parse(calls[0].options.body), {
    username: "admin",
    password: "password",
    remember_me: true,
  });
  assert.equal(calls[1].options.headers.Authorization, `Bearer ${bearer}`);
  assert.equal(calls[1].options.credentials, "omit");
  assert.equal(calls[1].options.cache, "no-store");
  assert.equal(calls[1].url, "http://127.0.0.1:5000/v1/catalog/list");
  assert.equal(calls[1].options.body, "{}");
  assert.equal(api.allows("catalog.write"), false);
  api.clear();
  await assert.rejects(api.post("catalog/list"), /Sign in to continue/);
  assert.equal(calls.length, 2);
});

test("simultaneous requests share one proactive refresh and use the rotated pair", async () => {
  const oldBearer = secret("c");
  const oldRefresh = secret("d");
  const newBearer = secret("e");
  const newRefresh = secret("f");
  let refreshes = 0;
  const { api, calls } = apiHarness(async (url) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(oldBearer, oldRefresh, future(10), future(600), {
          remember_me: true,
        }),
      );
    if (url.endsWith("/auth/refresh")) {
      refreshes += 1;
      await new Promise((resolve) => setTimeout(resolve, 5));
      return Response.json(
        sessionReply(newBearer, newRefresh, future(300), future(600), {
          remember_me: true,
        }),
      );
    }
    return Response.json({ ok: true, entries: [] });
  });
  await api.login("admin", "password", true);
  await Promise.all([api.post("catalog/list"), api.post("catalog/list")]);

  assert.equal(refreshes, 1);
  assert.equal(calls[1].url, "http://127.0.0.1:5000/v1/auth/refresh");
  assert.equal(calls[1].options.headers.Authorization, undefined);
  assert.deepEqual(JSON.parse(calls[1].options.body), {
    refresh_token: oldRefresh,
  });
  for (const call of calls.slice(2))
    assert.equal(call.options.headers.Authorization, `Bearer ${newBearer}`);
});

test("an unauthorized protected request refreshes once and retries with the new bearer", async () => {
  const oldBearer = secret("1");
  const oldRefresh = secret("2");
  const newBearer = secret("3");
  const newRefresh = secret("4");
  let catalogRequests = 0;
  const { api, calls } = apiHarness((url) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(oldBearer, oldRefresh, future(3600), future(7200)),
      );
    if (url.endsWith("/auth/refresh"))
      return Response.json(
        sessionReply(newBearer, newRefresh, future(3600), future(7200)),
      );
    catalogRequests += 1;
    return catalogRequests === 1
      ? Response.json({ ok: false, code: "unauthorized" }, { status: 403 })
      : Response.json({ ok: true, entries: [] });
  });
  await api.login("admin", "password");
  assert.deepEqual(await api.post("catalog/list"), {
    ok: true,
    entries: [],
  });
  assert.equal(calls.length, 4);
  assert.equal(calls[1].options.headers.Authorization, `Bearer ${oldBearer}`);
  assert.equal(calls[2].url, "http://127.0.0.1:5000/v1/auth/refresh");
  assert.equal(calls[3].options.headers.Authorization, `Bearer ${newBearer}`);
});

test("late unauthorized responses reuse a concurrent rotation instead of replaying refresh", async () => {
  const oldBearer = secret("a");
  const oldRefresh = secret("b");
  const newBearer = secret("c");
  const newRefresh = secret("d");
  let refreshes = 0;
  let catalogRequests = 0;
  const { api, calls } = apiHarness(async (url) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(oldBearer, oldRefresh, future(3600), future(7200)),
      );
    if (url.endsWith("/auth/refresh")) {
      refreshes += 1;
      return Response.json(
        sessionReply(newBearer, newRefresh, future(3600), future(7200)),
      );
    }
    catalogRequests += 1;
    const requestNumber = catalogRequests;
    if (requestNumber === 2) await new Promise((resolve) => setTimeout(resolve, 20));
    return requestNumber <= 2
      ? Response.json({ ok: false, code: "unauthorized" }, { status: 403 })
      : Response.json({ ok: true, entries: [] });
  });
  await api.login("admin", "password");
  await Promise.all([api.post("catalog/list"), api.post("catalog/list")]);

  assert.equal(refreshes, 1);
  assert.equal(catalogRequests, 4);
  assert.equal(calls.filter((call) => call.url.endsWith("/auth/refresh")).length, 1);
  const catalogCalls = calls.filter((call) => call.url.endsWith("/catalog/list"));
  assert.equal(catalogCalls[0].options.headers.Authorization, `Bearer ${oldBearer}`);
  assert.equal(catalogCalls[1].options.headers.Authorization, `Bearer ${oldBearer}`);
  assert.equal(catalogCalls[2].options.headers.Authorization, `Bearer ${newBearer}`);
  assert.equal(catalogCalls[3].options.headers.Authorization, `Bearer ${newBearer}`);
});

test("refresh failure clears the session and returns the UI to authentication", async () => {
  const bearer = secret("5");
  const refreshToken = secret("6");
  const { api, calls } = apiHarness((url) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(bearer, refreshToken, future(3600), future(7200)),
      );
    return Response.json({ ok: false, code: "unauthorized" }, { status: 403 });
  });
  let lost = 0;
  api.onAuthenticationLost = () => lost += 1;
  await api.login("admin", "password");
  await assert.rejects(api.post("catalog/list"), /Access denied/);
  assert.equal(calls.length, 3);
  assert.equal(api.subject, "");
  assert.equal(lost, 1);
});

test("logout revokes by refresh token after clearing local credentials immediately", async () => {
  const bearer = secret("7");
  const refreshToken = secret("8");
  const { api, calls } = apiHarness((url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200))
        : { ok: true },
    ),
  );
  await api.login("admin", "password");
  await api.logout();

  assert.equal(api.subject, "");
  assert.equal(calls[1].url, "http://127.0.0.1:5000/v1/auth/logout");
  assert.deepEqual(JSON.parse(calls[1].options.body), {
    refresh_token: refreshToken,
  });
  assert.equal(calls[1].options.headers.Authorization, undefined);
  assert.equal(calls[1].options.credentials, "omit");
});

test("API token scope failures are surfaced without refresh or retry", async () => {
  const { api, calls } = apiHarness(() =>
    Response.json({ ok: false, error: { code: "revision_conflict" } }),
  );
  api.useToken(secret("9"));
  await assert.rejects(
    api.post("catalog/update", { expected_revision: 1 }),
    /changed elsewhere/,
  );
  assert.equal(calls.length, 1);
});
