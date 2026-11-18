// @ts-check
// ガイドラインの照合から試験・修正・再試験までの流れ。
// 判定と観測の状態は事例の記録（/api/v1/case）が正。流れの記録（/api/v1/flow）は画面を続けるための控え。
// AIへ送る内容は、送る前にサーバーが組み立てた内容をそのまま表示し、人が確かめたときだけ送る。
import { apiRequest as api } from './api.js';
import { applyScan, applyPlan, applyInvestigation, runConcerns, planEvidence, carryableConcerns, carriedInvestigation, byLatestSave, concernSource, setAnswer, answerTo, answersOf, scanGeneration, planGeneration, planIsCurrent, approve, approvedCheck, approvedTests, startStep, migrate, untestedFindings, UNTESTED } from './flow-record.js';

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
const SECTIONS = [['g3', '3省2ガイドライン'], ['pi', '個人情報保護法のガイドライン'], ['web', 'Webアプリケーションの技術基準'], ['dev', '医療機器'], ['nist', 'NISTの基準（参考）'], ['concern', 'コードから出た懸念'], ['other', 'その他']];
const PRIO = { high: '高', medium: '中', low: '低' };
const PRIO_ORDER = { high: 0, medium: 1, low: 2 };
const STEPS = ['1 照合の準備', '2 照合とヒアリング', '3 検証案の了承', '4 結果', '5 修正用のプロンプト', '6 再試験'];

/** @type {any} */
const state = { step: 1, source: null, view: null, flow: null, record: null, agent: null, sel: null };

// 記録者はこの画面では聞かない。名前が保存されていなければ、理解と試験の画面（ui/app.js）と同じ既定の名前を記録に使う。
function person() { try { return JSON.parse(sessionStorage.getItem('eel-person') || 'null')?.name || '開発者（本人確認なし）'; } catch { return '開発者（本人確認なし）'; } }
/** 読み込んだブランチ。コミットを直接指定して開いた場合はコミットを示す。 */
function branch() { const r = state.source?.reference || ''; return !r || r === state.source?.commit || /^[0-9a-f]{7,40}$/i.test(r) ? `コミット ${short(state.source?.commit || '')}` : `ブランチ ${r}`; }
function setStatus(/** @type {string} */ t) { $('status').textContent = t; }
function showError(/** @type {any} */ e) { fill($('alert'), h('div', { class: 'alert' }, e instanceof Error ? e.message : String(e))); }
async function guarded(/** @type {() => Promise<any>} */ f) { $('alert').replaceChildren(); try { return await f(); } catch (e) { showError(e); } }
function key() { return `${state.source.repository}@${state.source.commit}`; }
async function saveRecord() {
  // 保存日時を残す。引き継ぎ元と再試験の「前回」を、最後に保存した記録から選ぶため（byLatestSave）。
  if (state.record) state.record.saved_at = new Date().toISOString();
  state.flow = await api('POST', '/api/v1/flow', { repository: state.source.repository, commit: state.source.commit, record: state.record });
}
async function loadCase() { state.view = await api('GET', '/api/v1/case'); }

// ---------- 共通 ----------
function renderChrome() {
  const s = state.source;
  fill($('target'),
    h('div', {}, h('span', { class: 'k' }, 'リポジトリ'), '\u3000', h('strong', {}, s?.repository || '（未指定）')),
    h('div', {}, h('span', { class: 'k' }, '対象'), '\u3000', h('strong', {}, branch())),
    s?.reference && s.reference !== s.commit ? h('div', { class: 'note', 'data-loaded-commit': '' }, `読み込んだコミット ${short(s.commit)}`) : null,
    state.record?.files ? h('div', {}, h('span', { class: 'k' }, '読んだコード'), `\u3000${state.record.files.length}ファイル`) : null);
  const done = state.record || {};
  const approvedAny = approvedTests(done).length > 0;
  const reachable = [true, true, planIsCurrent(done), approvedAny, approvedAny, true];
  fill($('steps'), STEPS.map((t, i) => h('button', { type: 'button', 'aria-current': state.step === i + 1 ? 'step' : null, disabled: !reachable[i], onclick: () => go(i + 1) }, t)));
  $('foot-left').textContent = done.scan?.model_call?.response_id ? `照合の応答ID ${done.scan.model_call.response_id}` : '';
}
function go(/** @type {number} */ n) { state.step = n; render(); window.scrollTo(0, 0); }
function render() {
  renderChrome();
  const f = [null, step1, step2, step3, step4, step5, step6][state.step];
  guarded(async () => { await f(); });
}

/** 送る前の確認。サーバーが組み立てた内容をそのまま示し、確かめたときだけ送る。 */
/** 送る前の確認。送信先・送るファイル・説明文を示し、人が確かめてから送る。
 * SHA-256は人が読み比べる値ではないので「技術的な詳細」に置き、送るときは画面が同じ値を添える（AGENTS.md）。 */
function confirmAndSend(/** @type {HTMLElement} */ out, /** @type {string} */ path, /** @type {any} */ body, /** @type {(r: any) => any} */ onSent) {
  return guarded(async () => {
    setStatus('送る内容を組み立てています。');
    const preview = await api('POST', path, body);
    // 送信先のモデルは送る直前の状態を問い合わせる。運営者がAIエージェントの設定を変えている場合があるため。
    state.agent = await api('GET', '/api/v1/agent').catch(() => state.agent);
    setStatus('');
    fill(out);
    const dialog = /** @type {HTMLDialogElement} */ (h('dialog', { class: 'send-dialog', 'data-preview': '', 'aria-labelledby': 'send-title' }));
    const close = () => { dialog.close(); dialog.remove(); };
    const agree = h('input', { type: 'checkbox', id: 'agree' });
    const send = btn('この内容を送る', () => guarded(async () => {
      send.setAttribute('disabled', '');
      setStatus('AIエージェントに送っています。応答まで1分ほどかかることがあります。');
      // 失敗しても閉じる。エラーはダイアログの裏の通知欄に出るため、開いたままだと読めない。
      let r;
      try { r = await api('POST', path, { ...body, confirm: { payload_sha256: preview.payload_sha256, synthetic_only: agree.checked } }); }
      finally { close(); }
      await onSent(r); setStatus('');
    }), 'primary');
    send.setAttribute('disabled', '');
    const endpoint = state.agent?.endpoint;
    // モデルを確かめられない場合は送らせない。送信先を示してから送る規則のため（AGENTS.md）。
    const model = state.agent?.model;
    agree.addEventListener('change', () => agree.checked && model ? send.removeAttribute('disabled') : send.setAttribute('disabled', ''));
    const rows = [
      ['送信先', [endpoint ? ['照合用のAIエージェント ', h('span', { class: 'code' }, endpoint)] : '照合用のAIエージェント', model
        ? h('div', { 'data-agent-model': '' }, 'その先のモデル ', h('span', { class: 'code' }, model))
        : h('div', { class: 'alert', 'data-agent-model-missing': '' }, `その先のモデルを確かめられないため、送れません。${state.agent?.model_error || ''}`)]],
      body.files?.length ? ['ファイル', body.files.map((/** @type {string} */ f, /** @type {number} */ i) => [i ? '、' : '', h('span', { class: 'code' }, f)])] : null,
      body.summary ? ['説明文', body.summary] : null,
      body.question ? ['質問', body.question] : null,
      body.findings?.length ? ['確認項目', `${body.findings.length}件`] : null,
    ].filter(Boolean);
    fill(dialog,
      h('h2', { id: 'send-title' }, '送信内容の確認'),
      h('table', {}, h('tbody', {}, rows.map((/** @type {any} */ [k, v]) => h('tr', {}, h('th', { scope: 'row' }, k), h('td', {}, v))))),
      h('details', {}, h('summary', {}, '送る内容をすべて見る'), h('pre', { class: 'payload' }, JSON.stringify(preview.payload, null, 2))),
      h('details', { class: 'tech' }, h('summary', {}, '技術的な詳細'),
        h('p', { class: 'note' }, '送信記録と照らし合わせるための値（SHA-256）'), h('p', { class: 'code', 'data-payload-sha256': '' }, preview.payload_sha256),
        h('p', { class: 'note' }, `${preview.bytes} バイト`)),
      h('label', { class: 'choice' }, agree, '架空のデータだけで、実在の患者の情報・認証情報・生のログを含まないことを確かめた'),
      h('div', { class: 'actions dialog-actions' }, btn('戻る', close), send));
    document.body.append(dialog);
    dialog.addEventListener('cancel', close);
    dialog.showModal();
  });
}

