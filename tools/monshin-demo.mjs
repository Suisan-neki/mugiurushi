// 架空の問診アプリ（examples/monshin）で、照合から試験の自動実行・修正用プロンプトまでの流れ（/flow）を見せるための準備。
// 架空のリポジトリ example/monshin（ブランチ main と fix/answer-access）を手元に作り、mugiurushi-ai と mugiurushi を起動して、
// 画面のURLを表示する。画面の操作は人が行う。Ctrl+C で止めると、作った一時ディレクトリを消す。
// GitHubには公開しない。ブランチの一覧・読み取り・試験用の取得は、手元に作ったリポジトリから行う。
// 必要なもの: Python 3、mugiurushi のビルド（target/debug/mugiurushi）、mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR、文書を取り込み済み）、
// MUGIURUSHI_AI_MODEL と認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）。
// PORT を指定すると、mugiurushi をそのポートで起動する（既定は空いているポート）。
// MONSHIN_DEMO_DIR を指定すると、架空のリポジトリとデータの保存先をそのディレクトリに作り、止めても消さない。
// 同じディレクトリを指定して起動し直すと、作り直さずに続きから使う（コミットが変わらないので、照合・試験の案・結果の記録が残る）。
// 会場のネットワークやAIの応答が遅いときに備えて、前もって試験の案まで進めた記録から続けるために使う。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm, stat } from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';

const root = resolve(import.meta.dirname, '..');
const binary = process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi');
const agentDir = process.env.MUGIURUSHI_AI_DIR;
assert(agentDir, 'MUGIURUSHI_AI_DIR（mugiurushi-ai のチェックアウト）を指定してください');
assert(process.env.MUGIURUSHI_AI_MODEL, 'MUGIURUSHI_AI_MODEL を指定してください');
const REPO = 'example/monshin';
const kept = process.env.MONSHIN_DEMO_DIR ? resolve(process.env.MONSHIN_DEMO_DIR) : null;
const temp = kept || await mkdtemp(join(tmpdir(), 'eel-monshin-demo-'));
if (kept) await mkdir(kept, { recursive: true });
const children = [];
const stop = async () => { for (const c of children) c.kill('SIGTERM'); if (!kept) await rm(temp, { recursive: true, force: true }); process.exit(0); };
process.on('SIGINT', stop); process.on('SIGTERM', stop);
const started = (child, pattern, label) => new Promise((ok, no) => {
  let out = '', err = ''; const timer = setTimeout(() => no(Error(`${label} の起動待ちが上限に達した: ${err.slice(-1500)}`)), 60000);
  const look = () => { const m = pattern.exec(out + err); if (m) { clearTimeout(timer); ok(m[1]); } };
  child.stdout.on('data', d => { out += d; look(); }); child.stderr.on('data', d => { err += d; look(); process.stderr.write(d); });
  child.once('exit', code => { clearTimeout(timer); no(Error(`${label} が終了した（${code}）: ${err.slice(-1500)}`)); });
});

// 1. 架空のリポジトリを作る。main は受付の職員も回答を読める版、fix/answer-access は医師だけが読める版。
// 指定したディレクトリに前回のデータがあれば、作り直さない（コミットと記録をそのまま使う）。
const data = join(temp, 'app');
const resumed = !!kept && await stat(join(data, 'repositories', REPO)).then(() => true, () => false);
if (!resumed) {
  const repo = join(temp, 'monshin');
  await mkdir(repo, { recursive: true });
  const git = (...a) => execFileSync('git', ['-C', repo, ...a], { encoding: 'utf8' }).trim();
  git('init', '-q', '-b', 'main'); git('config', 'user.name', '架空の開発者'); git('config', 'user.email', 'demo@example.invalid');
  const source = await readFile(join(root, 'examples/monshin/monshin.py'), 'utf8');
  await writeFile(join(repo, 'monshin.py'), source);
  await writeFile(join(repo, 'README.md'), await readFile(join(root, 'examples/monshin/README.md'), 'utf8'));
  git('add', '.'); git('commit', '-qm', '架空の診療所の問診（最初の版）');
  git('checkout', '-q', '-b', 'fix/answer-access');
  const fixed = source.replace('CAN_READ_ANSWERS = {"doctor", "reception"}', 'CAN_READ_ANSWERS = {"doctor"}');
  assert.notEqual(fixed, source, '修正版を作れませんでした');
  await writeFile(join(repo, 'monshin.py'), fixed);
  git('commit', '-qam', '問診の回答を読めるのを医師だけにする');
  git('checkout', '-q', 'main');

  // 2. 読取記録（全ファイルの本文）と、試験用の取得先（ブランチつきのbareリポジトリ）を置く。
  await mkdir(join(data, 'snapshots', REPO), { recursive: true });
  for (const branch of ['main', 'fix/answer-access']) {
    const commit = git('rev-parse', branch);
    const files = {}, inventory = [];
    for (const path of git('ls-tree', '-r', '--name-only', commit).split('\n')) {
      const text = git('show', `${commit}:${path}`) + '\n';
      files[path] = text; inventory.push({ path, mode: '100644', size: Buffer.byteLength(text) });
    }
    await writeFile(join(data, 'snapshots', REPO, commit + '.json'), JSON.stringify({ schema: 1, fullName: REPO, reference: commit, commit, inventory, files, omitted: [] }));
  }
  await mkdir(join(data, 'repositories', 'example'), { recursive: true });
  execFileSync('git', ['clone', '-q', '--bare', repo, join(data, 'repositories', REPO)]);
  execFileSync('git', ['-C', join(data, 'repositories', REPO), 'remote', 'set-url', 'origin', `https://github.com/${REPO}.git`]);
}

// 3. mugiurushi-ai と mugiurushi（local方式で実行）を起動する。
const token = randomBytes(24).toString('hex'), tokenFile = join(temp, 'agent-token');
await writeFile(tokenFile, token, { mode: 0o600 });
const free = () => new Promise(ok => { const s = createServer().listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => ok(p)); }); });
const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(await free())], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
children.push(agent);
const agentUrl = await started(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
const app = spawn(binary, ['app', data, '--port', process.env.PORT || '0', '--execution-mode', 'local'], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile }, stdio: ['ignore', 'pipe', 'pipe'] });
children.push(app);
const url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi');
console.log(`\n${kept ? `データの保存先: ${kept}（${resumed ? '前回の続き' : '新しく作成'}。止めても消しません）\n` : ''}照合の流れの画面: ${url}/flow\nリポジトリのURL: https://github.com/${REPO}\n止めるときは Ctrl+C\n`);
