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
  const size = video ? { width: 1024, height: 768 } : { width: 1440, height: 1000 };
  const context = await browser.newContext({ viewport: size, deviceScaleFactor: video ? 1 : 2, ...(video ? { recordVideo: { dir: video, size } } : {}) });
  const page = await context.newPage(); page.setDefaultTimeout(30000);
  // 録画では、画面の下に段階の見出し・操作の目的・待機中の状態を出す。画面の操作はスクリプトが行うことも常に示す。
  if (video) await page.addInitScript(() => {
    const draw = () => {
      let c; try { c = JSON.parse(sessionStorage.getItem('demo-caption') || 'null'); } catch { return; }
      if (!c || !document.body) return;
      let el = document.getElementById('demo-caption');
      if (!el) { el = document.createElement('div'); el.id = 'demo-caption'; document.body.appendChild(el); }
      el.style.cssText = 'position:fixed;left:0;right:0;bottom:0;z-index:99999;background:#00118F;color:#fff;padding:12px 24px 14px;font:16px/1.55 "Noto Sans JP",sans-serif;box-shadow:0 -2px 8px rgba(0,0,0,.25);pointer-events:none';
      document.body.style.paddingBottom = '140px';
      el.replaceChildren();
      const top = document.createElement('div'); top.style.cssText = 'display:flex;gap:12px;align-items:baseline';
      const step = document.createElement('span'); step.textContent = c.step; step.style.cssText = 'background:#fff;color:#00118F;font-weight:700;padding:1px 10px;border-radius:4px;white-space:nowrap';
      const title = document.createElement('span'); title.textContent = c.title; title.style.cssText = 'font-weight:700;font-size:19px';
      const badge = document.createElement('span'); badge.textContent = '自動操作（スクリプト）・無編集の録画'; badge.style.cssText = 'margin-left:auto;font-size:12px;opacity:.85;white-space:nowrap';
      top.append(step, title, badge);
      const text = document.createElement('div'); text.textContent = c.text; text.style.cssText = 'margin-top:4px';
      el.append(top, text);
    };
    window.__drawCaption = draw;
    document.addEventListener('DOMContentLoaded', draw);
    // 録画の幅では2列のままだと片側だけが残って大半が白くなるため、照合の画面の2列（.cols）を1列に並べる。表示する内容は変えない。
    const stack = () => document.querySelectorAll('.cols').forEach(e => { if (e.style.display !== 'block') e.style.display = 'block'; });
    new MutationObserver(stack).observe(document, { childList: true, subtree: true });
  });
  const say = async (step, title, text, ms = 0) => {
    if (!video) return;
    await page.evaluate(c => { try { sessionStorage.setItem('demo-caption', JSON.stringify(c)); } catch { /* 表紙では保存しない */ } window.__drawCaption?.(); }, { step, title, text });
    if (ms) await page.waitForTimeout(ms);
  };
  const focus = async (locator, ms = 0) => {
    if (!video || !(await locator.count())) return;
    await locator.first().evaluate(el => { el.scrollIntoView({ block: 'center' }); el.style.outline = '4px solid #0017C1'; el.style.outlineOffset = '3px'; });
    if (ms) await page.waitForTimeout(ms);
  };
  const card = async (heading, lines, ms) => {
    if (!video) return;
    await page.goto('about:blank');
    const esc = t => t.replace(/[&<>]/g, ch => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[ch]);
    await page.setContent(`<!doctype html><meta charset="utf-8"><body style="margin:0;background:#00118F;color:#fff;font-family:'Noto Sans JP',sans-serif;display:flex;align-items:center;justify-content:center;height:100vh"><div style="max-width:1040px;padding:40px"><div style="font-size:15px;opacity:.85">mugiurushi（麦漆）デモ ／ 自動操作（スクリプト）・無編集の録画 ／ AIと試験の実行は実際のもの</div><h1 style="font-size:34px;margin:14px 0 22px">${esc(heading)}</h1>${lines.map(l => `<p style="font-size:21px;line-height:1.6;margin:8px 0">${esc(l)}</p>`).join('')}</div></body>`);
    await page.waitForTimeout(ms);
  };
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  const sent = [];
  page.on('response', async r => { if (r.url().includes('/api/v1/agent/')) sent.push({ path: new URL(r.url()).pathname, status: r.status(), body: await r.json().catch(() => null) }); });
  await page.addInitScript(() => { try { sessionStorage.setItem('eel-person', JSON.stringify({ name: '架空の開発者', role: 'developer' })); } catch { /* 表紙と締めの画面には保存しない */ } });
  const poster = async (name, locator) => { if (locator) await locator.scrollIntoViewIfNeeded(); else await page.evaluate(() => window.scrollTo(0, 0)); await page.screenshot({ path: join(output, `poster-${name}.png`) }); };
  const shot = async (name, locator) => (locator || page).screenshot({ path: join(output, `clinic-demo-${name}.png`), ...(locator ? {} : { fullPage: true }) });
  const send = async waiting => {
    const preview = page.locator('[data-preview]'); await preview.waitFor();
    const before = sent.length;
    await focus(preview);
    await preview.locator('#agree').check();
    await preview.getByRole('button', { name: 'この内容を送る' }).click();
    if (waiting) await say(...waiting);
    for (let i = 0; i < 330 && sent.length === before; i++) await new Promise(r => setTimeout(r, 1000));
    const res = sent.at(-1); assert.equal(res.status, 200, JSON.stringify(res.body)); return res.body;
  };
  const flowUrl = new URL(url + '/flow'); flowUrl.search = new URLSearchParams({ url: 'https://github.com/' + REPO, ref: commit }).toString();

  // 3. ガイドラインとの照合（1・2）。
  await card('担当外の職員に、患者のタスクが見えてしまわないか', [
    '対象：架空の診療所アプリ。職員が、担当する患者ごとの連絡タスクを追加し、一覧で読み直す。',
    'この録画の流れ：①対象と困り事 → ②出典つきの候補 → ③試験の案の了承と設定 → ④実行して不足を確かめる → ⑤修正の依頼',
    '修正と、修正後の再試験は、この録画では行っていない。'], 9000);
  await page.goto(flowUrl.href);
  await page.locator('[data-files]').waitFor();
  await say('①', '対象と困り事', 'AIに読ませるコード（clinic_tasks.py）を選び、システムの説明を書く。架空のコードと説明だけを送る。');
  await focus(page.locator('[data-files]'), 4000);
  await page.locator('#desc').fill('架空の診療所で、職員が患者ごとの連絡タスク（架空の患者ID・内容）を追加し、一覧で読み直す。職員はそれぞれ担当する患者のタスクだけを扱う。');
  await focus(page.locator('#desc'), 4000);
  await page.getByRole('button', { name: /照合を始める/ }).click();
  await say('①', '送る内容を確かめてから送る', '送る前に、AIへ渡す内容と量を表示する。確認の印を付けてから送る。', 4000);
  const scan = await send(['①', 'AIの応答を待っています', '取り込んだ公開文書（ガイドライン・セキュリティ基準など）とコードを照らしています。応答まで20〜40秒ほどかかります。']);
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  if (video) {
    // 録画用の配置を確かめる：照合の結果とヒアリングの欄の位置と大きさ、ヒアリングの画面の静止画。
    record.layout = await page.evaluate(() => ['.cols>.main', '.cols>.side'].map(q => { const r = document.querySelector(q)?.getBoundingClientRect(); return r && { q, x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) }; }));
    await page.getByRole('heading', { name: 'ヒアリング' }).evaluate(el => el.scrollIntoView({ block: 'start' }));
    await page.screenshot({ path: join(output, 'video-check-hearing.png') });
    await page.evaluate(() => window.scrollTo(0, 0));
  }
  record.scan = { findings: scan.findings.length, testable: scan.findings.filter(f => f.verification.executable_check).map(f => ({ requirement: f.requirement, priority: f.priority, observation: f.verification.method, source: f.citations[0]?.document })), questions: scan.questions.length, model_call: scan.model_call };
  await shot('1-scan');
  await poster('1-guideline-scan');
  stage('照合：実際のAIから候補を受け取った', { findings: scan.findings.length, testable: record.scan.testable.length, questions: scan.questions.length });
  assert(record.scan.testable.length > 0, '試験で確かめられる候補が無い');
  const testableIds = Object.fromEntries(scan.findings.map((f, i) => [`f-${i + 1}`, f]));
  const scopeRe = /閲覧|アクセス(権|制御|管理)|アクセスを(原則)?(制御|制限|拒否)|原則拒否|所有(権|者)|担当|認可|権限/;
  const repId = Object.keys(testableIds).find(id => testableIds[id].verification.executable_check && scopeRe.test(testableIds[id].requirement) && !/ログ|記録/.test(testableIds[id].requirement));
  record.representative = repId ? { id: repId, requirement: testableIds[repId].requirement, source: testableIds[repId].citations[0]?.document } : null;
  if (video) {
    await page.evaluate(() => window.scrollTo(0, 0));
    await say('②', '出典つきの候補', `${record.scan.findings}件の候補のうち、試験で確かめられる${record.scan.testable.length}件を、ガイドラインごとに優先度順に並べる。`, 6000);
    if (repId) {
      await say('②', '代表例を選ぶ', `「${testableIds[repId].requirement}」（出典：${testableIds[repId].citations[0]?.document || '—'}）`);
      await focus(page.locator(`[data-finding="${repId}"]`), 8000);
    }
  }
  const q = page.locator('[data-question]').first();
  if (await q.count()) await q.locator('input[type=radio]').first().check();

  // 4. 試験の案と了承（3）。
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  await say('③', '試験の案を作る', '選んだ候補を、何をして何を観測すれば確かめられるかという試験の案にする。', 3000);
  const plan = await send(['③', 'AIの応答を待っています', '試験の手順と合格の条件を作っています。応答まで20〜40秒ほどかかります。']);
  await page.locator('[data-test]').first().waitFor();
  record.plan = { tests: plan.tests.map(t => ({ name: t.name, observation: t.observation, steps: t.steps, pass: t.pass, findings: t.finding_ids })), model_call: plan.model_call };
  await shot('2-plan');
  stage('試験の案：実際のAIから受け取った', { tests: plan.tests.length });
  const repTest = plan.tests.findIndex(t => repId && t.finding_ids.includes(repId));
  if (video && repTest >= 0) {
    await say('③', '人が試験の案を読んで了承する', '手順と合格の条件を開発者が読み、了承するまで試験は実行しない。');
    await focus(page.locator(`[data-test="${plan.tests[repTest].id}"]`), 9000);
  }
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
    // 録画では代表例の試験だけを実行する。ほかの試験は結果の画面で「未実行」と表示される。
    if (video && repTest >= 0 && n !== repTest) continue;
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
    await say('③', '試験の設定（開発者が入力する内容を、スクリプトが入力）', '職員nurse1として患者P001のタスクを追加し、別の職員nurse2として一覧を読み直す。期待は「nurse2の一覧に、そのタスクが出ない」。');
    await focus(page.locator('[data-flow-plan]'), 6000);
    // 起動のしかた（試験の内容）：起動対象・引数・保存先・後から読み出す。
    await body.getByLabel(/^起動対象/).first().fill(cfg.entry);
    await body.getByRole('textbox', { name: '引数', exact: true }).fill(cfg.args.join('\n'));
    await body.getByRole('textbox', { name: '保存先', exact: true }).fill(cfg.storage);
    const readback = body.locator('summary', { hasText: '後から読み出す' });
    if (await readback.count()) await readback.click();
    await body.getByLabel(/^読み出し用の起動対象/).first().fill(cfg.readback ? cfg.entry : '');
    await body.getByRole('textbox', { name: '読み出しの引数', exact: true }).fill(cfg.readback ? cfg.readback.join('\n') : '');
    await focus(body.getByRole('textbox', { name: '引数', exact: true }), 5000);
    await focus(body.getByRole('textbox', { name: '読み出しの引数', exact: true }), 5000);
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
    if (video) {
      await say('③', '判定の条件', '後からの読み出し（nurse2の一覧）に、追加したタスクの文字列を含まないこと。含めば「期待と異なる」と判定する。');
      await focus(body.locator('.check-row').nth(1), 7000);
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
    if (video) {
      await say('③', 'どの候補を、どの判定の条件で確かめるかを選ぶ', '選ばなかった候補は「未確認」として残り、合否には数えない。');
      await focus(editor, 6000);
    }
    if (n === 0) await shot('5-before-run');
    await page.getByRole('button', { name: '試験を始める', exact: true }).click();
    await say('④', '試験を実行しています', '架空のアプリを手元で実際に起動し、出力・保存先・後からの読み出しを観測しています。');
    await page.locator('#result:not([hidden])').waitFor({ timeout: 300000 });
    const v = await view(page, url);
    const cv = v.view.checks.find(c => c.check_id === v.flowChecks[n]);
    record.trials.push({ test: test.name, config: kinds.includes('path') ? 'path' : 'access', linked, executed: cv.evidence.some(e => e.applicable), verdict: cv.verdict, aspects: cv.coverage.aspects.map(a => ({ id: a.id, status: a.status })) });
    if (n === 0) { await shot('6-trial-result'); await page.evaluate(() => document.querySelector('[data-coverage]')?.scrollIntoView({ block: 'start' })); await page.screenshot({ path: join(output, 'poster-2b-trial-run-result.png') }); }
    if (video) {
      await say('④', '実際の結果と証拠', '判定の条件ごとに、観測した内容と期待との違いを示す。');
      await focus(page.locator('#result'), 5000);
      await focus(page.locator('[data-coverage]'), 9000);
    }
    stage(`試験を実行した：${test.name}`, record.trials.at(-1));
  }

  // 6. 照合の流れに戻り、結果と修正用プロンプトを確かめる。
  await page.goto(flowUrl.href);
  await page.locator('[data-result]').first().waitFor();
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('[data-result]').first().waitFor();
  const states = await page.locator('[data-result]').evaluateAll(rows => rows.map(r => r.getAttribute('data-result')));
  record.result_states = states;
  if (video) {
    await page.evaluate(() => window.scrollTo(0, 0));
    await say('④', '結果の一覧', '実際に確かめた候補だけが「試験で不足を確認」になる。この録画で実行していない試験の候補は「未実行」のまま。', 7000);
    if (repId) await focus(page.locator('[data-result]').filter({ hasText: testableIds[repId].requirement.slice(0, 20) }), 6000);
  }
  await shot('7-result');
  await poster('2c-results-by-guideline');
  stage('結果の画面を表示した', { states });
  if (await page.getByRole('button', { name: '修正用のプロンプトを作る' }).count()) {
    await page.getByRole('button', { name: '修正用のプロンプトを作る' }).click();
    record.prompt = await page.locator('[data-prompt]').innerText();
    if (video) {
      await say('⑤', '修正の依頼', '出典の抜粋・確かめた判定の条件・結果・関係するコード・対象のコミットを1つにまとめる。手元のAIにそのまま渡せる。');
      await focus(page.locator('[data-prompt]'), 10000);
    }
    await shot('8-prompt');
    await poster('3-fix-prompt');
    stage('修正用のプロンプトを表示した', { chars: record.prompt.length });
  } else stage('試験で不足を確認した項目が無く、修正用のプロンプトは作らなかった');
  assert.deepEqual(errors, []);
  if (video) {
    const tested = record.trials.flatMap(t => t.aspects.filter(a => a.status === 'contradicted')).length;
    await card('この録画で行ったこと', [
      `照合：候補${record.scan.findings}件、そのうち試験で確かめられる候補${record.scan.testable.length}件（AIの応答は実行ごとに変わる）。`,
      `試験：${record.trials.length}件を実行し、確かめた${tested}件で不足を確認。ほかの試験は実行していない（未実行）。`,
      '行っていないこと：コードの修正と、修正後の再試験。',
      '記録：リポジトリの docs/ に、この実行の記録（応答ID・コミット）を残す。'], 10000);
  }
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
