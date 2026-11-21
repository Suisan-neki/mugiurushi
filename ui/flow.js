// @ts-check
// ガイドラインの照合から試験・修正までの流れ。1画面に1つの操作だけを置き、待つ処理には進み具合の画面を出す。
// 判定と観測の状態は事例の記録（/api/v1/case）が正。流れの記録（/api/v1/flow）は画面を続けるための控え。
// AIへ送る内容は、送る前にサーバーが組み立てた内容をそのまま画面に示し、人が確かめたときだけ送る（AGENTS.md）。
// 試験の設定（起動のしかた・判定の条件）をAIが案に含めた場合は、試験の案の画面で中身を示し、了承した試験だけを
// 既存の試験の設定画面と同じAPIの順（確認項目の登録 → 判定条件の対応づけ → 固定コミットの取得 → 実行）で実行する。
import { apiRequest as api } from './api.js';
import { applyScan, applyPlan, applyInvestigation, runConcerns, planEvidence, carryableConcerns, carriedInvestigation, carryablePlan, carriedPlan, byLatestSave, concernSource, concernRequirement, concernEvidenceText, concernPlanCandidate, setAnswer, answerTo, answersOf, scanGeneration, planGeneration, planIsCurrent, approve, approvedCheck, approvedTests, startStep, migrate, untestedFindings, UNTESTED } from './flow-record.js';

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
const sleep = (/** @type {number} */ ms) => new Promise(r => setTimeout(r, ms));
const count = (/** @type {number|null|undefined} */ n) => typeof n === 'number' ? n.toLocaleString('ja-JP') : '不明';

const SECTION_OF = { 'provider-security': 'g3', 'mhlw-security': 'g3', 'mhlw-cyber-checklist': 'g3', 'ppc-medical-guidance': 'pi', 'ppc-general': 'pi', 'ipa-websecurity': 'web', 'owasp-top10': 'web', 'owasp-asvs': 'web', 'nist-800-53': 'nist', 'nist-csf': 'nist', 'mhlw-samd': 'dev', 'mhlw-device-cyber': 'dev' };
const SECTIONS = [['g3', '3省2ガイドライン'], ['pi', '個人情報保護法のガイドライン'], ['web', 'Webアプリケーションの技術基準'], ['dev', '医療機器'], ['nist', 'NISTの基準（参考）'], ['concern', 'コードから出た懸念'], ['other', 'その他']];
const PRIO = { high: '高', medium: '中', low: '低' };
const PRIO_ORDER = { high: 0, medium: 1, low: 2 };
const STEPS = ['照合の準備', '照合とヒアリング', '検証案の了承', '結果', '修正用のプロンプト'];
/** 画面ごとの手順の番号。 */
const STEP_OF = { repo: 1, loading: 1, files: 1, describe: 1, confirmScan: 1, scan: 2, hearing: 2, plan: 3, result: 4, prompt: 5, retest: 5 };
const METHOD_LABEL = { exit_code: '終了コード', stdout: '標準出力', stderr: '標準エラー', stored_file: '保存先のファイル', readback_output: '読み出しの出力' };
/** 試験の案の文の中の、試験の値の記号（{{test_value}}）を言葉にする。実行のしかたのコマンドは、記号のまま示す。 */
const worded = (/** @type {string} */ text) => (text || '').replaceAll('「{{test_value}}」', '試験の値').replaceAll('{{test_value}}', '試験の値');
const quoted = (/** @type {string} */ text) => text === '{{test_value}}' ? '試験の値' : `「${worded(text)}」`;
const EXPECT_LABEL = (/** @type {any} */ c) => ({ zero: '0で終わる', nonzero: '0以外で終わる', contains: `${quoted(c.text)}を含む`, not_contains: `${quoted(c.text)}を含まない` })[/** @type {'zero'} */ (c.expect)] || c.expect;

/** @type {any} */
const state = { screen: 'repo', source: null, branch: '', url: '', view: null, flow: null, record: null, agent: null, runnable: null };

// 記録者はこの画面では聞かない。名前が保存されていなければ、理解と試験の画面（ui/app.js）と同じ既定の名前を記録に使う。
function person() { try { return JSON.parse(sessionStorage.getItem('eel-person') || 'null')?.name || '開発者（本人確認なし）'; } catch { return '開発者（本人確認なし）'; } }
/** 読み込んだブランチ。ブランチを選ばずにコミットで開いた場合はコミットを示す。 */
function branchLabel() { return state.branch || `コミット ${short(state.source?.commit || '')}`; }
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

// ---------- 共通の枠 ----------
function renderChrome() {
  const s = state.source;
  fill($('target'), s ? [h('span', { class: 'code' }, s.repository), h('span', { class: 'sep', 'aria-hidden': 'true' }, '/'), h('span', { class: 'code', 'data-branch': '' }, branchLabel()),
    state.branch ? h('span', { class: 'note', 'data-loaded-commit': '' }, `コミット ${short(s.commit)}`) : null] : null);
  $('person').textContent = `記録者：${person()}`;
  const cur = STEP_OF[/** @type {'repo'} */ (state.screen)] || 1;
  const r = state.record || {};
  const approvedAny = approvedTests(r).length > 0;
  // 済んだ段だけ戻れる。戻り先は、その段の結果を示す画面。
  const target = { 1: s ? 'files' : 'repo', 2: s ? 'scan' : null, 3: planIsCurrent(r) ? 'plan' : null, 4: approvedAny ? 'result' : null, 5: approvedAny ? 'prompt' : null };
  fill($('steps'), h('ol', { class: 'stepper' }, STEPS.map((t, i) => {
    const n = i + 1, done = n < cur, on = n === cur;
    const to = /** @type {any} */ (target)[n];
    const mark = h('span', { class: 'mark', 'aria-hidden': 'true' }, done ? '✓' : String(n));
    return h('li', { class: on ? 'on' : done ? 'done' : '', 'aria-current': on ? 'step' : null },
      to && !on && s ? h('button', { type: 'button', onclick: () => go(to) }, mark, t) : h('span', { class: 'label' }, mark, t));
  })));
}
function go(/** @type {string} */ screen) { state.screen = screen; render(); window.scrollTo(0, 0); }
function render() {
  renderChrome();
  const f = /** @type {any} */ ({ repo: repoScreen, files: filesScreen, describe: describeScreen, confirmScan: confirmScanScreen, scan: scanScreen, hearing: hearingScreen, plan: planScreen, result: resultScreen, prompt: promptScreen, retest: retestScreen })[state.screen];
  if (f) guarded(async () => { await f(); });
}

/** 1画面1操作の枠。sub は段と位置、primary は主な操作。 */
function screen(/** @type {{ sub: string, title: any, lead?: any, body?: any, primary?: HTMLElement | null, back?: (() => any) | null, extra?: any, name?: string }} */ o) {
  fill($('main'), h('div', { class: 'screen', 'data-screen': o.name || state.screen },
    h('p', { class: 'sub' }, o.sub),
    h('h1', {}, o.title),
    o.lead ? h('p', { class: 'lead' }, o.lead) : null,
    h('div', { class: 'content' }, o.body),
    o.primary || o.back || o.extra ? h('div', { class: 'screen-actions' },
      o.back ? btn('戻る', o.back, 'tertiary') : h('span', {}),
      h('div', { class: 'right' }, o.extra, o.primary)) : null));
}
const primary = (/** @type {string} */ text, /** @type {() => any} */ onclick) => btn(text, onclick, 'primary lg');
const card = (/** @type {Record<string, any>} */ attrs, /** @type {any[]} */ ...c) => h('section', { ...attrs, class: `card ${attrs.class || ''}` }, ...c);
const badge = (/** @type {string} */ text, /** @type {string} */ tone) => h('span', { class: `badge ${tone}` }, text);
function prio(/** @type {string} */ p) { return p ? badge(`優先度 ${PRIO[/** @type {'high'} */ (p)] || p}`, { high: 'error', medium: 'warning', low: 'neutral' }[/** @type {'high'} */ (p)] || 'neutral') : null; }

/** 待つ処理の画面。項目を上から一つずつ進め、終わったら「完了しました」を示す。項目は実際に行う処理の段階だけにする。 */
function waiting(/** @type {{ sub: string, title: string, lead: string, items: string[], note?: string }} */ o) {
  const spin = () => h('span', { class: 'spin', 'aria-hidden': 'true' });
  const ok = () => h('span', { class: 'ok', 'aria-hidden': 'true' }, '✓');
  const rows = o.items.map((t, i) => h('li', { 'data-progress': i ? 'wait' : 'now' }, i ? h('span', { class: 'idle', 'aria-hidden': 'true' }) : spin(), h('span', { class: 'what' }, t), h('span', { class: 'state' }, i ? '待機中' : '実行中')));
  const head = h('div', { class: 'wait-head', role: 'status' }, h('span', { class: 'spin big', 'aria-hidden': 'true' }), h('div', {}, h('h1', {}, o.title), h('p', { class: 'lead' }, o.lead)));
  const note = o.note ? h('p', { class: 'note' }, o.note) : null;
  fill($('main'), h('div', { class: 'screen waiting', 'data-screen': 'waiting' }, h('p', { class: 'sub' }, o.sub), head, h('ol', { class: 'progress card', 'aria-label': '進み具合' }, rows), note));
  let at = 0;
  const set = (/** @type {number} */ i, /** @type {'done'|'now'|'failed'} */ st, /** @type {string} */ label = '') => {
    const li = rows[i]; if (!li) return;
    li.dataset.progress = st;
    /** @type {HTMLElement} */ (li.firstElementChild).replaceWith(st === 'done' ? ok() : st === 'now' ? spin() : h('span', { class: 'ng', 'aria-hidden': 'true' }, '!'));
    /** @type {HTMLElement} */ (li.lastElementChild).textContent = label || { done: '完了', now: '実行中', failed: 'できなかった' }[st];
  };
  return {
    /** 今の項目を終え、次の項目を始める。 */
    next(/** @type {string} */ label = '') { set(at, 'done', label); at++; if (at < rows.length) set(at, 'now'); },
    fail(/** @type {string} */ label) { set(at, 'failed', label); at++; if (at < rows.length) set(at, 'now'); },
    /** すべて終えたことを示し、少し見せてから次へ進む。 */
    async done(/** @type {string} */ lead) {
      while (at < rows.length) this.next();
      /** @type {HTMLElement} */ (head.firstElementChild).replaceWith(h('span', { class: 'ok big', 'aria-hidden': 'true' }, '✓'));
      /** @type {HTMLElement} */ (head.querySelector('h1')).textContent = '完了しました';
      /** @type {HTMLElement} */ (head.querySelector('.lead')).textContent = lead;
      note?.remove();
      await sleep(1200);
    },
  };
}

