// 実Rust APIと画面で、固定レシピの選択・版の変更・実行要求を確認する。
// --liveではOpenEMRを実際に起動する。通常は実行直前で止める合成試験。
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join, relative } from 'node:path';
import { chromium } from 'playwright';
const root = resolve(import.meta.dirname, '..'), live = process.argv.includes('--live');
const temp = await mkdtemp(join(tmpdir(), 'eel-browser-handoff-')), data = join(temp, 'app');
const original = JSON.parse(await readFile(join(root, 'examples/openemr-browser/soap-recipe.json'), 'utf8'));
const repository = original.source.repository, commit = original.source.commit, before = 'a'.repeat(40), path = original.source.path;
const output = join(root, 'test-results');
await mkdir(join(data, 'snapshots', repository), { recursive: true });
let text = '# 合成の読取記録。実OpenEMRのコードではありません。\n';
if (live) {
  const response = await fetch(`https://raw.githubusercontent.com/${repository}/${commit}/${path}`);
  assert.equal(response.status, 200); text = await response.text();
}
for (const sha of [before, commit]) await writeFile(join(data, 'snapshots', repository, sha + '.json'), JSON.stringify({
  schema: 1, fullName: repository, reference: sha, commit: sha,
  inventory: [{ path, mode: '100644' }], files: { [path]: sha === before ? '# 合成の旧版\n' : text }, omitted: [],
}));
const secrets = join(temp, 'secrets.json');
await writeFile(secrets, JSON.stringify({ username: 'eel-operator', password: 'eel-pass-trial' }), { mode: 0o600 });
const child = spawn(process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi'), ['app', data, '--port', '0'], {
  cwd: root, env: { ...process.env, EEL_BROWSER_SECRETS_FILE: relative(root, secrets) }, stdio: ['ignore', 'pipe', 'pipe'],
});
let browser;
try {
  const url = await new Promise((ok, no) => {
    let out = '', err = ''; const timer = setTimeout(() => no(Error(err || '起動待ちの上限')), 15000);
    child.stderr.on('data', d => err += d); child.stdout.on('data', d => {
      out += d; const m = /http:\/\/127\.0\.0\.1:\d+/.exec(out); if (m) { clearTimeout(timer); ok(m[0]); }
    }); child.once('exit', () => { clearTimeout(timer); no(Error(err)); });
  });
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined, args: ['--no-sandbox'] });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } }); page.setDefaultTimeout(15000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '合成の試験担当', role: 'developer' })));
  // AIエージェントの案（偽物）。採用は画面で人が選び、保存は麦漆の set_aspects で行う。
  await page.route('**/api/v1/agent', route => route.fulfill({ json: { status: 'configured', message: '合成' } }));
  const proposeRequests = [];
  await page.route('**/api/v1/agent/propose', route => {
    const body = route.request().postDataJSON(); proposeRequests.push(body);
    if (!body.confirm) return route.fulfill({ json: { stage: 'preview', destination: '合成のAIエージェント', path: '/v1/dialogue/turn', bytes: 42,
      payload_sha256: 'a'.repeat(64), note: '合成の確認', payload: { concern: '合成の確認項目', current_aspects: [] } } });
    return route.fulfill({ json: { stage: 'sent', status: 'draft', reply: 'DBと再表示を分けて確かめます', questions: ['保存先の表はどれですか'],
      out_of_scope: [{ text: '本番環境の保存', reason: '試験環境だけを扱う' }],
      note: '合成', proposals: [
        { id: 'ai-readback', text: 'AIの案: 再表示した画面にSOAPの主訴が出る', origin: 'mugiurushi-ai の案（合成）', observation: 'browser_text', rationale: '画面で確かめる', unknowns: [] },
        { id: 'ai-unused', text: 'AIの案: 採用しない案', origin: 'mugiurushi-ai の案（合成）', observation: null, rationale: '', unknowns: ['不明'] },
      ] } });
  });
  // ガイドラインとの照合（偽物）。送るのは人が書いた説明だけで、候補は確認項目にも判定にも入らない。
  const scanRequests = [];
  await page.route('**/api/v1/agent/scan', route => {
    const body = route.request().postDataJSON(); scanRequests.push(body);
    if (!body.confirm) return route.fulfill({ json: { stage: 'preview', destination: '合成のAIエージェント', path: '/v1/scan', bytes: 42,
      payload_sha256: 'b'.repeat(64), note: '合成の確認', payload: { system: { summary: body.summary, excerpts: [] } } } });
    const citation = { passage_id: 'synthetic:p3:1', source: '合成の指針', publisher: '合成の公開元', document: '合成の指針 本編', version: null, page: '3',
      origin: 'https://example.invalid/a.pdf', sha256: 'c'.repeat(64), fetched_at: '2026-11-06T00:00:00+00:00', excerpt: '合成の断片' };
    const finding = (requirement, method, executable_check, support) => ({ status: 'candidate', requirement, relevance: 'likely', citations: [citation], unknowns: ['合成の未確認点'],
      verification: { method, method_label: method, observation: support ? method : null, support, support_note: '合成の注記', executable_check, how: '合成の確かめ方' } });
    return route.fulfill({ json: { stage: 'sent', status: 'candidate', limits: '合成の限界', note: '照合の候補です（合成）', not_covered: ['合成の照合できなかった点'], model_call: null,
      findings: [finding('合成の要求: 記録を残す', 'container_log', true, 'judged'), finding('合成の要求: 手順書を整える', 'document_review', false, null)],
      sources: [{ source_id: 'synthetic', title: '合成の指針', status: 'used', status_label: '断片をモデルに渡した', documents: [{ document_id: 'synthetic-main', ingested: true, sha256: 'c'.repeat(64), passages_used: 1 }] },
        { source_id: 'missing', title: '合成の未取り込み', status: 'not_ingested', status_label: '取り込まれていない', documents: [] }] } });
  });
  await page.goto(url + '/workbench');
  const token = await page.locator('meta[name=review-token]').getAttribute('content'), headers = { 'X-Review-Token': token, 'Content-Type': 'application/json' };
  const view = async () => (await page.request.get(url + '/api/v1/case', { headers })).json();
  const post = async (name, body) => {
    const r = await page.request.post(url + '/api/v1/' + name, { headers, data: body });
    const result = await r.json(); assert.equal(r.status(), 200, JSON.stringify(result)); return result;
  };
  // 対応づけの保存は非同期。前回の状態表示が残るため、事例の版が進むまで待ってから確認する。
  const saveAspects = async editor => {
    const before = (await view()).workspace.revision;
    await editor.getByRole('button', { name: '対応づけを保存する' }).click();
    for (let i = 0; i < 60; i++) {
      if ((await view()).workspace.revision > before) break;
      if (await page.locator('#alert .dads-notification-banner').count()) throw Error('対応づけの保存に失敗: ' + await page.locator('#alert').innerText());
      await new Promise(r => setTimeout(r, 250));
    }
    assert.ok((await view()).workspace.revision > before, '対応づけの保存で事例の版が進む');
    await page.getByText('対応づけを保存しました', { exact: false }).waitFor();
  };
  const origin = '合成の相談から登録した確認項目。実利用者の相談ではありません。';
  await post('change', { expected_revision: (await view()).workspace.revision, actor: '合成の試験担当', reason: '画面の接続試験', action: { op: 'add_check', check: {
    id: 'soap-dialogue-check', version: 1, text: 'SOAPを保存して開き直し、4項目の内容をDBと照合できる', origin,
    source: { ...original.source, commit: before }, rule: 'manual_only', dependencies: [], questions: [],
  } } });
  const requestUrl = new URL(url + '/workbench'); requestUrl.search = new URLSearchParams({ url: 'https://github.com/' + repository, ref: before, check: 'soap-dialogue-check', from: 'system', focus: 'synthetic-node', diagram: 'd'.repeat(64) }); requestUrl.hash = 'trial';
  await page.goto(requestUrl.href);
  const preset = page.locator('[data-browser-preset="openemr-soap"]'); await preset.waitFor();
  assert.match(await page.getByRole('region', { name: '同梱のブラウザ試験' }).innerText(), /適した試験かどうかは自動で確認していません/);
  assert.match(await preset.innerText(), /現在の確認項目の版/);
  assert.equal((await view()).workspace.checks[0].source.commit, before);
  await preset.getByRole('button', { name: /^レシピの版に切り替えて設定する/ }).click();
  await page.getByRole('region', { name: 'ブラウザ試験の設定' }).waitFor();
  let v = await view(); assert.equal(v.workspace.checks[0].source.commit, commit); assert.equal(v.workspace.checks[0].origin, origin); assert.equal(v.workspace.evidence.length, 0);
  assert.equal(new URL(await page.locator('#back-to-system').getAttribute('href'), url).searchParams.get('ref'), before);
  await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
  await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
  await page.getByRole('button', { name: '実行前に確認する', exact: true }).click();
  assert.match(await page.locator('#trial-body').innerText(), /soap-subjective/);
  assert.match(await page.locator('#trial-body').innerText(), /患者とSOAPの対応：DBから読み取った/);
  const targets = page.locator('[data-database-targets]');
  assert.match(await page.locator('#trial-body').innerText(), /確認項目の全体を確かめたことにはなりません/);
  assert.match(await targets.innerText(), /form_soap/);
  assert.match(await targets.innerText(), /書き込み先を確認したことにはなりません/);
  assert.doesNotMatch(await page.locator('#trial-body').innerText(), /undefined/);
  assert.equal((await view()).workspace.evidence.length, 0);
  // 確認項目のどの部分をこの試験で確かめるかは、人が観測の種類と判定条件IDで登録する。文章の一致では決めない。
  const editor = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' });
  assert.match(await editor.innerText(), /まだ登録されていません/);
  assert.equal((await view()).workspace.checks[0].aspects.length, 0);
  await editor.getByRole('button', { name: '確かめたい部分を追加する' }).click();
  await editor.locator('[data-aspect] textarea').first().fill('SOAPの主訴が試験用DBの form_soap に保存される');
  await editor.locator('[data-aspect] select').first().selectOption('database_rows');
  await editor.locator('[data-aspect] input[type=checkbox][value="soap-subjective-row"]').check();
  await editor.getByRole('button', { name: '確かめたい部分を追加する' }).click();
  await editor.locator('[data-aspect] textarea').nth(1).fill('診療データが意図しない外部へ送信されない');
  await editor.locator('[data-aspect] select').nth(1).selectOption('external_destination');
  assert.match(await editor.locator('[data-aspect]').nth(1).innerText(), /麦漆では観測できない/);
  assert.match(await editor.locator('[data-uncovered]').innerText(), /外部へ送信されない/);
  const saveButton = editor.getByRole('button', { name: '対応づけを保存する' });
  assert.equal(await saveButton.isDisabled(), false, '変更があれば保存できる');
  await saveAspects(editor);
  assert.equal(await saveButton.isDisabled(), true, '保存直後は変更がない');
  v = await view();
  assert.deepEqual(v.workspace.checks[0].aspects.map(a => [a.observation, a.trial_checks.map(l => l.id)]), [['database_rows', ['soap-subjective-row']], ['external_destination', []]]);
  const savedLink = v.workspace.checks[0].aspects[0].trial_checks[0];
  assert.equal(savedLink.fingerprint.length, 64, '麦漆が判定条件の内容を指紋にして保存する'); assert.equal(savedLink.definition, undefined); assert.match(savedLink.label, /SOAP subjectiveの保存/);
  // AIの案は、人がチェックした案だけを加え、保存しても既存の部分のIDと対応づけは変わらない。加えた部分の対応づけは空。
  const proposals = editor.locator('[data-agent-proposals]');
  await proposals.getByRole('button', { name: /AIに確かめる範囲の案を出してもらう/ }).click();
  // まず送る内容を表示し、架空のデータだけと確かめるまで送れない。
  const preview = proposals.locator('[data-agent-preview]');
  await preview.waitFor();
  assert.match(await preview.innerText(), /送り先: 合成のAIエージェント[\s\S]*合成の確認項目/);
  assert.equal(await preview.getByRole('button', { name: 'この内容を送る' }).isDisabled(), true);
  assert.deepEqual(proposeRequests, [{ check_id: 'soap-dialogue-check', request: null }], '確認の段階では確認項目のIDと任意の依頼だけ');
  await preview.locator('[data-agent-synthetic]').check();
  await preview.getByRole('button', { name: 'この内容を送る' }).click();
  await proposals.locator('[data-proposal="ai-readback"]').waitFor();
  assert.deepEqual(proposeRequests[1].confirm, { payload_sha256: 'a'.repeat(64), synthetic_only: true });
  assert.match(await proposals.locator('[data-agent-out-of-scope]').innerText(), /本番環境の保存（理由: 試験環境だけを扱う）/);
  assert.match(await proposals.locator('[data-agent-questions]').innerText(), /保存先の表はどれですか/);
  assert.equal((await view()).workspace.checks[0].aspects.length, 2, '案を出しただけでは確認項目は変わらない');
  await proposals.locator('[data-proposal="ai-readback"] input[type=checkbox]').check();
  await proposals.getByRole('button', { name: /選んだ案を確かめたい部分に加える/ }).click();
  await saveAspects(editor);
  v = await view();
  assert.deepEqual(v.workspace.checks[0].aspects.map(a => a.id).slice(2), ['ai-readback'], '採用しなかった案は入らない');
  assert.deepEqual(v.workspace.checks[0].aspects[0].trial_checks[0].fingerprint, savedLink.fingerprint, '既存の部分の対応づけを保つ');
  assert.deepEqual(v.workspace.checks[0].aspects[2].trial_checks, []); assert.equal(v.workspace.checks[0].aspects[2].observation, 'browser_text');
  await editor.locator('[data-aspect="ai-readback"]').getByRole('button', { name: 'この部分を外す' }).click();
  await saveAspects(editor);
  v = await view();
  assert.equal(v.workspace.checks[0].aspects.length, 2);
  // 保存済みの対応づけでチェックボックスだけを変えると保存ボタンが有効になり、未保存のまま試験は始められない。
  await editor.locator('[data-aspect] input[type=checkbox][value="soap-objective-row"]').first().check();
  assert.equal(await saveButton.isDisabled(), false);
  await page.getByRole('button', { name: '試験を始める', exact: true }).click();
  assert.match(await page.locator('#alert').innerText(), /未保存の変更があります/);
  assert.equal((await view()).workspace.evidence.length, 0);
  assert.deepEqual((await view()).workspace.checks[0].aspects[0].trial_checks.map(l => l.id), ['soap-subjective-row'], '未保存の変更は判定に使う記録へ入らない');
  await editor.locator('[data-aspect] input[type=checkbox][value="soap-objective-row"]').first().uncheck();
  assert.equal(await saveButton.isDisabled(), true, '元に戻せば未保存の変更はない');
  await page.locator('#alert').getByRole('button', { name: '閉じる' }).click();
  assert.equal(v.workspace.checks[0].version, 2, '対応づけの登録は条件の版を変えない（レシピの版への切り替えで2になった版のまま）');
  assert.equal(v.checks[0].coverage.status, 'not_covered');
  assert.deepEqual(v.checks[0].coverage.aspects.map(a => a.status), ['no_evidence', 'unsupported']);
  assert.equal(v.workspace.evidence.length, 0);
  await page.locator('[data-aspect-editor][data-link-changes="loaded"]').waitFor();
  assert.equal(await editor.locator('[data-link-changed]').count(), 0, '同じ試験設定なら変更なし');
  // ガイドラインとの照合：送る内容を表示してから送り、候補・出典・出典ごとの状態を示す。事例の記録は変わらない。
  const beforeScan = (await view()).workspace.revision;
  await page.locator('a.step-link[href="#expect"]').click();
  const scan = page.locator('[data-guideline-scan]'); await scan.waitFor();
  await scan.locator('textarea').fill('合成のシステムの説明');
  await scan.getByRole('button', { name: /ガイドラインと照らす/ }).click();
  const scanPreview = scan.locator('[data-agent-preview]'); await scanPreview.waitFor();
  assert.deepEqual(scanRequests, [{ summary: '合成のシステムの説明' }], '確認の段階では書いた説明だけ');
  assert.equal(await scanPreview.getByRole('button', { name: 'この内容を送る' }).isDisabled(), true);
  await scanPreview.locator('[data-agent-synthetic]').check();
  await scanPreview.getByRole('button', { name: 'この内容を送る' }).click();
  await scan.locator('[data-scan-sources]').waitFor();
  assert.deepEqual(scanRequests[1].confirm, { payload_sha256: 'b'.repeat(64), synthetic_only: true });
  assert.equal(await scan.locator('[data-scan-finding]').count(), 2);
  assert.match(await scan.locator('[data-scan-finding="0"]').innerText(), /判定条件として観測できる種類/);
  assert.match(await scan.locator('[data-scan-finding="1"]').innerText(), /麦漆の試験では確かめられません/);
  assert.match(await scan.locator('[data-scan-sources]').innerText(), /合成の未取り込み[\s\S]*取り込まれていない/);
  assert.match(await scan.locator('[data-scan-not-covered]').innerText(), /合成の照合できなかった点/);
  assert.doesNotMatch(await scan.innerText(), /undefined/);
  assert.equal((await view()).workspace.revision, beforeScan, '照合で事例の記録は変わらない');
  // 判定条件 soap-subjective-row は同じまま、試す状況（DB停止）だけを変えた試験では、前の対応づけを適用しない。
  await page.locator('a.step-link[href="#trial"]').click();
  await page.locator('[data-browser-preset="openemr-soap-db-stopped"]').getByRole('button').click();
  await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
  await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
  await page.getByRole('button', { name: '実行前に確認する', exact: true }).click();
  const editor2 = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' });
  await page.locator('[data-aspect-editor][data-link-changes="loaded"]').waitFor();
  assert.equal(await editor2.locator('[data-link-changed="true"][data-link="soap-subjective-row"]').count(), 1);
  assert.match(await editor2.locator('[data-changed-links]').innerText(), /1 件あります/);
  assert.match(await editor2.locator('[data-uncovered]').innerText(), /form_soap に保存される/, '変わった判定条件への対応づけはこの試験では確かめない');
  // 対応づけ直す前に、何が変わったかを対応づけた時点と現在の値で示す。環境変数の値は記録・表示しない。
  const comparison = editor2.locator('[data-link-comparison="soap-subjective-row"]');
  assert.match(await comparison.innerText(), /対応づけた時点に麦漆が保存した設定/);
  assert.match(await comparison.innerText(), /変わった設定: .*故障の条件（1 件）/);
  assert.match(await comparison.locator('[data-change-section="故障の条件"]').innerText(), /normal[\s\S]*database_stopped/);
  assert.equal(await comparison.locator('[data-change-section="ブラウザの準備"]').count(), 1, '保存前の操作が準備へ移ったことを示す');
  await comparison.locator('[data-change-section="ブラウザの準備"] summary').click();
  assert.match(await comparison.locator('[data-uncomparable]').innerText(), /環境変数の値は記録しない/);
  assert.doesNotMatch(await editor2.innerText(), /eel-root-trial|eel-pass-trial/, '試験用の認証情報を表示しない');
  assert.doesNotMatch(JSON.stringify((await view()).workspace.checks), /eel-root-trial|eel-pass-trial/, '試験用の認証情報を対応づけの記録に残さない');
  await mkdir(output, { recursive: true });
  await comparison.scrollIntoViewIfNeeded(); await page.screenshot({ path: join(output, 'browser-handoff-link-changes.png') });
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), '比較表を出しても390px幅で横にはみ出さない');
  await comparison.scrollIntoViewIfNeeded(); await page.screenshot({ path: join(output, 'browser-handoff-link-changes-mobile.png') });
  await page.setViewportSize({ width: 1440, height: 1000 });
  // 別の部分を追加して保存しても、変わった判定条件への対応づけは以前の指紋のまま。
  await editor2.getByRole('button', { name: '確かめたい部分を追加する' }).click();
  await editor2.locator('[data-aspect] textarea').nth(2).fill('DB停止中に保存が失敗したと画面に出る');
  await saveAspects(editor2);
  v = await view();
  assert.equal(v.workspace.checks[0].aspects.length, 3);
  assert.equal(v.workspace.checks[0].aspects[0].trial_checks[0].fingerprint, savedLink.fingerprint, '別の部分の保存で、変わった判定条件への対応づけを更新しない');
  assert.equal(await editor2.locator('[data-link-changed="true"][data-link="soap-subjective-row"]').count(), 1);
  // 人が「現在の内容で対応づけ直す」を押して保存すると、この試験の内容で対応づける。
  await editor2.locator('[data-relink="soap-subjective-row"]').click();
  await saveAspects(editor2);
  v = await view();
  const relinked = v.workspace.checks[0].aspects[0].trial_checks[0];
  assert.notEqual(relinked.fingerprint, savedLink.fingerprint); assert.match(relinked.label, /データベースのコンテナを止める/);
  assert.equal(await editor2.locator('[data-link-changed]').count(), 0);
  // 通常時の試験へ戻す。前の試験の内容へ対応づけ直して、以降の確認と実行に使う。
  await page.locator('a.step-link[href="#trial"]').click();
  await page.locator('[data-browser-preset="openemr-soap"]').getByRole('button').click();
  await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
  await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
  await page.getByRole('button', { name: '実行前に確認する', exact: true }).click();
  const editor3 = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' });
  await page.locator('[data-aspect-editor][data-link-changes="loaded"]').waitFor();
  assert.equal(await editor3.locator('[data-link-changed="true"]').count(), 1);
  await editor3.locator('[data-relink="soap-subjective-row"]').click();
  await editor3.locator('[data-aspect]').nth(2).getByRole('button', { name: 'この部分を外す' }).click();
  await saveAspects(editor3);
  v = await view();
  assert.equal(v.workspace.checks[0].aspects[0].trial_checks[0].fingerprint, savedLink.fingerprint, '同じ試験設定へ戻せば同じ指紋になる');
  assert.equal(v.workspace.checks[0].aspects.length, 2);
  assert.equal(v.workspace.evidence.length, 0);
  await mkdir(output, { recursive: true }); await page.screenshot({ path: join(output, 'browser-handoff-confirm.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), '390px幅で横にはみ出さない');
  await page.screenshot({ path: join(output, 'browser-handoff-confirm-mobile.png'), fullPage: true });
  await page.setViewportSize({ width: 1440, height: 1000 });
  let sent;
  if (!live) {
    await page.route('**/api/v1/prepare', route => route.fulfill({ json: { executed: false } }));
    await page.route('**/api/v1/execute', route => {
      sent = route.request().postDataJSON(); return route.fulfill({ status: 400, json: { error: '合成試験：実行直前で停止' } });
    });
  }
  await page.getByRole('button', { name: '試験を始める', exact: true }).click();
  if (live) {
    await Promise.race([
      page.locator('#result:not([hidden])').waitFor({ timeout: 600000 }),
      page.locator('#alert .dads-notification-banner').waitFor({ timeout: 600000 }).then(async () => { throw Error(await page.locator('#alert').innerText()); }),
    ]);
    v = await view(); const evidence = v.workspace.evidence.at(-1);
    assert.equal(evidence.check_id, 'soap-dialogue-check'); assert.equal(evidence.execution.recipe.source.commit, commit);
    assert.equal(evidence.execution.containers.browser.completed, true); assert.deepEqual(evidence.execution.containers.leftovers, []);
    assert.match(evidence.execution.containers.database_query, /応答は 1$/);
    assert.match(await page.locator('#result-body').innerText(), /ブラウザの実行: 記録した操作が完了した/);
    assert(evidence.execution.trial.results.every(r => r.observed === 'pass'));
    const live = v.checks.find(c => c.check_id === 'soap-dialogue-check');
    assert.equal(live.verdict, 'satisfied');
    assert.equal(live.verification_passed, false, '外部送信の部分は観測できないため、試験が通っても確認項目は未通過');
    assert.deepEqual(live.coverage.aspects.map(a => a.status), ['observed', 'unsupported']);
    assert.match(await page.locator('[data-coverage-note]').innerText(), /一部を観測した/);
    const actual = evidence.execution.recipe;
    const normalized = value => {
      if (Array.isArray(value)) return value.map(normalized);
      if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
        .filter(([key, v]) => !(v === null || (['frames', 'http_reads'].includes(key) && Array.isArray(v) && !v.length)))
        .map(([key, v]) => [key, normalized(v)]));
      return value;
    };
    assert.deepEqual(normalized(actual.browser), normalized(original.browser));
    await writeFile(join(output, 'browser-handoff-live.json'), JSON.stringify({ source: actual.source, run_id: evidence.manifest.correlation.run_id, verdict: live.verdict, verification_passed: live.verification_passed, coverage: live.coverage.status, human_evaluation: 'not_conducted' }, null, 2));
    // 再読込後の再試験は、保存したブラウザ手順を復元する。
    await page.reload(); await page.locator('#result:not([hidden]) [data-evidence]').first().waitFor();
    assert.equal(new URL(await page.locator('#back-to-system').getAttribute('href'), url).searchParams.get('ref'), before);
    await page.locator('a.step-link[href="#trial"]').click();
    await page.getByRole('region', { name: 'ブラウザ試験の設定' }).waitFor();
  } else {
    await page.getByText('合成試験：実行直前で停止', { exact: true }).waitFor();
    const expected = structuredClone(original); expected.check_id = 'soap-dialogue-check'; expected.criterion.subject = expected.check_id;
    assert.deepEqual(sent.recipe, expected, '準備・操作・HTTP・DBの対象・入力・条件IDを落とさない');
    assert.equal((await view()).workspace.evidence.length, 0);
    await page.reload(); await page.locator('[data-browser-preset="openemr-soap"]').waitFor();
    assert.equal(new URL(await page.locator('#back-to-system').getAttribute('href'), url).searchParams.get('ref'), before);
    assert.equal(await page.locator('[data-browser-preset]').count(), 3, '同梱の試験はSOAPの通常時・DB停止・ログ');
    await page.locator('[data-browser-preset="openemr-soap-logs"]').getByRole('button').click();
    await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
    await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
    const logPreview = await page.locator('#trial-body').innerText();
    assert.match(logPreview, /OpenEMRのコンテナのログ（標準出力・標準エラー）に試験値が出ない/);
    assert.doesNotMatch(logPreview, /undefined/);
  }
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ checks: ['版の相違を表示', '明示操作による参照先変更', '相談の出所を保持', '確認まで実行しない', 'ブラウザ手順・入力・観測先を保持', '確かめる部分を観測の種類と判定条件の内容で登録', '試す状況を変えた試験には前の対応づけを適用しない', ...(live ? ['画面から実OpenEMRを起動・保存・再表示・DB照合', '再読込後にブラウザ設定へ戻る'] : [])], actual_target_execution: live ? 'OpenEMR' : 'not_conducted' }));
} catch (error) {
  await mkdir(output, { recursive: true });
  const page = browser?.contexts()[0]?.pages()[0];
  await page?.screenshot({ path: join(output, 'browser-handoff-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally { child.kill('SIGTERM'); await browser?.close(); await rm(temp, { recursive: true, force: true }); }
