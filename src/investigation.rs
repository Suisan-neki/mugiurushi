//! 操作を出発点に、AIエージェントとの往復でコードをたどり、懸念を受け取る（#230 の段階3）。
//!
//! - 往復を回すのは麦漆である。AIエージェント（mugiurushi-ai の `POST /v1/investigate`）は1回ずつ答え、
//!   次に求めるもの（検索・一覧・読み取り）か、結論（懸念と質問）を返す。
//! - 麦漆は、固定したコミットのコードを `code_tree` で取得し、求められたものに答える。
//! - 送る前の確認は、送るたびの内容ではなく、最初に一度、人が範囲を了承する形にする（2026-11-16に決定）。
//!   範囲は、リポジトリ、コミット、対象の操作、説明、送らないパス、往復と量の上限である。
//!   了承した範囲の外のものは送らない。1往復ごとの送信内容のSHA-256と、答えたものを記録に残す。
//! - 懸念の根拠の行は、その調べで実際にAIへ返した行に限る。AIエージェントの側でも確かめるが、ここでも確かめる。
use crate::{
    agent::Config,
    code_tree::{CodeTree, Exclusions},
    error::invalid,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

/// 往復の回数の既定と上限。
pub const DEFAULT_ROUNDS: usize = 12;
pub const MAX_ROUNDS: usize = 30;
/// 1往復でAIが求められるものの件数。
pub const REQUESTS_PER_ROUND: usize = 8;
/// 送る資料の合計の上限。文字の数はAIエージェントの上限と同じ数え方で、バイト数はHTTPの入力の上限（1MiB）に収めるため。
pub const MAX_MATERIAL_CHARS: usize = 400_000;
pub const MAX_MATERIAL_BYTES: usize = 800_000;
/// 一覧で返す項目の数の上限。
pub const MAX_LIST_ENTRIES: usize = 500;
const MAX_OPERATION: usize = 2000;
const MAX_SUMMARY: usize = 8000;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvestigateRequest {
    pub repository: String,
    pub commit: String,
    /// 対象の操作（例：患者の新規登録）。調べる出発点になる。
    pub operation: String,
    #[serde(default)]
    pub summary: String,
    /// 構成図の要約。任意。
    #[serde(default)]
    pub architecture: Option<String>,
    /// 既定に足す、送らないパスの指定。
    #[serde(default)]
    pub exclusions: Vec<String>,
    #[serde(default)]
    pub rounds: Option<usize>,
    #[serde(default)]
    pub confirm: Option<ScopeConfirm>,
}

/// 人が了承した範囲のSHA-256。範囲が変わったら、了承し直す。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfirm {
    pub scope_sha256: String,
}

/// 固定したコミットの取得元。画面からは GitHub の公開リポジトリだけを使う。
pub enum Source<'a> {
    GitHub,
    /// 試験などで、取得元のURL（https か file）を指定する。
    Url(&'a str),
}

