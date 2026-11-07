// 架空の診療所アプリ（examples/clinic-tasks）を対象に、mugiurushi-ai と mugiurushi を起動して、照合の画面を開ける状態にする。
// 審査や試用で、人が画面を操作して照合 → 試験の案 → 了承 → 試験の設定と実行 → 結果 → 修正用プロンプトを確かめるためのもの。
// 画面の操作は行わない（自動で通す場合は tools/clinic-demo.mjs）。
// 必要なもの: Python 3、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR、文書を取り込み済み）、
// MUGIURUSHI_AI_MODEL と認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）、cargo build 済みの mugiurushi。
// 送るのは架空のアプリのコードと、画面で利用者が書く説明だけ。実患者の情報・生のログ・認証情報は入れない。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';

const root = resolve(import.meta.dirname, '..');
const binary = process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi');
const agentDir = process.env.MUGIURUSHI_AI_DIR;
assert(agentDir, 'MUGIURUSHI_AI_DIR（mugiurushi-ai のチェックアウト）を指定してください');
assert(process.env.MUGIURUSHI_AI_MODEL, 'MUGIURUSHI_AI_MODEL を指定してください');
assert(process.env.OPENAI_API_KEY || process.env.MUGIURUSHI_AI_AUTH === 'proxy', 'OPENAI_API_KEY を指定してください');
const REPO = 'example/clinic-tasks';
const appPort = Number(process.env.PORT || 8080);
const temp = await mkdtemp(join(tmpdir(), 'mugiurushi-try-clinic-'));
const children = [];
const stop = async code => { for (const c of children) c.kill('SIGTERM'); await rm(temp, { recursive: true, force: true }); process.exit(code); };
process.on('SIGINT', () => stop(0)); process.on('SIGTERM', () => stop(0));
const started = (child, pattern, label) => new Promise((ok, no) => {
  let out = '', err = ''; const timer = setTimeout(() => no(Error(`${label} の起動待ちが上限に達した: ${err.slice(-1500)}`)), 120000);
  const look = () => { const m = pattern.exec(out + err); if (m) { clearTimeout(timer); ok(m[1]); } };
  child.stdout.on('data', d => { out += d; look(); }); child.stderr.on('data', d => { err += d; look(); });
  child.once('exit', code => { clearTimeout(timer); no(Error(`${label} が終了した（${code}）: ${err.slice(-1500)}`)); });
});

try {
  // 1. 架空のリポジトリを手元に作り、試験用の取得先（管理用ディレクトリ）に置く。GitHubからは取得しない。
  const repo = join(temp, 'clinic-tasks');
  await mkdir(repo, { recursive: true });
  const git = (...a) => execFileSync('git', ['-C', repo, ...a], { encoding: 'utf8' }).trim();
  git('init', '-q'); git('config', 'user.name', '架空の開発者'); git('config', 'user.email', 'demo@example.invalid');
  git('remote', 'add', 'origin', `https://github.com/${REPO}.git`);
  for (const f of ['clinic_tasks.py', 'README.md']) await writeFile(join(repo, f), await readFile(join(root, 'examples/clinic-tasks', f), 'utf8'));
  git('add', '.'); git('commit', '-qm', '架空の診療所の連絡タスク（最初の版）');
  const commit = git('rev-parse', 'HEAD');
  const data = join(temp, 'app');
  const files = {}, inventory = [];
  for (const path of git('ls-files').split('\n')) { const text = await readFile(join(repo, path), 'utf8'); files[path] = text; inventory.push({ path, mode: '100644', size: Buffer.byteLength(text) }); }
  await mkdir(join(data, 'snapshots', REPO), { recursive: true });
  await writeFile(join(data, 'snapshots', REPO, commit + '.json'), JSON.stringify({ schema: 1, fullName: REPO, reference: commit, commit, inventory, files, omitted: [] }));
  await mkdir(join(data, 'repositories', 'example'), { recursive: true });
  execFileSync('git', ['clone', '-q', '--bare', repo, join(data, 'repositories', REPO)]);
  execFileSync('git', ['-C', join(data, 'repositories', REPO), 'remote', 'set-url', 'origin', `https://github.com/${REPO}.git`]);

  // 2. mugiurushi-ai と mugiurushi（local方式で実行）を起動する。共有トークンは毎回作り、画面には出さない。
  const token = randomBytes(24).toString('hex'), tokenFile = join(temp, 'agent-token');
  await writeFile(tokenFile, token, { mode: 0o600 });
  const port = await new Promise(ok => { const s = createServer().listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => ok(p)); }); });
  const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(port)], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(agent);
  const agentUrl = await started(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  const app = spawn(binary, ['app', data, '--port', String(appPort), '--execution-mode', 'local'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  const url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi');
  const flowUrl = new URL(url + '/flow'); flowUrl.search = new URLSearchParams({ url: 'https://github.com/' + REPO, ref: commit }).toString();
  console.log(`\n準備ができました。ブラウザで次を開いてください（終了は Ctrl+C）:\n\n  ${flowUrl.href}\n`);
  console.log('説明の欄の例: 架空の診療所で、職員が患者ごとの連絡タスク（架空の患者ID・内容）を追加し、一覧で読み直す。職員はそれぞれ担当する患者のタスクだけを扱う。\n');
  for (const c of children) c.once('exit', code => { console.error(`停止しました（${code}）`); stop(1); });
} catch (e) {
  console.error(e.message);
  await stop(1);
}
