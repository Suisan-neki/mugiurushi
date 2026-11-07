// @ts-check
// ガイドラインの照合から試験・修正・再試験までの流れ。
// 判定と観測の状態は事例の記録（/api/v1/case）が正。流れの記録（/api/v1/flow）は画面を続けるための控え。
// AIへ送る内容は、送る前にサーバーが組み立てた内容をそのまま表示し、人が確かめたときだけ送る。
import { apiRequest as api } from './api.js';

/** @param {string} tag @param {Record<string, any>} [attrs] @param {...any} children */
function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (k === 'style') el.style.cssText = String(v); // 画面のCSPは style 属性を許さない。CSSOMで設定する。
    else if (k === 'checked' || k === 'disabled' || k === 'value') /** @type {any} */ (el)[k] = v;
    else el.setAttribute(k, v === true ? '' : String(v));
  }
  for (const c of children.flat(Infinity)) if (c !== null && c !== undefined && c !== false) el.append(c instanceof Node ? c : String(c));
  return el;
}
const $ = (/** @type {string} */ id) => /** @type {HTMLElement} */ (document.getElementById(id));
const fill = (/** @type {HTMLElement} */ el, /** @type {any[]} */ ...c) => el.replaceChildren(...c.flat(Infinity).filter(x => x !== null && x !== undefined && x !== false).map(x => x instanceof Node ? x : document.createTextNode(String(x))));
const btn = (/** @type {string} */ text, /** @type {() => any} */ onclick, cls = '') => h('button', { type: 'button', class: `btn ${cls}`, onclick }, text);
const short = (/** @type {string} */ c) => (c || '').slice(0, 7);

const SECTION_OF = { 'provider-security': 'g3', 'mhlw-security': 'g3', 'mhlw-cyber-checklist': 'g3', 'ppc-medical-guidance': 'pi', 'ppc-general': 'pi', 'ipa-websecurity': 'web', 'owasp-top10': 'web', 'owasp-asvs': 'web', 'nist-800-53': 'nist', 'nist-csf': 'nist', 'mhlw-samd': 'dev', 'mhlw-device-cyber': 'dev' };
const SECTIONS = [['g3', '3省2ガイドライン'], ['pi', '個人情報保護法のガイドライン'], ['web', 'Webアプリケーションの技術基準'], ['dev', '医療機器'], ['nist', 'NISTの基準（参考）'], ['other', 'その他']];
const PRIO = { high: '高', medium: '中', low: '低' };
const PRIO_ORDER = { high: 0, medium: 1, low: 2 };
const STEPS = ['1 照合の準備', '2 照合とヒアリング', '3 検証案の了承', '4 結果', '5 修正用のプロンプト', '6 再試験'];
const CODE = /\.(py|js|ts|tsx|jsx|rb|go|php|java|kt|rs|cs|html|htm|vue|svelte|sql|yml|yaml|toml|json|cfg|ini|conf)$/i;

/** @type {any} */
const state = { step: 1, source: null, view: null, flow: null, record: null, agent: null, sel: null };

function person() { try { return JSON.parse(sessionStorage.getItem('eel-person') || 'null')?.name || ''; } catch { return ''; } }
function setStatus(/** @type {string} */ t) { $('status').textContent = t; }
function showError(/** @type {any} */ e) { fill($('alert'), h('div', { class: 'alert' }, e instanceof Error ? e.message : String(e))); }
async function guarded(/** @type {() => Promise<any>} */ f) { $('alert').replaceChildren(); try { return await f(); } catch (e) { showError(e); } }
function key() { return `${state.source.repository}@${state.source.commit}`; }
async function saveRecord() {
  state.flow = await api('POST', '/api/v1/flow', { repository: state.source.repository, commit: state.source.commit, record: state.record });
}
async function loadCase() { state.view = await api('GET', '/api/v1/case'); }

// ---------- 共通 ----------
function renderChrome() {
  const s = state.source;
  fill($('target'),
    h('div', {}, h('span', { class: 'k' }, 'リポジトリ'), '\u3000', h('strong', { style: 'font-size:15px' }, s?.repository || '（未指定）')),
    h('div', {}, h('span', { class: 'k' }, 'コミット'), '\u3000', h('strong', {}, short(s?.commit || '')) ),
    state.record?.files ? h('div', {}, h('span', { class: 'k' }, '読んだコード'), `\u3000${state.record.files.length}ファイル`) : null);
  const done = state.record || {};
  const reachable = [true, !!done.scan, !!done.plan, !!done.approved && Object.keys(done.approved).length > 0, !!done.approved && Object.keys(done.approved).length > 0, true];
  fill($('steps'), STEPS.map((t, i) => h('button', { type: 'button', 'aria-current': state.step === i + 1 ? 'step' : null, disabled: !reachable[i], onclick: () => go(i + 1) }, (i ? '▸ ' : '') + t)));
  $('person').textContent = person() ? `記録者：${person()}` : '';
  $('foot-left').textContent = done.scan?.model_call?.response_id ? `照合の応答ID ${done.scan.model_call.response_id}` : '';
}
function go(/** @type {number} */ n) { state.step = n; render(); window.scrollTo(0, 0); }
function render() {
  renderChrome();
  const f = [null, step1, step2, step3, step4, step5, step6][state.step];
  guarded(async () => { await f(); });
}

