// 新しい流れ（/flow）から、既存の同梱OpenEMR試験（examples/openemr-browser の固定レシピ）へ受け渡す試験。
// 照合 → 試験の案 → 了承して確認項目に登録 → 設定画面で同梱の試験を人が選ぶ → 照合の候補を判定条件に人が対応づける →
// 実行（通常は実行直前で止める合成試験。--live では実OpenEMRを起動する）→ /flow の結果。
// AIエージェントは偽物のHTTPサーバー（応答は麦漆のサーバーが検査する）。新しい架空アプリの試験ではない。
// 後半は、/flow を別のコミットで開いてから同梱の試験（固定した版）を選ぶと、結果を照合したコミットの結果として数えないことを確かめる。
// --live の前提：Docker、レシピの固定したイメージ、Chromium 151.0.7922.34（EEL_BROWSER_EXECUTABLE）、
// 試験用アカウントのファイル（EEL_BROWSER_SECRETS_FILE）。実行器はChromiumのOSサンドボックスを使うため root でない利用者で動かす。
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..'), live = process.argv.includes('--live');
const recipe = JSON.parse(await readFile(join(root, 'examples/openemr-browser/soap-recipe.json'), 'utf8'));
const repository = recipe.source.repository, pinned = recipe.source.commit, other = 'a'.repeat(40), composePath = recipe.source.path;
const temp = await mkdtemp(join(tmpdir(), 'eel-flow-openemr-')), data = join(temp, 'app'), output = join(root, 'test-results');
let compose = '# 合成の読取記録。実OpenEMRのコードではありません。\nservices: {}\n';
if (live) {
  const response = await fetch(`https://raw.githubusercontent.com/${repository}/${pinned}/${composePath}`);
  assert.equal(response.status, 200); compose = await response.text();
}
await mkdir(join(data, 'snapshots', repository), { recursive: true });
for (const sha of [pinned, other]) await writeFile(join(data, 'snapshots', repository, sha + '.json'), JSON.stringify({
  schema: 1, fullName: repository, reference: sha, commit: sha,
  inventory: [{ path: composePath, mode: '100644' }], files: { [composePath]: sha === pinned ? compose : '# 合成の別の版\n' }, omitted: [],
}));
const tokenFile = join(temp, 'agent-token'); await writeFile(tokenFile, 'synthetic-agent-token-0123456789\n');

