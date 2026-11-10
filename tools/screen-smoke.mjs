// 架空の画面で、画面の構造・HTTPのやり取り・属性の読み取りの記録を確かめる。
// 実在のアプリでの結果の代わりにはしない。
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {spawn} from 'node:child_process';
import {mkdtemp, readFile, writeFile, mkdir, stat, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve, join} from 'node:path';
import {chromium} from 'playwright';

const root = resolve(import.meta.dirname, '..');
const temp = await mkdtemp(join(tmpdir(), 'eel-screen-'));
const launched = await chromium.launch({channel: 'chromium', headless: true, chromiumSandbox: true, timeout: 15000, executablePath: process.env.EEL_BROWSER_EXECUTABLE || undefined, args: JSON.parse(process.env.EEL_BROWSER_EXTRA_ARGS || '[]')});
const version = launched.version(); await launched.close();

// 記録に残ってはいけない値。入力欄の値、Cookieの値、クエリ文字列、対象外のヘッダー、本文。
const forbidden = {hidden: 'HIDDEN-FIELD-VALUE-7731', cookie: 'COOKIE-VALUE-5520', query: 'QUERY-VALUE-9914', header: 'UNLISTED-HEADER-VALUE-3308', body: 'RESPONSE-BODY-VALUE-6642', prefilled: 'PREFILLED-VALUE-1187'};
const stored = [];
const html = body => `<!doctype html><html lang="ja"><head><title>架空の受診記録</title></head><body>${body}</body></html>`;
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  let posted = '';
  if (req.method === 'POST') for await (const chunk of req) posted += chunk;
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('X-Unlisted', forbidden.header);
  if (url.pathname === '/') {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    res.setHeader('X-Frame-Options', 'SAMEORIGIN');
    res.setHeader('Set-Cookie', [`session=${forbidden.cookie}; HttpOnly; SameSite=Strict; Path=/`, `theme=${forbidden.cookie}; Secure`]);
    res.end(html(`<h1>受診の記録</h1>
      <form action="/save?next=${forbidden.query}" method="post" onsubmit="return false">
        <label>利用者<input name="user" id="user" value="${forbidden.prefilled}"></label>
        <label for="pin">暗証</label><input type="password" name="pin" id="pin" autocomplete="off" maxlength="8" required>
        <input type="hidden" name="csrf" value="${forbidden.hidden}">
        <select name="ward" aria-label="病棟"><option>東</option></select>
        <textarea name="note" readonly></textarea>
        <button type="submit" id="save">保存する</button>
      </form>
      <a href="/next">次の画面</a><a href="https://example.invalid/outside">外部</a>
      <p id="result"></p>
      <iframe name="outer" src="/outer"></iframe>
      <script>document.querySelector('#save').onclick = async () => {
        await fetch('/api/undeclared?token=${forbidden.query}');
        await fetch('/save', {method: 'POST', body: 'x'});
        document.querySelector('#result').textContent = '保存の操作を終えた';
      };</script>`));
    return;
  }
  if (url.pathname === '/outer') { res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(html('<h2>外側の枠</h2><iframe id="inner" src="/inner"></iframe>')); return; }
  if (url.pathname === '/inner') { res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(html('<label>深い欄<input name="deep" disabled></label>')); return; }
  if (url.pathname === '/many') { res.setHeader('Content-Type', 'text/html; charset=utf-8'); res.end(html('<h1>多い画面</h1>' + Array.from({length: 320}, (_, i) => `<input name="f${i}">`).join(''))); return; }
  if (url.pathname === '/track') {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    res.end(html(`<label>記録<input id="note"></label><button id="send">送る</button><p id="shown"></p>
      <script>document.querySelector('#send').onclick = async () => {
        const v = document.querySelector('#note').value;
        await (await fetch('/echo?q=' + encodeURIComponent(v))).text();
        await fetch('/store', {method: 'POST', headers: {'Content-Type': 'application/x-www-form-urlencoded'}, body: 'note=' + encodeURIComponent(v).replace(/%20/g, '+')});
        document.querySelector('#shown').textContent = '保存した値: ' + v;
      };</script>`));
    return;
  }
  if (url.pathname === '/echo') { res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify({q: url.searchParams.get('q')})); return; }
  if (url.pathname === '/store') { stored.push(posted); res.setHeader('Content-Type', 'text/plain'); res.end('ok'); return; }
  if (url.pathname === '/save' || url.pathname === '/api/undeclared') { res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify({message: forbidden.body})); return; }
  res.writeHead(404); res.end();
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const origin = `http://127.0.0.1:${server.address().port}`;
const css = selector => ({locator: {by: 'css', selector}});
const spec = {service: 'web', tls: false, start_path: '/', expected_version: version,
  preparation: [{id: 'opened', action: 'read_text', target: css('h1')}],
  actions: [
    {id: 'type', action: 'fill', target: css('#user'), value: {source: 'literal', value: '{{test_value}}'}},
    {id: 'pin-type', action: 'read_attribute', target: css('#pin'), name: 'type'},
    {id: 'user-autocomplete', action: 'read_attribute', target: css('#user'), name: 'autocomplete'},
    {id: 'save', action: 'click', target: css('#save'), wait_response: 'save'},
    {id: 'done', action: 'read_text', target: css('#result'), contains: '保存の操作を終えた'},
    {id: 'many', action: 'goto', path: '/many'},
  ],
  responses: [{id: 'save', path: '/save', method: 'POST'}], database: []};