/** 送る前の確認。サーバーが組み立てた内容をそのまま示し、確かめたときだけ送る。 */
function confirmAndSend(/** @type {HTMLElement} */ out, /** @type {string} */ path, /** @type {any} */ body, /** @type {(r: any) => any} */ onSent) {
  return guarded(async () => {
    setStatus('送る内容を組み立てています。');
    const preview = await api('POST', path, body);
    setStatus('');
    const agree = h('input', { type: 'checkbox', id: 'agree' });
    const send = btn('この内容を送る', () => guarded(async () => {
      send.setAttribute('disabled', '');
      setStatus('AIエージェントに送っています。応答まで1分ほどかかることがあります。');
      try { await onSent(await api('POST', path, { ...body, confirm: { payload_sha256: preview.payload_sha256, synthetic_only: agree.checked } })); setStatus(''); }
      finally { send.removeAttribute('disabled'); }
    }), 'primary');
    send.setAttribute('disabled', '');
    agree.addEventListener('change', () => agree.checked ? send.removeAttribute('disabled') : send.setAttribute('disabled', ''));
    fill(out, h('div', { class: 'box', 'data-preview': '' },
      h('div', { class: 'head' }, h('strong', {}, '送る内容の確認'), h('span', { class: 'note' }, `送り先：${preview.destination}`)),
      h('div', { class: 'pad' },
        h('details', {}, h('summary', {}, `送る内容（${preview.bytes} バイト）`), h('pre', { class: 'payload' }, JSON.stringify(preview.payload, null, 2))),
        h('label', { style: 'display:flex;gap:8px;align-items:flex-start;margin-top:8px' }, agree, '送る内容は公開してよいコードと架空のデータだけで、実患者の情報・認証情報・生のログを含まないことを確かめた'),
        h('div', { class: 'actions' }, send))));
  });
}

// ---------- 1 照合の準備 ----------
async function step1() {
  const inv = (state.source.inventory || []).filter((/** @type {any} */ f) => f.path);
  const chosen = new Set(state.record?.files || inv.filter((/** @type {any} */ f) => CODE.test(f.path) && !/(^|\/)(tests?|docs?|node_modules|vendor|dist|build)\//.test(f.path)).slice(0, 20).map((/** @type {any} */ f) => f.path));
  const count = h('span', {});
  const update = () => { count.textContent = `読むファイル ${chosen.size}件（上限40件）`; };
  update();
  const name = h('input', { type: 'text', value: person(), style: 'width:240px;padding:6px;border:1px solid #8A9199', id: 'person-name' });
  name.addEventListener('input', () => sessionStorage.setItem('eel-person', JSON.stringify({ name: /** @type {HTMLInputElement} */ (name).value.trim(), role: 'developer' })));
  const desc = h('textarea', { rows: 6, id: 'desc' }); /** @type {HTMLTextAreaElement} */ (desc).value = state.record?.summary || '';
  const out = h('div', {});
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main' }, h('h2', {}, '照合に使うコード'),
      h('p', {}, `コミット ${short(state.source.commit)} から読むファイルを選びます。読んだコードは照合のためにAIエージェントへ送ります。送る前に内容を表示します。`),
      h('div', { class: 'scroll' }, h('table', { 'data-files': '' },
        h('thead', {}, h('tr', {}, h('th', { scope: 'col', style: 'width:48px;text-align:center' }, '読む'), h('th', { scope: 'col' }, 'ファイル'), h('th', { scope: 'col', style: 'width:100px;text-align:right' }, 'バイト'))),
        h('tbody', {}, inv.slice(0, 400).map((/** @type {any} */ f) => {
          const box = h('input', { type: 'checkbox', checked: chosen.has(f.path), 'aria-label': `${f.path}を読む` });
          box.addEventListener('change', () => { /** @type {HTMLInputElement} */ (box).checked ? chosen.add(f.path) : chosen.delete(f.path); update(); });
          return h('tr', {}, h('td', { style: 'text-align:center' }, box), h('td', { class: 'code' }, f.path), h('td', { style: 'text-align:right' }, f.size ?? ''));
        })))),
      h('p', { class: 'note' }, count)),
    h('section', { class: 'side' }, h('h2', {}, '記録者'), h('label', { for: 'person-name' }, '名前（記録に残します。本人確認はしません）'), h('div', {}, name),
      h('h2', { style: 'margin-top:16px' }, 'システムの説明（任意）'),
      h('p', {}, 'コードから分からないこと（使う人、扱う情報、提供の形）を書くと、照合が具体的になります。'), h('label', { for: 'desc' }, '説明'), desc,
      h('div', { class: 'actions' }, btn('照合を始める（送る内容を先に確認）', () => {
        if (!person()) return showError('記録者の名前を入力してください。');
        const files = [...chosen];
        state.record = { ...(state.record || {}), files, summary: /** @type {HTMLTextAreaElement} */ (desc).value.trim() };
        return confirmAndSend(out, '/api/v1/agent/flow-scan', { repository: state.source.repository, commit: state.source.commit, files, summary: state.record.summary, answers: answersOf(state.record) }, async (/** @type {any} */ r) => {
          state.record.scan = withIds(r); delete state.record.plan; await saveRecord(); go(2);
        });
      }, 'primary')), out)));
}
function withIds(/** @type {any} */ scan) { scan.findings.forEach((/** @type {any} */ f, /** @type {number} */ i) => { f.id = `f-${i + 1}`; }); return scan; }
function answersOf(/** @type {any} */ record) {
  const qs = record?.scan?.questions || [];
  return qs.filter((/** @type {any} */ q) => record.answers?.[q.id]).map((/** @type {any} */ q) => ({ question: q.text, answer: record.answers[q.id] }));
}
function testable() { return (state.record?.scan?.findings || []).filter((/** @type {any} */ f) => f.verification?.executable_check); }
function sectionOf(/** @type {any} */ f) {
  const ids = (f.citations || []).map((/** @type {any} */ c) => SECTION_OF[c.source_id] || 'other');
  return SECTIONS.map(([k]) => k).find(k => ids.includes(k)) || 'other';
}
function bySection(/** @type {any[]} */ items) {
  return SECTIONS.map(([k, name]) => ({ key: k, name, rows: items.filter(f => sectionOf(f) === k).sort((a, b) => PRIO_ORDER[a.priority] - PRIO_ORDER[b.priority]) })).filter(s => s.rows.length);
}
function prio(/** @type {string} */ p) { return h('span', { class: `prio ${p}` }, PRIO[p] || p); }