/// 範囲を示すか（了承がないとき）、了承した範囲で往復を回して結論を返す。
/// `checkouts` は取得したコミットの置き場所、`records` は往復の記録の置き場所。
pub fn investigate(
    config: Option<&Config>,
    checkouts: &Path,
    records: &Path,
    request: InvestigateRequest,
    source: Source,
) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    let operation = request.operation.trim();
    if operation.is_empty() || operation.chars().count() > MAX_OPERATION {
        return Err(invalid(format!(
            "対象の操作は1〜{MAX_OPERATION}文字で書いてください"
        )));
    }
    let summary = request.summary.trim();
    let architecture = request.architecture.as_deref().map(str::trim);
    if summary.chars().count() > MAX_SUMMARY
        || architecture.is_some_and(|a| a.chars().count() > MAX_SUMMARY)
    {
        return Err(invalid(format!(
            "説明と構成図の要約は、それぞれ{MAX_SUMMARY}文字以内にしてください"
        )));
    }
    let rounds = request.rounds.unwrap_or(DEFAULT_ROUNDS);
    if !(1..=MAX_ROUNDS).contains(&rounds) {
        return Err(invalid(format!(
            "往復の回数は1〜{MAX_ROUNDS}回にしてください"
        )));
    }
    let exclusions = Exclusions::new(&request.exclusions)?;
    // 形の確かめだけ。取得は了承の後に行う。
    crate::code_tree::validate_source(&request.repository, &request.commit)?;
    let scope = json!({
        "repository": request.repository,
        "commit": request.commit,
        "operation": operation,
        "summary": summary,
        "architecture": architecture,
        "exclusions": exclusions.patterns(),
        "limits": {
            "rounds": rounds,
            "requests_per_round": REQUESTS_PER_ROUND,
            "material_chars": MAX_MATERIAL_CHARS,
            "material_bytes": MAX_MATERIAL_BYTES,
        },
    });
    let scope_sha256 = sha256(&scope);
    let Some(confirm) = request.confirm else {
        return Ok(json!({
            "stage": "preview",
            "destination": "運営者が設定したAIエージェント（mugiurushi-ai）と、その先のOpenAI API",
            "scope": scope,
            "scope_sha256": scope_sha256,
            "note": "このリポジトリのこのコミットのコードのうち、送らないパスを除く範囲から、AIが求めた検索の結果・ファイルの一覧・読み取った範囲を、往復のたびに送ります。送る中身は、往復の前には決まりません。範囲と上限を確かめて了承してください。送ったものは記録に残ります。麦漆は内容の安全性を判断しません。",
        }));
    };
    if confirm.scope_sha256 != scope_sha256 {
        return Err(invalid(
            "了承した後に範囲が変わりました。もう一度確かめてください",
        ));
    }
    let tree = match source {
        Source::GitHub => {
            CodeTree::open(checkouts, &request.repository, &request.commit, exclusions)?
        }
        Source::Url(url) => CodeTree::open_from(
            checkouts,
            &request.repository,
            &request.commit,
            url,
            exclusions,
        )?,
    };
    let mut run = Run {
        tree: &tree,
        material: vec![],
        answered: BTreeSet::new(),
        chars: 0,
        bytes: 0,
        full: false,
        log: vec![],
    };
    let outcome = run.rounds(config, &scope, rounds);
    let record_id = save_record(records, &scope, &scope_sha256, &run.log, &outcome)?;
    let mut result = outcome?;
    result["record"] = json!(record_id);
    result["scope_sha256"] = json!(scope_sha256);
    Ok(result)
}

struct Run<'a> {
    tree: &'a CodeTree,
    material: Vec<Value>,
    /// 答えた要求。同じ要求に二度答えない。
    answered: BTreeSet<String>,
    chars: usize,
    bytes: usize,
    /// 資料の上限に達した。次の往復で結論を求める。
    full: bool,
    log: Vec<Value>,
}