// 送らないファイル。秘密の値を持つことが多い種類で、選べないようにする。
// サーバーも照合の送信で同じ種類を拒否する（src/code_tree.rs の DEFAULT_EXCLUSIONS）。
const SECRET = /(^|\/)(\.env(\.[^/]*)?|[^/]*\.(pem|key|p12|pfx|jks|keystore|kdbx)|id_(rsa|dsa|ecdsa|ed25519)(\.[^/]*)?|\.npmrc|\.pypirc|\.netrc|\.git-credentials)$/;
// 最初は外しておくファイル。全部を読む前提で、読んでも照合に役立たない依存・生成物・バイナリ・ロックファイルだけを外す。
const SKIP = /(^|\/)(node_modules|vendor|dist|build|\.git)\/|\.(png|jpe?g|gif|webp|ico|bmp|pdf|zip|gz|tgz|jar|war|woff2?|ttf|otf|eot|mp3|mp4|mov|wasm|exe|dll|so|dylib)$|(^|\/)(package-lock\.json|yarn\.lock|pnpm-lock\.yaml|Cargo\.lock|poetry\.lock|uv\.lock|Gemfile\.lock|composer\.lock)$/i;

// ---------- 1 照合の準備 ----------
// この段の操作は「送信内容を確認する」だけ。ファイルは最初から全部を選んでおき、外したいものだけを外す。
async function step1() {
  const inv = (state.source.inventory || []).filter((/** @type {any} */ f) => f.path);
  const chosen = new Set((state.record?.files || inv.filter((/** @type {any} */ f) => !SKIP.test(f.path)).map((/** @type {any} */ f) => f.path)).filter((/** @type {string} */ p) => !SECRET.test(p)));
  const count = h('span', {});
  // 大きなリポジトリでは一覧が長いので、パスの一部で絞り込む。外したファイルは絞り込みに関係なく先頭に出す。
  const LIMIT = 400;
  const tbody = h('tbody', {});
  const shown = h('span', { class: 'note', 'data-files-shown': '' });
  const filter = h('input', { type: 'search', id: 'file-filter', placeholder: '例：interface/forms/soap', style: 'width:320px' });
  const rowOf = (/** @type {any} */ f) => {
    const secret = SECRET.test(f.path);
    const box = h('input', { type: 'checkbox', checked: chosen.has(f.path), disabled: secret, 'aria-label': `${f.path}を読む` });
    box.addEventListener('change', () => { /** @type {HTMLInputElement} */ (box).checked ? chosen.add(f.path) : chosen.delete(f.path); update(); });
    return h('tr', { 'data-secret': secret ? '' : null }, h('td', { style: 'text-align:center' }, box),
      h('td', {}, h('span', { class: 'code' }, f.path), secret ? h('div', { class: 'note' }, '秘密の値を持つことが多い種類のファイルなので送りません') : null),
      h('td', { style: 'text-align:right' }, f.size ?? ''));
  };
  const list = () => {
    const q = /** @type {HTMLInputElement} */ (filter).value.trim().toLowerCase();
    const left = inv.filter((/** @type {any} */ f) => !chosen.has(f.path));
    const matched = inv.filter((/** @type {any} */ f) => chosen.has(f.path) && (!q || f.path.toLowerCase().includes(q)));
    fill(tbody, [...left, ...matched.slice(0, LIMIT)].map(rowOf));
    shown.textContent = `全${inv.length}件中、外した${left.length}件と、${q ? `「${q}」を含む` : ''}送る${Math.min(matched.length, LIMIT)}件を表示${matched.length > LIMIT ? `（残り${matched.length - LIMIT}件は絞り込んでください）` : ''}`;
  };
  filter.addEventListener('input', list);
  list();
  const update = () => { count.textContent = `送るファイル ${chosen.size}件`; };
  update();
  const desc = h('textarea', { rows: 4, id: 'desc' }); /** @type {HTMLTextAreaElement} */ (desc).value = state.record?.summary || '';
  const out = h('div', {});
  fill($('main'), h('section', { class: 'pad narrow' }, h('h2', {}, '照合するコード'),
    h('p', {}, `${branch()} のコードをガイドラインと照らし合わせます。外したいファイルの印を外してください。`),
    h('label', { for: 'file-filter', style: 'font-weight:700' }, 'ファイルの絞り込み（パスの一部）'), ' ', filter, ' ', shown,
    h('div', { class: 'scroll' }, h('table', { 'data-files': '' },
      h('thead', {}, h('tr', {}, h('th', { scope: 'col', style: 'width:56px;text-align:center' }, '送る'), h('th', { scope: 'col' }, 'ファイル'), h('th', { scope: 'col', style: 'width:100px;text-align:right' }, 'バイト'))),
      tbody)),
    h('p', { class: 'note' }, count),
    h('label', { for: 'desc', class: 'field-label' }, 'システムの説明（任意）'),
    h('p', { class: 'note' }, '例：使う人、扱う情報、提供の形'), desc,
    h('div', { class: 'actions' }, btn('送信内容を確認する', () => {
      const files = [...chosen];
      if (!files.length) return showError('送るファイルを1件以上選んでください。');
      state.record = { ...(state.record || {}), files, summary: /** @type {HTMLTextAreaElement} */ (desc).value.trim() };
      const answers = answersOf(state.record), at = scanGeneration(state.record);
      return confirmAndSend(out, '/api/v1/agent/flow-scan', { repository: state.source.repository, commit: state.source.commit, files, summary: state.record.summary, answers, ...screenOf(state.record) }, async (/** @type {any} */ r) => {
        applyScan(state.record, r, answers, at); await saveRecord(); go(2);
      });
    }, 'primary')), out));
}
// ---------- コードをたどって懸念を出す（#230）----------
// 対象の操作を出発点に、AIエージェントが求めた検索と読み取りに麦漆が固定したコミットから答える往復を回す。
// 送る中身は往復の前に決まらないので、最初に一度、麦漆が組み立てた範囲を確かめて了承する。
const BASIS = { repository: 'このリポジトリのコードと設定', external_default: 'リポジトリの外にある製品の既定の動き（このコードからは確かめていない）' };
const CHECK_BY = { read: 'コードをさらに読む', ask: '人に聞く', run: '麦漆で動かして確かめる' };
const STOPPED = { concluded: 'AIエージェントが結論を返した', rounds: '往復の上限に達した（AIエージェントは結論を返していない）', carried: '前のコミットで試験にした懸念を引き継いだ（このコミットのコードはたどり直していない）' };
function lineUrl(/** @type {string} */ path, /** @type {number} */ start, /** @type {number} */ end, /** @type {string | undefined} */ commit = undefined) {
  const s = state.source;
  return `https://github.com/${s.repository}/blob/${commit || s.commit}/${path.split('/').map(encodeURIComponent).join('/')}#L${start}-L${end}`;
}
function investigationView(/** @type {() => string} */ summary) {
  const saved = state.record?.investigation;
  const operation = h('textarea', { rows: 2, id: 'operation', placeholder: '例：患者の新規登録（国民識別番号・氏名・性別・年齢を入力して保存する）' });
  /** @type {HTMLTextAreaElement} */ (operation).value = saved?.operation || '';
  const rounds = h('input', { type: 'number', id: 'rounds', min: '1', max: '30', value: String(saved?.rounds_limit || 12), style: 'width:80px' });
  const exclusions = h('textarea', { rows: 2, id: 'exclusions', placeholder: '例：deploy/secrets/**（1行に1つ。既定の指定に足します）' });
  /** @type {HTMLTextAreaElement} */ (exclusions).value = (saved?.exclusions || []).join('\n');
  const out = h('div', {}, saved?.result ? investigationResult(saved) : null);
  const request = () => ({
    repository: state.source.repository, commit: state.source.commit,
    operation: /** @type {HTMLTextAreaElement} */ (operation).value.trim(), summary: summary(),
    exclusions: /** @type {HTMLTextAreaElement} */ (exclusions).value.split('\n').map(x => x.trim()).filter(Boolean),
    rounds: Number(/** @type {HTMLInputElement} */ (rounds).value) || 12,
  });
  const check = btn('範囲を確かめる', () => guarded(async () => {
    const body = request();
    if (!body.operation) return showError('対象の操作を書いてください。');
    const preview = await api('POST', '/api/v1/agent/investigate', body);
    const scope = preview.scope;
    const agree = h('input', { type: 'checkbox', id: 'agree-scope' });
    const start = btn('この範囲で始める', () => guarded(async () => {
      start.setAttribute('disabled', '');
      setStatus('コードをたどっています。往復の回数によっては数分かかります。');
      try {
        const result = await api('POST', '/api/v1/agent/investigate', { ...body, confirm: { scope_sha256: preview.scope_sha256 } });
        const record = { operation: body.operation, exclusions: body.exclusions, rounds_limit: body.rounds, scope_sha256: preview.scope_sha256, result, at: new Date().toISOString() };
        // 記録の物は差し替えない。往復の途中でほかの段へ移ると、その段の操作が今の記録の物を書き換えるため。
        // 前の懸念から作った試験の案は、履歴へ移す（applyInvestigation）。
        if (!state.record) state.record = {};
        applyInvestigation(state.record, record);
        await saveRecord();
        setStatus('');
        // 懸念を試験の案の候補として並べ直すため、この段を描き直す。
        if (state.step === 2) render(); else fill(out, investigationResult(record));
      } finally { start.removeAttribute('disabled'); }
    }), 'primary');
    start.setAttribute('disabled', '');
    agree.addEventListener('change', () => /** @type {HTMLInputElement} */ (agree).checked ? start.removeAttribute('disabled') : start.setAttribute('disabled', ''));
    fill(out, h('div', { class: 'box', 'data-scope': '' },
      h('div', { class: 'head' }, h('strong', {}, '送る範囲の確認'), h('span', { class: 'note' }, `送り先：${preview.destination}`)),
      h('div', { class: 'pad' },
        h('p', {}, preview.note),
        h('table', {}, h('tbody', {},
          h('tr', {}, h('th', { scope: 'row' }, 'リポジトリ'), h('td', {}, scope.repository)),
          h('tr', {}, h('th', { scope: 'row' }, 'コミット'), h('td', { class: 'code' }, scope.commit)),
          h('tr', {}, h('th', { scope: 'row' }, '対象の操作'), h('td', {}, scope.operation)),
          scope.summary ? h('tr', {}, h('th', { scope: 'row' }, '説明'), h('td', {}, scope.summary)) : null,
          h('tr', {}, h('th', { scope: 'row' }, '往復の上限'), h('td', {}, `${scope.limits.rounds}回（1回に${scope.limits.requests_per_round}件まで、資料の合計は${scope.limits.material_chars.toLocaleString('ja-JP')}文字まで）`)),
          h('tr', {}, h('th', { scope: 'row' }, '送らないパス'), h('td', { class: 'code', 'data-exclusions': '' }, scope.exclusions.join('、'))))),
        h('details', { class: 'tech' }, h('summary', {}, '技術的な詳細'), h('p', { class: 'note' }, '範囲のSHA-256'), h('p', { class: 'code', 'data-scope-sha256': '' }, preview.scope_sha256)),
        h('label', { style: 'display:flex;gap:8px;align-items:flex-start;margin-top:8px' }, agree, 'このリポジトリのこのコミットのコードを、送らないパスを除いて、AIが求めた分だけ送ることを了承する。送ってよいコードであることを確かめた'),
        h('div', { class: 'actions' }, start))));
  }));
  return h('div', { 'data-investigation-form': '' },
    h('h2', {}, '操作からコードをたどる（任意）'),
    h('p', {}, '確かめたい操作を書くと、AIがコードをたどって懸念を挙げます。'),
    h('label', { for: 'operation', style: 'font-weight:700' }, '対象の操作'), operation,
    h('div', { style: 'display:flex;gap:16px;flex-wrap:wrap;margin-top:8px' },
      h('div', {}, h('label', { for: 'rounds' }, '往復の上限（回）'), ' ', rounds),
      h('div', { style: 'flex:1;min-width:240px' }, h('label', { for: 'exclusions' }, '送らないパスの追加（任意）'), exclusions)),
    h('div', { class: 'actions' }, check, carryButton(out)), out);
}
/** 前のコミットで試験にした懸念を引き継ぐ。再試験で前後を比べるため（carryableConcerns）。 */
function carryButton(/** @type {HTMLElement} */ out) {
  const carry = carryableConcerns(state.flow?.records, state.source.repository, state.source.commit);
  if (!carry) return null;
  return btn(`前のコミット ${short(carry.from)} で試験にした懸念を引き継ぐ（${carry.concerns.length}件）`, () => guarded(async () => {
    const record = carriedInvestigation(carry);
    if (!state.record) state.record = {};
    applyInvestigation(state.record, record);
    await saveRecord();
    if (state.step === 2) render(); else fill(out, investigationResult(record));
  }));
}
const count = (/** @type {number|null|undefined} */ n) => typeof n === 'number' ? n.toLocaleString('ja-JP') : '不明';
function investigationResult(/** @type {any} */ saved) {
  const r = saved.result;
  const sent = (r.material || []).filter((/** @type {any} */ m) => m.kind !== 'refused').length;
  return h('div', { class: 'box', 'data-investigation': r.stopped_by },
    h('div', { class: 'head' }, h('strong', {}, `懸念 ${r.concerns.length}件`), h('span', { class: 'note' }, `${STOPPED[r.stopped_by] || r.stopped_by}。往復${r.rounds}回、送った資料${sent}件。記録 ${r.record}`)),
    h('div', { class: 'pad' },
      r.note ? h('p', { class: 'note' }, r.note) : null,
      r.limits ? h('p', { class: 'note' }, r.limits) : null,
      r.usage ? h('p', { class: 'note', 'data-usage': '' }, `使ったトークン：入力 ${count(r.usage.input_tokens)}（うちキャッシュから読んだ分 ${count(r.usage.cached_input_tokens)}、キャッシュに書いた分 ${count(r.usage.cache_write_tokens)}）、出力 ${count(r.usage.output_tokens)}。モデルの呼び出し${r.usage.calls}回`) : null,
      (r.concerns || []).map((/** @type {any} */ c) => h('div', { class: 'box', 'data-concern': c.id, style: 'margin-top:8px' },
        h('div', { class: 'pad' },
          h('p', {}, h('strong', {}, c.concern)),
          h('p', {}, `推定：${c.estimate}`),
          h('p', { class: 'note' }, `根拠の種類：${BASIS[c.basis] || c.basis}`),
          h('p', {}, '根拠の行：', (c.evidence || []).map((/** @type {any} */ e, /** @type {number} */ i) => [i ? '、' : '', h('a', { href: lineUrl(e.path, e.start, e.end, c.commit), target: '_blank', rel: 'noopener', 'data-evidence': '' }, `${e.path} ${e.start}〜${e.end}行${c.commit && c.commit !== state.source.commit ? `（コミット ${short(c.commit)}）` : ''}`)])),
          h('p', {}, `確かめ方：${CHECK_BY[c.check?.by] || c.check?.by}。${c.check?.detail || ''}${c.check?.observation ? `（観測の種類：${c.check.observation}）` : ''}`)))),
      (r.questions || []).length ? [h('h3', {}, 'コードからは決められなかったこと'), h('ul', { class: 'items' }, r.questions.map((/** @type {string} */ q) => h('li', {}, q)))] : null));
}

