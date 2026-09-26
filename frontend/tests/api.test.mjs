import assert from "node:assert/strict";
import { webcrypto } from "node:crypto";
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

const browserReply = (expiry, overrides = {}) => ({
  ok: true,
  subject: "admin",
  scopes: ["catalog.read"],
  expires_at: expiry,
  ...overrides,
});

function memoryStorage(failures = {}) {
  const values = new Map();
  return {
    getItem(key) {
      if (failures.get) throw new Error("storage get blocked");
      return values.has(key) ? values.get(key) : null;
    },
    setItem(key, value) {
      if (failures.set) throw new Error("storage set blocked");
      values.set(key, String(value));
    },
    removeItem(key) {
      if (failures.remove) throw new Error("storage remove blocked");
      values.delete(key);
    },
    values,
  };
}

function lockManager() {
  const queues = new Map();
  return {
    request(name, _options, callback) {
      const previous = queues.get(name) ?? Promise.resolve();
      let release;
      const held = new Promise((resolve) => release = resolve);
      const tail = previous.then(() => held);
      queues.set(name, tail);
      return previous.then(async () => {
        try {
          return await callback({ name });
        } finally {
          release();
          if (queues.get(name) === tail) queues.delete(name);
        }
      });
    },
  };
}

function apiHarness(responder, options = {}) {
  const calls = [];
  const storage = options.storage ?? memoryStorage();
  const storageListeners = [];
  const context = vm.createContext({
    Error,
    URL,
    AbortController,
    crypto: webcrypto,
    clearTimeout,
    localStorage: storage,
    navigator: options.locks === null ? undefined : { locks: options.locks ?? lockManager() },
    location: { origin: options.origin ?? "http://127.0.0.1:5000" },
    window: {
      setTimeout,
      addEventListener: (name, listener) => {
        if (name === "storage") storageListeners.push(listener);
      },
    },
    fetch: async (url, options) => {
      calls.push({ url, options });
      return responder(url, options);
    },
  });
  if (options.storageGetterThrows)
    Object.defineProperty(context, "localStorage", {
      configurable: true,
      get() {
        throw new Error("storage getter blocked");
      },
    });
  vm.runInContext(
    ts.transpile(readFileSync("src/api.ts", "utf8"), {
      target: ts.ScriptTarget.ES2022,
    }),
    context,
  );
  return {
    api: new context.Amaterasu.Api(""),
    calls,
    storage,
    dispatchStorage: (key, newValue) => {
      for (const listener of storageListeners)
        listener({ key, newValue, storageArea: storage });
    },
  };
}

test("remembered login carries the choice and protected requests use bearer auth", async () => {
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
  await api.clear();
  await assert.rejects(api.post("catalog/list"), /Sign in to continue/);
  assert.equal(calls.length, 2);
});

test("a stale queued remembered login only revokes its own refresh token", async () => {
  const winningBearer = secret("c");
  const winningRefresh = secret("d");
  const losingBearer = secret("e");
  const losingRefresh = secret("f");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const refreshLockName = "amaterasu.session.refresh:http://127.0.0.1:5000";
  const baseLocks = lockManager();

  let releaseBlocker;
  let signalBlockerAcquired;
  const blockerAcquired = new Promise((resolve) => {
    signalBlockerAcquired = resolve;
  });
  const blocker = baseLocks.request(
    refreshLockName,
    { mode: "exclusive" },
    () => new Promise((resolve) => {
      releaseBlocker = resolve;
      signalBlockerAcquired();
    }),
  );
  await blockerAcquired;

  let queuedInstallCount = 0;
  let signalFirstInstallQueued;
  let signalBothInstallsQueued;
  const firstInstallQueued = new Promise((resolve) => {
    signalFirstInstallQueued = resolve;
  });
  const bothInstallsQueued = new Promise((resolve) => {
    signalBothInstallsQueued = resolve;
  });
  let releaseSecondInstall;
  const secondInstallGate = new Promise((resolve) => {
    releaseSecondInstall = resolve;
  });
  let signalSecondInstallWaiting;
  const secondInstallWaiting = new Promise((resolve) => {
    signalSecondInstallWaiting = resolve;
  });
  const locks = {
    request(name, options, callback) {
      queuedInstallCount += 1;
      const guardedCallback = queuedInstallCount === 2
        ? async (lock) => {
            signalSecondInstallWaiting();
            await secondInstallGate;
            return callback(lock);
          }
        : callback;
      const pending = baseLocks.request(name, options, guardedCallback);
      if (queuedInstallCount === 1) signalFirstInstallQueued();
      if (queuedInstallCount === 2) signalBothInstallsQueued();
      return pending;
    },
  };

  const loginResponses = new Map();
  let loginRequestCount = 0;
  let signalBothLoginRequests;
  const bothLoginRequests = new Promise((resolve) => {
    signalBothLoginRequests = resolve;
  });
  let signalRevocationStarted;
  const revocationStarted = new Promise((resolve) => {
    signalRevocationStarted = resolve;
  });
  const responder = (url, options) => {
    if (url.endsWith("/auth/login")) {
      const username = JSON.parse(options.body).username;
      loginRequestCount += 1;
      if (loginRequestCount === 2) signalBothLoginRequests();
      return new Promise((resolve) => loginResponses.set(username, resolve));
    }
    if (url.endsWith("/auth/logout")) {
      signalRevocationStarted();
      return Response.json({ ok: true });
    }
    return Response.json({ ok: true });
  };
  const { api, calls } = apiHarness(responder, { storage, locks });

  const winningLogin = api.login("winner", "password", true);
  const staleLogin = api.login("stale", "password", true);
  await bothLoginRequests;

  const resolveWinningResponse = loginResponses.get("winner");
  assert.ok(resolveWinningResponse);
  resolveWinningResponse(Response.json(
    sessionReply(winningBearer, winningRefresh, future(3600), future(7200), {
      remember_me: true,
    }),
  ));
  await firstInstallQueued;

  const resolveStaleResponse = loginResponses.get("stale");
  assert.ok(resolveStaleResponse);
  resolveStaleResponse(Response.json(
    sessionReply(losingBearer, losingRefresh, future(3600), future(7200), {
      remember_me: true,
    }),
  ));
  await bothInstallsQueued;

  releaseBlocker();
  await blocker;
  await secondInstallWaiting;
  await winningLogin;
  releaseSecondInstall();
  const results = await Promise.allSettled([winningLogin, staleLogin]);
  await revocationStarted;

  assert.equal(results[0].status, "fulfilled");
  assert.equal(results[1].status, "rejected");
  assert.match(results[1].reason.message, /sign-in attempt was cancelled/i);
  assert.equal(api.subject, "admin");
  assert.equal(api.bearer, winningBearer);
  assert.equal(api.refreshToken, winningRefresh);

  const stored = JSON.parse(storage.getItem(storageKey));
  assert.equal(stored.bearer, winningBearer);
  assert.equal(stored.refresh_token, winningRefresh);

  const revocations = calls.filter((call) => call.url.endsWith("/auth/logout"));
  assert.equal(revocations.length, 1);
  assert.deepEqual(JSON.parse(revocations[0].options.body), {
    refresh_token: losingRefresh,
  });
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
  await api.useToken(secret("9"));
  await assert.rejects(
    api.post("catalog/update", { expected_revision: 1 }),
    /changed elsewhere/,
  );
  assert.equal(calls.length, 1);
});

