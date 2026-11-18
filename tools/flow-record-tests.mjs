// 流れの記録（ui/flow-record.js）の状態の移り変わりを、画面を使わずに確かめる。
// 照合・案を作り直したときに、前の了承・結果・回答を流用しないこと、遅れて返った応答を採用しないことを見る。
import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  applyScan, applyPlan, setAnswer, answerTo, answersOf, scanGeneration, planGeneration, StaleResponse,
  approve, approvedCheck, approvedTests, planForCheck, checkIdOf, startStep, migrate, fingerprint, untestedFindings,
  applyInvestigation, runConcerns, planIsCurrent,
} from '../ui/flow-record.js';

const scan = (reqs, questions = []) => ({ findings: reqs.map(requirement => ({ requirement, verification: { executable_check: true } })), questions });
const q = (id, text, options = ['はい', 'いいえ']) => ({ id, text, options });
const plan = (steps, pass) => ({ tests: [{ id: 't-a92fa377c2ab', name: '一覧の試験', kind: 'cli', finding_ids: ['f-1'], steps, pass, observation: 'readback_output' }], not_planned: [] });
const scanned = (s, answers = []) => applyScan({}, s, answers, null);

test('照合し直すと、前の照合・回答・案・了承・結果を履歴へ移し、今の記録から外す', () => {
  const r = scanned(scan(['担当外に出さない'], [q('q1', '職員ごとに担当がありますか')]));
  setAnswer(r, r.scan.questions[0], 'はい');
  applyPlan(r, plan(['A で追加し B で読む'], ['B に出ない']), planGeneration(r));
  approve(r, r.plan.tests[0], 'flow-ccccccc-ta92fa377c2ab');
  r.results = { 'f-1': { state: 'ok' } };
  applyScan(r, scan(['操作の記録を残す'], [q('q1', 'ログはどこに保存しますか', ['ファイル', 'DB'])]), answersOf(r), scanGeneration(r));
  for (const k of ['plan', 'chosen', 'approved', 'results', 'answers']) assert.equal(r[k], undefined, k);
  assert.equal(r.scan.generation, 2);
  assert.equal(r.history.length, 1);
  assert.equal(r.history[0].approved['t-a92fa377c2ab'].check_id, 'flow-ccccccc-ta92fa377c2ab');
  assert.equal(r.history[0].answers.q1.answer, 'はい');
  assert.equal(startStep(r), 2, '前の了承で結果へ進まない');
});

test('同じ質問IDでも質問文が変われば、前の回答を新しい質問の回答にしない', () => {
  const r = scanned(scan(['x'], [q('q1', '院外から使いますか')]));
  setAnswer(r, r.scan.questions[0], 'はい');
  r.scan.questions = [q('q1', '別の新しい質問', ['はい', 'いいえ'])];
  assert.equal(answerTo(r, r.scan.questions[0]), null);
  r.scan.questions = [q('q1', '院外から使いますか', ['使う', '使わない'])];
  assert.equal(answerTo(r, r.scan.questions[0]), null, '今の選択肢にない回答は送らない');
  r.answers = { q1: 'はい' }; // 以前の形式（回答の文字列だけ）
  r.scan.questions = [q('q1', '院外から使いますか')];
  assert.equal(answerTo(r, r.scan.questions[0]), null, 'どの質問への回答か分からない以前の形式は使わない');
});

test('照合へ反映済みの回答は、AIがその質問を返さなくても試験の案へ送る。新しい回答があればそちらを使う', () => {
  const r = scanned(scan(['x'], [q('q1', '職員ごとに担当がありますか')]));
  setAnswer(r, r.scan.questions[0], 'はい');
  applyScan(r, scan(['担当外に出さない'], []), answersOf(r), scanGeneration(r));
  assert.deepEqual(answersOf(r), [{ question: '職員ごとに担当がありますか', answer: 'はい' }]);
  applyScan(r, scan(['担当外に出さない'], [q('q2', '職員ごとに担当がありますか')]), answersOf(r), scanGeneration(r));
  setAnswer(r, r.scan.questions[0], 'いいえ');
  assert.deepEqual(answersOf(r), [{ question: '職員ごとに担当がありますか', answer: 'いいえ' }]);
});