// ---------- 懸念を試験の案の候補にする（#230）----------
// 確かめ方が「麦漆で動かす」の懸念だけを候補にする。「人に聞く」「さらに読む」の懸念は、試験にしても麦漆で観測できないので示すだけにする。
// 懸念は「こうなると問題」という形なので、候補の要求の文は「その懸念が起きていないこと」にする。
const concernChosen = new Map();
function concernRequirement(/** @type {any} */ c) { return `次のことが起きていないこと：${c.concern}`.slice(0, 2000); }
function evidenceText(/** @type {any} */ c) {
  const at = c.commit && c.commit !== state.source.commit ? `（コミット ${short(c.commit)}）` : '';
  return (c.evidence || []).map((/** @type {any} */ e) => `${e.path} ${e.start}〜${e.end}行`).join('、') + at;
}
function concernCandidate(/** @type {any} */ c) {
  return { id: c.id, requirement: concernRequirement(c), how: `${c.check?.detail || ''}（コードからの推定：${c.estimate}。根拠の行：${evidenceText(c)}）`.slice(0, 2000), source: 'concern' };
}
function chosenConcerns(/** @type {any} */ r) {
  const id = r?.investigation?.result?.record;
  return runConcerns(r).filter((/** @type {any} */ c) => concernChosen.get(`${id}:${c.id}`) !== false);
}
function concernsView(/** @type {any} */ r) {
  const id = r?.investigation?.result?.record;
  const all = r?.investigation?.result?.concerns || [];
  if (!all.length) return null;
  const others = all.filter((/** @type {any} */ c) => c.check?.by !== 'run');
  return h('div', { 'data-concerns': '' },
    h('h2', {}, 'コードから出た懸念'),
    h('p', { class: 'note' }, `対象の操作：${r.investigation.operation}`),
    runConcerns(r).length ? h('table', {}, h('tbody', {}, runConcerns(r).map((/** @type {any} */ c) => {
      const box = h('input', { type: 'checkbox', checked: concernChosen.get(`${id}:${c.id}`) !== false, 'aria-label': `${c.id}を試験の案の候補にする` });
      box.addEventListener('change', () => concernChosen.set(`${id}:${c.id}`, /** @type {HTMLInputElement} */ (box).checked));
      return h('tr', { 'data-concern-candidate': c.id },
        h('td', { style: 'width:48px;text-align:center' }, box),
        h('td', {}, c.concern, h('div', { class: 'note' }, `確かめ方：${c.check.detail}（観測の種類：${c.check.observation}）。根拠の行：${evidenceText(c)}`)));
    }))) : h('p', { class: 'note' }, '動かして確かめる懸念はありません。'),
    others.length ? h('div', { 'data-concerns-other': '' },
      h('h3', {}, '試験にしない懸念'),
      h('ul', { class: 'items' }, others.map((/** @type {any} */ c) => h('li', {}, `${c.concern}（確かめ方：${CHECK_BY[c.check?.by] || c.check?.by}。${c.check?.detail || ''}）`)))) : null);
}
// 画面の記録を添えた照合は、照合し直しと試験の案でも同じ試行を添える。
function screenOf(/** @type {any} */ r) { return r?.screen_evidence ? { screen_evidence: r.screen_evidence } : {}; }
function testable() { return (state.record?.scan?.findings || []).filter((/** @type {any} */ f) => f.verification?.executable_check); }
/** 結果・修正用のプロンプト・再試験で扱う項目。照合の候補と、今の案に写した懸念。 */
function resultItems() {
  const r = state.record;
  const concerns = planIsCurrent(r) ? (r.plan.concerns || []) : [];
  return [...testable(), ...concerns.map((/** @type {any} */ c) => ({ id: c.id, requirement: concernRequirement(c), concern: c }))];
}
function sectionOf(/** @type {any} */ f) {
  if (f.concern) return 'concern';
  const ids = (f.citations || []).map((/** @type {any} */ c) => SECTION_OF[c.source_id] || 'other');
  return SECTIONS.map(([k]) => k).find(k => ids.includes(k)) || 'other';
}
function bySection(/** @type {any[]} */ items) {
  return SECTIONS.map(([k, name]) => ({ key: k, name, rows: items.filter(f => sectionOf(f) === k).sort((a, b) => (PRIO_ORDER[a.priority] ?? 3) - (PRIO_ORDER[b.priority] ?? 3)) })).filter(s => s.rows.length);
}
function prio(/** @type {string} */ p) { return p ? h('span', { class: `prio ${p}` }, `優先度 ${PRIO[p] || p}`) : h('span', { class: 'note' }, '—'); }
/** 候補を挙げた根拠の一節と、その文書・ページ。 */
function basisView(/** @type {any} */ f) {
  const cs = (f.citations || []).filter((/** @type {any} */ c) => c.excerpt);
  if (!cs.length) return null;
  return h('details', { class: 'basis', 'data-basis': '' }, h('summary', {}, '根拠を読む'),
    cs.map((/** @type {any} */ c) => [h('blockquote', {}, plain(c.excerpt)), h('p', { class: 'note' }, [c.document, c.page ? `${c.page}ページ` : ''].filter(Boolean).join('・'))]));
}

