// 架空の画面で、操作の記録が手順になり、その手順を既存の実行器で再実行できることを確かめる。
// 「人の操作」は、ブラウザへ実際の入力イベントを送って作る。実在のアプリでの結果の代わりにはしない。
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {spawn} from 'node:child_process';
import {mkdtemp, readFile, writeFile, mkdir, stat, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve, join} from 'node:path';
import {chromium} from 'playwright';
import {record, RULES} from './browser-recorder.mjs';

const root = resolve(import.meta.dirname, '..');
const temp = await mkdtemp(join(tmpdir(), 'eel-recorder-'));
const saved = [];
const html = body => `<!doctype html><html lang="ja"><body>${body}</body></html>`;
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/save') { let body = ''; for await (const chunk of req) body += chunk; saved.push(JSON.parse(body)); res.end('ok'); return; }
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  if (url.pathname === '/health') { res.end(); return; }
  if (url.pathname === '/side') { res.end(html('<label>メモ<input name="memo"></label>')); return; }
  if (url.pathname !== '/') { res.writeHead(404); res.end(); return; }
  res.end(html(`<label>利用者<input id="user"></label><label>パスワード<input type="password" id="pass"></label><button id="login">ログイン</button>
    <div id="form" hidden><label>記録<textarea name="note"></textarea></label><select name="ward" aria-label="病棟"><option>東</option><option>西</option></select>
      <style>.hidden-input{position:absolute;width:1px;height:1px;margin:-1px;padding:0;border:0;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap}</style>
      <input class="hidden-input" type="radio" name="sex" id="sex-m" value="M"><label for="sex-m"><span>男</span></label>
      <input class="hidden-input" type="radio" name="sex" id="sex-f" value="F"><label for="sex-f"><span>女</span></label>
      <button id="detail-1">詳細</button><button id="detail-2">詳細</button>
      <label>連絡先<input name="contact"></label><button id="clear-contact">連絡先を消す</button><iframe name="side" src="/side"></iframe><button id="save">保存</button><p id="result"></p></div>
    <script>
      let opened = 0;
      // 押すと、ボタンの文字が変わる。押す前の画面では「ログイン」という名前のボタンが1つだけある。
      document.querySelector('#login').onclick = event => { event.target.textContent = '処理中'; document.querySelector('#form').hidden = false; };
      // 押すと、HEADの要求を送る。書き込みではないので、待ち合わせにはしない。
      document.querySelector('#detail-2').onclick = () => { opened++; fetch('/health', {method: 'HEAD'}); };
      document.querySelector('#clear-contact').onclick = () => { document.querySelector('input[name=contact]').value = ''; };
      document.querySelector('#save').onclick = async () => {
        const memo = document.querySelector('iframe').contentDocument.querySelector('input').value;
        await (await fetch('/save', {method: 'POST', body: JSON.stringify({user: document.querySelector('#user').value, pass: document.querySelector('#pass').value, note: document.querySelector('textarea').value, ward: document.querySelector('select').value, sex: document.querySelector('input[name=sex]:checked')?.value ?? null, contact: document.querySelector('input[name=contact]').value, memo, opened})})).text();
        document.querySelector('#result').textContent = '保存した';
      };
    </script>`));
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const origin = `http://127.0.0.1:${server.address().port}`;
const secrets = {user: 'clerk-secret-id-5521', pass: 'pw-secret-xyz-9907'};
const checks = [];
let browser;
try {
  browser = await chromium.launch({channel: 'chromium', headless: true, chromiumSandbox: true, timeout: 15000, executablePath: process.env.EEL_BROWSER_EXECUTABLE || undefined});
  const version = browser.version();
  const context = await browser.newContext();
  // 秘密値ファイルにあるのは利用者名だけ。パスワードは欄の種類から秘密値として扱われる。
  const recorder = await record(context, {origins: [origin], secrets: {user: secrets.user}, testValue: 'EEL-TEST-REC', runId: 'recording-synthetic'});
  const page = await context.newPage();
  await page.goto(origin + '/');
  // ここからが「人の操作」。
  await page.locator('#user').click(); await page.keyboard.type(secrets.user);
  await page.locator('#pass').click(); await page.keyboard.type(secrets.pass);
  // 人がボタンを押している時間を置く。押し始めてから離すまでの間に、道具が押す前の画面を調べる。
  await page.getByRole('button', {name: 'ログイン'}).click({delay: 150});
  await page.locator('textarea').click(); await page.keyboard.type('所見 EEL-TEST-REC');
  await page.locator('select').selectOption('西');
  // 見た目を作り込んだラジオボタン。欄そのものは隠れていて、人はラベルの文字を押す。
  await page.locator('label[for=sex-m] span').click();
  await page.frameLocator('iframe').locator('input').click(); await page.keyboard.type('メモ EEL-TEST-REC');
  // HEADの応答が届いてから、次の操作をする。応答はクリックの直後に届くことがあるので、押す前から待ち始める。
  const head = page.waitForResponse(r => r.request().method() === 'HEAD');
  await page.locator('#detail-2').click();
  await head;
  // 入れた値を、別の操作が消す。人は同じ値を入れ直す。値は1回の入力で入る（貼り付けと同じ）。
  await page.locator('input[name=contact]').fill('内線 12');
  await page.locator('#clear-contact').click();
  await page.locator('input[name=contact]').fill('内線 12');
  await page.locator('#save').click();
  await page.locator('#result').filter({hasText: '保存した'}).waitFor();
  const recorded = await recorder.stop();
  await browser.close(); browser = null;

  assert.deepEqual(recorded.problems, []);
  const text = JSON.stringify(recorded);
  assert.ok(!text.includes(secrets.pass) && !text.includes(secrets.user), '秘密値が記録に残っている');
  assert.ok(!text.includes('EEL-TEST-REC'), '記録のときの試験値が残っている');
  const brief = recorded.actions.map(s => [s.action, s.target.frames.join(' '), s.target.locator.by, s.target.locator.text || s.target.locator.name || s.target.locator.selector, s.value?.source ? `${s.value.source}:${s.value.key ?? s.value.value}` : s.value ?? '']);
  assert.deepEqual(brief, [
    ['fill', '', 'label', '利用者', 'secret:user'],
    ['fill', '', 'label', 'パスワード', 'secret:pass'],
    ['click', '', 'role', 'ログイン', ''],
    ['fill', '', 'label', '記録', 'literal:所見 {{test_value}}'],
    ['select', '', 'label', '病棟', '西'],
    // ラベルを押した操作は、ラベルとして記録する。隠れた欄へのクリックは記録しない。
    ['click', '', 'css', 'label[for="sex-m"]', ''],
    ['fill', 'iframe[name="side"]', 'label', 'メモ', 'literal:メモ {{test_value}}'],
    // 同じ名前のボタンが2つあるので、役割では一意にならず、IDのCSSになる。
    ['click', '', 'css', '#detail-2', ''],
    // 前と同じ値の入れ直しも、手順として残す。残さないと、再実行で欄が空のままになる。
    ['fill', '', 'label', '連絡先', 'literal:内線 12'],
    ['click', '', 'role', '連絡先を消す', ''],
    ['fill', '', 'label', '連絡先', 'literal:内線 12'],
    ['click', '', 'role', '保存', ''],
  ]);
  assert.ok(recorded.actions.every(s => /^[a-z0-9_-]{1,64}$/.test(s.id)) && new Set(recorded.actions.map(s => s.id)).size === recorded.actions.length);
  // 保存のボタンを押した後に送られた書き込みの要求は、次の操作の前に応答が届いたので、待ち合わせとして残る。
  const save = recorded.actions.at(-1);
  assert.deepEqual(recorded.responses, [{id: 'w-' + save.id, method: 'POST', path: '/save'}]);
  assert.equal(save.wait_response, recorded.responses[0].id);
  assert.ok(recorded.actions.slice(0, -1).every(s => !('wait_response' in s)));
  assert.deepEqual(Object.keys(recorded.provenance.responses), [recorded.responses[0].id]);
  assert.deepEqual(Object.keys(recorded.provenance.steps), recorded.actions.map(s => s.id));
  assert.ok(Object.values(recorded.provenance.steps).every(b => b.source === 'operation_recording' && b.run_id === 'recording-synthetic' && b.rules === RULES));
  checks.push('操作をラベル・役割・CSSの順で一意に指し、秘密値と試験値を記録に残さない');

  // 記録した手順を、既存の実行器でそのまま再実行する。別の試験値で、同じ操作の結果になる。
  const dir = join(temp, 'replay'); await mkdir(dir);
  const secretsPath = join(temp, 'secrets.json'); await writeFile(secretsPath, JSON.stringify(secrets), {mode: 0o600});
  const run_id = 'synthetic-replay', test_value = 'EEL-TEST-REPLAY-7', timeout_ms = 20000 * Number(process.env.EEL_BROWSER_SMOKE_TIMEOUT_SCALE || 1);
  const spec = {service: 'web', tls: false, start_path: '/', expected_version: version, preparation: [], actions: [...recorded.actions, {id: 'done', action: 'read_text', target: {locator: {by: 'css', selector: '#result'}}, contains: '保存した'}], responses: recorded.responses, database: []};
  await writeFile(join(dir, 'request.json'), JSON.stringify({schema_version: 1, spec, run_id, test_value, origin, origins: [origin], timeout_ms}));
  saved.length = 0;
  const child = spawn(process.execPath, [join(root, 'tools/browser-runner.mjs'), join(dir, 'request.json')], {env: {...process.env, EEL_PLAYWRIGHT_MODULE: join(root, 'node_modules/playwright/index.mjs'), EEL_BROWSER_SECRETS_FILE: secretsPath}, stdio: 'pipe'});
  let stderr = ''; child.stderr.on('data', d => stderr += d); child.stdout.resume();
  const waitFile = async name => { const until = Date.now() + timeout_ms + 10000; while (Date.now() < until) { if (await stat(join(dir, name)).then(() => true, () => false)) return JSON.parse(await readFile(join(dir, name), 'utf8')); await new Promise(r => setTimeout(r, 20)); } throw Error(name + ' が出ない\n' + stderr); };
  const ready = await waitFile('ready.json'); assert.equal(ready.prepared, true, ready.detail);
  await writeFile(join(dir, 'continue.json'), JSON.stringify({run_id, proceed: true}));
  const result = await waitFile('result.json'); child.kill();
  assert.equal(result.completed, true, result.detail + '\n' + JSON.stringify(result.steps.filter(s => !s.completed)));
  assert.deepEqual(saved, [{user: secrets.user, pass: secrets.pass, note: '所見 ' + test_value, ward: '西', sex: 'M', contact: '内線 12', memo: 'メモ ' + test_value, opened: 1}]);
  checks.push('記録した手順を既存の実行器で再実行でき、別の試験値で同じ操作の結果になる');
  console.log(JSON.stringify({version, steps: recorded.actions.length, checks}));
} finally {
  if (browser) await browser.close().catch(() => {});
  server.close();
  await rm(temp, {recursive: true, force: true});
}
