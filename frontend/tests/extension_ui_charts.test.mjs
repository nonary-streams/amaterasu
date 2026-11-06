import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
const context=vm.createContext({});
vm.runInContext(ts.transpile(readFileSync('src/extension_ui_charts.ts','utf8'),{target:ts.ScriptTarget.ES2022}),context);
const validate=context.Amaterasu.validateExtensionChartSeries;
test('chart SDK preserves missing values and bounds ordered series',()=>{
 const series=[{id:'a',label:'Measurement',points:[{time:1,value:0},{time:2,value:null},{time:3,value:12.5}]}];
 const before=JSON.stringify(series);validate(series);assert.equal(JSON.stringify(series),before);
 for(const points of [[{time:NaN,value:1}],[{time:1,value:Infinity}],[{time:2,value:1},{time:1,value:2}],Array.from({length:2001},(_,time)=>({time,value:1}))])
  assert.throws(()=>validate([{id:'a',label:'Measurement',points}]));
 assert.throws(()=>validate([...series,...series]));
});