// ---------- 2 照合とヒアリング ----------
async function step2() {
  const r = state.record;
  const out = h('div', {});
  const askOut = h('div', {});
  const ask = h('textarea', { rows: 3, id: 'ask', placeholder: '例：ログの保存期間はどれくらい必要ですか' });
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main' }, h('h2', {}, '照合の結果：満たしていない可能性がある項目'),
      h('p', {}, '読んだコードと説明をガイドラインと照らしました。試験で確かめられる項目を、ガイドラインごとに優先度順で並べています。'),
      testable().length ? bySection(testable()).map(s => h('div', { 'data-section': s.key }, h('h3', {}, s.name),
        h('table', {}, h('tbody', {}, s.rows.map((/** @type {any} */ f) => h('tr', { 'data-finding': f.id },
          h('td', { style: 'width:48px;text-align:center' }, prio(f.priority)),
          h('td', {}, f.requirement, f.system_refs?.length ? h('div', { class: 'note', style: 'font-size:12px' }, `関係するコード：${f.system_refs.join('、')}`) : null))))))) : h('p', { class: 'note' }, '試験で確かめられる項目は見つかりませんでした。'),
      h('p', { class: 'note', style: 'font-size:12px' }, r.scan.priority_rule || '')),
    h('section', { class: 'side' }, h('h2', {}, 'ヒアリング'),
      r.scan.questions?.length ? [h('p', {}, 'コードだけでは決められないことを確認します。回答は照合と試験の案に使います。'),
        r.scan.questions.map((/** @type {any} */ q, /** @type {number} */ i) => h('fieldset', { 'data-question': q.id }, h('legend', {}, `質問${i + 1}`), h('div', {}, q.text), h('div', { class: 'note', style: 'font-size:12px' }, q.why),
          h('div', { style: 'display:flex;flex-wrap:wrap;gap:12px;margin-top:4px' }, q.options.map((/** @type {string} */ o) => {
            const radio = h('input', { type: 'radio', name: q.id, checked: r.answers?.[q.id] === o });
            radio.addEventListener('change', () => { r.answers = { ...(r.answers || {}), [q.id]: o }; });
            return h('label', { style: 'display:inline-flex;gap:6px;align-items:center' }, radio, o);
          }))))] : h('p', { class: 'note' }, '確認したいことはありません。'),
      h('label', { for: 'ask', style: 'font-weight:700;display:block;margin-top:8px' }, 'こちらから聞きたいこと'), ask,
      h('div', { class: 'actions' }, btn('ガイドラインに照らして答えてもらう', () => {
        const q = /** @type {HTMLTextAreaElement} */ (ask).value.trim(); if (!q) return showError('質問を書いてください。');
        return confirmAndSend(askOut, '/api/v1/agent/ask', { question: q }, async (/** @type {any} */ a) => { r.asks = [...(r.asks || []), { question: q, ...a }]; await saveRecord(); render(); });
      })), askOut,
      (r.asks || []).slice().reverse().map((/** @type {any} */ a) => h('div', { class: 'box', 'data-asked': '' }, h('div', { class: 'pad' }, h('strong', {}, `質問：${a.question}`), h('div', {}, a.answer),
        a.citations?.map((/** @type {any} */ c) => h('div', { class: 'note', style: 'font-size:12px' }, `出典：${c.document} ${c.page}ページ（SHA-256 ${c.sha256.slice(0, 12)}…）`))))),
      h('div', { class: 'actions' },
        btn('回答して照合し直す', () => confirmAndSend(out, '/api/v1/agent/flow-scan', { repository: state.source.repository, commit: state.source.commit, files: r.files, summary: r.summary, answers: answersOf(r) }, async (/** @type {any} */ s) => { r.scan = withIds(s); delete r.plan; await saveRecord(); render(); })),
        btn('試験の案を作る', () => {
          const findings = testable().map((/** @type {any} */ f) => ({ id: f.id, requirement: f.requirement, how: f.verification.how || '' }));
          if (!findings.length) return showError('試験で確かめられる項目がありません。');
          return confirmAndSend(out, '/api/v1/agent/plan', { repository: state.source.repository, commit: state.source.commit, files: r.files, summary: r.summary, answers: answersOf(r), findings }, async (/** @type {any} */ p) => { r.plan = p; r.chosen = Object.fromEntries(p.tests.map((/** @type {any} */ t) => [t.id, true])); await saveRecord(); go(3); });
        }, 'primary')), out)));
}

