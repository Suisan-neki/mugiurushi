// 架空の診療所アプリ（examples/clinic-tasks）で、新しい流れを画面から通し、試験を実際に実行して記録する。
// ガイドラインとの照合 → 検証案の了承 → 試験の設定と実行（既存の設定画面で人が入力する操作を再現）→ 結果 → 修正用プロンプト。
// 実際の AIエージェント（mugiurushi-ai）と実際のモデルを使う。送るのは架空のアプリの公開してよいコードと架空の説明だけ。
// 必要なもの: Python 3、Playwright のブラウザ、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR、文書を取り込み済み）、
// MUGIURUSHI_AI_MODEL と認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// 試験の設定は案から自動では作らない。このスクリプトは、人が設定画面で入力する内容を決まった規則で入力する。
// 対応づけられない項目は対応づけずに残し、結果の画面で「未確認」と表示されることを確かめる。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
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
const REPO = 'example/clinic-tasks';
const output = join(root, 'test-results'); await mkdir(output, { recursive: true });
const temp = await mkdtemp(join(tmpdir(), 'eel-clinic-demo-'));
const record = { started_at: new Date().toISOString(), model: process.env.MUGIURUSHI_AI_MODEL, repository: REPO, stages: [], trials: [] };
const stage = (name, detail = {}) => { record.stages.push({ name, at: new Date().toISOString(), ...detail }); console.log('・', name); };
const children = [];
const started = (child, pattern, label) => new Promise((ok, no) => {
  let out = '', err = ''; const timer = setTimeout(() => no(Error(`${label} の起動待ちが上限に達した: ${err.slice(-1500)}`)), 60000);
  const look = () => { const m = pattern.exec(out + err); if (m) { clearTimeout(timer); ok(m[1]); } };
  child.stdout.on('data', d => { out += d; look(); }); child.stderr.on('data', d => { err += d; look(); });
  child.once('exit', code => { clearTimeout(timer); no(Error(`${label} が終了した（${code}）: ${err.slice(-1500)}`)); });
});
let browser;
try {
  // 1. 架空のリポジトリを手元に作る。最初の版（不足あり）だけを使う。
  const repo = join(temp, 'clinic-tasks');
  await mkdir(repo, { recursive: true });
  const git = (...a) => execFileSync('git', ['-C', repo, ...a], { encoding: 'utf8' }).trim();
  git('init', '-q'); git('config', 'user.name', '架空の開発者'); git('config', 'user.email', 'demo@example.invalid');
  git('remote', 'add', 'origin', `https://github.com/${REPO}.git`);
  for (const f of ['clinic_tasks.py', 'README.md']) await writeFile(join(repo, f), await readFile(join(root, 'examples/clinic-tasks', f), 'utf8'));
  git('add', '.'); git('commit', '-qm', '架空の診療所の連絡タスク（最初の版）');
  const commit = git('rev-parse', 'HEAD');
  const data = join(temp, 'app');
  const files = {}, inventory = [];
  for (const path of git('ls-files').split('\n')) { const text = await readFile(join(repo, path), 'utf8'); files[path] = text; inventory.push({ path, mode: '100644', size: Buffer.byteLength(text) }); }
  await mkdir(join(data, 'snapshots', REPO), { recursive: true });
  await writeFile(join(data, 'snapshots', REPO, commit + '.json'), JSON.stringify({ schema: 1, fullName: REPO, reference: commit, commit, inventory, files, omitted: [] }));
  // 試験用の取得先（管理用ディレクトリ）に固定コミットを置く。GitHubからは取得しない。
  await mkdir(join(data, 'repositories', 'example'), { recursive: true });
  execFileSync('git', ['clone', '-q', '--bare', repo, join(data, 'repositories', REPO)]);
  execFileSync('git', ['-C', join(data, 'repositories', REPO), 'remote', 'set-url', 'origin', `https://github.com/${REPO}.git`]);
  record.commit = commit;
  stage('架空の診療所アプリのリポジトリを手元に作った', { commit });

  // 2. 実際の mugiurushi-ai と、mugiurushi の画面版（local方式で実行）を起動する。
  const token = randomBytes(24).toString('hex'), tokenFile = join(temp, 'agent-token');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const port = await new Promise(ok => { const s = createServer().listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => ok(p)); }); });
  const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(port)], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(agent);
  const agentUrl = await started(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
  record.agent_commit = execFileSync('git', ['-C', agentDir, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  const app = spawn(binary, ['app', data, '--port', '0', '--execution-mode', 'local'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi');
  stage('mugiurushi-ai と mugiurushi を起動した', { agent_commit: record.agent_commit });

  // DEMO_VIDEO_DIR を指定すると、画面の操作を編集なしで録画する（DEMO_SLOWMO で操作の間隔をミリ秒で空ける）。
  const video = process.env.DEMO_VIDEO_DIR;
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined, slowMo: Number(process.env.DEMO_SLOWMO || 0) });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: video ? 1 : 2, ...(video ? { recordVideo: { dir: video, size: { width: 1440, height: 1000 } } } : {}) });
  const page = await context.newPage(); page.setDefaultTimeout(30000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  const sent = [];
  page.on('response', async r => { if (r.url().includes('/api/v1/agent/')) sent.push({ path: new URL(r.url()).pathname, status: r.status(), body: await r.json().catch(() => null) }); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '架空の開発者', role: 'developer' })));
  const poster = async (name, locator) => { if (locator) await locator.scrollIntoViewIfNeeded(); else await page.evaluate(() => window.scrollTo(0, 0)); await page.screenshot({ path: join(output, `poster-${name}.png`) }); };
  const shot = async (name, locator) => (locator || page).screenshot({ path: join(output, `clinic-demo-${name}.png`), ...(locator ? {} : { fullPage: true }) });
  const send = async () => {
    const preview = page.locator('[data-preview]'); await preview.waitFor();
    const before = sent.length;
    await preview.locator('#agree').check();
    await preview.getByRole('button', { name: 'この内容を送る' }).click();
    for (let i = 0; i < 330 && sent.length === before; i++) await new Promise(r => setTimeout(r, 1000));
    const res = sent.at(-1); assert.equal(res.status, 200, JSON.stringify(res.body)); return res.body;
  };
  const flowUrl = new URL(url + '/flow'); flowUrl.search = new URLSearchParams({ url: 'https://github.com/' + REPO, ref: commit }).toString();

  // 3. ガイドラインとの照合（1・2）。
  await page.goto(flowUrl.href);
  await page.locator('[data-files]').waitFor();
  await page.locator('#desc').fill('架空の診療所で、職員が患者ごとの連絡タスク（架空の患者ID・内容）を追加し、一覧で読み直す。職員はそれぞれ担当する患者のタスクだけを扱う。');
  await page.getByRole('button', { name: /照合を始める/ }).click();
  const scan = await send();
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  record.scan = { findings: scan.findings.length, testable: scan.findings.filter(f => f.verification.executable_check).map(f => ({ requirement: f.requirement, priority: f.priority, observation: f.verification.method, source: f.citations[0]?.document })), questions: scan.questions.length, model_call: scan.model_call };
  await shot('1-scan');
  await poster('1-guideline-scan');
  stage('照合：実際のAIから候補を受け取った', { findings: scan.findings.length, testable: record.scan.testable.length, questions: scan.questions.length });
  assert(record.scan.testable.length > 0, '試験で確かめられる候補が無い');
  const q = page.locator('[data-question]').first();
  if (await q.count()) await q.locator('input[type=radio]').first().check();

  // 4. 試験の案と了承（3）。
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  const plan = await send();
  await page.locator('[data-test]').first().waitFor();
  record.plan = { tests: plan.tests.map(t => ({ name: t.name, observation: t.observation, steps: t.steps, pass: t.pass, findings: t.finding_ids })), model_call: plan.model_call };
  await shot('2-plan');
  stage('試験の案：実際のAIから受け取った', { tests: plan.tests.length });
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  await page.locator('[data-result]').first().waitFor({ timeout: 60000 });
  stage('了承して確認項目に登録した');

  // 5. 試験の設定と実行。結果の画面の「試験を設定して実行する」から設定画面を開き、人が入力する内容を入力する。
  const findingText = Object.fromEntries(scan.findings.map((f, i) => [`f-${i + 1}`, f.requirement]));
  const kindOf = text => /ファイル名|パス|トラバーサル|ディレクトリ|保存先の外/.test(text) ? 'path' : /記録|ログ/.test(text) ? 'log' : /閲覧|アクセス(権|制御|管理)|アクセスを(原則)?(制御|制限|拒否)|原則拒否|所有(権|者)|範囲|担当|認可|権限/.test(text) ? 'scope' : null;
  const configs = {
    access: { entry: 'clinic_tasks.py', args: ['--dir', '.', '--user', 'nurse1', 'add', 'P001', '{{test_value}}'], storage: 'access.log',
      readback: ['--dir', '.', '--user', 'nurse2', 'list'],
      checks: [{ scope: '受付', method: 'exit_code', expect: 'zero' }, { scope: '担当外の職員の一覧に出ない', method: 'readback_output', expect: 'not_contains', text: '{{test_value}}' }, { scope: '操作の記録が残る', method: 'stored_file', expect: 'contains', text: 'user=nurse1 action=add' }],
      link: { scope: 'c2', log: 'c3' } },
    path: { entry: 'clinic_tasks.py', args: ['--dir', '.', '--list', '../outside.tsv', '--user', 'nurse1', 'add', 'P001', '{{test_value}}'], storage: '', readback: null,
      checks: [{ scope: '保存先の外の指定を拒否する', method: 'exit_code', expect: 'nonzero' }, { scope: '保存先の外へ書き込まない', method: 'stdout', expect: 'not_contains', text: '追加しました' }],
      link: { path: 'c1' } },
  };
  const tests = plan.tests;
  for (const [n, test] of tests.entries()) {
    const kinds = test.finding_ids.map(id => kindOf(findingText[id] || ''));
    const cfg = kinds.includes('path') ? configs.path : configs.access;
    // 結果の画面のこの試験の行にある「試験を設定して実行する」のリンクから開く。
    await page.goto(flowUrl.href); await page.locator('[data-result]').first().waitFor();
    const checkId = (await view(page, url)).flowChecks[n];
    const anchor = page.locator(`[data-result] a[href*="check=${encodeURIComponent(checkId)}"]`).first();
    assert(await anchor.count(), `結果の画面に ${checkId} の設定へのリンクが無い`);
    await anchor.click();
    const plan_panel = page.locator('[data-flow-plan]'); await plan_panel.waitFor();
    if (n === 0) { await shot('3-settings-plan'); await page.evaluate(() => document.querySelector('[data-flow-plan]').scrollIntoView({ block: 'start' })); await page.screenshot({ path: join(output, 'poster-2a-trial-settings.png') }); }
    const body = page.locator('#trial-body');
    // 起動のしかた（試験の内容）：起動対象・引数・保存先・後から読み出す。
    await body.getByLabel(/^起動対象/).first().fill(cfg.entry);
    await body.getByRole('textbox', { name: '引数', exact: true }).fill(cfg.args.join('\n'));
    await body.getByRole('textbox', { name: '保存先', exact: true }).fill(cfg.storage);
    const readback = body.locator('summary', { hasText: '後から読み出す' });
    if (await readback.count()) await readback.click();
    await body.getByLabel(/^読み出し用の起動対象/).first().fill(cfg.readback ? cfg.entry : '');
    await body.getByRole('textbox', { name: '読み出しの引数', exact: true }).fill(cfg.readback ? cfg.readback.join('\n') : '');
    await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
    await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
    // 判定の条件：試験の設定として人が決めた条件を入れる。
    while (await body.getByRole('button', { name: 'この条件を外す' }).count()) await body.getByRole('button', { name: 'この条件を外す' }).first().click();
    for (const c of cfg.checks) {
      await body.getByRole('button', { name: '条件を追加' }).click();
      const row = body.locator('.check-row').last();
      await row.getByLabel(/^確認する範囲/).fill(c.scope);
      await row.getByLabel(/^観測の方法/).selectOption(c.method);
      await row.getByLabel(/^期待する結果/).selectOption(c.expect);
      if (c.text) await row.getByLabel(/^文字列/).fill(c.text);
    }
    if (n === 0) await shot('4-settings-filled');
    await page.getByRole('button', { name: '実行前に確認する', exact: true }).click();
    const editor = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' }); await editor.waitFor();
    const linked = [];
    for (const [i, id] of test.finding_ids.entries()) {
      const target = cfg.link[kinds[i]];
      if (!target) continue;
      const row = editor.locator(`[data-aspect="${id}"]`);
      const method = cfg.checks[Number(target.slice(1)) - 1].method;
      await row.locator('select').selectOption(method);
      await row.locator(`input[type=checkbox][value="${target}"]`).check();
      linked.push({ finding: id, check: target, method });
    }
    if (linked.length) {
      const before = (await view(page, url)).revision;
      await editor.getByRole('button', { name: '対応づけを保存する' }).click();
      for (let i = 0; i < 60 && (await view(page, url)).revision === before; i++) await new Promise(r => setTimeout(r, 250));
    }
    if (n === 0) await shot('5-before-run');
    await page.getByRole('button', { name: '試験を始める', exact: true }).click();
    await page.locator('#result:not([hidden])').waitFor({ timeout: 300000 });
    const v = await view(page, url);
    const cv = v.view.checks.find(c => c.check_id === v.flowChecks[n]);
    record.trials.push({ test: test.name, config: kinds.includes('path') ? 'path' : 'access', linked, executed: cv.evidence.some(e => e.applicable), verdict: cv.verdict, aspects: cv.coverage.aspects.map(a => ({ id: a.id, status: a.status })) });
    if (n === 0) { await shot('6-trial-result'); await page.evaluate(() => document.querySelector('[data-coverage]')?.scrollIntoView({ block: 'start' })); await page.screenshot({ path: join(output, 'poster-2b-trial-run-result.png') }); }
    stage(`試験を実行した：${test.name}`, record.trials.at(-1));
  }

  // 6. 照合の流れに戻り、結果と修正用プロンプトを確かめる。
  await page.goto(flowUrl.href);
  await page.locator('[data-result]').first().waitFor();
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('[data-result]').first().waitFor();
  const states = await page.locator('[data-result]').evaluateAll(rows => rows.map(r => r.getAttribute('data-result')));
  record.result_states = states;
  await shot('7-result');
  await poster('2c-results-by-guideline');
  stage('結果の画面を表示した', { states });
  if (await page.getByRole('button', { name: '修正用のプロンプトを作る' }).count()) {
    await page.getByRole('button', { name: '修正用のプロンプトを作る' }).click();
    record.prompt = await page.locator('[data-prompt]').innerText();
    await shot('8-prompt');
    await poster('3-fix-prompt');
    stage('修正用のプロンプトを表示した', { chars: record.prompt.length });
  } else stage('試験で不足を確認した項目が無く、修正用のプロンプトは作らなかった');
  assert.deepEqual(errors, []);
  if (video) await page.waitForTimeout(5000);
  record.completed = true;
} catch (error) {
  record.completed = false; record.error = String(error.message || error).slice(0, 4000);
  throw error;
} finally {
  await writeFile(join(output, 'clinic-demo.json'), JSON.stringify(record, null, 2));
  await browser?.close();
  for (const child of children) child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
  console.log(JSON.stringify({ completed: record.completed, stages: record.stages.map(s => s.name), output: relative(root, join(output, 'clinic-demo.json')) }));
}

async function view(page, url) {
  const token = await page.locator('meta[name=review-token]').getAttribute('content');
  const v = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': token } })).json();
  const flow = await (await page.request.get(url + '/api/v1/flow', { headers: { 'X-Review-Token': token } })).json();
  const rec = Object.values(flow.records)[0]?.record || {};
  return { view: v, revision: v.workspace.revision, flowChecks: (rec.plan?.tests || []).map(t => rec.approved?.[t.id]) };
}
