// 架空の問診アプリ（examples/monshin）の流れ（/flow）を、実際の mugiurushi-ai と実際のモデルで、画面から最後まで通して記録する。
// tools/monshin-demo.mjs で起動した画面を操作する（リポジトリの登録 → 読み込み → ファイルを外す → 説明 → 送る内容の確認 → 照合 →
// ヒアリング → 送る内容の確認 → 試験の案 → 了承して実行 → 結果 → 修正用のプロンプト → fix/answer-access で、了承した試験を実行し直す）。
// 実際のモデルの応答は毎回変わるため、応答の中身は検査せずに記録する（test-results/monshin-live.json と画面の写真）。
// 流れが最後の画面まで進まなかったときだけ失敗にする。合否の見込み（main で満たさない、修正版で解消）は記録を人が読んで確かめる。
// MONSHIN_DEMO_DIR（tools/monshin-demo.mjs のデータの保存先を残す指定）と組み合わせて、前もって進めた記録から続ける流れも試せる：
// MONSHIN_STOP_AT=plan で、main の試験の案の画面まで進めて止める。同じ MONSHIN_DEMO_DIR でもう一度実行すると、
// main を読み込んだ時点で試験の案の画面から始まるので、照合と試験の案をAIへ送らずに、了承・実行・修正用のプロンプト・再試験へ進む。
// 必要なもの: tools/monshin-demo.mjs と同じ（MUGIURUSHI_AI_DIR、MUGIURUSHI_AI_MODEL、認証）と、Playwrightのブラウザ。
import { spawn } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const REPO = 'example/monshin';
const DESCRIPTION = process.env.MONSHIN_DESCRIPTION || '架空の診療所の問診システム。患者がコマンドで答え、医師が読む。受付の職員は、回答があるかどうかだけを確かめる。';
// ヒアリングで選ぶ選択肢に含まれる語（先に書いた語を優先する）。説明文と同じく、デモで人が答える想定の内容。
const ANSWERS = (process.env.MONSHIN_ANSWERS || '有無のみ,医師だけ,医師のみ').split(',').map(w => w.trim()).filter(Boolean);
const output = join(root, 'test-results'); await mkdir(output, { recursive: true });
const record = { started_at: new Date().toISOString(), model: process.env.MUGIURUSHI_AI_MODEL, repository: REPO, description: DESCRIPTION, answer_words: ANSWERS, runs: [] };
const demo = spawn(process.execPath, [join(root, 'tools/monshin-demo.mjs')], { env: process.env, stdio: ['ignore', 'pipe', 'pipe'] });
let browser;
try {
  const url = await new Promise((ok, no) => {
    let out = '', err = ''; const timer = setTimeout(() => no(Error('画面の起動待ちが上限に達した: ' + err.slice(-1500))), 120000);
    demo.stdout.on('data', d => { out += d; const m = /照合の流れの画面: (http:\/\/127\.0\.0\.1:\d+)\/flow/.exec(out); if (m) { clearTimeout(timer); ok(m[1]); } });
    demo.stderr.on('data', d => { err += d; });
    demo.once('exit', code => { clearTimeout(timer); no(Error(`tools/monshin-demo.mjs が終了した（${code}）: ${err.slice(-1500)}`)); });
  });
  browser = await chromium.launch({ headless: true, executablePath: process.env.REVIEW_BROWSER_EXECUTABLE || undefined, args: ['--no-sandbox'] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: url });
  const page = await context.newPage(); page.setDefaultTimeout(30000);
  const errors = []; page.on('pageerror', e => errors.push(e.message));
  /** @type {any[]} */ const agent = [];
  page.on('response', async r => { if (r.url().includes('/api/v1/agent/') && r.request().method() === 'POST') agent.push({ path: new URL(r.url()).pathname, status: r.status(), confirmed: !!r.request().postDataJSON()?.confirm, body: await r.json().catch(() => null) }); });
  await page.addInitScript(() => sessionStorage.setItem('eel-person', JSON.stringify({ name: '架空の開発者', role: 'developer' })));
  const token = async () => page.locator('meta[name=review-token]').getAttribute('content');
  const alertText = async () => (await page.locator('#alert').innerText().catch(() => '')).trim();

  // 送る前の確認の画面で確かめて送り、確認後の応答を返す。
  const send = async (/** @type {string} */ name, /** @type {string} */ path) => {
    await page.locator('[data-preview]').waitFor();
    const before = agent.length, at = Date.now();
    await page.locator('#agree').check();
    await page.getByRole('button', { name, exact: true }).click();
    for (let i = 0; i < 600; i++) { if (agent.slice(before).some(a => a.path === path && a.confirmed)) break; await new Promise(r => setTimeout(r, 500)); }
    const res = agent.slice(before).find(a => a.path === path && a.confirmed);
    if (!res || res.status !== 200) throw Error(`${path} の応答を受け取れなかった: ${JSON.stringify(res?.body ?? null).slice(0, 1500)} ${await alertText()}`);
    return { body: res.body, seconds: Math.round((Date.now() - at) / 100) / 10 };
  };

  // 読み込んだブランチで、ファイルを外す画面から結果の画面まで進む。
  // carried は、前のコミットで了承した試験を引き継いだ場合（照合と試験の案をAIへ送らず、試験の案の画面から始まる）。
  const run = async (/** @type {string} */ branch, /** @type {string} */ tag, carried = false) => {
    /** @type {any} */ const r = { branch, carried }; record.runs.push(r);
    const shot = (/** @type {string} */ name) => page.screenshot({ path: join(output, `monshin-live-${tag}-${name}.png`), fullPage: true });
    if (!carried) {
      await page.locator('[data-screen=files]').waitFor({ timeout: 60000 });
      r.loaded_branch = await page.locator('[data-branch]').innerText();
      await page.getByLabel('README.mdを外す').check();
      await shot('2-files');
      await page.getByRole('button', { name: '次へ' }).click();
      await page.locator('#desc').fill(DESCRIPTION);
      await page.getByRole('button', { name: '次へ' }).click();
      await page.locator('[data-runnable]').waitFor();
      r.runnable = { state: await page.locator('[data-runnable]').getAttribute('data-runnable'), text: await page.locator('[data-runnable]').innerText() };
      await shot('3-confirm-scan');
      const scan = await send('照合を始める', '/api/v1/agent/flow-scan');
      r.scan = { seconds: scan.seconds, model_call: scan.body.model_call, questions: scan.body.questions,
        findings: scan.body.findings.map((/** @type {any} */ f, /** @type {number} */ i) => ({ id: `f-${i + 1}`, requirement: f.requirement, priority: f.priority, method: f.verification?.method, executable_check: f.verification?.executable_check, citations: (f.citations || []).map((/** @type {any} */ c) => `${c.document} ${c.page}ページ`) })) };
      await page.locator('[data-screen=scan]').waitFor({ timeout: 60000 });
      await shot('4-scan');
      await page.getByRole('button', { name: /^次へ/ }).click();
      await page.locator('[data-screen=hearing]').waitFor();
      // ヒアリングは、ANSWERS の語を含む選択肢を先頭から探して選ぶ。どれにも合わない質問は「未定」、それもなければ答えない（選んだ内容を記録する）。
      r.answers = [];
      for (const q of await page.locator('[data-question]').all()) {
        const options = await q.locator('label.choice').allInnerTexts();
        const index = [...ANSWERS, '未定'].map(w => options.findIndex(o => o.includes(w))).find(i => i >= 0);
        if (index === undefined) { r.answers.push({ question: await q.locator('legend').innerText(), answer: null, options }); continue; }
        const choice = q.locator('label.choice').nth(index);
        await choice.locator('input').check(); r.answers.push({ question: await q.locator('legend').innerText(), answer: await choice.innerText() });
      }
      await shot('5-hearing');
      await page.getByRole('button', { name: '試験の案を作る' }).click();
      await page.locator('[data-preview]').waitFor();
      await shot('6-confirm-plan');
      const plan = await send('試験の案を作る', '/api/v1/agent/plan');
      r.plan = { seconds: plan.seconds, model_call: plan.body.model_call, not_planned: plan.body.not_planned, unclassified: plan.body.unclassified,
        tests: plan.body.tests.map((/** @type {any} */ t) => ({ id: t.id, name: t.name, kind: t.kind, finding_ids: t.finding_ids, steps: t.steps, pass: t.pass, observation: t.observation, trial: t.trial ?? null, trial_note: t.trial_note ?? null })) };
    } else {
      await page.locator('[data-screen=plan]').waitFor({ timeout: 60000 });
      r.loaded_branch = await page.locator('[data-branch]').innerText();
      r.carried_note = await page.locator('[data-carried]').count() ? await page.locator('[data-carried]').innerText() : null;
      r.agent_calls_before = agent.length;
    }
    await page.locator('[data-test]').first().waitFor({ timeout: 60000 });
    for (const s of await page.locator('[data-test] details summary').all()) await s.click();
    await shot('7-plan');
    if (process.env.MONSHIN_STOP_AT === 'plan') { record.stopped_at = 'plan'; return null; }
    const approve = page.getByRole('button', { name: /^了承して(実行|登録)する$/ });
    r.approve_button = await approve.innerText();
    const at = Date.now();
    await approve.click();
    await page.locator('[data-screen=result]').waitFor({ timeout: 600000 });
    r.execute_seconds = Math.round((Date.now() - at) / 100) / 10;
    if (carried) r.agent_calls_during = agent.length - r.agent_calls_before;
    r.alert = await alertText();
    r.title = await page.locator('h1').innerText();
    for (const s of await page.locator('[data-result] details summary').all()) await s.click();
    r.results = await page.locator('[data-result]').evaluateAll(rows => rows.map(x => ({ finding: x.getAttribute('data-finding'), state: x.getAttribute('data-result'), text: /** @type {HTMLElement} */ (x).innerText })));
    const view = await (await page.request.get(url + '/api/v1/case', { headers: { 'X-Review-Token': /** @type {string} */ (await token()) } })).json();
    r.checks = view.workspace.checks.filter((/** @type {any} */ c) => c.id.startsWith('flow-')).map((/** @type {any} */ c) => ({ id: c.id, text: c.text, aspects: c.aspects.map((/** @type {any} */ a) => ({ id: a.id, observation: a.observation, trial_checks: a.trial_checks.map((/** @type {any} */ x) => x.id) })) }));
    await shot('8-result');
    return r;
  };

  // 1. main を読み込み、結果まで進む。
  await page.goto(url + '/flow');
  await page.locator('[data-screen=repo]').waitFor();
  await page.locator('#repo').fill(`https://github.com/${REPO}`);
  await page.locator('#branch:not([disabled])').waitFor();
  record.branches = await page.locator('#branch option').allInnerTexts();
  await page.screenshot({ path: join(output, 'monshin-live-main-1-repo.png'), fullPage: true });
  await page.getByRole('button', { name: '読み込む' }).click();
  // 前もって進めた記録があれば、読み込んだ時点で試験の案の画面から始まる。
  await page.locator('[data-screen=files], [data-screen=plan]').waitFor({ timeout: 60000 });
  record.resumed_at_plan = await page.locator('[data-screen=plan]').count() > 0;
  await run('main', 'main', record.resumed_at_plan);

  // 2. 修正用のプロンプトを作る（満たさない項目がある場合だけボタンが出る）。
  const promptButton = page.getByRole('button', { name: '修正用のプロンプトを作る' });
  if (record.stopped_at) record.prompt_note = '試験の案の画面で止めた';
  else if (await promptButton.count()) {
    await promptButton.click();
    record.prompt = await page.locator('[data-prompt]').innerText();
    await page.getByRole('button', { name: 'コピーする' }).click();
    await page.locator('.toast').waitFor();
    await page.screenshot({ path: join(output, 'monshin-live-main-9-prompt.png'), fullPage: true });
    // 3. 修正版のブランチで再試験し、前回との比較を記録する。
    await page.getByRole('link', { name: '再試験へ' }).click();
    await page.locator('#retest-branch option[value="fix/answer-access"]').waitFor({ state: 'attached' });
    await page.locator('#retest-branch').selectOption('fix/answer-access');
    await page.screenshot({ path: join(output, 'monshin-live-main-10-retest.png'), fullPage: true });
    await page.getByRole('button', { name: '了承した試験を、このブランチで実行し直す' }).click();
    await run('fix/answer-access', 'fix', true);
    await page.locator('.stepper button', { hasText: '修正用のプロンプト' }).click();
    await page.getByRole('link', { name: '再試験へ' }).click();
    await page.locator('[data-compare-table]').waitFor();
    record.compare = await page.locator('[data-compare]').evaluateAll(rows => rows.map(x => ({ change: x.getAttribute('data-compare'), cells: [...x.querySelectorAll('td')].map(td => /** @type {HTMLElement} */ (td).innerText) })));
    await page.screenshot({ path: join(output, 'monshin-live-fix-9-retest.png'), fullPage: true });
  } else record.prompt_note = '満たさない項目がなく、修正用のプロンプトを作るボタンが出なかった';
  record.page_errors = errors;
  record.completed = true;
} catch (error) {
  record.completed = false; record.error = String(/** @type {any} */ (error).message || error).slice(0, 4000);
  const p = browser?.contexts()[0]?.pages()[0];
  if (p) { await p.screenshot({ path: join(output, 'monshin-live-failure.png'), fullPage: true }).catch(() => {}); record.failure_screen = await p.locator('main').innerText().catch(() => ''); }
  process.exitCode = 1;
} finally {
  record.finished_at = new Date().toISOString();
  await writeFile(join(output, 'monshin-live.json'), JSON.stringify(record, null, 2));
  await browser?.close();
  demo.kill('SIGTERM');
  console.log(JSON.stringify({ completed: record.completed, error: record.error, output: relative(root, join(output, 'monshin-live.json')) }));
}
