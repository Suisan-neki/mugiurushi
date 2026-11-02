//! 理解と試験の画面が使う記録・照合・AI接続の振る舞いを、架空のデータで確かめる。
use mugiurushi::{
    ai,
    execution::Criterion,
    recording::new_run_directory,
    understanding::{Store, UnderstandRequest, verify_refs},
    workspace::*,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        Self(
            new_run_directory(&std::env::temp_dir().join("eel-app-tests"))
                .unwrap()
                .0,
        )
    }
    fn case(&self) -> PathBuf {
        let path = self.0.join("case");
        initialize(
            &path,
            empty_workspace("app".into(), "架空の記録".into()),
            "基盤",
            "試験用の空の事例",
        )
        .unwrap();
        path
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const NOTES: &str = "import sys\n\nclass BadFile(Exception):\n    pass\n\ndef save(path, text):\n    try:\n        with open(path, 'w') as f:\n            f.write(text)\n    except OSError:\n        raise BadFile(path)\n\ndef main():\n    try:\n        save('notes.txt', sys.argv[1])\n    except BadFile:\n        sys.stderr.write('保存できませんでした\\n')\n        sys.exit(1)\n\nif __name__ == '__main__':\n    main()\n";

fn snapshot() -> Value {
    json!({
        "schema": 1,
        "fullName": "example/notes",
        "description": "架空の記録アプリ",
        "reference": "main",
        "commit": COMMIT,
        "scannedAt": "2026-10-14T00:00:00Z",
        "truncated": false,
        "inventory": [
            {"path": "notes.py", "size": NOTES.len(), "mode": "100644", "sha": "0".repeat(40)},
            {"path": "docs/guide.md", "size": 10, "mode": "100644", "sha": "0".repeat(40)},
        ],
        "files": {"notes.py": NOTES},
        "omitted": [{"path": "docs/guide.md", "reason": "初回の読取範囲外"}],
    })
}

fn store(sandbox: &Sandbox) -> Store {
    let base = sandbox.0.join("snapshots");
    fs::create_dir_all(base.join("example/notes")).unwrap();
    fs::write(
        base.join(format!("example/notes/{COMMIT}.json")),
        serde_json::to_vec(&snapshot()).unwrap(),
    )
    .unwrap();
    Store::new(base).unwrap()
}

fn apply(case: &Path, actor: &str, action: Change) -> std::io::Result<WorkspaceView> {
    let revision = load(case)?.revision;
    change(
        case,
        ChangeRequest {
            expected_revision: revision,
            actor: actor.into(),
            reason: "架空の操作".into(),
            action,
        },
    )
}

fn source(line: u32) -> CodeReference {
    CodeReference {
        repository: "example/notes".into(),
        commit: COMMIT.into(),
        path: "notes.py".into(),
        line,
        end_line: line,
    }
}

fn statement(id: &str, kind: StatementKind, role: Role) -> Statement {
    Statement {
        id: id.into(),
        kind,
        role: Some(role),
        text: "架空の発言".into(),
        origin: "試験".into(),
        check_ids: vec!["notes-1".into()],
        source: None,
        evidence_id: None,
        recorded_by: String::new(),
        recorded_revision: 0,
        check_versions: BTreeMap::new(),
        confirmations: vec![],
        resolution: None,
    }
}

fn add_check(case: &Path) {
    apply(
        case,
        "架空の運用担当（運用担当）",
        Change::AddCheck {
            check: Check {
                id: "notes-1".into(),
                version: 1,
                text: "利用者がメモを保存したとき、後から読み出せる".into(),
                origin: "架空の運用担当が画面で入力した期待".into(),
                source: None,
                observation: None,
                rule: Rule::ManualOnly,
                dependencies: vec![],
                questions: vec![],
                aspects: vec![],
            },
        },
    )
    .unwrap();
}

fn status_of(view: &WorkspaceView, id: &str) -> ConfirmationStatus {
    view.statements
        .iter()
        .find(|s| s.id == id)
        .unwrap()
        .status
        .clone()
}

#[test]
fn both_roles_must_confirm_the_current_version_before_it_counts() {
    let sandbox = Sandbox::new();
    let case = sandbox.case();
    add_check(&case);
    let view = apply(
        &case,
        "架空の開発者（開発者）",
        Change::AddStatement {
            statement: Statement {
                source: Some(source(8)),
                ..statement("s-impl", StatementKind::Implementation, Role::Developer)
            },
        },
    )
    .unwrap();
    let saved = view
        .workspace
        .statements
        .iter()
        .find(|s| s.id == "s-impl")
        .unwrap();
    assert_eq!(saved.recorded_by, "架空の開発者（開発者）");
    assert_eq!(saved.check_versions.get("notes-1"), Some(&1));
    assert!(matches!(
        status_of(&view, "s-impl"),
        ConfirmationStatus::Unconfirmed
    ));

    let confirm = |actor: &str, role: Role| {
        apply(
            &case,
            actor,
            Change::ConfirmStatement {
                statement_id: "s-impl".into(),
                role,
                note: String::new(),
            },
        )
        .unwrap()
    };
    let view = confirm("架空の運用担当（運用担当）", Role::Operator);
    assert!(matches!(
        status_of(&view, "s-impl"),
        ConfirmationStatus::PartlyConfirmed
    ));
    let view = confirm("架空の開発者（開発者）", Role::Developer);
    assert!(matches!(
        status_of(&view, "s-impl"),
        ConfirmationStatus::BothConfirmed
    ));

    // 期待の文を直すと条件の版が上がり、前の確認は現在の版の確認として数えない。
    let view = apply(
        &case,
        "架空の運用担当（運用担当）",
        Change::ReviseCheck {
            check_id: "notes-1".into(),
            text: "利用者がメモを保存したとき、別の画面からも読み出せる".into(),
            origin: "架空の運用担当が直した期待".into(),
            rule: Rule::ManualOnly,
            dependencies: vec![],
        },
    )
    .unwrap();
    assert!(matches!(
        status_of(&view, "s-impl"),
        ConfirmationStatus::Outdated
    ));
    let saved = view
        .workspace
        .statements
        .iter()
        .find(|s| s.id == "s-impl")
        .unwrap();
    assert_eq!(saved.confirmations.len(), 2);
}

#[test]
fn clients_cannot_supply_confirmations_or_resolve_plain_statements() {
    let sandbox = Sandbox::new();
    let case = sandbox.case();
    add_check(&case);
    let forged = Statement {
        confirmations: vec![Confirmation {
            actor: "誰か".into(),
            role: Role::Developer,
            revision: 1,
            check_versions: BTreeMap::new(),
            note: String::new(),
        }],
        ..statement("s-forged", StatementKind::Statement, Role::Operator)
    };
    assert!(
        apply(
            &case,
            "架空の運用担当（運用担当）",
            Change::AddStatement { statement: forged }
        )
        .is_err()
    );

    apply(
        &case,
        "架空の運用担当（運用担当）",
        Change::AddStatement {
            statement: statement("s-plain", StatementKind::Statement, Role::Operator),
        },
    )
    .unwrap();
    assert!(
        apply(
            &case,
            "架空の開発者（開発者）",
            Change::ResolveStatement {
                statement_id: "s-plain".into(),
                text: "対応した".into()
            }
        )
        .is_err()
    );

    // 指摘は対象の期待が必要で、対応を記録すると未対応ではなくなる。元の文は残る。
    let orphan = Statement {
        check_ids: vec![],
        ..statement("s-orphan", StatementKind::Discrepancy, Role::Operator)
    };
    assert!(
        apply(
            &case,
            "架空の運用担当（運用担当）",
            Change::AddStatement { statement: orphan }
        )
        .is_err()
    );
    let view = apply(
        &case,
        "架空の運用担当（運用担当）",
        Change::AddStatement {
            statement: statement("s-gap", StatementKind::Discrepancy, Role::Operator),
        },
    )
    .unwrap();
    assert!(
        view.statements
            .iter()
            .find(|s| s.id == "s-gap")
            .unwrap()
            .open
    );
    let view = apply(
        &case,
        "架空の開発者（開発者）",
        Change::ResolveStatement {
            statement_id: "s-gap".into(),
            text: "判定の条件に終了コードを加えた".into(),
        },
    )
    .unwrap();
    assert!(
        !view
            .statements
            .iter()
            .find(|s| s.id == "s-gap")
            .unwrap()
            .open
    );
    let saved = view
        .workspace
        .statements
        .iter()
        .find(|s| s.id == "s-gap")
        .unwrap();
    assert_eq!(saved.text, "架空の発言");
    assert_eq!(
        saved.resolution.as_ref().unwrap().actor,
        "架空の開発者（開発者）"
    );
}

#[test]
fn stale_revision_is_rejected() {
    let sandbox = Sandbox::new();
    let case = sandbox.case();
    add_check(&case);
    let error = change(
        &case,
        ChangeRequest {
            expected_revision: 1,
            actor: "架空の開発者（開発者）".into(),
            reason: "古い版で操作する".into(),
            action: Change::ConfigureObservation {
                check_id: "notes-1".into(),
                criterion: Criterion {
                    assertion: "eel.trial".into(),
                    subject: "notes-1".into(),
                },
            },
        },
    )
    .unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn references_are_checked_against_the_pinned_files() {
    let snapshot = snapshot();
    let lines = NOTES.lines().count() as u64;
    let mut result = json!({
        "items": [
            {"text": "書き込む", "refs": [{"path": "notes.py", "line": 8, "end_line": 9}]},
            {"text": "ファイル全体", "refs": [{"path": "notes.py", "line": 1, "end_line": lines}]},
            {"text": "範囲外", "refs": [{"path": "notes.py", "line": 1, "end_line": lines + 1}]},
            {"text": "未読", "refs": [{"path": "docs/guide.md", "line": 1, "end_line": 1}]},
        ]
    });
    let summary = verify_refs(&mut result, &snapshot);
    assert_eq!(summary["total"], 4);
    assert_eq!(summary["verified"], 2);
    let refs: Vec<&Value> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["refs"][0])
        .collect();
    assert_eq!(refs[0]["verified"], true);
    assert!(
        refs[0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("open(path, 'w')")
    );
    assert_eq!(refs[1]["verified"], true);
    assert_eq!(refs[2]["verified"], false);
    assert_eq!(refs[3]["verified"], false);
    assert!(
        refs[3]["reason"]
            .as_str()
            .unwrap()
            .contains("読取記録にない")
    );
}

#[test]
fn stored_commit_is_reopened_without_network_and_questions_cite_real_lines() {
    let sandbox = Sandbox::new();
    let store = store(&sandbox);
    let opened = store
        .handle(UnderstandRequest::Open {
            url: "https://github.com/example/notes".into(),
            reference: COMMIT.into(),
        })
        .unwrap();
    assert_eq!(opened["source"]["commit"], COMMIT);
    assert!(opened["source"]["inventory"][0].get("text").is_none());
    assert_eq!(
        opened["references"]["verified"],
        opened["references"]["total"]
    );

    let answer = store
        .handle(UnderstandRequest::Inquire {
            repository: "example/notes".into(),
            commit: COMMIT.into(),
            question: "保存できなかったらどうなる？".into(),
            path: None,
            line: None,
        })
        .unwrap();
    let text = answer["result"].to_string();
    assert!(text.contains("BadFile"));
    assert!(text.contains("main"));
    assert_eq!(
        answer["references"]["verified"],
        answer["references"]["total"]
    );

    let missing = store.handle(UnderstandRequest::Plan {
        repository: "example/notes".into(),
        commit: "f".repeat(40),
    });
    assert!(missing.is_err());
}

#[test]
fn ai_is_reported_as_unconfigured_or_failed_and_never_replaced_by_fixed_text() {
    assert_eq!(ai::status(None)["status"], "not_configured");
    let sandbox = Sandbox::new();
    let store = store(&sandbox);
    let request = || ai::AiRequest {
        repository: "example/notes".into(),
        commit: COMMIT.into(),
        question: "保存できなかったらどうなる？".into(),
        path: None,
        line: None,
    };
    let unset = ai::explain(None, &store, request()).unwrap();
    assert_eq!(unset["status"], "not_configured");
    assert!(unset.get("answer").is_none());

    let missing_key = ai::Config::new("http://127.0.0.1:9/v1", "synthetic", sandbox.0.join("none"));
    assert_eq!(ai::status(Some(&missing_key))["status"], "failed");

    let key = sandbox.0.join("key");
    fs::write(&key, "synthetic-key\n").unwrap();
    let unreachable = ai::Config::new("http://127.0.0.1:9/v1", "synthetic", key);
    assert_eq!(ai::status(Some(&unreachable))["status"], "configured");
    let failed = ai::explain(Some(&unreachable), &store, request()).unwrap();
    assert_eq!(failed["status"], "failed");
    assert!(failed.get("answer").is_none());
    assert!(
        !failed["message"]
            .as_str()
            .unwrap()
            .contains("synthetic-key")
    );
}

/// 合成のモデル応答だけを返すlocalhostサーバー。実際のモデルや課金先へ接続しない。
fn synthetic_ai(response: Value) -> (String, std::thread::JoinHandle<Value>) {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let thread = std::thread::spawn(move || {
        let started = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < Duration::from_secs(10),
                        "合成AIへの接続がない"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("合成AI: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let (offset, length) = loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() <= 2 * 1024 * 1024);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]);
                assert!(header.starts_with("POST /v1/chat/completions "));
                let length: usize = header
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|s| s.trim().parse().unwrap())
                    })
                    .unwrap();
                break (end + 4, length);
            }
        };
        while bytes.len() < offset + length {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        let request = serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
        let body = json!({"model":"synthetic-review", "choices":[{"message":{"content":response.to_string()}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        request
    });
    (format!("http://{address}/v1"), thread)
}

#[test]
fn ai_candidates_use_the_real_transport_and_do_not_write_or_confirm_the_case() {
    let sandbox = Sandbox::new();
    let store = store(&sandbox);
    let case = sandbox.case();
    let before = serde_json::to_value(load(&case).unwrap()).unwrap();
    let key = sandbox.0.join("synthetic-key");
    fs::write(&key, "synthetic-key").unwrap();
    let response = json!({
        "answer":"書き込みの例外を扱う処理がある。実際の保存は未確認。",
        "claims":[{"text":"ファイルに書き込む", "refs":[{"path":"notes.py","line":8}]}],
        "unknowns":["運用時の閲覧権限は不明"],
        "test_candidates":[
            {"expectation":"保存後に同じ値を読み出せる", "reason":"書き込みと読出しを分けて確かめる", "refs":[{"path":"notes.py","line":8,"end_line":9}], "unknowns":["後からの閲覧は未確認"]},
            {"expectation":"未読の説明が現在の動作と一致する", "reason":"説明の内容は不明", "refs":[{"path":"docs/guide.md","line":1}], "unknowns":["ファイルを読んでいない"]}
        ]
    });
    let (url, server) = synthetic_ai(response.clone());
    let config = ai::Config::new(&url, "synthetic-review", key.clone());
    let request = || ai::AiRequest {
        repository: "example/notes".into(),
        commit: COMMIT.into(),
        question: "保存できなかったらどうなる？".into(),
        path: None,
        line: None,
    };
    let result = ai::explain(Some(&config), &store, request()).unwrap();
    let sent = server.join().unwrap();
    assert!(
        sent["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("8:         with open(path, 'w') as f:")
    );
    assert!(
        sent["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("test_candidates")
    );
    assert_eq!(result["status"], "ok");
    assert_eq!(result["test_candidates"][0]["can_use"], true);
    assert_eq!(result["test_candidates"][1]["can_use"], false);
    assert_eq!(result["source"]["commit"], COMMIT);
    assert_eq!(result["context_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(result["response_sha256"].as_str().unwrap().len(), 64);
    let provenance: Value =
        serde_json::from_str(result["test_candidates"][0]["provenance"].as_str().unwrap()).unwrap();
    assert_eq!(provenance["question"], "保存できなかったらどうなる？");
    assert_eq!(
        provenance["candidate"]["unknowns"][0],
        "後からの閲覧は未確認"
    );
    assert_eq!(provenance["response_sha256"], result["response_sha256"]);
    assert_eq!(serde_json::to_value(load(&case).unwrap()).unwrap(), before);

    let mut oversized = response;
    oversized["unknowns"] = json!(vec!["未確認".repeat(400); 16]);
    let (url, server) = synthetic_ai(oversized.clone());
    let result = ai::explain(
        Some(&ai::Config::new(&url, "synthetic-review", key.clone())),
        &store,
        request(),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["unknowns"], oversized["unknowns"]);
    assert_eq!(result["test_candidates"][0]["can_use"], false);
    assert!(result["test_candidates"][0].get("provenance").is_none());
    assert!(
        result["test_candidates"][0]["unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("保存上限")
    );

    let (url, server) =
        synthetic_ai(json!({"answer":"壊れた応答", "claims":[null], "unknowns":[]}));
    let failed = ai::explain(
        Some(&ai::Config::new(&url, "synthetic-review", key)),
        &store,
        request(),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(failed["status"], "failed");
    assert!(failed.get("test_candidates").is_none());
    assert_eq!(serde_json::to_value(load(&case).unwrap()).unwrap(), before);
}

#[test]
fn native_system_diagram_generation_persists_auditable_models_and_reopens_without_ai() {
    use mugiurushi::system_diagram::{self, Request};
    let sandbox = Sandbox::new();
    let base = sandbox.0.join("diagrams");
    fs::create_dir_all(base.join("example/record-system")).unwrap();
    let snapshot: Value =
        serde_json::from_str(include_str!("fixtures/reader/system.json")).unwrap();
    let commit = snapshot["commit"].as_str().unwrap();
    fs::write(
        base.join(format!("example/record-system/{commit}.json")),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let store = Store::new(base.clone()).unwrap();
    let case = sandbox.case();
    let before = serde_json::to_value(load(&case).unwrap()).unwrap();
    let response: Value =
        serde_json::from_str(include_str!("fixtures/reader/system-ai.json")).unwrap();
    let (url, server) = synthetic_ai(response);
    let settings_path = sandbox.0.join("ai.json");
    ai::initialize_connection(&settings_path).unwrap();
    fs::write(
        &settings_path,
        serde_json::to_vec(&json!({
            "base_url":url, "model":"synthetic-architecture", "api_key":"synthetic-private-key"
        }))
        .unwrap(),
    )
    .unwrap();
    let config = ai::Config::load_connection(&settings_path)
        .unwrap()
        .unwrap();
    assert!(
        !ai::status(Some(&config))
            .to_string()
            .contains("synthetic-private-key")
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&settings_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let request = || Request {
        repository: "example/record-system".into(),
        commit: commit.into(),
        refresh: false,
        revision: None,
    };
    let generated = system_diagram::generate(Some(&config), &store, request()).unwrap();
    let sent = server.join().unwrap();
    assert!(
        sent["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("SOURCE EXCERPTS")
    );
    assert!(!sent.to_string().contains("synthetic-private-key"));
    assert_eq!(generated["result"]["generator"], "gitdiagram-inspired-ai");
    assert_eq!(generated["references"]["unverified"], json!([]));
    assert_eq!(generated["cached"], false);
    let reopened = system_diagram::generate(None, &Store::new(base).unwrap(), request()).unwrap();
    assert_eq!(reopened["cached"], true);
    assert_eq!(reopened["result"], generated["result"]);
    assert_eq!(
        serde_json::to_value(load(&case).unwrap()).unwrap(),
        before,
        "生成は期待や実行結果を保存しない"
    );
    let failed = system_diagram::generate(
        None,
        &store,
        Request {
            refresh: true,
            ..request()
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        system_diagram::generate(None, &store, request()).unwrap()["result"],
        generated["result"]
    );
}

#[test]
fn operator_connection_rejects_unsafe_hosts_and_does_not_echo_secrets() {
    let sandbox = Sandbox::new();
    let path = sandbox.0.join("ai.json");
    ai::initialize_connection(&path).unwrap();
    let settings = |url: &str| {
        json!({
            "base_url":url, "model":"synthetic", "api_key":"synthetic-private-key"
        })
    };
    for url in [
        "http://remote.example.invalid/v1",
        "https://key:secret@api.example.invalid/v1",
        "https://api.example.invalid/v1?key=secret",
    ] {
        fs::write(&path, serde_json::to_vec(&settings(url)).unwrap()).unwrap();
        let error = ai::Config::load_connection(&path)
            .err()
            .unwrap()
            .to_string();
        assert!(!error.contains("secret"));
        assert!(!error.contains("synthetic-private-key"));
    }
    fs::write(&path, r#"{"synthetic-private-key":"misplaced"}"#).unwrap();
    let error = ai::Config::load_connection(&path)
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("synthetic-private-key"));
    let valid = serde_json::to_vec(&settings("https://api.example.invalid/v1/")).unwrap();
    fs::write(&path, &valid).unwrap();
    assert!(
        ai::initialize_connection(&path).is_err(),
        "既存の運営者設定を初期化で上書きしない"
    );
    assert_eq!(fs::read(&path).unwrap(), valid);
    let config = ai::Config::load_connection(&path).unwrap().unwrap();
    assert_eq!(ai::status(Some(&config))["host"], "api.example.invalid");
}

#[test]
fn operator_cli_initializes_once_and_requires_complete_server_settings() {
    use std::process::Command;
    let sandbox = Sandbox::new();
    let cli = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mugiurushi"));
        for name in ["EEL_AI_BASE_URL", "EEL_AI_MODEL", "EEL_AI_API_KEY_FILE"] {
            command.env_remove(name);
        }
        command
    };
    let init = cli()
        .args(["ai", "init", "--data-dir"])
        .arg(&sandbox.0)
        .output()
        .unwrap();
    assert!(init.status.success());
    let path = sandbox.0.join("ai.json");
    let original = fs::read(&path).unwrap();
    let again = cli()
        .args(["ai", "init", "--data-dir"])
        .arg(&sandbox.0)
        .output()
        .unwrap();
    assert!(!again.status.success());
    assert_eq!(fs::read(&path).unwrap(), original);
    let incomplete = cli()
        .args(["ai", "test", "--data-dir"])
        .arg(&sandbox.0)
        .env("EEL_AI_MODEL", "synthetic-model")
        .output()
        .unwrap();
    assert!(!incomplete.status.success());
    assert!(String::from_utf8_lossy(&incomplete.stderr).contains("すべて指定"));
    // 完全な起動時設定はファイルより優先する。壊れたファイルの秘密を表示しない。
    fs::write(&path, "malformed-synthetic-private-key").unwrap();
    let key = sandbox.0.join("key");
    fs::write(&key, "synthetic-private-key").unwrap();
    let (url, server) = synthetic_ai(json!({"connected":true}));
    let tested = cli()
        .args(["ai", "test", "--data-dir"])
        .arg(&sandbox.0)
        .env("EEL_AI_BASE_URL", url)
        .env("EEL_AI_MODEL", "synthetic-model")
        .env("EEL_AI_API_KEY_FILE", &key)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&tested.stdout);
    let stderr = String::from_utf8_lossy(&tested.stderr);
    assert!(tested.status.success(), "{stderr}");
    assert!(stdout.contains("指定したモデルから応答"));
    assert!(!stdout.contains("private-key") && !stderr.contains("private-key"));
    let sent = server.join().unwrap();
    assert_eq!(sent["model"], "synthetic-model");
    assert!(!sent.to_string().contains("private-key"));
}