// ---------- 1 照合の準備 ----------
// リポジトリのURLを入れると、ブランチの一覧を読む。ブランチは書かせずに選ばせる。
async function repoScreen() {
  const url = h('input', { type: 'url', id: 'repo', class: 'field', placeholder: 'https://github.com/所有者/名前', autocomplete: 'off', value: state.url || '' });
  const select = h('select', { id: 'branch', class: 'field', disabled: true }, h('option', {}, 'URLを入れると選べます'));
  const msg = h('p', { class: 'note', 'data-branch-note': '' });
  /** @type {any} */ let list = null; let asked = '';
  const load = primary('読み込む', () => guarded(async () => {
    const b = list?.branches.find((/** @type {any} */ x) => x.name === /** @type {HTMLSelectElement} */ (select).value);
    if (!b) throw Error('ブランチを選んでください。');
    await openRepository(list.url, b.name, b.commit);
  }));
  load.setAttribute('disabled', '');
  const lookup = async () => {
    const u = /** @type {HTMLInputElement} */ (url).value.trim();
    if (!u || u === asked) return;
    asked = u; list = null; load.setAttribute('disabled', ''); select.setAttribute('disabled', '');
    fill(select, h('option', {}, '読み込んでいます…')); msg.textContent = '';
    try {
      const r = await api('POST', '/api/v1/branches', { url: u });
      if (asked !== u) return;
      list = { ...r, url: u };
      const sorted = [...r.branches].sort((a, b) => (b.name === r.default_branch ? 1 : 0) - (a.name === r.default_branch ? 1 : 0));
      fill(select, sorted.map(b => h('option', { value: b.name }, b.name === r.default_branch ? `${b.name}（既定のブランチ）` : b.name)));
      select.removeAttribute('disabled'); load.removeAttribute('disabled');
      msg.textContent = r.source === 'local' ? '試験用に取得したリポジトリから一覧を読みました（GitHubからは読めませんでした）。' : '';
    } catch (e) {
      if (asked !== u) return;
      fill(select, h('option', {}, '選べるブランチがありません'));
      msg.textContent = e instanceof Error ? e.message : String(e);
    }
  };
  let timer = 0;
  url.addEventListener('input', () => { clearTimeout(timer); timer = window.setTimeout(lookup, 500); });
  url.addEventListener('change', lookup);
  screen({
    sub: '1 照合の準備　1/4', title: '照らし合わせるリポジトリを登録する', lead: 'GitHubのリポジトリと、確かめたいブランチを選びます。',
    body: [
      h('label', { for: 'repo', class: 'field-label' }, 'GitHubのリポジトリのURL ', h('span', { class: 'req' }, '必須')),
      h('p', { class: 'hint' }, '例：https://github.com/所有者/名前'), url,
      h('label', { for: 'branch', class: 'field-label' }, 'ブランチ ', h('span', { class: 'req' }, '必須')),
      h('p', { class: 'hint' }, 'リポジトリにあるブランチから選びます。選んだブランチの最新のコミットを読み込みます'),
      h('div', { class: 'select-wrap' }, select), msg,
      h('p', { class: 'note' }, 'いまは公開されているリポジトリだけを読み込めます')],
    primary: load,
  });
  if (state.url) lookup();
}

