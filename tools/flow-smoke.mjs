// 新しい流れ（/flow）を、偽のAIエージェントの応答で画面から通す合成試験。
// 照合の準備 → 照合とヒアリング（質問への回答・利用者からの質問）→ 試験の案 → 了承して確認項目に登録 →
// 結果（試験で不足を確認した項目）→ 修正用のプロンプト → 前のコミットとの比較。
// 試験の実行そのものは既存の試験で確かめているため、ここでは事例の記録の観測の状態を差し替えて結果の画面を確かめる。
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const temp = await mkdtemp(join(tmpdir(), 'eel-flow-smoke-')), data = join(temp, 'app');
const repository = 'example/clinic', commit = 'c'.repeat(40), previous = 'b'.repeat(40);
await mkdir(join(data, 'snapshots', repository), { recursive: true });
for (const sha of [previous, commit]) await writeFile(join(data, 'snapshots', repository, sha + '.json'), JSON.stringify({
  schema: 1, fullName: repository, reference: sha, commit: sha,
  inventory: [{ path: 'app/auth.py', mode: '100644' }, { path: 'tests/test_auth.py', mode: '100644' }, { path: 'README.md', mode: '100644' }],
  files: { 'app/auth.py': 'def login(user, password):\n    return True\n', 'tests/test_auth.py': 'def test(): pass\n', 'README.md': '架空の診療所' }, omitted: [],
}));
const child = spawn(process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi'), ['app', data, '--port', '0'], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
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
  page.on('console', m => { if (m.type() === 'error' && /Content Security Policy/.test(m.text())) errors.push(m.text()); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '合成の開発者', role: 'developer' })));

  const citation = (source_id, document, page, excerpt) => ({ passage_id: `${source_id}:p${page}:1`, source_id, document_id: source_id + '-main', source: document, publisher: '合成', document, version: null, page, origin: 'file:合成', sha256: 'd'.repeat(64), fetched_at: '2026-11-06T00:00:00+00:00', excerpt });
  const finding = (requirement, cite, priority, method = 'browser_text') => ({ status: 'candidate', requirement, relevance: 'likely', citations: [cite], system_refs: ['app/auth.py'], patient_data: priority === 'high', priority, unknowns: [],
    verification: { method, method_label: method, observation: method, support: method === 'document_review' ? null : 'judged', support_note: '合成', executable_check: method !== 'document_review', how: '合成の確かめ方' } });
  const requests = { scan: [], plan: [], ask: [] };
  const sendOrPreview = (name, body, preview, answer) => {
    requests[name].push(body);
    return body.confirm ? answer : { stage: 'preview', destination: '合成のAIエージェント', path: '/v1/' + name, bytes: 42, payload_sha256: 'e'.repeat(64), note: '合成', payload: preview };
  };
  await page.route('**/api/v1/agent', route => route.fulfill({ json: { status: 'configured', message: '合成' } }));
  await page.route('**/api/v1/agent/flow-scan', route => {
    const body = route.request().postDataJSON();
    return route.fulfill({ json: sendOrPreview('scan', body, { system: { summary: body.summary, excerpts: body.files.map(f => ({ ref: f, text: '…' })) }, answers: body.answers }, {
      stage: 'sent', status: 'candidate', findings: [
        finding('アプリケーションログイン時の二要素認証', citation('provider-security', '合成の提供事業者ガイドライン', '37', '多要素認証を可能な限り早期に採用すべきである。'), 'high'),
        finding('アクセス記録の保存', citation('ppc-medical-guidance', '合成の医療・介護ガイダンス', '42', '個人データに対するアクセス記録の保存'), 'medium', 'stored_file'),
        finding('運用の手順書を整える', citation('mhlw-security', '合成の安全管理ガイドライン', '5', '手順書'), 'low', 'document_review'),
      ], questions: [{ id: 'q1', text: '院外から使いますか', why: '外部接続の項目に使う', options: ['はい', 'いいえ'] }], sources: [], not_covered: [],
      priority_rule: '優先度は決まった規則で付ける（合成）', model_call: { response_id: 'fake-scan', response_model: 'fake' } }) });
  });
  await page.route('**/api/v1/agent/ask', route => {
    const body = route.request().postDataJSON();
    return route.fulfill({ json: sendOrPreview('ask', body, { question: body.question }, { stage: 'sent', answer: '合成の回答：法定保存年限以上保存する。', grounded: true, citations: [citation('provider-security', '合成の提供事業者ガイドライン', '37', 'ログの保存期間')] }) });
  });
  await page.route('**/api/v1/agent/plan', route => {
    const body = route.request().postDataJSON();
    return route.fulfill({ json: sendOrPreview('plan', body, { findings: body.findings }, { stage: 'sent', status: 'draft', not_planned: [], model_call: { response_id: 'fake-plan', response_model: 'fake' }, tests: [
      { id: 't-login', name: 'ログインと一覧の表示', kind: 'browser', finding_ids: ['f-1'], steps: ['職員1でログインする'], pass: ['2つ目の認証を求められる'], observation: 'browser_text', observation_label: 'ブラウザの画面' },
      { id: 't-log', name: '操作の記録', kind: 'browser', finding_ids: ['f-2'], steps: ['タスクを開く', '記録を読み出す'], pass: ['職員・日時・対象が記録に残る'], observation: 'stored_file', observation_label: '保存先' },
    ] }) });
  });
  // 事例の記録の観測の状態を差し替える（f-1 は不足を確認、f-2 は期待どおり）。
  let override = false;
  await page.route('**/api/v1/case', async route => {
    const response = await route.fetch(); const json = await response.json();
    if (override) for (const cv of json.checks) for (const a of cv.coverage?.aspects || []) {
      if (a.id === 'f-1') Object.assign(a, { status: 'contradicted', status_label: '期待と異なる', detail: 'IDとパスワードだけで一覧が表示された' });
      if (a.id === 'f-2') Object.assign(a, { status: 'observed', status_label: '観測した', detail: '記録に残った' });
      cv.evidence = [{ id: 'ev-1', run_id: 'run-1', applicable: true, invalidated_by: [], observed: {} }]; cv.selected_evidence = 'ev-1';
    }
    return route.fulfill({ response, json });
  });
  const token = async () => page.locator('meta[name=review-token]').getAttribute('content');
  const flowUrl = sha => { const u = new URL(url + '/flow'); u.search = new URLSearchParams({ url: 'https://github.com/' + repository, ref: sha }).toString(); return u.href; };

  // 前のコミットの結果の控えを置いておく（6 再試験の比較に使う）。
  await page.goto(flowUrl(commit));
  const saved = await page.request.post(url + '/api/v1/flow', { headers: { 'X-Review-Token': await token(), 'Content-Type': 'application/json' },
    data: { repository, commit: previous, record: { results: { 'f-1': { requirement: 'アプリケーションログイン時の二要素認証', state: 'ng' }, 'f-2': { requirement: 'アクセス記録の保存', state: 'ng' } } } } });
  assert.equal(saved.status(), 200, await saved.text());
  await page.reload();

  // 1 照合の準備：コードのファイルを選び、テストのファイルは既定で外す。
  const files = page.locator('[data-files]'); await files.waitFor();
  assert.deepEqual(await files.locator('tbody tr').evaluateAll(rows => rows.filter(r => r.querySelector('input').checked).map(r => r.children[1].textContent)), ['app/auth.py']);
  await page.locator('#desc').fill('合成の診療所のタスク管理');
  await page.getByRole('button', { name: /照合を始める/ }).click();
  await page.locator('[data-preview]').waitFor();
  assert.equal(requests.scan.length, 1); assert.equal(requests.scan[0].confirm, undefined, '確認の段階では送らない');
  assert.deepEqual(requests.scan[0].files, ['app/auth.py']);
  assert.equal(await page.getByRole('button', { name: 'この内容を送る' }).isDisabled(), true);
  await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();

  // 2 照合とヒアリング：試験で確かめられる項目だけをガイドライン別に出す。
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  assert.deepEqual(requests.scan[1].confirm, { payload_sha256: 'e'.repeat(64), synthetic_only: true });
  assert.deepEqual(await page.locator('[data-section]').evaluateAll(s => s.map(x => x.getAttribute('data-section'))), ['g3', 'pi']);
  assert.doesNotMatch(await page.locator('main').innerText(), /運用の手順書/, '試験で確かめられない項目は出さない');
  await page.locator('[data-question="q1"] input[type=radio]').first().check();
  await page.locator('#ask').fill('ログの保存期間は？');
  await page.getByRole('button', { name: 'ガイドラインに照らして答えてもらう' }).click();
  await page.locator('[data-preview]').waitFor(); await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();
  await page.locator('[data-asked]').waitFor();
  assert.match(await page.locator('[data-asked]').innerText(), /法定保存年限[\s\S]*合成の提供事業者ガイドライン 37ページ/);
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  await page.locator('[data-preview]').waitFor();
  assert.deepEqual(requests.plan[0].answers, [{ question: '院外から使いますか', answer: 'はい' }], 'ヒアリングの回答を試験の案に使う');
  assert.deepEqual(requests.plan[0].findings.map(f => f.id), ['f-1', 'f-2']);
  await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();

  // 3 検証案の了承：了承した試験ごとに確認項目を作り、照合の候補を確かめる部分として登録する。
  await page.locator('[data-test="t-login"]').waitFor();
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  await page.locator('[data-result]').first().waitFor();
  const view = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await token() } })).json();
  const checks = view.workspace.checks.filter(c => c.id.startsWith('flow-'));
  assert.deepEqual(checks.map(c => c.aspects.map(a => [a.id, a.observation, a.trial_checks.length])), [[['f-1', 'browser_text', 0]], [['f-2', 'stored_file', 0]]]);
  assert.match(checks[0].origin, /試験の案（fake・応答ID fake-plan）を合成の開発者が了承/);

  // 4 結果：観測の状態から、不足を確認した項目と期待どおりの項目を分ける。
  override = true;
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('[data-ng-count="1"]').waitFor();
  assert.match(await page.locator('[data-result="ng"]').innerText(), /二要素認証[\s\S]*試験で不足を確認/);
  assert.match(await page.locator('main').innerText(), /試験で期待どおりだった項目：アクセス記録の保存/);
  assert.match(await page.locator('[data-detail]').innerText(), /多要素認証を可能な限り早期に採用すべきである[\s\S]*IDとパスワードだけで一覧が表示された/);

  // 5 修正用のプロンプト：根拠・期待・実際・記録・コード・コミットを入れる。
  await page.getByRole('button', { name: '修正用のプロンプトを作る' }).click();
  const prompt = await page.locator('[data-prompt]').innerText();
  for (const part of ['対象: example/clinic のコミット ccccccc', '多要素認証を可能な限り早期に採用すべきである', '- 期待: 2つ目の認証を求められる', '- 実際: IDとパスワードだけで一覧が表示された', '試行 run-1', 'app/auth.py', '修正しただけでは満たしたとは扱いません'])
    assert(prompt.includes(part), part);
  assert.doesNotMatch(prompt, /アクセス記録の保存/, '期待どおりだった項目は入れない');

  // 6 再試験：前のコミットの控えと比べる。
  await page.getByRole('button', { name: '修正をコミットしたら再試験へ' }).click();
  const compare = page.locator('[data-compare-table]'); await compare.waitFor();
  assert.match(await compare.innerText(), /アクセス記録の保存\s+不足を確認\s+期待どおり\s+解消/);
  assert.match(await compare.innerText(), /二要素認証\s+不足を確認\s+不足を確認\s+変わらず不足/);
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), '390px幅で横にはみ出さない');
  assert.deepEqual(errors, []);
  console.log('flow smoke: ok');
} finally {
  await browser?.close();
  child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
}
