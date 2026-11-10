// 人が試験環境のブラウザで行った操作を、レシピの手順（browser.Step）の形で記録する。
// 要素の指定は、ラベル、役割、CSSの順に試し、そのフレームの中で一意になった最初のものを使う。
// パスワード欄と、秘密値ファイルにある値は、値を記録せずに秘密値の参照へ置き換える。
// 記録した手順は既存の検査と実行器でそのまま使う。再実行で通らない手順を、記録の側で書き換えることはしない。
import fs from 'node:fs/promises';
import {pathToFileURL} from 'node:url';

export const RULES = 1;
const ROLES = ['button', 'textbox', 'link', 'combobox', 'checkbox', 'menuitem', 'tab', 'heading'];
const slug = text => String(text || '').toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 24);

// ページの中で実行する。操作された要素の特徴を集めて、基盤の側へ渡す。入力した値は、パスワード欄では渡さない。
function capture() {
  if (window.__eelCapturing) return; window.__eelCapturing = true;
  const cut = (s, n = 120) => String(s ?? '').replace(/\s+/g, ' ').trim().slice(0, n);
  const describe = el => {
    const tag = el.tagName.toLowerCase(), type = tag === 'input' ? (el.type || 'text') : null;
    const label = el.labels?.length ? cut(el.labels[0].innerText) : el.getAttribute?.('aria-label') ? cut(el.getAttribute('aria-label')) : '';
    const role = el.getAttribute('role') || (tag === 'button' || ['submit', 'button', 'reset'].includes(type) ? 'button' : tag === 'a' && el.hasAttribute('href') ? 'link' : '');
    const name = tag === 'input' ? cut(el.value || el.getAttribute('aria-label')) : cut(el.innerText || el.getAttribute('aria-label'));
    // 最後の手段のCSS。IDと name を優先し、なければ親からの位置で指す。
    const css = [];
    if (el.id && !/\d{4,}/.test(el.id)) css.push('#' + CSS.escape(el.id));
    if (el.getAttribute('name')) css.push(`${tag}[name="${CSS.escape(el.getAttribute('name'))}"]`);
    const path = []; for (let e = el; e && e.nodeType === 1 && path.length < 5; e = e.parentElement) {
      const same = e.parentElement ? [...e.parentElement.children].filter(x => x.tagName === e.tagName) : [e];
      path.unshift(e.tagName.toLowerCase() + (same.length > 1 ? `:nth-of-type(${same.indexOf(e) + 1})` : ''));
    }
    css.push(path.join(' > '));
    return {tag, type, label, role, name, css, hint: cut(el.getAttribute('name') || el.id || el.innerText, 24)};
  };
  document.addEventListener('click', event => {
    const el = event.target.closest?.('button, a[href], input[type=submit], input[type=button], input[type=reset], input[type=checkbox], input[type=radio], summary, [role=button], [role=link]');
    if (el) window.__eelRecord({kind: 'click', element: describe(el)});
  }, true);
  // 入力は、打つたびに届ける。確定（change）はフォーカスが外れるまで届かないことがあり、
  // それを待つと、後から行った操作と順番が入れ替わるためである。
  const typed = event => {
    const el = event.target;
    if (!el.matches?.('input, textarea, select') || ['checkbox', 'radio', 'submit', 'button', 'file'].includes(el.type)) return;
    const secret = el.type === 'password';
    window.__eelRecord({kind: el.tagName === 'SELECT' ? 'select' : 'fill', element: describe(el), secret, value: secret ? null : el.value});
  };
  document.addEventListener('input', typed, true);
  document.addEventListener('change', typed, true);
}

/**
 * ページの操作を記録する。`stop()` を呼ぶと、記録した手順と、一意に指せなかった操作の一覧を返す。
 * @param {import('playwright').BrowserContext} context
 * @param {{origins: string[], secrets?: Record<string, string>, testValue?: string, runId: string}} options
 */
