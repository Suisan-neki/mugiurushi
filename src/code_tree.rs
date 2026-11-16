//! 固定したコミットのコードを手元に浅く取得し、語で検索し、行の範囲で読む（#230 の段階1）。
//!
//! 照合で、AIエージェントが求めた検索と読み取りに、麦漆が答えるために使う。
//! - 取得するのは固定したコミットだけ（`git fetch --depth 1`）。作業ツリーは作らず、bare のリポジトリから読む。
//!   ファイルを展開しないので、対象のシンボリックリンクや実行の権限は手元に現れない。
//! - 送らないパス（秘密を含みうるファイル）は、検索の結果にも、ファイルの一覧にも、読み取りにも出さない。
//!   gitの除外の指定（pathspec）にも渡すが、出すかどうかはこのモジュールの照合で決める。
//! - 読めるのは通常のファイルだけ。シンボリックリンク、サブモジュール、バイナリ、大きすぎるファイルは読まない。
//! - 返す量に上限を置く。上限で打ち切ったときは、結果にそう示す。打ち切った結果を、全部だとは扱わない。
use crate::error::invalid;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// 1回の読み取りで返す行の数の上限。
pub const MAX_READ_LINES: usize = 400;
/// 1回の読み取りで返す文字の数の上限。長い行のファイルでは、行の上限より先にこちらに達する。
pub const MAX_READ_CHARS: usize = 40_000;
/// 1回の検索で返す、当たった行の数の上限。
pub const MAX_HITS: usize = 50;
/// 検索の結果の1行（当たった行と、前後の行）の文字の数の上限。
pub const MAX_LINE_CHARS: usize = 300;
/// 検索の語の文字の数の上限。
pub const MAX_QUERY_CHARS: usize = 200;
/// 読めるファイルの大きさの上限。
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// 扱うコミットのファイルの数の上限。超えるリポジトリは扱わない。
pub const MAX_TREE_ENTRIES: usize = 200_000;
/// 利用者が足せる、送らないパスの指定の数と長さの上限。
pub const MAX_EXTRA_EXCLUSIONS: usize = 32;
pub const MAX_PATTERN_CHARS: usize = 200;

/// 既定で送らないパス。秘密の値を持つことが多い種類のファイルだけを挙げる。
/// 名前に「secret」を含むコードのファイル（例：秘密の値を扱う処理）は、処理を読むために必要なので含めない。
pub const DEFAULT_EXCLUSIONS: [&str; 21] = [
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.jks",
    "*.keystore",
    "*.kdbx",
    "id_rsa",
    "id_rsa.*",
    "id_dsa",
    "id_dsa.*",
    "id_ecdsa",
    "id_ecdsa.*",
    "id_ed25519",
    "id_ed25519.*",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".git-credentials",
];

const FETCH_TIMEOUT: Duration = Duration::from_secs(600);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// 検索の結果の1件（gitが出す1行）として読み込む大きさの上限。長い行は、ここで切る。
const MAX_RECORD_BYTES: usize = 64 * 1024;
/// gitの出力として受け取る大きさの上限（検索以外）。
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

static PARTIAL: AtomicU64 = AtomicU64::new(0);

/// 検索で当たった行（パス、行番号、行）と、上限で打ち切ったか。
type Found = (Vec<(String, usize, String)>, bool);

/// 送らないパスの指定。既定の指定に、利用者の指定を足したもの。
///
/// 指定の書き方は次のとおり。`/` を含まない指定は、どの階層でもファイルの名前と照らす。
/// `/` を含む指定は、リポジトリの最上位からのパスと照らす。`*` は `/` を含まない任意の文字の並び、
/// `**/` は0個以上のディレクトリ、それ以外の `**` は任意の文字の並び、`?` は `/` 以外の1文字。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusions {
    patterns: Vec<String>,
}

