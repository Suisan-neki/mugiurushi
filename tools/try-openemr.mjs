// 実際の AIエージェント（mugiurushi-ai）と mugiurushi を起動し、実OpenEMR（同梱レシピの固定版）を対象に、
// 新しい流れ（/flow）を人が画面で操作できる状態にする。操作はすべて人が行う：
//   読むファイルの選択と説明 → 照合とヒアリング → 試験の案の了承 → 設定画面で同梱のOpenEMR試験を選ぶ →
//   照合の候補を判定条件に対応づける → 実行 → /flow の結果 → 修正用プロンプト。
// このスクリプトは操作を自動では行わない。終了（Ctrl+C）時に、事例と流れの記録を出力先に残し、要約（summary.json）を書く。
// 合否は麦漆の判定をそのまま残し、このスクリプトでは決めない。
//
// 必要なもの（起動前に確かめ、足りなければ理由を出して止める）:
//   - root でない利用者（試験用ブラウザのOSサンドボックスのため。--no-sandbox は使わない）
//   - Docker と、レシピが固定したイメージ（ダイジェスト一致）
//   - レシピが求める版の Chromium（EEL_BROWSER_EXECUTABLE）と、試験用アカウントのファイル（EEL_BROWSER_SECRETS_FILE）
//   - mugiurushi-ai のチェックアウト（MUGIURUSHI_AI_DIR、文書を取り込み済み）、モデル名（MUGIURUSHI_AI_MODEL）と
//     認証（OPENAI_API_KEY か MUGIURUSHI_AI_AUTH=proxy）
//   - GitHub への接続（固定コミットの読み取り）
// 送るのは OpenEMR の公開コードの抜粋と、画面で人が書く説明だけ。実患者の情報・生のログ・認証情報は入れない。
//
// 使い方: node tools/try-openemr.mjs [--snapshot-from-git] [出力先（既定 evaluations/openemr-flow-<時刻>）]
//   --snapshot-from-git: GitHubのAPIを使えない環境のための代わり。固定コミットを git で浅く取得し、ファイルの一覧
//   （パス・モード・大きさ）だけの読取記録を作る。ファイルの中身は、画面で選んだときに raw.githubusercontent.com から読む。
//   GitHubのAPIで作る読取記録と違い、作った方法を要約（summary.json）に残す。
import assert from 'node:assert/strict';
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import { mkdir, writeFile, readFile, access } from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';

const root = resolve(import.meta.dirname, '..');
const binary = process.env.REVIEW_BINARY || join(root, 'target/debug/mugiurushi');
const recipe = JSON.parse(await readFile(join(root, 'examples/openemr-browser/soap-recipe.json'), 'utf8'));
const { repository, commit } = recipe.source;
const args = process.argv.slice(2), fromGit = args.includes('--snapshot-from-git');
const output = resolve(args.find(a => !a.startsWith('--')) || join(root, 'evaluations', `openemr-flow-${new Date().toISOString().replace(/[:.]/g, '-')}`));
const appPort = Number(process.env.PORT || 0);

// ---------- 起動前の確認 ----------
const problems = [];
if (process.getuid?.() === 0) problems.push('root で実行しています。試験用ブラウザはOSサンドボックス付きで起動するため、root でない利用者で実行してください。');
const docker = spawnSync('docker', ['info', '--format', '{{.ServerVersion}}'], { encoding: 'utf8' });
if (docker.status !== 0) problems.push(`Docker に接続できません（デーモンが起動しているか、この利用者が docker グループに入っているか確かめてください）: ${(docker.stderr || '').trim().split('\n')[0]}`);
else for (const service of recipe.containers.services) {
  const found = spawnSync('docker', ['image', 'inspect', service.image], { encoding: 'utf8' });
  if (found.status !== 0) problems.push(`レシピが固定したイメージが手元にありません: ${service.image}（docker pull で取得してください）`);
}
const executable = process.env.EEL_BROWSER_EXECUTABLE, expected = recipe.browser.expected_version;
if (!executable) problems.push(`EEL_BROWSER_EXECUTABLE に Chromium ${expected} の実行ファイルを指定してください。`);
else {
  const version = spawnSync(executable, ['--version'], { encoding: 'utf8' }).stdout?.trim() || '';
  if (!version.includes(expected)) problems.push(`試験用ブラウザの版がレシピと違います（必要 ${expected}、指定 ${version || '読み取れない'}）。`);
}
if (!process.env.EEL_BROWSER_SECRETS_FILE) problems.push('EEL_BROWSER_SECRETS_FILE に試験用アカウントのファイルを指定してください（公開された架空の値。実環境に流用しない）。');
else await access(process.env.EEL_BROWSER_SECRETS_FILE).catch(() => problems.push(`試験用アカウントのファイルを読めません: ${process.env.EEL_BROWSER_SECRETS_FILE}`));
const agentDir = process.env.MUGIURUSHI_AI_DIR;
if (!agentDir) problems.push('MUGIURUSHI_AI_DIR に mugiurushi-ai のチェックアウトを指定してください。');
if (!process.env.MUGIURUSHI_AI_MODEL) problems.push('MUGIURUSHI_AI_MODEL にモデル名を指定してください。');
if (!process.env.OPENAI_API_KEY && process.env.MUGIURUSHI_AI_AUTH !== 'proxy') problems.push('OPENAI_API_KEY を指定するか、MUGIURUSHI_AI_AUTH=proxy を指定してください。');
await access(binary).catch(() => problems.push(`mugiurushi の実行ファイルがありません: ${binary}（cargo build --locked）`));
if (problems.length) { console.error('起動できません:\n' + problems.map(p => `- ${p}`).join('\n')); process.exit(2); }