async function waitFile(file, child, timeout) {
  const until = Date.now() + timeout;
  while (Date.now() < until) {
    if (await stat(file).then(() => true, () => false)) return JSON.parse(await readFile(file, 'utf8'));
    if (child.exitCode !== null && !(await stat(file).then(() => true, () => false))) throw Error('操作器が ' + file + ' を残さず終了した');
    await new Promise(r => setTimeout(r, 20));
  }
  throw Error('操作器が時間内に終わらなかった');
}
async function run(name, config, extra = {}) {
  const dir = join(temp, name); await mkdir(dir);
  const run_id = 'synthetic-' + name, test_value = 'EEL-TEST-TYPED-4471';
  const scale = Number(process.env.EEL_BROWSER_SMOKE_TIMEOUT_SCALE || 1), timeout_ms = 20000 * scale;
  await writeFile(join(dir, 'request.json'), JSON.stringify({schema_version: 1, spec: config, run_id, test_value, origin, origins: [origin], timeout_ms, ...extra}));
  const child = spawn(process.execPath, [join(root, 'tools/browser-runner.mjs'), join(dir, 'request.json')], {env: {...process.env, EEL_PLAYWRIGHT_MODULE: join(root, 'node_modules/playwright/index.mjs')}, stdio: 'pipe'});
  let stderr = ''; child.stderr.on('data', d => stderr += d); child.stdout.resume();
  try {
    const ready = await waitFile(join(dir, 'ready.json'), child, timeout_ms + 10000);
    assert.equal(ready.prepared, true, ready.detail);
    await writeFile(join(dir, 'continue.json'), JSON.stringify({run_id, proceed: true}));
    const result = await waitFile(join(dir, 'result.json'), child, timeout_ms + 10000);
    const screens = JSON.parse(await readFile(join(dir, 'screens-raw.json'), 'utf8'));
    return {result, screens, test_value};
  } catch (error) { throw new Error(`${name}: ${error.message}\n${stderr}`, {cause: error}); }
  finally { child.kill(); }
}