// ---------- 3 検証案の了承 ----------
async function step3() {
  const r = state.record; const findings = Object.fromEntries((r.scan.findings || []).map((/** @type {any} */ f) => [f.id, f]));
  const out = h('div', {});
  const approved = r.approved || {};
  fill($('main'), h('div', { class: 'pad' },
    h('h2', {}, `試験の案（${r.plan.tests.length}件）`),
    h('p', {}, '同じ操作で確かめられる項目は一つの試験にまとめています。了承した試験は確認項目として登録し、試験の設定（起動・操作・判定条件）を確かめてから実行します。'),
    r.plan.tests.map((/** @type {any} */ t) => {
      const box = h('input', { type: 'checkbox', checked: r.chosen?.[t.id] !== false, disabled: !!approved[t.id] });
      box.addEventListener('change', () => { r.chosen = { ...(r.chosen || {}), [t.id]: /** @type {HTMLInputElement} */ (box).checked }; });
      return h('div', { class: 'box', 'data-test': t.id },
        h('div', { class: 'head' }, h('label', { style: 'display:inline-flex;gap:8px;align-items:center;font-weight:700;font-size:14px' }, box, t.name),
          h('span', { class: 'note' }, { browser: 'ブラウザ操作', http: 'HTTP', cli: 'コマンド', other: 'その他' }[t.kind] || t.kind),
          h('span', { class: 'note' }, `確かめる項目：${t.finding_ids.map((/** @type {string} */ i) => findings[i]?.requirement || i).join('、')}`),
          approved[t.id] ? h('span', { style: 'margin-left:auto' }, h('a', { href: workbenchUrl(approved[t.id]) }, '登録済み：試験の設定を開く')) : null),
        h('div', { class: 'cols' },
          h('div', { class: 'main', style: 'border-right:1px solid #D3D8DD' }, h('strong', {}, '手順'), h('ol', { style: 'margin:4px 0 0;padding-left:20px' }, t.steps.map((/** @type {string} */ s) => h('li', {}, s)))),
          h('div', { class: 'side', style: 'background:#fff' }, h('strong', {}, '合格の条件'), h('ul', { style: 'margin:4px 0 0;padding-left:18px' }, t.pass.map((/** @type {string} */ p) => h('li', {}, p))),
            h('div', { class: 'note', style: 'font-size:12px;margin-top:6px' }, `観測：${t.observation_label}`))));
    }),
    r.plan.not_planned?.length ? h('p', { class: 'note' }, `試験の案にしなかった項目：${r.plan.not_planned.map((/** @type {any} */ n) => `${findings[n.finding_id]?.requirement || n.finding_id}（${n.reason}）`).join('、')}`) : null,
    h('div', { class: 'actions' }, btn('了承して確認項目に登録する', () => guarded(async () => {
      if (!person()) throw Error('記録者の名前を「1 照合の準備」で入力してください。');
      const tests = r.plan.tests.filter((/** @type {any} */ t) => r.chosen?.[t.id] !== false && !approved[t.id]);
      if (!tests.length) throw Error('了承する試験を選んでください。');
      for (const t of tests) approved[t.id] = await register(t, findings);
      r.approved = approved; await saveRecord(); go(4);
    }), 'primary'), h('span', { class: 'note' }, '手順はAIエージェントの案です。'), out)));
}
/** 試験の結果をコードへたどれるように、案の候補が関係するとしたファイル（なければ照合で読んだ最初のファイル）を対応づける。 */
function codeRef(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  const files = state.record.files || [];
  const refs = t.finding_ids.flatMap((/** @type {string} */ id) => findings[id]?.system_refs || []);
  const path = refs.find((/** @type {string} */ p) => files.includes(p)) || files[0];
  return path ? { repository: state.source.repository, commit: state.source.commit, path, line: 1, end_line: 1 } : null;
}
function workbenchUrl(/** @type {string} */ checkId) {
  const u = new URL('/workbench', location.origin);
  u.search = new URLSearchParams({ url: `https://github.com/${state.source.repository}`, ref: state.source.commit, check: checkId, from: 'flow' }).toString(); u.hash = 'trial';
  return u.pathname + u.search + u.hash;
}
async function change(/** @type {any} */ action, /** @type {string} */ reason) {
  await loadCase();
  state.view = await api('POST', '/api/v1/change', { expected_revision: state.view.workspace.revision, actor: `${person()}（開発者）`, reason, action });
}
async function register(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  const r = state.record;
  let id = `flow-${short(state.source.commit)}-${t.id.replace(/[^a-z0-9-]/gi, '').slice(0, 20)}`;
  await loadCase();
  while (state.view.workspace.checks.some((/** @type {any} */ c) => c.id === id)) id += '-2';
  const call = r.plan.model_call || {};
  const origin = `ガイドライン照合の試験の案（${call.response_model || call.requested_model || 'AI'}・応答ID ${call.response_id || '不明'}）を${person()}が了承して登録。対象 ${state.source.repository}@${short(state.source.commit)}`;
  const text = `${t.name}：${t.pass.join('／')}`.slice(0, 1000);
  await change({ op: 'add_check', check: { id, version: 1, text, origin, source: codeRef(t, findings), observation: null, rule: 'manual_only', dependencies: [], questions: [] } }, 'ガイドライン照合の試験の案を了承した');
  const aspects = t.finding_ids.map((/** @type {string} */ fid) => {
    const f = findings[fid]; const c = f.citations?.[0] || {};
    return { id: fid, text: f.requirement.slice(0, 2000), origin: `照合の候補。出典：${c.document || ''} ${c.page || ''}ページ（SHA-256 ${(c.sha256 || '').slice(0, 12)}…）`.slice(0, 1000), observation: t.observation, trial_checks: [] };
  });
  await change({ op: 'set_aspects', check_id: id, aspects }, '試験で確かめる項目を登録した');
  return id;
}

