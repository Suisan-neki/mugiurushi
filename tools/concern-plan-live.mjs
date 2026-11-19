// 実際の mugiurushi-ai とモデルで、コードをたどって出た懸念から試験の案を作り、結果を記録する（#230）。
// 往復の調べはたどり直さず、tools/investigate-live.mjs が書いた結果（test-results/investigate-live-<番号>.json）を使う。
// 画面と同じく、確かめ方が「麦漆で動かす」の懸念を、要求の文「その懸念が起きていないこと」の候補にし、根拠の行を添える。
// 送る前に、人が送る内容を確かめる。1回目は、送る内容の要約とSHA-256を示すだけで、何も送らずに終わる。
// 内容を確かめた人が、そのSHA-256を EEL_APPROVED_PLAN に入れて実行し直したときだけ送る。
// 必要なもの: 対象のコミットを含むgitのリポジトリ（EEL_SOURCE_GIT。根拠のファイルを読んで麦漆の読取記録を作る）、
// mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR）と uv、モデル名（MUGIURUSHI_AI_MODEL）と認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// 送るのは公開リポジトリのコードの根拠の行と懸念だけで、実患者の情報・認証情報・ログは送らない。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, basename } from 'node:path';
import { createServer } from 'node:net';
import { concernPlanCandidate, planEvidence } from '../ui/flow-record.js';

const root = resolve(import.meta.dirname, '..');
const agentDir = process.env.MUGIURUSHI_AI_DIR;
assert(agentDir, 'MUGIURUSHI_AI_DIR（mugiurushi-ai のチェックアウト）を指定してください');
assert(process.env.MUGIURUSHI_AI_MODEL, 'MUGIURUSHI_AI_MODEL を指定してください');
const sourceGit = process.env.EEL_SOURCE_GIT;
assert(sourceGit, 'EEL_SOURCE_GIT（対象のコミットを含むgitのリポジトリ）を指定してください');
const input = process.argv[2];
assert(input, 'tools/investigate-live.mjs が書いた結果のファイルを指定してください');
const saved = JSON.parse(await readFile(input, 'utf8'));
const { repository, commit, summary } = saved.scope;
const concerns = (saved.result?.concerns || []).filter(c => c.check?.by === 'run');
assert(concerns.length, '確かめ方が「麦漆で動かす」の懸念がありません');
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

const temp = await mkdtemp(join(tmpdir(), 'eel-concern-plan-live-'));
const children = [];
try {
  // 麦漆の読取記録を、根拠のファイルだけで作る（この環境ではGitHubのAPIで大きなリポジトリを読めないため）。
  const paths = [...new Set(concerns.flatMap(c => c.evidence.map(e => e.path)))];
  const files = Object.fromEntries(paths.map(p => [p, execFileSync('git', ['-C', sourceGit, 'show', `${commit}:${p}`], { encoding: 'utf8', maxBuffer: 16 << 20 })]));
  const data = join(temp, 'app');
  await mkdir(join(data, 'snapshots', repository), { recursive: true });
  await writeFile(join(data, 'snapshots', repository, commit + '.json'), JSON.stringify({ schema: 1, fullName: repository, reference: commit, commit, inventory: paths.map(path => ({ path, mode: '100644' })), files, omitted: [] }));

  const token = randomBytes(24).toString('hex'), tokenFile = join(temp, 'agent-token');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const port = await freePort();
  const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(port)], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(agent);
  const [, agentUrl] = await waitFor(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  const app = spawn(binary, ['app', data, '--port', '0'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const [, url] = await waitFor(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, '麦漆');
  const review = /name="review-token" content="([^"]+)"/.exec(await (await fetch(url + '/flow')).text())?.[1];
  assert(review, '操作のトークンを画面から読めない');
  const headers = { 'X-Review-Token': review, 'Content-Type': 'application/json' };
  const post = async body => {
    let response = await fetch(url + '/api/v1/agent/plan', { method: 'POST', headers: { ...headers, Prefer: 'respond-async' }, body: JSON.stringify(body) });
    let json = await response.json();
    if (response.status === 202 && json.kind === 'request') {
      while (json.status === 'queued' || json.status === 'running') {
        await new Promise(r => setTimeout(r, 1000));
        json = await (await fetch(url + '/api/v1/jobs/' + json.id, { headers })).json();
      }
      if (json.response_status >= 400) throw Error(json.result?.error || `HTTP ${json.response_status}`);
      return json.result;
    }
    if (!response.ok) throw Error(json.error || `HTTP ${response.status}`);
    return json;
  };

  // 画面と同じ組み立て（ui/flow-record.js）。照合をしていないので、読んだファイルはなく、根拠の行だけを添える。
  const body = { repository, commit, files: [], summary: summary || '', answers: [], findings: concerns.map(c => concernPlanCandidate(c, commit)), evidence: planEvidence(concerns) };
  const preview = await post(body);
  console.log(`送り先：${preview.destination}、${preview.bytes}バイト`);
  console.log(`候補：${preview.payload.findings.map(f => f.id).join('、')}`);
  console.log(`根拠の抜粋：${preview.payload.evidence.map(e => e.ref).join('、')}`);
  console.log(`SHA-256 ${preview.payload_sha256}\n${preview.note}`);
  await writeFile(join(out, 'concern-plan-live-preview.json'), JSON.stringify(preview, null, 2) + '\n');
  if (process.env.EEL_APPROVED_PLAN !== preview.payload_sha256) {
    console.log(`了承されていないので送らない。送る内容（test-results/concern-plan-live-preview.json）を確かめて了承する場合は、EEL_APPROVED_PLAN=${preview.payload_sha256} を付けて実行し直す。`);
  } else {
    const started = Date.now();
    const plan = await post({ ...body, confirm: { payload_sha256: preview.payload_sha256, synthetic_only: true } });
    const record = { at: new Date().toISOString(), model: process.env.MUGIURUSHI_AI_MODEL, investigation: basename(input), seconds: Math.round((Date.now() - started) / 1000), concerns, payload_sha256: preview.payload_sha256, plan };
    const file = join(out, 'concern-plan-live.json');
    await writeFile(file, JSON.stringify(record, null, 2) + '\n');
    console.log(`試験の案 ${plan.tests.length}件、試験にしなかった候補 ${plan.not_planned.length}件、未分類 ${plan.unclassified.length}件。記録: ${file}`);
    console.log(`モデルの呼び出し: ${JSON.stringify(plan.model_call)}`);
  }
} finally {
  for (const child of children) child.kill('SIGTERM');
  await rm(temp, { recursive: true, force: true });
}