/** リポジトリを読み込み、動かせるかを判定してから、続きの画面へ進む。commit はブランチの一覧で見たコミット。 */
async function openRepository(/** @type {string} */ url, /** @type {string} */ branch, /** @type {string} */ commit, carry = false) {
  // 別の対象を読み込むので、前の対象で外したファイルの選択は使わない（読み込んだ対象の記録か既定の規則から作り直す）。
  skipped = null;
  state.url = url; state.branch = branch;
  const name = url.replace(/^https:\/\/github\.com\//, '').replace(/\/$/, '');
  const w = waiting({ sub: '1 照合の準備', title: 'リポジトリを読み込んでいます', lead: `${name} の ${branch} ブランチを読み込んでいます。`,
    items: ['ブランチの最新のコミットを確かめる', 'ファイルの一覧と中身を読み込む', '依存関係から、試験環境で動かせるか確かめる'], note: 'この画面のまま、少しお待ちください。' });
  try {
    // ブランチの一覧で見たコミットに固定して読む。一覧を読んだ後にブランチが進んでいても、選んだ時点のコミットを読む。
    w.next(short(commit));
    const res = await api('POST', '/api/v1/understand', { operation: 'open', url, reference: commit });
    state.source = res.source;
    w.next(`${(res.source.inventory || []).length}件`);
    state.runnable = await api('POST', '/api/v1/runnable', { repository: state.source.repository, commit: state.source.commit }).catch((/** @type {any} */ e) => ({ runnable: false, blockers: [e instanceof Error ? e.message : String(e)], entrypoints: [], notes: [] }));
    w.next(state.runnable.runnable ? '動かせる' : '動かせない');
    await loadFlow();
    // 再試験：前のコミットで了承した試験の案を引き継ぐ。このコミットに照合や案の記録があれば、それを残して引き継がない。
    /** @type {string} */ let notCarried = '';
    if (carry) {
      const c = carryablePlan(state.flow?.records, state.source.repository, state.source.commit);
      if (!c) notCarried = '引き継げる了承済みの試験がないため、照合から進めます。';
      else if (state.record?.scan || state.record?.plan) notCarried = 'このコミットには照合か試験の案の記録があるため、前回の試験は引き継ぎませんでした。';
      else { state.record = carriedPlan(c.record, c.from, (state.source.inventory || []).map((/** @type {any} */ f) => f.path), state.runnable?.entrypoints || []); await saveRecord(); }
    }
    const u = new URL(location.href); u.search = new URLSearchParams({ url, ref: state.source.commit, branch }).toString(); history.replaceState(null, '', u);
    await w.done(`${name} の ${branch} ブランチを読み込みました。${state.runnable.runnable ? '試験環境で動かせます。' : 'コードから確かめます。'}`);
    go(resumeScreen());
    if (notCarried) showError(notCarried);
  } catch (e) { go('repo'); showError(e); }
}
async function loadFlow() {
  state.agent = await api('GET', '/api/v1/agent').catch(() => null);
  if (state.agent?.status !== 'configured') showError('AIエージェントが設定されていません。運営者が EEL_AGENT_URL と EEL_AGENT_TOKEN_FILE を設定して起動してください。');
  state.flow = await api('GET', '/api/v1/flow');
  state.record = migrate(state.flow.records?.[key()]?.record || null);
  await loadCase();
}
function resumeScreen() { return ['files', 'files', 'scan', 'plan', 'result'][startStep(state.record)] || 'files'; }

// 送らないファイル。秘密の値を持つことが多い種類で、選べないようにする。
// サーバーも照合の送信で同じ種類を拒否する（src/code_tree.rs の DEFAULT_EXCLUSIONS）。
const SECRET = /(^|\/)(\.env(\.[^/]*)?|[^/]*\.(pem|key|p12|pfx|jks|keystore|kdbx)|id_(rsa|dsa|ecdsa|ed25519)(\.[^/]*)?|\.npmrc|\.pypirc|\.netrc|\.git-credentials)$/;
// 最初から外しておくファイル。読んでも照合に役立たない依存・生成物・バイナリ・ロックファイルだけを外す。
const SKIP = /(^|\/)(node_modules|vendor|dist|build|\.git)\/|\.(png|jpe?g|gif|webp|ico|bmp|pdf|zip|gz|tgz|jar|war|woff2?|ttf|otf|eot|mp3|mp4|mov|wasm|exe|dll|so|dylib)$|(^|\/)(package-lock\.json|yarn\.lock|pnpm-lock\.yaml|Cargo\.lock|poetry\.lock|uv\.lock|Gemfile\.lock|composer\.lock)$/i;
// 一度の照合で送れる量（src/agent.rs の MAX_FILES・MAX_FILE_CHARS・MAX_TOTAL_CHARS と同じ値）。
const SEND = { files: 40, fileChars: 20_000, totalChars: 400_000 };
const weightOf = (/** @type {any} */ f) => Math.min(f?.size ?? SEND.fileChars, SEND.fileChars);
/** 送れる量を超えたファイルを、ディレクトリのまとまりを崩さないように、1回で送れる量ずつの回に分ける。 */
function splitRounds(/** @type {string[]} */ files, /** @type {Map<string, any>} */ byPath) {
  /** @type {{ label: string, files: string[] }[]} */
  const groups = [];
  const fits = (/** @type {string[]} */ fs) => fs.length <= SEND.files && fs.reduce((n, f) => n + weightOf(byPath.get(f)), 0) <= SEND.totalChars;
  const walk = (/** @type {string[]} */ fs, /** @type {number} */ depth, /** @type {string} */ prefix) => {
    if (fits(fs)) return groups.push({ label: prefix || '（最上位）', files: fs });
    const sub = new Map(), here = [];
    for (const f of fs) { const parts = f.split('/'); if (parts.length > depth + 1) { const k = parts.slice(0, depth + 1).join('/') + '/'; sub.set(k, [...(sub.get(k) || []), f]); } else here.push(f); }
    if (sub.size === 1 && !here.length) return walk(fs, depth + 1, [...sub.keys()][0]);
    for (const [k, v] of [...sub].sort()) walk(v, depth + 1, k);
    for (let i = 0, chunk = []; i <= here.length; i++) {
      const f = here[i];
      if (f === undefined || !fits([...chunk, f])) { if (chunk.length) groups.push({ label: `${prefix || '（最上位）'}の直下`, files: chunk }); chunk = f === undefined ? [] : [f]; } else chunk.push(f);
    }
  };
  walk([...files].sort(), 0, '');
  /** @type {{ labels: string[], files: string[] }[]} */
  const rounds = [];
  for (const g of groups) {
    const last = rounds[rounds.length - 1];
    if (last && fits([...last.files, ...g.files])) { last.labels.push(g.label); last.files.push(...g.files); }
    else rounds.push({ labels: [g.label], files: [...g.files] });
  }
  return rounds;
}

// 照らし合わせないファイルを外す。最初はすべてを照らし合わせる前提で、外したいファイルにだけ印を付ける。
/** @type {Set<string> | null} */
let skipped = null;
async function filesScreen() {
  const inv = (state.source.inventory || []).filter((/** @type {any} */ f) => f.path);
  const all = inv.map((/** @type {any} */ f) => f.path);
  if (!skipped) skipped = new Set(state.record?.files ? all.filter((/** @type {string} */ p) => !state.record.files.includes(p)) : all.filter((/** @type {string} */ p) => SKIP.test(p) || SECRET.test(p)));
  const chosen = () => all.filter((/** @type {string} */ p) => !skipped?.has(p) && !SECRET.test(p));
  const LIMIT = 400;
  const summary = h('span', { 'data-files-count': '' });
  const tbody = h('div', { class: 'rows' });
  const filter = h('input', { type: 'search', id: 'file-filter', class: 'field sm', placeholder: 'パスの一部で絞り込む（例：app/）' });
  const out = h('div', {});
  const update = () => { summary.textContent = `${all.length}件中${all.length - chosen().length}件を外します`; };
  const rowOf = (/** @type {any} */ f) => {
    const secret = SECRET.test(f.path), off = secret || !!skipped?.has(f.path);
    const box = h('input', { type: 'checkbox', checked: off, disabled: secret, 'aria-label': `${f.path}を外す` });
    const row = h('label', { class: `file-row${off ? ' off' : ''}`, 'data-file': f.path, 'data-secret': secret ? '' : null }, box, h('span', { class: 'code' }, f.path),
      secret ? h('span', { class: 'note' }, '秘密の値を持つことが多い種類のため送りません') : null);
    box.addEventListener('change', () => { /** @type {HTMLInputElement} */ (box).checked ? skipped?.add(f.path) : skipped?.delete(f.path); row.classList.toggle('off', /** @type {HTMLInputElement} */ (box).checked); update(); });
    return row;
  };
  const list = () => {
    const q = /** @type {HTMLInputElement} */ (filter).value.trim().toLowerCase();
    const rows = inv.filter((/** @type {any} */ f) => !q || f.path.toLowerCase().includes(q));
    fill(tbody, rows.slice(0, LIMIT).map(rowOf), rows.length > LIMIT ? h('p', { class: 'note more' }, `ほかに${rows.length - LIMIT}件あります。絞り込んでください。`) : null);
  };
  filter.addEventListener('input', list);
  list(); update();
  screen({
    sub: '1 照合の準備　2/4', title: '照らし合わせないファイルを外す',
    lead: 'このブランチのファイルを、すべてガイドラインと照らし合わせます。照らし合わせなくてよいファイルにだけ、チェックを付けてください。',
    body: [card({ class: 'flush', 'data-files': '' }, h('div', { class: 'card-head' }, h('span', {}, `${state.source.repository} の `, h('span', { class: 'code' }, branchLabel()), state.branch ? ' ブランチ' : ''), summary),
      inv.length > 30 ? h('div', { class: 'card-tools' }, filter) : null, tbody), out],
    back: () => go('repo'),
    primary: primary('次へ', () => {
      const files = chosen();
      if (!files.length) return showError('照らし合わせるファイルを1件以上残してください。');
      // 一度に送れる量を超える場合は送らず、ディレクトリごとに分けて照合する案を示す。
      const byPath = new Map(inv.map((/** @type {any} */ f) => [f.path, f]));
      if (files.length > SEND.files || files.reduce((n, f) => n + weightOf(byPath.get(f)), 0) > SEND.totalChars) {
        const rounds = splitRounds(files, byPath);
        fill(out, card({ 'data-split-proposal': '' },
          h('h2', {}, `${files.length}件を、${rounds.length}回に分けて照合しましょう`),
          h('p', {}, '一度に照合できる量を超えています。ディレクトリごとに分けました。1回分を選んで照合してください。'),
          h('ol', { class: 'rounds' }, rounds.map((r, i) => h('li', { 'data-round': i + 1 },
            h('div', {}, h('strong', {}, `${i + 1}回目`), ` ${r.files.length}件`),
            h('div', { class: 'note' }, r.labels.join('、')),
            btn(`${i + 1}回目のファイルだけを残す`, () => { skipped = new Set(all.filter((/** @type {string} */ p) => !r.files.includes(p))); fill(out); list(); update(); })))),
          h('p', { class: 'note' }, '分けた回の結果をまとめて比べる機能はまだありません（#243）。')));
        out.scrollIntoView({ block: 'start', behavior: 'smooth' });
        return;
      }
      state.record = { ...(state.record || {}), files };
      go('describe');
    }),
  });
}

async function describeScreen() {
  const desc = h('textarea', { rows: 4, id: 'desc', class: 'field' }); /** @type {HTMLTextAreaElement} */ (desc).value = state.record?.summary || '';
  screen({
    sub: '1 照合の準備　3/4', title: 'どんなシステムかを書く', lead: '使う人や扱う情報を書くと、照合が具体的になります。書かずに進むこともできます。',
    body: [h('label', { for: 'desc', class: 'field-label' }, 'システムの説明 ', h('span', { class: 'req' }, '任意')), h('p', { class: 'hint' }, '例：患者がスマートフォンで答え、医師が診察室で読む'), desc],
    back: () => go('files'),
    primary: primary('次へ', () => { state.record = { ...(state.record || {}), summary: /** @type {HTMLTextAreaElement} */ (desc).value.trim() }; go('confirmScan'); }),
  });
}

/** 確かめ方（動かせるかの判定）の説明。 */
function howToCheck() {
  const r = state.runnable;
  if (!r) return h('span', {}, 'コードから確かめます');
  return r.runnable
    ? h('div', { 'data-runnable': 'yes' }, '試験環境で動かして確かめます', h('div', { class: 'note' }, `依存関係が少なく、外部サービスなしで起動できる見込みのため（起動の候補：${r.entrypoints.join('、')}）`))
    : h('div', { 'data-runnable': 'no' }, 'コードから確かめます（自動では動かしません）', h('ul', { class: 'note items' }, (r.blockers || []).map((/** @type {string} */ b) => h('li', {}, b))));
}

/** 送る前の確認の画面。サーバーが組み立てた内容をそのまま示し、確かめたときだけ送る。
 * SHA-256は人が読み比べる値ではないので「技術的な詳細」に置き、送るときは画面が同じ値を添える（AGENTS.md）。 */
/** 送る前の確認の中身：送信先（AIエージェントと、その先のモデル）、送るもの、大きさ、送る内容の全文、SHA-256、確かめた印。
 * 印を付け、モデルを確かめられたときだけ、送るボタン（send）を押せるようにする。送信先を示してから送る規則のため（AGENTS.md）。 */
function confirmPanel(/** @type {any} */ preview, /** @type {any[]} */ extraRows, /** @type {HTMLElement} */ send) {
  const model = state.agent?.model, endpoint = state.agent?.endpoint;
  const agree = h('input', { type: 'checkbox', id: 'agree' });
  send.setAttribute('disabled', '');
  agree.addEventListener('change', () => /** @type {HTMLInputElement} */ (agree).checked && model ? send.removeAttribute('disabled') : send.setAttribute('disabled', ''));
  const rows = [
    ['送信先', [endpoint ? ['照合用のAIエージェント ', h('span', { class: 'code' }, endpoint)] : '照合用のAIエージェント', model
      ? h('div', { 'data-agent-model': '' }, 'その先のモデル ', h('span', { class: 'code' }, model))
      : h('div', { class: 'alert', 'data-agent-model-missing': '' }, `その先のモデルを確かめられないため、送れません。${state.agent?.model_error || ''}`)]],
    ...extraRows,
    ['大きさ', `${count(preview.bytes)}バイト`],
  ].filter(Boolean);
  return { agree, nodes: [card({ 'data-preview': '' }, h('dl', { class: 'facts' }, rows.map((/** @type {any} */ [k, v]) => [h('dt', {}, k), h('dd', {}, v)])),
    h('details', {}, h('summary', {}, '送る内容をすべて見る'), h('pre', { class: 'payload' }, JSON.stringify(preview.payload, null, 2))),
    h('details', { class: 'tech' }, h('summary', {}, '技術的な詳細'),
      h('p', { class: 'note' }, '送信記録と照らし合わせるための値（SHA-256）'), h('p', { class: 'code', 'data-payload-sha256': '' }, preview.payload_sha256))),
  h('label', { class: 'agree' }, agree, h('span', {}, '公開してよいコードと架空のデータだけで、実在の患者の情報・認証情報・生のログを含まないことを確かめました'))] };
}
/** 送る内容を組み立て、送信先のモデルを問い合わせる。モデルは送る直前の状態を問い合わせる（運営者がAIエージェントの設定を変えている場合があるため）。 */
async function loadPreview(/** @type {string} */ path, /** @type {any} */ body) {
  setStatus('送る内容を組み立てています。');
  try {
    const preview = await api('POST', path, body);
    state.agent = await api('GET', '/api/v1/agent').catch(() => state.agent);
    return preview;
  } finally { setStatus(''); }
}
/** 人が確かめた内容（SHA-256）を添えて送り、待つ画面を出して、終わったら次の画面へ進む。失敗したら back で元の画面に戻る。 */
async function sendConfirmed(/** @type {{ path: string, body: any, back: () => any, wait: { sub: string, title: string, lead: string, items: string[], note: string }, done: (r: any) => Promise<{ lead: string, next: string }> }} */ o, /** @type {any} */ preview, /** @type {HTMLElement} */ agree) {
  const synthetic = /** @type {HTMLInputElement} */ (agree).checked;
  const w = waiting(o.wait);
  try {
    w.next();
    const r = await api('POST', o.path, { ...o.body, confirm: { payload_sha256: preview.payload_sha256, synthetic_only: synthetic } });
    w.next();
    const after = await o.done(r);
    await w.done(after.lead);
    go(after.next);
  } catch (e) { o.back(); throw e; }
}
async function confirmScreen(/** @type {{ sub: string, title: string, lead: string, path: string, body: any, rows: any[], send: string, back: () => any, wait: { sub: string, title: string, lead: string, items: string[], note: string }, done: (r: any) => Promise<{ lead: string, next: string }> }} */ o) {
  const preview = await loadPreview(o.path, o.body);
  /** @type {any} */ let panel = null;
  const send = primary(o.send, () => guarded(() => sendConfirmed(o, preview, panel.agree)));
  panel = confirmPanel(preview, o.rows, send);
  screen({ sub: o.sub, title: o.title, lead: o.lead, name: 'confirm', body: panel.nodes, back: o.back, primary: send });
}
const fileList = (/** @type {string[]} */ files) => h('details', { class: 'inline' }, h('summary', {}, `${files.length}件（`, h('span', { class: 'code' }, files[0] || ''), files.length > 1 ? ' ほか）' : '）'),
  h('p', { class: 'code small' }, files.join('、')));

async function confirmScanScreen() {
  const r = state.record;
  const answers = answersOf(r), at = scanGeneration(r);
  await confirmScreen({
    sub: '1 照合の準備　4/4', title: 'AIエージェントに送る内容を確かめる', lead: '送るのは、選んだファイルと説明だけです。送る前に中身を確かめてください。',
    path: '/api/v1/agent/flow-scan', body: { repository: state.source.repository, commit: state.source.commit, files: r.files, summary: r.summary || '', answers, ...screenOf(r) },
    rows: [['ファイル', fileList(r.files)], ['確かめ方', howToCheck()], ['説明', r.summary || '（なし）'], answers.length ? ['回答', answers.map(a => `${a.question}：${a.answer}`).join('／')] : null],
    send: '照合を始める', back: () => go('describe'),
    wait: { sub: '2 照合とヒアリング', title: 'ガイドラインと照らし合わせています', lead: 'AIエージェントが、コードと医療情報システムの運用ガイドラインを照らし合わせています。',
      items: ['送る内容をAIエージェントに送る', 'ガイドラインとの照らし合わせを待つ', '結果を記録する'], note: '応答までに1分ほどかかることがあります。' },
    done: async s => { applyScan(r, s, answers, at); await saveRecord(); return { lead: `確認候補が${(s.findings || []).length}件見つかりました。`, next: 'scan' }; },
  });
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
  const operation = h('textarea', { rows: 2, id: 'operation', class: 'field', placeholder: '例：患者の新規登録（国民識別番号・氏名・性別・年齢を入力して保存する）' });
  /** @type {HTMLTextAreaElement} */ (operation).value = saved?.operation || '';
  const rounds = h('input', { type: 'number', id: 'rounds', class: 'field sm', min: '1', max: '30', value: String(saved?.rounds_limit || 12) });
  const exclusions = h('textarea', { rows: 2, id: 'exclusions', class: 'field', placeholder: '例：deploy/secrets/**（1行に1つ。既定の指定に足します）' });
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
        if (!state.record) state.record = {};
        applyInvestigation(state.record, record);
        await saveRecord();
        setStatus('');
        if (state.screen === 'scan') render(); else fill(out, investigationResult(record));
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
        h('label', { class: 'agree' }, agree, 'このリポジトリのこのコミットのコードを、送らないパスを除いて、AIが求めた分だけ送ることを了承する。送ってよいコードであることを確かめた'),
        h('div', { class: 'actions' }, start))));
  }));
  return h('div', { 'data-investigation-form': '' },
    h('p', {}, '確かめたい操作を書くと、AIがコードをたどって懸念を挙げます。'),
    h('label', { for: 'operation', class: 'field-label' }, '対象の操作'), operation,
    h('div', { class: 'row-fields' },
      h('div', {}, h('label', { for: 'rounds', class: 'field-label' }, '往復の上限（回）'), rounds),
      h('div', { class: 'grow' }, h('label', { for: 'exclusions', class: 'field-label' }, '送らないパスの追加（任意）'), exclusions)),
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
    if (state.screen === 'scan') render(); else fill(out, investigationResult(record));
  }));
}
function investigationResult(/** @type {any} */ saved) {
  const r = saved.result;
  const sent = (r.material || []).filter((/** @type {any} */ m) => m.kind !== 'refused').length;
  return h('div', { class: 'box', 'data-investigation': r.stopped_by },
    h('div', { class: 'head' }, h('strong', {}, `懸念 ${r.concerns.length}件`), h('span', { class: 'note' }, `${/** @type {any} */ (STOPPED)[r.stopped_by] || r.stopped_by}。往復${r.rounds}回、送った資料${sent}件。記録 ${r.record}`)),
    h('div', { class: 'pad' },
      r.note ? h('p', { class: 'note' }, r.note) : null,
      r.limits ? h('p', { class: 'note' }, r.limits) : null,
      r.usage ? h('p', { class: 'note', 'data-usage': '' }, `使ったトークン：入力 ${count(r.usage.input_tokens)}（うちキャッシュから読んだ分 ${count(r.usage.cached_input_tokens)}、キャッシュに書いた分 ${count(r.usage.cache_write_tokens)}）、出力 ${count(r.usage.output_tokens)}。モデルの呼び出し${r.usage.calls}回`) : null,
      (r.concerns || []).map((/** @type {any} */ c) => h('div', { class: 'box', 'data-concern': c.id },
        h('div', { class: 'pad' },
          h('p', {}, h('strong', {}, c.concern)),
          h('p', {}, `推定：${c.estimate}`),
          h('p', { class: 'note' }, `根拠の種類：${/** @type {any} */ (BASIS)[c.basis] || c.basis}`),
          h('p', {}, '根拠の行：', (c.evidence || []).map((/** @type {any} */ e, /** @type {number} */ i) => [i ? '、' : '', h('a', { href: lineUrl(e.path, e.start, e.end, c.commit), target: '_blank', rel: 'noopener', 'data-evidence': '' }, `${e.path} ${e.start}〜${e.end}行${c.commit && c.commit !== state.source.commit ? `（コミット ${short(c.commit)}）` : ''}`)])),
          h('p', {}, `確かめ方：${/** @type {any} */ (CHECK_BY)[c.check?.by] || c.check?.by}。${c.check?.detail || ''}${c.check?.observation ? `（観測の種類：${c.check.observation}）` : ''}`)))),
      (r.questions || []).length ? [h('h3', {}, 'コードからは決められなかったこと'), h('ul', { class: 'items' }, r.questions.map((/** @type {string} */ q) => h('li', {}, q)))] : null));
}

