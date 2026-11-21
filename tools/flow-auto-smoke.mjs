// 1画面1操作の流れ（/flow）を、架空の問診アプリ（examples/monshin）で画面から通す合成試験。
// AIエージェントの応答は偽物に差し替えるが、ブランチの一覧・読み込み・動かせるかの判定・試験の実行（local方式）・判定は本物。
// リポジトリの登録 → 読み込み → ファイルを外す → 説明 → 送る内容の確認 → 照合 → ヒアリング → 試験の案 → 了承して実行 →
// 結果（受付の職員にも回答が見える版なので、閲覧の試験は「満たさない」、保存先の試験は「満たす」）→ 修正用のプロンプト →
// 修正版のブランチで、了承した試験を引き継いで実行し直し、閲覧の試験が「満たす」に変わることを確かめる。
// そのあと、同じブランチで照合からやり直す操作も残っていることを確かめる。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const REPO = 'example/monshin';
const temp = await mkdtemp(join(tmpdir(), 'eel-flow-auto-')), data = join(temp, 'app'), output = join(root, 'test-results');
await mkdir(output, { recursive: true });

// 架空のリポジトリ（main と fix/answer-access）を作り、読取記録と試験用の取得先を置く（tools/monshin-demo.mjs と同じ形）。
const repo = join(temp, 'monshin'); await mkdir(repo, { recursive: true });
const git = (...a) => execFileSync('git', ['-C', repo, ...a], { encoding: 'utf8' }).trim();
git('init', '-q', '-b', 'main'); git('config', 'user.name', '架空の開発者'); git('config', 'user.email', 'demo@example.invalid');
const source = await readFile(join(root, 'examples/monshin/monshin.py'), 'utf8');
await writeFile(join(repo, 'monshin.py'), source);
await writeFile(join(repo, 'README.md'), await readFile(join(root, 'examples/monshin/README.md'), 'utf8'));
await writeFile(join(repo, 'icon.png'), 'png');
git('add', '.'); git('commit', '-qm', '最初の版');
git('checkout', '-q', '-b', 'fix/answer-access');
await writeFile(join(repo, 'monshin.py'), source.replace('CAN_READ_ANSWERS = {"doctor", "reception"}', 'CAN_READ_ANSWERS = {"doctor"}'));
git('commit', '-qam', '修正版'); git('checkout', '-q', 'main');
await mkdir(join(data, 'snapshots', REPO), { recursive: true });
const commits = {};
for (const branch of ['main', 'fix/answer-access']) {
  const commit = commits[branch] = git('rev-parse', branch);
  const files = {}, inventory = [];
  for (const path of git('ls-tree', '-r', '--name-only', commit).split('\n')) { const text = git('show', `${commit}:${path}`) + '\n'; files[path] = text; inventory.push({ path, mode: '100644', size: Buffer.byteLength(text) }); }
  await writeFile(join(data, 'snapshots', REPO, commit + '.json'), JSON.stringify({ schema: 1, fullName: REPO, reference: commit, commit, inventory, files, omitted: [] }));
}
await mkdir(join(data, 'repositories', 'example'), { recursive: true });
execFileSync('git', ['clone', '-q', '--bare', repo, join(data, 'repositories', REPO)]);
execFileSync('git', ['-C', join(data, 'repositories', REPO), 'remote', 'set-url', 'origin', `https://github.com/${REPO}.git`]);

