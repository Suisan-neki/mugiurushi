// 実物のOpenEMRで、値の追跡を有効にしたSOAPの保存の試験を1回実行する。
// 基盤が見つけたDBの場所を、レシピに手で書いた読み出し先と比べ、候補と項目数を出す。
// Docker、固定版のChromium、試験用の認証ファイル（EEL_BROWSER_SECRETS_FILE）が必要。CIの通常の試験には含めない。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync, execFileSync} from 'node:child_process';
import {count} from './recipe-count.mjs';

const root = path.resolve(import.meta.dirname, '..'), bin = process.env.REVIEW_BINARY || path.join(root, 'target/debug/mugiurushi');
const perStep = process.argv.includes('--value-per-step');
const output = path.join(root, 'evaluations', perStep ? 'openemr-tracking-per-step' : 'openemr-tracking');
fs.rmSync(output, {recursive: true, force: true}); fs.mkdirSync(output, {recursive: true});
const recipe = JSON.parse(fs.readFileSync(path.join(root, 'examples/openemr-browser/soap-recipe.json')));
const repository = path.join(output, 'repository'); fs.mkdirSync(repository);
for (const args of [['init', '-q', repository], ['-C', repository, 'remote', 'add', 'origin', 'https://github.com/' + recipe.source.repository + '.git'], ['-C', repository, 'fetch', '-q', '--depth', '1', 'origin', recipe.source.commit]]) execFileSync('git', args, {stdio: 'inherit'});
function cli(args) {
  const r = spawnSync(bin, args, {encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 900000});
  assert.equal(r.error, undefined, String(r.error)); assert.equal(r.status, 0, (r.stderr || '').slice(-4000));
  return JSON.parse(r.stdout);
}
const caseDir = path.join(output, 'case');
const spec = {schema_version: 1, revision: 1, id: 'openemr-tracking', title: '架空の患者のSOAPの保存で、入力した値の行き先を記録する', statements: [], context: {}, evidence: [], decisions: [], history: [],
  checks: [{id: 'openemr', version: 1, text: 'SOAPの4項目を保存し、再表示とDBの値を照合できる', origin: '既存の試験条件', source: recipe.source, rule: 'manual_only', dependencies: [], questions: []}]};
const specFile = path.join(output, 'case-spec.json'); fs.writeFileSync(specFile, JSON.stringify(spec));
let view = cli(['case', 'init', caseDir, '--file', specFile]);
const configure = path.join(output, 'configure.json');
fs.writeFileSync(configure, JSON.stringify({expected_revision: view.workspace.revision, actor: '合成の試験担当', reason: '実行前に観測名と対象を保存', action: {op: 'configure_observation', check_id: 'openemr', criterion: recipe.criterion}}));
cli(['case', 'apply', caseDir, configure]);

// 既存のレシピに、追跡の指定だけを足す。手書きの読み出し先と判定条件はそのまま残し、比較の基準にする。
const tracked = structuredClone(recipe);
tracked.browser.track_values = true;
if (perStep) tracked.browser.value_per_step = true;
const file = path.join(output, 'recipe.json'); fs.writeFileSync(file, JSON.stringify(tracked, null, 2));
view = cli(['case', 'execute', caseDir, file, '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', '値の追跡']);
const evidence = view.workspace.evidence.at(-1), phase = evidence.execution.containers;
assert(phase.value_trace, '値の出現の記録がない');
const scan = phase.database_scan, before = phase.database_scan_before;
assert.equal(scan?.performed, true, scan?.detail);