test("remembered sessions restore after reload without a network request", async () => {
  const bearer = secret("a");
  const refreshToken = secret("b");
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: true, entries: [] },
    );
  const first = apiHarness(responder, { storage, locks });
  await first.api.login("admin", "password", true);

  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const saved = JSON.parse(storage.getItem(storageKey));
  assert.equal(saved.version, 1);
  assert.equal(saved.remember_me, true);
  assert.equal(saved.bearer, bearer);
  assert.equal(saved.refresh_token, refreshToken);

  const reloaded = apiHarness(responder, { storage, locks });
  assert.equal(await reloaded.api.restoreSession(), true);
  assert.equal(reloaded.calls.length, 0);
  await reloaded.api.post("catalog/list");
  assert.equal(
    reloaded.calls[0].options.headers.Authorization,
    `Bearer ${bearer}`,
  );
});

test("expired remembered storage clears without a network request", async () => {
  const bearer = secret("c");
  const refreshToken = secret("d");
  const storage = memoryStorage();
  const { api, calls } = apiHarness(
    (url) =>
      Response.json(
        url.endsWith("/auth/login")
          ? sessionReply(bearer, refreshToken, future(-10), future(-5), {
              remember_me: true,
            })
          : { ok: true },
      ),
    { storage },
  );
  await api.login("admin", "password", true);
  assert.equal(await api.restoreSession(), false);
  assert.equal(calls.length, 1);
  assert.equal(storage.getItem("amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000"), null);
});

test("restore proactively rotates a remembered session near access expiry", async () => {
  const oldBearer = secret("e");
  const oldRefresh = secret("f");
  const newBearer = secret("1");
  const newRefresh = secret("2");
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(oldBearer, oldRefresh, future(5), future(600), {
            remember_me: true,
          })
        : url.endsWith("/auth/refresh")
          ? sessionReply(newBearer, newRefresh, future(3600), future(7200), {
              remember_me: true,
            })
          : { ok: true },
    );
  const first = apiHarness(responder, { storage, locks });
  await first.api.login("admin", "password", true);
  const reloaded = apiHarness(responder, { storage, locks });

  assert.equal(await reloaded.api.restoreSession(), true);
  assert.equal(reloaded.calls.length, 1);
  assert.equal(reloaded.calls[0].url, "http://127.0.0.1:5000/v1/auth/refresh");
  assert.deepEqual(JSON.parse(reloaded.calls[0].options.body), {
    refresh_token: oldRefresh,
  });
  const saved = JSON.parse(
    storage.getItem("amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000"),
  );
  assert.equal(saved.bearer, newBearer);
  assert.equal(saved.refresh_token, newRefresh);
  assert.equal(saved.refreshing, false);
});

test("restore rotates an expired bearer while its refresh token is still valid", async () => {
  const oldBearer = secret("4");
  const oldRefresh = secret("5");
  const newBearer = secret("6");
  const newRefresh = secret("7");
  const refreshExpiresAt = future(600);
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(oldBearer, oldRefresh, future(-10), refreshExpiresAt, {
            remember_me: true,
          })
        : url.endsWith("/auth/refresh")
          ? sessionReply(newBearer, newRefresh, future(300), refreshExpiresAt, {
              remember_me: true,
            })
          : { ok: true },
    );
  const first = apiHarness(responder, { storage, locks });
  await first.api.login("admin", "password", true);
  const reloaded = apiHarness(responder, { storage, locks });

  assert.equal(await reloaded.api.restoreSession(), true);
  assert.equal(reloaded.calls.length, 1);
  assert.equal(reloaded.calls[0].url, "http://127.0.0.1:5000/v1/auth/refresh");
  assert.deepEqual(JSON.parse(reloaded.calls[0].options.body), {
    refresh_token: oldRefresh,
  });
  const saved = JSON.parse(
    storage.getItem("amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000"),
  );
  assert.equal(saved.bearer, newBearer);
  assert.equal(saved.refresh_token, newRefresh);
  assert.equal(saved.expires_at > Math.floor(Date.now() / 1000), true);
  assert.equal(saved.refresh_expires_at, refreshExpiresAt);
});

