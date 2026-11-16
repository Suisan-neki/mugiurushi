// 候補の取り込みの試験。架空の候補を、実在のレシピの形へ取り込む。コンテナは起動しない。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import {adopt} from './recipe-adopt.mjs';

const root = path.resolve(import.meta.dirname, '..');
const read = name => JSON.parse(fs.readFileSync(path.join(root, 'examples', name), 'utf8'));
const basis = {run_id: 'run-1', rules: 3, source: 'value_trace'};
// 架空の候補。読み出し先1件、応答の観測先2件（1件はレシピにある要求と同じ）、判定条件3件。
const draftFor = (recipe, value = '{{test_value}}') => {
  const known = recipe.browser.responses?.[0];
  const responses = [
    ...(known ? [{id: 'http-known', method: known.method, path: known.path}] : []),
    {id: 'http-new', method: 'POST', path: '/synthetic/new'},
  ];
  return {
    rules: 3, run_id: 'run-1',
    database: [{id: 'db-found', service: 'db', database: 'records', table: 'visit', match_column: 'note', columns: ['note'], match_value: value}],
    responses,
    checks: [
      {id: 'db-found', scope: '架空の保存', method: 'database_rows', target: 'db-found', expect: 'contains', text: value},
      ...responses.map(w => ({id: w.id, scope: '架空の応答', method: 'browser_response', target: w.id, expect: 'contains', text: 'status=200'})),
    ],
    provenance: {database: {'db-found': basis}, responses: Object.fromEntries(responses.map(w => [w.id, basis])), checks: {'db-found': basis, ...Object.fromEntries(responses.map(w => [w.id, basis]))}},
    skipped: [],
  };
};
// 取り込んだレシピの中で、参照が切れていないこと。基盤のレシピの検査が拒否する形を、ここで確かめる。
function references(recipe) {
  const probes = new Set(recipe.browser.database.map(p => p.id)), watches = new Set(recipe.browser.responses.map(w => w.id));
  for (const check of recipe.trial.checks) {
    if (check.method === 'database_rows') assert.ok(probes.has(check.target), `${check.id}: 読み出し先 ${check.target} がない`);
    if (check.method === 'browser_response') assert.ok(watches.has(check.target), `${check.id}: 応答の観測先 ${check.target} がない`);
    if (check.method === 'database_relation') for (const id of [check.relation.left, check.relation.right]) assert.ok(probes.has(id), `${check.id}: 読み出し先 ${id} がない`);
  }
  const ids = recipe.trial.checks.map(c => c.id);
  assert.equal(new Set(ids).size, ids.length, '判定条件のIDが重複している');
  for (const id of Object.keys(recipe.browser.provenance.database || {})) assert.ok(probes.has(id), `出所の記録が、ない読み出し先 ${id} を指す`);
  for (const id of Object.keys(recipe.browser.provenance.responses || {})) assert.ok(watches.has(id), `出所の記録が、ない観測先 ${id} を指す`);
}

test('DBの行どうしの対応の条件を残すとき、その条件が参照する読み出し先も残す', () => {
  const recipe = read('openemr-browser/soap-recipe.json');
  const relations = recipe.trial.checks.filter(c => c.method === 'database_relation');
  assert.ok(relations.length > 0, 'この試験は、対応の条件を持つレシピを使う');
  const adopted = adopt(recipe, draftFor(recipe));
  references(adopted);
  const needed = [...new Set(relations.flatMap(c => [c.relation.left, c.relation.right]))].sort();
  const probes = adopted.browser.database.map(p => p.id);
  // 参照される手書きの読み出し先と、候補の読み出し先だけが残る。参照されない手書きの読み出し先は、候補に置き換わる。
  assert.deepEqual([...probes].sort(), [...needed, 'db-found'].sort());
  assert.deepEqual(Object.keys(adopted.browser.provenance.database), ['db-found']);
  // DBの行の条件は候補に置き換わり、対応の条件と画面の表示の条件は残る。
  assert.deepEqual(adopted.trial.checks.filter(c => c.method === 'database_rows').map(c => c.id), ['db-found']);
  assert.equal(adopted.trial.checks.filter(c => c.method === 'database_relation').length, relations.length);
  assert.equal(adopted.trial.checks.filter(c => c.method === 'browser_text').length, recipe.trial.checks.filter(c => c.method === 'browser_text').length);
  // もとのレシピは変えない。
  assert.deepEqual(recipe, read('openemr-browser/soap-recipe.json'));
});

test('同じ要求を指す応答の観測先は、レシピのものを使い、ない要求だけを足す', () => {
  const recipe = read('openemr-browser/soap-recipe.json'), known = recipe.browser.responses[0];
  const adopted = adopt(recipe, draftFor(recipe));
  references(adopted);
  assert.equal(adopted.browser.responses.filter(w => w.method === known.method && w.path === known.path).length, 1);
  assert.equal(adopted.trial.checks.find(c => c.id === 'http-known').target, known.id);
  assert.ok(adopted.browser.responses.some(w => w.id === 'http-new'));
  assert.deepEqual(Object.keys(adopted.browser.provenance.responses), ['http-new']);
});

test('応答の観測先とDBの読み出し先を省略したレシピにも取り込める', () => {
  const recipe = read('openelis-browser/recorded-recipe.json');
  delete recipe.browser.responses; delete recipe.browser.database;
  for (const step of [...recipe.browser.preparation, ...recipe.browser.actions]) delete step.wait_response;
  delete recipe.browser.provenance.responses;
  const adopted = adopt(recipe, draftFor(recipe));
  references(adopted);
  assert.deepEqual(adopted.browser.responses.map(w => w.id), ['http-new']);
  assert.deepEqual(adopted.browser.database.map(p => p.id), ['db-found']);
  // 記録の出所は残る。
  assert.deepEqual(adopted.browser.provenance.steps, recipe.browser.provenance.steps);
});

test('値の割り当ての指定は、候補のもとになった試行のままにする', () => {
  const recipe = read('openelis-browser/recorded-recipe.json');
  recipe.browser.track_values = true; recipe.browser.value_per_step = true;
  // 手順ごとの値で作った候補は、読み出しと判定条件に手順ごとの値を持つ。
  const adopted = adopt(recipe, draftFor(recipe, '{{test_value}}-01'));
  references(adopted);
  assert.equal(adopted.browser.track_values, true);
  assert.equal(adopted.browser.value_per_step, true);
  assert.equal(adopted.browser.database[0].match_value, '{{test_value}}-01');
  // 指定のないレシピに、指定を足さない。
  const plain = adopt(read('openelis-browser/recorded-recipe.json'), draftFor(recipe));
  assert.ok(!('value_per_step' in plain.browser) && !('track_values' in plain.browser));
});