const child = spawn(process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi'), ['app', data, '--port', '0', '--execution-mode', 'local'], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
let browser;
try {
  const url = await new Promise((ok, no) => {
    let out = '', err = ''; const timer = setTimeout(() => no(Error(err)), 30000);
    child.stdout.on('data', d => { out += d; const m = /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/.exec(out); if (m) { clearTimeout(timer); ok(m[1]); } });
    child.stderr.on('data', d => { err += d; });
    child.once('exit', () => { clearTimeout(timer); no(Error(err)); });
  });
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined, args: ['--no-sandbox'] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: url });
  const page = await context.newPage(); page.setDefaultTimeout(20000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  page.on('console', m => { if (m.type() === 'error' && /Content Security Policy/.test(m.text())) errors.push(m.text()); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '合成の開発者', role: 'developer' })));
  const shot = name => page.screenshot({ path: join(output, `flow-auto-${name}.png`), fullPage: true });

  // 偽のAIエージェント。送る前の確認（preview）と、確認後の応答を返す。試験の案には、麦漆が検査した形の試験の設定を入れる。
  const requests = { scan: [], plan: [] };
  const preview = (body, payload) => ({ stage: 'preview', destination: '合成のAIエージェント', bytes: 42, payload_sha256: 'e'.repeat(64), note: '合成', payload });
  await page.route('**/api/v1/agent', route => route.fulfill({ json: { status: 'configured', endpoint: 'http://127.0.0.1:8787', model: 'fake-model', model_error: null, message: '合成' } }));
  const citation = (document, page, excerpt) => ({ passage_id: 'mhlw-security:p1:1', source_id: 'mhlw-security', document_id: 'mhlw-security-main', source: document, publisher: '合成', document, version: null, page, origin: 'file:合成', sha256: 'd'.repeat(64), fetched_at: '2026-11-21T00:00:00+00:00', excerpt });
  const finding = (requirement, priority, method) => ({ status: 'candidate', requirement, relevance: 'likely', citations: [citation('合成の安全管理ガイドライン', '24', '利用者の職種に応じて閲覧できる情報を制限する。')], system_refs: ['monshin.py'], patient_data: true, priority, unknowns: [],
    verification: { method, method_label: method, observation: method, support: 'judged', support_note: '合成', executable_check: true, how: '合成' } });
  await page.route('**/api/v1/agent/flow-scan', route => {
    const body = route.request().postDataJSON(); requests.scan.push(body);
    if (!body.confirm) return route.fulfill({ json: preview(body, { system: { summary: body.summary, files: body.files } }) });
    return route.fulfill({ json: { stage: 'sent', status: 'candidate', findings: [finding('閲覧の権限がない職員に、問診の回答を表示しない', 'high', 'readback_output'), finding('問診の回答を、その患者本人の記録に保存する', 'medium', 'stored_file')],
      questions: [{ id: 'q1', text: '院外から使いますか', why: '外部接続の項目に使う', options: ['はい', 'いいえ'] }], sources: [], not_covered: [], priority_rule: '優先度は決まった規則で付けています（合成）', model_call: { response_id: 'fake-scan', response_model: 'fake' } } });
  });
  await page.route('**/api/v1/agent/plan', route => {
    const body = route.request().postDataJSON(); requests.plan.push(body);
    if (!body.confirm) return route.fulfill({ json: preview(body, { findings: body.findings, run: body.run }) });
    const args = (...a) => ['--dir', '.', ...a];
    return route.fulfill({ json: { stage: 'sent', status: 'draft', not_planned: [], unclassified: [], model_call: { response_id: 'fake-plan', response_model: 'fake' }, tests: [
      { id: 't-access', name: '権限のない職員で、問診の回答を開く', kind: 'cli', finding_ids: ['f-1'], steps: ['患者P001として問診に答える', '受付の職員として回答を表示する'], pass: ['回答の内容が表示されない'], observation: 'readback_output', observation_label: '読み出しの出力',
        trial: { entrypoint: 'monshin.py', args: args('--user', 'P001', 'answer', 'P001', '{{test_value}}'), storage: null, readback: { entrypoint: 'monshin.py', args: args('--user', 'reception_b', 'show', 'P001') },
          checks: [{ id: 'c1', scope: '回答を保存できる', method: 'exit_code', expect: 'zero', text: null, target: null }, { id: 'c2', scope: '受付の職員に回答が出ない', method: 'readback_output', expect: 'not_contains', text: '{{test_value}}', target: null }],
          links: [{ finding_id: 'f-1', check_ids: ['c2'], observation: 'readback_output' }] }, trial_note: '医師による表示は確かめない' },
      { id: 't-store', name: '回答の保存先を確かめる', kind: 'cli', finding_ids: ['f-2'], steps: ['患者P001として問診に答える', '保存先の記録を読み出す'], pass: ['回答がP001の記録に保存される'], observation: 'stored_file', observation_label: '保存先',
        trial: { entrypoint: 'monshin.py', args: args('--user', 'P001', 'answer', 'P001', '{{test_value}}'), storage: 'answers/P001.tsv', readback: null,
          checks: [{ id: 'c1', scope: 'P001の記録に保存される', method: 'stored_file', expect: 'contains', text: '{{test_value}}', target: null }],
          links: [{ finding_id: 'f-2', check_ids: ['c1'], observation: 'stored_file' }] } },
      { id: 't-manual', name: '画面で確かめる試験', kind: 'browser', finding_ids: [], steps: ['画面を開く'], pass: ['表示されない'], observation: 'browser_text', observation_label: 'ブラウザの画面', trial: null, trial_note: 'コマンドの試験ではないため、試験の設定は使いません' },
    ].filter(t => t.finding_ids.length) } });
  });

  // 1. リポジトリを登録する。URLを入れるとブランチの一覧が出る（GitHubから読めないので、試験用に取得したリポジトリから）。
  await page.goto(url + '/flow');
  await page.locator('[data-screen=repo]').waitFor();
  await page.locator('#repo').fill(`https://github.com/${REPO}`);
  await page.locator('#branch:not([disabled])').waitFor();
  const options = await page.locator('#branch option').allInnerTexts();
  assert.deepEqual(options, ['main（既定のブランチ）', 'fix/answer-access']);
  await shot('1-repo');
  await page.getByRole('button', { name: '読み込む' }).click();
  // 読み込み中の画面の後、ファイルを外す画面へ進む。
  await page.locator('[data-screen=files]').waitFor({ timeout: 30000 });
  assert.equal(await page.locator('[data-branch]').innerText(), 'main');
  assert.equal(await page.locator('.file-row.off').count(), 1, '画像は最初から外す');
  await page.getByLabel('README.mdを外す').check();
  assert.match(await page.locator('[data-files-count]').innerText(), /3件中2件を外します/);
  await shot('2-files');
  await page.getByRole('button', { name: '次へ' }).click();
  await page.locator('#desc').fill('架空の診療所の問診システム。患者がコマンドで答え、医師が読む');
  await page.getByRole('button', { name: '次へ' }).click();
  // 送る内容の確認：確かめ方に、試験環境で動かせる判定を示す。
  await page.locator('[data-runnable=yes]').waitFor();
  assert.match(await page.locator('[data-runnable]').innerText(), /monshin\.py/);
  await shot('3-confirm');
  const start = page.getByRole('button', { name: '照合を始める' });
  assert(await start.isDisabled(), '確かめる前は送れない');
  await page.locator('#agree').check();
  await start.click();
  await page.locator('[data-screen=scan]').waitFor({ timeout: 30000 });
  assert.deepEqual(requests.scan.at(-1).files, ['monshin.py']);
  assert.equal(requests.scan.at(-1).confirm.payload_sha256, 'e'.repeat(64));
  assert.equal(await page.locator('[data-finding]').count(), 2);
  await shot('4-scan');
  await page.getByRole('button', { name: '次へ：質問に答える' }).click();
  await page.getByRole('heading', { name: '院外から使いますか' }).waitFor();
  await page.getByLabel('いいえ').check();
  // 送る内容の確認は、ヒアリングの画面の中に出る（画面を増やさない）。確かめた印を付けるまで送れない。回答を変えたら確かめ直す。
  const makePlan = page.getByRole('button', { name: '試験の案を作る' });
  await makePlan.click();
  await page.locator('[data-screen=hearing] [data-plan-send] [data-preview]').waitFor();
  assert.match(await page.locator('[data-plan-send]').innerText(), /回答\s*院外から使いますか：いいえ/);
  assert(await makePlan.isDisabled(), '確かめる前は送れない');
  assert.equal(requests.plan.filter(b => b.confirm).length, 0, '確かめる前は送らない');
  await page.getByLabel('はい').check();
  assert.equal(await page.locator('[data-preview]').count(), 0, '回答を変えたら、確かめ直す');
  await page.getByLabel('いいえ').check();
  await makePlan.click();
  await page.locator('[data-screen=hearing] [data-plan-send] [data-preview]').waitFor();
  assert.equal(requests.plan.at(-1).run, true, '動かせると判定したので、試験の設定も求める');
  await page.locator('#agree').check();
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  // 試験の案：試験の設定（実行のしかた・判定の条件）を詳細に示す。
  await page.locator('[data-test]').first().waitFor({ timeout: 30000 });
  assert.deepEqual(requests.plan.at(-1).answers, [{ question: '院外から使いますか', answer: 'いいえ' }]);
  await page.locator('[data-test=t-access] details summary').click();
  assert.match(await page.locator('[data-test=t-access] [data-trial=auto]').innerText(), /python3 monshin\.py --dir \. --user P001 answer P001 "\{\{test_value\}\}"/);
  // 合否は対応づけた判定条件だけで決まる。合否に使わない条件を、了承の前に示す。候補が1つの試験では、候補の名前は繰り返さない。
  assert.match(await page.locator('[data-test=t-access] [data-trial-check=c1]').innerText(), /記録するだけで、合否には使いません/);
  assert.equal(await page.locator('[data-test=t-access] [data-trial-check=c2] [data-unlinked]').count(), 0);
  assert.equal(await page.locator('[data-test=t-access] [data-trial-link]').count(), 0);
  assert.match(await page.locator('[data-test=t-access] [data-trial-note]').innerText(), /AIの注記：医師による表示は確かめない/);
  assert.equal(await page.locator('[data-test=t-store] [data-trial-note]').count(), 0);
  await shot('5-plan');
  // 固定コミットの取得が一度だけ失敗した場合：登録した試験は残り、もう一度押すと、登録し直さずに実行だけをやり直す。
  let failPrepare = true;
  await page.route('**/api/v1/prepare', route => { if (failPrepare) { failPrepare = false; return route.fulfill({ status: 500, json: { error: '合成の取得の失敗' } }); } return route.continue(); });
  await page.getByRole('button', { name: '了承して実行する' }).click();
  await page.locator('#alert .alert', { hasText: '合成の取得の失敗' }).waitFor();
  await page.locator('[data-screen=plan]').waitFor();
  await page.getByRole('button', { name: '了承して実行する' }).click();
  // 実行中の画面で、試験ごとに完了の印が付く。
  await page.locator('[data-screen=waiting] [data-progress]').first().waitFor();
  await page.locator('[data-screen=result]').waitFor({ timeout: 120000 });
  const registered = (await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json()).workspace.checks.filter(c => c.id.startsWith('flow-'));
  assert.equal(registered.length, 2, 'やり直しで確認項目を二重に作らない');
  const states = await page.locator('[data-result]').evaluateAll(rows => rows.map(r => [r.getAttribute('data-finding'), r.getAttribute('data-result')]));
  assert.deepEqual(states, [['f-1', 'ng'], ['f-2', 'ok']], JSON.stringify(states));
  assert.match(await page.locator('h1').innerText(), /満たさない項目が1件/);
  await page.locator('[data-finding=f-1] details summary').click();
  assert.match(await page.locator('[data-finding=f-1]').innerText(), /受付の職員に回答が出ない/);
  await shot('6-result');
  await page.getByRole('button', { name: '修正用のプロンプトを作る' }).click();
  const prompt = await page.locator('[data-prompt]').innerText();
  assert.match(prompt, /example\/monshin の main ブランチ/);
  assert.match(prompt, /閲覧の権限がない職員に、問診の回答を表示しない/);
  assert.doesNotMatch(prompt, /問診の回答を、その患者本人の記録に保存する/, '満たした項目は入れない');
  await page.getByRole('button', { name: 'コピーする' }).click();
  await page.locator('.toast').waitFor();
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), prompt);
  await shot('7-prompt');

  // 2. 修正版のブランチを読み込み、了承した試験をそのまま実行し直す。照合と試験の案は作り直さない（AIへ送らない）。前回との比較で「解消」になる。
  await page.getByRole('link', { name: '再試験へ' }).click();
  await page.locator('#retest-branch option[value="fix/answer-access"]').waitFor({ state: 'attached' });
  await page.locator('#retest-branch').selectOption('fix/answer-access');
  const sent = [requests.scan.length, requests.plan.length];
  await page.getByRole('button', { name: '了承した試験を、このブランチで実行し直す' }).click();
  await page.locator('[data-screen=plan]').waitFor({ timeout: 30000 });
  assert.equal(await page.locator('[data-branch]').innerText(), 'fix/answer-access');
  assert.match(await page.locator('[data-carried]').innerText(), new RegExp(`前のコミット ${commits.main.slice(0, 7)} のコードから作ったものを引き継いでいます`));
  assert.equal(await page.locator('[data-test]').count(), 2);
  assert.equal(await page.locator('[data-test] a', { hasText: '登録済み' }).count(), 0, '了承は引き継がない');
  await page.getByRole('button', { name: '了承して実行する' }).click();
  await page.locator('[data-screen=result]').waitFor({ timeout: 120000 });
  assert.deepEqual([requests.scan.length, requests.plan.length], sent, '実行し直しでは、照合も試験の案もAIへ送らない');
  const after = await page.locator('[data-result]').evaluateAll(rows => rows.map(r => r.getAttribute('data-result')));
  assert.deepEqual(after, ['ok', 'ok'], JSON.stringify(after));
  await page.locator('.stepper button', { hasText: '修正用のプロンプト' }).count().then(n => assert.equal(n, 1));
  await page.locator('.stepper button', { hasText: '修正用のプロンプト' }).click();
  await page.getByRole('link', { name: '再試験へ' }).click();
  await page.locator('[data-compare-table]').waitFor();
  assert.equal(await page.locator('[data-compare=解消]').count(), 1);
  assert.match(await page.locator('[data-compare=解消]').innerText(), /閲覧の権限がない職員に、問診の回答を表示しない/);
  await shot('8-retest');
  // 3. 照合からやり直す操作も残す。main を読み込み直すと、main の記録（結果の画面）に戻る。
  await page.locator('#retest-branch').selectOption('main');
  await page.getByRole('button', { name: '照合からやり直す' }).click();
  await page.locator('[data-screen=result]').waitFor({ timeout: 30000 });
  assert.equal(await page.locator('[data-branch]').innerText(), 'main');
  assert.equal(await page.locator('[data-carried]').count(), 0);
  assert.deepEqual(errors, []);
  console.log('flow-auto-smoke: ok');
} catch (e) {
  const p = browser?.contexts()[0]?.pages()[0];
  if (p) { await p.screenshot({ path: join(output, 'flow-auto-failure.png'), fullPage: true }); console.error(await p.locator('main').innerText().catch(() => ''), await p.locator('#alert').innerText().catch(() => '')); }
  throw e;
} finally {
  await browser?.close();
  child.kill('SIGTERM');
  if (process.env.KEEP) console.log('残した一時ディレクトリ:', temp); else await rm(temp, { recursive: true, force: true });
}