// ---------- 起動 ----------
const data = join(output, 'app');
await mkdir(data, { recursive: true });
let snapshotMethod = 'GitHubのAPI（画面で開いたときに麦漆が作る）';
if (fromGit) {
  // 固定コミットだけを浅く取得し、ファイルの一覧から読取記録を作る。中身は入れない（画面で選んだときに読む）。
  const source = join(output, 'source');
  await mkdir(source, { recursive: true });
  const git = (...a) => execFileSync('git', ['-C', source, ...a], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, env: { ...process.env, GIT_LFS_SKIP_SMUDGE: '1' } });
  if (!(await access(join(source, '.git')).then(() => true, () => false))) { git('init', '-q'); git('remote', 'add', 'origin', `https://github.com/${repository}`); }
  git('fetch', '-q', '--depth', '1', 'origin', commit);
  const inventory = git('ls-tree', '-r', '-l', commit).split('\n').filter(Boolean).flatMap(line => {
    const [meta, path] = line.split('\t'); const [mode, type, , size] = meta.split(/\s+/);
    return type === 'blob' ? [{ path, mode, size: size === '-' ? null : Number(size) }] : [];
  });
  await mkdir(join(data, 'snapshots', repository), { recursive: true });
  await writeFile(join(data, 'snapshots', repository, commit + '.json'), JSON.stringify({ schema: 1, fullName: repository, reference: commit, commit, inventory, files: {}, omitted: [] }));
  snapshotMethod = `git で取得した固定コミットのファイル一覧（${inventory.length}件。中身は画面で選んだときに raw.githubusercontent.com から読む）`;
  console.log(`読取記録を git から作りました: ${snapshotMethod}`);
}
const children = [];
const started = (child, pattern, label) => new Promise((ok, no) => {
  let out = '', err = ''; const timer = setTimeout(() => no(Error(`${label} の起動待ちが上限に達した: ${err.slice(-1500)}`)), 120000);
  const look = () => { const m = pattern.exec(out + err); if (m) { clearTimeout(timer); ok(m[1]); } };
  child.stdout.on('data', d => { out += d; look(); }); child.stderr.on('data', d => { err += d; look(); });
  child.once('exit', code => { clearTimeout(timer); no(Error(`${label} が終了した（${code}）: ${err.slice(-1500)}`)); });
});
const token = randomBytes(24).toString('hex'), tokenFile = join(output, '.agent-token');
await writeFile(tokenFile, token, { mode: 0o600 });
const port = await new Promise(ok => { const s = createServer().listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => ok(p)); }); });
const agent = spawn('uv', ['run', '--directory', agentDir, 'mugiurushi-ai', 'serve', '--port', String(port)], { env: { ...process.env, MUGIURUSHI_AI_TOKEN: token }, stdio: ['ignore', 'pipe', 'pipe'] });
children.push(agent);
let url, reviewToken, stopping = false;
const stop = async code => {
  if (stopping) return; stopping = true;
  try { if (url && reviewToken) await collect(); } catch (e) { console.error(`記録の書き出しに失敗しました: ${e.message}`); code = code || 1; }
  for (const c of children) c.kill('SIGTERM');
  process.exit(code);
};
process.on('SIGINT', () => stop(0)); process.on('SIGTERM', () => stop(0));

