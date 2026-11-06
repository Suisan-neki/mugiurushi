// 実際の AIエージェント（mugiurushi-ai）と実際のモデルで、麦漆の画面から一往復を通して記録する。
// 対象は公開リポジトリ sjl/t（架空のタスクを追加して一覧で読み直す）。送るのは架空のデータだけ。
// 必要なもの: GitHubへのgit接続、Python 3、Playwrightのブラウザ、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR）と、
// 運営者が設定するモデル名（MUGIURUSHI_AI_MODEL）・認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// 実際のモデルの応答は毎回変わるため、このスクリプトは麦漆の判定を変えず、通った段階と結果を記録する。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, readdir, stat, rm } from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve, relative } from 'node:path';
import { createServer } from 'node:net';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const binary = process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi');
const agentDir = process.env.MUGIURUSHI_AI_DIR;
assert(agentDir, 'MUGIURUSHI_AI_DIR（mugiurushi-ai のチェックアウト）を指定してください');
assert(process.env.MUGIURUSHI_AI_MODEL, 'MUGIURUSHI_AI_MODEL を指定してください');
const REPO = 'sjl/t', SHORT = '70817fd';
const output = join(root, 'test-results'); await mkdir(output, { recursive: true });
const temp = await mkdtemp(join(tmpdir(), 'eel-agent-live-'));
const record = { started_at: new Date().toISOString(), model: process.env.MUGIURUSHI_AI_MODEL, repository: REPO, stages: [] };
const stage = (name, detail = {}) => { record.stages.push({ name, at: new Date().toISOString(), ...detail }); console.log('・', name); };
const children = [];
const started = (child, pattern, label) => new Promise((ok, no) => {
  let out = '', err = ''; const timer = setTimeout(() => no(Error(`${label} の起動待ちが上限に達した: ${err.slice(-1500)}`)), 60000);
  child.stdout.on('data', d => { out += d; const m = pattern.exec(out + err); if (m) { clearTimeout(timer); ok(m[1]); } });
  child.stderr.on('data', d => { err += d; const m = pattern.exec(out + err); if (m) { clearTimeout(timer); ok(m[1]); } });
  child.once('exit', code => { clearTimeout(timer); no(Error(`${label} が終了した（${code}）: ${err.slice(-1500)}`)); });
});
let browser;
try {
  // 1. 実際の mugiurushi-ai を起動する。共有トークンは試行ごとに作り、記録しない。
  const token = randomBytes(24).toString('hex'), tokenFile = join(temp, 'agent-token');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const port = await new Promise(ok => { const s = createServer().listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => ok(p)); }); });
  const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(port)], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(agent);
  const agentUrl = await started(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
  record.agent_commit = execFileSync('git', ['-C', agentDir, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  stage('mugiurushi-ai を起動', { agent_commit: record.agent_commit });

  // 2. sjl/t の固定コミットの読取記録を置く（この環境ではGitHub APIでほかのリポジトリを読めないため、gitで取得して作る）。
  const repo = join(temp, 'sjl-t');
  execFileSync('git', ['clone', '-q', '--filter=blob:none', `https://github.com/${REPO}`, repo]);
  const commit = execFileSync('git', ['-C', repo, 'rev-parse', SHORT], { encoding: 'utf8' }).trim();
  execFileSync('git', ['-C', repo, 'checkout', '-q', commit]);
  const files = {}, inventory = [];
  for (const path of execFileSync('git', ['-C', repo, 'ls-files'], { encoding: 'utf8' }).trim().split('\n')) {
    const full = join(repo, path); if (!(await stat(full)).isFile()) continue;
    inventory.push({ path, mode: '100644' }); files[path] = await readFile(full, 'utf8');
  }
  const data = join(temp, 'app');
  await mkdir(join(data, 'snapshots', REPO), { recursive: true });
  await writeFile(join(data, 'snapshots', REPO, commit + '.json'), JSON.stringify({ schema: 1, fullName: REPO, reference: commit, commit, inventory, files, omitted: [] }));
  record.commit = commit;

  // 3. 麦漆の画面版を、AIエージェントの設定つきで起動する。
  const app = spawn(binary, ['app', data, '--port', '0', '--execution-mode', 'local'], { env: { ...process.env, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, '麦漆');
  stage('麦漆を起動（AIエージェントの設定つき）');
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } }); page.setDefaultTimeout(30000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  const agentResponses = [];
  page.on('response', async r => { if (r.url().includes('/api/v1/agent/')) agentResponses.push({ path: new URL(r.url()).pathname, status: r.status(), body: await r.json().catch(() => null) }); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '架空の開発者', role: 'developer' })));
  await page.goto(url + '/workbench');
  const reviewToken = await page.locator('meta[name=review-token]').getAttribute('content'), headers = { 'X-Review-Token': reviewToken, 'Content-Type': 'application/json' };
  const view = async () => (await page.request.get(url + '/api/v1/case', { headers })).json();
  const checkId = 'sjl-t-agent';
  const r = await page.request.post(url + '/api/v1/change', { headers, data: { expected_revision: (await view()).workspace.revision, actor: '架空の開発者', reason: 'AIエージェントの通し確認', action: { op: 'add_check', check: {
    id: checkId, version: 1, text: '架空のタスクを追加すると、保存先に書き込まれ、後から一覧で読み直せる', origin: '架空の確認項目（通し確認用）',
    source: { repository: REPO, commit, path: 't.py', line: 1, end_line: 1 }, observation: null, rule: 'manual_only', dependencies: [], questions: [] } } } });
  assert.equal(r.status(), 200, await r.text());
  const target = new URL(url + '/workbench'); target.search = new URLSearchParams({ url: 'https://github.com/' + REPO, ref: commit, check: checkId }); target.hash = 'trial';
  await page.goto(target.href);
  await page.getByRole('button', { name: '通常時の設定例を使う' }).click();
  for (const name of ['次へ：試す状況を選ぶ', '次へ：判定の条件を確認する', '実行前に確認する']) {
    const b = page.getByRole('button', { name, exact: true }); if (await b.isVisible()) await b.click();
  }
  const editor = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' });
  await editor.waitFor();
  stage('通常時の設定例で、実行前の確認画面を開いた');

  // 4. AIの案：送る内容を確認 → 架空のデータとして確認して送る → 案を受け取る。
  const proposals = editor.locator('[data-agent-proposals]');
  await proposals.getByRole('button', { name: /AIに確かめる範囲の案を出してもらう/ }).click();
  const preview = proposals.locator('[data-agent-preview]'); await preview.waitFor();
  record.propose_preview = agentResponses.at(-1)?.body;
  stage('案：送る内容を表示（まだ送っていない）', { bytes: record.propose_preview?.bytes });
  await page.screenshot({ path: join(output, 'agent-live-propose-preview.png'), fullPage: true });
  await preview.locator('[data-agent-synthetic]').check();
  await preview.getByRole('button', { name: 'この内容を送る' }).click();
  await proposals.locator('[data-proposal]').first().waitFor({ timeout: 300000 });
  const sent = agentResponses.at(-1);
  assert.equal(sent.status, 200, JSON.stringify(sent.body));
  record.propose = sent.body;
  stage('案：実際のAIから受け取った', { proposals: sent.body.proposals.length, questions: sent.body.questions.length, out_of_scope: sent.body.out_of_scope.length, model_call: sent.body.model_call });
  await page.screenshot({ path: join(output, 'agent-live-proposals.png'), fullPage: true });

  // 5. 人が1件を採用し、観測の種類と判定条件（後からの読み出し c3）を選んで保存する。
  // 判定条件 c3 は別のプロセスで一覧を読み直す観測なので、AIが読み直しを観測の案に挙げた案を選ぶ（無ければ最初の案）。
  const readback = sent.body.proposals.find(p => p.observation === 'readback_output');
  const first = readback || sent.body.proposals[0];
  record.selection = readback ? 'AIが観測の種類に readback_output を挙げた案を選んだ' : 'readback_output を挙げた案が無く、最初の案を選んだ（内容と判定条件の対応は要確認）';
  await proposals.locator(`[data-proposal="${first.id}"] input[type=checkbox]`).check();
  await proposals.getByRole('button', { name: /選んだ案を確かめたい部分に加える/ }).click();
  const row = editor.locator(`[data-aspect="${first.id}"]`);
  await row.locator('select').selectOption('readback_output');
  await row.locator('input[type=checkbox][value="c3"]').check();
  const before = (await view()).workspace.revision;
  await editor.getByRole('button', { name: '対応づけを保存する' }).click();
  for (let i = 0; i < 60 && (await view()).workspace.revision === before; i++) await new Promise(r => setTimeout(r, 250));
  const saved = (await view()).workspace.checks.find(c => c.id === checkId).aspects;
  assert.equal(saved.length, 1); assert.equal(saved[0].id, first.id); assert.equal(saved[0].trial_checks[0].id, 'c3');
  record.adopted = { id: first.id, text: first.text, ai_observation: first.observation, chosen_observation: 'readback_output', trial_check: 'c3' };
  stage('人が案を1件採用し、観測の種類と判定条件を選んで保存した', record.adopted);

  // 6. 既存の判定条件で試験を実行する。
  await page.getByRole('button', { name: '試験を始める', exact: true }).click();
  await page.locator('#result:not([hidden])').waitFor({ timeout: 300000 });
  const checkView = (await view()).checks.find(c => c.check_id === checkId);
  record.trial = { verdict: checkView.verdict, verification_passed: checkView.verification_passed, coverage: checkView.coverage.status, aspects: checkView.coverage.aspects.map(a => ({ id: a.id, status: a.status })) };
  stage('既存の判定条件で試験を実行した', record.trial);
  await page.screenshot({ path: join(output, 'agent-live-result.png'), fullPage: true });

  // 7. 結果の説明：送る内容を確認 → 送る → 麦漆の状態とAIの説明を分けて表示。
  const coverage = page.locator('[data-coverage]');
  await coverage.getByRole('button', { name: /AIに結果を説明してもらう/ }).click();
  const explainPreview = coverage.locator('[data-agent-preview]'); await explainPreview.waitFor();
  record.explain_preview = agentResponses.at(-1)?.body;
  stage('説明：送る内容を表示（まだ送っていない）', { bytes: record.explain_preview?.bytes });
  await explainPreview.locator('[data-agent-synthetic]').check();
  await explainPreview.getByRole('button', { name: 'この内容を送る' }).click();
  await coverage.locator('[data-agent-explanation] tbody tr').first().waitFor({ timeout: 300000 });
  const explained = agentResponses.at(-1);
  assert.equal(explained.status, 200, JSON.stringify(explained.body));
  record.explain = explained.body;
  assert.deepEqual(explained.body.aspects.map(a => a.status), record.trial.aspects.map(a => a.status), '説明の状態は麦漆の値');
  stage('説明：実際のAIから受け取り、麦漆の状態と分けて表示した', { model_call: explained.body.model_call });
  await page.screenshot({ path: join(output, 'agent-live-explanation.png'), fullPage: true });
  assert.deepEqual(errors, []);
  record.completed = true;
} catch (error) {
  record.completed = false; record.error = String(error.message || error).slice(0, 4000);
  throw error;
} finally {
  await writeFile(join(output, 'agent-live.json'), JSON.stringify(record, null, 2));
  await browser?.close();
  for (const child of children) child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
  console.log(JSON.stringify({ completed: record.completed, stages: record.stages.map(s => s.name), output: relative(root, join(output, 'agent-live.json')) }));
}