impl Run<'_> {
    fn rounds(&mut self, config: &Config, scope: &Value, rounds: usize) -> io::Result<Value> {
        for round in 0..rounds {
            let rounds_left = if self.full { 0 } else { rounds - 1 - round };
            let payload = json!({
                "repository": scope["repository"],
                "commit": scope["commit"],
                "operation": scope["operation"],
                "summary": scope["summary"],
                "architecture": scope["architecture"],
                "material": self.material,
                "budget": {"rounds_left": rounds_left, "requests_per_round": REQUESTS_PER_ROUND},
            });
            let mut entry = json!({
                "round": round + 1,
                "rounds_left": rounds_left,
                "payload_sha256": sha256(&payload),
                "payload_bytes": serde_json::to_vec(&payload).map_err(io::Error::other)?.len(),
            });
            let answer = config.post("/v1/investigate", &payload);
            let answer = match answer {
                Ok(answer) => answer,
                Err(error) => {
                    entry["error"] = json!(error.to_string());
                    self.log.push(entry);
                    return Err(error);
                }
            };
            entry["response_id"] = answer["model"]["response_id"].clone();
            if answer["done"] == json!(true) {
                entry["done"] = json!(true);
                self.log.push(entry);
                return self.conclusion(&answer, round + 1, "concluded");
            }
            let requests = answer["requests"]
                .as_array()
                .filter(|r| !r.is_empty() && r.len() <= REQUESTS_PER_ROUND)
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "AIエージェントの求めるものの形が不正です（1回に{REQUESTS_PER_ROUND}件まで）"
                    ))
                })?
                .clone();
            if rounds_left == 0 {
                // 最後の往復でも求めるものが返った。これ以上は送らない。
                entry["requests"] = json!(requests);
                entry["stopped"] = json!("rounds");
                self.log.push(entry);
                return Ok(json!({
                    "stage": "done",
                    "stopped_by": "rounds",
                    "rounds": round + 1,
                    "concerns": [],
                    "questions": [],
                    "material": self.references(),
                    "note": "往復の上限に達し、AIエージェントが結論を返さなかった。懸念は受け取っていない",
                }));
            }
            let mut answered = vec![];
            for request in &requests {
                answered.push(self.answer(request));
            }
            entry["requests"] = json!(requests);
            entry["answered"] = json!(answered);
            self.log.push(entry);
        }
        Err(io::Error::other(
            "往復の回数の上限に達しましたが、結論を受け取れませんでした",
        ))
    }

    /// 求められたもの1件に答え、資料に足す。答えられないときは、理由を資料に足す。記録用の要約を返す。
    fn answer(&mut self, request: &Value) -> Value {
        let kind = request["kind"].as_str().unwrap_or_default();
        let key = describe(request);
        let reply = if !self.answered.insert(key.clone()) {
            Err("同じ要求に、すでに答えている".to_owned())
        } else if self.full {
            Err("資料の合計の上限に達したので、これ以上は返さない".to_owned())
        } else {
            self.lookup(kind, request).map_err(|e| e.to_string())
        };
        let material = match reply {
            Ok(material) => {
                let (chars, bytes) = size_of(&material);
                if self.chars + chars > MAX_MATERIAL_CHARS
                    || self.bytes + bytes > MAX_MATERIAL_BYTES
                {
                    self.full = true;
                    refused(&key, "資料の合計の上限に達したので、これ以上は返さない")
                } else {
                    material
                }
            }
            Err(reason) => refused(&key, &reason),
        };
        let (chars, bytes) = size_of(&material);
        self.chars += chars;
        self.bytes += bytes;
        let summary = json!({
            "request": key,
            "kind": material["kind"],
            "path": material.get("path"),
            "start": material.get("start"),
            "end": material.get("end"),
            "blob": material.get("blob"),
            "hits": material.get("hits").and_then(Value::as_array).map(Vec::len),
            "entries": material.get("paths").and_then(Value::as_array).map(Vec::len),
            "truncated": material.get("truncated"),
            "reason": material.get("reason"),
        });
        self.material.push(material);
        summary
    }

    fn lookup(&self, kind: &str, request: &Value) -> io::Result<Value> {
        match kind {
            "search" => {
                let query = request["query"].as_str().unwrap_or_default();
                let found = self
                    .tree
                    .search(query, request["ignore_case"] == json!(true))?;
                Ok(
                    json!({"kind": "search", "query": found.query, "ignore_case": found.ignore_case,
                    "hits": found.hits, "truncated": found.truncated}),
                )
            }
            "list" => {
                let prefix = request["prefix"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_end_matches('/');
                let (paths, truncated) = list(self.tree, prefix);
                if paths.is_empty() {
                    return Err(invalid(format!(
                        "{prefix:?} の下に、送ってよいファイルはない"
                    )));
                }
                Ok(
                    json!({"kind": "list", "prefix": prefix, "paths": paths, "truncated": truncated}),
                )
            }
            "read" => {
                let path = request["path"].as_str().unwrap_or_default();
                let start = request["start"].as_u64().unwrap_or(0) as usize;
                let end = request["end"].as_u64().unwrap_or(0) as usize;
                let read = self.tree.read(path, start, end)?;
                Ok(json!({"kind": "read", "path": read.path, "blob": read.blob,
                    "total_lines": read.total_lines, "start": read.start, "end": read.end,
                    "text": read.text, "truncated": read.truncated}))
            }
            _ => Err(invalid("知らない種類の要求には答えない")),
        }
    }

    /// 結論を受け取る。懸念の根拠の行が、この調べで返した行にあることを確かめる。
    fn conclusion(&self, answer: &Value, rounds: usize, stopped_by: &str) -> io::Result<Value> {
        let covered = covered_lines(&self.material);
        let concerns = answer["concerns"]
            .as_array()
            .ok_or_else(|| io::Error::other("AIエージェントの結論の形が不正です"))?;
        for concern in concerns {
            let evidence = concern["evidence"].as_array().filter(|e| !e.is_empty());
            let inside = evidence.is_some_and(|evidence| {
                evidence.iter().all(|e| {
                    let path = e["path"].as_str().unwrap_or_default();
                    let (start, end) = (e["start"].as_u64(), e["end"].as_u64());
                    match (start, end, covered.get(path)) {
                        (Some(start), Some(end), Some(lines)) if start >= 1 && start <= end => {
                            (start..=end).all(|n| lines.contains(&(n as usize)))
                        }
                        _ => false,
                    }
                })
            });
            if !inside {
                return Err(io::Error::other(
                    "AIエージェントが、この調べで返していない行を根拠にした懸念を返しました。受け取りません",
                ));
            }
        }
        Ok(json!({
            "stage": "done",
            "stopped_by": stopped_by,
            "rounds": rounds,
            "concerns": concerns,
            "questions": answer["questions"],
            "requests_dropped": answer["requests_dropped"],
            "limits": answer["limits"],
            "material": self.references(),
        }))
    }

    /// 返した資料の要約（中身は持たない）。どのファイルのどの範囲を送ったかを、結果と記録で示す。
    fn references(&self) -> Vec<Value> {
        self.material
            .iter()
            .map(|m| match m["kind"].as_str() {
                Some("read") => json!({"kind": "read", "path": m["path"], "start": m["start"], "end": m["end"], "blob": m["blob"]}),
                Some("search") => json!({"kind": "search", "query": m["query"], "hits": m["hits"].as_array().map_or(0, Vec::len), "truncated": m["truncated"]}),
                Some("list") => json!({"kind": "list", "prefix": m["prefix"], "entries": m["paths"].as_array().map_or(0, Vec::len)}),
                _ => json!({"kind": "refused", "request": m["request"], "reason": m["reason"]}),
            })
            .collect()
    }
}

