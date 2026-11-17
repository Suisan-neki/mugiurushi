// 実際の mugiurushi-ai とモデルで、操作を出発点にコードをたどる往復（#230 の段階4）を回し、結果を記録する。
// 画面は使わず、画面のAPI（POST /api/v1/agent/investigate）を、範囲の確認 → 了承の順に呼ぶ。
// 必要なもの: GitHubへのgit接続、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR）と uv、
// 運営者が設定するモデル名（MUGIURUSHI_AI_MODEL）・認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// 対象と操作は引数のJSONファイルで渡す（例: examples/investigate-live/openelis.json）。
// 送るのは、公開リポジトリの固定したコミットのコードのうち、AIが求めた範囲だけである。実患者の情報・認証情報・ログは送らない。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, readFile, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';

const root = resolve(import.meta.dirname, '..');
const agentDir = process.env.MUGIURUSHI_AI_DIR;
assert(agentDir, 'MUGIURUSHI_AI_DIR（mugiurushi-ai のチェックアウト）を指定してください');
assert(process.env.MUGIURUSHI_AI_MODEL, 'MUGIURUSHI_AI_MODEL を指定してください');
const planFile = process.argv[2];
assert(planFile, '対象と操作のJSONファイルを指定してください');
const plan = JSON.parse(await readFile(planFile, 'utf8'));
const only = process.argv[3] ? Number(process.argv[3]) : null;
const binary = process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi');
const out = join(root, 'test-results');
await mkdir(out, { recursive: true });

const freePort = () => new Promise((ok, no) => { const s = createServer(); s.listen(0, '127.0.0.1', () => { const { port } = /** @type {any} */ (s.address()); s.close(() => ok(port)); }); s.on('error', no); });
const waitFor = (child, pattern, label) => new Promise((ok, no) => {
  let text = ''; const timer = setTimeout(() => no(Error(`${label} が起動しない: ${text}`)), 60000);
  const take = d => { text += d; const m = pattern.exec(text); if (m) { clearTimeout(timer); ok(m); } };
  child.stdout.on('data', take); child.stderr.on('data', take);
  child.once('exit', code => { clearTimeout(timer); no(Error(`${label} が終了した（${code}）: ${text}`)); });
});

const temp = await mkdtemp(join(tmpdir(), 'eel-investigate-live-'));
const children = [];
try {
  const token = randomBytes(24).toString('hex'), tokenFile = join(temp, 'agent-token');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const port = await freePort();
  const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(port)], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(agent);
  const [, agentUrl] = await waitFor(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
  const commits = { mugiurushi: execFileSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(), agent: execFileSync('git', ['-C', agentDir, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() };
  // AIの認証は、麦漆本体の環境から外す。
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  const data = join(temp, 'app');
  const app = spawn(binary, ['app', data, '--port', '0'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const [, url] = await waitFor(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, '麦漆');
  const page = await (await fetch(url + '/flow')).text();
  const review = /name="review-token" content="([^"]+)"/.exec(page)?.[1];
  assert(review, '操作のトークンを画面から読めない');
  const headers = { 'X-Review-Token': review, 'Content-Type': 'application/json' };
  // 画面と同じく、長い処理は受付と待ち合わせで待つ。
  const post = async body => {
    let response = await fetch(url + '/api/v1/agent/investigate', { method: 'POST', headers: { ...headers, Prefer: 'respond-async' }, body: JSON.stringify(body) });
    let json = await response.json();
    if (response.status === 202 && json.kind === 'request') {
      while (json.status === 'queued' || json.status === 'running') {
        await new Promise(r => setTimeout(r, 2000));
        json = await (await fetch(url + '/api/v1/jobs/' + json.id, { headers })).json();
      }
      assert.equal(json.status, 'completed', JSON.stringify(json));
      if (json.response_status >= 400) throw Error(json.result?.error || `HTTP ${json.response_status}`);
      return json.result;
    }
    if (!response.ok) throw Error(json.error || `HTTP ${response.status}`);
    return json;
  };

  for (const [index, operation] of plan.operations.entries()) {
    if (only !== null && only !== index + 1) continue;
    const body = { repository: plan.repository, commit: plan.commit, operation: operation.operation, summary: plan.summary || '', exclusions: plan.exclusions || [], rounds: operation.rounds || plan.rounds || 12 };
    const started = Date.now();
    const preview = await post(body);
    assert.equal(preview.stage, 'preview');
    console.log(`範囲 ${index + 1}: ${preview.scope.operation}（SHA-256 ${preview.scope_sha256}）`);
    let result, error = null;
    try { result = await post({ ...body, confirm: { scope_sha256: preview.scope_sha256 } }); }
    catch (e) { error = e instanceof Error ? e.message : String(e); }
    const seconds = Math.round((Date.now() - started) / 1000);
    // 往復の記録（送った資料の中身は持たない）も写す。
    // 往復の記録は事例の置き場所の investigations/ にある。いちばん新しいものを読む。
    let saved = null;
    const files = (await readdir(data, { recursive: true })).filter(p => /(^|\/)investigations\/[^/]+\.json$/.test(p)).sort((a, b) => a.split('/').pop().localeCompare(b.split('/').pop()));
    if (files.length) saved = JSON.parse(await readFile(join(data, files[files.length - 1]), 'utf8'));
    const record = { at: new Date().toISOString(), model: process.env.MUGIURUSHI_AI_MODEL, commits, seconds, scope: preview.scope, scope_sha256: preview.scope_sha256, result: result || null, error, record: saved };
    const file = join(out, `investigate-live-${index + 1}.json`);
    await writeFile(file, JSON.stringify(record, null, 2) + '\n');
    const usage = result?.usage || saved?.usage;
    console.log(error ? `失敗: ${error}` : `終了: ${result.stopped_by}、往復${result.rounds}回、懸念${result.concerns.length}件`);
    console.log(`時間: ${seconds}秒、トークン: ${JSON.stringify(usage)}、記録: ${file}`);
  }
} finally {
  for (const child of children) child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
}