// ---------- 懸念を試験の案の候補にする（#230）----------
// 確かめ方が「麦漆で動かす」の懸念だけを候補にする。懸念は「こうなると問題」という形なので、候補の要求の文は「その懸念が起きていないこと」にする。
const concernChosen = new Map();
function evidenceText(/** @type {any} */ c) { return concernEvidenceText(c, state.source.commit); }
function concernCandidate(/** @type {any} */ c) { return concernPlanCandidate(c, state.source.commit); }
function chosenConcerns(/** @type {any} */ r) {
  const id = r?.investigation?.result?.record;
  return runConcerns(r).filter((/** @type {any} */ c) => concernChosen.get(`${id}:${c.id}`) !== false);
}
function concernsView(/** @type {any} */ r) {
  const id = r?.investigation?.result?.record;
  const all = r?.investigation?.result?.concerns || [];
  if (!all.length) return null;
  const others = all.filter((/** @type {any} */ c) => c.check?.by !== 'run');
  return card({ class: 'flush', 'data-concerns': '' }, h('div', { class: 'card-head' }, h('strong', {}, 'コードから出た懸念'), h('span', { class: 'note' }, `対象の操作：${r.investigation.operation}`)),
    runConcerns(r).length ? runConcerns(r).map((/** @type {any} */ c) => {
      const box = h('input', { type: 'checkbox', checked: concernChosen.get(`${id}:${c.id}`) !== false, 'aria-label': `${c.id}を試験の案の候補にする` });
      box.addEventListener('change', () => concernChosen.set(`${id}:${c.id}`, /** @type {HTMLInputElement} */ (box).checked));
      return h('label', { class: 'item-row', 'data-concern-candidate': c.id }, box, h('div', {}, h('p', { class: 'item-title' }, c.concern), h('p', { class: 'note' }, `確かめ方：${c.check.detail}（観測の種類：${c.check.observation}）。根拠の行：${evidenceText(c)}`)));
    }) : h('p', { class: 'note pad' }, '動かして確かめる懸念はありません。'),
    others.length ? h('div', { class: 'pad', 'data-concerns-other': '' }, h('h3', {}, '試験にしない懸念'),
      h('ul', { class: 'items' }, others.map((/** @type {any} */ c) => h('li', {}, `${c.concern}（確かめ方：${/** @type {any} */ (CHECK_BY)[c.check?.by] || c.check?.by}。${c.check?.detail || ''}）`)))) : null);
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
  const ids = (f.citations || []).map((/** @type {any} */ c) => /** @type {any} */ (SECTION_OF)[c.source_id] || 'other');
  return SECTIONS.map(([k]) => k).find(k => ids.includes(k)) || 'other';
}
function bySection(/** @type {any[]} */ items) {
  return SECTIONS.map(([k, name]) => ({ key: k, name, rows: items.filter(f => sectionOf(f) === k).sort((a, b) => (/** @type {any} */ (PRIO_ORDER)[a.priority] ?? 3) - (/** @type {any} */ (PRIO_ORDER)[b.priority] ?? 3)) })).filter(s => s.rows.length);
}
/** 抜粋の表示用に、Markdownの強調記号と余分な空白を外す。 */
function plain(/** @type {string} */ text) { return (text || '').replace(/\*\*/g, '').replace(/(^|\s)\*(?=\s|$)/g, '$1・').replace(/\s+・?\s*$/, '').replace(/\s+/g, ' ').trim(); }
const docOf = (/** @type {any} */ c) => [c.document, c.page ? `${c.page}ページ` : ''].filter(Boolean).join(' ');
/** 候補を挙げた根拠の一節と、その文書・ページ。 */
function basisView(/** @type {any} */ f) {
  const cs = (f.citations || []).filter((/** @type {any} */ c) => c.excerpt);
  if (!cs.length) return null;
  return h('details', { class: 'basis', 'data-basis': '' }, h('summary', {}, '根拠を読む'),
    cs.map((/** @type {any} */ c) => [h('blockquote', {}, plain(c.excerpt)), h('p', { class: 'note' }, docOf(c))]));
}

// ---------- 2 照合とヒアリング ----------
async function scanScreen() {
  if (!state.record) state.record = {};
  const r = state.record;
  const found = testable();
  const askConcerns = (r.investigation?.result?.concerns || []).filter((/** @type {any} */ c) => c.check?.by === 'ask');
  const hasQuestions = (r.scan?.questions || []).length > 0 || askConcerns.length > 0;
  const list = r.scan ? (found.length ? bySection(found).map(s => card({ class: 'flush', 'data-section': s.key }, h('div', { class: 'card-head' }, h('strong', {}, s.name)),
    h('ul', { class: 'candidates' }, s.rows.map((/** @type {any} */ f) => h('li', { 'data-finding': f.id },
      h('span', { class: 'lead-badge' }, prio(f.priority)),
      h('div', {}, h('p', { class: 'item-title' }, f.requirement),
        h('p', { class: 'note' }, `根拠：${docOf(f.citations?.[0] || {})}`, f.system_refs?.length ? ['　関係するコード：', h('span', { class: 'code' }, f.system_refs.join('、'))] : null),
        f.basis?.includes('screen') ? h('p', { class: 'note', 'data-basis-screen': '' }, '根拠に、試験で記録した画面の構造を含みます。') : null,
        basisView(f)))))))
    : h('p', { class: 'note' }, '試験で確かめられる候補は見つかりませんでした。')) : h('p', { class: 'note', 'data-no-scan': '' }, '照合はまだしていません。');
  const other = r.scan ? (r.scan.findings || []).length - found.length : 0;
  screen({
    sub: '2 照合とヒアリング　1/2',
    title: r.scan ? (found.length ? `不足がありそうな点が${found.length}件見つかりました` : '試験で確かめられる点は見つかりませんでした') : 'コードから出た懸念',
    lead: r.scan ? 'ガイドラインとコードを照らし合わせ、満たしていない可能性がある項目を優先度の順に並べました。' : '',
    body: [carriedNote(r.scan?.carried_from, 'この照合の結果'), h('div', { class: 'stack' }, list), r.scan ? h('p', { class: 'note small', 'data-scan-count': '' }, `候補 ${(r.scan.findings || []).length}件のうち、試験で確かめられる ${found.length}件を示しています${other ? `（文書の確認などで確かめる ${other}件は除いています）` : ''}。${r.scan.priority_rule || '優先度は決まった規則で付けています'}`) : null,
      concernsView(r),
      h('details', { class: 'trace', 'data-trace': '', open: !r.scan || !!r.investigation }, h('summary', {}, '操作からコードをたどる（任意）'), investigationView(() => r.summary || ''))],
    back: () => go('files'),
    primary: primary(hasQuestions ? '次へ：質問に答える' : '次へ', () => go('hearing')),
  });
}

