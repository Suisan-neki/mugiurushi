// 試験の案で、照合の候補が試験・AIが試験にしなかった候補・未分類（AIの案に含まれなかった）・了承しなかった候補の
// どれかとして画面に残り、黙って消えないことを確かめる合成試験。AIエージェントは偽物のHTTPサーバーで、
// 麦漆のサーバーが応答を検査する（渡していない候補や重複を含む案は受け取らない）経路をそのまま通す。試験の実行はしない。
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const temp = await mkdtemp(join(tmpdir(), 'eel-flow-plan-')), data = join(temp, 'app');
const repository = 'example/clinic', commit = 'c'.repeat(40);
await mkdir(join(data, 'snapshots', repository), { recursive: true });
await writeFile(join(data, 'snapshots', repository, commit + '.json'), JSON.stringify({
  schema: 1, fullName: repository, reference: commit, commit,
  inventory: [{ path: 'app/tasks.py', mode: '100644' }],
  files: { 'app/tasks.py': 'def list_tasks(user):\n    return ALL\n' }, omitted: [],
}));
const tokenFile = join(temp, 'agent-token'); await writeFile(tokenFile, 'synthetic-agent-token-0123456789\n');

// 偽のAIエージェント。照合は毎回同じ4件（うち1件は文書の確認で試験にできない）、試験の案は呼ばれた順に返す。
const cite = { passage_id: 'p:1', source_id: 'owasp-top10', document: '合成のOWASP', page: '1', sha256: 'b'.repeat(64), origin: 'x', excerpt: '原則拒否' };
const finding = (requirement, method = 'readback_output') => ({ requirement, relevance: 'likely', citations: [cite], system_refs: ['app/tasks.py'], verification: { method, how: '合成' }, unknowns: [], patient_data: true, priority: 'high' });
const scanAnswer = { findings: [finding('担当外に出さない'), finding('操作の記録を残す', 'stored_file'), finding('保存先の外に書かない', 'stored_file'), finding('運用の手順書', 'document_review')],
  questions: [], sources: [], not_covered: [], model_call: { response_id: 'fake-scan' } };
const t = (name, ids) => ({ id: 't-' + name, name, kind: 'cli', finding_ids: ids, steps: ['合成の操作'], pass: ['合成の条件'], observation: 'readback_output' });
const plans = [
  { tests: [t('未知', ['f-9'])], not_planned: [] }, // 渡していない候補
  { tests: [t('一覧', ['f-1'])], not_planned: [{ finding_id: 'f-1', reason: 'x' }] }, // 試験と試験にしなかった候補の両方
  { tests: [], not_planned: [] }, // 空の案
  { tests: [t('一覧', ['f-1'])], not_planned: [{ finding_id: 'f-2', reason: '本番の構成による' }] }, // f-3 が欠落
  { tests: [t('一覧', ['f-1']), t('記録', ['f-2'])], not_planned: [{ finding_id: 'f-3', reason: '保存先の仕様が分からない' }], unclassified: [] }, // 了承しない試験あり
];
let planCalls = 0;
const agent = createServer((req, res) => {
  let body = ''; req.on('data', d => { body += d; });
  req.on('end', () => {
    const answer = req.url === '/v1/scan' ? scanAnswer : req.url === '/v1/plan' ? { ...plans[planCalls++], model_call: { response_id: `fake-plan-${planCalls}` } } : null;
    res.writeHead(answer ? 200 : 404, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(answer || {}));
  });
}).listen(0, '127.0.0.1');
await new Promise(ok => agent.once('listening', ok));
const child = spawn(process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi'), ['app', data, '--port', '0'], {
  cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, EEL_AGENT_URL: `http://127.0.0.1:${agent.address().port}`, EEL_AGENT_TOKEN_FILE: tokenFile } });