test("two API instances serialize refresh and adopt the rotated pair", async () => {
  const oldBearer = secret("3");
  const oldRefresh = secret("4");
  const newBearer = secret("5");
  const newRefresh = secret("6");
  const storage = memoryStorage();
  const locks = lockManager();
  let refreshes = 0;
  const responder = async (url, options) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(oldBearer, oldRefresh, future(3600), future(7200), {
          remember_me: true,
        }),
      );
    if (url.endsWith("/auth/refresh")) {
      refreshes += 1;
      await new Promise((resolve) => setTimeout(resolve, 5));
      return Response.json(
        sessionReply(newBearer, newRefresh, future(3600), future(7200), {
          remember_me: true,
        }),
      );
    }
    const authorization = options.headers.Authorization;
    return authorization === `Bearer ${oldBearer}`
      ? Response.json({ ok: false, code: "unauthorized" }, { status: 401 })
      : Response.json({ ok: true, entries: [] });
  };
  const first = apiHarness(responder, { storage, locks });
  await first.api.login("admin", "password", true);
  const second = apiHarness(responder, { storage, locks });
  assert.equal(await second.api.restoreSession(), true);

  await Promise.all([
    first.api.post("catalog/list"),
    second.api.post("catalog/list"),
  ]);
  assert.equal(refreshes, 1);
  const refreshCalls = [...first.calls, ...second.calls].filter((call) =>
    call.url.endsWith("/auth/refresh"),
  );
  assert.equal(refreshCalls.length, 1);
  assert.deepEqual(JSON.parse(refreshCalls[0].options.body), {
    refresh_token: oldRefresh,
  });
  for (const harness of [first, second]) {
    const catalogCalls = harness.calls.filter((item) =>
      item.url.endsWith("/catalog/list"),
    );
    assert.equal(catalogCalls.length, 2);
    assert.equal(catalogCalls[0].options.headers.Authorization, `Bearer ${oldBearer}`);
    assert.equal(catalogCalls[1].options.headers.Authorization, `Bearer ${newBearer}`);
  }
});

test("logout waits for rotation and revokes the committed replacement pair", async () => {
  const bearer = secret("7");
  const refreshToken = secret("8");
  const rotatedBearer = secret("9");
  const rotatedRefresh = secret("a");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  let refreshStarted;
  const started = new Promise((resolve) => refreshStarted = resolve);
  let releaseRefresh;
  let refreshSignal;
  let rotationCommitted = false;
  const responder = (url, options) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(bearer, refreshToken, future(5), future(7200), {
          remember_me: true,
        }),
      );
    if (url.endsWith("/auth/refresh")) {
      refreshStarted();
      refreshSignal = options.signal;
      return new Promise((resolve, reject) => {
        options.signal.addEventListener("abort", () => {
          reject(new Error("refresh response was aborted after rotation"));
        });
        releaseRefresh = () => {
          rotationCommitted = true;
          resolve(Response.json(
            sessionReply(rotatedBearer, rotatedRefresh, future(3600), future(7200), {
              remember_me: true,
            }),
          ));
        };
      });
    }
    return Response.json({ ok: true });
  };
  const { api, calls } = apiHarness(responder, { storage });
  await api.login("admin", "password", true);
  const pendingRequest = api.post("catalog/list").catch(() => undefined);
  await started;
  const pendingLogout = api.logout();
  assert.equal(api.subject, "");
  assert.equal(refreshSignal.aborted, false);
  assert.notEqual(storage.getItem(storageKey), null);
  await assert.rejects(api.post("catalog/list"), /Sign out is already in progress/);
  releaseRefresh();
  await Promise.all([pendingRequest, pendingLogout]);

  assert.equal(rotationCommitted, true);
  assert.equal(refreshSignal.aborted, false);
  assert.equal(api.subject, "");
  assert.equal(storage.getItem(storageKey), null);
  const lateRevocations = calls.filter(
    (call) => call.url.endsWith("/auth/logout") &&
      JSON.parse(call.options.body).refresh_token === rotatedRefresh,
  );
  assert.equal(lateRevocations.length, 1);
});

test("suspend during refresh leaves a marker that reload clears without replay", async () => {
  const bearer = secret("b");
  const refreshToken = secret("c");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  let refreshStarted;
  const started = new Promise((resolve) => refreshStarted = resolve);
  const responder = (url, options) => {
    if (url.endsWith("/auth/login"))
      return Response.json(
        sessionReply(bearer, refreshToken, future(5), future(7200), {
          remember_me: true,
        }),
      );
    if (url.endsWith("/auth/refresh")) {
      refreshStarted();
      return new Promise((_resolve, reject) => {
        options.signal.addEventListener("abort", () => {
          const error = new Error("refresh interrupted by page lifecycle");
          error.name = "AbortError";
          reject(error);
        });
      });
    }
    return Response.json({ ok: true });
  };
  const { api, calls } = apiHarness(responder, { storage });
  await api.login("admin", "password", true);
  const pendingRequest = api.post("catalog/list").catch(() => undefined);
  await started;

  api.suspend();
  await pendingRequest;
  const interruptedRecord = JSON.parse(storage.getItem(storageKey));
  assert.equal(interruptedRecord.refreshing, true);
  assert.equal(api.subject, "");

  const reloaded = apiHarness(responder, { storage });
  assert.equal(await reloaded.api.restoreSession(), false);
  assert.equal(reloaded.calls.length, 0);
  assert.equal(storage.getItem(storageKey), null);
  assert.equal(
    calls.filter((call) => call.url.endsWith("/auth/refresh")).length,
    1,
  );
});

test("corrupted remembered storage is cleared without a network request", async () => {
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  storage.setItem(storageKey, "not-json");
  const { api, calls } = apiHarness(() => Response.json({ ok: true }), {
    storage,
  });

  assert.equal(await api.restoreSession(), false);
  assert.equal(calls.length, 0);
  assert.equal(storage.getItem(storageKey), null);
});

test("rejected remembered refresh clears storage and reports lost authentication", async () => {
  const bearer = secret("b");
  const refreshToken = secret("c");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const { api, calls } = apiHarness((url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(5), future(600), {
            remember_me: true,
          })
        : { ok: false, code: "forbidden" },
      { status: url.endsWith("/auth/login") ? 200 : 403 },
    ),
  { storage });
  let lost = 0;
  api.onAuthenticationLost = () => lost += 1;
  await api.login("admin", "password", true);
  await assert.rejects(api.restoreSession(), /account cannot perform that action/);

  assert.equal(storage.getItem(storageKey), null);
  assert.equal(api.subject, "");
  assert.equal(lost, 1);
  assert.equal(calls.filter((call) => call.url.endsWith("/auth/refresh")).length, 1);
});