/// パスの先頭の下にある、直下の項目（ファイルと、`/` で終わるディレクトリ）。
fn list(tree: &CodeTree, prefix: &str) -> (Vec<String>, bool) {
    let head = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let mut entries = BTreeSet::new();
    for path in tree.files() {
        let Some(rest) = path.strip_prefix(&head) else {
            continue;
        };
        let entry = match rest.split_once('/') {
            Some((dir, _)) => format!("{head}{dir}/"),
            None => path.clone(),
        };
        entries.insert(entry);
    }
    let truncated = entries.len() > MAX_LIST_ENTRIES;
    (
        entries.into_iter().take(MAX_LIST_ENTRIES).collect(),
        truncated,
    )
}

fn refused(request: &str, reason: &str) -> Value {
    json!({"kind": "refused", "request": request, "reason": reason})
}

/// 記録と重複の判定に使う、要求の短い表し方。
fn describe(request: &Value) -> String {
    let text = |key: &str| request[key].as_str().unwrap_or_default().to_owned();
    let line: String = match request["kind"].as_str() {
        Some("search") => format!(
            "search {:?}{}",
            text("query"),
            if request["ignore_case"] == json!(true) {
                " ignore_case"
            } else {
                ""
            }
        ),
        Some("list") => format!("list {:?}", text("prefix")),
        Some("read") => format!(
            "read {:?} {}-{}",
            text("path"),
            request["start"].as_u64().unwrap_or(0),
            request["end"].as_u64().unwrap_or(0)
        ),
        _ => format!("unknown {}", request["kind"]),
    };
    line.chars().take(2000).collect()
}