impl Exclusions {
    /// 既定の指定に、利用者の指定を足す。
    pub fn new(extra: &[String]) -> io::Result<Self> {
        if extra.len() > MAX_EXTRA_EXCLUSIONS {
            return Err(invalid(format!(
                "送らないパスの指定は{MAX_EXTRA_EXCLUSIONS}件までにしてください"
            )));
        }
        let mut patterns: Vec<String> =
            DEFAULT_EXCLUSIONS.iter().map(|p| (*p).to_owned()).collect();
        for pattern in extra {
            validate_pattern(pattern)?;
            if !patterns.contains(pattern) {
                patterns.push(pattern.clone());
            }
        }
        Ok(Self { patterns })
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// このパスを送らないか。
    pub fn excludes(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.patterns.iter().any(|pattern| {
            if pattern.contains('/') {
                glob(pattern, path)
            } else {
                glob(pattern, name)
            }
        })
    }

    /// gitの検索へ渡す除外の指定。除外したファイルで、検索の結果の上限を使い切らないために渡す。
    fn pathspecs(&self) -> Vec<String> {
        self.patterns
            .iter()
            .map(|pattern| {
                if pattern.contains('/') {
                    format!(":(exclude,glob){pattern}")
                } else {
                    format!(":(exclude,glob)**/{pattern}")
                }
            })
            .collect()
    }
}

fn validate_pattern(pattern: &str) -> io::Result<()> {
    let bad = pattern.trim().is_empty()
        || pattern.chars().count() > MAX_PATTERN_CHARS
        || pattern.starts_with(':')
        || pattern.starts_with('/')
        || pattern
            .chars()
            .any(|c| c.is_control() || matches!(c, '[' | ']' | '\\'));
    if bad {
        return Err(invalid(format!(
            "送らないパスの指定 {pattern:?} は使えません。最上位からの相対パスで、`*`、`**`、`?` だけを使ってください"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Char(char),
    /// `?`
    One,
    /// `*`
    Star,
    /// `**/`。空か、`/` で終わる任意の文字の並び。
    DirStar,
    /// `**`（`/` が続かないもの）
    AnyStar,
}

fn tokens(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    out.push(Token::DirStar);
                    i += 3;
                } else {
                    out.push(Token::AnyStar);
                    i += 2;
                }
            }
            '*' => {
                out.push(Token::Star);
                i += 1;
            }
            '?' => {
                out.push(Token::One);
                i += 1;
            }
            c => {
                out.push(Token::Char(c));
                i += 1;
            }
        }
    }
    out
}

/// 指定と文字の並びの全体が合うか。動的計画法で、指定の長さと文字の並びの長さの積に比例する時間で決める。
fn glob(pattern: &str, text: &str) -> bool {
    let p = tokens(pattern);
    let t: Vec<char> = text.chars().collect();
    // ok[i][j]：指定の i 番目以降が、文字の並びの j 番目以降の全体と合う。
    let mut ok = vec![vec![false; t.len() + 1]; p.len() + 1];
    ok[p.len()][t.len()] = true;
    for i in (0..p.len()).rev() {
        for j in (0..=t.len()).rev() {
            ok[i][j] = match p[i] {
                Token::Char(c) => j < t.len() && t[j] == c && ok[i + 1][j + 1],
                Token::One => j < t.len() && t[j] != '/' && ok[i + 1][j + 1],
                Token::Star => ok[i + 1][j] || (j < t.len() && t[j] != '/' && ok[i][j + 1]),
                Token::AnyStar => ok[i + 1][j] || (j < t.len() && ok[i][j + 1]),
                Token::DirStar => {
                    ok[i + 1][j] || (j..t.len()).any(|k| t[k] == '/' && ok[i + 1][k + 1])
                }
            };
        }
    }
    ok[0][0]
}

/// 検索の結果の1件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub text: String,
    /// 前の行。最初の行のとき、またはファイルが大きすぎて読まないときは持たない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// 次の行。最後の行のとき、またはファイルが大きすぎて読まないときは持たない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// 検索の結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub query: String,
    pub ignore_case: bool,
    pub hits: Vec<Hit>,
    /// 上限で打ち切った。当たった行は、`hits` のほかにもある。
    pub truncated: bool,
}

/// 読み取りの結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    pub path: String,
    /// ファイルの内容のgitのオブジェクトID（SHA-1）。どの内容を読んだかを、後から確かめるために持つ。
    pub blob: String,
    pub total_lines: usize,
    /// 返した最初の行と最後の行（1から数える）。
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// 文字の数の上限で、求めた範囲の途中で打ち切った。
    pub truncated: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    blob: String,
    size: u64,
}

/// 手元に取得した、固定したコミットのコード。
pub struct CodeTree {
    git_dir: PathBuf,
    repository: String,
    commit: String,
    exclusions: Exclusions,
    /// 送ってよい通常のファイル。パスからオブジェクトIDと大きさ。
    files: BTreeMap<String, Entry>,
}