// ---------- 2 照合とヒアリング ----------
async function step2() {
  if (!state.record) state.record = {};
  const r = state.record;
  const out = h('div', {});
  const askOut = h('div', {});
  const ask = h('textarea', { rows: 3, id: 'ask', placeholder: '例：ログの保存期間はどれくらい必要ですか' });
  const planButton = btn('試験の案を作る', () => {
    const concerns = chosenConcerns(r);
    const findings = [
      ...testable().map((/** @type {any} */ f) => ({ id: f.id, requirement: f.requirement, how: f.verification.how || '' })),
      ...concerns.map(concernCandidate),
    ];
    if (!findings.length) return showError('試験で確かめられる項目がありません。');
    if (findings.length > 24) return showError('試験の案の候補は24件までです。懸念の選択を減らしてください。');
    const at = planGeneration(r);
    return confirmAndSend(out, '/api/v1/agent/plan', { repository: state.source.repository, commit: state.source.commit, files: r.files || [], evidence: planEvidence(concerns), summary: r.summary || '', answers: answersOf(r), findings, ...screenOf(r) }, async (/** @type {any} */ p) => { applyPlan(r, p, at, concerns); await saveRecord(); go(3); });
  }, 'primary');
  const scanMain = r.scan ? [
    h('h2', {}, 'ガイドラインから見つかった確認候補'),
    h('p', { class: 'note', 'data-scan-count': '' }, `候補 ${(r.scan.findings || []).length}件のうち、試験で確かめられる ${testable().length}件`),
    testable().length ? bySection(testable()).map(s => h('div', { 'data-section': s.key }, h('h3', {}, s.name),
      h('ul', { class: 'candidates' }, s.rows.map((/** @type {any} */ f) => h('li', { 'data-finding': f.id },
        h('div', { class: 'candidate' }, prio(f.priority), h('div', { class: 'candidate-text' }, f.requirement),
          f.basis?.includes('screen') ? h('div', { class: 'note', 'data-basis-screen': '' }, '根拠に、試験で記録した画面の構造を含みます。') : null,
          basisView(f))))))) : h('p', { class: 'note' }, '試験で確かめられる候補は見つかりませんでした。')] : [
    h('h2', {}, '試験の案の候補'),
    h('p', { class: 'note', 'data-no-scan': '' }, '照合はまだしていません。')];
  const scanSide = r.scan ? [
    r.scan.questions?.length ? [r.scan.questions.map((/** @type {any} */ q) => h('fieldset', { 'data-question': q.id }, h('legend', {}, q.text),
        h('div', { style: 'display:flex;flex-wrap:wrap;gap:12px;margin-top:4px' }, q.options.map((/** @type {string} */ o) => {
          const radio = h('input', { type: 'radio', name: q.id, checked: answerTo(r, q) === o });
          radio.addEventListener('change', () => setAnswer(r, q, o));
          return h('label', { style: 'display:inline-flex;gap:6px;align-items:center' }, radio, o);
        }))))] : h('p', { class: 'note' }, '確認したいことはありません。'),
    h('details', { 'data-ask': '', open: (r.asks || []).length > 0 }, h('summary', {}, 'ガイドラインに質問する'),
      h('label', { for: 'ask', class: 'field-label' }, '聞きたいこと'), ask,
      h('div', { class: 'actions' }, btn('質問を送る', () => {
        const q = /** @type {HTMLTextAreaElement} */ (ask).value.trim(); if (!q) return showError('質問を書いてください。');
        return confirmAndSend(askOut, '/api/v1/agent/ask', { question: q }, async (/** @type {any} */ a) => { r.asks = [...(r.asks || []), { question: q, ...a }]; await saveRecord(); render(); });
      })), askOut),
    (r.asks || []).slice().reverse().map((/** @type {any} */ a) => h('div', { class: 'box', 'data-asked': '' }, h('div', { class: 'pad' }, h('strong', {}, `質問：${a.question}`), h('div', {}, a.answer),
      a.citations?.map((/** @type {any} */ c) => h('div', { class: 'note' }, `出典：${c.document}・${c.page}ページ`)))))] : [];
  const askConcerns = (r.investigation?.result?.concerns || []).filter((/** @type {any} */ c) => c.check?.by === 'ask');
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main' }, scanMain, concernsView(r),
      h('details', { class: 'trace', 'data-trace': '', open: !r.scan || !!r.investigation }, h('summary', {}, '操作からコードをたどる'), investigationView(() => r.summary || ''))),
    h('section', { class: 'side' }, h('h2', {}, 'ヒアリング'),
      scanSide,
      askConcerns.length ? h('div', { 'data-ask-concerns': '' }, h('h3', {}, '懸念のうち、人に聞いて確かめること'),
        h('ul', { class: 'items' }, askConcerns.map((/** @type {any} */ c) => h('li', {}, `${c.check.detail}（懸念：${c.concern}）`)))) : null,
      !r.scan && !askConcerns.length ? h('p', { class: 'note' }, '確認したいことはありません。') : null,
      h('div', { class: 'actions stack' }, planButton,
        r.scan ? btn('回答して照合し直す', () => { const answers = answersOf(r), at = scanGeneration(r); return confirmAndSend(out, '/api/v1/agent/flow-scan', { repository: state.source.repository, commit: state.source.commit, files: r.files, summary: r.summary, answers, ...screenOf(r) }, async (/** @type {any} */ s) => { applyScan(r, s, answers, at); await saveRecord(); render(); }); }) : null),
      r.plan ? h('p', { class: 'note', 'data-rescan-note': '' }, '照合し直すと、今の試験の案と結果は履歴に移ります。') : null,
      r.history?.length ? h('p', { class: 'note' }, `前の照合・試験の案 ${r.history.length}件を履歴に残しています。`) : null, out)));
}

