import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ atob, Blob, Error });
vm.runInContext(
  ts.transpile(readFileSync("src/catalog.ts", "utf8"), {
    target: ts.ScriptTarget.ES2022,
  }),
  context,
);
const readSteamArtwork = context.Amaterasu.readSteamArtwork;
const reference = { provider: "steam", app_id: 730, revision: "42" };

test("Steam artwork chunks assemble into the expected image without a local path", async () => {
  const source = Buffer.alloc(8195, 0x5a);
  const offsets = [];
  const api = {
    async post(path, request) {
      assert.equal(path, "steam/artwork_read");
      assert.equal(request.app_id, 730);
      assert.equal(request.revision, "42");
      offsets.push(request.offset);
      const end = Math.min(request.offset + 8192, source.length);
      return {
        ok: true,
        app_id: 730,
        revision: "42",
        offset: request.offset,
        next_offset: end === source.length ? null : end,
        total_bytes: source.length,
        mime_type: "image/png",
        data: source.subarray(request.offset, end).toString("base64"),
      };
    },
  };
  const image = await readSteamArtwork(api, reference);
  assert.deepEqual(offsets, [0, 8192]);
  assert.equal(image.type, "image/png");
  assert.deepEqual(Buffer.from(await image.arrayBuffer()), source);
});

test("Steam artwork transfer stops on changed revision and invalid cursors", async () => {
  const api = {
    async post(_path, request) {
      return {
        ok: true,
        app_id: 730,
        revision: request.offset ? "43" : "42",
        offset: request.offset,
        next_offset: request.offset ? null : 8192,
        total_bytes: 8193,
        mime_type: "image/png",
        data: Buffer.alloc(request.offset ? 1 : 8192).toString("base64"),
      };
    },
  };
  await assert.rejects(readSteamArtwork(api, reference), /changed during transfer/);
  await assert.rejects(readSteamArtwork(api, { ...reference, revision: "../art" }), /Invalid Steam artwork reference/);
  const invalidCursor = {
    post: async () => ({
      ok: true, app_id: 730, revision: "42", offset: 0,
      next_offset: 1, total_bytes: 8193, mime_type: "image/png",
      data: Buffer.alloc(8192).toString("base64"),
    }),
  };
  await assert.rejects(readSteamArtwork(invalidCursor, reference), /Invalid Steam artwork cursor/);
});

test("Steam artwork accepts a 16 MiB transfer declaration but rejects larger images", async () => {
  let calls = 0;
  const api = {
    async post(_path, request) {
      calls++;
      return {
        ok: true, app_id: 730, revision: request.offset ? "43" : "42",
        offset: request.offset, next_offset: 8192,
        total_bytes: 16 * 1024 * 1024,
        mime_type: "image/jpeg",
        data: Buffer.alloc(8192).toString("base64"),
      };
    },
  };
  await assert.rejects(readSteamArtwork(api, reference), /changed during transfer/);
  assert.equal(calls, 2);
  calls = 0;
  const oversized = { post: async (path, request) => ({
    ...await api.post(path, request), total_bytes: 16 * 1024 * 1024 + 1,
  }) };
  await assert.rejects(readSteamArtwork(oversized, reference), /changed during transfer/);
  assert.equal(calls, 1);
});
