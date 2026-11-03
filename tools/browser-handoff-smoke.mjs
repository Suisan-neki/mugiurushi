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
  await page.goto(url + '/workbench');
  const token = await page.locator('meta[name=review-token]').getAttribute('content'), headers = { 'X-Review-Token': token, 'Content-Type': 'application/json' };
  const view = async () => (await page.request.get(url + '/api/v1/case', { headers })).json();
  const post = async (name, body) => {
    const r = await page.request.post(url + '/api/v1/' + name, { headers, data: body });
    const result = await r.json(); assert.equal(r.status(), 200, JSON.stringify(result)); return result;
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
  await saveButton.click();
  await page.getByText('対応づけを保存しました', { exact: false }).waitFor();
  assert.equal(await saveButton.isDisabled(), true, '保存直後は変更がない');
  v = await view();
  assert.deepEqual(v.workspace.checks[0].aspects.map(a => [a.observation, a.trial_checks.map(l => l.id)]), [['database_rows', ['soap-subjective-row']], ['external_destination', []]]);
  const savedLink = v.workspace.checks[0].aspects[0].trial_checks[0];
  assert.equal(savedLink.fingerprint.length, 64, '麦漆が判定条件の内容を指紋にして保存する'); assert.equal(savedLink.definition, undefined); assert.match(savedLink.label, /SOAP subjectiveの保存/);
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
  await page.locator('[data-aspect-editor][data-fingerprints="loaded"]').waitFor();
  assert.equal(await editor.locator('[data-link-changed]').count(), 0, '同じ試験設定なら変更なし');
  // 判定条件 soap-subjective-row は同じまま、試す状況（DB停止）だけを変えた試験では、前の対応づけを適用しない。
  await page.locator('a.step-link[href="#trial"]').click();
  await page.locator('[data-browser-preset="openemr-soap-db-stopped"]').getByRole('button').click();
  await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
  await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
  await page.getByRole('button', { name: '実行前に確認する', exact: true }).click();
  const editor2 = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' });
  await page.locator('[data-aspect-editor][data-fingerprints="loaded"]').waitFor();
  assert.equal(await editor2.locator('[data-link-changed="true"][data-link="soap-subjective-row"]').count(), 1);
  assert.match(await editor2.locator('[data-changed-links]').innerText(), /1 件あります/);
  assert.match(await editor2.locator('[data-uncovered]').innerText(), /form_soap に保存される/, '変わった判定条件への対応づけはこの試験では確かめない');
  // 別の部分を追加して保存しても、変わった判定条件への対応づけは以前の指紋のまま。
  await editor2.getByRole('button', { name: '確かめたい部分を追加する' }).click();
  await editor2.locator('[data-aspect] textarea').nth(2).fill('DB停止中に保存が失敗したと画面に出る');
  await editor2.getByRole('button', { name: '対応づけを保存する' }).click();
  await page.getByText('対応づけを保存しました', { exact: false }).waitFor();
  v = await view();
  assert.equal(v.workspace.checks[0].aspects.length, 3);
  assert.equal(v.workspace.checks[0].aspects[0].trial_checks[0].fingerprint, savedLink.fingerprint, '別の部分の保存で、変わった判定条件への対応づけを更新しない');
  assert.equal(await editor2.locator('[data-link-changed="true"][data-link="soap-subjective-row"]').count(), 1);
  // 人が「現在の内容で対応づけ直す」を押して保存すると、この試験の内容で対応づける。
  await editor2.locator('[data-relink="soap-subjective-row"]').click();
  await editor2.getByRole('button', { name: '対応づけを保存する' }).click();
  await page.getByText('対応づけを保存しました', { exact: false }).waitFor();
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
  await page.locator('[data-aspect-editor][data-fingerprints="loaded"]').waitFor();
  assert.equal(await editor3.locator('[data-link-changed="true"]').count(), 1);
  await editor3.locator('[data-relink="soap-subjective-row"]').click();
  await editor3.locator('[data-aspect]').nth(2).getByRole('button', { name: 'この部分を外す' }).click();
  await editor3.getByRole('button', { name: '対応づけを保存する' }).click();
  await page.getByText('対応づけを保存しました', { exact: false }).waitFor();
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
    const twoPatients = page.locator('[data-browser-preset="openemr-soap-two-patients"]');
    await twoPatients.getByRole('button').click();
    await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
    await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
    const preview = await page.locator('#trial-body').innerText();
    assert.match(preview, /患者Bの患者IDの変化：DBから読み取った操作前の/);
    assert.match(preview, /患者AとBの識別子：DBから読み取った/);
    assert.doesNotMatch(preview, /undefined/);
  }
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ checks: ['版の相違を表示', '明示操作による参照先変更', '相談の出所を保持', '確認まで実行しない', 'ブラウザ手順・入力・観測先を保持', '確かめる部分を観測の種類と判定条件の内容で登録', '試す状況を変えた試験には前の対応づけを適用しない', ...(live ? ['画面から実OpenEMRを起動・保存・再表示・DB照合', '再読込後にブラウザ設定へ戻る'] : [])], actual_target_execution: live ? 'OpenEMR' : 'not_conducted' }));
} catch (error) {
  await mkdir(output, { recursive: true });
  const page = browser?.contexts()[0]?.pages()[0];
  await page?.screenshot({ path: join(output, 'browser-handoff-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally { child.kill('SIGTERM'); await browser?.close(); await rm(temp, { recursive: true, force: true }); }