test("peer logout clears a remembered session after its storage event", async () => {
  const bearer = secret("d");
  const refreshToken = secret("e");
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: true },
    );
  const first = apiHarness(responder, { storage, locks });
  await first.api.login("admin", "password", true);
  const second = apiHarness(responder, { storage, locks });
  await second.api.restoreSession();
  let lost = 0;
  second.api.onAuthenticationLost = () => lost += 1;

  await first.api.logout();
  second.dispatchStorage(
    "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000",
    null,
  );
  assert.equal(second.api.subject, "");
  assert.equal(lost, 1);
  await assert.rejects(second.api.post("catalog/list"), /Sign in to continue/);
});

test("peer storage clear signs out a remembered tab", async () => {
  const storage = memoryStorage();
  const bearer = secret("a");
  const refreshToken = secret("b");
  const responder = (url) => Response.json(
    url.endsWith("/auth/login")
      ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
          remember_me: true,
        })
      : { ok: true },
  );
  const first = apiHarness(responder, { storage });
  await first.api.login("admin", "password", true);
  const second = apiHarness(responder, { storage });
  assert.equal(await second.api.restoreSession(), true);
  let lost = 0;
  second.api.onAuthenticationLost = () => lost += 1;

  storage.values.clear();
  second.dispatchStorage(null, null);

  assert.equal(second.api.subject, "");
  assert.equal(lost, 1);
  await assert.rejects(second.api.post("catalog/list"), /Sign in to continue/);
});

test("storage clear notification preserves a still-present remembered record", async () => {
  const storage = memoryStorage();
  const bearer = secret("c");
  const refreshToken = secret("d");
  const { api, dispatchStorage } = apiHarness(
    (url) => Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: true },
    ),
    { storage },
  );
  await api.login("admin", "password", true);
  let lost = 0;
  api.onAuthenticationLost = () => lost += 1;

  dispatchStorage(null, null);

  assert.equal(api.subject, "admin");
  assert.equal(lost, 0);
  await api.post("catalog/list");
});

test("delayed cleanup and stale storage events preserve a newer remembered login", async () => {
  const firstBearer = secret("a");
  const firstRefresh = secret("b");
  const secondBearer = secret("c");
  const secondRefresh = secret("d");
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url, options) => {
    if (!url.endsWith("/auth/login")) return Response.json({ ok: true });
    const username = JSON.parse(options.body).username;
    return Response.json(
      username === "first"
        ? sessionReply(firstBearer, firstRefresh, future(3600), future(7200), {
            remember_me: true,
          })
        : sessionReply(secondBearer, secondRefresh, future(3600), future(7200), {
            remember_me: true,
          }),
    );
  };
  const first = apiHarness(responder, { storage, locks });
  await first.api.login("first", "password", true);
  const second = apiHarness(responder, { storage, locks });
  await second.api.restoreSession();

  let acquiredLock;
  const lockAcquired = new Promise((resolve) => acquiredLock = resolve);
  let releaseLock;
  const blocker = locks.request(
    "amaterasu.session.refresh:http://127.0.0.1:5000",
    { mode: "exclusive" },
    () => new Promise((resolve) => {
      releaseLock = resolve;
      acquiredLock();
    }),
  );
  await lockAcquired;
  const clearing = first.api.clear();
  const loggingIn = second.api.login("second", "password", true);
  releaseLock();
  await blocker;
  await Promise.all([clearing, loggingIn]);
  await new Promise((resolve) => setTimeout(resolve, 0));

  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  assert.equal(JSON.parse(storage.getItem(storageKey)).bearer, secondBearer);
  let lost = 0;
  second.api.onAuthenticationLost = () => lost += 1;
  second.dispatchStorage(storageKey, null);
  assert.equal(second.api.subject, "admin");
  assert.equal(second.api.scopes.includes("catalog.read"), true);
  assert.equal(lost, 0);
});

test("a failed refresh in a stale tab preserves a newer remembered login", async () => {
  const oldBearer = secret("1");
  const oldRefresh = secret("2");
  const newBearer = secret("3");
  const newRefresh = secret("4");
  const storage = memoryStorage();
  const baseLocks = lockManager();
  let observeNextLockRequest = false;
  let resolveLoginQueued;
  const loginQueued = new Promise((resolve) => resolveLoginQueued = resolve);
  const locks = {
    request(name, options, callback) {
      const pending = baseLocks.request(name, options, callback);
      if (observeNextLockRequest) {
        observeNextLockRequest = false;
        resolveLoginQueued();
      }
      return pending;
    },
  };
  let refreshStarted;
  const started = new Promise((resolve) => refreshStarted = resolve);
  let releaseRefresh;
  const responder = (url, options) => {
    if (url.endsWith("/auth/login")) {
      const username = JSON.parse(options.body).username;
      return Response.json(
        username === "old"
          ? sessionReply(oldBearer, oldRefresh, future(5), future(7200), {
              remember_me: true,
            })
          : sessionReply(newBearer, newRefresh, future(3600), future(7200), {
              remember_me: true,
            }),
      );
    }
    if (url.endsWith("/auth/refresh")) {
      return new Promise((resolve) => {
        releaseRefresh = () => resolve(
          Response.json({ ok: false, code: "unauthorized" }, { status: 401 }),
        );
        refreshStarted();
      });
    }
    return Response.json({ ok: true });
  };
  const oldTab = apiHarness(responder, { storage, locks });
  await oldTab.api.login("old", "password", true);
  const failedRequest = oldTab.api.post("catalog/list").catch(() => undefined);
  await started;

  const newTab = apiHarness(responder, { storage, locks });
  observeNextLockRequest = true;
  const newLogin = newTab.api.login("new", "password", true);
  await loginQueued;
  releaseRefresh();
  await Promise.all([failedRequest, newLogin]);
  await new Promise((resolve) => setTimeout(resolve, 0));

  const saved = JSON.parse(
    storage.getItem("amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000"),
  );
  assert.equal(oldTab.api.subject, "");
  assert.equal(newTab.api.subject, "admin");
  assert.equal(saved.bearer, newBearer);
  assert.equal(saved.refresh_token, newRefresh);
});