// ---------- 3 検証案の了承 ----------
async function step3() {
  const r = state.record; const findings = candidatesOf(r);
  if (!planIsCurrent(r)) return go(2);
  const out = h('div', {});
  const done = (/** @type {any} */ t) => approvedCheck(r, t);
  const covered = new Set(r.plan.tests.flatMap((/** @type {any} */ t) => t.finding_ids));
  fill($('main'), h('div', { class: 'pad narrow' },
    h('h2', {}, '試験の案'),
    h('p', { class: 'note', 'data-plan-count': '' }, `${covered.size}つの確認項目を、${r.plan.tests.length}件の試験で確かめます。`),
    r.plan.tests.map((/** @type {any} */ t) => {
      const box = h('input', { type: 'checkbox', checked: r.chosen?.[t.id] !== false, disabled: !!done(t) });
      box.addEventListener('change', () => { r.chosen = { ...(r.chosen || {}), [t.id]: /** @type {HTMLInputElement} */ (box).checked }; });
      return h('div', { class: 'box', 'data-test': t.id },
        h('div', { class: 'head' }, h('label', { class: 'test-name' }, box, t.name),
          done(t) ? h('span', { style: 'margin-left:auto' }, h('a', { href: workbenchUrl(/** @type {string} */ (done(t))) }, '登録済み：試験の設定を開く')) : null),
        h('div', { class: 'test-body' },
          h('div', {}, h('h3', {}, '手順'), h('ol', {}, t.steps.map((/** @type {string} */ s) => h('li', {}, s)))),
          h('div', { class: 'test-grid' },
            h('div', {}, h('h3', {}, '確認項目'), h('ul', { 'data-test-items': '' }, t.finding_ids.map((/** @type {string} */ i) => h('li', {}, findings[i]?.requirement || i)))),
            h('div', {}, h('h3', {}, '合格の条件'), h('ul', {}, t.pass.map((/** @type {string} */ p) => h('li', {}, p)))))));
    }),
    r.plan.not_planned?.length ? h('p', { class: 'note', 'data-not-planned': '' }, `試験の案に入らなかった項目：${r.plan.not_planned.map((/** @type {any} */ n) => `${findings[n.finding_id]?.requirement || n.finding_id}（AIの理由：${n.reason || '理由の記載なし'}）`).join('、')}`) : null,
    r.plan.unclassified?.length ? h('p', { class: 'note', 'data-unclassified': '' }, `試験の案に含まれず、理由もない項目：${r.plan.unclassified.map((/** @type {string} */ id) => findings[id]?.requirement || id).join('、')}`) : null,
    h('div', { class: 'actions' }, btn('了承して試験を設定する', () => guarded(async () => {
      if (registering) return; // 連打で同じ試験を二重に登録しない
      const tests = r.plan.tests.filter((/** @type {any} */ t) => r.chosen?.[t.id] !== false && !done(t));
      if (!tests.length) throw Error('了承する試験を選んでください。');
      registering = true; setStatus('確認項目に登録しています。');
      try {
        // 1件ずつ登録して控える。途中で失敗しても、登録できた分は了承済みとして残り、やり直しで二重に作らない。
        for (const t of tests) { approve(r, t, await register(t, findings)); await saveRecord(); }
      } finally { registering = false; setStatus(''); }
      go(4);
    }), 'primary'), out)));
}
/** 今の案の候補（照合の候補と、案に写した懸念）。懸念は、要求の文を「懸念が起きていないこと」にして、元の懸念を持たせる。 */
function candidatesOf(/** @type {any} */ r) {
  return Object.fromEntries([
    ...(r.scan?.findings || []).map((/** @type {any} */ f) => [f.id, f]),
    ...(r.plan?.concerns || []).map((/** @type {any} */ c) => [c.id, { id: c.id, requirement: concernRequirement(c), concern: c }]),
  ]);
}
/** 試験の結果をコードへたどれるように、案の候補が関係するとしたファイル（なければ照合で読んだ最初のファイル）を対応づける。
 * 懸念の候補は、根拠の最初の行の範囲を対応づける。 */
