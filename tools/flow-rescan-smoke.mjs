// 照合し直したときに、前の照合の了承・結果・ヒアリングの回答を新しい照合へ流用しないことを、画面から確かめる合成試験。
// 了承して登録 → 結果 → 照合し直す（質問の文言と選択肢が変わる）→ 同じ試験IDで手順と合格の条件が違う案 →
// 新しい案は未了承として扱い、前の確認項目の観測を新しい案の結果にしない。前の照合は履歴に残す。
// AIの応答は偽物（page.route）で、試験の実行はしない。観測の状態は事例の記録の応答を差し替えて作る。
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const temp = await mkdtemp(join(tmpdir(), 'eel-flow-rescan-')), data = join(temp, 'app');
const repository = 'example/clinic', commit = 'c'.repeat(40);
await mkdir(join(data, 'snapshots', repository), { recursive: true });
await writeFile(join(data, 'snapshots', repository, commit + '.json'), JSON.stringify({
  schema: 1, fullName: repository, reference: commit, commit,
  inventory: [{ path: 'app/tasks.py', mode: '100644' }, { path: 'README.md', mode: '100644' }],
  files: { 'app/tasks.py': 'def list_tasks(user):\n    return ALL\n', 'README.md': '架空の診療所' }, omitted: [],
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
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '合成の開発者', role: 'developer' })));

  const cite = { passage_id: 'owasp-top10:p1:1', source_id: 'owasp-top10', document_id: 'owasp-top10-main', source: '合成のOWASP', publisher: '合成', document: '合成のOWASP', version: null, page: '1', origin: 'file:合成', sha256: 'd'.repeat(64), excerpt: '原則拒否' };
  const finding = requirement => ({ status: 'candidate', requirement, relevance: 'likely', citations: [cite], system_refs: ['app/tasks.py'], patient_data: true, priority: 'high', unknowns: [],
    verification: { method: 'readback_output', method_label: '読み出し', observation: 'readback_output', support: 'judged', support_note: '合成', executable_check: true, how: '合成' } });
  // 1回目と2回目の照合で、同じ位置（f-1）に違う要求、同じ質問ID（q1）に違う質問を返す。
  const scans = [
    { findings: [finding('担当外の職員の一覧に患者のタスクを出さない')], questions: [{ id: 'q1', text: '院外から使いますか', why: '合成', options: ['はい', 'いいえ'] }] },
    { findings: [finding('操作の記録を残す')], questions: [{ id: 'q1', text: 'ログはどこに保存しますか', why: '合成', options: ['ファイル', 'データベース'] }] },
  ];
  // 2つの案は、試験ID・名前・候補IDが同じで、手順と合格の条件だけが違う。
  const plans = [
    { id: 't-same', name: '一覧の試験', kind: 'cli', finding_ids: ['f-1'], steps: ['職員Aで追加し、職員Bで一覧を読む'], pass: ['職員Bの一覧に出ない'], observation: 'readback_output', observation_label: '読み出し' },
    { id: 't-same', name: '一覧の試験', kind: 'cli', finding_ids: ['f-1'], steps: ['職員Aで追加し、記録のファイルを読む'], pass: ['記録に user=A action=add が残る'], observation: 'readback_output', observation_label: '読み出し' },
  ];
  const sent = { scan: [], plan: [] };
  const reply = (name, body, answer) => {
    if (!body.confirm) return { stage: 'preview', destination: '合成', path: '/v1/' + name, bytes: 1, payload_sha256: 'e'.repeat(64), note: '合成', payload: body };
    sent[name].push(body); return answer(sent[name].length - 1);
  };
  await page.route('**/api/v1/agent', route => route.fulfill({ json: { status: 'configured', message: '合成' } }));
  await page.route('**/api/v1/agent/flow-scan', route => route.fulfill({ json: reply('scan', route.request().postDataJSON(), n => ({
    stage: 'sent', status: 'candidate', ...scans[Math.min(n, 1)], sources: [], not_covered: [], priority_rule: '合成', model_call: { response_id: `fake-scan-${n + 1}`, response_model: 'fake' } })) }));
  await page.route('**/api/v1/agent/plan', route => route.fulfill({ json: reply('plan', route.request().postDataJSON(), n => ({
    stage: 'sent', status: 'draft', not_planned: [], tests: [plans[Math.min(n, 1)]], model_call: { response_id: `fake-plan-${n + 1}`, response_model: 'fake' } })) }));
  // 最初に登録した確認項目だけ、観測の結果「期待どおり」を返す。新しい案の結果に流用されてはいけない。
  let firstCheck = null;
  await page.route('**/api/v1/case', async route => {
    const response = await route.fetch(); const json = await response.json();
    for (const cv of json.checks || []) if (cv.check_id === firstCheck) {
      for (const a of cv.coverage?.aspects || []) Object.assign(a, { status: 'observed', status_label: '観測した', detail: '前の案での観測', trial_checks: [{ id: 'c2', label: '合成' }] });
      cv.evidence = [{ id: 'ev-1', run_id: 'run-old', applicable: true, invalidated_by: [], observed: {} }]; cv.selected_evidence = 'ev-1';
    }
    return route.fulfill({ response, json });
  });
  const send = async () => { await page.locator('[data-preview]').waitFor(); await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click(); };
  const stepEnabled = async n => !(await page.locator('#steps button').nth(n - 1).isDisabled());
  const u = new URL(url + '/flow'); u.search = new URLSearchParams({ url: 'https://github.com/' + repository, ref: commit }).toString();
  await page.goto(u.href);

  // 1回目：照合 → 回答 → 案 → 了承して登録。
  await page.locator('[data-files]').waitFor();
  await page.getByRole('button', { name: /照合を始める/ }).click(); await send();
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  await page.locator('[data-question="q1"] input[type=radio]').first().check();
  await page.getByRole('button', { name: '試験の案を作る' }).click(); await send();
  assert.deepEqual(sent.plan[0].answers, [{ question: '院外から使いますか', answer: 'はい' }]);
  await page.locator('[data-test="t-same"]').waitFor();
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  await page.locator('[data-result]').first().waitFor();
  let view = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json();
  firstCheck = view.workspace.checks.find(c => c.id.startsWith('flow-')).id;
  await page.getByRole('button', { name: '結果を読み直す' }).click();
  await page.locator('main p.note', { hasText: '試験で期待どおりだった項目：担当外の職員の一覧に患者のタスクを出さない' }).waitFor(); // 前提：1回目の確認項目は期待どおり

  // 2回目の照合：了承と結果は新しい照合に持ち越さない。前の回答も、別の質問には使わない。
  await page.locator('#steps button').nth(1).click();
  await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  await page.getByRole('button', { name: '回答して照合し直す' }).click(); await send();
  await page.locator('[data-question="q1"]').filter({ hasText: 'ログはどこに保存しますか' }).waitFor();
  assert.equal(await page.locator('[data-question="q1"] input[type=radio]:checked').count(), 0, '前の質問の回答を別の質問の回答として表示しない');
  assert.equal(await stepEnabled(3), false, '新しい照合ではまだ試験の案がない');
  assert.equal(await stepEnabled(4), false, '前の了承で結果へ進めない');
  await page.reload(); await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
  assert.equal(await page.locator('#steps button[aria-current=step]').innerText(), '▸ 2 照合とヒアリング', '読み込み直しても前の了承で結果へ飛ばない');
  await page.getByRole('button', { name: '試験の案を作る' }).click(); await send();
  assert.deepEqual(sent.plan[1].answers, [], '回答していない新しい質問に前の回答を送らない');

  // 同じ試験IDで中身が違う案：未了承として出し、前の確認項目の観測を新しい案の結果にしない。
  const box = page.locator('[data-test="t-same"]'); await box.waitFor();
  assert.match(await box.innerText(), /記録に user=A action=add が残る/);
  assert.equal(await box.locator('input[type=checkbox]').isDisabled(), false, '新しい案はまだ了承していない');
  assert.doesNotMatch(await box.innerText(), /登録済み/);
  assert.equal(await stepEnabled(4), false);
  await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
  await page.locator('[data-result]').first().waitFor();
  view = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json();
  const flows = view.workspace.checks.filter(c => c.id.startsWith('flow-'));
  assert.equal(flows.length, 2, '新しい案は新しい確認項目として登録する');
  assert.match(flows.find(c => c.id !== firstCheck).text, /記録に user=A action=add が残る/);
  assert.deepEqual(await page.locator('[data-result]').evaluateAll(r => r.map(x => x.getAttribute('data-result'))), ['pending'], '前の案の観測を新しい案の結果にしない');
  assert.doesNotMatch(await page.locator('main').innerText(), /期待どおりだった項目/);

  // 前の照合・案・了承・結果は履歴に残す。
  const flow = await (await page.request.get(url + '/api/v1/flow', { headers: { 'X-Review-Token': await page.locator('meta[name=review-token]').getAttribute('content') } })).json();
  const record = flow.records[`${repository}@${commit}`].record;
  assert.equal(record.history?.length, 1, '照合し直す前の照合・案・了承・回答を履歴に残す');
  assert.equal(record.history[0].scan.findings[0].requirement, '担当外の職員の一覧に患者のタスクを出さない');
  assert.equal(record.history[0].approved['t-same'].check_id, firstCheck);
  assert.equal(record.history[0].answers.q1.answer, 'はい');
  assert.deepEqual(errors, []);
  console.log('flow rescan smoke: ok');
} finally {
  await browser?.close();
  child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
}