// ---------- 4 結果 ----------
function resultOf(/** @type {any} */ f) {
  const r = state.record;
  const testId = Object.keys(r.approved || {}).find(t => r.plan.tests.find((/** @type {any} */ x) => x.id === t)?.finding_ids.includes(f.id));
  if (!testId) return { state: 'none' };
  const checkId = r.approved[testId];
  const test = r.plan.tests.find((/** @type {any} */ x) => x.id === testId);
  const cv = state.view.checks.find((/** @type {any} */ c) => c.check_id === checkId);
  const a = cv?.coverage?.aspects?.find((/** @type {any} */ x) => x.id === f.id);
  const applicable = (cv?.evidence || []).filter((/** @type {any} */ e) => e.applicable);
  const ev = applicable.find((/** @type {any} */ e) => e.id === cv.selected_evidence) || applicable[0];
  // 状態は事例の記録の値だけで決める。実行していない試験を成功・失敗として表示しない。
  // 実行したが判定条件に対応づけていない部分などは「未確認」とし、理由（mugiurushi の状態の説明）を添える。
  const st = !ev ? 'pending' : a?.status === 'observed' ? 'ok' : a?.status === 'contradicted' ? 'ng' : 'unconfirmed';
  const linked = (a?.trial_checks || []).length > 0;
  return { state: st, checkId, test, aspect: a, linked, record: ev ? `試行 ${ev.run_id}` : '—' };
}
/** 抜粋の表示用に、Markdownの強調記号と余分な空白を外す。 */
function plain(/** @type {string} */ text) { return (text || '').replace(/\*\*/g, '').replace(/(^|\s)\*(?=\s|$)/g, '$1・').replace(/\s+・?\s*$/, '').replace(/\s+/g, ' ').trim(); }
/** 実際に確かめた判定条件（mugiurushi の試験設定の文言）。対応づけた判定条件が無ければ書かない。 */
function checkedBy(/** @type {any} */ aspect) {
  return (aspect?.trial_checks || []).map((/** @type {any} */ l) => `- 確かめた判定条件: ${l.id} ${l.label || ''}`.trim());
}
const NEXT = { pending: '試験を設定して実行する', unconfirmed: '判定条件に対応づける', ng: '試験の詳細を開く', ok: '試験の詳細を開く' };
function stateLabel(/** @type {any} */ res) {
  if (res.state === 'pending') return res.linked ? '未実行' : '未実行（判定条件への対応づけ前）';
  if (res.state === 'unconfirmed') return `未確認：${res.aspect?.status_label || '状態を読み取れない'}`;
  return res.state === 'ok' ? '期待どおり' : '';
}
async function step4() {
  await loadCase();
  const r = state.record;
  const items = testable().map((/** @type {any} */ f) => ({ ...f, res: resultOf(f) })).filter((/** @type {any} */ f) => f.res.state !== 'none');
  r.results = Object.fromEntries(items.map((/** @type {any} */ f) => [f.id, { requirement: f.requirement, state: f.res.state }]));
  await saveRecord();
  const ng = items.filter((/** @type {any} */ f) => f.res.state === 'ng').length;
  const pending = [...new Set(items.filter((/** @type {any} */ f) => f.res.state === 'pending').map((/** @type {any} */ f) => f.res.checkId))];
  if (!state.sel || !items.find((/** @type {any} */ f) => f.id === state.sel)) state.sel = items.find((/** @type {any} */ f) => f.res.state !== 'ok')?.id || items[0]?.id;
  const sel = items.find((/** @type {any} */ f) => f.id === state.sel);
  const detail = h('section', { class: 'side', 'data-detail': '' });
  const showDetail = (/** @type {any} */ f) => {
    if (!f) return fill(detail, h('p', { class: 'note' }, '項目を選ぶと詳細を表示します。'));
    const c = f.citations?.[0] || {};
    fill(detail, h('h2', {}, '詳細'), h('div', { style: 'font-size:15px;font-weight:700;margin-bottom:8px' }, f.requirement),
      h('div', { style: 'font-weight:700' }, 'ガイドラインの該当箇所（抜粋）'), h('blockquote', {}, plain(c.excerpt)),
      h('div', { class: 'note', style: 'font-size:12px;margin:4px 0 10px' }, `${c.document || ''} ${c.page || ''}ページ（取得元 ${c.origin || ''}、SHA-256 ${(c.sha256 || '').slice(0, 12)}…）`),
      h('table', {}, h('tbody', {},
        h('tr', {}, h('th', { scope: 'row' }, '優先度'), h('td', {}, prio(f.priority), ' ', f.patient_data ? '患者の情報に関わる' : '', f.relevance === 'likely' ? '・該当の見込みが高い' : '')),
        h('tr', {}, h('th', { scope: 'row' }, '試験'), h('td', {}, f.res.test.name)),
        h('tr', {}, h('th', { scope: 'row' }, '手順'), h('td', {}, h('ol', { style: 'margin:0;padding-left:18px' }, f.res.test.steps.map((/** @type {string} */ s) => h('li', {}, s))))),
        h('tr', {}, h('th', { scope: 'row' }, '合格の条件'), h('td', {}, f.res.test.pass.join('／'))),
        h('tr', {}, h('th', { scope: 'row' }, '結果'), h('td', { style: f.res.state === 'ng' ? 'color:#A3261E;font-weight:700' : '' }, f.res.state === 'pending' ? 'まだ実行していない' : f.res.aspect ? `${f.res.aspect.status_label}。${f.res.aspect.detail || ''}` : '—')),
        h('tr', {}, h('th', { scope: 'row' }, '記録'), h('td', {}, f.res.record)))),
      h('div', { class: 'actions' }, h('a', { class: 'btn navy', href: workbenchUrl(f.res.checkId) }, NEXT[f.res.state] || '試験の詳細を開く')));
  };
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main' },
      h('div', { style: 'display:flex;flex-wrap:wrap;gap:8px;align-items:center;justify-content:space-between' }, h('h2', { style: 'flex:1' }, 'ガイドラインの要求と、このコミットの試験結果'),
        ng ? h('span', { class: 'ng', 'data-ng-count': String(ng) }, `試験で不足を確認 ${ng}件`) : null),
      pending.length ? h('p', {}, `まだ実行していない試験が${pending.length}件あります。「試験を設定して実行する」から、了承した手順と合格の条件を見ながら設定して実行してください。`) : null,
      bySection(items.filter((/** @type {any} */ f) => f.res.state !== 'ok')).map(s => h('div', { 'data-result-section': s.key }, h('h3', {}, `${s.name}：満たしていない可能性があるのは、次の${s.rows.length}件です（優先度順）`),
        h('table', {}, h('thead', {}, h('tr', {}, h('th', { scope: 'col', style: 'width:56px' }, '優先度'), h('th', { scope: 'col' }, '項目'), h('th', { scope: 'col', style: 'width:150px' }, '状態'), h('th', { scope: 'col', style: 'width:220px' }, '次にやること'), h('th', { scope: 'col', style: 'width:64px' }, h('span', { style: 'position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)' }, '詳細')))),
          h('tbody', {}, s.rows.map((/** @type {any} */ f) => h('tr', { class: f.id === state.sel ? 'sel' : f.res.state === 'ng' ? 'rowng' : '', 'data-result': f.res.state },
            h('td', { style: 'text-align:center' }, prio(f.priority)),
            h('td', {}, f.requirement),
            h('td', { 'data-state-label': '' }, f.res.state === 'ng' ? h('span', { class: 'ng' }, '試験で不足を確認') : stateLabel(f.res)),
            h('td', {}, f.res.state === 'ng' ? '直す（修正用のプロンプトを作る）' : h('a', { href: workbenchUrl(f.res.checkId) }, NEXT[f.res.state]), h('div', { class: 'note', style: 'font-size:12px' }, f.res.test.name)),
            h('td', { style: 'text-align:center' }, btn('詳細', () => { state.sel = f.id; render(); }, f.id === state.sel ? 'primary' : '')))))))),
      items.filter((/** @type {any} */ f) => f.res.state === 'ok').length ? h('p', { class: 'note' }, `試験で期待どおりだった項目：${items.filter((/** @type {any} */ f) => f.res.state === 'ok').map((/** @type {any} */ f) => f.requirement).join('、')}`) : null,
      h('div', { class: 'actions' }, ng ? btn('修正用のプロンプトを作る', () => go(5), 'primary') : null, btn('結果を読み直す', () => render()))),
    detail));
  showDetail(sel);
}

