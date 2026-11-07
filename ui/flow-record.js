// @ts-check
// 流れの記録（/api/v1/flow の record）の状態の移り変わり。画面（flow.js）から使い、DOMには触れない。
//
// 照合をやり直すと、候補ID（f-1…）・試験ID・質問ID（q1…）は新しい応答で同じ値になりうる。
// そのため、了承・結果・ヒアリングの回答は、それを作った照合と案にだけ結びつけ、新しい照合や案へは流用しない。
// 照合や案を作り直すときは、それまでの照合・回答・案・選択・了承・結果を履歴（history）へ移して残す。

/** @typedef {{ question: string, answer: string }} Answer */

/** 照合の応答に候補IDを付け、何回目の照合かを記録する。 */
function stamp(/** @type {any} */ scan, /** @type {number} */ generation, /** @type {any[]} */ sentAnswers) {
  scan.findings.forEach((/** @type {any} */ f, /** @type {number} */ i) => { f.id = `f-${i + 1}`; });
  scan.generation = generation;
  scan.sent_answers = sentAnswers;
  return scan;
}

/** 今の照合・回答・案・選択・了承・結果を履歴へ移し、空にする。移すものが無ければ何もしない。 */
function archive(/** @type {any} */ record, /** @type {string} */ reason) {
  const keys = ['scan', 'answers', 'plan', 'chosen', 'approved', 'results'];
  if (!keys.some(k => record[k] !== undefined)) return;
  const entry = { reason, archived_at: new Date().toISOString() };
  for (const k of keys) if (record[k] !== undefined) /** @type {any} */ (entry)[k] = record[k];
  record.history = [...(record.history || []), entry];
  for (const k of keys) delete record[k];
}

/** 送った時点の照合の世代。応答が返るまでに別の照合が終わっていたら、その応答は使わない。 */
export function scanGeneration(/** @type {any} */ record) { return record?.scan?.generation ?? null; }
/** 送った時点の照合と案の世代。 */
export function planGeneration(/** @type {any} */ record) { return { scan: scanGeneration(record), plan: record?.plan?.generation ?? null }; }
export class StaleResponse extends Error {
  constructor() { super('送った後に照合または試験の案が変わったため、この応答は使いません。もう一度送ってください。'); }
}

/** 新しい照合の応答を記録に入れる。前の照合に結びついたものはすべて履歴へ移す。
 * sentAnswers は照合へ送った回答（この照合に反映済みの条件）。expected は送った時点の照合の世代。 */
export function applyScan(/** @type {any} */ record, /** @type {any} */ scan, /** @type {Answer[]} */ sentAnswers, /** @type {number | null} */ expected) {
  if (scanGeneration(record) !== expected) throw new StaleResponse();
  const generation = (record.scan?.generation || record.history?.filter((/** @type {any} */ h) => h.scan).length || 0) + 1;
  if (record.scan) archive(record, '照合し直した');
  record.scan = stamp(scan, generation, sentAnswers);
  return record;
}

/** ヒアリングの回答を、質問の文言といっしょに記録する。文言が変われば別の質問として扱う。 */
export function setAnswer(/** @type {any} */ record, /** @type {any} */ question, /** @type {string} */ option) {
  record.answers = { ...(record.answers || {}), [question.id]: { question: question.text, answer: option } };
}

/** 今の照合の質問に対する回答。質問の文言が同じで、回答が今の選択肢にあるものだけを返す。 */
export function answerTo(/** @type {any} */ record, /** @type {any} */ question) {
  const a = record?.answers?.[question.id];
  return a && typeof a === 'object' && a.question === question.text && question.options?.includes(a.answer) ? a.answer : null;
}

/** 今の照合の質問への、新しい回答。 */
export function newAnswers(/** @type {any} */ record) {
  return (record?.scan?.questions || []).flatMap((/** @type {any} */ q) => {
    const a = answerTo(record, q);
    return a ? [{ question: q.text, answer: a }] : [];
  });
}
/** AIへ送る回答。今の照合に反映済みの回答（照合へ送ったもの）と、今の質問への新しい回答。
 * 同じ質問文に両方あれば、新しい回答を使う。AIが回答済みの質問を返さなくても、反映済みの条件は落とさない。 */
export function answersOf(/** @type {any} */ record) {
  const fresh = newAnswers(record);
  const asked = new Set(fresh.map((/** @type {Answer} */ a) => a.question));
  const carried = (record?.scan?.sent_answers || []).filter((/** @type {Answer} */ a) => a && !asked.has(a.question));
  return [...carried, ...fresh];
}

