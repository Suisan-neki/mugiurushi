// 実物のOpenEMRで、対象とDBの間に文を記録する中継を置き、次の2つを確かめる。
//   1. 同じ試験を2回実行して、書き込みとコミットの並びが一致する割合を出す。
//   2. SOAPの保存の書き込みを選び、3つの時点で接続を切って、操作の完了とDBの状態を比べる。
// Docker、固定版のChromium、試験用の認証ファイル（EEL_BROWSER_SECRETS_FILE）が必要。CIの通常の試験には含めない。
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawnSync, execFileSync} from 'node:child_process';

const root = path.resolve(import.meta.dirname, '..'), bin = process.env.REVIEW_BINARY || path.join(root, 'target/debug/mugiurushi');
const output = path.join(root, 'evaluations', 'openemr-relay');
// --reuse を付けると、保存済みの5回の試行（記録2回、切断3回）から、要約だけを作り直す。
const reuse = process.argv.includes('--reuse');
if (!reuse) { fs.rmSync(output, {recursive: true, force: true}); fs.mkdirSync(output, {recursive: true}); }
const base = JSON.parse(fs.readFileSync(path.join(root, 'examples/openemr-browser/soap-recipe.json')));
const repository = path.join(output, 'repository');
if (!reuse) fs.mkdirSync(repository);
for (const args of reuse ? [] : [['init', '-q', repository], ['-C', repository, 'remote', 'add', 'origin', 'https://github.com/' + base.source.repository + '.git'], ['-C', repository, 'fetch', '-q', '--depth', '1', 'origin', base.source.commit]]) execFileSync('git', args, {stdio: 'inherit'});
function cli(args) {
  const r = spawnSync(bin, args, {encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, timeout: 900000});
  assert.equal(r.error, undefined, String(r.error)); assert.equal(r.status, 0, (r.stderr || '').slice(-4000));
  return JSON.parse(r.stdout);
}
const caseDir = path.join(output, 'case');
const spec = {schema_version: 1, revision: 1, id: 'openemr-relay', title: '架空の患者のSOAPの保存で、DBへ送った文を記録する', statements: [], context: {}, evidence: [], decisions: [], history: [],
  checks: [{id: 'openemr', version: 1, text: 'SOAPの4項目を保存し、再表示とDBの値を照合できる', origin: '既存の試験条件', source: base.source, rule: 'manual_only', dependencies: [], questions: []}]};
let view;
if (reuse) view = cli(['case', 'show', caseDir]);
else {
  fs.writeFileSync(path.join(output, 'case-spec.json'), JSON.stringify(spec));
  view = cli(['case', 'init', caseDir, '--file', path.join(output, 'case-spec.json')]);
  fs.writeFileSync(path.join(output, 'configure.json'), JSON.stringify({expected_revision: view.workspace.revision, actor: '合成の試験担当', reason: '実行前に観測名と対象を保存', action: {op: 'configure_observation', check_id: 'openemr', criterion: base.criterion}}));
  cli(['case', 'apply', caseDir, path.join(output, 'configure.json')]);
}
let reused = 0;

// 既存のレシピに、中継と値の追跡の指定だけを足す。入力の手順ごとに異なる値を使い、文と欄を対応づける。
function recipe(cut) {
  const r = structuredClone(base);
  r.browser.track_values = true; r.browser.value_per_step = true;
  r.containers.relay = {service: 'mysql', protocol: 'mysql', ...(cut ? {cut} : {})};
  if (cut) r.trial.fault = 'relay_cut';
  return r;
}
function execute(label, cut) {
  let evidence;
  if (reuse) { evidence = view.workspace.evidence[reused++]; assert(evidence, '保存済みの試行が足りない'); }
  else {
    const file = path.join(output, label + '-recipe.json'); fs.writeFileSync(file, JSON.stringify(recipe(cut), null, 2));
    view = cli(['case', 'execute', caseDir, file, '--repository-root', repository, '--mode', 'local', '--actor', '合成の試験担当', '--reason', label]);
    evidence = view.workspace.evidence.at(-1);
  }
  return {evidence, phase: evidence.execution.containers, trial: evidence.execution.trial};
}
const key = s => `${s.class}:${s.table ?? ''}:${s.shape}#${s.occurrence}`;
const soapSlots = ['fill-subjective', 'fill-objective', 'fill-assessment', 'fill-plan'];
const places = phase => [...new Set((phase.value_trace?.appearances || []).filter(a => soapSlots.includes(a.slot)).flatMap(a => a.database.map(h => `${h.table}.${h.column}`)))].sort();

