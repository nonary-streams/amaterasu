import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({ FormData, Error });
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
  const entry = ordinary(model.entryFromForm(form));
  assert.deepEqual(entry.launch, {
    kind: "executable",
    program: "/opt/example",
    arguments: ["--flag", "$(do-not-run)", "a b"],
    working_directory: null,
  });
  assert.equal(entry.name, "Example");
  assert.deepEqual(entry.tags, ["arcade", "co-op"]);
});

test("invalid argument arrays fail before sending mutations", () => {
  for (const value of ["{}", "[1]", "[null]", '["ok", {}]', "not JSON"]) {
    assert.throws(() => model.parseArguments(value));
  }
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

test("page counts never claim an exact total while continuation exists", () => {
  assert.equal(model.countLabel(12, { after: "next" }), "12+");
  assert.equal(model.countLabel(0, { after: "continue-search" }), "0+");
  assert.equal(model.countLabel(12, null), "12");
});