export async function record(context, options) {
  const steps = [], problems = [], pending = new Set(), secrets = Object.entries(options.secrets || {}).filter(([, v]) => typeof v === 'string' && v.length);
  const counted = async locator => { try { return await locator.count(); } catch { return 0; } };
  // 親から順に、iframeをそのフレームの中で一意に指すセレクタを作る。
  async function chain(frame) {
    const selectors = [];
    for (let f = frame; f.parentFrame(); f = f.parentFrame()) {
      const element = await f.frameElement(), parent = f.parentFrame();
      const candidates = await element.evaluate(e => {
        const list = []; const name = e.getAttribute('name'), id = e.getAttribute('id');
        if (name) list.push(`iframe[name="${CSS.escape(name)}"]`);
        if (id) list.push(`iframe#${CSS.escape(id)}`);
        try { const p = new URL(e.getAttribute('src') || '', location.href).pathname.split('/').pop(); if (p) list.push(`iframe[src*="${CSS.escape(p)}"]`); } catch { /* srcのないiframe */ }
        return list;
      });
      let chosen = null;
      for (const selector of candidates) if (await counted(parent.locator(selector)) === 1) { chosen = selector; break; }
      if (!chosen) return null;
      selectors.unshift(chosen);
    }
    return selectors;
  }
  async function locatorFor(frame, element) {
    if (element.label && await counted(frame.getByLabel(element.label, {exact: true})) === 1) return {by: 'label', text: element.label};
    // 役割は、レシピの検査が受け付けるものだけを使う。
    if (ROLES.includes(element.role) && element.name && await counted(frame.getByRole(element.role, {name: element.name, exact: true})) === 1) return {by: 'role', role: element.role, name: element.name};
    for (const selector of element.css) if (await counted(frame.locator(selector)) === 1) return {by: 'css', selector};
    return null;
  }
  async function handle(frame, event) {
    let origin = ''; try { origin = new URL(frame.url()).origin; } catch { /* about:blank など */ }
    if (!options.origins.includes(origin)) return;
    const frames = await chain(frame), locator = frames && await locatorFor(frame, event.element);
    const id = `r${String(steps.length + problems.length + 1).padStart(2, '0')}-${event.kind}${slug(event.element.hint) ? '-' + slug(event.element.hint) : ''}`;
    if (!locator) { problems.push({id, kind: event.kind, reason: frames ? '要素を一意に指す指定を作れなかった' : 'iframeを一意に指す指定を作れなかった'}); return; }
    const step = {id, action: event.kind, target: {frames, locator}};
    if (event.kind === 'fill') {
      // パスワード欄と、秘密値ファイルの値は、値を残さず参照にする。試験値として入力した文字は置き換える。
      const known = event.secret ? null : secrets.find(([, v]) => v === event.value);
      if (event.secret) step.value = {source: 'secret', key: slug(event.element.hint) || 'password'};
      else if (known) step.value = {source: 'secret', key: known[0]};
      else step.value = {source: 'literal', value: options.testValue ? event.value.replaceAll(options.testValue, '{{test_value}}') : event.value};
    }
    if (event.kind === 'select') step.value = event.value;
    // 同じ欄への続けての入力は、1つの手順にまとめる。最後の値が、その欄に入れた文字になる。
    if (['fill', 'select'].includes(step.action)) {
      const same = other => other.action === step.action && JSON.stringify(other.target) === JSON.stringify(step.target);
      const last = steps.at(-1), earlier = steps.findLast(same);
      if (last && same(last)) { last.value = step.value; return; }
      // フォーカスが外れたときに遅れて届く確定は、すでに記録した値と同じなので、手順を増やさない。
      if (earlier && JSON.stringify(earlier.value) === JSON.stringify(step.value)) return;
    }
    steps.push(step);
  }
  await context.exposeBinding('__eelRecord', (source, event) => {
    // 記録は届いた順に1件ずつ処理し、手順の順番を操作の順番と同じにする。
    const previous = [...pending].at(-1) || Promise.resolve();
    const task = previous.then(() => handle(source.frame, event)).catch(error => { problems.push({id: 'r-error', kind: event?.kind, reason: String(error.message).slice(0, 200)}); });
    pending.add(task); task.finally(() => pending.delete(task));
  });
  await context.addInitScript(capture);
  return {
    async stop() {
      await Promise.all([...pending]);
      const basis = {run_id: options.runId, rules: RULES, source: 'operation_recording'};
      return {schema_version: 1, rules: RULES, actions: steps, provenance: {steps: Object.fromEntries(steps.map(s => [s.id, basis]))}, problems};
    },
  };
}

// 画面を表示するブラウザで記録する。対象は、試験環境で起動済みのものを指定する。
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  const args = Object.fromEntries(process.argv.slice(2).reduce((list, arg, i, all) => arg.startsWith('--') ? [...list, [arg.slice(2), all[i + 1]]] : list, []));
  if (!args.origin || !args.out) { console.error('使い方: node tools/browser-recorder.mjs --origin http://127.0.0.1:PORT --out NEW_FILE [--start-path /] [--secrets FILE] [--test-value TEXT]'); process.exit(2); }
  const {chromium} = await import('playwright');
  const secrets = args.secrets ? JSON.parse(await fs.readFile(args.secrets, 'utf8')) : {};
  const browser = await chromium.launch({channel: 'chromium', headless: false});
  const context = await browser.newContext({ignoreHTTPSErrors: args.origin.startsWith('https:')});
  const recorder = await record(context, {origins: [new URL(args.origin).origin], secrets, testValue: args['test-value'], runId: 'recording-' + Date.now()});
  const page = await context.newPage();
  await page.goto(new URL(args['start-path'] || '/', args.origin).href);
  console.error('操作を記録しています。終わったらブラウザのウィンドウを閉じてください。');
  await new Promise(resolve => page.once('close', resolve));
  const result = await recorder.stop();
  await fs.writeFile(args.out, JSON.stringify(result, null, 2), {flag: 'wx', mode: 0o600});
  await browser.close();
  console.error(`手順${result.actions.length}件を記録した。一意に指せなかった操作は${result.problems.length}件。`);
}
