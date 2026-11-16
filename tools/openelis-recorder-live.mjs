// 起動済みのOpenELIS Global 2 で、ログインから患者登録・検索までを操作し、操作の記録から試験のレシピを作る。
// 「人の操作」は、要素の位置へ実際のマウスとキーボードの入力を送って作る。どの要素かを知っているのは、
// この台本だけである。記録の道具（browser-recorder.mjs）には、要素の指定を渡さない。
// 使い方: node tools/openelis-recorder-live.mjs --origin https://127.0.0.1:PORT --secrets FILE --base RECIPE --out NEW_RECIPE [--steps NEW_FILE]
//   --base  コンテナの設定を持つレシピ（examples/openelis-browser/register-recipe.json）。
//   --out   記録した手順で組んだレシピ。手で書くのは、結果を読む手順1つと、表示の判定条件1つだけ。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {chromium} from 'playwright';
import {record} from './browser-recorder.mjs';

const args = Object.fromEntries(process.argv.slice(2).reduce((list, arg, i, all) => arg.startsWith('--') ? [...list, [arg.slice(2), all[i + 1]]] : list, []));
assert(args.origin && args.secrets && args.base && args.out, '使い方: node tools/openelis-recorder-live.mjs --origin https://127.0.0.1:PORT --secrets FILE --base RECIPE --out NEW_RECIPE [--steps NEW_FILE]');
const secrets = JSON.parse(fs.readFileSync(args.secrets, 'utf8')), value = 'EEL-TEST-RECORDING';
const browser = await chromium.launch({channel: 'chromium', executablePath: process.env.EEL_BROWSER_EXECUTABLE || undefined});
const context = await browser.newContext({viewport: {width: 1440, height: 1000}, ignoreHTTPSErrors: true});
const recorder = await record(context, {origins: [new URL(args.origin).origin], secrets, testValue: value, runId: 'recording-openelis'});
const page = await context.newPage();
// 人がボタンを押している時間を置く。押し始めてから離すまでの間に、道具が押す前の画面を調べる。
const press = selector => page.locator(selector).click({delay: 150});
const type = async (selector, text) => { await page.locator(selector).click(); await page.keyboard.type(text); };
await page.goto(new URL('/', args.origin).href, {waitUntil: 'networkidle', timeout: 120000});
await type('#loginName', secrets.username);
await type('#password', secrets.password);
await page.getByRole('button', {name: 'Login', exact: true}).click({delay: 150});
await page.locator('#menu_home_nav').waitFor({timeout: 60000});
await press('#menu_patient');
await press('#menu_patient_add_or_edit_nav');
await press('#newPatient');
await type('#nationalId', value);
await type('#lastName', 'Yamada');
await type('#firstName', 'Taro');
await press('label[for=radio-1]');
await type('#years', '40');
// 保存の応答が届くのを見てから、次の操作へ進む。
const saved = page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname.endsWith('/rest/PatientManagement'), {timeout: 60000});
await press('#submit');
assert.equal((await saved).status(), 200);
await press('#searchPatient');
await type('#patientId', value);
await press('#local_search');
await page.locator('main table').filter({hasText: value}).waitFor({timeout: 60000});
const recorded = await recorder.stop();
await browser.close();
assert.ok(!JSON.stringify(recorded).includes(value) && !JSON.stringify(recorded).includes(secrets.password), '記録に、記録のときの試験値か秘密値が残っている');
if (args.steps) fs.writeFileSync(args.steps, JSON.stringify(recorded, null, 2), {flag: 'wx'});

// 記録した手順でレシピを組む。ログインの応答を待った手順までを準備、その後を操作にする。
const recipe = JSON.parse(fs.readFileSync(args.base, 'utf8'));
const login = recorded.actions.findIndex(s => s.wait_response && recorded.responses.find(w => w.id === s.wait_response)?.path.endsWith('/ValidateLogin'));
assert(login >= 0, 'ログインの応答を待つ手順が記録されていない');
recipe.browser.preparation = recorded.actions.slice(0, login + 1);
// 手で書く部分。記録の道具は、画面の表示を読む手順を作らない。
const read = {id: 'search-result', action: 'read_text', target: {frames: [], locator: {by: 'css', selector: 'main table'}}, contains: '{{test_value}}'};
recipe.browser.actions = [...recorded.actions.slice(login + 1), read];
recipe.browser.responses = recorded.responses;
recipe.browser.database = [];
recipe.browser.provenance = recorded.provenance;
recipe.trial.checks = [{id: 'patient-display', scope: '登録した患者の検索結果の表示', method: 'browser_text', target: read.id, expect: 'contains', text: '{{test_value}}'}];
fs.writeFileSync(args.out, JSON.stringify(recipe, null, 2) + '\n', {flag: 'wx'});
const by = kind => recorded.actions.filter(s => s.target.locator.by === kind).length;
console.log(JSON.stringify({steps: recorded.actions.length, by_label: by('label'), by_role: by('role'), by_css: by('css'), waited: recorded.responses.map(w => `${w.method} ${w.path}`), problems: recorded.problems, locators: recorded.actions.map(s => `${s.id}: ${s.action} ${JSON.stringify(s.target.locator)}`)}, null, 1));