impl CodeTree {
    /// GitHubの公開リポジトリの固定したコミットを、`base` の下に取得して開く。取得済みなら、取得し直さない。
    pub fn open(
        base: &Path,
        repository: &str,
        commit: &str,
        exclusions: Exclusions,
    ) -> io::Result<Self> {
        validate_source(repository, commit)?;
        let url = format!("https://github.com/{repository}.git");
        Self::open_from(base, repository, commit, &url, exclusions)
    }

    /// 取得元のURL（https か file）を指定して開く。試験と、GitHub以外から取得する場合に使う。
    pub fn open_from(
        base: &Path,
        repository: &str,
        commit: &str,
        url: &str,
        exclusions: Exclusions,
    ) -> io::Result<Self> {
        validate_source(repository, commit)?;
        let (owner, name) = repository.split_once('/').unwrap_or_default();
        let parent = base.join(owner).join(name);
        let git_dir = parent.join(format!("{commit}.git"));
        if !has_commit(&git_dir, commit) {
            fs::create_dir_all(&parent)?;
            let partial = parent.join(format!(
                "{commit}.git.partial-{}-{}",
                std::process::id(),
                PARTIAL.fetch_add(1, Ordering::Relaxed)
            ));
            let fetched = fetch(&partial, url, commit);
            match fetched {
                Ok(()) => {
                    // 同時に取得した別の処理が先に置いた場合は、そちらを使う。
                    if fs::rename(&partial, &git_dir).is_err() {
                        let _ = fs::remove_dir_all(&partial);
                        if !has_commit(&git_dir, commit) {
                            return Err(io::Error::other(
                                "取得したコミットを置き場所へ移せませんでした",
                            ));
                        }
                    }
                }
                Err(error) => {
                    let _ = fs::remove_dir_all(&partial);
                    return Err(error);
                }
            }
        }
        let files = list_files(&git_dir, commit, &exclusions)?;
        Ok(Self {
            git_dir,
            repository: repository.to_owned(),
            commit: commit.to_owned(),
            exclusions,
            files,
        })
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }

    pub fn exclusions(&self) -> &Exclusions {
        &self.exclusions
    }

