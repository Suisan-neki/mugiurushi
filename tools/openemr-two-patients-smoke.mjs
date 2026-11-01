// Runs a fixed OpenEMR recipe with synthetic patients; no real data or external environment.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync, execFileSync } from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..');
const bin = process.env.REVIEW_BINARY || path.join(root, 'target/debug/mugiurushi');
const recipe = JSON.parse(fs.readFileSync(path.join(root, 'examples/openemr-browser/soap-two-patients-recipe.json')));
const output = path.join(root, 'evaluations/ci-openemr-two-patients');
fs.mkdirSync(output, { recursive: true });
const repository = path.join(output, 'repository');
fs.mkdirSync(repository);
for (const args of [['init', repository], ['-C', repository, 'remote', 'add', 'origin', 'https://github.com/' + recipe.source.repository + '.git'], ['-C', repository, 'fetch', '--depth', '1', 'origin', recipe.source.commit]]) {
  execFileSync('git', args, { stdio: 'inherit' });
}
function cli(args) {
  const result = spawnSync(bin, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, timeout: 600000 });
  assert.equal(result.error, undefined, String(result.error));
  assert.equal(result.status, 0, (result.stderr || '').slice(-12000));
  return JSON.parse(result.stdout);
}
const caseDir = path.join(output, 'case');
const spec = { schema_version: 1, revision: 1, id: 'openemr-two-patients', title: '患者Aへの保存と患者Bへの影響', statements: [], context: {}, checks: [{ id: 'openemr', version: 1, text: '患者Aに保存し、患者Bの記録に変化がないことを確認する', origin: '架空データを使うデモの試験条件', source: recipe.source, rule: 'manual_only', dependencies: [], questions: [] }], evidence: [], decisions: [], history: [] };
const specFile = path.join(output, 'case-spec.json');
fs.writeFileSync(specFile, JSON.stringify(spec));
let view = cli(['case', 'init', caseDir, '--file', specFile]);
const change = path.join(output, 'configure.json');
fs.writeFileSync(change, JSON.stringify({ expected_revision: view.workspace.revision, actor: '合成の試験担当', reason: '試験条件を設定', action: { op: 'configure_observation', check_id: 'openemr', criterion: recipe.criterion } }));
view = cli(['case', 'apply', caseDir, change]);
const recipeFile = path.join(output, 'recipe.json');
fs.writeFileSync(recipeFile, JSON.stringify(recipe));
view = cli(['case', 'execute', caseDir, recipeFile, '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', '患者A・Bの保存を確認']);
const evidence = view.workspace.evidence.at(-1);
fs.writeFileSync(path.join(output, 'result.json'), JSON.stringify({ verdict: view.checks[0].verdict, evidence }, null, 2));
const trace = evidence.execution;
assert.equal(trace.containers.browser.prepared, true);
assert.equal(trace.containers.browser.completed, true);
assert.deepEqual(trace.containers.leftovers, []);
assert.equal(trace.containers.egress, 'blocked');
assert.equal(trace.containers.egress_confirmed, true);
assert(trace.containers.database_reads_before.some(read => read.id === 'soap-b' && read.performed && read.exit_code === 0 && read.text.trim()), JSON.stringify(trace.containers.database_reads_before));
for (const check of trace.trial.results) assert.equal(check.observed, 'pass', `${check.id}: ${check.detail}`);
assert.equal(view.checks[0].verdict, 'satisfied');
console.log('Synthetic OpenEMR: patient A save and patient B record before/after were checked.');
