// レシピを1つ受け取り、事例を作って麦漆の実行器で1回実行する。対象ごとの処理は持たない。
// 使い方: node tools/recipe-live.mjs <レシピ> <出力先の名前> [--track | --track-only] [--command=<サービス名>=<JSONの配列>] [--limit-ms=<ミリ秒>]
//   --track       入力した値を追跡し、入力の手順ごとに異なる値を使う。等値で書いた手書きの判定条件は一致しなくなる。
//   --track-only  入力した値を追跡するが、値は手順ごとに変えない。手書きの判定条件もそのまま評価する。
//   --command     実行する環境の都合で、サービスの起動コマンドをこの1回だけ置き換える。レシピのファイルは変えない。
//   --adopt       追跡した試行の候補（DBの読み出し先、応答の観測先、判定条件）をレシピへ取り込み、もう一度実行する。
//                 取り込んだレシピは evaluations/<出力先の名前>/adopted-recipe.json に残す。
//   --limit-ms    実行器を待つ上限。省略すると、レシピ自身の待機の上限（各コンテナの準備、復旧後の準備、操作）から決める。
//                 上限に達したら実行器を止め、その試行のコンテナ・ネットワーク・ボリュームを回収する。
// 結果は evaluations/<出力先の名前>/summary.json に残す。ログインの値は EEL_BROWSER_SECRETS_FILE で渡す。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync, execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {count} from './recipe-count.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..'), bin = process.env.REVIEW_BINARY || path.join(root, 'target/debug/mugiurushi');
const [recipeFile, name, ...flags] = process.argv.slice(2);
assert(recipeFile && /^[a-z0-9][a-z0-9-]{0,63}$/.test(name || ''), '使い方: node tools/recipe-live.mjs <レシピ> <出力先の名前（英小文字・数字・ハイフン）> [--track | --track-only] [--command=<サービス名>=<JSONの配列>] [--limit-ms=<ミリ秒>]');
const output = path.join(root, 'evaluations', name);
fs.rmSync(output, {recursive: true, force: true}); fs.mkdirSync(output, {recursive: true});
const recipe = JSON.parse(fs.readFileSync(recipeFile));
if (flags.includes('--track')) { recipe.browser.track_values = true; recipe.browser.value_per_step = true; }
// 手書きの判定条件も成立させたい場合は、手順ごとの値を使わずに追跡だけを有効にする。
// 入力のレシピが手順ごとの値を指定していても、外す。残すと、等値で書いた判定条件が一致しなくなる。
if (flags.includes('--track-only')) { recipe.browser.track_values = true; delete recipe.browser.value_per_step; }
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
// 実行器を待つ上限は、レシピ自身の待機の上限から決める。正しい設定の試行を、この道具が先に止めないためである。
// コンテナは1つずつ準備を待つので、準備の上限は合計する。
const services = recipe.containers?.services || [];
const waits = services.reduce((sum, s) => sum + s.ready_timeout_ms, 0) + (recipe.trial?.recovery ? recipe.trial.recovery.ready_timeout_ms * services.length : 0);
const limitFlag = flags.find(f => f.startsWith('--limit-ms='));
const limit = limitFlag ? Number(limitFlag.slice('--limit-ms='.length)) : waits + 4 * recipe.timeout_ms + 15 * 60 * 1000;
assert(Number.isInteger(limit) && limit > 0, '--limit-ms は正の整数（ミリ秒）で指定してください');
// 止めた試行の資源を、試行のラベルで探して消す。実行器は、止められたときに後始末をしない。
// この実行で作った試行のラベルだけを対象にし、ほかの試行の資源には触れない。
function collect() {
  const runs = path.join(output, 'case', 'runs');
  const prefixes = (fs.existsSync(runs) ? fs.readdirSync(runs) : []).map(n => n.replace(/^run-/, '').replace(/[^A-Za-z0-9-]/g, '-').slice(0, 40) + '-');
  const docker = args => spawnSync('docker', args, {encoding: 'utf8'});
  const listed = (args, key) => (docker([...args, '--filter', 'label=eel.managed=1', '--format', `{{.${key}}}\t{{.Label "eel.trial"}}`]).stdout || '').split('\n').filter(Boolean).map(line => line.split('\t')).filter(([, trial]) => prefixes.some(p => (trial || '').startsWith(p))).map(([id]) => id);
  const find = () => ({containers: listed(['ps', '-a'], 'ID'), networks: listed(['network', 'ls'], 'ID'), volumes: listed(['volume', 'ls'], 'Name')});
  const found = find();
  for (const id of found.containers) docker(['rm', '-f', '-v', id]);
  for (const id of found.networks) docker(['network', 'rm', id]);
  for (const volume of found.volumes) docker(['volume', 'rm', volume]);
  return {removed: found, remaining: find()};
}
function cli(args) {
  const r = spawnSync(bin, args, {encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, timeout: limit});
  if (r.error?.code === 'ETIMEDOUT') {
    console.log(`実行器が ${limit}ms 以内に終わらなかったので止めた。その試行の資源を回収する`);
    console.log(JSON.stringify(collect(), null, 1));
    process.exit(1);
  }
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
  count: count(recipe),
};
if (phase?.value_trace) {
  const draft = cli(['case', 'draft', caseDir, '--evidence', evidence.id]);
  summary.draft = {rules: draft.rules, database: draft.database.map(p => `${p.database}.${p.schema ? p.schema + '.' : ''}${p.table}.${p.match_column} = ${p.match_value}`), responses: draft.responses.map(w => `${w.method} ${w.path}`), checks: draft.checks.map(c => `${c.id}: ${c.method} ${c.expect} ${c.text}`), skipped: draft.skipped};
  fs.writeFileSync(path.join(output, 'draft.json'), JSON.stringify(draft, null, 1));
  if (flags.includes('--adopt')) {
    // 候補を取り込む。DBの読み出し先は候補に置き換える。応答の観測先は、同じ要求を指すものがレシピにあればそれを使い、
    // なければ足す。判定条件は、候補が作る種類（DBの行、HTTPの状態、コンテナのログ）を候補に置き換え、
    // ほかの種類（画面の表示など）はレシピのものを残す。
    const adopted = structuredClone(recipe), browser = adopted.browser;
    const same = w => browser.responses.find(k => k.method === w.method && k.path === w.path);
    const target = Object.fromEntries(draft.responses.map(w => [w.id, same(w)?.id || w.id]));
    const provenance = {...(browser.provenance || {})};
    provenance.responses = {...(provenance.responses || {})};
    for (const w of draft.responses) if (!same(w)) { browser.responses.push(w); provenance.responses[w.id] = draft.provenance.responses[w.id]; }
    browser.database = draft.database;
    provenance.database = draft.provenance.database;
    const generated = ['database_rows', 'browser_response', 'container_log'];
    const kept = adopted.trial.checks.filter(c => !generated.includes(c.method));
    adopted.trial.checks = [...kept, ...draft.checks.map(c => c.method === 'browser_response' ? {...c, target: target[c.target]} : c)];
    provenance.checks = draft.provenance.checks;
    browser.provenance = provenance;
    if (flags.includes('--track')) { delete browser.track_values; delete browser.value_per_step; }
    const adoptedFile = path.join(output, 'adopted-recipe.json'); fs.writeFileSync(adoptedFile, JSON.stringify(adopted, null, 2));
    view = cli(['case', 'execute', caseDir, adoptedFile, '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', name + '（候補を取り込んだレシピ）']);
    const run = view.workspace.evidence.at(-1);
    summary.adopted = {
      count: count(adopted),
      verdict: view.checks.find(c => c.check_id === recipe.check_id)?.verdict,
      completed: run.execution.containers?.browser?.completed,
      results: run.execution.trial?.results?.map(r => `${r.id}: ${r.observed} ${String(r.detail || '').slice(0, 120)}`),
      leftovers: run.execution.containers?.leftovers,
    };
  }
}
fs.writeFileSync(path.join(output, 'summary.json'), JSON.stringify(summary, null, 1));
console.log(JSON.stringify(summary, null, 1));
