// レシピを1つ受け取り、事例を作って麦漆の実行器で1回実行する。対象ごとの処理は持たない。
// 使い方: node tools/recipe-live.mjs <レシピ> <出力先の名前> [--track | --track-only] [--command=<サービス名>=<JSONの配列>]
//   --track       入力した値を追跡し、入力の手順ごとに異なる値を使う。等値で書いた手書きの判定条件は一致しなくなる。
//   --track-only  入力した値を追跡するが、値は手順ごとに変えない。手書きの判定条件もそのまま評価する。
//   --command     実行する環境の都合で、サービスの起動コマンドをこの1回だけ置き換える。レシピのファイルは変えない。
// 結果は evaluations/<出力先の名前>/summary.json に残す。ログインの値は EEL_BROWSER_SECRETS_FILE で渡す。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync, execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..'), bin = process.env.REVIEW_BINARY || path.join(root, 'target/debug/mugiurushi');
const [recipeFile, name, ...flags] = process.argv.slice(2);
assert(recipeFile && /^[a-z0-9][a-z0-9-]{0,63}$/.test(name || ''), '使い方: node tools/recipe-live.mjs <レシピ> <出力先の名前（英小文字・数字・ハイフン）> [--track | --track-only] [--command=<サービス名>=<JSONの配列>]');
const output = path.join(root, 'evaluations', name);
fs.rmSync(output, {recursive: true, force: true}); fs.mkdirSync(output, {recursive: true});
const recipe = JSON.parse(fs.readFileSync(recipeFile));
if (flags.includes('--track')) { recipe.browser.track_values = true; recipe.browser.value_per_step = true; }
// 手書きの判定条件も成立させたい場合は、手順ごとの値を使わずに追跡だけを有効にする。
if (flags.includes('--track-only')) recipe.browser.track_values = true;
const replaced = [];
for (const flag of flags.filter(f => f.startsWith('--command='))) {
  const rest = flag.slice('--command='.length), at = rest.indexOf('=');
  const service = recipe.containers.services.find(s => s.name === rest.slice(0, at));
  assert(at > 0 && service, `--command のサービス名がレシピにありません: ${rest.slice(0, at)}`);
  service.command = JSON.parse(rest.slice(at + 1));
  assert(Array.isArray(service.command) && service.command.every(a => typeof a === 'string'), '--command の値は文字列の配列にしてください');
  replaced.push(service.name);
}
const repository = path.join(output, 'repository'); fs.mkdirSync(repository);
for (const args of [['init', '-q', repository], ['-C', repository, 'remote', 'add', 'origin', 'https://github.com/' + recipe.source.repository + '.git'], ['-C', repository, 'fetch', '-q', '--depth', '1', 'origin', recipe.source.commit]]) execFileSync('git', args, {stdio: 'inherit'});
function cli(args) {
  const r = spawnSync(bin, args, {encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, timeout: 3600000});
  if (r.status !== 0) { console.log('実行器の終了コード', r.status); console.log((r.stderr || '').slice(-3000)); process.exit(1); }
  return JSON.parse(r.stdout);
}
const caseDir = path.join(output, 'case');
const spec = {schema_version: 1, revision: 1, id: name, title: `${recipe.check_id} のレシピを1回実行する`, statements: [], context: {}, evidence: [], decisions: [], history: [],
  checks: [{id: recipe.check_id, version: 1, text: 'レシピの操作を実行し、判定条件を照合できる', origin: '試験条件', source: recipe.source, rule: 'manual_only', dependencies: [], questions: []}]};
fs.writeFileSync(path.join(output, 'case-spec.json'), JSON.stringify(spec));
let view = cli(['case', 'init', caseDir, '--file', path.join(output, 'case-spec.json')]);
fs.writeFileSync(path.join(output, 'configure.json'), JSON.stringify({expected_revision: view.workspace.revision, actor: '合成の試験担当', reason: '実行前に観測名と対象を保存', action: {op: 'configure_observation', check_id: recipe.check_id, criterion: recipe.criterion}}));
cli(['case', 'apply', caseDir, path.join(output, 'configure.json')]);
const file = path.join(output, 'recipe.json'); fs.writeFileSync(file, JSON.stringify(recipe, null, 2));
const started = Date.now();
view = cli(['case', 'execute', caseDir, file, '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', name]);
const evidence = view.workspace.evidence.at(-1), phase = evidence.execution.containers, trial = evidence.execution.trial;
const summary = {
  recipe: path.relative(root, path.resolve(recipeFile)),
  flags: flags.filter(f => !f.startsWith('--command=')),
  command_replaced: replaced,
  elapsed_s: Math.round((Date.now() - started) / 1000),
  verdict: view.checks.find(c => c.check_id === recipe.check_id)?.verdict,
  services: phase?.services.map(s => `${s.name}: ready=${s.ready} ${s.ready_after_ms ?? ''}ms ${String(s.detail || '').slice(0, 160)}`),
  image_volumes: phase?.image_volumes?.length ?? 0,
  leftovers: phase?.leftovers,
  browser: phase?.browser && {prepared: phase.browser.prepared, completed: phase.browser.completed, blocked_requests: phase.browser.blocked_requests, steps: phase.browser.steps.map(s => `${s.phase}:${s.id} ${s.completed ? 'ok' : 'NG ' + String(s.detail || '').slice(0, 200)}`), note: phase.browser.note},
  results: trial?.results?.map(r => `${r.id}: ${r.observed} ${String(r.detail || '').slice(0, 120)}`),
  database_reads: phase?.database_reads?.map(r => `${r.id}: performed=${r.performed} ${String(r.text || r.detail).slice(0, 120)}`),
  scan: phase?.database_scan && {performed: phase.database_scan.performed, tables: phase.database_scan.tables_scanned, columns: phase.database_scan.columns_scanned, listed: phase.database_scan.tables_listed, stopped: phase.database_scan.stopped ?? null, ms: phase.database_scan.elapsed_ms, detail: phase.database_scan.detail.slice(0, 200)},
  appearances: phase?.value_trace?.appearances.map(a => ({slot: a.slot, database: a.database.map(h => `${h.schema}.${h.table}.${h.column} rows=${h.rows} exact=${h.exact_rows}${h.baseline_unknown ? ' 前の探索なし' : ''}`), requests: a.requests.map(e => `${e.method} ${e.path} -> ${e.status}`), screens: a.screens.length, logs: a.logs})),
  limits: phase?.value_trace?.limits,
  evidence_id: evidence.id,
};
if (phase?.value_trace) {
  const draft = cli(['case', 'draft', caseDir, '--evidence', evidence.id]);
  summary.draft = {rules: draft.rules, database: draft.database.map(p => `${p.database}.${p.schema ? p.schema + '.' : ''}${p.table}.${p.match_column} = ${p.match_value}`), responses: draft.responses.map(w => `${w.method} ${w.path}`), checks: draft.checks.map(c => `${c.id}: ${c.method} ${c.expect} ${c.text}`), skipped: draft.skipped};
  fs.writeFileSync(path.join(output, 'draft.json'), JSON.stringify(draft, null, 1));
}
fs.writeFileSync(path.join(output, 'summary.json'), JSON.stringify(summary, null, 1));
console.log(JSON.stringify(summary, null, 1));