    /// 送ってよい通常のファイルのパス。名前順。
    pub fn files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    /// 語をそのままの文字の並びとして検索する（正規表現としては扱わない）。
    pub fn search(&self, query: &str, ignore_case: bool) -> io::Result<SearchResult> {
        if query.is_empty()
            || query.chars().count() > MAX_QUERY_CHARS
            || query.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "検索の語は、改行などの制御文字を含まない1〜{MAX_QUERY_CHARS}文字にしてください"
            )));
        }
        let mut args: Vec<String> = ["grep", "-z", "-n", "-I", "-F", "--no-color"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        if ignore_case {
            args.push("-i".into());
        }
        args.extend([
            "-e".into(),
            query.to_owned(),
            self.commit.clone(),
            "--".into(),
        ]);
        args.extend(self.exclusions.pathspecs());
        let (raw, more) = grep(&self.git_dir, &args, &self.commit, &self.files)?;
        let mut hits: Vec<Hit> = raw
            .into_iter()
            .map(|(path, line, text)| Hit {
                path,
                line,
                text: clip(&text, MAX_LINE_CHARS),
                before: None,
                after: None,
            })
            .collect();
        self.add_context(&mut hits)?;
        Ok(SearchResult {
            query: query.to_owned(),
            ignore_case,
            hits,
            truncated: more,
        })
    }

    /// 当たった行の前後の行を足す。大きすぎるファイルは読まない。
    fn add_context(&self, hits: &mut [Hit]) -> io::Result<()> {
        let mut texts: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for hit in hits.iter() {
            if texts.contains_key(&hit.path) {
                continue;
            }
            let entry = &self.files[&hit.path];
            if entry.size > MAX_FILE_BYTES {
                continue;
            }
            let Ok(text) = self.blob_text(&entry.blob) else {
                continue;
            };
            texts.insert(hit.path.clone(), text.lines().map(str::to_owned).collect());
        }
        for hit in hits.iter_mut() {
            if let Some(lines) = texts.get(&hit.path) {
                if hit.line >= 2 {
                    hit.before = lines.get(hit.line - 2).map(|l| clip(l, MAX_LINE_CHARS));
                }
                hit.after = lines.get(hit.line).map(|l| clip(l, MAX_LINE_CHARS));
            }
        }
        Ok(())
    }

    /// 1から数えた行の範囲を読む。最後の行がファイルの行の数を超える場合は、最後の行までにする。
    pub fn read(&self, path: &str, start: usize, end: usize) -> io::Result<ReadResult> {
        if self.exclusions.excludes(path) {
            return Err(invalid(format!(
                "{path} は送らないパスに当たるので読みません"
            )));
        }
        let entry = self.files.get(path).ok_or_else(|| {
            invalid(format!(
                "{path} は、このコミットの通常のファイルではありません"
            ))
        })?;
        if start == 0 || end < start || end - start + 1 > MAX_READ_LINES {
            return Err(invalid(format!(
                "読む範囲は、1以上の最初の行から、{MAX_READ_LINES}行までにしてください"
            )));
        }
        if entry.size > MAX_FILE_BYTES {
            return Err(invalid(format!(
                "{path} は{}KiBを超えるので読みません",
                MAX_FILE_BYTES / 1024
            )));
        }
        let text = self.blob_text(&entry.blob).map_err(|_| {
            invalid(format!(
                "{path} はUTF-8のテキストではないので読みません（バイナリなど）"
            ))
        })?;
        let lines: Vec<&str> = text.lines().collect();
        if start > lines.len() {
            return Err(invalid(format!(
                "{path} は{}行です。最初の行が行の数を超えています",
                lines.len()
            )));
        }
        let last = end.min(lines.len());
        let mut out = String::new();
        let mut used = 0;
        let mut returned = start - 1;
        let mut truncated = false;
        for line in &lines[start - 1..last] {
            let count = line.chars().count() + usize::from(!out.is_empty());
            if used + count > MAX_READ_CHARS {
                truncated = true;
                // 1行目だけで上限を超える場合は、その行の先頭だけを返す。
                if out.is_empty() {
                    out = clip(line, MAX_READ_CHARS);
                    returned += 1;
                }
                break;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
            used += count;
            returned += 1;
        }
        Ok(ReadResult {
            path: path.to_owned(),
            blob: entry.blob.clone(),
            total_lines: lines.len(),
            start,
            end: returned,
            text: out,
            truncated,
        })
    }

    /// オブジェクトの内容を、UTF-8のテキストとして読む。NULを含むものはバイナリとして扱う。
    fn blob_text(&self, blob: &str) -> io::Result<String> {
        let output = run(
            &self.git_dir,
            &["cat-file", "blob", blob],
            COMMAND_TIMEOUT,
            MAX_FILE_BYTES as usize + 1,
        )?;
        if output.contains(&0) {
            return Err(invalid("バイナリのファイルです"));
        }
        String::from_utf8(output).map_err(|_| invalid("UTF-8のテキストではありません"))
    }
}

/// リポジトリ（owner/name）とコミット（40桁の16進数）の形を確かめる。
pub fn validate_source(repository: &str, commit: &str) -> io::Result<()> {
    let part = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    let good_repository = repository
        .split_once('/')
        .is_some_and(|(owner, name)| part(owner) && part(name) && !name.contains('/'));
    if !good_repository {
        return Err(invalid("リポジトリは owner/name の形で指定してください"));
    }
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "コミットは40桁の16進数（小文字）で指定してください",
        ));
    }
    Ok(())
}

/// 手元のリポジトリの中のgitの操作。利用者のgitの設定とフックを使わない。
fn local(git_dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "--no-replace-objects",
            "-c",
            "core.hooksPath=/dev/null",
            "--git-dir",
        ])
        .arg(git_dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

fn has_commit(git_dir: &Path, commit: &str) -> bool {
    git_dir.is_dir()
        && run(
            git_dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{commit}^{{commit}}"),
            ],
            COMMAND_TIMEOUT,
            1024,
        )
        .is_ok_and(|out| String::from_utf8_lossy(&out).trim() == commit)
}

/// 固定したコミットだけを取得する。取得の通信には、利用者のgitの設定（プロキシなど）を使う。
/// 取得先のURLの方式だけを許し、ほかの方式への転送には従わない。
fn fetch(git_dir: &Path, url: &str, commit: &str) -> io::Result<()> {
    let scheme = url.split_once("://").map(|(s, _)| s).unwrap_or_default();
    if !matches!(scheme, "https" | "file") {
        return Err(invalid("取得先のURLは https か file にしてください"));
    }
    fs::create_dir_all(git_dir)?;
    run(
        git_dir,
        &["init", "--bare", "-q"],
        COMMAND_TIMEOUT,
        64 * 1024,
    )?;
    let mut command = Command::new("git");
    command
        .args(["--no-replace-objects", "-c", "core.hooksPath=/dev/null"])
        .args(["-c", "protocol.allow=never", "-c"])
        .arg(format!("protocol.{scheme}.allow=always"))
        .arg("--git-dir")
        .arg(git_dir)
        .args([
            "fetch",
            "-q",
            "--depth",
            "1",
            "--no-tags",
            "--no-write-fetch-head",
            url,
            commit,
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    finish(command, FETCH_TIMEOUT, 64 * 1024).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("コミット {commit} を取得できませんでした: {error}"),
        )
    })?;
    if !has_commit(git_dir, commit) {
        return Err(io::Error::other(format!(
            "取得した中に、コミット {commit} がありません"
        )));
    }
    Ok(())
}