// 手で書いた読み出し先（表と、読む列）と、基盤が見つけた場所を比べる。
const handwritten = [...new Set(recipe.browser.database.flatMap(p => [p.match_column, ...p.columns].map(c => `${p.database}.${p.table}.${c}`)))].sort();
const added = phase.value_trace.appearances.flatMap(a => a.database.map(h => ({slot: a.slot, place: `${h.schema}.${h.table}.${h.column}`, rows: h.rows, exact_rows: h.exact_rows})));
const places = [...new Set(added.map(h => h.place))].sort();
const draft = cli(['case', 'draft', caseDir, '--evidence', evidence.id]);
const summary = {
  verdict: view.checks.find(c => c.check_id === 'openemr')?.verdict,
  value_per_step: perStep,
  values: phase.value_trace.values.map(v => ({slot: v.slot, template: v.template})),
  scan: {before: before && {performed: before.performed, hits: before.hits.length}, tables: scan.tables_scanned, columns: scan.columns_scanned, truncated: scan.truncated, skipped: scan.skipped.length, elapsed_ms: scan.elapsed_ms, before_elapsed_ms: before?.elapsed_ms},
  handwritten_places: handwritten,
  found_places: places,
  handwritten_not_found: handwritten.filter(p => !places.includes(p)),
  found_not_handwritten: places.filter(p => !handwritten.includes(p)),
  exact_places: [...new Set(added.filter(h => h.exact_rows > 0).map(h => h.place))].sort(),
  requests_with_values: [...new Set(phase.value_trace.appearances.flatMap(a => a.requests.map(e => `${e.method} ${e.path} -> ${e.status}`)))].sort(),
  handwritten_urls: recipe.browser.responses.map(w => `${w.method} ${w.path}`).sort(),
  screens_with_values: [...new Set(phase.value_trace.appearances.flatMap(a => a.screens))],
  logs_with_values: [...new Set(phase.value_trace.appearances.flatMap(a => a.logs))],
  limits: phase.value_trace.limits,
  draft: {rules: draft.rules, database: draft.database.length, responses: draft.responses.length, checks: draft.checks.length, skipped: draft.skipped},
  handwritten_count: count(recipe).handwritten,
};
// 候補を採用したレシピで、もう一度実行する。手書きの読み出し先と判定条件は、候補に置き換える。
// 操作の待ち合わせに使っている応答の観測先（手順が参照するもの）は、操作の一部なので残す。
if (process.argv.includes('--adopt')) {
  const adopted = structuredClone(recipe);
  const waited = new Set([...adopted.browser.preparation, ...adopted.browser.actions].map(s => s.wait_response).filter(Boolean));
  const kept = adopted.browser.responses.filter(w => waited.has(w.id));
  const same = w => kept.find(k => k.method === w.method && k.path === w.path);
  const provenance = {database: draft.provenance.database, responses: {}, checks: draft.provenance.checks};
  // 同じ要求を指す候補は、残した観測先を使う。その観測先は手書きのまま数える。
  const target = Object.fromEntries(draft.responses.map(w => [w.id, same(w)?.id || w.id]));
  for (const w of draft.responses) if (!same(w)) { kept.push(w); provenance.responses[w.id] = draft.provenance.responses[w.id]; }
  adopted.browser.responses = kept;
  adopted.browser.database = draft.database;
  adopted.browser.provenance = provenance;
  adopted.trial.checks = draft.checks.map(c => c.method === 'browser_response' ? {...c, target: target[c.target]} : c);
  const adoptedFile = path.join(output, 'adopted-recipe.json'); fs.writeFileSync(adoptedFile, JSON.stringify(adopted, null, 2));
  const before = view.workspace.evidence.length;
  view = cli(['case', 'execute', caseDir, adoptedFile, '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', '候補を採用したレシピ']);
  assert.equal(view.workspace.evidence.length, before + 1);
  const run = view.workspace.evidence.at(-1);
  summary.adopted = {
    count: count(adopted),
    results: run.execution.trial.results.map(r => `${r.id}: ${r.observed}`),
    verdict: view.checks.find(c => c.check_id === 'openemr')?.verdict,
  };
}
fs.writeFileSync(path.join(output, 'summary.json'), JSON.stringify(summary, null, 1));
fs.writeFileSync(path.join(output, 'draft.json'), JSON.stringify(draft, null, 1));
console.log(JSON.stringify(summary, null, 1));
