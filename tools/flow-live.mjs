// 実際の AIエージェント（mugiurushi-ai）と実際のモデルで、新しい流れ（/flow）を画面から通して記録する。
// 照合の準備（読むファイルの選択）→ 照合とヒアリング → 試験の案 → 了承して確認項目に登録 → 結果の画面。
// 対象は公開リポジトリ sjl/t。送るのは公開コードの抜粋と架空の説明だけ。
// mugiurushi-ai 側で文書を取り込んでおく（uv run mugiurushi-ai ingest --all）。
// 必要なもの: GitHubへのgit接続、Python 3、Playwrightのブラウザ、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR）と、
// 運営者が設定するモデル名（MUGIURUSHI_AI_MODEL）・認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// 実際のモデルの応答は毎回変わるため、中身は記録するだけで、流れが進むこと・送る内容・登録した記録の形を検査する。
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
const temp = await mkdtemp(join(tmpdir(), 'eel-flow-live-'));
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

  // 3. mugiurushi の画面版を、AIエージェントの設定つきで起動する。AIの認証は本体の子プロセスの環境から外す。
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  const app = spawn(binary, ['app', data, '--port', '0', '--execution-mode', 'local'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi');
  stage('mugiurushi を起動（AIエージェントの設定つき）');
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } }); page.setDefaultTimeout(30000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  const sent = [];
  page.on('response', async r => { if (r.url().includes('/api/v1/agent/')) sent.push({ path: new URL(r.url()).pathname, status: r.status(), body: await r.json().catch(() => null) }); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '架空の開発者', role: 'developer' })));
  const target = new URL(url + '/flow'); target.search = new URLSearchParams({ url: 'https://github.com/' + REPO, ref: commit });
  await page.goto(target.href);
  await page.locator('[data-files]').waitFor();
  const shot = async name => page.screenshot({ path: join(output, `flow-live-${name}.png`), fullPage: true });
  await shot('1-prepare');
  stage('1 照合の準備を開いた');
  const send = async () => {
    const preview = page.locator('[data-preview]'); await preview.waitFor();
    const before = sent.length;
    record.previews = [...(record.previews || []), sent.at(-1)?.body && { path: sent.at(-1).path, bytes: sent.at(-1).body.bytes, payload_keys: Object.keys(sent.at(-1).body.payload || {}) }];
    await preview.locator('#agree').check();
    await preview.getByRole('button', { name: 'この内容を送る' }).click();
    for (let i = 0; i < 330 && sent.length === before; i++) await new Promise(r => setTimeout(r, 1000));
    const res = sent.at(-1); assert.equal(res.status, 200, JSON.stringify(res.body)); return res.body;
  };

  // 4. 照合：選んだファイルの抜粋と説明を送る。
  await page.locator('#desc').fill('架空の診療所で、職員が患者ごとの連絡タスク（架空の氏名・電話番号・内容）を管理するのに使う。');
  await page.getByRole('button', { name: /照合を始める/ }).click();
  const scanPreview = await (async () => { await page.locator('[data-preview]').waitFor(); return sent.at(-1).body; })();
  const checked = await page.locator('[data-files] tbody tr').evaluateAll(rows => rows.filter(r => r.querySelector('input').checked).map(r => r.children[1].textContent));
  assert.deepEqual(scanPreview.payload.system.excerpts.map(e => e.ref), checked, '選んだファイルだけを送る');
  record.files = checked;
  const scan = await send();
  record.scan = { findings: scan.findings.length, testable: scan.findings.filter(f => f.verification.executable_check).length, questions: scan.questions, priorities: scan.findings.map(f => f.priority), model_call: scan.model_call,
    used_sources: scan.sources.filter(s => s.status === 'used').map(s => s.source_id) };
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  await shot('2-scan');
  stage('2 照合とヒアリング：実際のAIから受け取った', record.scan);

  // 5. ヒアリングに1件答え、試験の案を作る。
  const firstQuestion = page.locator('[data-question]').first();
  if (await firstQuestion.count()) await firstQuestion.locator('input[type=radio]').first().check();
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  const plan = await send();
  record.plan = { tests: plan.tests.map(t => ({ name: t.name, kind: t.kind, findings: t.finding_ids.length, observation: t.observation, steps: t.steps.length })), not_planned: plan.not_planned.length, model_call: plan.model_call };
  await page.locator('[data-test]').first().waitFor();
  await shot('3-plan');
  stage('3 検証案：実際のAIから試験の案を受け取った', record.plan);

  // 6. 了承して確認項目に登録する。登録した確認項目の確かめる部分は、照合の候補と同じ数。
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  await page.locator('[data-result]').first().waitFor({ timeout: 60000 });
  const view = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json();
  const checks = view.workspace.checks.filter(c => c.id.startsWith('flow-'));
  assert.equal(checks.length, plan.tests.length, '了承した試験ごとに確認項目を登録する');
  assert.deepEqual(checks.map(c => c.aspects.length), plan.tests.map(t => t.finding_ids.length), '照合の候補を確かめる部分として登録する');
  record.checks = checks.map(c => ({ id: c.id, text: c.text, aspects: c.aspects.map(a => ({ id: a.id, observation: a.observation, linked: a.trial_checks.length })) }));
  await shot('4-result');
  stage('4 結果：確認項目に登録し、試験の設定へのリンクを表示した', { checks: checks.length });
  assert.deepEqual(errors, []);
  record.completed = true;
} catch (error) {
  record.completed = false; record.error = String(error.message || error).slice(0, 4000);
  throw error;
} finally {
  await writeFile(join(output, 'flow-live.json'), JSON.stringify(record, null, 2));
  await browser?.close();
  for (const child of children) child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
  console.log(JSON.stringify({ completed: record.completed, stages: record.stages.map(s => s.name), output: relative(root, join(output, 'flow-live.json')) }));
}