test('試験IDが同じでも、手順・合格の条件・対象の候補の中身が違えば了承していない扱い', () => {
  const r = scanned(scan(['担当外に出さない']));
  applyPlan(r, plan(['A で追加し B で読む'], ['B に出ない']), planGeneration(r));
  approve(r, r.plan.tests[0], 'chk-1');
  assert.equal(approvedCheck(r, r.plan.tests[0]), 'chk-1');
  applyPlan(r, plan(['A で追加し記録を読む'], ['記録に残る']), planGeneration(r));
  assert.equal(r.plan.tests[0].id, 't-a92fa377c2ab');
  assert.equal(approvedCheck(r, r.plan.tests[0]), null);
  assert.deepEqual(approvedTests(r), []);
  assert.equal(startStep(r), 3);
  assert.equal(r.history.at(-1).reason, '試験の案を作り直した');
  assert.equal(r.history.at(-1).approved['t-a92fa377c2ab'].check_id, 'chk-1');
  assert.equal(r.scan.generation, 1, '案を作り直しても照合は同じ');
  // 了承の後で今の案の中身が書き換わっても、了承した中身ではないので結果に使わない。
  approve(r, r.plan.tests[0], 'chk-2');
  r.plan.tests[0].pass = ['別の条件'];
  assert.equal(approvedCheck(r, r.plan.tests[0]), null);
  // 候補の要求文が変われば（同じ f-1 でも）別の試験として扱う。
  const t = { ...r.plan.tests[0] };
  assert.notEqual(fingerprint(t, scan(['担当外に出さない'])), fingerprint(t, { findings: [{ id: 'f-1', requirement: '操作の記録を残す' }] }));
});

test('送った後に照合や案が変わっていたら、遅れて返った応答を採用しない', () => {
  const r = scanned(scan(['担当外に出さない']));
  const planSentAt = planGeneration(r), scanSentAt = scanGeneration(r);
  applyScan(r, scan(['操作の記録を残す']), [], scanGeneration(r)); // 案の応答を待つ間に照合し直した
  assert.throws(() => applyPlan(r, plan(['古い案'], ['古い条件']), planSentAt), StaleResponse);
  assert.equal(r.plan, undefined);
  assert.throws(() => applyScan(r, scan(['古い照合']), [], scanSentAt), StaleResponse, '後から返った前の照合で今の照合を置き換えない');
  assert.equal(r.scan.findings[0].requirement, '操作の記録を残す');
  const sentAt = planGeneration(r);
  applyPlan(r, plan(['案1'], ['条件1']), planGeneration(r)); // 同じ照合で別の案が先に返った
  assert.throws(() => applyPlan(r, plan(['案2'], ['条件2']), sentAt), StaleResponse);
  assert.equal(r.plan.tests[0].steps[0], '案1');
});

test('確認項目から了承した案を、今の記録と履歴から探す。以前の形式の了承も読める', () => {
  const r = scanned(scan(['担当外に出さない']));
  applyPlan(r, plan(['A で追加し B で読む'], ['B に出ない']), planGeneration(r));
  approve(r, r.plan.tests[0], 'chk-1');
  assert.deepEqual([planForCheck(r, 'chk-1')?.current, planForCheck(r, 'chk-1')?.test.pass], [true, ['B に出ない']]);
  applyScan(r, scan(['操作の記録を残す']), [], scanGeneration(r));
  const old = planForCheck(r, 'chk-1');
  assert.equal(old.current, false, '作り直す前の了承は今のものではない');
  assert.equal(old.scan.findings[0].requirement, '担当外に出さない', '了承した時の照合の候補を返す');
  assert.equal(planForCheck(r, 'chk-x'), null);
  assert.equal(checkIdOf('chk-legacy'), 'chk-legacy');
  assert.equal(checkIdOf({ check_id: 'chk-1' }), 'chk-1');
  assert.equal(checkIdOf(undefined), null);
});

test('以前の形式の記録は、照合だけを残して了承・回答・案を履歴へ移す', () => {
  const legacy = { files: ['a.py'], scan: { findings: [{ id: 'f-1', requirement: 'x' }], questions: [q('q1', '質問')] }, answers: { q1: 'はい' },
    plan: { tests: [{ id: 't-1', name: 'n', finding_ids: ['f-1'], steps: [], pass: [] }] }, approved: { 't-1': 'chk-legacy' }, results: { 'f-1': { state: 'ok' } } };
  const r = migrate(structuredClone(legacy));
  assert.equal(startStep(r), 2);
  assert.equal(r.scan.generation, 2);
  assert.equal(r.approved, undefined);
  assert.equal(r.history[0].approved['t-1'], 'chk-legacy');
  const found = planForCheck(r, 'chk-legacy');
  assert.deepEqual([found.current, found.legacy], [false, true]);
  assert.deepEqual(answersOf(r), [], '以前の形式の回答は、どの質問への回答か確かめられないので送らない');
  assert.equal(migrate(r), r); assert.equal(r.history.length, 1, '2回読み込んでも重ねて移さない');
  assert.equal(migrate(null), null);
});