// 終了時の記録。事例と流れの記録そのものは出力先の app/ に残る。要約には、照合・案の応答ID、了承した試験、
// 人が対応づけた判定条件、試行と判定を写す。合否は麦漆の値をそのまま写す。
async function collect() {
  const headers = { 'X-Review-Token': reviewToken };
  const [view, flow] = await Promise.all([fetch(url + '/api/v1/case', { headers }).then(r => r.json()), fetch(url + '/api/v1/flow', { headers }).then(r => r.json())]);
  const record = flow.records?.[`${repository}@${commit}`]?.record || null;
  const flowChecks = new Set(Object.values(record?.approved || {}).map(a => a?.check_id).filter(Boolean));
  const summary = {
    note: '人が /flow と設定画面で操作した記録。合否は麦漆の判定をそのまま写した。操作者・選んだ範囲・理由は、この要約とは別に記録すること。',
    recipe: recipe.source, images: recipe.containers.services.map(s => s.image), browser_expected_version: recipe.browser.expected_version,
    model: process.env.MUGIURUSHI_AI_MODEL, snapshot: snapshotMethod,
    scan: record?.scan ? { response_id: record.scan.model_call?.response_id || null, files: record.files, findings: record.scan.findings.map(f => ({ id: f.id, requirement: f.requirement, method: f.verification?.method, executable_check: f.verification?.executable_check })) } : null,
    plan: record?.plan ? { response_id: record.plan.model_call?.response_id || null, tests: record.plan.tests.map(t => ({ id: t.id, name: t.name, finding_ids: t.finding_ids, observation: t.observation })), not_planned: record.plan.not_planned, unclassified: record.plan.unclassified ?? null } : null,
    checks: view.workspace.checks.filter(c => flowChecks.has(c.id)).map(c => ({ id: c.id, source: c.source,
      aspects: c.aspects.map(a => ({ id: a.id, observation: a.observation, trial_checks: a.trial_checks.map(l => l.id) })) })),
    trials: view.workspace.evidence.filter(e => flowChecks.has(e.check_id)).map(e => ({ evidence_id: e.id, check_id: e.check_id, run_id: e.manifest?.correlation?.run_id, source: e.execution?.recipe?.source,
      results: e.execution?.trial?.results?.map(r => ({ id: r.id, observed: r.observed })) })),
    coverage: view.checks.filter(c => flowChecks.has(c.check_id)).map(c => ({ check_id: c.check_id, verdict: c.verdict, aspects: c.coverage?.aspects?.map(a => ({ id: a.id, status: a.status, status_label: a.status_label })) })),
    history: (record?.history || []).length,
  };
  await writeFile(join(output, 'summary.json'), JSON.stringify(summary, null, 2));
  console.log(`記録を残しました: ${join(output, 'summary.json')}（事例と流れの記録は ${data}）`);
}

try {
  const agentUrl = await started(agent, /Uvicorn running on (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi-ai');
  const appEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(OPENAI_|MUGIURUSHI_AI_)/.test(k)));
  // 同梱の試験の一覧は、EEL_RECIPE_DIR で指定したディレクトリから読む。指定しないと、設定画面に同梱のOpenEMR試験が出ない。
  const app = spawn(binary, ['app', data, '--port', String(appPort)], { env: { ...appEnv, EEL_AGENT_URL: agentUrl, EEL_AGENT_TOKEN_FILE: tokenFile, EEL_RECIPE_DIR: join(root, 'examples') }, stdio: ['ignore', 'pipe', 'pipe'] });
  children.push(app);
  url = await started(app, /理解と試験の画面: (http:\/\/127\.0\.0\.1:\d+)/, 'mugiurushi');
  reviewToken = /name="review-token" content="([^"]+)"/.exec(await (await fetch(url + '/flow')).text())?.[1];
  assert(reviewToken, '画面から確認用の識別子を読めない');
  const flowUrl = new URL(url + '/flow'); flowUrl.search = new URLSearchParams({ url: 'https://github.com/' + repository, ref: commit }).toString();
  console.log(`\n準備ができました。ブラウザで次を開いてください（終了は Ctrl+C。終了時に記録を残します）:\n\n  ${flowUrl.href}\n`);
  console.log(`対象は ${repository} の ${commit}（同梱のOpenEMR試験と同じ版）。照合したコードと試験したコードを同じ版にするため、この版で開いています。`);
  console.log('同梱の試験は、架空の患者でSOAPを保存・再表示する操作に固定されています。どの試験を選ぶか、照合のどの候補にどの判定条件が答えるかは、人が決めます。');
  console.log(`Gitの参照版と、実行するイメージ（${recipe.containers.services.map(s => s.image).join('、')}）のコードが同一かは確認していません。\n`);
  for (const c of children) c.once('exit', code => { if (!stopping) { console.error(`停止しました（${code}）`); stop(1); } });
} catch (e) {
  console.error(e.message);
  await stop(1);
}