test("logout from a stale tab preserves a newer remembered login", async () => {
  const oldBearer = secret("5");
  const oldRefresh = secret("6");
  const newBearer = secret("7");
  const newRefresh = secret("8");
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url, options) => {
    if (!url.endsWith("/auth/login")) return Response.json({ ok: true });
    const username = JSON.parse(options.body).username;
    return Response.json(
      username === "old"
        ? sessionReply(oldBearer, oldRefresh, future(3600), future(7200), {
            remember_me: true,
          })
        : sessionReply(newBearer, newRefresh, future(3600), future(7200), {
            remember_me: true,
          }),
    );
  };
  const oldTab = apiHarness(responder, { storage, locks });
  await oldTab.api.login("old", "password", true);
  const newTab = apiHarness(responder, { storage, locks });
  await newTab.api.login("new", "password", true);

  await oldTab.api.logout();

  const saved = JSON.parse(
    storage.getItem("amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000"),
  );
  assert.equal(oldTab.api.subject, "");
  assert.equal(newTab.api.subject, "admin");
  assert.equal(saved.bearer, newBearer);
  assert.equal(saved.refresh_token, newRefresh);
  const revocations = oldTab.calls.filter((call) =>
    call.url.endsWith("/auth/logout"),
  );
  assert.equal(revocations.length, 1);
  assert.deepEqual(JSON.parse(revocations[0].options.body), {
    refresh_token: oldRefresh,
  });
});

test("remembered login requires Web Locks", async () => {
  const bearer = secret("f");
  const refreshToken = secret("1");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const { api, calls } = apiHarness((url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: false, code: "unauthorized" },
      { status: url.endsWith("/auth/login") ? 200 : 401 },
    ),
  { storage, locks: null });
  await assert.rejects(api.login("admin", "password", true), /Web Locks/);
  assert.equal(calls.length, 0);
  assert.equal(storage.getItem(storageKey), null);
  assert.equal(api.subject, "");
});

test("restore without Web Locks leaves another tab's remembered session intact", async () => {
  const bearer = secret("f");
  const refreshToken = secret("1");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const responder = (url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: true },
    );
  const setup = apiHarness(responder, { storage, locks: lockManager() });
  await setup.api.login("admin", "password", true);
  const { api, calls } = apiHarness(responder, { storage, locks: null });
  assert.equal(await api.restoreSession(), false);
  assert.equal(api.subject, "");
  await assert.rejects(api.post("catalog/list"), /Sign in to continue/);
  assert.equal(calls.filter((call) => call.url.endsWith("/auth/refresh")).length, 0);
  assert.equal(JSON.parse(storage.getItem(storageKey)).bearer, bearer);
  await setup.api.post("catalog/list");
  assert.equal(setup.api.subject, "admin");
});

test("remember_me=false leaves no persistent session record", async () => {
  const bearer = secret("2");
  const refreshToken = secret("3");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const { api } = apiHarness(
    (url) =>
      Response.json(
        url.endsWith("/auth/login")
          ? sessionReply(bearer, refreshToken, future(3600), future(7200))
          : { ok: true },
      ),
    { storage },
  );
  await api.login("admin", "password", false);

  assert.equal(storage.getItem(storageKey), null);
  assert.equal(await api.restoreSession(), false);
  assert.equal(api.subject, "");
});

test("API-token login clears remembered credentials without persisting the token", async () => {
  const bearer = secret("4");
  const refreshToken = secret("5");
  const apiToken = secret("6");
  const storage = memoryStorage();
  const storageKey = "amaterasu.session.http%3A%2F%2F127.0.0.1%3A5000";
  const { api } = apiHarness(
    (url) =>
      Response.json(
        sessionReply(bearer, refreshToken, future(3600), future(7200), {
          remember_me: true,
        }),
      ),
    { storage },
  );
  await api.login("admin", "password", true);
  await api.useToken(apiToken);

  assert.equal(storage.getItem(storageKey), null);
  assert.equal(api.subject, "API token");
});

