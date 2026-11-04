// 実OpenEMRで、同梱の「OpenEMR：SOAPの保存でログに試験値が出ないか」を1回実行し、記録を残す。
// 判定は麦漆の結果をそのまま保存し、このスクリプトでは合否を決めない。Dockerと固定版のChromiumが必要。
import assert from 'node:assert/strict';
import fs from 'node:fs'; import path from 'node:path';
import {spawn, spawnSync, execFileSync} from 'node:child_process';
const root = path.resolve(import.meta.dirname, '..');
const bin = process.env.REVIEW_BINARY || path.join(root, 'target/debug/mugiurushi'), out = process.env.OUT || path.join(root, 'evaluations/live-openemr-logs'); fs.mkdirSync(out, {recursive: true});
// 1. 同梱の一覧を、そのバイナリのappから取り出す（手で作り直さない）。
const data = path.join(out, 'app-data'); fs.mkdirSync(data);
const app = spawn(bin, ['app', data, '--port', '0'], {stdio: ['ignore', 'pipe', 'pipe']});
const url = await new Promise((ok, no) => { let s = ''; const t = setTimeout(() => no(Error('app起動待ち')), 15000);
  app.stdout.on('data', d => { s += d; const m = /http:\/\/127\.0\.0\.1:\d+/.exec(s); if (m) { clearTimeout(t); ok(m[0]); } }); });
const html = await (await fetch(url + '/workbench')).text();
const token = /name="review-token" content="([^"]+)"/.exec(html)[1];
const catalog = await (await fetch(url + '/api/v1/browser-recipes', {headers: {'X-Review-Token': token}})).json();
app.kill('SIGTERM');
const entry = catalog.recipes.find(r => r.id === 'openemr-soap-logs'); assert(entry, '同梱一覧にログの試験がない');
const recipe = entry.recipe; fs.writeFileSync(path.join(out, 'recipe.json'), JSON.stringify(recipe, null, 2));
// 2. レシピが参照するOpenEMRのコミットを取得する。
const repository = path.join(out, 'repository'); fs.mkdirSync(repository);
for (const args of [['init', '-q', repository], ['-C', repository, 'remote', 'add', 'origin', 'https://github.com/' + recipe.source.repository + '.git'], ['-C', repository, 'fetch', '-q', '--depth', '1', 'origin', recipe.source.commit]]) execFileSync('git', args, {stdio: 'inherit'});
const cli = args => { const r = spawnSync(bin, args, {encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, timeout: 1500000});
  fs.appendFileSync(path.join(out, 'cli-stderr.log'), (r.stderr || ''));
  assert.equal(r.status, 0, (r.stderr || String(r.error)).slice(-8000)); return JSON.parse(r.stdout); };
// 3. 事例を作り、観測名を設定して実行する。
const caseDir = path.join(out, 'case');
const spec = {schema_version: 1, revision: 1, id: 'openemr-soap-logs', title: '架空の患者のSOAP保存とログ', statements: [], context: {}, evidence: [], decisions: [], history: [],
  checks: [{id: 'openemr', version: 1, text: 'SOAPを保存し、アプリとDBのコンテナのログに試験値が出ない', origin: '合成の試験条件（実利用者の要件ではない）', source: recipe.source, rule: 'manual_only', dependencies: [], questions: []}]};
fs.writeFileSync(path.join(out, 'case-spec.json'), JSON.stringify(spec));
let view = cli(['case', 'init', caseDir, '--file', path.join(out, 'case-spec.json')]);
fs.writeFileSync(path.join(out, 'configure.json'), JSON.stringify({expected_revision: view.workspace.revision, actor: '合成の試験担当', reason: '観測名と対象を保存', action: {op: 'configure_observation', check_id: 'openemr', criterion: recipe.criterion}}));
view = cli(['case', 'apply', caseDir, path.join(out, 'configure.json')]);
const started = Date.now();
view = cli(['case', 'execute', caseDir, path.join(out, 'recipe.json'), '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', 'PR #205 live']);
const e = view.workspace.evidence.at(-1), trace = e.execution, phase = trace.containers;
const summary = {
  elapsed_s: Math.round((Date.now() - started) / 1000), run_id: e.manifest.correlation.run_id, evidence_id: e.id,
  source: recipe.source, verdict: view.checks[0].verdict, reason: view.checks[0].reason,
  browser: {prepared: phase.browser?.prepared, completed: phase.browser?.completed, version: phase.browser?.version},
  results: trace.trial.results.map(r => ({id: r.id, observed: r.observed, detail: r.detail})),
  services: phase.services.map(s => ({name: s.name, image: s.image, digest: s.digest, log_note: s.log_note, log_excerpt_chars: [...s.log_excerpt].length})),
  database_reads: (phase.database_reads || []).map(q => ({id: q.id, performed: q.performed, exit_code: q.exit_code, rows: q.text.trim() ? q.text.trim().split('\n').length : 0})),
  egress: phase.egress, egress_confirmed: phase.egress_confirmed, leftovers: phase.leftovers,
};
fs.writeFileSync(path.join(out, 'evidence.json'), JSON.stringify(e, null, 2));
fs.writeFileSync(path.join(out, 'summary.json'), JSON.stringify(summary, null, 2));
console.log(JSON.stringify(summary, null, 2));
