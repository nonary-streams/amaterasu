import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function clientClass() {
  const context = vm.createContext({ crypto: { getRandomValues: (bytes) => bytes.fill(7) }, Uint8Array });
  const source = readFileSync(new URL("../src/session_extension_actions.ts", import.meta.url), "utf8");
  vm.runInContext(ts.transpile(source, { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
  return context.Amaterasu.ExtensionActionClient;
}
const plain = (value) => JSON.parse(JSON.stringify(value));

test("action client uses authenticated host snapshot and stable receipt identity", async () => {
  const calls = [];
  const api = { async post(route, payload) {
    calls.push([route, plain(payload ?? {})]);
    if (route.endsWith("open_epoch")) return { epoch: "epoch" };
    if (route.endsWith("snapshot")) return { host_epoch: "host" };
    return { status: "rejected", operation_id: "epoch:1", reason: "lifecycle_permit_unavailable" };
  } };
  const Client = clientClass(); const client = new Client(api);
  const receipt = await client.invoke("sample/action", { enabled: false });
  assert.equal(receipt.status, "rejected");
  assert.deepEqual(calls[2], ["extension-actions/invoke", { epoch: "epoch", sequence: 1, operation_id: "epoch:1", action_id: "sample/action", target: { kind: "host", host_epoch: "host" }, arguments: { enabled: false }, expected_settings_revision: null }]);
  assert.equal((await client.status()).reason, "lifecycle_permit_unavailable");
  await assert.rejects(client.invoke("sample/action", {}), /already/);
});

test("lost invoke response never causes a new action and open retries retain their identity", async () => {
  const calls = []; let openAttempts = 0;
  const api = { async post(route, payload) {
    calls.push([route, plain(payload ?? {})]);
    if (route.endsWith("open_epoch")) { if (++openAttempts === 1) throw new Error("timeout"); return { epoch: "epoch" }; }
    if (route.endsWith("snapshot")) return { host_epoch: "host" };
    if (route.endsWith("invoke")) throw new Error("timeout");
    return { status: "queued", operation_id: "epoch:1" };
  } };
  const Client = clientClass(); const client = new Client(api);
  await assert.rejects(client.invoke("sample/action", {}), /timeout/);
  await assert.rejects(client.invoke("sample/action", {}), /timeout/);
  assert.deepEqual(calls.filter(([route]) => route.endsWith("open_epoch"))[0], calls.filter(([route]) => route.endsWith("open_epoch"))[1]);
  await assert.rejects(client.invoke("sample/action", {}), /already/);
  assert.equal((await client.status()).status, "queued");
  assert.equal(calls.filter(([route]) => route.endsWith("invoke")).length, 1);
});


test("successful receipt remains distinguishable from a rejected action", async () => {
  const api = { async post(route) {
    if (route.endsWith("open_epoch")) return { epoch: "epoch" };
    if (route.endsWith("snapshot")) return { host_epoch: "host" };
    return { status: "succeeded", operation_id: "epoch:1", result: { done: true } };
  } };
  const Client = clientClass();
  assert.equal((await new Client(api).invoke("sample/action", {})).status, "succeeded");
});


test("only a confirmed rejected receipt can be retired", async () => {
  const calls = [];
  const api = { async post(route) { calls.push(route); if (route.endsWith("snapshot")) return { host_epoch: "h" }; if (route.endsWith("open_epoch")) return { epoch: "e" }; return { status: "rejected", operation_id: "e:1" }; } };
  const Client = clientClass(); const client = new Client(api);
  const receipt = await client.invoke("sample/action", {});
  await assert.rejects(client.retireRejected({ status: "succeeded", operation_id: "e:1" }));
  await client.retireRejected(receipt);
  assert.deepEqual(calls.slice(-2), ["extension-actions/close_epoch", "extension-actions/retire_epoch"]);
});