const checks = [];
try {
  const {result, screens, test_value} = await run('record', spec);
  assert.equal(result.completed, true, result.detail);
  const all = JSON.stringify(result) + JSON.stringify(screens);

  // 入力欄の値、Cookieの値、クエリ文字列、対象外のヘッダー、本文は、どの記録にも現れない。
  for (const [kind, value] of Object.entries(forbidden)) assert.ok(!all.includes(value), `${kind} の値が記録に残っている`);
  assert.ok(!JSON.stringify(screens).includes(test_value), '入力した値が画面の記録に残っている');
  checks.push('入力欄の値・Cookieの値・クエリ文字列・対象外のヘッダー・本文を記録しない');

  // 属性の読み取り。属性がない場合と、値がある場合を区別する。
  const step = id => result.steps.find(s => s.id === id && s.phase === 'action');
  assert.deepEqual(step('pin-type').attribute, {present: true, value: 'password'});
  assert.deepEqual(step('user-autocomplete').attribute, {present: false, value: null});
  assert.equal(step('type').attribute, null);
  checks.push('属性の値と、属性がないことを区別して読む');

  // 画面の構造。入れ子のiframeと、欄の属性。
  assert.equal(screens.schema_version, 1); assert.equal(screens.run_id, 'synthetic-record');
  const opened = screens.screens.find(s => s.step === 'opened' && s.phase === 'preparation');
  assert.ok(opened?.content, '準備の手順の画面の記録がない');
  assert.equal(opened.content.path, '/'); assert.equal(opened.content.title, '架空の受診記録');
  assert.deepEqual(opened.content.frames.map(f => f.chain), [[], ['name=outer'], ['name=outer', 'id=inner']]);
  const [top, outer, inner] = opened.content.frames;
  assert.deepEqual(top.headings, ['受診の記録']); assert.deepEqual(outer.headings, ['外側の枠']);
  const field = name => top.fields.find(f => f.name === name);
  assert.deepEqual(field('pin'), {tag: 'input', input_type: 'password', name: 'pin', id: 'pin', label: '暗証', required: true, readonly: false, disabled: false, autocomplete: 'off', maxlength: 8, form_action: '/save', form_method: 'post'});
  assert.equal(field('user').label, '利用者'); assert.equal(field('user').autocomplete, null);
  assert.deepEqual([field('csrf').input_type, field('csrf').label], ['hidden', null]);
  assert.equal(field('ward').label, '病棟'); assert.equal(field('note').readonly, true);
  assert.ok(top.fields.every(f => !('value' in f)));
  assert.deepEqual(inner.fields.map(f => [f.name, f.label, f.disabled]), [['deep', '深い欄', true]]);
  assert.deepEqual(top.controls, [{tag: 'button', input_type: 'submit', text: '保存する'}]);
  assert.deepEqual(top.links, ['/next']);
  checks.push('入れ子のiframeの位置と、欄の属性・ボタン・同じオリジンのリンクを記録する');

  // 最上位の文書の応答。対象の名前のヘッダーと、Cookieの名前と属性だけ。
  assert.equal(opened.content.document.status, 200);
  assert.deepEqual(Object.keys(opened.content.document.headers).sort(), ['cache-control', 'content-type', 'x-frame-options']);
  assert.deepEqual(opened.content.document.cookies, [
    {name: 'session', secure: false, http_only: true, same_site: 'Strict', path: '/'},
    {name: 'theme', secure: true, http_only: false, same_site: null, path: null},
  ]);
  checks.push('文書の応答は、対象の名前のヘッダーと、Cookieの名前と属性だけを記録する');

  // 内容が変わらない手順は、先行の記録を参照する。
  const typed = screens.screens.find(s => s.step === 'pin-type');
  assert.equal(typed.content, null); assert.equal(typed.same_as, 'preparation:opened');
  const done = screens.screens.find(s => s.step === 'done');
  assert.equal(done.same_as, 'preparation:opened');
  checks.push('内容が同じ画面は、先行の記録への参照にする');

  // 上限を超える画面は、切り捨てたことを残す。
  const many = screens.screens.find(s => s.step === 'many');
  assert.equal(many.content.path, '/many'); assert.equal(many.content.truncated, true);
  assert.equal(many.content.frames[0].fields.length, 300);
  assert.equal(opened.content.truncated, false);
  checks.push('上限を超えた欄は捨て、切り捨てたことを記録する');

  // HTTPのやり取り。宣言していない通信も、操作の段階のものは残る。準備の段階のものは混ぜない。
  const exchange = path => result.exchanges.filter(e => e.path === path);
  assert.equal(exchange('/api/undeclared').length, 1);
  assert.deepEqual({...exchange('/api/undeclared')[0], headers: undefined}, {step: 'save', method: 'GET', path: '/api/undeclared', status: 200, resource: 'fetch', headers: undefined, cookies: [], request_slots: [], response_slots: [], response_unread: false});
  // 追跡する値を渡していない試行では、値の出現の記録は空になる。
  assert.ok(result.steps.every(s => Array.isArray(s.visible_slots) && s.visible_slots.length === 0));
  assert.deepEqual(Object.keys(exchange('/api/undeclared')[0].headers).sort(), ['cache-control', 'content-type']);
  assert.equal(exchange('/save')[0].method, 'POST');
  assert.equal(exchange('/many')[0].step, 'many');
  assert.equal(exchange('/outer').length, 0, '準備の段階の通信が混ざっている');
  assert.equal(result.exchange_limit, false);
  assert.ok(result.exchanges.every(e => !e.path.includes('?')));
  // 宣言した応答には、判定に使うヘッダーが付く。
  assert.equal(result.responses.find(r => r.watch === 'save').headers['cache-control'], 'no-store');
  checks.push('操作中のHTTPのやり取りを、宣言の有無によらず記録する');

  // 要素が見つからない場合、手順は未完了になり、属性なしとは記録しない。
  const missing = await run('missing', {...spec, actions: [{id: 'gone', action: 'read_attribute', target: css('#no-such-element'), name: 'type'}]}).catch(error => error);
  if (missing instanceof Error) throw missing;
  const gone = missing.result.steps.find(s => s.id === 'gone');
  assert.equal(gone.completed, false); assert.equal(gone.attribute, null);
  assert.equal(missing.result.completed, false);
  checks.push('要素が見つからない手順は未完了で、属性なしとは記録しない');

  // 値の追跡。手順ごとの試験値を入力し、要求・応答・画面での出現を、値の出所の名前だけで記録する。
  const base = 'EEL-TEST-TYPED-4471', own = base + '-01', entered = '記録 ' + own;
  const tracking = await run('track', {...spec, start_path: '/track', preparation: [], responses: [{id: 'store', path: '/store', method: 'POST'}],
    actions: [
      {id: 'fill-note', action: 'fill', target: css('#note'), value: {source: 'literal', value: '記録 {{test_value}}'}},
      {id: 'send', action: 'click', target: css('#send'), wait_response: 'store'},
      {id: 'shown', action: 'read_text', target: css('#shown'), contains: '{{test_value}}'},
    ]}, {step_values: {'fill-note': own}, tracked: [{slot: 'test_value', value: base}, {slot: 'fill-note', value: entered}]});
  assert.equal(tracking.result.completed, true, tracking.result.detail);
  // 入力には手順ごとの値が使われ、対象はその値を受け取る。
  assert.deepEqual(stored, ['note=' + encodeURIComponent(entered).replace(/%20/g, '+')]);
  const sent = path => tracking.result.exchanges.find(e => e.path === path);
  // クエリ文字列と、URLエンコードされた本文の中の値も見つける。試験値は入力した値の一部なので、両方が現れる。
  assert.deepEqual(sent('/echo').request_slots, ['test_value', 'fill-note']);
  assert.deepEqual(sent('/echo').response_slots, ['test_value', 'fill-note']);
  assert.deepEqual(sent('/store').request_slots, ['test_value', 'fill-note']);
  assert.deepEqual(sent('/store').response_slots, []);
  // ページが本文を読まなかった応答は、調べられなかったことを記録する。値がないとは扱わない。
  assert.equal(sent('/echo').response_unread, false);
  assert.equal(sent('/store').response_unread, true);
  // 入力欄の中の値は、表示されている文字列として数えない。表示された後の手順でだけ現れる。
  const trackedStep = id => tracking.result.steps.find(s => s.id === id);
  assert.deepEqual(trackedStep('fill-note').visible_slots, []);
  assert.deepEqual(trackedStep('shown').visible_slots.sort(), ['fill-note', 'test_value']);
  // やり取りの記録には、値そのものは入らない。
  assert.ok(!JSON.stringify(tracking.result.exchanges).includes(base));
  checks.push('追跡する値の、要求・応答・画面での出現を、値を残さずに記録する');

  console.log(JSON.stringify({version, checks}));
} finally {
  server.close();
  await rm(temp, {recursive: true, force: true});
}
