// Drives an owned ephemeral Chromium over loopback CDP. Never emits credentials or cookies.
import { readFileSync } from 'node:fs';
const {debuggerUrl, origin, adminPassword, viewerPassword} = JSON.parse(readFileSync(0, 'utf8'));
const ws = new WebSocket(debuggerUrl);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let next = 0;
const pending = new Map();
ws.onmessage = ({data}) => {
  const message = JSON.parse(data);
  const waiter = pending.get(message.id);
  if (!waiter) return;
  pending.delete(message.id);
  if (message.error) waiter.reject(new Error('CDP command failed'));
  else waiter.resolve(message.result);
};
function command(method, params = {}, sessionId) {
  return new Promise((resolve, reject) => {
    const id = ++next;
    pending.set(id, {resolve, reject});
    ws.send(JSON.stringify({id, method, params, ...(sessionId ? {sessionId} : {})}));
  });
}
const check = (condition, message) => { if (!condition) throw new Error(message); };
try {
  const {targetId} = await command('Target.createTarget', {url: 'about:blank'});
  const {sessionId} = await command('Target.attachToTarget', {targetId, flatten: true});
  await command('Page.enable', {}, sessionId);
  await command('Runtime.enable', {}, sessionId);
  await command('Page.navigate', {url: origin + '/'}, sessionId);
  let ready = false;
  for (let i = 0; i < 100; i++) {
    await new Promise(r => setTimeout(r, 50));
    const v = await command('Runtime.evaluate', {expression: 'location.origin', returnByValue: true}, sessionId);
    if (v.result.value === origin) { ready = true; break; }
  }
  check(ready, 'browser origin did not load');
  const expression = `(async () => {
    const req = async (path, body={}, csrf=true) => {
      const headers = {'Content-Type':'application/json'};
      if (csrf) headers['X-Amaterasu-CSRF'] = '1';
      const response = await fetch(path, {method:'POST', credentials:'same-origin', headers, body:JSON.stringify(body)});
      const json = await response.json();
      return {status:response.status, json};
    };
    const check = (v, message) => { if (!v) throw new Error(message); };
    const login = async (username, password, role) => {
      const r = await req('/v1/auth/browser/login', {username,password,remember_me:true});
      check(r.status===200 && r.json.ok && r.json.role===role, role+' browser login');
      check(!('bearer' in r.json) && !('refresh_token' in r.json), 'browser credentials exposed');
    };
    await login('admin', ${JSON.stringify(adminPassword)}, 'administrator');
    let r = await req('/v1/accounts/list');
    check(r.status===200 && r.json.ok, 'admin browser account access');
    r = await req('/v1/accounts/list', {}, false);
    check(r.status===403 && r.json.code==='csrf_denied', 'missing CSRF accepted');
    r = await req('/v1/auth/browser/refresh');
    check(r.status===200 && r.json.ok && r.json.role==='administrator', 'admin browser refresh');
    r = await req('/v1/auth/browser/logout');
    check(r.status===200 && r.json.ok, 'admin browser logout');
    r = await req('/v1/accounts/list');
    check(r.status===401, 'logged-out browser retained access');
    await login('viewer', ${JSON.stringify(viewerPassword)}, 'viewer');
    r = await req('/v1/catalog/list');
    check(r.status===200 && r.json.ok, 'viewer browser read');
    r = await req('/v1/accounts/list');
    check(r.status===403, 'viewer browser account access');
    r = await req('/v1/auth/browser/refresh');
    check(r.status===200 && r.json.ok && r.json.role==='viewer', 'viewer browser refresh');
    r = await req('/v1/auth/browser/logout');
    check(r.status===200 && r.json.ok, 'viewer browser logout');
    r = await req('/v1/catalog/list');
    check(r.status===401, 'viewer browser retained access');
    return true;
  })()`;
  const result = await command('Runtime.evaluate', {expression, awaitPromise:true, returnByValue:true}, sessionId);
  check(!result.exceptionDetails && result.result.value === true,
    result.exceptionDetails?.exception?.description || 'browser role, cookie or CSRF checks failed');
  await command('Target.closeTarget', {targetId});
  console.log('Browser cookie role, refresh, logout, and CSRF checks passed.');
} finally { ws.close(); }