let browser;
try {
  const url = await new Promise((ok, no) => {
    let out = '', err = ''; const timer = setTimeout(() => no(Error(err)), 30000);
    child.stdout.on('data', d => { out += d; const m = /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/.exec(out); if (m) { clearTimeout(timer); ok(m[1]); } });
    child.stderr.on('data', d => { err += d; });
    child.once('exit', () => { clearTimeout(timer); no(Error(err)); });
  });
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined, args: ['--no-sandbox'] });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } }); page.setDefaultTimeout(15000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '合成の開発者', role: 'developer' })));
  const send = async () => { await page.locator('[data-preview]').waitFor(); await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click(); };
  const planOnce = async () => { await page.locator('#steps button').nth(1).click(); await page.getByRole('button', { name: '試験の案を作る' }).click(); await send(); };
  const alert = async () => { await page.locator('#alert .alert').waitFor(); const text = await page.locator('#alert .alert').innerText(); await page.locator('#alert').evaluate(e => e.replaceChildren()); return text; };
  const u = new URL(url + '/flow'); u.search = new URLSearchParams({ url: 'https://github.com/' + repository, ref: commit }).toString();
  await page.goto(u.href);
  await page.locator('[data-files]').waitFor();
  await page.getByRole('button', { name: '送信内容を確認する' }).click(); await send();
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();

  // 未知ID・両方への重複：サーバーが受け取らず、案は記録しない。
  await page.getByRole('button', { name: '試験の案を作る' }).click(); await send();
  assert.match(await alert(), /渡していない候補/);
  await planOnce();
  assert.match(await alert(), /試験と、試験にしなかった候補の両方/);
  assert.equal(await page.locator('#steps button').nth(2).isDisabled(), true, '受け取らなかった案で次へ進めない');

  // 空の案：試験は0件で、送った3件すべてが未分類として残る。
  await planOnce();
  await page.locator('[data-plan-count]', { hasText: '0件の試験' }).waitFor();
  assert.match(await page.locator('[data-unclassified]').innerText(), /試験の案に含まれず、理由もない項目：担当外に出さない、操作の記録を残す、保存先の外に書かない/);

  // 欠落：AIが試験にしなかった候補は理由つき、どちらにも入らなかった候補は未分類として残す。
  await planOnce();
  await page.locator('[data-test="t-一覧"]').waitFor();
  assert.match(await page.locator('[data-not-planned]').innerText(), /操作の記録を残す（AIの理由：本番の構成による）/);
  assert.match(await page.locator('[data-unclassified]').innerText(), /保存先の外に書かない/);
  await page.getByRole('button', { name: '了承して試験を設定する' }).click();
  await page.locator('[data-untested-section]').waitFor();
  assert.deepEqual(await page.locator('[data-result]').evaluateAll(r => r.map(x => x.getAttribute('data-result'))), ['pending'], '試験は未実行として出す');
  assert.deepEqual(await page.locator('[data-untested]').evaluateAll(r => r.map(x => [x.getAttribute('data-untested'), x.children[1].textContent])),
    [['not_planned', '操作の記録を残す'], ['unclassified', '保存先の外に書かない']]);
  const untested = await page.locator('[data-untested-section]').innerText();
  assert.match(untested, /合否は付けていません[\s\S]*AIが試験にしなかった[\s\S]*AIの理由：本番の構成による[\s\S]*AIの試験の案に含まれなかった（未分類）/);
  assert.doesNotMatch(untested, /運用の手順書/, '試験の案に送っていない候補（試験で確かめられない候補）はこの表の対象外');
  assert.doesNotMatch(untested, /未実行|未確認|不足|期待どおり/, '未実行・未確認・合否と混ぜない');

  // 了承しなかった試験の候補は「了承していない」として残す。
  await planOnce();
  await page.locator('[data-test="t-記録"]').waitFor();
  await page.locator('[data-test="t-記録"] input[type=checkbox]').uncheck();
  await page.getByRole('button', { name: '了承して試験を設定する' }).click();
  await page.locator('[data-untested-section]').waitFor();
  assert.deepEqual(await page.locator('[data-untested]').evaluateAll(r => r.map(x => [x.getAttribute('data-untested'), x.children[1].textContent])),
    [['not_approved', '操作の記録を残す'], ['not_planned', '保存先の外に書かない']]);
  assert.equal(planCalls, 5);
  assert.deepEqual(errors, []);
  console.log('flow plan accounting smoke: ok');
} finally {
  await browser?.close();
  child.kill('SIGTERM'); agent.close();
  await rm(temp, { recursive: true, force: true });
}