// ---------- 5 修正用のプロンプト ----------
async function step5() {
  await loadCase();
  const r = state.record;
  const ng = testable().map((/** @type {any} */ f) => ({ ...f, res: resultOf(f) })).filter((/** @type {any} */ f) => f.res.state === 'ng');
  const chosen = new Set(ng.map((/** @type {any} */ f) => f.id));
  const pre = h('pre', { class: 'prompt', 'data-prompt': '' });
  const build = () => {
    const blocks = ng.filter((/** @type {any} */ f) => chosen.has(f.id)).flatMap((/** @type {any} */ f) => {
      const c = f.citations?.[0] || {};
      return [`## ${f.requirement}`, '', `根拠: ${c.document || ''} ${c.page || ''}ページ`, `> ${plain(c.excerpt)}`, '',
        `試験の結果（${f.res.test.name}／${f.res.record}）`, ...checkedBy(f.res.aspect), `- 結果: ${f.res.aspect?.detail || f.res.aspect?.status_label || ''}`, `- 試験の案の合格の条件: ${f.res.test.pass.join('／')}`, '',
        `関係しそうなコード: ${(f.system_refs?.length ? f.system_refs : r.files).join('、')}`, ''];
    });
    pre.textContent = ['# 修正の依頼（mugiurushi の試験結果から）', '', `対象: ${state.source.repository} のコミット ${short(state.source.commit)}`, 'このリポジトリを、次の項目を満たすように修正してください。', '',
      ...(blocks.length ? blocks : ['（直す項目を選んでください）']),
      '## 進め方', '- 先に修正の方針を示してから、変更を小さく分けて行ってください。', '- 試験用の値や認証情報をコードに書き込まないでください。', '- 修正後は mugiurushi で同じ試験を実行し直します。修正しただけでは満たしたとは扱いません。'].join('\n');
  };
  build();
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main', style: 'flex:1 1 360px' }, h('h2', {}, '直す項目'), h('p', {}, '試験で不足を確認した項目から選びます。'),
      ng.length ? ng.map((/** @type {any} */ f) => { const box = h('input', { type: 'checkbox', checked: true }); box.addEventListener('change', () => { /** @type {HTMLInputElement} */ (box).checked ? chosen.add(f.id) : chosen.delete(f.id); build(); });
        return h('label', { style: 'display:flex;gap:8px;padding:8px;border:1px solid #D3D8DD;margin-bottom:6px' }, box, h('span', {}, h('strong', {}, f.requirement), h('span', { class: 'note', style: 'display:block;font-size:12px' }, `${f.citations?.[0]?.document || ''} ${f.citations?.[0]?.page || ''}ページ`))); }) : h('p', { class: 'note' }, '試験で不足を確認した項目はありません。'),
      h('p', { class: 'note', style: 'font-size:12px' }, '入れるもの：ガイドラインの該当箇所、期待と実際、試験の記録、関係しそうなコードの場所、対象のコミット。入れないもの：試験用の認証情報、生のログ。')),
    h('section', { class: 'side', style: 'flex:2 1 640px' }, h('h2', {}, 'あなたのAIに渡すプロンプト'),
      h('div', { class: 'actions', style: 'margin-top:0' },
        btn('コピー', () => guarded(async () => { await navigator.clipboard.writeText(pre.textContent || ''); setStatus('プロンプトをコピーしました。'); }), 'primary'),
        btn('.md で保存', () => { const a = h('a', { href: URL.createObjectURL(new Blob([pre.textContent || ''], { type: 'text/markdown' })), download: `fix-${short(state.source.commit)}.md` }); a.click(); })),
      pre, h('div', { class: 'actions' }, btn('修正をコミットしたら再試験へ', () => go(6), 'navy')))));
}