fn run(git_dir: &Path, args: &[&str], timeout: Duration, max_out: usize) -> io::Result<Vec<u8>> {
    finish(local(git_dir, args), timeout, max_out)
}

/// コマンドを実行し、終了コード0なら標準出力を返す。期限を過ぎたら、子のグループごと止めてエラーにする。
fn finish(mut command: Command, timeout: Duration, max_out: usize) -> io::Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::process::ChildGuard::spawn(&mut command)?;
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let reader = thread::spawn(move || read_capped(out, max_out));
    let errors = thread::spawn(move || read_capped(err, 8 * 1024));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            // 戻ると子のグループを止める（ChildGuard の破棄）。
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "gitの処理が時間内に終わりませんでした",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (stdout, over) = reader.join().unwrap_or_default();
    let (stderr, _) = errors.join().unwrap_or_default();
    if !status.success() {
        return Err(io::Error::other(format!(
            "git: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    if over {
        return Err(io::Error::other("gitの出力が上限を超えました"));
    }
    Ok(stdout)
}

/// 上限までを読み、残りは読み捨てる。上限を超えたかを返す。
fn read_capped(mut pipe: impl Read, max: usize) -> (Vec<u8>, bool) {
    let mut kept = vec![];
    let mut buffer = [0u8; 16 * 1024];
    let mut over = false;
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..n.min(room)]);
                over |= n > room;
            }
        }
    }
    (kept, over)
}

/// コミットの通常のファイルのうち、送ってよいものを一覧にする。
fn list_files(
    git_dir: &Path,
    commit: &str,
    exclusions: &Exclusions,
) -> io::Result<BTreeMap<String, Entry>> {
    let output = run(
        git_dir,
        &["ls-tree", "-r", "-z", "-l", "--full-tree", commit],
        COMMAND_TIMEOUT,
        MAX_OUTPUT_BYTES,
    )?;
    let mut files = BTreeMap::new();
    for (count, record) in output
        .split(|b| *b == 0)
        .filter(|r| !r.is_empty())
        .enumerate()
    {
        if count >= MAX_TREE_ENTRIES {
            return Err(invalid(format!(
                "ファイルが{MAX_TREE_ENTRIES}件を超えるコミットは扱いません"
            )));
        }
        // 形：<mode> SP <type> SP <object> SP* <size> TAB <path>
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            continue;
        };
        let meta = String::from_utf8_lossy(&record[..tab]);
        let Ok(path) = std::str::from_utf8(&record[tab + 1..]) else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        let [mode, kind, object, size] = fields[..] else {
            continue;
        };
        // 通常のファイルだけを扱う。シンボリックリンク（120000）とサブモジュール（160000）は読まない。
        if kind != "blob" || !matches!(mode, "100644" | "100755") || exclusions.excludes(path) {
            continue;
        }
        let Ok(size) = size.parse::<u64>() else {
            continue;
        };
        files.insert(
            path.to_owned(),
            Entry {
                blob: object.to_owned(),
                size,
            },
        );
    }
    Ok(files)
}