/// 資料1件の大きさ。文字の数は、AIエージェントが資料の合計を数えるのと同じ部分だけを数える。
fn size_of(material: &Value) -> (usize, usize) {
    let len = |v: &Value| v.as_str().map_or(0, |s| s.chars().count());
    let chars = match material["kind"].as_str() {
        Some("read") => len(&material["text"]),
        Some("search") => material["hits"].as_array().map_or(0, |hits| {
            hits.iter()
                .map(|h| len(&h["text"]) + len(&h["before"]) + len(&h["after"]))
                .sum()
        }),
        Some("list") => material["paths"]
            .as_array()
            .map_or(0, |paths| paths.iter().map(len).sum()),
        _ => len(&material["reason"]),
    };
    let bytes = serde_json::to_vec(material).map_or(usize::MAX, |b| b.len());
    (chars, bytes)
}

/// 資料の中にある行（パスごと）。懸念の根拠にできるのは、この行だけである。
fn covered_lines(material: &[Value]) -> BTreeMap<String, BTreeSet<usize>> {
    let mut lines: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for m in material {
        match m["kind"].as_str() {
            Some("read") => {
                let (Some(path), Some(start), Some(end)) =
                    (m["path"].as_str(), m["start"].as_u64(), m["end"].as_u64())
                else {
                    continue;
                };
                lines
                    .entry(path.to_owned())
                    .or_default()
                    .extend(start as usize..=end as usize);
            }
            Some("search") => {
                for hit in m["hits"].as_array().into_iter().flatten() {
                    let (Some(path), Some(line)) = (hit["path"].as_str(), hit["line"].as_u64())
                    else {
                        continue;
                    };
                    let line = line as usize;
                    let got = lines.entry(path.to_owned()).or_default();
                    got.insert(line);
                    if hit["before"].is_string() && line > 1 {
                        got.insert(line - 1);
                    }
                    if hit["after"].is_string() {
                        got.insert(line + 1);
                    }
                }
            }
            _ => {}
        }
    }
    lines
}

fn sha256(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

/// 往復の記録を保存する。送った資料の中身は持たず、どのファイルのどの範囲かと、内容のオブジェクトIDを持つ。
/// 中身は、固定したコミットとオブジェクトIDから読み直せる。
fn save_record(
    records: &Path,
    scope: &Value,
    scope_sha256: &str,
    log: &[Value],
    outcome: &io::Result<Value>,
) -> io::Result<String> {
    fs::create_dir_all(records)?;
    let id = format!(
        "{}-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let result = match outcome {
        Ok(result) => json!({"concerns": result["concerns"], "questions": result["questions"],
            "stopped_by": result["stopped_by"], "rounds": result["rounds"], "material": result["material"]}),
        Err(error) => json!({"error": error.to_string()}),
    };
    let record = json!({
        "schema_version": 1,
        "id": id,
        "scope": scope,
        "scope_sha256": scope_sha256,
        "rounds": log,
        "result": result,
    });
    let path = records.join(format!("{id}.json"));
    let bytes = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
    let partial = records.join(format!("{id}.json.partial"));
    fs::write(&partial, bytes)?;
    fs::rename(&partial, &path)?;
    Ok(id)
}