function codeRef(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  const concerns = t.finding_ids.map((/** @type {string} */ id) => findings[id]?.concern).filter(Boolean);
  const paths = (state.source.inventory || []).map((/** @type {any} */ f) => f.path).filter(Boolean);
  const at = concerns.length ? concernSource(concerns, state.source.commit, paths) : null;
  if (at) return { repository: state.source.repository, commit: state.source.commit, ...at };
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
let registering = false;
async function register(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  const r = state.record;
  const base = `flow-${short(state.source.commit)}-${t.id.replace(/[^a-z0-9-]/gi, '').slice(0, 20)}`;
  const call = r.plan.model_call || {};
  const kinds = new Set(t.finding_ids.map((/** @type {string} */ id) => findings[id]?.concern ? 'concern' : 'guideline'));
  const label = kinds.has('concern') ? (kinds.has('guideline') ? 'ガイドライン照合とコードから出た懸念の試験の案' : 'コードから出た懸念の試験の案') : 'ガイドライン照合の試験の案';
  const origin = `${label}（${call.response_model || call.requested_model || 'AI'}・応答ID ${call.response_id || '不明'}）を${person()}が了承して登録。対象 ${state.source.repository}@${short(state.source.commit)}`;
  const text = `${t.name}：${t.pass.join('／')}`.slice(0, 1000);
  await loadCase();
  const checks = state.view.workspace.checks;
  // 前回の登録が確認項目を作ったところで止まっていたら（同じ案・同じ了承者・部分が未登録）、作り直さずに続きから登録する。
  const partial = checks.find((/** @type {any} */ c) => (c.id === base || c.id.startsWith(base + '-')) && c.text === text && c.origin === origin && !(c.aspects || []).length);
  let id = partial?.id || base;
  if (!partial) {
    while (checks.some((/** @type {any} */ c) => c.id === id)) id += '-2';
    await change({ op: 'add_check', check: { id, version: 1, text, origin, source: codeRef(t, findings), observation: null, rule: 'manual_only', dependencies: [], questions: [] } }, `${label}を了承した`);
  }
  const aspects = t.finding_ids.map((/** @type {string} */ fid) => {
    const f = findings[fid]; const c = f.citations?.[0] || {};
    const origin = f.concern
      ? `コードから出た懸念（コードからの推定で、観測でも判定でもない）。根拠の行：${evidenceText(f.concern)}。調べの記録 ${r.plan.investigation}`
      : `照合の候補。出典：${c.document || ''} ${c.page || ''}ページ（SHA-256 ${(c.sha256 || '').slice(0, 12)}…）`;
    return { id: fid, text: f.requirement.slice(0, 2000), origin: origin.slice(0, 1000), observation: t.observation, trial_checks: [] };
  });
  await change({ op: 'set_aspects', check_id: id, aspects }, '試験で確かめる項目を登録した');
  return id;
}

// ---------- 4 結果 ----------
function resultOf(/** @type {any} */ f) {
  const r = state.record;
  // 今の照合から作った今の案で、了承した中身のまま登録した試験だけを見る。前の照合・案の確認項目の観測は使わない。
  const test = approvedTests(r).find((/** @type {any} */ t) => t.finding_ids.includes(f.id));
  if (!test) return { state: 'none' };
  const checkId = /** @type {string} */ (approvedCheck(r, test));
  const cv = state.view.checks.find((/** @type {any} */ c) => c.check_id === checkId);
  const a = cv?.coverage?.aspects?.find((/** @type {any} */ x) => x.id === f.id);
  const applicable = (cv?.evidence || []).filter((/** @type {any} */ e) => e.applicable);
  const ev = applicable.find((/** @type {any} */ e) => e.id === cv.selected_evidence) || applicable[0];
  // 状態は事例の記録の値だけで決める。実行していない試験を成功・失敗として表示しない。
  // 実行したが判定条件に対応づけていない部分などは「未確認」とし、理由（mugiurushi の状態の説明）を添える。
  // 設定画面で同梱の試験（固定した版のレシピ）を選ぶと、確認項目の参照先がその版へ変わる。照合したコミットと違う版の結果は、
  // このコミットの不足・期待どおりとして数えない。
  const bound = state.view.workspace?.checks?.find((/** @type {any} */ c) => c.id === checkId)?.source?.commit;
  const otherVersion = !!bound && bound !== state.source.commit;
  const st = otherVersion ? 'other_version' : !ev ? 'pending' : a?.status === 'observed' ? 'ok' : a?.status === 'contradicted' ? 'ng' : 'unconfirmed';
  const linked = (a?.trial_checks || []).length > 0;
  return { state: st, checkId, test, aspect: a, linked, boundCommit: bound, record: ev ? `試行 ${ev.run_id}` : '—' };
}
/** 抜粋の表示用に、Markdownの強調記号と余分な空白を外す。 */
function plain(/** @type {string} */ text) { return (text || '').replace(/\*\*/g, '').replace(/(^|\s)\*(?=\s|$)/g, '$1・').replace(/\s+・?\s*$/, '').replace(/\s+/g, ' ').trim(); }
/** 実際に確かめた判定条件（mugiurushi の試験設定の文言）。対応づけた判定条件が無ければ書かない。 */
function checkedBy(/** @type {any} */ aspect) {
  return (aspect?.trial_checks || []).map((/** @type {any} */ l) => `- 確かめた判定条件: ${l.id} ${l.label || ''}`.trim());
}
const NEXT = { pending: '試験を設定して実行する', unconfirmed: '判定条件に対応づける', ng: '試験の詳細を開く', ok: '試験の詳細を開く', other_version: '試験の詳細を開く' };
function stateLabel(/** @type {any} */ res) {
  if (res.state === 'other_version') return `別の版の試験：確認項目の参照先が ${short(res.boundCommit)} に変わっている（照合したのは ${short(state.source.commit)}）。このコミットの結果としては数えない`;
  if (res.state === 'pending') return res.linked ? '未実行' : '未実行（判定条件への対応づけ前）';
  if (res.state === 'unconfirmed') return `未確認：${res.aspect?.status_label || '状態を読み取れない'}`;
  return res.state === 'ok' ? '期待どおり' : '';
}
async function step4() {
  await loadCase();
  const r = state.record;
  const items = resultItems().map((/** @type {any} */ f) => ({ ...f, res: resultOf(f) })).filter((/** @type {any} */ f) => f.res.state !== 'none');
  r.results = Object.fromEntries(items.map((/** @type {any} */ f) => [f.id, { requirement: f.requirement, state: f.res.state, ...(f.concern ? { kind: 'concern' } : {}) }]));
  await saveRecord();
  const ng = items.filter((/** @type {any} */ f) => f.res.state === 'ng').length;
  const pending = [...new Set(items.filter((/** @type {any} */ f) => f.res.state === 'pending').map((/** @type {any} */ f) => f.res.checkId))];
  if (!state.sel || !items.find((/** @type {any} */ f) => f.id === state.sel)) state.sel = items.find((/** @type {any} */ f) => f.res.state !== 'ok')?.id || items[0]?.id;
  const sel = items.find((/** @type {any} */ f) => f.id === state.sel);
  const detail = h('section', { class: 'side', 'data-detail': '' });
  const showDetail = (/** @type {any} */ f) => {
    if (!f) return fill(detail, h('p', { class: 'note' }, '項目を選ぶと詳細を表示します。'));
    const c = f.citations?.[0] || {};
    const k = f.concern;
    // 懸念はガイドラインの出典を持たない。代わりに、懸念・コードからの推定・根拠の行を示す。
    const basis = k ? [
      h('div', { style: 'font-weight:700' }, 'コードから出た懸念'), h('blockquote', { 'data-detail-concern': '' }, k.concern),
      h('div', { class: 'note', style: 'margin:4px 0 10px' }, 'コードからの推定で、観測でも判定でもありません。試験の結果が、この懸念が起きているかどうかを示します。'),
      h('table', {}, h('tbody', {},
        h('tr', {}, h('th', { scope: 'row' }, '推定'), h('td', {}, k.estimate)),
        h('tr', {}, h('th', { scope: 'row' }, '根拠の行'), h('td', {}, (k.evidence || []).map((/** @type {any} */ e, /** @type {number} */ i) => [i ? '、' : '', h('a', { href: lineUrl(e.path, e.start, e.end, k.commit), target: '_blank', rel: 'noopener' }, `${e.path} ${e.start}〜${e.end}行`)]), k.commit && k.commit !== state.source.commit ? `（コミット ${short(k.commit)}）` : '')),
        h('tr', {}, h('th', { scope: 'row' }, '根拠の種類'), h('td', {}, BASIS[k.basis] || k.basis))))] : [
      h('div', { style: 'font-weight:700' }, 'ガイドラインの該当箇所（抜粋）'), h('blockquote', {}, plain(c.excerpt)),
      h('div', { class: 'note', style: 'margin:4px 0 10px' }, `${c.document || ''} ${c.page || ''}ページ`)];
    fill(detail, h('h2', {}, '詳細'), h('div', { class: 'detail-title' }, f.requirement),
      basis,
      h('table', {}, h('tbody', {},
        k ? null : h('tr', {}, h('th', { scope: 'row' }, '優先度'), h('td', {}, prio(f.priority), ' ', f.patient_data ? '患者の情報に関わる' : '', f.relevance === 'likely' ? '・該当の見込みが高い' : '')),
        h('tr', {}, h('th', { scope: 'row' }, '試験'), h('td', {}, f.res.test.name)),
        h('tr', {}, h('th', { scope: 'row' }, '手順'), h('td', {}, h('ol', { style: 'margin:0;padding-left:18px' }, f.res.test.steps.map((/** @type {string} */ s) => h('li', {}, s))))),
        h('tr', {}, h('th', { scope: 'row' }, '合格の条件'), h('td', {}, f.res.test.pass.join('／'))),
        h('tr', {}, h('th', { scope: 'row' }, '結果'), h('td', { style: f.res.state === 'ng' ? 'color:var(--error);font-weight:700' : '' }, f.res.state === 'other_version' ? stateLabel(f.res) : f.res.state === 'pending' ? 'まだ実行していない' : f.res.state === 'ng' ? [h('div', {}, happened(f)), f.res.aspect?.detail ? h('div', { class: 'note', style: 'color:var(--text-secondary);font-weight:400' }, f.res.aspect.detail) : null] : f.res.aspect ? `${f.res.aspect.status_label}。${f.res.aspect.detail || ''}` : '—')),
        h('tr', {}, h('th', { scope: 'row' }, '記録'), h('td', {}, f.res.record)))),
      h('div', { class: 'actions' }, h('a', { class: 'btn navy', href: workbenchUrl(f.res.checkId) }, NEXT[f.res.state] || '試験の詳細を開く')));
  };
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main' },
      h('div', { style: 'display:flex;flex-wrap:wrap;gap:8px;align-items:center;justify-content:space-between' }, h('h2', { style: 'flex:1' }, resultHeading(items)),
        ng ? h('span', { class: 'ng', 'data-ng-count': String(ng) }, `期待と異なる ${ng}件`) : null),
      pending.length ? h('p', {}, `まだ実行していない試験が${pending.length}件あります。`) : null,
      bySection(items.filter((/** @type {any} */ f) => f.res.state !== 'ok')).map(s => h('div', { 'data-result-section': s.key }, h('h3', {}, s.name),
        h('table', {}, h('thead', {}, h('tr', {}, h('th', { scope: 'col', style: 'width:56px' }, '優先度'), h('th', { scope: 'col' }, '起きたこと'), h('th', { scope: 'col', style: 'width:150px' }, '状態'), h('th', { scope: 'col', style: 'width:220px' }, '次にやること'), h('th', { scope: 'col', style: 'width:64px' }, h('span', { style: 'position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)' }, '詳細')))),
          h('tbody', {}, s.rows.map((/** @type {any} */ f) => h('tr', { class: f.id === state.sel ? 'sel' : f.res.state === 'ng' ? 'rowng' : '', 'data-result': f.res.state },
            h('td', { style: 'text-align:center' }, prio(f.priority)),
            h('td', {}, f.res.state === 'ng' ? [h('div', { 'data-happened': '' }, happened(f)), h('div', { class: 'note' }, `確認項目：${f.requirement}`)] : f.requirement),
            h('td', { 'data-state-label': '' }, f.res.state === 'ng' ? h('span', { class: 'ng' }, '期待と異なる') : stateLabel(f.res)),
            h('td', {}, f.res.state === 'ng' ? '直す' : h('a', { href: workbenchUrl(f.res.checkId) }, NEXT[f.res.state]), h('div', { class: 'note' }, f.res.test.name)),
            h('td', { style: 'text-align:center' }, btn('詳細', () => { state.sel = f.id; render(); }, f.id === state.sel ? 'primary' : '')))))))),
      items.filter((/** @type {any} */ f) => f.res.state === 'ok').length ? h('p', { class: 'note' }, `試験で期待どおりだった項目：${items.filter((/** @type {any} */ f) => f.res.state === 'ok').map((/** @type {any} */ f) => f.requirement).join('、')}`) : null,
      untestedView(),
      h('div', { class: 'actions' }, ng ? btn('修正用のプロンプトを作る', () => go(5), 'primary') : null, btn('結果を読み直す', () => render()))),
    detail));
  showDetail(sel);
}