async function hearingScreen() {
  const r = state.record;
  const qs = r.scan?.questions || [];
  const askConcerns = (r.investigation?.result?.concerns || []).filter((/** @type {any} */ c) => c.check?.by === 'ask');
  const askOut = h('div', {});
  const ask = h('textarea', { rows: 3, id: 'ask', class: 'field', placeholder: '例：ログの保存期間はどれくらい必要ですか' });
  const question = (/** @type {any} */ q, /** @type {boolean} */ single) => h('fieldset', { class: 'choices', 'data-question': q.id }, h('legend', { class: single ? 'visually-hidden' : '' }, q.text),
    q.why ? h('p', { class: 'note' }, q.why) : null,
    h('div', { class: 'choice-grid' }, q.options.map((/** @type {string} */ o) => {
      const radio = h('input', { type: 'radio', name: q.id, checked: answerTo(r, q) === o });
      radio.addEventListener('change', () => { setAnswer(r, q, o); reset(); });
      return h('label', { class: 'choice' }, radio, h('span', {}, o));
    })));
  const single = qs.length === 1 && !askConcerns.length;
  // 試験の案を作るために送る内容は、この画面の中で確かめる（画面を増やさない）。
  // 1回目の「試験の案を作る」で送る内容を組み立てて示し、確かめた印を付けると、同じボタンで送れる。回答を変えたら、確かめ直す。
  const sendOut = h('div', { 'data-plan-send': '' });
  /** @type {any} */ let ready = null;
  const reset = () => { ready = null; fill(sendOut); toPlan.removeAttribute('disabled'); };
  const toPlan = primary('試験の案を作る', () => guarded(async () => {
    if (ready) return sendConfirmed(ready.send, ready.preview, ready.agree);
    const concerns = chosenConcerns(r);
    const findings = [...testable().map((/** @type {any} */ f) => ({ id: f.id, requirement: f.requirement, how: f.verification.how || '' })), ...concerns.map(concernCandidate)];
    if (!findings.length) return showError('試験で確かめられる項目がありません。');
    if (findings.length > 24) return showError('試験の案の候補は24件までです。懸念の選択を減らしてください。');
    const at = planGeneration(r);
    const answers = answersOf(r);
    const send = {
      path: '/api/v1/agent/plan', back: () => go('hearing'),
      body: { repository: state.source.repository, commit: state.source.commit, files: r.files || [], evidence: planEvidence(concerns), summary: r.summary || '', answers, findings, run: !!state.runnable?.runnable, ...screenOf(r) },
      wait: { sub: '3 検証案の了承', title: '試験の案を作っています', lead: 'AIエージェントが、不足がありそうな点を確かめる試験を考えています。',
        items: ['確認候補と回答をAIエージェントに送る', '試験の手順と合格の条件を待つ', '試験の案を記録する'], note: '応答までに1分ほどかかることがあります。' },
      done: async (/** @type {any} */ p) => { applyPlan(r, p, at, concerns); await saveRecord(); return { lead: `${p.tests.length}件の試験の案ができました。`, next: 'plan' }; },
    };
    const preview = await loadPreview(send.path, send.body);
    const panel = confirmPanel(preview, [['確認候補', `${findings.length}件`], ['ファイル', fileList(r.files || [])], answers.length ? ['回答', answers.map(a => `${a.question}：${a.answer}`).join('／')] : null, ['確かめ方', howToCheck()]], toPlan);
    fill(sendOut, h('h2', {}, 'AIエージェントに送る内容を確かめる'), h('p', { class: 'note' }, '照合で読んだファイルと、確認候補と、ヒアリングの回答を送ります。確かめた印を付けて、もう一度「試験の案を作る」を押してください。'), panel.nodes);
    ready = { send, preview, agree: panel.agree };
    sendOut.scrollIntoView({ block: 'nearest' });
  }));
  screen({
    sub: '2 照合とヒアリング　2/2',
    title: single ? qs[0].text : qs.length || askConcerns.length ? 'コードだけでは判断できないことに答えてください' : '確かめたいことはありません',
    lead: qs.length || askConcerns.length ? 'コードだけでは判断できないことを確かめます。回答は試験の案に使います。' : '試験の案を作る前に、ガイドラインに質問することもできます。',
    body: [qs.map((/** @type {any} */ q) => question(q, single)),
      askConcerns.length ? card({ 'data-ask-concerns': '' }, h('h2', {}, '懸念のうち、人に聞いて確かめること'), h('ul', { class: 'items' }, askConcerns.map((/** @type {any} */ c) => h('li', {}, `${c.check.detail}（懸念：${c.concern}）`)))) : null,
      h('details', { 'data-ask': '', open: (r.asks || []).length > 0 }, h('summary', {}, 'ガイドラインに質問する（任意）'),
        h('label', { for: 'ask', class: 'field-label' }, '聞きたいこと'), ask,
        h('div', { class: 'actions' }, btn('質問を送る', () => {
          reset(); // 確認のダイアログと、この画面の確認を同時に出さない
          const q = /** @type {HTMLTextAreaElement} */ (ask).value.trim(); if (!q) return showError('質問を書いてください。');
          return confirmAndSend(askOut, '/api/v1/agent/ask', { question: q }, async (/** @type {any} */ a) => { r.asks = [...(r.asks || []), { question: q, ...a }]; await saveRecord(); render(); });
        })), askOut,
        (r.asks || []).slice().reverse().map((/** @type {any} */ a) => h('div', { class: 'box', 'data-asked': '' }, h('div', { class: 'pad' }, h('strong', {}, `質問：${a.question}`), h('div', {}, a.answer),
          a.citations?.map((/** @type {any} */ c) => h('div', { class: 'note' }, `出典：${c.document}・${c.page}ページ`)))))),
      r.plan ? h('p', { class: 'note', 'data-rescan-note': '' }, '照合し直すと、今の試験の案と結果は履歴に移ります。') : null,
      sendOut],
    back: () => go('scan'),
    extra: r.scan ? btn('回答して照合し直す', () => go('confirmScan'), 'tertiary') : null,
    primary: toPlan,
  });
}

/** 送る前の確認（ダイアログ）。ガイドラインへの質問で使う。 */
function confirmAndSend(/** @type {HTMLElement} */ out, /** @type {string} */ path, /** @type {any} */ body, /** @type {(r: any) => any} */ onSent) {
  return guarded(async () => {
    setStatus('送る内容を組み立てています。');
    const preview = await api('POST', path, body);
    state.agent = await api('GET', '/api/v1/agent').catch(() => state.agent);
    setStatus('');
    fill(out);
    const dialog = /** @type {HTMLDialogElement} */ (h('dialog', { class: 'send-dialog', 'data-preview': '', 'aria-labelledby': 'send-title' }));
    const close = () => { dialog.close(); dialog.remove(); };
    const agree = h('input', { type: 'checkbox', id: 'agree' });
    const model = state.agent?.model;
    const send = btn('この内容を送る', () => guarded(async () => {
      send.setAttribute('disabled', '');
      setStatus('AIエージェントに送っています。');
      let r;
      try { r = await api('POST', path, { ...body, confirm: { payload_sha256: preview.payload_sha256, synthetic_only: /** @type {HTMLInputElement} */ (agree).checked } }); }
      finally { close(); }
      await onSent(r); setStatus('');
    }), 'primary');
    send.setAttribute('disabled', '');
    agree.addEventListener('change', () => /** @type {HTMLInputElement} */ (agree).checked && model ? send.removeAttribute('disabled') : send.setAttribute('disabled', ''));
    fill(dialog, h('h2', { id: 'send-title' }, '送信内容の確認'),
      h('dl', { class: 'facts' }, h('dt', {}, '送信先'), h('dd', {}, '照合用のAIエージェント ', state.agent?.endpoint ? h('span', { class: 'code' }, state.agent.endpoint) : null,
        model ? h('div', { 'data-agent-model': '' }, 'その先のモデル ', h('span', { class: 'code' }, model)) : h('div', { class: 'alert', 'data-agent-model-missing': '' }, `その先のモデルを確かめられないため、送れません。${state.agent?.model_error || ''}`)),
        h('dt', {}, '質問'), h('dd', {}, body.question)),
      h('details', {}, h('summary', {}, '送る内容をすべて見る'), h('pre', { class: 'payload' }, JSON.stringify(preview.payload, null, 2))),
      h('details', { class: 'tech' }, h('summary', {}, '技術的な詳細'), h('p', { class: 'note' }, '送信記録と照らし合わせるための値（SHA-256）'), h('p', { class: 'code', 'data-payload-sha256': '' }, preview.payload_sha256)),
      h('label', { class: 'agree' }, agree, '架空のデータだけで、実在の患者の情報・認証情報・生のログを含まないことを確かめた'),
      h('div', { class: 'actions dialog-actions' }, btn('戻る', close, 'tertiary'), send));
    document.body.append(dialog);
    dialog.addEventListener('cancel', close);
    dialog.showModal();
  });
}

