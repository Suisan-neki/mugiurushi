// 人が試験環境のブラウザで行った操作を、レシピの手順（browser.Step）の形で記録する。
// 要素の指定は、ラベル、役割、CSSの順に試し、そのフレームの中で一意になった最初のものを使う。
// パスワード欄と、秘密値ファイルにある値は、値を記録せずに秘密値の参照へ置き換える。
// 記録した手順は既存の検査と実行器でそのまま使う。再実行で通らない手順を、記録の側で書き換えることはしない。
import fs from 'node:fs/promises';
import {pathToFileURL} from 'node:url';

export const RULES = 2;
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
    // ラベルは、結び付いた欄のIDで指す。文字で指すと、同じ文字を含む別のラベルと区別できないことがある。
    if (tag === 'label' && el.htmlFor && !/\d{4,}/.test(el.htmlFor)) css.push(`label[for="${CSS.escape(el.htmlFor)}"]`);
    if (el.id && !/\d{4,}/.test(el.id)) css.push('#' + CSS.escape(el.id));
    if (el.getAttribute('name')) css.push(`${tag}[name="${CSS.escape(el.getAttribute('name'))}"]`);
    const path = []; for (let e = el; e && e.nodeType === 1 && path.length < 5; e = e.parentElement) {
      const same = e.parentElement ? [...e.parentElement.children].filter(x => x.tagName === e.tagName) : [e];
      path.unshift(e.tagName.toLowerCase() + (same.length > 1 ? `:nth-of-type(${same.indexOf(e) + 1})` : ''));
    }
    css.push(path.join(' > '));
    return {tag, type, label, role, name, css, hint: cut(el.getAttribute('name') || el.id || el.innerText, 24)};
  };
  // ラベルを押すと、ブラウザは結び付いた欄へもクリックを送る。人が押したのはラベルなので、ラベルを記録し、
  // 続けて届く欄へのクリックは数えない。見た目を作り込んだチェックボックスやラジオボタンは、欄そのものを
  // 隠してラベルを押させる。欄へのクリックを記録すると、再実行でその欄を押せない。
  let viaLabel = null;
  // 押された要素。ボタンなどのほか、チェックボックスやラジオボタンに結び付いたラベル。
  const pressed = event => {
    const el = event.target.closest?.('button, a[href], input[type=submit], input[type=button], input[type=reset], input[type=checkbox], input[type=radio], summary, [role=button], [role=link]');
    if (el) return {el, label: false};
    const label = event.target.closest?.('label'), control = label?.control;
    return control && ['checkbox', 'radio'].includes(control.type) ? {el: label, label: true, control} : null;
  };
  // 押し始めた時点で、要素の特徴を届ける。クリックで画面が変わる要素（ログインのボタンなど）は、
  // クリックの後では、押す前の画面で一意だったかを調べられない。
  document.addEventListener('pointerdown', event => {
    const hit = pressed(event);
    if (hit) window.__eelRecord({kind: 'press', element: describe(hit.el)});
  }, true);
  document.addEventListener('click', event => {
    const hit = pressed(event);
    if (!hit) return;
    if (hit.label) { viaLabel = hit.control; setTimeout(() => { if (viaLabel === hit.control) viaLabel = null; }, 0); }
    else if (hit.el === viaLabel) { viaLabel = null; return; }
    window.__eelRecord({kind: 'click', element: describe(hit.el)});
  }, true);
  // 入力は、打つたびに届ける。確定（change）はフォーカスが外れるまで届かないことがあり、
  // それを待つと、後から行った操作と順番が入れ替わるためである。
  const typed = event => {
    const el = event.target;
    if (!el.matches?.('input, textarea, select') || ['checkbox', 'radio', 'submit', 'button', 'file'].includes(el.type)) return;
    const secret = el.type === 'password';
    // 確定（change）か、打った入力（input）かを届ける。遅れて届く確定だけを、記録の側で見分けるためである。
    window.__eelRecord({kind: el.tagName === 'SELECT' ? 'select' : 'fill', element: describe(el), secret, value: secret ? null : el.value, settled: event.type === 'change'});
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
  // クリックの後に対象が送った書き込みの要求（GET以外）と、その応答。人が次の操作をする前に応答が届いていれば、
  // 人はその応答を待ったとみなし、手順に待ち合わせを付ける。付けないと、再実行は応答を待たずに次の操作へ進む。
  let sequence = 0, latest = null;
  const owners = new WeakMap(), waited = new Map(), bySequence = new Map();
  context.on('request', request => {
    if (request.method() === 'GET' || latest?.kind !== 'click' || waited.has(latest.sequence)) return;
    try { if (!options.origins.includes(new URL(request.url()).origin)) return; } catch { return; }
    owners.set(request, latest.sequence);
  });
  context.on('response', response => {
    const owner = owners.get(response.request());
    if (owner === undefined || waited.has(owner) || latest?.sequence !== owner) return;
    waited.set(owner, {method: response.request().method(), path: new URL(response.url()).pathname});
  });
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
  // 押し始めた時点で調べた指定。続くクリックが同じ要素なら、これを使う。
  let prepared = null;
  async function handle(frame, event) {
    let origin = ''; try { origin = new URL(frame.url()).origin; } catch { /* about:blank など */ }
    if (!options.origins.includes(origin)) return;
    const signature = JSON.stringify([event.element.tag, event.element.css]);
    if (event.kind === 'press') {
      const frames = await chain(frame);
      prepared = {frame, signature, frames, locator: frames && await locatorFor(frame, event.element)};
      return;
    }
    const before = event.kind === 'click' && prepared?.frame === frame && prepared.signature === signature && prepared.locator ? prepared : null;
    if (event.kind === 'click') prepared = null;
    const frames = before ? before.frames : await chain(frame), locator = before ? before.locator : frames && await locatorFor(frame, event.element);
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
      // 打った入力は、前の値と同じでも記録する。別の操作で欄が空になった後に、人が同じ値を入れ直した場合である。
      if (event.settled && earlier && JSON.stringify(earlier.value) === JSON.stringify(step.value)) return;
    }
    steps.push(step);
    bySequence.set(event.sequence, step);
  }
  await context.exposeBinding('__eelRecord', (source, event) => {
    // 届いた時点で順番を決める。要求と応答を、どの操作の後に起きたかで分けるためである。
    event.sequence = ++sequence; latest = {sequence, kind: event.kind};
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
      // 待った応答は、観測先として書き出す。パスが実行のたびに変わる要求（IDを含むパスなど）は、再実行で一致しない。
      const responses = [];
      for (const [owner, wait] of [...waited].sort((a, b) => a[0] - b[0])) {
        const step = bySequence.get(owner);
        if (!step || step.action !== 'click' || wait.path.length > 512) continue;
        const id = ('w-' + step.id).slice(0, 64);
        responses.push({id, method: wait.method, path: wait.path}); step.wait_response = id;
      }
      return {schema_version: 1, rules: RULES, actions: steps, responses, provenance: {steps: Object.fromEntries(steps.map(s => [s.id, basis])), responses: Object.fromEntries(responses.map(w => [w.id, basis]))}, problems};
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