test('試験にしていない候補を、AIが試験にしなかった・未分類・了承していないに分け、理由を補わない', () => {
  const r = scanned(scan(['a', 'b', 'c', 'd']));
  const f = r.scan.findings;
  assert.deepEqual(untestedFindings(r, f), [], '案を作る前は出さない');
  const two = { tests: [
    { id: 't-1', name: '1', kind: 'cli', finding_ids: ['f-1'], steps: ['s'], pass: ['p'], observation: 'readback_output' },
    { id: 't-2', name: '2', kind: 'cli', finding_ids: ['f-2'], steps: ['s'], pass: ['p'], observation: 'readback_output' },
  ], not_planned: [{ finding_id: 'f-3', reason: '' }], unclassified: ['f-4'] };
  applyPlan(r, two, planGeneration(r));
  approve(r, r.plan.tests[0], 'chk-1');
  assert.deepEqual(untestedFindings(r, f).map(u => [u.finding.id, u.kind, u.reason]),
    [['f-2', 'not_approved', ''], ['f-3', 'not_planned', '（理由の記載なし）'], ['f-4', 'unclassified', '']]);
  // 以前の形式の案（未分類の欄が無い）で、どこにも入っていない候補は、理由が記録されていないものとして出す。
  delete r.plan.unclassified;
  assert.deepEqual(untestedFindings(r, f).at(-1).kind, 'unknown');
});

const concern = (id, by = 'run') => ({ id, estimate: '推定', evidence: [{ path: 'app/auth.py', start: 3, end: 5 }], concern: `懸念${id}`, basis: 'repository', check: { by, detail: '確かめ方', observation: by === 'run' ? 'container_log' : null } });
const investigated = (record, id, concerns) => applyInvestigation(record, { operation: '登録', result: { record: id, concerns } });
const concernPlan = () => ({ tests: [{ id: 't-c', name: '懸念の試験', kind: 'browser', finding_ids: ['c-1'], steps: ['登録する'], pass: ['ログに出ない'], observation: 'container_log' }], not_planned: [] });

test('照合なしでも、動かして確かめる懸念があれば試験の案の段へ進み、懸念だけから作った案を今の案として扱う', () => {
  const r = investigated({}, 'inv-1', [concern('c-1'), concern('c-2', 'ask'), concern('c-3', 'read')]);
  assert.deepEqual(runConcerns(r).map(c => c.id), ['c-1'], '確かめ方が「動かす」の懸念だけを候補にする');
  assert.equal(startStep(r), 2);
  applyPlan(r, concernPlan(), planGeneration(r), runConcerns(r));
  assert.equal(r.plan.investigation, 'inv-1');
  assert.deepEqual(r.plan.concerns.map(c => c.id), ['c-1'], '案の候補にした懸念を案に写す');
  assert.equal(planIsCurrent(r), true);
  approve(r, r.plan.tests[0], 'flow-x-tc');
  assert.equal(approvedCheck(r, r.plan.tests[0]), 'flow-x-tc');
  assert.equal(startStep(r), 4);
  // 指紋は懸念の文を含む。懸念の文が変われば了承していない扱い。
  r.plan.concerns[0].concern = '別の懸念';
  assert.equal(approvedCheck(r, r.plan.tests[0]), null);
  assert.equal(investigated({}, 'inv-0', [concern('c-1', 'ask')]) && startStep(investigated({}, 'inv-0', [concern('c-1', 'ask')])), 1, '動かして確かめる懸念がなければ進まない');
});

test('懸念を出し直すと、前の懸念から作った案・了承・結果を履歴へ移す。照合と回答は残す', () => {
  const r = scanned(scan(['担当外に出さない'], [q('q1', '院外から使いますか')]));
  setAnswer(r, r.scan.questions[0], 'はい');
  investigated(r, 'inv-1', [concern('c-1')]);
  applyPlan(r, concernPlan(), planGeneration(r), runConcerns(r));
  approve(r, r.plan.tests[0], 'flow-x-tc');
  investigated(r, 'inv-2', [concern('c-1')]);
  for (const k of ['plan', 'chosen', 'approved']) assert.equal(r[k], undefined, k);
  assert.equal(r.scan.generation, 1, '照合は残す');
  assert.equal(r.answers.q1.answer, 'はい', '回答は残す');
  assert.equal(r.history.at(-1).reason, '懸念を出し直した');
  // 履歴の案からも、了承した確認項目をたどれる（指紋は案に写した懸念で計算する）。
  assert.equal(planForCheck(r, 'flow-x-tc')?.test.id, 't-c');
});

test('照合の候補だけから作った案は、懸念を出し直しても今の案のまま', () => {
  const r = scanned(scan(['担当外に出さない']));
  applyPlan(r, plan(['A'], ['B']), planGeneration(r));
  approve(r, r.plan.tests[0], 'flow-x-t');
  const before = fingerprint(r.plan.tests[0], r.scan);
  investigated(r, 'inv-1', [concern('c-1')]);
  assert.equal(planIsCurrent(r), true);
  assert.equal(approvedCheck(r, r.plan.tests[0]), 'flow-x-t');
  assert.equal(fingerprint(r.plan.tests[0], r.scan, []), before, '懸念のない案の指紋は以前と同じ');
});

test('懸念を送った後に調べを出し直していたら、遅れて返った案を採用しない', () => {
  const r = investigated({}, 'inv-1', [concern('c-1')]);
  const at = planGeneration(r);
  investigated(r, 'inv-2', [concern('c-1')]);
  assert.throws(() => applyPlan(r, concernPlan(), at, runConcerns(r)), StaleResponse);
});