// 偽のAIエージェント。照合の候補は、試験で確かめる2件・案に含まれない1件・試験で確かめられない1件。
const cite = { passage_id: 'p:1', source_id: 'mhlw-security', document: '合成の安全管理ガイドライン', page: '1', sha256: 'b'.repeat(64), origin: 'x', excerpt: '合成の断片' };
const finding = (requirement, method) => ({ requirement, relevance: 'likely', citations: [cite], system_refs: [composePath], verification: { method, how: '合成' }, unknowns: [], patient_data: true, priority: 'high' });
const scanAnswer = { findings: [finding('診療記録を保存し、読み直せる', 'database_rows'), finding('ログに患者の情報を出さない', 'container_log'),
  finding('誰がいつ診療記録を操作したかを記録する', 'container_log'), finding('運用の手順書を整える', 'document_review')],
questions: [], sources: [], not_covered: [], model_call: { response_id: 'fake-scan' } };
const planAnswer = { tests: [
  { id: 't-soap', name: 'SOAPの保存と再表示', kind: 'browser', finding_ids: ['f-1'], steps: ['架空の患者でSOAPを保存し、開き直す'], pass: ['保存した内容がDBと画面に出る'], observation: 'database_rows' },
  { id: 't-log', name: 'SOAPの保存でログに試験値が出ないか', kind: 'browser', finding_ids: ['f-2'], steps: ['架空の患者でSOAPを保存する'], pass: ['コンテナのログに試験値が出ない'], observation: 'container_log' },
], not_planned: [] };
const agent = createServer((req, res) => {
  let body = ''; req.on('data', d => { body += d; });
  req.on('end', () => {
    const answer = req.url === '/v1/scan' ? scanAnswer : req.url === '/v1/plan' ? { ...planAnswer, model_call: { response_id: 'fake-plan' } } : null;
    res.writeHead(answer ? 200 : 404, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(answer || {}));
  });
}).listen(0, '127.0.0.1');
await new Promise(ok => agent.once('listening', ok));
// 同梱の試験の一覧は、運営者が EEL_RECIPE_DIR で指定したディレクトリから読む。指定しないと一覧は空になる。
const env = { ...process.env, EEL_AGENT_URL: `http://127.0.0.1:${agent.address().port}`, EEL_AGENT_TOKEN_FILE: tokenFile, EEL_RECIPE_DIR: join(root, 'examples') };
if (!live) { const secrets = join(temp, 'secrets.json'); await writeFile(secrets, JSON.stringify({ username: 'eel-operator', password: 'eel-pass-trial' }), { mode: 0o600 }); env.EEL_BROWSER_SECRETS_FILE = secrets; }
const child = spawn(process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi'), ['app', data, '--port', '0'], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env });
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
  const token = async () => page.locator('meta[name=review-token]').getAttribute('content');
  const view = async () => (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': await token() } })).json();
  const send = async () => { await page.locator('[data-preview]').waitFor(); await page.locator('#agree').check(); await page.getByRole('button', { name: 'この内容を送る' }).click(); };
  const flowUrl = sha => { const u = new URL(url + '/flow'); u.search = new URLSearchParams({ url: 'https://github.com/' + repository, ref: sha }).toString(); return u.href; };
  const flowToResults = async sha => {
    await page.goto(flowUrl(sha)); await page.locator('[data-files]').waitFor();
    await page.getByRole('button', { name: /照合を始める/ }).click(); await send();
    await page.getByRole('heading', { name: 'ヒアリング' }).waitFor();
    await page.getByRole('button', { name: '試験の案を作る' }).click(); await send();
    await page.locator('[data-test="t-log"]').waitFor();
    await page.getByRole('button', { name: '了承して確認項目に登録する' }).click();
    await page.locator('[data-result]').first().waitFor();
  };
  const row = requirement => page.locator('[data-result]', { hasText: requirement });
  // 設定画面で同梱の試験を選び、照合の候補を判定条件へ対応づけて保存する。
  const chooseLogRecipe = async (presetButton, links) => {
    await row('ログに患者の情報を出さない').getByRole('link').click();
    await page.locator('[data-flow-plan="t-log"]').waitFor();
    const preset = page.locator('[data-browser-preset="openemr-soap-logs"]'); await preset.waitFor();
    assert.match(await page.getByRole('region', { name: '同梱のブラウザ試験' }).innerText(), /適した試験かどうかは自動で確認していません/);
    await preset.getByRole('button', { name: presetButton }).click();
    await page.getByRole('region', { name: 'ブラウザ試験の設定' }).waitFor();
    await page.getByRole('button', { name: '次へ：試す状況を選ぶ', exact: true }).click();
    await page.getByRole('button', { name: '次へ：判定の条件を確認する', exact: true }).click();
    await page.getByRole('button', { name: '実行前に確認する', exact: true }).click();
    const editor = page.getByRole('region', { name: '確認項目のどの部分をこの試験で確かめるか' });
    const part = editor.locator('[data-aspect="f-2"]'); await part.waitFor();
    assert.equal(await part.locator('textarea').inputValue(), 'ログに患者の情報を出さない', '照合の候補が、確かめたい部分として出る');
    assert.match(await part.innerText(), /出所: 照合の候補。出典：合成の安全管理ガイドライン/);
    for (const id of links) await part.locator(`input[type=checkbox][value="${id}"]`).check();
    const before = (await view()).workspace.revision;
    await editor.getByRole('button', { name: '対応づけを保存する' }).click();
    for (let i = 0; i < 60 && (await view()).workspace.revision <= before; i++) await new Promise(r => setTimeout(r, 250));
    await page.getByText('対応づけを保存しました', { exact: false }).waitFor();
  };

  // 前半：レシピの固定版のコミットで照合し、同じ版の同梱の試験を選ぶ。
  await flowToResults(pinned);
  assert.deepEqual(await page.locator('[data-result]').evaluateAll(r => r.map(x => x.getAttribute('data-result'))), ['pending', 'pending']);
  assert.deepEqual(await page.locator('[data-untested]').evaluateAll(r => r.map(x => [x.getAttribute('data-untested'), x.children[1].textContent])),
    [['unclassified', '誰がいつ診療記録を操作したかを記録する']], '試験の案に含まれなかった候補は未分類として残る');
  await chooseLogRecipe(/^この試験を設定する：OpenEMR：SOAPの保存でログに試験値が出ないか/, ['soap-log-app', 'soap-log-db']);
  let v = await view();
  const logCheck = v.workspace.checks.find(c => c.aspects.some(a => a.id === 'f-2'));
  assert.equal(logCheck.source.commit, pinned, '同じ版なので参照先は変わらない');
  assert.deepEqual(logCheck.aspects.map(a => [a.id, a.observation, a.trial_checks.map(l => l.id)]), [['f-2', 'container_log', ['soap-log-app', 'soap-log-db']]]);
  let sent;
  if (!live) {
    await page.route('**/api/v1/prepare', route => route.fulfill({ json: { executed: false } }));
    await page.route('**/api/v1/execute', route => { sent = route.request().postDataJSON(); return route.fulfill({ status: 400, json: { error: '合成試験：実行直前で停止' } }); });
  }
  await page.getByRole('button', { name: '試験を始める', exact: true }).click();
  let liveSummary = null;
  if (live) {
    await Promise.race([
      page.locator('#result:not([hidden])').waitFor({ timeout: 900000 }),
      page.locator('#alert .dads-notification-banner').waitFor({ timeout: 900000 }).then(async () => { throw Error(await page.locator('#alert').innerText()); }),
    ]);
    v = await view();
    const evidence = v.workspace.evidence.at(-1);
    assert.equal(evidence.check_id, logCheck.id); assert.equal(evidence.execution.recipe.source.commit, pinned);
    const cv = v.checks.find(c => c.check_id === logCheck.id);
    liveSummary = { run_id: evidence.manifest.correlation.run_id, evidence_id: evidence.id, verdict: cv.verdict,
      results: evidence.execution.trial.results.map(r => ({ id: r.id, observed: r.observed })), aspects: cv.coverage.aspects.map(a => ({ id: a.id, status: a.status, status_label: a.status_label })),
      browser_completed: evidence.execution.containers.browser?.completed, leftovers: evidence.execution.containers.leftovers };
  } else {
    await page.getByText('合成試験：実行直前で停止', { exact: true }).waitFor();
    assert.equal(sent.recipe.check_id, logCheck.id);
    assert.equal(sent.recipe.source.commit, pinned);
    assert.deepEqual(sent.recipe.trial.checks.map(c => c.id), ['soap-subjective-row', 'soap-log-app', 'soap-log-db'], '同梱のログの試験の条件を変えずに送る');
    assert.deepEqual(sent.recipe.trial.checks.slice(1).map(c => [c.method, c.expect, c.text]), [['container_log', 'not_contains', '{{test_value}}'], ['container_log', 'not_contains', '{{test_value}}']]);
    assert.equal((await view()).workspace.evidence.length, 0);
  }
  // /flow の結果：ログの候補は判定条件に対応づけ済み。保存の候補はまだ対応づけていない。
  await page.goto(flowUrl(pinned)); await page.locator('[data-result]').first().waitFor();
  const logState = await row('ログに患者の情報を出さない').getAttribute('data-result');
  if (live) {
    assert(['ng', 'ok', 'unconfirmed'].includes(logState), `実行後のログの候補の状態: ${logState}`);
    liveSummary.flow_state = logState; liveSummary.flow_label = await row('ログに患者の情報を出さない').locator('[data-state-label]').innerText();
  } else {
    assert.equal(logState, 'pending');
    assert.equal(await row('ログに患者の情報を出さない').locator('[data-state-label]').innerText(), '未実行');
  }
  assert.equal(await row('診療記録を保存し、読み直せる').locator('[data-state-label]').innerText(), '未実行（判定条件への対応づけ前）');

  // 後半：別のコミットで照合してから、固定した版の同梱の試験を選ぶ。参照先はレシピの版に変わり、
  // /flow はその結果を照合したコミットの結果として数えない。
  await flowToResults(other);
  await chooseLogRecipe(/^レシピの版に切り替えて設定する：OpenEMR：SOAPの保存でログに試験値が出ないか/, ['soap-log-app']);
  v = await view();
  const moved = v.workspace.checks.filter(c => c.aspects.some(a => a.id === 'f-2')).find(c => c.id !== logCheck.id);
  assert.equal(moved.source.commit, pinned, '同梱の試験を選ぶと、参照先がレシピの版に変わる');
  await page.goto(flowUrl(other)); await page.locator('[data-result]').first().waitFor();
  assert.equal(await row('ログに患者の情報を出さない').getAttribute('data-result'), 'other_version');
  assert.match(await row('ログに患者の情報を出さない').locator('[data-state-label]').innerText(), new RegExp(`別の版の試験：確認項目の参照先が ${pinned.slice(0, 7)} に変わっている（照合したのは ${other.slice(0, 7)}）`));
  assert.equal(await page.locator('[data-ng-count]').count(), 0, '別の版の結果を、このコミットの不足として数えない');
  assert.deepEqual(errors, []);
  if (live) { await mkdir(output, { recursive: true }); await writeFile(join(output, 'flow-openemr-live.json'), JSON.stringify({ recipe: { repository, commit: pinned, path: composePath }, ...liveSummary }, null, 2)); }
  console.log(live ? JSON.stringify(liveSummary) : 'flow openemr handoff smoke: ok');
} catch (error) {
  await mkdir(output, { recursive: true });
  await browser?.contexts()[0]?.pages()[0]?.screenshot({ path: join(output, 'flow-openemr-handoff-failure.png'), fullPage: true }).catch(() => {});
  throw error;
} finally {
  await browser?.close(); child.kill('SIGTERM'); agent.close();
  await rm(temp, { recursive: true, force: true });
}