test("unavailable storage rejects remembered login and revokes its issued session", async () => {
  const bearer = secret("7");
  const refreshToken = secret("8");
  const storage = memoryStorage({ set: true });
  const { api, calls } = apiHarness((url) =>
    Response.json(
      url.endsWith("/auth/login")
        ? sessionReply(bearer, refreshToken, future(3600), future(7200), {
            remember_me: true,
          })
        : { ok: true },
    ),
  { storage });

  await assert.rejects(api.login("admin", "password", true), {
    code: "storage_unavailable",
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(api.subject, "");
  assert.ok(calls.some((call) =>
    call.url.endsWith("/auth/logout") &&
    JSON.parse(call.options.body).refresh_token === refreshToken,
  ));
});

test("session-only login remains in memory when the localStorage getter is denied", async () => {
  const bearer = secret("9");
  const refreshToken = secret("a");
  const { api, calls } = apiHarness(
    (url) =>
      Response.json(
        sessionReply(bearer, refreshToken, future(3600), future(7200)),
      ),
    { storageGetterThrows: true },
  );
  await api.login("admin", "password", false);

  assert.equal(api.subject, "admin");
  assert.equal(calls.length, 1);
});

test("HTTPS same-origin login and reload use only HttpOnly cookie routes", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  const locks = lockManager();
  const responder = (url) => Response.json(
    url.endsWith("/catalog/list") ? { ok: true, entries: [] } : browserReply(future(3600)),
  );
  const first = apiHarness(responder, { origin, storage, locks });
  assert.equal(first.api.usesSecureCookies, true);
  await first.api.login("admin", "password", true);
  const second = apiHarness(responder, { origin, storage, locks });
  assert.equal(await second.api.restoreSession(), true);
  await second.api.post("catalog/list");
  assert.equal(first.calls[0].url, `${origin}/v1/auth/browser/logout`);
  assert.equal(first.calls[1].url, `${origin}/v1/auth/browser/login`);
  assert.equal(second.calls[0].url, `${origin}/v1/auth/browser/session`);
  assert.equal(second.calls[1].url, `${origin}/v1/catalog/list`);
  for (const { options } of [...first.calls, ...second.calls]) {
    assert.equal(options.credentials, "same-origin");
    assert.equal(options.headers["X-Amaterasu-CSRF"], "1");
    assert.equal(options.headers.Authorization, undefined);
    assert.equal(options.referrerPolicy, "no-referrer");
  }
  assert.ok(!JSON.stringify([...storage.values]).includes("ama_"));
  assert.equal(
    storage.getItem(`amaterasu.session.cookie-pending-logout.${encodeURIComponent(origin)}`),
    null,
  );
});

test("expired HTTPS cookie reload refreshes once and rejects token-bearing metadata", async () => {
  const origin = "https://host.example";
  let refreshes = 0;
  const responder = (url) => {
    if (url.endsWith("/browser/session"))
      return Response.json({ ok: false, code: "unauthorized" }, { status: 401 });
    if (url.endsWith("/browser/refresh")) {
      refreshes += 1;
      return Response.json(browserReply(future(3600)));
    }
    return Response.json({ ok: true });
  };
  const { api, calls } = apiHarness(responder, { origin });
  assert.equal(await api.restoreSession(), true);
  assert.equal(refreshes, 1);
  assert.equal(calls.length, 2);
  const malformed = apiHarness(
    () => Response.json(browserReply(future(3600), { bearer: secret("a") })),
    { origin },
  );
  await assert.rejects(malformed.api.login("admin", "password", true), {
    code: "invalid_response",
  });
  assert.equal(malformed.api.subject, "");
});

test("concurrent HTTPS requests adopt peer rotation under one Web Lock", async () => {
  const origin = "https://host.example";
  const locks = lockManager();
  let expiry = future(5);
  let refreshes = 0;
  const responder = (url) => {
    if (url.endsWith("/browser/refresh")) {
      refreshes += 1;
      expiry = future(3600);
    }
    return Response.json(
      url.endsWith("/catalog/list") ? { ok: true, entries: [] } : browserReply(expiry),
    );
  };
  const first = apiHarness(responder, { origin, locks });
  const second = apiHarness(responder, { origin, locks });
  await first.api.login("admin", "password", true);
  await second.api.login("admin", "password", true);
  await Promise.all([
    first.api.post("catalog/list"),
    second.api.post("catalog/list"),
  ]);
  assert.equal(refreshes, 1);
  assert.equal(
    [...first.calls, ...second.calls].filter((call) =>
      call.url.endsWith("/browser/refresh")
    ).length,
    1,
  );
});

test("HTTPS logout revokes cookies and peers confirm lost authentication", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  const locks = lockManager();
  let active = false;
  const responder = (url) => {
    if (url.endsWith("/browser/logout")) {
      active = false;
      return Response.json({ ok: true });
    }
    if (url.endsWith("/browser/login")) active = true;
    return active
      ? Response.json(browserReply(future(3600)))
      : Response.json({ ok: false, code: "unauthorized" }, { status: 401 });
  };
  const first = apiHarness(responder, { origin, storage, locks });
  const second = apiHarness(responder, { origin, storage, locks });
  await first.api.login("admin", "password", true);
  await second.api.restoreSession();
  let lost = 0;
  second.api.onAuthenticationLost = () => lost += 1;
  await first.api.logout();
  second.dispatchStorage(`amaterasu.session.cookie-logout.${encodeURIComponent(origin)}`, "signal");
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(second.api.subject, "");
  assert.equal(lost, 1);
  assert.equal(first.calls.at(-1).url, `${origin}/v1/auth/browser/logout`);
  assert.equal(storage.values.size, 1);
  assert.ok(!JSON.stringify([...storage.values]).includes("ama_"));
});

test("HTTPS cookie login fails closed when browser storage is denied", async () => {
  const origin = "https://host.example";
  const { api, calls } = apiHarness(
    (url) => Response.json(
      url.endsWith("/browser/logout")
        ? { ok: true }
        : browserReply(future(3600)),
    ),
    { origin, storageGetterThrows: true },
  );
  await assert.rejects(api.login("admin", "password", true), {
    code: "storage_unavailable",
  });
  assert.equal(api.subject, "");
  await assert.rejects(api.restoreSession(), { code: "storage_unavailable" });
  assert.equal(calls.length, 0);
});

test("HTTPS cookie mode fails closed without Web Locks", async () => {
  const origin = "https://host.example";
  const { api, calls } = apiHarness(
    () => Response.json(browserReply(future(3600))),
    { origin, locks: null },
  );
  await assert.rejects(api.login("admin", "password", true), /Web Locks/);
  assert.equal(await api.restoreSession(), false);
  assert.equal(calls.length, 0);
});

test("rejected HTTPS cookie refresh clears local session and reports auth loss", async () => {
  const origin = "https://host.example";
  let signedIn = true;
  const responder = (url) => {
    if (url.endsWith("/browser/login"))
      return Response.json(browserReply(future(5)));
    if (url.endsWith("/browser/session"))
      return Response.json(
        signedIn ? browserReply(future(5)) : { ok: false, code: "unauthorized" },
        { status: signedIn ? 200 : 401 },
      );
    if (url.endsWith("/browser/logout")) return Response.json({ ok: true });
    return Response.json({ ok: false, code: "token_not_found" }, { status: 401 });
  };
  const { api, calls } = apiHarness(responder, { origin });
  await api.login("admin", "password", true);
  let lost = 0;
  api.onAuthenticationLost = () => lost += 1;
  signedIn = false;
  await assert.rejects(api.post("catalog/list"), { code: "token_not_found" });
  assert.equal(api.subject, "");
  assert.equal(lost, 1);
  assert.equal(calls.at(-2).url, `${origin}/v1/auth/browser/refresh`);
  assert.equal(calls.at(-1).url, `${origin}/v1/auth/browser/logout`);
});