/** 結果の見出し。 */
function resultHeading(/** @type {any[]} */ _items) { return '試験で起きたこと'; }
/** 試験で起きたこと。期待と異なった判定条件の説明（範囲と期待）を使う。要求の文（期待した動作）を結果として書かない。 */
function happened(/** @type {any} */ f) {
  const a = f.res.aspect;
  // 複数の判定条件を対応づけた場合、詳細には期待どおりの条件も並ぶ（src/coverage.rs の status_of）。期待と異なると書かれた条件だけを選び、見つからなければ一般の文にする。
  const id = /判定条件 (\S+?)（[^）]*）: 期待と異なる/.exec(a?.detail || '')?.[1];
  const label = (a?.trial_checks || []).find((/** @type {any} */ l) => l.id === id)?.label;
  return label ? `合格の条件「${label}」と異なる結果を観測した` : '期待と異なる結果を観測した';
}

/** 試験にしていない候補。未実行（了承して登録した試験をまだ実行していない）や未確認とは分け、合否は付けない。 */
function untestedView() {
  const rows = untestedFindings(state.record, resultItems());
  if (!rows.length) return null;
  return h('div', { 'data-untested-section': '' }, h('h3', {}, `試験にしていない候補（${rows.length}件・合否は付けていません）`),
    h('p', { class: 'note' }, '試験で確かめていません。'),
    h('table', {}, h('thead', {}, h('tr', {}, h('th', { scope: 'col', style: 'width:56px' }, '優先度'), h('th', { scope: 'col' }, '項目'), h('th', { scope: 'col', style: 'width:300px' }, '試験にしていない理由'))),
      h('tbody', {}, rows.map(u => h('tr', { 'data-untested': u.kind },
        h('td', { style: 'text-align:center' }, prio(u.finding.priority)),
        h('td', {}, u.finding.requirement),
        h('td', {}, /** @type {any} */ (UNTESTED)[u.kind], u.reason ? h('div', { class: 'note' }, `AIの理由：${u.reason}`) : null))))));
}

