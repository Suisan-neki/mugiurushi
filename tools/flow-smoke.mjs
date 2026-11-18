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
    // 懸念だけを候補にした案には、懸念を確かめる試験を返す。
    if (body.findings.every(f => f.source === 'concern')) return route.fulfill({ json: sendOrPreview('plan', body, { findings: body.findings }, { stage: 'sent', status: 'draft', not_planned: [], model_call: { response_id: 'fake-concern-plan', response_model: 'fake' }, tests: [
      { id: 't-concern', name: '2つ目の認証を求めるか', kind: 'browser', finding_ids: ['c-1'], steps: ['架空の職員でログインする'], pass: ['2つ目の認証を求められる'], observation: 'browser_text', observation_label: 'ブラウザの画面' }] }) });
    return route.fulfill({ json: sendOrPreview('plan', body, { findings: body.findings }, { stage: 'sent', status: 'draft', not_planned: [], model_call: { response_id: 'fake-plan', response_model: 'fake' }, tests: [
      { id: 't-login', name: 'ログインと一覧の表示', kind: 'browser', finding_ids: ['f-1'], steps: ['職員1でログインする'], pass: ['2つ目の認証を求められる'], observation: 'browser_text', observation_label: 'ブラウザの画面' },
      { id: 't-log', name: '操作の記録', kind: 'browser', finding_ids: ['f-2'], steps: ['タスクを開く', '記録を読み出す'], pass: ['職員・日時・対象が記録に残る'], observation: 'stored_file', observation_label: '保存先' },
    ] }) });
  });
  // コードをたどる往復（#230）。了承がなければ範囲だけを返し、了承があれば結論を返す。
  const investigations = []; let hold = null;
  await page.route('**/api/v1/agent/investigate', async route => {
    const body = route.request().postDataJSON(); investigations.push(body);
    if (body.confirm && hold) await hold;
    if (!body.confirm) return route.fulfill({ json: { stage: 'preview', destination: '合成のAIエージェント', scope_sha256: 'f'.repeat(64), note: '合成の範囲の説明',
      scope: { repository: body.repository, commit: body.commit, operation: body.operation, summary: body.summary, architecture: null, exclusions: ['.env', ...body.exclusions], limits: { rounds: body.rounds, requests_per_round: 8, material_chars: 400000, material_bytes: 800000 } } } });
    return route.fulfill({ json: { stage: 'done', stopped_by: 'concluded', rounds: 2, requests_dropped: 0, limits: '合成の限界の説明', record: 'inv-1', usage: { calls: 2, input_tokens: 12345, cached_input_tokens: null, cache_write_tokens: 2000, output_tokens: 678 }, scope_sha256: 'f'.repeat(64),
      concerns: [{ id: 'c-1', estimate: 'ログインは利用者IDとパスワードだけで通す', evidence: [{ path: 'app/auth.py', start: 1, end: 2 }], concern: '2つ目の認証がない', basis: 'repository', check: { by: 'run', detail: 'ログインして一覧が出るかを見る', observation: 'browser_text' } }],
      questions: ['院外から使うか'],
      material: [{ kind: 'search', query: 'login', hits: 1, truncated: false }, { kind: 'read', path: 'app/auth.py', start: 1, end: 10, blob: 'a'.repeat(40) }, { kind: 'refused', request: { kind: 'read', path: '.env' }, reason: '送らないパス' }] } });
  });
  // 事例の記録の観測の状態を差し替える（f-1 は不足を確認、f-2 は期待どおり）。
  let override = false, unlinked = false, concernNg = false;
  await page.route('**/api/v1/case', async route => {
    const response = await route.fetch(); const json = await response.json();
    if (override) for (const cv of json.checks) for (const a of cv.coverage?.aspects || []) {
      if (a.id === 'f-1') Object.assign(a, { status: 'contradicted', status_label: '期待と異なる', detail: 'IDとパスワードだけで一覧が表示された', trial_checks: [{ id: 'c2', label: '2つ目の認証を求める画面が出る（ブラウザの表示）' }] });
      if (a.id === 'c-1' && concernNg) Object.assign(a, { status: 'contradicted', status_label: '期待と異なる', detail: 'IDとパスワードだけで一覧が表示された', trial_checks: [{ id: 'c9', label: '2つ目の認証を求める画面が出る（ブラウザの表示）' }] });
      if (a.id === 'f-2') Object.assign(a, unlinked ? { status: 'unlinked', status_label: 'どの判定条件が答えるか未対応づけ', detail: 'この部分に答える判定条件を選んでいない' } : { status: 'observed', status_label: '観測した', detail: '記録に残った' });
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
  // 絞り込み：パスの一部で一覧を絞る。選んだファイルは絞り込みに関係なく出す。
  await page.locator('#file-filter').fill('readme');
  assert.deepEqual(await files.locator('tbody tr').evaluateAll(rows => rows.map(r => [r.children[1].textContent, r.querySelector('input').checked])), [['app/auth.py', true], ['README.md', false]]);
  assert.match(await page.locator('[data-files-shown]').innerText(), /全3件中、選んだ1件と、「readme」を含む1件を表示/);
  await page.locator('#file-filter').fill('');
  await page.locator('#desc').fill('合成の診療所のタスク管理');
  // コードをたどって懸念を出す：対象の操作がなければ送らない。範囲を確かめて了承してから往復を回す。
  await page.getByRole('button', { name: '範囲を確かめる' }).click();
  assert.match(await page.locator('#alert').innerText(), /対象の操作を書いてください/);
  assert.equal(investigations.length, 0);
  await page.locator('#operation').fill('ログインして一覧を開く');
  await page.locator('#rounds').fill('5');
  await page.locator('#exclusions').fill('deploy/**\n\n secrets/* ');
  await page.getByRole('button', { name: '範囲を確かめる' }).click();
  const scopeBox = page.locator('[data-scope]'); await scopeBox.waitFor();
  assert.deepEqual(investigations[0], { repository, commit, operation: 'ログインして一覧を開く', summary: '合成の診療所のタスク管理', exclusions: ['deploy/**', 'secrets/*'], rounds: 5 });
  assert.equal(await scopeBox.locator('[data-exclusions]').innerText(), '.env、deploy/**、secrets/*');
  assert.equal(await scopeBox.locator('[data-scope-sha256]').innerText(), 'f'.repeat(64), '了承する範囲のSHA-256を示す');
  assert.match(await scopeBox.innerText(), /5回（1回に8件まで、資料の合計は400,000文字まで）/);
  assert.equal(await page.getByRole('button', { name: 'この範囲で始める' }).isDisabled(), true, '了承の前は始められない');
  await page.locator('#agree-scope').check(); await page.getByRole('button', { name: 'この範囲で始める' }).click();
  const investigation = page.locator('[data-investigation]'); await investigation.waitFor();
  assert.deepEqual(investigations[1].confirm, { scope_sha256: 'f'.repeat(64) });
  assert.equal(await investigation.getAttribute('data-investigation'), 'concluded');
  assert.match(await investigation.innerText(), /懸念 1件[\s\S]*往復2回、送った資料2件[\s\S]*2つ目の認証がない[\s\S]*このリポジトリのコードと設定[\s\S]*麦漆で動かして確かめる[\s\S]*院外から使うか/);
  assert.equal(await investigation.locator('[data-usage]').innerText(), '使ったトークン：入力 12,345（うちキャッシュから読んだ分 不明、キャッシュに書いた分 2,000）、出力 678。モデルの呼び出し2回');
  assert.equal(await investigation.locator('[data-evidence]').getAttribute('href'), `https://github.com/${repository}/blob/${commit}/app/auth.py#L1-L2`);
  // 結果は事例の記録に残り、開き直しても出る。動かして確かめる懸念があるので、開き直すと試験の案の候補の段（2）から開く。
  await page.reload(); await page.locator('[data-concern-candidate="c-1"]').waitFor();
  await page.getByRole('button', { name: '1 照合の準備' }).click(); await page.locator('[data-investigation]').waitFor();
  assert.equal(await page.locator('#operation').inputValue(), 'ログインして一覧を開く');
  assert.equal(investigations.length, 2, '開き直しで送り直さない');
  await page.locator('#desc').fill('合成の診療所のタスク管理');
  // 画面の構造を記録した試験がない事例では、選択欄を出さず、画面の記録の項目を送らない。
  assert.match(await page.locator('main').innerText(), /画面の構造を記録した試験がありません/);
  assert.equal(await page.locator('#screen-evidence').count(), 0);
  await page.getByRole('button', { name: /照合を始める/ }).click();
  await page.locator('[data-preview]').waitFor();
  assert.equal(requests.scan.length, 1); assert.equal(requests.scan[0].confirm, undefined, '確認の段階では送らない');
  assert.deepEqual(requests.scan[0].files, ['app/auth.py']);
  assert.equal('screen_evidence' in requests.scan[0], false);
  assert.equal(await page.getByRole('button', { name: 'この内容を送る' }).isDisabled(), true);
  await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();

  // 2 照合とヒアリング：試験で確かめられる項目だけをガイドライン別に出す。
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  // 1で出した懸念のうち、動かして確かめるものを、試験の案の候補として並べる。
  await page.locator('[data-concern-candidate="c-1"]').waitFor();
  assert.deepEqual(requests.scan[1].confirm, { payload_sha256: 'e'.repeat(64), synthetic_only: true });
  assert.deepEqual(await page.locator('[data-section]').evaluateAll(s => s.map(x => x.getAttribute('data-section'))), ['g3', 'pi']);
  assert.doesNotMatch(await page.locator('main').innerText(), /運用の手順書/, '試験で確かめられない項目は出さない');
  // 往復の途中でこの段へ戻っても、往復の結果とこの段の操作（回答、試験の案）の両方が記録に残る。
  let release = () => {}; hold = new Promise(ok => { release = ok; });
  await page.getByRole('button', { name: '1 照合の準備' }).click();
  await page.getByRole('button', { name: '範囲を確かめる' }).click();
  await page.locator('[data-scope]').waitFor(); await page.locator('#agree-scope').check();
  await page.getByRole('button', { name: 'この範囲で始める' }).click();
  await page.getByRole('button', { name: /2 照合とヒアリング/ }).click();
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  const investigationSaved = page.waitForResponse(r => r.url().endsWith('/api/v1/flow') && r.request().method() === 'POST');
  release(); await investigationSaved; hold = null;
  await page.locator('[data-question="q1"] input[type=radio]').first().check();
  await page.locator('#ask').fill('ログの保存期間は？');
  await page.getByRole('button', { name: 'ガイドラインに照らして答えてもらう' }).click();
  await page.locator('[data-preview]').waitFor(); await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();
  await page.locator('[data-asked]').waitFor();
  assert.match(await page.locator('[data-asked]').innerText(), /法定保存年限[\s\S]*合成の提供事業者ガイドライン 37ページ/);
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  await page.locator('[data-preview]').waitFor();
  assert.deepEqual(requests.plan[0].answers, [{ question: '院外から使いますか', answer: 'はい' }], 'ヒアリングの回答を試験の案に使う');
  // 照合の候補に、動かして確かめる懸念（1で出したもの）を足して送る。懸念は「起きていないこと」を要求の文にする。
  assert.deepEqual(requests.plan[0].findings.map(f => f.id), ['f-1', 'f-2', 'c-1']);
  assert.deepEqual(requests.plan[0].findings[2], { id: 'c-1', requirement: '次のことが起きていないこと：2つ目の認証がない', how: 'ログインして一覧が出るかを見る（コードからの推定：ログインは利用者IDとパスワードだけで通す。根拠の行：app/auth.py 1〜2行）', source: 'concern' });
  assert.equal('source' in requests.plan[0].findings[0], false, 'ガイドラインの候補には出どころを付けない');
  assert.deepEqual(requests.plan[0].files, ['app/auth.py'], '照合で読んだファイルはそのまま');
  assert.deepEqual(requests.plan[0].evidence, [{ path: 'app/auth.py', start: 1, end: 2 }], '懸念の根拠の行は別に添える');
  await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();

  // 3 検証案の了承：了承した試験ごとに確認項目を作り、照合の候補を確かめる部分として登録する。
  await page.locator('[data-test="t-login"]').waitFor();
  const stored = (await (await page.request.get(url + '/api/v1/flow', { headers: { 'X-Review-Token': await token() } })).json()).records[`${repository}@${commit}`].record;
  assert.equal(stored.investigation?.result?.record, 'inv-1', '往復の結果が記録に残る');
  assert.deepEqual(stored.answers?.q1?.answer, 'はい', '往復の途中で答えた回答が記録に残る');
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  await page.locator('[data-result]').first().waitFor();
  const view = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await token() } })).json();
  const checks = view.workspace.checks.filter(c => c.id.startsWith('flow-'));
  assert.deepEqual(checks.map(c => c.aspects.map(a => [a.id, a.observation, a.trial_checks.length])), [[['f-1', 'browser_text', 0]], [['f-2', 'stored_file', 0]]]);
  assert.match(checks[0].origin, /試験の案（fake・応答ID fake-plan）を合成の開発者が了承/);

  // 4 結果：試験を実行する前は、成功・失敗ではなく「未実行」とだけ示す。
  assert.deepEqual(await page.locator('[data-result]').evaluateAll(r => r.map(x => x.getAttribute('data-result'))), ['pending', 'pending']);
  assert.match(await page.locator('[data-result="pending"] [data-state-label]').first().innerText(), /未実行（判定条件への対応づけ前）/);
  assert.equal(await page.locator('[data-ng-count]').count(), 0, '実行していない試験を不足として数えない');
  assert.match(await page.locator('[data-result] a').first().getAttribute('href'), /\/workbench\?.*check=flow-.*from=flow#trial/);
  // 結果の画面のリンクから設定画面を開くと、了承した案（手順・合格の条件）と、流れへ戻るリンクが出る。
  await page.locator('[data-result] a').first().click();
  const panel = page.locator('[data-flow-plan="t-login"]'); await panel.waitFor();
  assert.match(await panel.innerText(), /ログインと一覧の表示[\s\S]*アプリケーションログイン時の二要素認証[\s\S]*職員1でログインする[\s\S]*2つ目の認証を求められる/);
  assert.equal(await page.locator('#to-flow').innerText(), '照合の流れに戻る');
  await panel.getByRole('button', { name: '合格の条件から判定条件の下書きを作る' }).click();
  await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
  await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
  const rowsAfter = page.locator('#trial-body .check-row');
  assert.equal(await rowsAfter.count(), 1);
  assert.equal(await rowsAfter.first().getByLabel(/^確認する範囲/).inputValue(), '2つ目の認証を求められる');
  assert.equal(await rowsAfter.first().getByLabel(/^観測の方法/).inputValue(), 'browser_text');
  await page.locator('[data-back-to-flow]').click();
  await page.locator('[data-result]').first().waitFor();
  // 実行したが判定条件に対応づけていない部分は「未確認」とし、理由を添える。
  override = true; unlinked = true;
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('[data-result="unconfirmed"]').waitFor();
  assert.match(await page.locator('[data-result="unconfirmed"]').innerText(), /アクセス記録の保存[\s\S]*未確認：どの判定条件が答えるか未対応づけ[\s\S]*判定条件に対応づける/);
  // 観測の状態から、不足を確認した項目と期待どおりの項目を分ける。
  unlinked = false;
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('[data-result="unconfirmed"]').waitFor({ state: 'detached' });
  await page.locator('[data-ng-count="1"]').waitFor();
  assert.match(await page.locator('[data-result="ng"]').innerText(), /二要素認証[\s\S]*試験で不足を確認/);
  assert.match(await page.locator('main').innerText(), /試験で期待どおりだった項目：アクセス記録の保存/);
  assert.match(await page.locator('[data-detail]').innerText(), /多要素認証を可能な限り早期に採用すべきである[\s\S]*IDとパスワードだけで一覧が表示された/);

  // 5 修正用のプロンプト：根拠・期待・実際・記録・コード・コミットを入れる。
  await page.getByRole('button', { name: '修正用のプロンプトを作る' }).click();
  const prompt = await page.locator('[data-prompt]').innerText();
  for (const part of ['対象: example/clinic のコミット ccccccc', '多要素認証を可能な限り早期に採用すべきである', '- 確かめた判定条件: c2 2つ目の認証を求める画面が出る（ブラウザの表示）', '- 結果: IDとパスワードだけで一覧が表示された', '- 試験の案の合格の条件: 2つ目の認証を求められる', '試行 run-1', 'app/auth.py', '修正しただけでは満たしたとは扱いません'])
    assert(prompt.includes(part), part);
  assert.doesNotMatch(prompt, /アクセス記録の保存/, '期待どおりだった項目は入れない');

  // 6 再試験：前のコミットの控えと比べる。
  await page.getByRole('button', { name: '修正をコミットしたら再試験へ' }).click();
  const compare = page.locator('[data-compare-table]'); await compare.waitFor();
  assert.match(await compare.innerText(), /アクセス記録の保存\s+不足を確認\s+期待どおり\s+解消/);
  assert.match(await compare.innerText(), /二要素認証\s+不足を確認\s+不足を確認\s+変わらず不足/);
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), '390px幅で横にはみ出さない');

  // 懸念だけから：ガイドラインとの照合をしていない前のコミットで、コードをたどって出た懸念から試験の案を作り、了承する。
  await page.goto(flowUrl(previous));
  await page.locator('#operation').fill('ログインして一覧を開く');
  await page.getByRole('button', { name: '範囲を確かめる' }).click();
  await page.locator('[data-scope]').waitFor(); await page.locator('#agree-scope').check();
  await page.getByRole('button', { name: 'この範囲で始める' }).click();
  await page.locator('[data-investigation]').waitFor();
  await page.getByRole('button', { name: '動かして確かめる懸念から試験の案を作る' }).click();
  await page.locator('[data-no-scan]').waitFor();
  assert.equal(await page.locator('[data-finding]').count(), 0, '照合の候補はない');
  const planned = requests.plan.length;
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  await page.locator('[data-preview]').waitFor();
  const concernPlan = requests.plan[planned];
  assert.deepEqual(concernPlan.findings.map(f => [f.id, f.source]), [['c-1', 'concern']]);
  assert.deepEqual(concernPlan.files, [], '照合をしていなければ、読んだファイルはない');
  assert.deepEqual(concernPlan.evidence, [{ path: 'app/auth.py', start: 1, end: 2 }], '懸念の根拠の行を添える');
  await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click();
  const concernTest = page.locator('[data-test="t-concern"]'); await concernTest.waitFor();
  assert.match(await concernTest.innerText(), /確かめる項目：次のことが起きていないこと：2つ目の認証がない/);
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  // 4 結果：懸念の項目は「コードから出た懸念」の区分に出る。優先度は付けない。
  const concernSection = page.locator('[data-result-section="concern"]'); await concernSection.waitFor();
  assert.match(await concernSection.innerText(), /コードから出た懸念：[\s\S]*次のことが起きていないこと：2つ目の認証がない[\s\S]*2つ目の認証を求めるか/);
  assert.equal(await page.locator('main h2').first().innerText(), 'コードから出た懸念と、このコミットの試験結果');
  const after = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await token() } })).json();
  const concernCheck = after.workspace.checks.find(c => c.aspects?.some(a => a.id === 'c-1'));
  assert(concernCheck, '懸念を確かめる確認項目を登録した');
  assert.match(concernCheck.origin, /コードから出た懸念の試験の案（fake・応答ID fake-concern-plan）を合成の開発者が了承/);
  assert.deepEqual([concernCheck.source.path, concernCheck.source.line, concernCheck.source.end_line, concernCheck.source.commit], ['app/auth.py', 1, 2, previous], '根拠の行を確認項目の参照先にする');
  assert.match(concernCheck.aspects[0].origin, /コードから出た懸念（コードからの推定で、観測でも判定でもない）。根拠の行：app\/auth.py 1〜2行。調べの記録 inv-1/);
  // 試験で懸念が起きていた（期待と異なる）場合は、不足として数え、詳細にガイドラインの代わりに懸念と根拠の行を示す。
  concernNg = true;
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('[data-ng-count="1"]').waitFor();
  await page.locator('[data-result="ng"]').getByRole('button', { name: '詳細' }).click();
  assert.equal(await page.locator('[data-detail-concern]').innerText(), '2つ目の認証がない');
  assert.match(await page.locator('[data-detail]').innerText(), /推定\s+ログインは利用者IDとパスワードだけで通す[\s\S]*根拠の行\s+app\/auth.py 1〜2行[\s\S]*IDとパスワードだけで一覧が表示された/);
  // 5 修正用のプロンプト：ガイドラインの代わりに、懸念・推定・根拠の行を入れる。
  await page.getByRole('button', { name: '修正用のプロンプトを作る' }).click();
  const concernPrompt = await page.locator('[data-prompt]').innerText();
  for (const part of ['## 次のことが起きていないこと：2つ目の認証がない', '根拠: コードから出た懸念（コードからの推定で、観測でも判定でもない）', '> 2つ目の認証がない', '- 根拠の行: app/auth.py 1〜2行', '- 結果: IDとパスワードだけで一覧が表示された', '関係しそうなコード: app/auth.py'])
    assert(concernPrompt.includes(part), part);
  // 6 再試験：懸念の項目も、文が同じなら前後で比べる。
  await page.getByRole('button', { name: '修正をコミットしたら再試験へ' }).click();
  await page.locator('[data-compare-table]').waitFor();
  assert.match(await page.locator('[data-compare-table] [data-kind="concern"]').innerText(), /次のことが起きていないこと：2つ目の認証がない[\s\S]*コードから出た懸念\s+—\s+不足を確認\s+変わらず不足/);

  // 引き継ぎ：別のコミットで、前のコミットで試験にした懸念を引き継ぐ。文が同じなので、再試験で前後を比べられる。
  await page.goto(flowUrl(commit));
  await page.getByRole('button', { name: '1 照合の準備' }).click();
  await page.getByRole('button', { name: `前のコミット ${previous.slice(0, 7)} で試験にした懸念を引き継ぐ（1件）` }).click();
  const carried = page.locator('[data-investigation="carried"]'); await carried.waitFor();
  assert.match(await carried.innerText(), /前のコミットで試験にした懸念を引き継いだ/);
  assert.equal(await carried.locator('[data-evidence]').getAttribute('href'), `https://github.com/${repository}/blob/${previous}/app/auth.py#L1-L2`, '根拠の行は前のコミットの行を指す');
  await page.getByRole('button', { name: '動かして確かめる懸念から試験の案を作る' }).click();
  await page.locator('[data-concern-candidate="c-1"]').waitFor();
  const before = requests.plan.length;
  await page.getByRole('button', { name: '試験の案を作る' }).click();
  await page.locator('[data-preview]').waitFor();
  const carriedPlan = requests.plan[before];
  assert.equal(carriedPlan.findings.find(f => f.id === 'c-1').requirement, '次のことが起きていないこと：2つ目の認証がない', '引き継いだ懸念は文が同じ');
  assert.deepEqual(carriedPlan.evidence, [{ path: 'app/auth.py', start: 1, end: 2, commit: previous }], '根拠の行には前のコミットを添える');
  assert.deepEqual(errors, []);
  console.log('flow smoke: ok');
} finally {
  await browser?.close();
  child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
}