// 1. 並びの安定性。
const first = execute('record-1'), second = execute('record-2');
for (const run of [first, second]) assert(run.phase.statements?.length, '文の記録がない: ' + run.phase.statements_note);
const points = run => run.phase.statements.filter(s => ['insert', 'update', 'delete', 'replace', 'commit'].includes(s.class));
const [a, b] = [points(first).map(key), points(second).map(key)];
const common = a.filter(k => b.includes(k));
const withValues = run => run.phase.statements.filter(s => s.slots?.length);
const stability = {
  statements: [first.phase.statements.length, second.phase.statements.length],
  write_and_commit_points: [a.length, b.length],
  same_keys: common.length,
  same_key_ratio: Number((common.length / Math.max(a.length, b.length)).toFixed(3)),
  same_order: JSON.stringify(a) === JSON.stringify(b),
  // 接続ごとに見ると、順番も一致するか。接続をまたいだ順番は、並行する要求の前後で変わりうる。
  same_sets_sorted: JSON.stringify([...a].sort()) === JSON.stringify([...b].sort()),
  only_in_first: a.filter(k => !b.includes(k)).length,
  only_in_second: b.filter(k => !a.includes(k)).length,
  classes: Object.fromEntries(['select', 'insert', 'update', 'delete', 'replace', 'begin', 'commit', 'rollback', 'set', 'ddl', 'other'].map(c => [c, first.phase.statements.filter(s => s.class === c).length])),
  commit_points: first.phase.statements.filter(s => s.commit_point).length,
  statements_with_tracked_values: [withValues(first).map(s => `${s.class} ${s.table} [${s.slots.join(',')}]`), withValues(second).map(key).every(k => withValues(first).map(key).includes(k))],
  note: first.phase.statements_note,
};
const baselinePlaces = places(first.phase);
assert(baselinePlaces.length, '通常時の試行で、SOAPの入力がDBに見つかっていない');

// 2. SOAPの入力を含む書き込みを選び、3つの時点で切る。
const target = first.phase.statements.find(s => s.class === 'insert' && s.slots?.some(slot => soapSlots.includes(slot)));
assert(target, 'SOAPの入力を含む書き込みが記録にない');
// 通常時に、同じ接続（同じ要求の処理）で、この書き込みの後に続く書き込み。切断した試行では送られない。
const following = first.phase.statements.filter(s => s.connection === target.connection && s.ordinal > target.ordinal && ['insert', 'update', 'delete', 'replace'].includes(s.class));
const cuts = [];
for (const phase of ['before_forward', 'after_forward_before_reply', 'after_reply']) {
  const run = execute('cut-' + phase, {shape: target.shape, occurrence: target.occurrence, phase});
  const browser = run.phase.browser, stopped = browser.steps.find(s => s.phase === 'action' && !s.completed);
  const stored = places(run.phase), cut = run.phase.statements.find(s => s.cut);
  // 操作が最後まで完了したかを、対象が成功として進んだかの目安にする。対象の文言からは推定しない。
  const all = stored.length === baselinePlaces.length;
  const classification = stored.length && !all ? '入力した値の一部だけが保存された'
    : browser.completed && !stored.length ? '操作は完了したが、保存されていない'
    : !browser.completed && all ? '操作は未完了だが、保存されている'
    : browser.completed ? '整合（操作は完了し、保存されている）' : '整合（操作は未完了で、保存されていない）';
  // 切断した試行で、通常時に後へ続くはずの書き込みが送られたか。
  const sentKeys = new Set(run.phase.statements.map(s => `${s.shape}#${s.occurrence}`));
  const missing = following.filter(s => !sentKeys.has(`${s.shape}#${s.occurrence}`));
  cuts.push({phase, fault_effective: run.trial.fault_effective, fault_applied: run.trial.fault_applied,
    cut_statement: cut && {class: cut.class, table: cut.table, reply_seen_by_relay: cut.reply, commit_point: cut.commit_point},
    operation_completed: browser.completed, stopped_at: stopped?.id ?? null, stored_places: stored, scan_performed: run.phase.database_scan?.performed ?? false,
    following_writes_not_sent: [...new Set(missing.map(s => `${s.class} ${s.table}`))], scan_elapsed_ms: run.phase.database_scan?.elapsed_ms,
    classification});
}
const summary = {stability, following_writes_in_same_connection: following.map(s => `${s.class} ${s.table}${s.commit_point ? '（確定）' : ''}`), target: {class: target.class, table: target.table, shape: target.shape, occurrence: target.occurrence, slots: target.slots, commit_point: target.commit_point, in_transaction: target.in_transaction}, baseline_places: baselinePlaces, cuts};
fs.writeFileSync(path.join(output, 'summary.json'), JSON.stringify(summary, null, 1));
console.log(JSON.stringify(summary, null, 1));
