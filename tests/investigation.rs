//! 操作を出発点にコードをたどる往復（#230 の段階3）。架空のリポジトリと、応答を順に返す偽のAIエージェントで確かめる。
use mugiurushi::{
    agent::Config,
    investigation::{InvestigateRequest, ScopeConfirm, Source, investigate},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

const SECRET: &str = "synthetic-secret-value-4711";

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

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        // 日時を固定し、コミットのIDを毎回同じにする。受け渡しの固定ファイルにコミットのIDが入るため。
        .env("GIT_AUTHOR_DATE", "2026-11-16T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-11-16T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// 架空の診療所のリポジトリ。保存の処理と、送らないファイル（秘密の値を含む）を持つ。
fn fixture(name: &str) -> Fixture {
    fixture_with(name, &[])
}

/// 架空の診療所のリポジトリに、ファイルを足したもの。
fn fixture_with(name: &str, extra: &[(String, String)]) -> Fixture {
    let root = std::env::temp_dir().join(format!(
        "mugiurushi-investigation-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let source = root.join("source");
    fs::create_dir_all(source.join("app/conf")).unwrap();
    fs::write(
        source.join("app/patient.py"),
        "def save(patient):\n    log.info(patient.national_id)\n    db.insert('patient', patient)\n",
    )
    .unwrap();
    fs::write(
        source.join("app/routes.py"),
        "def register():\n    return save(form())\n",
    )
    .unwrap();
    fs::write(
        source.join("app/conf/.env"),
        format!("DB_PASSWORD={SECRET}\n"),
    )
    .unwrap();
    fs::write(source.join("README.md"), "架空の診療所\n").unwrap();
    for (path, text) in extra {
        let path = source.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
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

/// 受け取った要求（経路と本文）。
type Received = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// 応答を順に返し、受け取った本文を残す偽のAIエージェント。
struct Agent {
    url: String,
    token_file: PathBuf,
    received: Received,
}

impl Agent {
    fn start(directory: &Path, answers: Vec<Value>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        fs::create_dir_all(directory).unwrap();
        let token_file = directory.join("agent-token");
        fs::write(&token_file, "synthetic-agent-token-0123456789\n").unwrap();
        let received = Arc::new(Mutex::new(vec![]));
        let log = received.clone();
        std::thread::spawn(move || {
            let mut answers = answers.into_iter();
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock().unwrap().push((path, body));
                let answer = answers.next().unwrap_or_else(|| json!({}));
                // `{"__status": 502}` は、AIエージェントが処理できなかった応答を表す。
                let status = answer["__status"].as_u64().unwrap_or(200);
                let answer = if status == 200 {
                    answer.to_string()
                } else {
                    json!({"detail": "合成の失敗"}).to_string()
                };
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .unwrap();
            }
        });
        Self {
            url,
            token_file,
            received,
        }
    }

    fn config(&self) -> Config {
        Config::new(&self.url, self.token_file.clone()).unwrap()
    }

    fn bodies(&self) -> Vec<(Value, Vec<u8>)> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|(path, body)| {
                assert_eq!(path, "/v1/investigate");
                (serde_json::from_slice(body).unwrap(), body.clone())
            })
            .collect()
    }
}

fn request(f: &Fixture, rounds: usize, confirm: Option<String>) -> InvestigateRequest {
    InvestigateRequest {
        repository: "example/clinic".into(),
        commit: f.commit.clone(),
        operation: "患者の新規登録".into(),
        summary: "架空の診療所の受付".into(),
        architecture: None,
        exclusions: vec![],
        rounds: Some(rounds),
        confirm: confirm.map(|scope_sha256| ScopeConfirm { scope_sha256 }),
    }
}

fn need(requests: Value) -> Value {
    json!({"status": "candidate", "done": false, "requests": requests, "model": usage("fake")})
}

fn concluded(concerns: Value) -> Value {
    json!({"status": "candidate", "done": true, "concerns": concerns, "questions": ["ログは何日残しますか"],
        "requests_dropped": 0, "limits": "合成", "model": usage("fake-done")})
}

/// AIエージェントが返す呼び出しの記録（トークン数つき）。
fn usage(response_id: &str) -> Value {
    json!({"response_id": response_id, "response_model": "fake-model",
        "input_tokens": 1000, "cached_input_tokens": 400, "output_tokens": 50})
}

fn concern(path: &str, start: u64, end: u64) -> Value {
    json!({"id": "c-1", "estimate": "保存の処理が国民識別番号をログに出す", "evidence": [{"path": path, "start": start, "end": end}],
        "concern": "識別番号がログに平文で残る", "basis": "repository",
        "check": {"by": "run", "detail": "架空の番号で登録してログを読む", "observation": "container_log"}})
}

fn read(path: &str, start: u64, end: u64) -> Value {
    json!({"kind": "read", "path": path, "start": start, "end": end, "why": "読む"})
}

/// 範囲を示してから、そのSHA-256で了承して往復を回す。
fn run(f: &Fixture, agent: &Agent, rounds: usize) -> std::io::Result<Value> {
    let checkouts = f.root.join("checkouts");
    let records = f.root.join("case/investigations");
    let config = agent.config();
    let preview = investigate(
        Some(&config),
        &checkouts,
        &records,
        request(f, rounds, None),
        Source::Url(&f.url),
    )?;
    let sha = preview["scope_sha256"].as_str().unwrap().to_owned();
    investigate(
        Some(&config),
        &checkouts,
        &records,
        request(f, rounds, Some(sha)),
        Source::Url(&f.url),
    )
}

/// AIエージェント（mugiurushi-ai）との受け渡しを固定したファイルと比べる。
/// 同じファイルを mugiurushi-ai の `tests/contract/mugiurushi/` に置き、そちらの試験が入力として使う。
/// 内容を変えた場合は `MUGIURUSHI_UPDATE_CONTRACT=1` で書き直し、mugiurushi-ai の側も同じ内容に更新する。
fn contract(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/contract/agent")
        .join(name);
    if std::env::var_os("MUGIURUSHI_UPDATE_CONTRACT").is_some() {
        fs::write(&path, serde_json::to_string_pretty(actual).unwrap() + "\n").unwrap();
    }
    let expected: Value = serde_json::from_str(
        &fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap();
    assert_eq!(
        actual,
        &expected,
        "{} と送る内容が違う。MUGIURUSHI_UPDATE_CONTRACT=1 で書き直す",
        path.display()
    );
}

fn record(f: &Fixture, id: &str) -> Value {
    serde_json::from_slice(
        &fs::read(f.root.join(format!("case/investigations/{id}.json"))).unwrap(),
    )
    .unwrap()
}

#[test]
fn the_scope_is_shown_first_and_nothing_is_fetched_or_sent_until_it_is_accepted() {
    let f = fixture("preview");
    let agent = Agent::start(&f.root, vec![]);
    let config = agent.config();
    let checkouts = f.root.join("checkouts");
    let records = f.root.join("case/investigations");
    let preview = investigate(
        Some(&config),
        &checkouts,
        &records,
        request(&f, 4, None),
        Source::Url(&f.url),
    )
    .unwrap();
    assert_eq!(preview["stage"], "preview");
    assert_eq!(preview["scope"]["operation"], "患者の新規登録");
    assert_eq!(preview["scope"]["limits"]["rounds"], 4);
    // 送らないパスは、既定の分も含めて全部を示す。
    assert!(
        preview["scope"]["exclusions"]
            .as_array()
            .unwrap()
            .contains(&json!(".env"))
    );
    assert!(agent.bodies().is_empty(), "了承の前に送った");
    assert!(!checkouts.exists(), "了承の前に取得した");
    // 違う範囲のSHA-256では始めない。範囲を変えたら、了承し直す。
    let wrong = investigate(
        Some(&config),
        &checkouts,
        &records,
        request(&f, 4, Some("0".repeat(64))),
        Source::Url(&f.url),
    );
    assert!(wrong.is_err());
    let sha = preview["scope_sha256"].as_str().unwrap().to_owned();
    let changed = investigate(
        Some(&config),
        &checkouts,
        &records,
        request(&f, 5, Some(sha)),
        Source::Url(&f.url),
    );
    assert!(changed.is_err(), "往復の上限を変えたのに、前の了承で始めた");
    assert!(agent.bodies().is_empty());
}

#[test]
fn requests_are_answered_from_the_pinned_commit_and_concerns_must_cite_returned_lines() {
    let f = fixture("rounds");
    let agent = Agent::start(
        &f.root,
        vec![
            need(json!([
                {"kind": "search", "query": "national_id", "ignore_case": false, "why": "値の行き先を探す"},
                read("app/patient.py", 1, 3),
                read("app/conf/.env", 1, 1),
                {"kind": "list", "prefix": "", "why": "構成を見る"},
            ])),
            need(json!([
                read("app/patient.py", 1, 3),
                read("missing.py", 1, 1)
            ])),
            concluded(json!([concern("app/patient.py", 2, 2)])),
        ],
    );
    let result = run(&f, &agent, 4).unwrap();
    assert_eq!(result["stage"], "done");
    assert_eq!(
        (result["stopped_by"].as_str(), result["rounds"].as_u64()),
        (Some("concluded"), Some(3))
    );
    assert_eq!(
        result["concerns"][0]["evidence"],
        json!([{"path": "app/patient.py", "start": 2, "end": 2}])
    );
    let bodies = agent.bodies();
    assert_eq!(bodies.len(), 3);
    contract("investigate-first-round.json", &bodies[0].0);
    contract("investigate-second-round.json", &bodies[1].0);
    // 予算は往復ごとに減る。最初の往復の資料は空。
    let left: Vec<u64> = bodies
        .iter()
        .map(|(b, _)| b["budget"]["rounds_left"].as_u64().unwrap())
        .collect();
    assert_eq!(left, [3, 2, 1]);
    assert_eq!(bodies[0].0["material"], json!([]));
    assert_eq!(bodies[0].0["operation"], "患者の新規登録");
    // 2回目の往復の資料：検索の結果、読み取った範囲、送らないファイルは理由だけ、一覧。
    let material = bodies[1].0["material"].as_array().unwrap();
    let kinds: Vec<&str> = material
        .iter()
        .map(|m| m["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["search", "read", "refused", "list"]);
    assert_eq!(material[0]["hits"][0]["path"], "app/patient.py");
    assert_eq!(material[0]["hits"][0]["line"], 2);
    assert!(material[1]["text"].as_str().unwrap().contains("db.insert"));
    assert_eq!(material[1]["blob"].as_str().unwrap().len(), 40);
    assert!(material[2]["reason"].as_str().unwrap().contains("送らない"));
    assert_eq!(material[3]["paths"], json!(["README.md", "app/"]));
    // 3回目：同じ要求には二度答えず、固定版にないパスは理由を返す。
    let third = bodies[2].0["material"].as_array().unwrap();
    assert_eq!(third.len(), 6);
    assert!(
        third[4]["reason"]
            .as_str()
            .unwrap()
            .contains("すでに答えている")
    );
    assert_eq!(third[5]["kind"], "refused");
    // 送らないファイルの中身は、どの往復の本文にも出ない。
    for (_, raw) in &bodies {
        assert!(
            !String::from_utf8_lossy(raw).contains(SECRET),
            "送らないファイルの中身を送った"
        );
    }
    // 記録：範囲、往復ごとの送信内容のSHA-256（実際に送った本文と同じ）、答えたもの。資料の中身は持たない。
    let saved = record(&f, result["record"].as_str().unwrap());
    assert_eq!(saved["scope_sha256"], result["scope_sha256"]);
    let rounds = saved["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 3);
    for (entry, (body, _)) in rounds.iter().zip(&bodies) {
        use sha2::{Digest, Sha256};
        let sent = format!("{:x}", Sha256::digest(serde_json::to_vec(body).unwrap()));
        assert_eq!(entry["payload_sha256"].as_str().unwrap(), sent);
    }
    assert_eq!(rounds[0]["answered"][1]["blob"].as_str().unwrap().len(), 40);
    // 検索で送った行も、ファイルと範囲（当たった行と前後の行）を記録と結果に残す。
    let sent = json!([{"path": "app/patient.py", "start": 1, "end": 3}]);
    assert_eq!(rounds[0]["answered"][0]["ranges"], sent);
    assert_eq!(result["material"][0]["ranges"], sent);
    assert!(
        !serde_json::to_string(&saved).unwrap().contains("db.insert"),
        "記録に資料の中身がある"
    );
    assert_eq!(saved["result"]["concerns"].as_array().unwrap().len(), 1);
    // 往復ごとのトークン数と、その合計を残す。費用を出すために使う。
    assert_eq!(
        rounds[0]["usage"],
        json!({"input_tokens": 1000, "cached_input_tokens": 400, "output_tokens": 50})
    );
    assert_eq!(rounds[0]["response_model"], "fake-model");
    let total = json!({"calls": 3, "reported": 3, "input_tokens": 3000, "cached_input_tokens": 1200, "output_tokens": 150});
    assert_eq!(saved["usage"], total);
    assert_eq!(result["usage"], total);
}

#[test]
fn a_concern_citing_lines_that_were_not_returned_is_refused_and_recorded() {
    let f = fixture("outside");
    let agent = Agent::start(
        &f.root,
        vec![
            need(json!([read("app/patient.py", 1, 1)])),
            // 2行目は返していない。
            concluded(json!([concern("app/patient.py", 1, 2)])),
        ],
    );
    let error = run(&f, &agent, 3).unwrap_err().to_string();
    assert!(error.contains("返していない行"), "{error}");
    let ids: Vec<_> = fs::read_dir(f.root.join("case/investigations"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(ids.len(), 1, "失敗した調べも記録に残す");
    let saved: Value = serde_json::from_slice(&fs::read(ids[0].path()).unwrap()).unwrap();
    assert!(
        saved["result"]["error"]
            .as_str()
            .unwrap()
            .contains("返していない行")
    );
}

#[test]
fn the_last_round_asks_for_a_conclusion_and_more_requests_are_not_answered() {
    let f = fixture("limit");
    let search =
        |q: &str| json!([{"kind": "search", "query": q, "ignore_case": false, "why": "探す"}]);
    let agent = Agent::start(
        &f.root,
        vec![
            need(search("save")),
            need(search("insert")),
            need(search("log")),
        ],
    );
    let result = run(&f, &agent, 2).unwrap();
    assert_eq!(result["stopped_by"], "rounds");
    assert_eq!(result["concerns"], json!([]));
    let bodies = agent.bodies();
    assert_eq!(bodies.len(), 2, "上限を超えて送った");
    assert_eq!(bodies[1].0["budget"]["rounds_left"], 0);
}

#[test]
fn more_requests_than_allowed_in_one_round_are_refused() {
    let f = fixture("many");
    let many: Vec<Value> = (1..=9).map(|n| read("app/patient.py", n, n)).collect();
    let agent = Agent::start(&f.root, vec![need(json!(many))]);
    assert!(run(&f, &agent, 3).is_err());
    // 不正な応答で止まっても、送った往復（送信内容のSHA-256と応答のID）は記録に残る。
    let ids: Vec<_> = fs::read_dir(f.root.join("case/investigations"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(ids.len(), 1);
    let saved: Value = serde_json::from_slice(&fs::read(ids[0].path()).unwrap()).unwrap();
    let rounds = saved["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 1, "{saved}");
    assert_eq!(rounds[0]["payload_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(rounds[0]["response_id"], "fake");
    assert!(rounds[0]["error"].as_str().unwrap().contains("形が不正"));
}

#[test]
fn material_including_refusals_never_exceeds_the_accepted_total() {
    // 4万文字近いファイルを11件。8件と3件を求めると、合計の上限に達する。
    let line = "x".repeat(99);
    let big = format!("{line}\n").repeat(400);
    let extra: Vec<(String, String)> = (0..11)
        .map(|n| (format!("big/{n:02}.txt"), big.clone()))
        .collect();
    let f = fixture_with("total", &extra);
    let reads = |range: std::ops::Range<u64>| -> Value {
        json!(
            range
                .map(|n| read(&format!("big/{n:02}.txt"), 1, 400))
                .collect::<Vec<_>>()
        )
    };
    let mut second = reads(8..11);
    // 同じ要求と送らないファイルも求め、断る理由を増やす。
    for again in reads(0..3).as_array().unwrap() {
        second.as_array_mut().unwrap().push(again.clone());
    }
    second
        .as_array_mut()
        .unwrap()
        .push(read("app/conf/.env", 1, 1));
    let agent = Agent::start(
        &f.root,
        vec![need(reads(0..8)), need(second), concluded(json!([]))],
    );
    let result = run(&f, &agent, 5).unwrap();
    assert_eq!(result["stopped_by"], "concluded");
    let bodies = agent.bodies();
    assert_eq!(bodies.len(), 3);
    // 上限に達した後の往復では、結論を求める。
    assert_eq!(bodies[2].0["budget"]["rounds_left"], 0);
    let material = bodies[2].0["material"].as_array().unwrap();
    let chars: usize = material
        .iter()
        .map(|m| {
            let len = |key: &str| m[key].as_str().map_or(0, |s| s.chars().count());
            len("text") + len("reason")
        })
        .sum();
    let bytes: usize = material
        .iter()
        .map(|m| serde_json::to_vec(m).unwrap().len())
        .sum();
    assert!(chars <= 400_000, "資料の文字の数 {chars} が上限を超えた");
    assert!(bytes <= 800_000, "資料のバイト数 {bytes} が上限を超えた");
    // 上限に達した要求は、中身を返さず理由を返す。
    let refused: Vec<&str> = material
        .iter()
        .filter(|m| m["kind"] == "refused")
        .map(|m| m["reason"].as_str().unwrap())
        .collect();
    assert!(
        refused.iter().any(|r| r.contains("合計の上限")),
        "{refused:?}"
    );
}

#[test]
fn token_counts_that_were_not_reported_are_not_summed_as_zero() {
    let f = fixture("usage");
    let mut first = need(json!([read("app/patient.py", 1, 3)]));
    first["model"] = json!({"response_id": "fake", "input_tokens": 900, "output_tokens": 20});
    let agent = Agent::start(
        &f.root,
        vec![first, concluded(json!([concern("app/patient.py", 2, 2)]))],
    );
    let result = run(&f, &agent, 3).unwrap();
    assert_eq!(
        result["usage"],
        json!({"calls": 2, "reported": 2, "input_tokens": 1900, "cached_input_tokens": null, "output_tokens": 70})
    );
}

#[test]
fn a_round_whose_token_counts_were_not_received_makes_the_totals_unknown() {
    let f = fixture("usage-failed");
    let agent = Agent::start(
        &f.root,
        vec![
            need(json!([read("app/patient.py", 1, 3)])),
            json!({"__status": 502}),
        ],
    );
    assert!(run(&f, &agent, 3).is_err());
    let ids: Vec<_> = fs::read_dir(f.root.join("case/investigations"))
        .unwrap()
        .flatten()
        .collect();
    let saved: Value = serde_json::from_slice(&fs::read(ids[0].path()).unwrap()).unwrap();
    // 2回目の往復は送ったが、トークン数を受け取れていない。1回目の分だけを合計として見せない。
    assert_eq!(
        saved["usage"],
        json!({"calls": 2, "reported": 1, "input_tokens": null, "cached_input_tokens": null, "output_tokens": null})
    );
}

#[test]
fn without_an_agent_nothing_starts() {
    let f = fixture("none");
    let result = investigate(
        None,
        &f.root.join("checkouts"),
        &f.root.join("records"),
        request(&f, 2, None),
        Source::Url(&f.url),
    );
    assert!(result.is_err());
}
