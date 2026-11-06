// 実際の AIエージェント（mugiurushi-ai）と実際のモデルで、麦漆の画面からガイドラインとの照合を1回通して記録する。
// 画面は公開リポジトリ sjl/t の事例で開くが、送るのは人が書く架空のシステムの説明だけ（コード・記録は送らない）。
// mugiurushi-ai 側で文書を取り込んでおく（uv run mugiurushi-ai ingest --all）。
// 必要なもの: GitHubへのgit接続、Python 3、Playwrightのブラウザ、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR）と、
// 運営者が設定するモデル名（MUGIURUSHI_AI_MODEL）・認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// 実際のモデルの応答は毎回変わるため、候補の中身は記録するだけで、形（出典・状態・麦漆の判定可否）を検査する。
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
const temp = await mkdtemp(join(tmpdir(), 'eel-agent-scan-live-'));
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

  // 3. 麦漆の画面版を、AIエージェントの設定つきで起動する。AIの認証は麦漆本体の子プロセスの環境から外す。
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  const app = spawn(binary, ['app', data, '--port', '0', '--execution-mode', 'local'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, '麦漆');
  stage('麦漆を起動（AIエージェントの設定つき）');
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } }); page.setDefaultTimeout(30000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  const agentResponses = [];
  page.on('response', async r => { if (r.url().includes('/api/v1/agent/')) agentResponses.push({ path: new URL(r.url()).pathname, status: r.status(), body: await r.json().catch(() => null) }); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '架空の開発者', role: 'developer' })));
  const target = new URL(url + '/workbench'); target.search = new URLSearchParams({ url: 'https://github.com/' + REPO, ref: commit }); target.hash = 'expect';
  await page.goto(target.href);
  const scan = page.locator('[data-guideline-scan]'); await scan.waitFor();
  const before = (await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json()).workspace;
  stage('確認項目の画面で、ガイドラインとの照合の欄を開いた');

  // 4. 架空のシステムの説明を書き、送る内容を確認 → 架空のデータとして確認して送る → 候補を受け取る。
  const SUMMARY = '架空の診療所で使うタスク管理の試作。職員がブラウザから患者ごとの連絡タスク（架空の氏名・電話番号・内容）を追加し、サーバーのファイルに保存して一覧で読み直す。職員はIDとパスワードでログインする。操作の記録は残していない。';
  await scan.locator('textarea').fill(SUMMARY);
  await scan.getByRole('button', { name: /ガイドラインと照らす/ }).click();
  const preview = scan.locator('[data-agent-preview]'); await preview.waitFor();
  record.scan_preview = agentResponses.at(-1)?.body;
  assert.deepEqual(record.scan_preview.payload, { system: { summary: SUMMARY, excerpts: [] } }, '送るのは書いた説明だけ');
  stage('照合：送る内容を表示（まだ送っていない）', { bytes: record.scan_preview.bytes });
  await page.screenshot({ path: join(output, 'agent-scan-live-preview.png'), fullPage: true });
  await preview.locator('[data-agent-synthetic]').check();
  await preview.getByRole('button', { name: 'この内容を送る' }).click();
  // 麦漆からAIエージェントへの要求は300秒で打ち切る。失敗なら画面の警告と応答を記録して止める。
  const sentAt = Date.now();
  await Promise.race([scan.locator('[data-scan-sources]').waitFor({ timeout: 330000 }), page.locator('#alert .dads-notification-banner').waitFor({ timeout: 330000 })]);
  record.scan_seconds = Math.round((Date.now() - sentAt) / 1000);
  const sent = agentResponses.at(-1);
  if (sent?.status !== 200) record.alert = await page.locator('#alert').innerText().catch(() => null);
  assert.equal(sent.status, 200, JSON.stringify(sent.body));
  record.scan = sent.body;
  const findings = sent.body.findings;
  stage('照合：実際のAIから候補を受け取り、画面に表示した', {
    findings: findings.length, executable: findings.filter(f => f.verification.executable_check).length,
    sources: Object.fromEntries(sent.body.sources.map(s => [s.source_id, s.status])), model_call: sent.body.model_call });
  // 形の検査：出典は文書・ページ・SHA-256つき、麦漆の判定可否は観測の種類の一覧どおり、照合で事例は変わらない。
  assert(findings.length > 0, '照合の候補が無い');
  for (const f of findings) {
    assert(f.citations.length > 0 && f.citations.every(c => c.document && c.page && /^[0-9a-f]{64}$/.test(c.sha256)), '出典の欠けた候補');
    assert.equal(f.verification.executable_check, f.verification.support === 'judged');
  }
  assert(sent.body.sources.some(s => s.status === 'used'), '断片を渡した出典が無い');
  assert.equal(await scan.locator('[data-scan-finding]').count(), findings.length);
  const after = (await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json()).workspace;
  assert.equal(after.revision, before.revision, '照合で事例の記録は変わらない');
  await scan.locator('details').evaluateAll(ds => ds.forEach(d => { d.open = true; }));
  await scan.screenshot({ path: join(output, 'agent-scan-live-result.png') });

  // 5. 人が候補を1件選び、確認項目の文に直して保存する。出所に照合の候補と出典が残り、観測の種類と判定条件は未設定のまま。
  const pick = findings[0];
  await scan.locator('[data-scan-finding="0"]').getByRole('button', { name: /この候補をもとに確認項目を書く/ }).click();
  assert.equal(await page.locator('#expect-text').inputValue(), pick.requirement);
  const written = `架空の職員がタスク一覧を開いたとき、${pick.requirement}`;
  await page.locator('#expect-text').fill(written);
  await page.getByRole('button', { name: 'この動作を確かめる' }).click();
  let saved;
  for (let i = 0; i < 60 && !saved; i++) { saved = (await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json()).workspace.checks.find(c => c.text === written); if (!saved) await new Promise(r => setTimeout(r, 250)); }
  assert(saved, '確認項目が保存されていない');
  assert.match(saved.origin, /ガイドライン照合のAI候補/); assert.match(saved.origin, new RegExp(pick.citations[0].sha256));
  assert.equal(saved.observation, null); assert.deepEqual(saved.aspects ?? [], []);
  record.saved_check = { id: saved.id, text: saved.text, origin: saved.origin };
  stage('人が候補を1件選び、文を直して確認項目として保存した', { id: saved.id });
  assert.deepEqual(errors, []);
  record.completed = true;
} catch (error) {
  record.completed = false; record.error = String(error.message || error).slice(0, 4000);
  throw error;
} finally {
  await writeFile(join(output, 'agent-scan-live.json'), JSON.stringify(record, null, 2));
  await browser?.close();
  for (const child of children) child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
  console.log(JSON.stringify({ completed: record.completed, stages: record.stages.map(s => s.name), output: relative(root, join(output, 'agent-scan-live.json')) }));
}