/// gitの検索を実行し、当たった行を上限まで受け取る。送ってよい通常のファイルの行だけを残す。
/// 上限を超えた時点で検索を止める。上限を超えたか（ほかにも当たった行があるか）を返す。
fn grep(
    git_dir: &Path,
    args: &[String],
    commit: &str,
    allowed: &BTreeMap<String, Entry>,
) -> io::Result<Found> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut command = local(git_dir, &args);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = crate::process::ChildGuard::spawn(&mut command)?;
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let errors = thread::spawn(move || read_capped(err, 8 * 1024));
    let prefix = format!("{commit}:");
    let allowed: Arc<Vec<String>> = Arc::new(allowed.keys().cloned().collect());
    let found: Arc<Mutex<Found>> = Arc::new(Mutex::new((vec![], false)));
    let shared = found.clone();
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(out);
        while let Some(record) = next_record(&mut reader) {
            let Some(hit) = parse_hit(&record, &prefix) else {
                continue;
            };
            if allowed.binary_search(&hit.0).is_err() {
                continue;
            }
            let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
            if state.0.len() >= MAX_HITS {
                state.1 = true;
                break;
            }
            state.0.push(hit);
        }
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if reader.is_finished() {
            // 上限に達して読むのをやめた場合は、検索を止める。
            if found.lock().unwrap_or_else(|p| p.into_inner()).1 {
                let _ = crate::process::kill_group(&mut child);
                let _ = child.wait();
                break None;
            }
        }
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "検索が時間内に終わりませんでした",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let _ = reader.join();
    let (stderr, _) = errors.join().unwrap_or_default();
    // 終了コード1は「当たった行がない」。
    if let Some(status) = status
        && !status.success()
        && status.code() != Some(1)
    {
        return Err(io::Error::other(format!(
            "git grep: {}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    let state = found.lock().unwrap_or_else(|p| p.into_inner());
    Ok((state.0.clone(), state.1))
}

/// 1件（改行で終わる1行）を読む。長すぎる行は先頭だけを残し、残りは読み捨てる。
fn next_record(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut record = vec![];
    let mut any = false;
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(_) => return any.then_some(record),
        };
        if available.is_empty() {
            return any.then_some(record);
        }
        any = true;
        if let Some(at) = available.iter().position(|b| *b == b'\n') {
            let room = MAX_RECORD_BYTES.saturating_sub(record.len());
            record.extend_from_slice(&available[..at.min(room)]);
            reader.consume(at + 1);
            return Some(record);
        }
        let n = available.len();
        let room = MAX_RECORD_BYTES.saturating_sub(record.len());
        record.extend_from_slice(&available[..n.min(room)]);
        reader.consume(n);
    }
}

/// `-z` を付けたgitの検索の1件：`<コミット>:<パス>\0<行番号>\0<行>`。
fn parse_hit(record: &[u8], prefix: &str) -> Option<(String, usize, String)> {
    let first = record.iter().position(|b| *b == 0)?;
    let name = std::str::from_utf8(&record[..first]).ok()?;
    let path = name.strip_prefix(prefix)?;
    let rest = &record[first + 1..];
    let second = rest.iter().position(|b| *b == 0)?;
    let line = std::str::from_utf8(&rest[..second]).ok()?.parse().ok()?;
    let text = String::from_utf8_lossy(&rest[second + 1..]).into_owned();
    Some((path.to_owned(), line, text))
}

/// 文字の数で切る。切った場合は、末尾に「…」を付ける。
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    struct Fixture {
        root: PathBuf,
        url: String,
        commit: String,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// 架空のリポジトリ。送ってよいファイル、送らないファイル、読まない種類のファイルを含む。
    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "mugiurushi-code-tree-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        fs::create_dir_all(source.join("app/conf")).unwrap();
        fs::create_dir_all(source.join("deploy/secrets")).unwrap();
        let write = |path: &str, bytes: &[u8]| fs::write(source.join(path), bytes).unwrap();
        write(
            "app/patient.py",
            b"def save(patient):\n    log(patient.national_id)\n    db.insert('patient', patient)\n",
        );
        write("app/conf/.env", b"PATIENT_DB_PASSWORD=synthetic\n");
        write("app/conf/server.pem", b"patient synthetic key\n");
        write(
            "deploy/secrets/patient.json",
            b"{\"patient\": \"synthetic\"}\n",
        );
        write("app/logo.bin", b"patient\0binary\n");
        write("we:ird name.txt", b"a.*b patient\na literal\n");
        write("app/many.txt", "patient\n".repeat(MAX_HITS + 10).as_bytes());
        write(
            "app/long.txt",
            format!("{}\n", "x".repeat(200)).repeat(400).as_bytes(),
        );
        write("app/big.txt", &vec![b'a'; MAX_FILE_BYTES as usize + 10]);
        std::os::unix::fs::symlink("patient.py", source.join("app/link.py")).unwrap();
        git(&source, &["init", "-q"]);
        git(&source, &["add", "-A"]);
        git(
            &source,
            &[
                "-c",
                "user.name=Synthetic",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "synthetic",
            ],
        );
        let commit = git(&source, &["rev-parse", "HEAD"]);
        Fixture {
            url: format!("file://{}", source.display()),
            root,
            commit,
        }
    }

    fn open(f: &Fixture, extra: &[&str]) -> CodeTree {
        let extra: Vec<String> = extra.iter().map(|e| (*e).to_owned()).collect();
        CodeTree::open_from(
            &f.root.join("checkouts"),
            "example/clinic",
            &f.commit,
            &f.url,
            Exclusions::new(&extra).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn patterns_match_names_at_any_depth_and_paths_from_the_top() {
        let e = Exclusions::new(&["deploy/secrets/**".into(), "**/fixtures/*.sql".into()]).unwrap();
        for path in [
            ".env",
            "app/conf/.env",
            "app/.env.local",
            "keys/server.pem",
            "home/id_rsa",
            "deploy/secrets/a/b.json",
            "fixtures/seed.sql",
            "db/fixtures/seed.sql",
        ] {
            assert!(e.excludes(path), "{path} を送らない");
        }
        for path in [
            "app/env.py",
            "app/secret_manager.py",
            "app/keys.py",
            "other/deploy/secrets/a.json",
            "db/fixtures/deep/seed.sql",
            "app/.environment",
        ] {
            assert!(!e.excludes(path), "{path} は送ってよい");
        }
        for bad in [":(top)x", "/abs", "a[bc]", "a\\b", "", "a\nb"] {
            assert!(
                Exclusions::new(&[bad.into()]).is_err(),
                "{bad:?} を受け付けない"
            );
        }
        let many: Vec<String> = (0..=MAX_EXTRA_EXCLUSIONS)
            .map(|i| format!("x{i}"))
            .collect();
        assert!(Exclusions::new(&many).is_err());
    }

    #[test]
    fn only_regular_files_outside_the_exclusions_are_listed() {
        let f = fixture("list");
        let tree = open(&f, &["deploy/secrets/**"]);
        let files = tree.files();
        for path in [
            "app/patient.py",
            "we:ird name.txt",
            "app/logo.bin",
            "app/big.txt",
        ] {
            assert!(files.contains(&path.to_owned()), "{path} がない: {files:?}");
        }
        for path in [
            "app/conf/.env",
            "app/conf/server.pem",
            "deploy/secrets/patient.json",
            "app/link.py",
        ] {
            assert!(!files.contains(&path.to_owned()), "{path} が一覧にある");
        }
    }

    #[test]
    fn search_returns_lines_with_context_and_never_from_excluded_or_unreadable_files() {
        let f = fixture("search");
        let tree = open(&f, &["deploy/secrets/**"]);
        let result = tree.search("national_id", false).unwrap();
        assert!(!result.truncated);
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!((hit.path.as_str(), hit.line), ("app/patient.py", 2));
        assert_eq!(hit.text, "    log(patient.national_id)");
        assert_eq!(hit.before.as_deref(), Some("def save(patient):"));
        assert_eq!(
            hit.after.as_deref(),
            Some("    db.insert('patient', patient)")
        );
        // 「patient」は送らないファイル、バイナリ、シンボリックリンクにもあるが、結果には出ない。
        let all = tree.search("PATIENT", true).unwrap();
        for hit in &all.hits {
            assert!(
                !matches!(
                    hit.path.as_str(),
                    "app/conf/.env"
                        | "app/conf/server.pem"
                        | "deploy/secrets/patient.json"
                        | "app/logo.bin"
                        | "app/link.py"
                ),
                "{} が結果に出た",
                hit.path
            );
        }
        // 上限で打ち切り、打ち切ったことを示す。
        assert_eq!(all.hits.len(), MAX_HITS);
        assert!(all.truncated);
        // 大文字と小文字を区別する検索では当たらない。
        assert!(tree.search("PATIENT", false).unwrap().hits.is_empty());
        // 語は、正規表現ではなく文字どおりに探す。パスに「:」を含むファイルも扱える。
        let literal = tree.search("a.*b", false).unwrap();
        assert_eq!(literal.hits.len(), 1);
        assert_eq!(
            (literal.hits[0].path.as_str(), literal.hits[0].line),
            ("we:ird name.txt", 1)
        );
        assert!(tree.search("-e", false).unwrap().hits.is_empty());
        for bad in ["", "a\nb", &"x".repeat(MAX_QUERY_CHARS + 1)] {
            assert!(tree.search(bad, false).is_err(), "{bad:?} を受け付けない");
        }
    }

    #[test]
    fn search_results_are_limited_to_sendable_files_even_without_the_git_exclusions() {
        // 送るかどうかは、gitの除外の指定ではなく、このモジュールの照合で決める。
        // gitへ除外の指定を渡さない検索でも、送らないファイルの行は結果に出ない。
        let f = fixture("filter");
        let tree = open(&f, &["deploy/secrets/**"]);
        let args: Vec<String> = [
            "grep",
            "-z",
            "-n",
            "-I",
            "-F",
            "-e",
            "synthetic",
            &f.commit,
            "--",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
        let (hits, more) = grep(&tree.git_dir, &args, &f.commit, &tree.files).unwrap();
        assert!(!more);
        assert!(hits.is_empty(), "送らないファイルの行が出た: {hits:?}");
        // シンボリックリンクの中身（リンク先のパス）は、検索の結果に出ない。
        assert!(tree.search("patient.py", false).unwrap().hits.is_empty());
    }

    #[test]
    fn read_returns_the_range_with_the_blob_and_refuses_what_must_not_be_sent() {
        let f = fixture("read");
        let tree = open(&f, &["deploy/secrets/**"]);
        let read = tree.read("app/patient.py", 2, 10).unwrap();
        assert_eq!((read.start, read.end, read.total_lines), (2, 3, 3));
        assert_eq!(
            read.text,
            "    log(patient.national_id)\n    db.insert('patient', patient)"
        );
        assert_eq!(read.blob.len(), 40);
        assert!(!read.truncated);
        // 文字の数の上限で、範囲の途中で打ち切る。
        let long = tree.read("app/long.txt", 1, MAX_READ_LINES).unwrap();
        assert!(long.truncated && long.end < MAX_READ_LINES);
        assert!(long.text.chars().count() <= MAX_READ_CHARS);
        for (path, start, end) in [
            ("app/conf/.env", 1, 1),
            ("deploy/secrets/patient.json", 1, 1),
            ("app/conf/server.pem", 1, 1),
            ("app/link.py", 1, 1),
            ("app/logo.bin", 1, 1),
            ("app/big.txt", 1, 1),
            ("app", 1, 1),
            ("app/../app/patient.py", 1, 1),
            ("missing.py", 1, 1),
            ("app/patient.py", 0, 1),
            ("app/patient.py", 3, 2),
            ("app/patient.py", 4, 5),
            ("app/patient.py", 1, MAX_READ_LINES + 1),
        ] {
            assert!(
                tree.read(path, start, end).is_err(),
                "{path} {start}-{end} を読まない"
            );
        }
    }

    #[test]
    fn a_fetched_commit_is_reused_and_a_failed_fetch_leaves_nothing() {
        let f = fixture("reuse");
        let first = open(&f, &[]);
        // 取得元を消しても、取得済みのものを開ける。
        fs::remove_dir_all(f.root.join("source")).unwrap();
        let again = open(&f, &[]);
        assert_eq!(first.files(), again.files());
        let base = f.root.join("checkouts");
        let missing = CodeTree::open_from(
            &base,
            "example/other",
            &"0".repeat(40),
            &f.url,
            Exclusions::new(&[]).unwrap(),
        );
        assert!(missing.is_err());
        let left: Vec<_> = fs::read_dir(base.join("example/other")).unwrap().collect();
        assert!(left.is_empty(), "取得に失敗した残りがある");
        for (repository, commit) in [
            ("example", "0".repeat(40)),
            ("../x/y", "0".repeat(40)),
            ("example/..", "0".repeat(40)),
            ("example/clinic", "ABC".to_owned()),
            ("example/clinic", "A".repeat(40)),
        ] {
            assert!(
                CodeTree::open(&base, repository, &commit, Exclusions::new(&[]).unwrap()).is_err(),
                "{repository} {commit}"
            );
        }
        assert!(
            CodeTree::open_from(
                &base,
                "example/clinic",
                &f.commit,
                "ssh://example.invalid/x",
                Exclusions::new(&[]).unwrap()
            )
            .is_ok(),
            "取得済みなら、URLの方式によらず開ける"
        );
    }

    #[test]
    fn other_url_schemes_are_refused_before_fetching() {
        let base = std::env::temp_dir().join(format!(
            "mugiurushi-code-tree-scheme-{}",
            std::process::id()
        ));
        let result = CodeTree::open_from(
            &base,
            "example/clinic",
            &"1".repeat(40),
            "ext::sh -c touch% /tmp/x",
            Exclusions::new(&[]).unwrap(),
        );
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&base);
    }
}