// ---------- 3 検証案の了承 ----------
/** 今の案の候補（照合の候補と、案に写した懸念）。 */
function candidatesOf(/** @type {any} */ r) {
  return Object.fromEntries([
    ...(r.scan?.findings || []).map((/** @type {any} */ f) => [f.id, f]),
    ...(r.plan?.concerns || []).map((/** @type {any} */ c) => [c.id, { id: c.id, requirement: concernRequirement(c), concern: c }]),
  ]);
}
const topPriority = (/** @type {any[]} */ fs) => fs.map(f => f?.priority).filter(Boolean).sort((a, b) => (/** @type {any} */ (PRIO_ORDER)[a] ?? 3) - (/** @type {any} */ (PRIO_ORDER)[b] ?? 3))[0];
const commandOf = (/** @type {string} */ entry, /** @type {string[]} */ args) => ['python3', entry, ...args].map(a => /[\s"'{}]/.test(a) ? JSON.stringify(a) : a).join(' ');
/** 試験の設定の表示。AIが案に含め、麦漆が検査した設定を、そのまま示す。 */
/** 前のコミットから引き継いだ照合・試験の案であることの注記。今のコミットのコードは照合していないことを示す。 */
function carriedNote(/** @type {string | undefined} */ from, /** @type {string} */ what) {
  return from ? h('p', { class: 'note', 'data-carried': from }, `${what}は、前のコミット ${short(from)} のコードから作ったものを引き継いでいます。今のコミット ${short(state.source.commit)} のコードは、AIと照合していません。照合し直すときは「1 照合の準備」から進めます。`) : null;
}
function trialView(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  if (!t.trial) return h('div', { class: 'full', 'data-trial': 'none' }, h('h3', {}, '実行のしかた'), h('p', {}, '自動では実行しません。了承すると確認項目に登録し、試験の設定画面で設定してから実行します。'), t.trial_note ? h('p', { class: 'note' }, `理由：${t.trial_note}`) : null);
  const tr = t.trial;
  // 合否は、候補に対応づけた判定条件だけで決まる。合否に使わない条件にはその旨を添え、了承の前に確かめられるようにする。
  // 候補が2つ以上の試験では、条件ごとに、どの候補の合否を決めるかも添える。
  const owners = (/** @type {string} */ id) => tr.links.filter((/** @type {any} */ l) => l.check_ids.includes(id)).map((/** @type {any} */ l) => findings[l.finding_id]?.requirement || l.finding_id);
  const many = t.finding_ids.length > 1;
  return [
    h('div', { class: 'full', 'data-trial': 'auto' }, h('h3', {}, '実行のしかた'), h('p', { class: 'code small' }, commandOf(tr.entrypoint, tr.args)),
      tr.readback ? h('p', { class: 'note' }, '続けて読み出す：', h('span', { class: 'code' }, commandOf(tr.readback.entrypoint, tr.readback.args))) : null,
      h('p', { class: 'note' }, `試行ごとに新しい作業ディレクトリで起動します。{{test_value}} には、麦漆が試行ごとに作る架空の値（試験の値）が入ります。${tr.storage ? `保存先 ${tr.storage} を麦漆が直接読み出します。` : ''}`),
      t.trial_note ? h('p', { class: 'note', 'data-trial-note': '' }, `AIの注記：${t.trial_note}`) : null),
    h('div', { class: 'full' }, h('h3', {}, '判定の条件'), h('ul', { class: 'items' }, tr.checks.map((/** @type {any} */ c) => {
      const of = owners(c.id);
      return h('li', { 'data-trial-check': c.id }, `${c.scope}：${/** @type {any} */ (METHOD_LABEL)[c.method] || c.method}が${EXPECT_LABEL(c)}`,
        !of.length ? h('span', { class: 'note', 'data-unlinked': '' }, '（記録するだけで、合否には使いません）') : many ? h('span', { class: 'note', 'data-trial-link': '' }, `（確かめる項目：${of.join('／')}）`) : null);
    }))),
  ];
}
async function planScreen() {
  const r = state.record; const findings = candidatesOf(r);
  if (!planIsCurrent(r)) return go('scan');
  const done = (/** @type {any} */ t) => approvedCheck(r, t);
  const auto = r.plan.tests.some((/** @type {any} */ t) => t.trial);
  const H3 = (/** @type {string} */ t) => h('h3', {}, t);
  const cards = r.plan.tests.map((/** @type {any} */ t) => {
    const fs = t.finding_ids.map((/** @type {string} */ i) => findings[i]);
    const box = h('input', { type: 'checkbox', checked: r.chosen?.[t.id] !== false, disabled: !!done(t), 'aria-label': `${t.name}を了承する` });
    box.addEventListener('change', () => { r.chosen = { ...(r.chosen || {}), [t.id]: /** @type {HTMLInputElement} */ (box).checked }; });
    const cite = fs.map((/** @type {any} */ f) => f?.citations?.[0]).find((/** @type {any} */ c) => c?.excerpt);
    const refs = [...new Set(fs.flatMap((/** @type {any} */ f) => f?.system_refs || (f?.concern ? (f.concern.evidence || []).map((/** @type {any} */ e) => e.path) : [])))];
    return h('article', { class: 'card flush test', 'data-test': t.id },
      h('div', { class: 'pad' },
        h('div', { class: 'test-head' }, box, h('div', { class: 'grow' }, h('h2', {}, t.name), h('p', { class: 'note' }, `確かめる項目：${fs.map((/** @type {any} */ f) => f?.requirement || '').join('／')}`)), prio(topPriority(fs)),
          done(t) ? h('a', { class: 'note', href: workbenchUrl(/** @type {string} */ (done(t))) }, '登録済み') : null),
        h('div', { class: 'two' }, h('div', {}, H3('手順'), h('ol', {}, t.steps.map((/** @type {string} */ s) => h('li', {}, worded(s))))),
          h('div', {}, H3('合格の条件'), t.pass.map((/** @type {string} */ p) => h('p', { class: 'strong' }, worded(p)))))),
      h('details', { class: 'more' }, h('summary', {}, '詳細'),
        h('div', { class: 'two' },
          cite ? h('div', { class: 'full' }, H3('根拠'), h('blockquote', {}, plain(cite.excerpt)), h('p', { class: 'note' }, docOf(cite))) : null,
          fs.some((/** @type {any} */ f) => f?.concern) ? h('div', { class: 'full' }, H3('コードから出た懸念'), fs.filter((/** @type {any} */ f) => f?.concern).map((/** @type {any} */ f) => h('p', {}, f.concern.concern, h('span', { class: 'note' }, `（根拠の行：${evidenceText(f.concern)}）`)))) : null,
          trialView(t, findings),
          h('div', {}, H3('観測するもの'), h('p', {}, t.observation_label || t.observation)),
          refs.length ? h('div', {}, H3('関係するコード'), h('p', { class: 'code small' }, refs.join('、'))) : null)));
  });
  screen({
    sub: '3 検証案の了承', title: '試験の案を確かめてください',
    lead: auto ? '不足がありそうな点を確かめる試験です。了承した試験だけを、検証基盤が試験環境で実行します。' : '不足がありそうな点を確かめる試験です。了承した試験を確認項目に登録します。',
    body: [carriedNote(r.plan.carried_from, 'この試験の案'), h('p', { class: 'note', 'data-plan-count': '' }, `${new Set(r.plan.tests.flatMap((/** @type {any} */ t) => t.finding_ids)).size}つの確認項目を、${r.plan.tests.length}件の試験で確かめます。`),
      h('div', { class: 'stack' }, cards),
      r.plan.not_planned?.length ? h('p', { class: 'note', 'data-not-planned': '' }, `試験の案に入らなかった項目：${r.plan.not_planned.map((/** @type {any} */ n) => `${findings[n.finding_id]?.requirement || n.finding_id}（AIの理由：${n.reason || '理由の記載なし'}）`).join('、')}`) : null,
      r.plan.unclassified?.length ? h('p', { class: 'note', 'data-unclassified': '' }, `試験の案に含まれず、理由もない項目：${r.plan.unclassified.map((/** @type {string} */ id) => findings[id]?.requirement || id).join('、')}`) : null],
    back: () => go('hearing'),
    primary: primary(auto ? '了承して実行する' : '了承して登録する', () => guarded(runApproved)),
  });
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
/** 試験の結果をコードへたどれるように、案の候補が関係するとしたファイル（なければ照合で読んだ最初のファイル）を対応づける。 */
function codeRef(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  const concerns = t.finding_ids.map((/** @type {string} */ id) => findings[id]?.concern).filter(Boolean);
  const paths = (state.source.inventory || []).map((/** @type {any} */ f) => f.path).filter(Boolean);
  const at = concerns.length ? concernSource(concerns, state.source.commit, paths) : null;
  if (at) return { repository: state.source.repository, commit: state.source.commit, ...at };
  const files = state.record.files || [];
  const refs = t.finding_ids.flatMap((/** @type {string} */ id) => findings[id]?.system_refs || []);
  const path = t.trial?.entrypoint && files.includes(t.trial.entrypoint) ? t.trial.entrypoint : refs.find((/** @type {string} */ p) => files.includes(p)) || files[0];
  return path ? { repository: state.source.repository, commit: state.source.commit, path, line: 1, end_line: 1 } : null;
}
/** 試験の設定から、実行要求の試験設定を作る（ui/workbench.js の buildRecipe と同じ形）。 */
function recipeOf(/** @type {any} */ check, /** @type {any} */ trial, /** @type {any} */ criterion) {
  return { schema_version: 1, check_id: check.id, source: check.source, runtime: 'python3', launch: 'file', entrypoint: trial.entrypoint, args: trial.args, input: {}, criterion, timeout_ms: 10000,
    python: null, server: null, containers: null, http: null,
    trial: { schema_version: 1, fault: 'normal', storage: trial.storage || null, readback: trial.readback ? { entrypoint: trial.readback.entrypoint, args: trial.readback.args } : null,
      checks: trial.checks.map((/** @type {any} */ c) => ({ id: c.id, scope: c.scope, method: c.method, expect: c.expect, text: c.method === 'exit_code' ? null : c.text, target: null })) } };
}
/** 了承した試験を確認項目に登録する。試験の設定がある試験は、判定条件の対応づけまで登録し、実行要求の試験設定を返す。 */
async function register(/** @type {any} */ t, /** @type {Record<string, any>} */ findings) {
  const r = state.record;
  const base = `flow-${short(state.source.commit)}-${t.id.replace(/[^a-z0-9-]/gi, '').slice(0, 20)}`;
  const call = r.plan.model_call || {};
  const kinds = new Set(t.finding_ids.map((/** @type {string} */ id) => findings[id]?.concern ? 'concern' : 'guideline'));
  const label = kinds.has('concern') ? (kinds.has('guideline') ? 'ガイドライン照合とコードから出た懸念の試験の案' : 'コードから出た懸念の試験の案') : 'ガイドライン照合の試験の案';
  const carried = r.plan.carried_from ? `。案はコミット ${short(r.plan.carried_from)} の照合から作ったものを引き継いだ` : '';
  const origin = `${label}（${call.response_model || call.requested_model || 'AI'}・応答ID ${call.response_id || '不明'}）を${person()}が了承して登録。対象 ${state.source.repository}@${short(state.source.commit)}${carried}`;
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
  /** @type {any} */ let recipe = null;
  if (t.trial) {
    // 試験の案の判定条件で判定するように設定してから、判定条件の内容（definition）を添えて対応づける。
    const criterion = { assertion: 'eel.trial', subject: id };
    await change({ op: 'configure_observation', check_id: id, criterion }, '了承した試験の案の判定条件で判定するように設定した');
    const check = state.view.workspace.checks.find((/** @type {any} */ c) => c.id === id);
    recipe = recipeOf(check, t.trial, criterion);
  }
  const aspects = t.finding_ids.map((/** @type {string} */ fid) => {
    const f = findings[fid]; const c = f.citations?.[0] || {};
    const origin = f.concern
      ? `コードから出た懸念（コードからの推定で、観測でも判定でもない）。根拠の行：${evidenceText(f.concern)}。調べの記録 ${r.plan.investigation}`
      : `照合の候補。出典：${c.document || ''} ${c.page || ''}ページ（SHA-256 ${(c.sha256 || '').slice(0, 12)}…）`;
    const link = t.trial?.links?.find((/** @type {any} */ l) => l.finding_id === fid);
    const trialChecks = link && recipe ? link.check_ids.map((/** @type {string} */ cid) => ({ id: cid, definition: { check: recipe.trial.checks.find((/** @type {any} */ c) => c.id === cid), recipe } })) : [];
    return { id: fid, text: f.requirement.slice(0, 2000), origin: origin.slice(0, 1000), observation: link ? link.observation : t.observation, trial_checks: trialChecks };
  });
  await change({ op: 'set_aspects', check_id: id, aspects }, t.trial ? '試験で確かめる項目と、了承した判定条件への対応づけを登録した' : '試験で確かめる項目を登録した');
  return { id, recipe };
}
let registering = false;
/** 了承した試験を登録し、試験の設定がある試験を順に実行する。途中で失敗しても、登録・実行できた分は残る。 */
async function runApproved() {
  if (registering) return; // 連打で同じ試験を二重に登録しない
  const r = state.record; const findings = candidatesOf(r);
  await loadCase();
  const chosen = r.plan.tests.filter((/** @type {any} */ t) => r.chosen?.[t.id] !== false);
  // 登録と実行を分けて扱う。前回、登録の後に取得や実行で止まった試験は、登録し直さずに実行だけをやり直す。
  const executed = (/** @type {any} */ t) => { const id = approvedCheck(r, t); return !!id && !!state.view.checks.find((/** @type {any} */ c) => c.check_id === id)?.evidence?.some((/** @type {any} */ e) => e.applicable); };
  const tests = chosen.filter((/** @type {any} */ t) => !approvedCheck(r, t));
  const runs = chosen.filter((/** @type {any} */ t) => t.trial && !executed(t));
  if (!tests.length && !runs.length && approvedTests(r).length) return go('result');
  if (!tests.length && !runs.length) throw Error('了承する試験を選んでください。');
  registering = true;
  const w = waiting({ sub: '4 結果', title: runs.length ? '検証しています' : '確認項目に登録しています',
    lead: runs.length ? '検証基盤が試験環境でアプリを動かし、出力と保存先を記録しています。' : '了承した試験を確認項目に登録しています。',
    items: [runs.length ? '試験環境を用意する' : '確認項目に登録する', ...runs.map((/** @type {any} */ t) => t.name), '期待した動作と、実際の記録を比べる'],
    note: runs.length ? `試験は架空のデータだけで行います。${runs.length}件で数分かかることがあります。` : '' });
  /** @type {string[]} */ const failed = [];
  try {
    /** @type {Map<string, any>} */ const recipes = new Map();
    // 1件ずつ登録して控える。途中で失敗しても、登録できた分は了承済みとして残り、やり直しで二重に作らない。
    for (const t of tests) { const { id, recipe } = await register(t, findings); approve(r, t, id); if (recipe) recipes.set(t.id, recipe); await saveRecord(); }
    // 登録済みで未実行の試験は、登録した確認項目と了承した試験の設定から、実行の要求を作り直す。
    for (const t of runs) if (!recipes.has(t.id)) {
      await loadCase();
      const id = /** @type {string} */ (approvedCheck(r, t));
      const check = state.view.workspace.checks.find((/** @type {any} */ c) => c.id === id);
      recipes.set(t.id, recipeOf(check, t.trial, { assertion: 'eel.trial', subject: id }));
    }
    if (runs.length) await api('POST', '/api/v1/prepare', { repository: state.source.repository, commit: state.source.commit });
    w.next();
    for (const t of runs) {
      try {
        await loadCase();
        const job = await api('POST', '/api/v1/execute', { expected_revision: state.view.workspace.revision, actor: `${person()}（開発者）`, reason: `了承した試験の案「${t.name}」を実行した`, recipe: recipes.get(t.id) });
        let status;
        for (let i = 0; i < 600; i++) {
          await sleep(500);
          status = await api('GET', '/api/v1/execution');
          if (status.job?.id === job.id && status.job.status !== 'running') break;
        }
        if (status?.job?.status !== 'completed') throw Error(status?.job?.error || '状態を確認できません');
        w.next('記録した');
      } catch (e) { failed.push(`${t.name}：${e instanceof Error ? e.message : String(e)}`); w.fail('実行できなかった'); }
    }
    await loadCase();
    await w.done(failed.length ? `${failed.length}件の試験を実行できませんでした。結果の画面で確かめてください。` : runs.length ? `${runs.length}件の試験が終わりました。` : `${tests.length}件の試験を登録しました。`);
  } catch (e) {
    // 登録の途中で止まった場合は、登録できた分を残して試験の案の画面に戻る。やり直すと続きから登録する。
    registering = false; go('plan'); throw e;
  } finally { registering = false; }
  go('result');
  if (failed.length) showError(`実行できなかった試験：${failed.join('／')}`);
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
  // 設定画面で同梱の試験（固定した版のレシピ）を選ぶと、確認項目の参照先がその版へ変わる。照合したコミットと違う版の結果は数えない。
  const bound = state.view.workspace?.checks?.find((/** @type {any} */ c) => c.id === checkId)?.source?.commit;
  const otherVersion = !!bound && bound !== state.source.commit;
  const st = otherVersion ? 'other_version' : !ev ? 'pending' : a?.status === 'observed' ? 'ok' : a?.status === 'contradicted' ? 'ng' : 'unconfirmed';
  const linked = (a?.trial_checks || []).length > 0;
  // 対応づけた判定条件の、試行で記録した結果（麦漆の要約の文）。
  const ids = new Set((a?.trial_checks || []).map((/** @type {any} */ l) => l.id));
  const observed = (ev?.summary?.items || []).filter((/** @type {any} */ i) => ids.has(i.id));
  const value = testValuesOf(ev?.run_id).find(v => observed.some((/** @type {any} */ i) => (i.detail || '').includes(v)));
  return { state: st, checkId, test, aspect: a, linked, boundCommit: bound, observed, runId: ev?.run_id, record: ev ? `試行 ${ev.run_id}${value ? `（試験で入力した値：${value}）` : ''}` : '—' };
}
/** 試行で使った試験の値。麦漆は試行のIDから作る（src/execution.rs：「eel-test-」か「EEL-TEST-」に、試行のIDの「run-」より後ろを続ける）。 */
function testValuesOf(/** @type {string | undefined} */ runId) {
  const id = (runId || '').replace(/^run-/, '');
  return id ? [`eel-test-${id}`, `EEL-TEST-${id}`] : [];
}
/** 判定条件ごとの結果の説明（麦漆が記録した文）を、結果の画面で読みやすくする。記録した事実は変えない。
 * 試行ごとに変わる試験の値は「試験で入力した値」と書き（値そのものは詳細の「記録」に示す）、読み出しの出力は試験の案の画面と同じ呼び方にする。 */
function readable(/** @type {string} */ detail, /** @type {string | undefined} */ runId) {
  let text = detail || '';
  for (const v of testValuesOf(runId)) text = text.replaceAll(`「${v}」`, '試験で入力した値').replaceAll(v, '（試験で入力した値）');
  return text.replaceAll('別のプロセスで読み出した標準出力に', '読み出しの出力に');
}
/** 実際に確かめた判定条件（mugiurushi の試験設定の文言）。対応づけた判定条件が無ければ書かない。 */
function checkedBy(/** @type {any} */ aspect) {
  return (aspect?.trial_checks || []).map((/** @type {any} */ l) => `- 確かめた判定条件: ${l.id} ${l.label || ''}`.trim());
}
const STATE_BADGE = { ng: ['満たさない', 'error'], ok: ['満たす', 'success'], unconfirmed: ['未確認', 'neutral'], pending: ['未実行', 'neutral'], other_version: ['別の版の試験', 'neutral'] };
/** 試験で起きたこと。期待と異なった判定条件の説明を使う。要求の文（期待した動作）を結果として書かない。 */
function happened(/** @type {any} */ f) {
  const bad = (f.res.observed || []).filter((/** @type {any} */ i) => i.observed === 'contradicted' || i.observed === 'fail');
  if (bad.length) return bad.map((/** @type {any} */ i) => `「${i.scope}」：${readable(i.detail, f.res.runId)}`).join('／');
  const a = f.res.aspect;
  const id = /判定条件 (\S+?)（[^）]*）: 期待と異なる/.exec(a?.detail || '')?.[1];
  const label = (a?.trial_checks || []).find((/** @type {any} */ l) => l.id === id)?.label;
  return label ? `合格の条件「${label}」と異なる結果を観測した` : '期待と異なる結果を観測した';
}
function stateLine(/** @type {any} */ f) {
  const el = stateText(f); el.setAttribute('data-state-label', ''); return el;
}
function stateText(/** @type {any} */ f) {
  const res = f.res;
  if (res.state === 'ng') return h('span', { class: 'ng-text', 'data-happened': '' }, happened(f));
  if (res.state === 'ok') return h('span', { class: 'note' }, (res.observed || []).map((/** @type {any} */ i) => readable(i.detail, res.runId)).join('／') || '期待どおりの結果を観測した');
  if (res.state === 'other_version') return h('span', { class: 'note' }, `確認項目の参照先が ${short(res.boundCommit)} に変わっている（照合したのは ${short(state.source.commit)}）。このコミットの結果としては数えない`);
  if (res.state === 'pending') return h('span', { class: 'note' }, res.linked ? 'まだ実行していない' : 'まだ実行していない（試験の設定画面で設定して実行する）');
  // 判定できなかった理由は、麦漆が記録した判定条件ごとの説明を優先する（読み出しが失敗した、など）。
  const why = (res.observed || []).map((/** @type {any} */ i) => readable(i.detail, res.runId)).filter(Boolean).join('／');
  return h('span', { class: 'note' }, `未確認：${why || `${res.aspect?.status_label || '状態を読み取れない'}。${res.aspect?.detail || ''}`}`);
}
async function resultScreen() {
  await loadCase();
  const r = state.record;
  const items = resultItems().map((/** @type {any} */ f) => ({ ...f, res: resultOf(f) })).filter((/** @type {any} */ f) => f.res.state !== 'none');
  r.results = Object.fromEntries(items.map((/** @type {any} */ f) => [f.id, { requirement: f.requirement, state: f.res.state, ...(f.concern ? { kind: 'concern' } : {}) }]));
  await saveRecord();
  const n = (/** @type {string} */ s) => items.filter((/** @type {any} */ f) => f.res.state === s).length;
  const ng = n('ng');
  const order = { ng: 0, unconfirmed: 1, pending: 2, other_version: 3, ok: 4 };
  const H3 = (/** @type {string} */ t) => h('h3', {}, t);
  const rows = [...items].sort((a, b) => (/** @type {any} */ (order)[a.res.state] - /** @type {any} */ (order)[b.res.state]) || ((/** @type {any} */ (PRIO_ORDER)[a.priority] ?? 3) - (/** @type {any} */ (PRIO_ORDER)[b.priority] ?? 3))).map((/** @type {any} */ f) => {
    const [label, tone] = /** @type {any} */ (STATE_BADGE)[f.res.state];
    const c = f.citations?.[0] || {};
    const k = f.concern;
    return h('div', { class: `result-row is-${f.res.state}`, 'data-result': f.res.state, 'data-finding': f.id },
      h('div', { class: 'result-head' }, badge(label, tone), h('div', {}, h('p', { class: 'item-title' }, f.requirement), h('p', {}, stateLine(f)))),
      h('details', { class: 'more' }, h('summary', {}, '詳細'), h('div', { class: 'two' },
        k ? h('div', { class: 'full' }, H3('コードから出た懸念'), h('blockquote', { 'data-detail-concern': '' }, k.concern), h('p', { class: 'note' }, `コードからの推定：${k.estimate}。根拠の行：${evidenceText(k)}`))
          : c.excerpt ? h('div', { class: 'full' }, H3('根拠'), h('blockquote', {}, plain(c.excerpt)), h('p', { class: 'note' }, docOf(c))) : null,
        h('div', {}, H3('試験'), h('p', {}, f.res.test.name), h('ol', { class: 'note' }, f.res.test.steps.map((/** @type {string} */ s) => h('li', {}, worded(s))))),
        h('div', {}, H3('合格の条件と観測'), h('p', {}, `条件：${worded(f.res.test.pass.join('／'))}`),
          (f.res.observed || []).map((/** @type {any} */ i) => h('p', { class: i.observed === 'contradicted' || i.observed === 'fail' ? 'ng-text' : i.observed === 'observed' || i.observed === 'pass' ? 'ok-text' : 'note' }, `観測（${i.scope}）：${readable(i.detail, f.res.runId)}`)),
          !(f.res.observed || []).length ? h('p', { class: 'note' }, f.res.state === 'pending' ? '観測：まだ実行していない' : `観測：${f.res.aspect?.detail || '記録なし'}`) : null),
        h('div', {}, H3('記録'), h('p', {}, f.res.record)),
        h('div', {}, H3('試験の詳細'), h('a', { href: workbenchUrl(f.res.checkId) }, f.res.state === 'pending' ? '試験の設定画面で設定して実行する' : '試験の設定と記録を開く')))));
  });
  const untested = untestedView();
  screen({
    sub: '4 結果',
    title: items.length ? (ng ? `試験条件を満たさない項目が${ng}件ありました` : n('ok') === items.length ? 'すべての項目が試験条件を満たしました' : '試験の結果') : 'まだ結果はありません',
    lead: n('ok') + ng ? '検証基盤が試験環境でアプリを動かし、記録した事実だけで判定しました。' : '了承した試験を実行すると、ここに結果が出ます。',
    body: [card({ class: 'flush' }, h('div', { class: 'card-head badges' }, badge(`満たさない ${ng}件`, 'error'), badge(`満たす ${n('ok')}件`, 'success'), badge(`未確認 ${n('unconfirmed')}件`, 'neutral'), n('pending') ? badge(`未実行 ${n('pending')}件`, 'neutral') : null), rows),
      h('p', { class: 'note small' }, '記録が足りずに判定できない項目は「未確認」として示します'), untested],
    back: () => go('plan'),
    extra: btn('結果を読み直す', () => render(), 'tertiary'),
    primary: ng ? primary('修正用のプロンプトを作る', () => go('prompt')) : null,
  });
}
/** 試験にしていない候補。未実行や未確認とは分け、合否は付けない。 */
function untestedView() {
  const rows = untestedFindings(state.record, resultItems());
  if (!rows.length) return null;
  return h('details', { 'data-untested-section': '' }, h('summary', {}, `試験にしていない候補（${rows.length}件・合否は付けていません）`),
    h('ul', { class: 'items' }, rows.map(u => h('li', { 'data-untested': u.kind }, h('span', { 'data-untested-text': '' }, u.finding.requirement), h('span', { class: 'note' }, `（${/** @type {any} */ (UNTESTED)[u.kind]}${u.reason ? `。AIの理由：${u.reason}` : ''}）`)))));
}

// ---------- 5 修正用のプロンプト ----------
async function promptScreen() {
  await loadCase();
  const r = state.record;
  const ng = resultItems().map((/** @type {any} */ f) => ({ ...f, res: resultOf(f) })).filter((/** @type {any} */ f) => f.res.state === 'ng');
  const chosen = new Set(ng.map((/** @type {any} */ f) => f.id));
  const pre = h('pre', { class: 'prompt', 'data-prompt': '' });
  const copied = h('div', {});
  const build = () => {
    const blocks = ng.filter((/** @type {any} */ f) => chosen.has(f.id)).flatMap((/** @type {any} */ f) => {
      const c = f.citations?.[0] || {};
      // 懸念の項目は、ガイドラインの代わりに、懸念・コードからの推定・根拠の行を入れる。
      const basis = f.concern
        ? ['根拠: コードから出た懸念（コードからの推定で、観測でも判定でもない）', `> ${f.concern.concern}`, `- コードからの推定: ${f.concern.estimate}`, `- 根拠の行: ${evidenceText(f.concern)}`]
        : [`根拠: ${docOf(c)}`, `> ${plain(c.excerpt)}`];
      // 観測は判定条件の範囲と結果だけを書く。試験に使った値（麦漆が作った架空の値）は入れない。
      const obs = (f.res.observed || []).map((/** @type {any} */ i) => `- 観測（${i.scope}）: ${i.observed_label || i.observed}`);
      return [`## ${f.requirement}`, '', ...basis, '',
        `試験の結果（${f.res.test.name}／${f.res.record}）`, ...checkedBy(f.res.aspect), `- 試験の案の合格の条件: ${f.res.test.pass.join('／')}`, ...(obs.length ? obs : [`- 結果: ${f.res.aspect?.detail || f.res.aspect?.status_label || ''}`]), '',
        `関係しそうなコード: ${(f.concern ? [...new Set((f.concern.evidence || []).map((/** @type {any} */ e) => e.path))] : f.system_refs?.length ? f.system_refs : f.res.test.trial?.entrypoint ? [f.res.test.trial.entrypoint] : r.files || []).join('、')}`, ''];
    });
    pre.textContent = ['# 修正の依頼（mugiurushi の試験結果から）', '', `対象: ${state.source.repository} の${state.branch ? ` ${state.branch} ブランチ（読み込んだコミット ${short(state.source.commit)}）` : `コミット ${short(state.source.commit)}`}`, 'このリポジトリを、次の項目を満たすように修正してください。', '',
      ...(blocks.length ? blocks : ['（直す項目を選んでください）']),
      '## 進め方', '- 先に修正の方針を示してから、変更を小さく分けて行ってください。', '- 試験用の値や認証情報をコードに書き込まないでください。', '- 修正後は mugiurushi で同じ試験を実行し直します。修正しただけでは満たしたとは扱いません。'].join('\n');
  };
  build();
  const H3 = (/** @type {string} */ t) => h('h3', {}, t);
  screen({
    sub: '5 修正用のプロンプト', title: 'コーディング用のAIに渡すプロンプト', lead: '根拠と観測をまとめました。試験の値や認証情報は入れていません。',
    body: [copied, card({ class: 'flush' }, h('details', { class: 'more' }, h('summary', {}, '詳細'), h('div', { class: 'two' },
      h('div', { class: 'full' }, H3('直す項目'), ng.length ? ng.map((/** @type {any} */ f) => {
        const box = h('input', { type: 'checkbox', checked: true });
        box.addEventListener('change', () => { /** @type {HTMLInputElement} */ (box).checked ? chosen.add(f.id) : chosen.delete(f.id); build(); });
        return h('label', { class: 'check-line' }, box, h('span', {}, f.requirement));
      }) : h('p', { class: 'note' }, '試験条件を満たさなかった項目はありません。'), h('p', { class: 'note' }, '試験条件を満たさなかった項目だけを選べます')),
      h('div', {}, H3('入れたもの'), h('p', {}, 'ガイドラインの該当箇所、合格の条件、観測、試行の記録、関係しそうなコード、対象のブランチとコミット')),
      h('div', {}, H3('入れないもの'), h('p', {}, '試験に使った値、認証情報、生のログ'))))),
      card({ class: 'flush' }, pre),
      h('p', { class: 'note' }, '修正したら、修正したブランチを読み込んで同じ試験を実行し直します。', ' ', h('a', { href: '#', onclick: (/** @type {Event} */ e) => { e.preventDefault(); go('retest'); } }, '再試験へ'))],
    back: () => go('result'),
    extra: h('a', { href: '#', class: 'text-link', onclick: (/** @type {Event} */ e) => { e.preventDefault(); const a = h('a', { href: URL.createObjectURL(new Blob([pre.textContent || ''], { type: 'text/markdown' })), download: `fix-${short(state.source.commit)}.md` }); a.click(); } }, '.md で保存'),
    primary: primary('コピーする', () => guarded(async () => {
      await navigator.clipboard.writeText(pre.textContent || '');
      fill(copied, h('div', { class: 'toast', role: 'status' }, h('span', { class: 'ok-text' }, '✓'), 'コピーしました。コーディング用のAIに貼り付けてください'));
    })),
  });
}

// ---------- 修正後の再試験 ----------
async function retestScreen() {
  const records = Object.values(state.flow?.records || {}).filter((/** @type {any} */ x) => x.repository === state.source.repository && x.record?.results);
  const others = records.filter((/** @type {any} */ x) => x.commit !== state.source.commit);
  const current = state.record?.results || {};
  const prev = byLatestSave(others)[0];
  const label = { ok: '満たす', ng: '満たさない', pending: '未実行', unconfirmed: '未確認', other_version: '別の版の試験' };
  const rows = prev ? [...new Set([...Object.values(prev.record.results), ...Object.values(current)].map((/** @type {any} */ x) => x.requirement))].map(req => {
    const b = /** @type {any} */ (Object.values(prev.record.results).find((/** @type {any} */ x) => x.requirement === req));
    const a = /** @type {any} */ (Object.values(current).find((/** @type {any} */ x) => x.requirement === req));
    const change = b?.state === 'ng' && a?.state === 'ok' ? '解消' : b?.state === 'ok' && a?.state === 'ng' ? '新たに満たさない' : a?.state === 'ng' ? '変わらず満たさない' : '—';
    const concern = (a || b)?.kind === 'concern';
    return h('tr', { 'data-compare': change, 'data-kind': concern ? 'concern' : 'guideline' }, h('td', {}, req, concern ? h('div', { class: 'note' }, 'コードから出た懸念') : null), h('td', {}, /** @type {any} */ (label)[b?.state] || '—'), h('td', {}, /** @type {any} */ (label)[a?.state] || '—'), h('td', { class: change === '解消' ? 'ok-text' : change.includes('満たさない') ? 'ng-text' : '' }, change));
  }) : [];
  const select = h('select', { id: 'retest-branch', class: 'field' }, h('option', {}, '読み込んでいます…'));
  /** @type {any} */ let list = null;
  const open = (/** @type {boolean} */ carry) => guarded(async () => {
    const b = list?.branches.find((/** @type {any} */ x) => x.name === /** @type {HTMLSelectElement} */ (select).value);
    if (!b) throw Error('ブランチを選んでください。');
    await openRepository(`https://github.com/${state.source.repository}`, b.name, b.commit, carry);
  });
  // 了承済みの試験があれば、同じ試験の実行し直しを主な操作にする。照合からやり直す操作も残す。
  const canCarry = approvedTests(state.record).length > 0 && !state.record?.plan?.investigation && !!state.record?.results;
  const load = canCarry ? primary('了承した試験を、このブランチで実行し直す', () => open(true)) : primary('このブランチを読み込む', () => open(false));
  screen({
    sub: '5 修正用のプロンプト　再試験', title: '修正したブランチで試験し直す', lead: canCarry ? '修正したブランチを読み込み、了承した試験をそのまま実行し直すと、前回の結果と項目ごとに比べられます。試験は、読み込んだ後にもう一度確かめて了承してから実行します。' : '修正したブランチを読み込むと、同じ流れで照合と試験をやり直し、前回の結果と比べられます。',
    body: [h('label', { for: 'retest-branch', class: 'field-label' }, '読み込むブランチ'), h('div', { class: 'select-wrap' }, select),
      prev ? card({ class: 'flush' }, h('div', { class: 'card-head' }, h('strong', {}, `前回 ${short(prev.commit)} と、今回 ${short(state.source.commit)} の比較`)),
        h('div', { class: 'scroll' }, h('table', { 'data-compare-table': '' }, h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, '項目'), h('th', { scope: 'col' }, `前回 ${short(prev.commit)}`), h('th', { scope: 'col' }, `今回 ${short(state.source.commit)}`), h('th', { scope: 'col' }, '変化'))), h('tbody', {}, rows))))
        : h('p', { class: 'note' }, 'このリポジトリで比べられる前のコミットの結果はまだありません。')],
    back: () => go('prompt'), primary: load,
    extra: canCarry ? btn('照合からやり直す', () => open(false), 'tertiary') : null,
  });
  try {
    list = await api('POST', '/api/v1/branches', { url: `https://github.com/${state.source.repository}` });
    fill(select, list.branches.map((/** @type {any} */ b) => h('option', { value: b.name, selected: b.name !== state.branch && b.name !== list.default_branch ? true : null }, b.name === list.default_branch ? `${b.name}（既定のブランチ）` : b.name)));
  } catch (e) { fill(select, h('option', {}, '選べるブランチがありません')); showError(e); }
}

// ---------- 起動 ----------
async function init() {
  const p = new URLSearchParams(location.search);
  const url = p.get('url'), ref = p.get('ref');
  renderChrome();
  if (!url) return go('repo');
  // 以前の形式のリンク（ref だけ）と、読み込んだ後のリンク（ref はコミット、branch はブランチ名）の両方を開く。
  state.url = url; state.branch = p.get('branch') || '';
  setStatus('リポジトリを読み込んでいます。');
  const res = await api('POST', '/api/v1/understand', { operation: 'open', url, reference: ref || 'HEAD' });
  state.source = res.source;
  if (!state.branch && ref && ref !== res.source.commit && !/^[0-9a-f]{7,40}$/i.test(ref) && ref !== 'HEAD') state.branch = ref;
  state.runnable = await api('POST', '/api/v1/runnable', { repository: state.source.repository, commit: state.source.commit }).catch(() => null);
  await loadFlow();
  setStatus('');
  go(resumeScreen());
}
guarded(init);