// ---------- 6 再試験 ----------
async function step6() {
  const records = Object.values(state.flow?.records || {}).filter((/** @type {any} */ x) => x.repository === state.source.repository && x.record?.results);
  const others = records.filter((/** @type {any} */ x) => x.commit !== state.source.commit);
  const ref = h('input', { type: 'text', id: 'ref', placeholder: 'ブランチ名・タグ・コミット', style: 'width:280px;padding:6px;border:1px solid #8A9199' });
  const current = state.record?.results || {};
  const prev = others[others.length - 1];
  const label = { ok: '期待どおり', ng: '不足を確認', pending: '未実行', unconfirmed: '未確認' };
  const rows = prev ? [...new Set([...Object.values(prev.record.results), ...Object.values(current)].map((/** @type {any} */ x) => x.requirement))].map(req => {
    const b = Object.values(prev.record.results).find((/** @type {any} */ x) => x.requirement === req);
    const a = Object.values(current).find((/** @type {any} */ x) => x.requirement === req);
    const change = b?.state === 'ng' && a?.state === 'ok' ? '解消' : b?.state === 'ok' && a?.state === 'ng' ? '新たに不足' : a?.state === 'ng' ? '変わらず不足' : '—';
    return h('tr', { 'data-compare': change }, h('td', {}, req), h('td', {}, label[b?.state] || '—'), h('td', {}, label[a?.state] || '—'), h('td', { style: change === '解消' ? 'color:#1F4E79;font-weight:700' : change.includes('不足') ? 'color:#A3261E;font-weight:700' : '' }, change));
  }) : [];
  fill($('main'), h('div', { class: 'pad' },
    h('h2', {}, '新しいコミットでの再試験'),
    h('p', {}, '前のコミットの結果は、新しいコミットには使いません。修正したコミットを読み込み、照合と試験をやり直して比べます。'),
    h('label', { for: 'ref', style: 'font-weight:700' }, '読み込むコミット'), ' ', ref, ' ',
    btn('読み込む', () => { const v = /** @type {HTMLInputElement} */ (ref).value.trim(); if (!v) return showError('ブランチ名・タグ・コミットを入力してください。'); const u = new URL(location.href); u.search = new URLSearchParams({ url: `https://github.com/${state.source.repository}`, ref: v }).toString(); location.href = u.toString(); }, 'primary'),
    prev ? [h('h3', {}, `前回 ${short(prev.commit)} と、今回 ${short(state.source.commit)} の比較`),
      h('div', { class: 'scroll' }, h('table', { 'data-compare-table': '' }, h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, '項目'), h('th', { scope: 'col', style: 'width:140px' }, `前回 ${short(prev.commit)}`), h('th', { scope: 'col', style: 'width:140px' }, `今回 ${short(state.source.commit)}`), h('th', { scope: 'col', style: 'width:120px' }, '変化'))), h('tbody', {}, rows)))]
      : h('p', { class: 'note' }, 'このリポジトリで比べられる前のコミットの結果はまだありません。')));
}