test("HTTPS logout waits for in-flight rotation then revokes the cookie", async () => {
  const origin = "https://host.example";
  let activeSession = "original";
  let logouts = 0;
  let refreshStarted;
  const started = new Promise((resolve) => refreshStarted = resolve);
  let finishRefresh;
  const refreshReply = new Promise((resolve) => finishRefresh = resolve);
  const responder = (url) => {
    if (url.endsWith("/browser/login") || url.endsWith("/browser/session"))
      return Response.json(browserReply(future(5)));
    if (url.endsWith("/browser/refresh")) {
      refreshStarted();
      return refreshReply;
    }
    if (url.endsWith("/browser/logout")) {
      logouts += 1;
      if (logouts === 2) {
        assert.equal(activeSession, "rotated");
        activeSession = "revoked";
      }
    }
    return Response.json({ ok: true });
  };
  const { api, calls } = apiHarness(responder, { origin });
  await api.login("admin", "password", true);
  const request = api.post("catalog/list");
  await started;
  const logout = api.logout();
  assert.equal(api.subject, "");
  assert.equal(
    calls.find((call) => call.url.endsWith("/browser/refresh")).options.signal.aborted,
    false,
  );
  activeSession = "rotated";
  finishRefresh(Response.json(browserReply(future(3600))));
  await assert.rejects(request, { code: "unauthorized" });
  await logout;
  assert.equal(calls.at(-1).url, `${origin}/v1/auth/browser/logout`);
  assert.equal(calls.some((call) => call.url.endsWith("/catalog/list")), false);
  assert.equal(activeSession, "revoked");
});

test("peer sign-out during a locked login cancels that login without a later revoke", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  const locks = lockManager();
  let logins = 0;
  let logouts = 0;
  let active = false;
  let loginStarted;
  const started = new Promise((resolve) => loginStarted = resolve);
  let finishLogin;
  const delayedLogin = new Promise((resolve) => finishLogin = resolve);
  const responder = (url) => {
    if (url.endsWith("/browser/logout")) {
      logouts += 1;
      active = false;
      return Response.json({ ok: true });
    }
    if (url.endsWith("/browser/login")) {
      logins += 1;
      if (logins === 2) {
        loginStarted();
        return delayedLogin;
      }
      active = true;
    }
    return Response.json(browserReply(future(3600)));
  };
  const first = apiHarness(responder, { origin, storage, locks });
  const second = apiHarness(responder, { origin, storage, locks });
  await first.api.login("admin", "password", true);
  const replacing = second.api.login("replacement", "password", true);
  await started;
  const logout = first.api.logout();
  active = true;
  finishLogin(Response.json(browserReply(future(3600))));
  await assert.rejects(replacing, /cancelled by a pending sign-out/);
  await assert.rejects(logout, { code: "logout_unconfirmed" });
  assert.equal(active, false);
  assert.equal(logouts, 3); // Initial cleanup, replacement cleanup, pending cleanup.
  assert.equal(second.api.subject, "");
});

test("queued logout reports unconfirmed when peer replaces its marker and session", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  const locks = lockManager();
  let logouts = 0;
  let peerSessionActive = false;
  const { api } = apiHarness((url) => {
    if (url.endsWith("/browser/logout")) {
      logouts += 1;
      return Response.json({ ok: true });
    }
    return Response.json(browserReply(future(3600)));
  }, { origin, storage, locks });
  await api.login("admin", "password", true);

  let release;
  let acquired;
  const lockAcquired = new Promise((resolve) => acquired = resolve);
  const blocker = locks.request(
    `amaterasu.session.refresh:${origin}`,
    { mode: "exclusive" },
    () => new Promise((resolve) => {
      release = resolve;
      acquired();
    }),
  );
  await lockAcquired;
  const logout = api.logout();
  storage.removeItem(`amaterasu.session.cookie-pending-logout.${encodeURIComponent(origin)}`);
  peerSessionActive = true;
  release();
  await blocker;

  await assert.rejects(logout, { code: "logout_unconfirmed" });
  assert.equal(logouts, 1); // Initial pre-login cleanup only.
  assert.equal(peerSessionActive, true);
  assert.equal(api.subject, "");
});

test("queued stale HTTPS login cannot overwrite a newer cookie session", async () => {
  const origin = "https://host.example";
  const locks = lockManager();
  let release;
  let acquired;
  const lockAcquired = new Promise((resolve) => acquired = resolve);
  const blocker = locks.request(
    `amaterasu.session.refresh:${origin}`,
    { mode: "exclusive" },
    () => new Promise((resolve) => {
      release = resolve;
      acquired();
    }),
  );
  const { api, calls } = apiHarness(
    () => Response.json(browserReply(future(3600))),
    { origin, locks },
  );
  const winner = api.login("admin", "password", true);
  const stale = api.login("old", "password", true);
  await lockAcquired;
  release();
  await blocker;
  await winner;
  await assert.rejects(stale, /cancelled/);
  assert.equal(api.subject, "admin");
  assert.equal(calls.filter((call) => call.url.endsWith("/browser/login")).length, 1);
});