// ---------- 5 修正用のプロンプト ----------
async function step5() {
  await loadCase();
  const r = state.record;
  const ng = resultItems().map((/** @type {any} */ f) => ({ ...f, res: resultOf(f) })).filter((/** @type {any} */ f) => f.res.state === 'ng');
  const chosen = new Set(ng.map((/** @type {any} */ f) => f.id));
  const pre = h('pre', { class: 'prompt', 'data-prompt': '' });
  const build = () => {
    const blocks = ng.filter((/** @type {any} */ f) => chosen.has(f.id)).flatMap((/** @type {any} */ f) => {
      const c = f.citations?.[0] || {};
      // 懸念の項目は、ガイドラインの代わりに、懸念・コードからの推定・根拠の行を入れる。
      const basis = f.concern
        ? [`根拠: コードから出た懸念（コードからの推定で、観測でも判定でもない）`, `> ${f.concern.concern}`, `- コードからの推定: ${f.concern.estimate}`, `- 根拠の行: ${evidenceText(f.concern)}`]
        : [`根拠: ${c.document || ''} ${c.page || ''}ページ`, `> ${plain(c.excerpt)}`];
      return [`## ${f.requirement}`, '', ...basis, '',
        `試験の結果（${f.res.test.name}／${f.res.record}）`, ...checkedBy(f.res.aspect), `- 結果: ${f.res.aspect?.detail || f.res.aspect?.status_label || ''}`, `- 試験の案の合格の条件: ${f.res.test.pass.join('／')}`, '',
        `関係しそうなコード: ${(f.concern ? [...new Set((f.concern.evidence || []).map((/** @type {any} */ e) => e.path))] : f.system_refs?.length ? f.system_refs : r.files || []).join('、')}`, ''];
    });
    pre.textContent = ['# 修正の依頼（mugiurushi の試験結果から）', '', `対象: ${state.source.repository} のコミット ${short(state.source.commit)}`, 'このリポジトリを、次の項目を満たすように修正してください。', '',
      ...(blocks.length ? blocks : ['（直す項目を選んでください）']),
      '## 進め方', '- 先に修正の方針を示してから、変更を小さく分けて行ってください。', '- 試験用の値や認証情報をコードに書き込まないでください。', '- 修正後は mugiurushi で同じ試験を実行し直します。修正しただけでは満たしたとは扱いません。'].join('\n');
  };
  build();
  fill($('main'), h('div', { class: 'cols' },
    h('section', { class: 'main', style: 'flex:1 1 360px' }, h('h2', {}, '直す項目'),
      ng.length ? ng.map((/** @type {any} */ f) => { const box = h('input', { type: 'checkbox', checked: true }); box.addEventListener('change', () => { /** @type {HTMLInputElement} */ (box).checked ? chosen.add(f.id) : chosen.delete(f.id); build(); });
        return h('label', { class: 'choice' }, box, h('span', {}, h('strong', {}, f.requirement), h('span', { class: 'note', style: 'display:block' }, happened(f)))); }) : h('p', { class: 'note' }, '期待と異なる結果はありません。')),
      h('section', { class: 'side', style: 'flex:2 1 640px' }, h('h2', {}, 'あなたのAIに渡すプロンプト'),
      h('div', { class: 'actions', style: 'margin-top:0' },
        btn('コピー', () => guarded(async () => { await navigator.clipboard.writeText(pre.textContent || ''); setStatus('プロンプトをコピーしました。'); }), 'primary'),
        btn('.md で保存', () => { const a = h('a', { href: URL.createObjectURL(new Blob([pre.textContent || ''], { type: 'text/markdown' })), download: `fix-${short(state.source.commit)}.md` }); a.click(); })),
      pre, h('div', { class: 'actions' }, btn('修正したら再試験へ', () => go(6))))));
}

// ---------- 6 再試験 ----------
async function step6() {
  const records = Object.values(state.flow?.records || {}).filter((/** @type {any} */ x) => x.repository === state.source.repository && x.record?.results);
  const others = records.filter((/** @type {any} */ x) => x.commit !== state.source.commit);
  // ブランチで開いた場合は、同じブランチの最新を読み込み直す。コミットを直接指定して開いた場合だけ、読み込むブランチを書いてもらう。
  // コミットのSHA（短縮を含む）で開いた場合は、読み直しても同じ版になるので、ブランチを書いてもらう。
  // タグで開いた場合も同じ版になるが、ブランチと見分けられないので、読み直すボタンに加えて、別のブランチを書く欄を折りたたみで出す。
  const requested = state.source.reference || '';
  const reference = requested && requested !== state.source.commit && !/^[0-9a-f]{7,40}$/i.test(requested) ? requested : '';
  const ref = h('input', { type: 'text', id: 'ref', placeholder: '例：main', style: 'width:280px' });
  const refForm = () => [h('label', { for: 'ref', style: 'font-weight:700' }, '読み込むブランチ'), ' ', ref, ' ',
    btn('読み込む', () => { const v = /** @type {HTMLInputElement} */ (ref).value.trim(); if (!v) return showError('ブランチ名を入力してください。'); reload(v); }, 'primary')];
  const reload = (/** @type {string} */ v) => { const u = new URL(location.href); u.search = new URLSearchParams({ url: `https://github.com/${state.source.repository}`, ref: v }).toString(); location.href = u.toString(); };
  const current = state.record?.results || {};
  const prev = byLatestSave(others)[0];
  // 別の版の試験は、結果がない場合（—）と区別して示す。このコミットを試験していない理由が、比較表でも分かるようにする。
  const label = { ok: '期待どおり', ng: '期待と異なる', pending: '未実行', unconfirmed: '未確認', other_version: '別の版の試験' };
  const rows = prev ? [...new Set([...Object.values(prev.record.results), ...Object.values(current)].map((/** @type {any} */ x) => x.requirement))].map(req => {
    const b = Object.values(prev.record.results).find((/** @type {any} */ x) => x.requirement === req);
    const a = Object.values(current).find((/** @type {any} */ x) => x.requirement === req);
    const change = b?.state === 'ng' && a?.state === 'ok' ? '解消' : b?.state === 'ok' && a?.state === 'ng' ? '新たに異なる' : a?.state === 'ng' ? '変わらず異なる' : '—';
    // 懸念の項目は、引き継いだ懸念なら文が同じなので、前後で同じ行になる。
    const concern = (a || b)?.kind === 'concern';
    return h('tr', { 'data-compare': change, 'data-kind': concern ? 'concern' : 'guideline' }, h('td', {}, req, concern ? h('div', { class: 'note' }, 'コードから出た懸念') : null), h('td', {}, label[b?.state] || '—'), h('td', {}, label[a?.state] || '—'), h('td', { style: change === '解消' ? 'color:var(--success);font-weight:700' : change.includes('不足') ? 'color:var(--error);font-weight:700' : '' }, change));
  }) : [];
  fill($('main'), h('div', { class: 'pad' },
    h('h2', {}, '修正後の再試験'),
    reference
      ? [h('div', { class: 'actions' }, btn(`${reference} の最新を読み込み直す`, () => reload(reference), 'primary')),
        h('details', { 'data-other-ref': '' }, h('summary', {}, '別のブランチを読み込む'), refForm())]
      : refForm(),
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
  state.record = migrate(state.flow.records?.[key()]?.record || null);
  await loadCase();
  setStatus('');
  state.step = startStep(state.record);
  render();
}
function renderChromeEmpty() { fill($('steps')); fill($('target')); }
guarded(init);