// ---------- 起動 ----------
async function init() {
  const p = new URLSearchParams(location.search);
  const url = p.get('url'), ref = p.get('ref') || 'HEAD';
  if (!url) { fill($('main'), h('div', { class: 'pad' }, h('h2', {}, '対象のリポジトリ'), h('p', {}, 'URLに ?url=https://github.com/所有者/名前&ref=ブランチ を付けて開いてください。'))); renderChromeEmpty(); return; }
  setStatus('リポジトリを読み込んでいます。');
  const res = await api('POST', '/api/v1/understand', { operation: 'open', url, reference: ref });
  state.source = res.source;
  state.agent = await api('GET', '/api/v1/agent').catch(() => null);
  if (state.agent?.status !== 'configured') showError('AIエージェントが設定されていません。運営者が EEL_AGENT_URL と EEL_AGENT_TOKEN_FILE を設定して起動してください。');
  state.flow = await api('GET', '/api/v1/flow');
  state.record = state.flow.records?.[key()]?.record || null;
  await loadCase();
  setStatus('');
  state.step = state.record?.approved && Object.keys(state.record.approved).length ? 4 : state.record?.plan ? 3 : state.record?.scan ? 2 : 1;
  render();
}
function renderChromeEmpty() { fill($('steps')); fill($('target')); }
guarded(init);