test("repeated HTTPS login revokes the prior session before slot replacement", async () => {
  const origin = "https://host.example";
  let active = false;
  let allocated = 0;
  let peak = 0;
  const { api, calls } = apiHarness((url) => {
    if (url.endsWith("/browser/logout")) {
      if (!active)
        return Response.json(
          { ok: false, code: "session_missing" }, { status: 401 },
        );
      active = false;
      allocated -= 1;
      return Response.json({ ok: true });
    }
    if (url.endsWith("/browser/login")) {
      active = true;
      allocated += 1;
      peak = Math.max(peak, allocated);
      if (allocated > 16)
        return Response.json({ ok: false, code: "capacity" }, { status: 503 });
    }
    return Response.json(browserReply(future(3600)));
  }, { origin });

  for (let index = 0; index < 20; index += 1)
    await api.login("admin", "password", true);

  assert.equal(allocated, 1);
  assert.equal(peak, 1);
  assert.equal(api.subject, "admin");
  for (let index = 0; index < calls.length - 1; index += 2) {
    assert.ok(calls[index].url.endsWith("/browser/logout"));
    assert.ok(calls[index + 1].url.endsWith("/browser/login"));
  }
});

test("failed HTTPS replacement blocks new login and preserves pending revocation", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  let logouts = 0;
  let available = true;
  const { api, calls } = apiHarness((url) => {
    if (url.endsWith("/browser/logout")) {
      logouts += 1;
      return Response.json(
        available
          ? { ok: false, code: "session_missing" }
          : { ok: false, code: "unavailable" },
        { status: available ? 401 : 503 },
      );
    }
    return Response.json(browserReply(future(3600)));
  }, { origin, storage });
  await api.login("admin", "password", true);
  available = false;
  await assert.rejects(api.login("replacement", "password", true), {
    code: "logout_unconfirmed",
  });
  assert.equal(api.subject, "admin");
  assert.equal(calls.filter((call) => call.url.endsWith("/browser/login")).length, 1);
  assert.ok(storage.getItem(
    `amaterasu.session.cookie-pending-logout.${encodeURIComponent(origin)}`,
  ));
  await assert.rejects(api.post("catalog/list"), { code: "logout_unconfirmed" });
  assert.equal(logouts, 2);
});

test("unconfirmed HTTPS logout blocks reload until revocation succeeds", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  const locks = lockManager();
  let available = true;
  const responder = (url) => {
    if (url.endsWith("/browser/logout"))
      return Response.json(
        available ? { ok: true } : { ok: false, code: "unavailable" },
        { status: available ? 200 : 503 },
      );
    return Response.json(browserReply(future(3600)));
  };
  const first = apiHarness(responder, { origin, storage, locks });
  await first.api.login("admin", "password", true);
  available = false;
  await assert.rejects(first.api.logout(), { code: "logout_unconfirmed" });
  assert.equal(first.api.subject, "");
  const pendingKey = `amaterasu.session.cookie-pending-logout.${encodeURIComponent(origin)}`;
  assert.ok(storage.getItem(pendingKey));

  available = true;
  const reloaded = apiHarness(responder, { origin, storage, locks });
  assert.equal(await reloaded.api.restoreSession(), false);
  assert.equal(reloaded.calls.length, 1);
  assert.equal(reloaded.calls[0].url, `${origin}/v1/auth/browser/logout`);
  assert.equal(storage.getItem(pendingKey), null);
});

test("storage denied after login prevents HTTPS logout from losing its retry marker", async () => {
  const origin = "https://host.example";
  const failures = {};
  const storage = memoryStorage(failures);
  let available = true;
  const { api, calls } = apiHarness(
    (url) => Response.json(
      url.endsWith("/browser/logout")
        ? available ? { ok: true } : { ok: false, code: "unavailable" }
        : browserReply(future(3600)),
      { status: url.endsWith("/browser/logout") && !available ? 503 : 200 },
    ),
    { origin, storage },
  );
  await api.login("admin", "password", true);
  available = false;
  failures.get = true;
  failures.set = true;
  await assert.rejects(api.logout(), {
    code: "storage_unavailable",
  });
  assert.equal(api.subject, "admin");
  assert.equal(calls.filter((call) => call.url.endsWith("/browser/logout")).length, 1);
  const reloaded = apiHarness(
    () => Response.json(browserReply(future(3600))),
    { origin, storage },
  );
  await assert.rejects(reloaded.api.restoreSession(), {
    code: "storage_unavailable",
  });
  assert.equal(reloaded.calls.length, 0);
});

test("HTTPS logout keeps a retry marker when 401 does not prove cookie clearing", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  let logouts = 0;
  const { api } = apiHarness(
    (url) => Response.json(
      url.endsWith("/browser/logout")
        ? { ok: false, code: ++logouts === 1 ? "session_missing" : "token_not_found" }
        : browserReply(future(3600)),
      { status: url.endsWith("/browser/logout") ? 401 : 200 },
    ),
    { origin, storage },
  );
  await api.login("admin", "password", true);
  await assert.rejects(api.logout(), { code: "logout_unconfirmed" });
  assert.equal(api.subject, "");
  assert.ok(
    storage.getItem(`amaterasu.session.cookie-pending-logout.${encodeURIComponent(origin)}`),
  );
});

test("HTTPS logout accepts session_missing 401 with cleared cookies", async () => {
  const origin = "https://host.example";
  const storage = memoryStorage();
  const { api } = apiHarness(
    (url) => Response.json(
      url.endsWith("/browser/logout")
        ? { ok: false, code: "session_missing" }
        : browserReply(future(3600)),
      { status: url.endsWith("/browser/logout") ? 401 : 200 },
    ),
    { origin, storage },
  );
  await api.login("admin", "password", true);
  await api.logout();
  assert.equal(api.subject, "");
  assert.equal(
    storage.getItem(`amaterasu.session.cookie-pending-logout.${encodeURIComponent(origin)}`),
    null,
  );
});

test("session-only login remains in memory when storage methods throw", async () => {
  const bearer = secret("a");
  const refreshToken = secret("b");
  const failures = { get: true, remove: true, set: true };
  const storage = memoryStorage(failures);
  const { api, calls } = apiHarness(
    () => Response.json(sessionReply(bearer, refreshToken, future(3600), future(7200))),
    { storage },
  );
  await api.login("admin", "password", false);

  assert.equal(api.subject, "admin");
  assert.equal(calls.length, 1);
  assert.equal(storage.values.size, 0);
});