/** 新しい試験の案を記録に入れる。前の案があれば、その案の選択・了承・結果ごと履歴へ移す（照合と回答は残す）。 */
export function applyPlan(/** @type {any} */ record, /** @type {any} */ plan, /** @type {{ scan: number | null, plan: number | null }} */ expected) {
  const now = planGeneration(record);
  if (now.scan !== expected.scan || now.plan !== expected.plan) throw new StaleResponse();
  if (record.plan) {
    const keep = { scan: record.scan, answers: record.answers };
    archive(record, '試験の案を作り直した');
    // 照合と回答は作り直していないので、今の記録にも戻す（履歴には作り直す前の状態として残る）。
    Object.assign(record, keep);
    if (keep.answers === undefined) delete record.answers;
  }
  plan.scan_generation = record.scan?.generation;
  plan.generation = (record.history || []).filter((/** @type {any} */ h) => h.plan).length + 1;
  record.plan = plan;
  record.chosen = Object.fromEntries(plan.tests.map((/** @type {any} */ t) => [t.id, true]));
  delete record.approved; delete record.results;
  return record;
}

/** 試験の中身の指紋。試験IDが同じでも、手順・合格の条件・対象の候補・観測が違えば別の試験として扱う。 */
export function fingerprint(/** @type {any} */ test, /** @type {any} */ scan) {
  const findings = Object.fromEntries((scan?.findings || []).map((/** @type {any} */ f) => [f.id, f.requirement]));
  return JSON.stringify([test.name, test.kind, test.steps, test.pass, test.observation, (test.finding_ids || []).map((/** @type {string} */ id) => [id, findings[id] ?? null])]);
}

/** 今の案が、今の照合から作ったものか。 */
export function planIsCurrent(/** @type {any} */ record) {
  return !!record?.plan?.tests && !!record.scan && record.plan.scan_generation === record.scan.generation;
}

/** 了承を記録する。確認項目のIDといっしょに、了承した試験の中身の指紋を残す。 */
export function approve(/** @type {any} */ record, /** @type {any} */ test, /** @type {string} */ checkId) {
  record.approved = { ...(record.approved || {}), [test.id]: { check_id: checkId, fingerprint: fingerprint(test, record.scan), plan_generation: record.plan.generation } };
}

/** 今の案のこの試験について、了承して登録した確認項目のID。了承した中身と今の中身が違えば了承していない扱い。 */
export function approvedCheck(/** @type {any} */ record, /** @type {any} */ test) {
  if (!planIsCurrent(record)) return null;
  const a = record.approved?.[test.id];
  if (!a || typeof a !== 'object') return null; // 以前の形式（確認項目IDだけ）は、どの案を了承したか分からないので使わない
  return a.plan_generation === record.plan.generation && a.fingerprint === fingerprint(test, record.scan) ? a.check_id : null;
}

/** 今の案で了承済みの試験。 */
export function approvedTests(/** @type {any} */ record) {
  return planIsCurrent(record) ? record.plan.tests.filter((/** @type {any} */ t) => approvedCheck(record, t)) : [];
}

/** 了承の値から確認項目のIDを取り出す（以前の形式は確認項目IDの文字列）。 */
export function checkIdOf(/** @type {any} */ approval) { return typeof approval === 'string' ? approval : approval?.check_id ?? null; }

/** 確認項目を登録した試験の案を、今の記録と履歴から探す。了承した中身と違う案には結びつけない。
 * current は今の照合・今の案で了承したものか。legacy は以前の形式で、どの案を了承したか確かめられないもの。 */
export function planForCheck(/** @type {any} */ record, /** @type {string} */ checkId) {
  const entries = [record, ...(record?.history || []).slice().reverse()];
  for (const e of entries) for (const t of e?.plan?.tests || []) {
    const a = e.approved?.[t.id];
    if (checkIdOf(a) !== checkId) continue;
    if (typeof a === 'object' && a.fingerprint !== fingerprint(t, e.scan)) continue;
    return { test: t, scan: e.scan, current: e === record && approvedCheck(record, t) === checkId, legacy: typeof a === 'string' };
  }
  return null;
}

/** 記録から開く段階。今の案に了承済みの試験があるときだけ結果へ進む。 */
export function startStep(/** @type {any} */ record) {
  if (approvedTests(record).length) return 4;
  if (planIsCurrent(record)) return 3;
  return record?.scan ? 2 : 1;
}

/** 以前の形式の記録を読み込んだとき、今の照合と案に結びついていることを確かめられないものを履歴へ移す。 */
export function migrate(/** @type {any} */ record) {
  if (!record) return record;
  if (record.scan && record.scan.generation === undefined) {
    // 世代のない記録は、照合し直しで了承・回答が古い照合から残っている可能性がある。照合だけを今の記録として残す。
    const scan = record.scan;
    archive(record, '以前の形式の記録（照合と了承の対応を確かめられない）');
    record.scan = { ...scan, generation: (record.history || []).length + 1, sent_answers: null };
  }
  return record;
}
