import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const context = vm.createContext({});
vm.runInContext(ts.transpile(readFileSync("src/plugin_drivers.ts", "utf8"), { target:ts.ScriptTarget.ES2022 }),context);
const drivers = context.Amaterasu;
const driver = {id:"example.display",name:"Virtual display",publisher:"Example",reason:"Creates a screen",required_by:["example.plugin"],state:"missing",message:"",may_require_reboot:true,installed_version:null};
const status = () => ({ok:true,supported:true,installation_revision:"a".repeat(64),drivers:[],operation:{id:"setup1",revision:"2",installation_revision:"a".repeat(64),phase:"awaiting_approval",approval:"b".repeat(64),drivers:[driver],error:null,cancellable:true}});

test("driver approval retains exact operation and installation revisions", () => {
  const parsed = drivers.parsePluginDriverStatus(status());
  assert.equal(parsed.operation.revision,"2");
  assert.equal(parsed.operation.installationRevision,"a".repeat(64));
  assert.equal(parsed.operation.drivers[0].requiredBy[0],"example.plugin");
});
test("missing provisioning is distinct from no required drivers", () => {
  assert.equal(drivers.parsePluginDriverStatus({ok:true,supported:false}).supported,false);
  const ready = status(); ready.operation=null;
  assert.equal(drivers.parsePluginDriverStatus(ready).supported,true);
});
test("stale preparation cannot supply an approval for another installation", () => {
  const value=status(); value.operation.installation_revision="c".repeat(64);
  assert.throws(()=>drivers.parsePluginDriverStatus(value));
});
test("unknown readiness, duplicate identities and oversized lists fail closed", () => {
  const value=status(); value.operation.drivers=[driver,{...driver}];
  assert.throws(()=>drivers.parsePluginDriverStatus(value));
  value.operation.drivers=[{...driver,state:"installed"}];
  assert.throws(()=>drivers.parsePluginDriverStatus(value));
  value.operation.drivers=Array(65).fill(driver);
  assert.throws(()=>drivers.parsePluginDriverStatus(value));
});
test("revision strings preserve u64 identity without rounding", () => {
  const value=status(); value.operation.revision="18446744073709551615";
  assert.equal(drivers.parsePluginDriverStatus(value).operation.revision,value.operation.revision);
  value.operation.revision="18446744073709551616";
  assert.throws(()=>drivers.parsePluginDriverStatus(value));
});
test("driver requirements retain component identities for multi-component plugin details", () => {
  const value=status();
  value.operation.drivers[0]={...driver,required_components:["example.capture","example.display"]};
  assert.deepEqual(Array.from(drivers.parsePluginDriverStatus(value).operation.drivers[0].requiredComponents),["example.capture","example.display"]);
});

test("update status distinguishes signed bundled versions from unknown information", () => {
  const value=status();
  value.operation.drivers=[{...driver,installed_version:"1.9",offered_version:"1.10",update:"available"}];
  let parsed=drivers.parsePluginDriverStatus(value).operation.drivers[0];
  assert.equal(parsed.offeredVersion,"1.10");
  assert.equal(drivers.driverUpdateLabel(parsed.update),"Update available in this plugin");
  value.operation.drivers=[{...driver}];
  parsed=drivers.parsePluginDriverStatus(value).operation.drivers[0];
  assert.equal(parsed.update,"unknown");
  assert.equal(parsed.offeredVersion,null);
  assert.equal(drivers.driverStateLabel("incompatible"),"Different version required");
});
test("malformed update claims fail closed", () => {
  for (const row of [
    {...driver,update:"available"},
    {...driver,installed_version:"1.9",offered_version:"1.10",update:"latest"},
    {...driver,installed_version:"1.9",offered_version:"1.10\n",update:"available"},
  ]) {
    const value=status(); value.operation.drivers=[row];
    assert.throws(()=>drivers.parsePluginDriverStatus(value));
  }
});
