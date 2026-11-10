//! ダイジェスト付きの公式イメージで、試験専用コンテナの起動・HTTP操作・故障・掃除を確かめる。
//! Dockerのエンジンがない環境では省略し、成功とは書かない。
mod support;
use mugiurushi::{container::*, execution::*, model::*, recording::*, trial::*, workspace::*};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use support::execute;

const NGINX: &str =
    "nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591";

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}

fn fixture() -> (std::path::PathBuf, std::path::PathBuf, String) {
    let directory = new_run_directory(&std::env::temp_dir().join("evidence-containers"))
        .expect("dir")
        .0;
    let root = directory.join("repo");
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Synthetic test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/web.git",
        ],
    );
    fs::write(
        root.join("README.md"),
        "架空の対象。コンテナは公式イメージをダイジェストで固定する。\n",
    )
    .unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "synthetic"]);
    let commit = git(&root, &["rev-parse", "HEAD"]);
    (directory, root, commit)
}

fn nginx(name: &str, role: ServiceRole) -> Service {
    Service {
        name: name.into(),
        image: NGINX.into(),
        role,
        expose: 80,
        env: Default::default(),
        volumes: vec![],
        web_build: None,
        engine: Default::default(),
        command: vec![],
        ready: ReadyCheck::Http {
            path: "/".into(),
            tls: false,
        },
        ready_timeout_ms: 20000,
    }
}

fn recipe(
    commit: &str,
    services: Vec<Service>,
    fault: TrialFault,
    checks: Vec<TrialCheck>,
) -> Recipe {
    let http_service = services
        .iter()
        .find(|s| s.role == ServiceRole::App)
        .expect("app")
        .name
        .clone();
    Recipe {
        schema_version: 1,
        check_id: "web".into(),
        source: CodeReference {
            repository: "example/web".into(),
            commit: commit.into(),
            path: "README.md".into(),
            line: 1,
            end_line: 1,
        },
        runtime: Runtime::Http,
        entrypoint: "/".into(),
        args: vec![],
        input: serde_json::json!({}),
        criterion: Criterion {
            assertion: "eel.trial".into(),
            subject: "web".into(),
        },
        timeout_ms: 30000,
        trial: Some(TrialSpec {
            schema_version: 1,
            recovery: None,
            fault,
            storage: None,
            readback: None,
            checks,
        }),
        launch: Default::default(),
        python: None,
        server: None,
        containers: Some(ContainerSpec {
            services,
            egress: Default::default(),
            relay: None,
        }),
        browser: None,
        http: Some(HttpOp {
            service: http_service,
            method: "GET".into(),
            path: "/".into(),
            body_file: None,
            content_type: None,
        }),
    }
}

fn check(
    id: &str,
    scope: &str,
    method: CheckMethod,
    expect: Expectation,
    text: &str,
) -> TrialCheck {
    TrialCheck {
        id: id.into(),
        scope: scope.into(),
        method,
        expect,
        text: if text.is_empty() {
            None
        } else {
            Some(text.into())
        },
        target: None,
        relation: None,
    }
}

/// 試行名の接頭辞と対象の名前から、起動中の対象のコンテナ名を探す。試験の並列実行で連番が変わる。
fn container_named(trial: &str, service: &str) -> String {
    let output = Command::new("docker")
        .args(["ps", "--format", "{{.Names}}", "--filter"])
        .arg(format!("name=eel-{trial}-"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|name| name.ends_with(&format!("-{service}")))
        .unwrap_or_else(|| panic!("{service} のコンテナが見つかりません"))
        .to_owned()
}

fn docker_available() -> bool {
    match engine() {
        Ok(_) => true,
        Err(error) => {
            eprintln!("Dockerがないためコンテナ試験を省略: {error}");
            false
        }
    }
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn blocked_egress_keeps_the_target_from_reaching_outside() {
    if !docker_available() {
        return;
    }
    let spec = ContainerSpec {
        services: vec![nginx("web", ServiceRole::App)],
        egress: Egress::Blocked,
        relay: None,
    };
    let mut session = Session::start("egress-blocked", &spec).unwrap();
    assert!(session.all_ready());
    let port = session.port_of("web").unwrap();
    // ホストの127.0.0.1からはゲートウェイ経由で届く。
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
    let container = container_named("egress-blocked", "web");
    let container = container.as_str();
    // 対象のコンテナから外部のIPへ接続できない。
    let outside = Command::new("docker")
        .args([
            "exec",
            container,
            "wget",
            "-q",
            "-T",
            "3",
            "-O",
            "-",
            "http://1.1.1.1/",
        ])
        .output()
        .unwrap();
    assert!(!outside.status.success(), "遮断したはずが外部へ接続できた");
    // 権限を絞っている。
    let caps = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{.HostConfig.CapDrop}} {{.HostConfig.SecurityOpt}}",
            container,
        ])
        .output()
        .unwrap();
    let caps = String::from_utf8_lossy(&caps.stdout);
    assert!(
        caps.contains("ALL") && caps.contains("no-new-privileges"),
        "{caps}"
    );
    let phase = session.snapshot();
    let host = phase
        .traffic
        .iter()
        .find(|t| t.from == HOST_CLIENT && t.to == "web")
        .expect("ホストから対象への接続が記録されていない");
    assert!(host.connections >= 1, "{host:?}");
    assert_eq!(phase.egress, "blocked");
    assert!(phase.egress_confirmed);
    let leftovers = session.cleanup().unwrap();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn traffic_between_targets_and_failures_are_recorded() {
    if !docker_available() {
        return;
    }
    let spec = ContainerSpec {
        services: vec![
            nginx("web", ServiceRole::App),
            nginx("back", ServiceRole::Other),
        ],
        egress: Egress::Blocked,
        relay: None,
    };
    let mut session = Session::start("traffic", &spec).unwrap();
    assert!(session.all_ready());
    let web = container_named("traffic", "web");
    let web = web.as_str();
    let back = container_named("traffic", "back");
    let fetch = |url: &str| {
        Command::new("docker")
            .args(["exec", web, "wget", "-q", "-T", "3", "-O", "-", url])
            .output()
            .unwrap()
            .status
            .success()
    };
    // 対象どうしの通信は中継を通って届く。
    assert!(fetch("http://back/"));
    // 接続先が止まると失敗し、その失敗も記録される。
    Command::new("docker")
        .args(["stop", "-t", "1", &back])
        .output()
        .unwrap();
    assert!(!fetch("http://back/"));
    std::thread::sleep(std::time::Duration::from_secs(3));
    let phase = session.snapshot();
    let peer = phase
        .traffic
        .iter()
        .find(|t| t.from == "web" && t.to == "back")
        .unwrap_or_else(|| panic!("対象どうしの接続が記録されていない: {:?}", phase.traffic));
    assert!(peer.connections >= 2, "{peer:?}");
    // 止まった接続先への接続は、失敗として閉じるか、閉じないまま残る。どちらも「成功」には数えない。
    assert!(peer.failed + peer.unfinished >= 1, "{peer:?}");
    assert!(peer.bytes_to_client > 0, "{peer:?}");
    assert!(phase.traffic_note.contains("中継を通った"));
    assert!(session.cleanup().unwrap().is_empty());
}

#[test]
fn tag_without_digest_is_rejected() {
    let mut spec = ContainerSpec {
        services: vec![nginx("web", ServiceRole::App)],
        egress: Default::default(),
        relay: None,
    };
    spec.services[0].image = "nginx:1.27.4-alpine".into();
    assert!(validate_spec(&spec).is_err());
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn nginx_http_is_satisfied() {
    if !docker_available() {
        return;
    }
    let (directory, root, commit) = fixture();
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe(
            &commit,
            vec![nginx("web", ServiceRole::App)],
            TrialFault::Normal,
            vec![
                check(
                    "status",
                    "受付",
                    CheckMethod::ExitCode,
                    Expectation::Zero,
                    "",
                ),
                check(
                    "welcome",
                    "応答",
                    CheckMethod::Stdout,
                    Expectation::Contains,
                    "Welcome to nginx",
                ),
                check(
                    "log",
                    "ログ",
                    CheckMethod::ContainerLog,
                    Expectation::Contains,
                    "Configuration complete",
                ),
            ],
        ),
        Condition {
            id: "web".into(),
            version: 1,
            text: "試験用のnginxが応答する".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    assert_eq!(
        result.assessment.verdict,
        Verdict::Satisfied,
        "{}",
        result.assessment.reason
    );
    let trace: ExecutionTrace =
        serde_json::from_str(&fs::read_to_string(result.directory.join("execution.json")).unwrap())
            .unwrap();
    let phase = trace.containers.expect("containers");
    assert!(phase.leftovers.is_empty(), "{:?}", phase.leftovers);
    assert!(phase.services[0].ready);
    assert!(phase.services[0].digest.contains("sha256:"));
    assert!(!phase.services[0].running_after_stop);
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn stopping_the_database_container_is_effective() {
    if !docker_available() {
        return;
    }
    let (directory, root, commit) = fixture();
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe(
            &commit,
            vec![
                nginx("web", ServiceRole::App),
                nginx("db", ServiceRole::Database),
            ],
            TrialFault::DatabaseStopped,
            vec![
                check(
                    "welcome",
                    "応答",
                    CheckMethod::Stdout,
                    Expectation::Contains,
                    "Welcome to nginx",
                ),
                check(
                    "log",
                    "ログ",
                    CheckMethod::ContainerLog,
                    Expectation::Contains,
                    "Configuration complete",
                ),
            ],
        ),
        Condition {
            id: "web".into(),
            version: 1,
            text: "データベースを止めても、止めたことを記録する".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    let trace: ExecutionTrace =
        serde_json::from_str(&fs::read_to_string(result.directory.join("execution.json")).unwrap())
            .unwrap();
    let trial = trace.trial.expect("trial");
    assert!(trial.fault_effective, "{}", trial.fault_applied);
    let db = trial
        .containers
        .as_ref()
        .unwrap()
        .services
        .iter()
        .find(|s| s.name == "db")
        .unwrap();
    assert!(!db.running_after_stop);
    assert!(trial.containers.as_ref().unwrap().leftovers.is_empty());
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn disconnecting_the_database_is_effective() {
    if !docker_available() {
        return;
    }
    let (directory, root, commit) = fixture();
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe(
            &commit,
            vec![
                nginx("web", ServiceRole::App),
                nginx("db", ServiceRole::Database),
            ],
            TrialFault::DatabaseUnreachable,
            vec![check(
                "welcome",
                "応答",
                CheckMethod::Stdout,
                Expectation::Contains,
                "Welcome to nginx",
            )],
        ),
        Condition {
            id: "web".into(),
            version: 1,
            text: "データベースとの通信を切ったことを記録する".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    let trace: ExecutionTrace =
        serde_json::from_str(&fs::read_to_string(result.directory.join("execution.json")).unwrap())
            .unwrap();
    assert!(trace.trial.as_ref().unwrap().fault_effective);
}

/// 故障の後に解除して、復旧後の状態を読み直す。解除の実行と、解除後の準備を記録する。
fn recovery_after(fault: TrialFault, database_volume: bool) -> TrialOutcome {
    let (directory, root, commit) = fixture();
    let mut db = nginx("db", ServiceRole::Database);
    if database_volume {
        db.volumes = vec![VolumeMount {
            name: "data".into(),
            path: "/data".into(),
        }];
    }
    let mut recipe = recipe(
        &commit,
        vec![nginx("web", ServiceRole::App), db],
        fault,
        vec![check(
            "welcome",
            "応答",
            CheckMethod::Stdout,
            Expectation::Contains,
            "Welcome to nginx",
        )],
    );
    recipe.trial.as_mut().unwrap().recovery = Some(Recovery {
        ready_timeout_ms: 30_000,
    });
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe,
        Condition {
            id: "web".into(),
            version: 1,
            text: "故障を解除した後の状態を記録する".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    let trace: ExecutionTrace =
        serde_json::from_str(&fs::read_to_string(result.directory.join("execution.json")).unwrap())
            .unwrap();
    assert!(
        trace
            .trial
            .as_ref()
            .unwrap()
            .containers
            .as_ref()
            .unwrap()
            .leftovers
            .is_empty()
    );
    trace.trial.expect("trial")
}

fn assert_recovered(trial: &TrialOutcome) {
    assert!(trial.fault_effective, "{}", trial.fault_applied);
    let recovery = trial.recovery.as_ref().expect("復旧の記録");
    assert!(
        recovery.attempted && recovery.effective,
        "{}",
        recovery.applied
    );
    assert!(
        recovery.services.iter().all(|s| s.ready),
        "{:?}",
        recovery.services
    );
    assert!(
        recovery.limits.contains("再実行"),
        "分からないことを記録している"
    );
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn stopped_database_comes_back_after_recovery() {
    if !docker_available() {
        return;
    }
    assert_recovered(&recovery_after(TrialFault::DatabaseStopped, false));
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn disconnected_database_is_reachable_again_after_recovery() {
    if !docker_available() {
        return;
    }
    assert_recovered(&recovery_after(TrialFault::DatabaseUnreachable, false));
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn read_only_volume_is_writable_again_after_recovery() {
    if !docker_available() {
        return;
    }
    assert_recovered(&recovery_after(TrialFault::VolumeReadOnly, true));
}

const ORTHANC: &str = "orthancteam/orthanc:26.9.1@sha256:d2705f2c56547e55ce9bc2250fd22d05d326493b3f3d9fa191ecb7db8ca6b5a2";
const OPENEMR: &str = "openemr/openemr:latest@sha256:f0bfbc17fae13503ab64bbd271f74d80dd4a0786f5a8d0683841aeef4e159700";
const MARIADB: &str =
    "mariadb:12.3.3@sha256:2bdff1534a7e569fecaf4b10b66ad0d410806e382d9db29ba305448addda97c4";

fn sample_dicom(root: &Path) -> bool {
    let src = PathBuf::from(format!(
        "{}/.cache/pynetdicom/pynetdicom/tests/dicom_files/CTImageStorage.dcm",
        std::env::var("HOME").unwrap_or_default()
    ));
    if !src.is_file() {
        eprintln!("試験用DICOMがないため Orthanc の登録試験を省略");
        return false;
    }
    fs::copy(src, root.join("sample.dcm")).unwrap();
    Command::new("git")
        .args(["-C", root.to_str().unwrap(), "add", "sample.dcm"])
        .status()
        .ok();
    git(root, &["commit", "-qm", "sample"]);
    true
}

#[test]
#[ignore = "Orthancのイメージ取得が必要"]
fn orthanc_stores_the_sample_dicom() {
    if !docker_available() {
        return;
    }
    let (directory, root, _) = fixture();
    if !sample_dicom(&root) {
        return;
    }
    let commit = git(&root, &["rev-parse", "HEAD"]);
    let mut env = std::collections::BTreeMap::new();
    env.insert("DICOM_WEB_PLUGIN_ENABLED".into(), "true".into());
    env.insert("ORTHANC__REMOTE_ACCESS_ALLOWED".into(), "true".into());
    env.insert("ORTHANC__AUTHENTICATION_ENABLED".into(), "false".into());
    let mut recipe = recipe(
        &commit,
        vec![Service {
            name: "orthanc".into(),
            image: ORTHANC.into(),
            role: ServiceRole::App,
            expose: 8042,
            env,
            volumes: vec![],
            web_build: None,
            engine: Default::default(),
            command: vec![],
            ready: ReadyCheck::Http {
                path: "/system".into(),
                tls: false,
            },
            ready_timeout_ms: 60000,
        }],
        TrialFault::Normal,
        vec![
            check(
                "status",
                "受付",
                CheckMethod::ExitCode,
                Expectation::Zero,
                "",
            ),
            check(
                "stored",
                "書き込み",
                CheckMethod::Stdout,
                Expectation::Contains,
                "ParentStudy",
            ),
        ],
    );
    recipe.http = Some(HttpOp {
        service: "orthanc".into(),
        method: "POST".into(),
        path: "/instances".into(),
        body_file: Some("sample.dcm".into()),
        content_type: Some("application/dicom".into()),
    });
    recipe.entrypoint = "/instances".into();
    recipe.timeout_ms = 60000;
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe,
        Condition {
            id: "web".into(),
            version: 1,
            text: "試験用のDICOMをOrthancへ登録できる".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    assert_eq!(
        result.assessment.verdict,
        Verdict::Satisfied,
        "{}",
        result.assessment.reason
    );
}

#[test]
#[ignore = "OpenEMRとMariaDBのイメージ取得と数分の起動が必要"]
fn openemr_becomes_ready_with_pinned_images() {
    if !docker_available() {
        return;
    }
    let (directory, root, commit) = fixture();
    let mut db_env = std::collections::BTreeMap::new();
    db_env.insert("MYSQL_ROOT_PASSWORD".into(), "eel-root-trial".into());
    db_env.insert("MARIADB_AUTO_UPGRADE".into(), "1".into());
    let mut app_env = std::collections::BTreeMap::new();
    app_env.insert("MYSQL_HOST".into(), "mysql".into());
    app_env.insert("MYSQL_ROOT_PASS".into(), "eel-root-trial".into());
    app_env.insert("MYSQL_USER".into(), "openemr".into());
    app_env.insert("MYSQL_PASS".into(), "eel-openemr-trial".into());
    app_env.insert("OE_USER".into(), "eel-operator".into());
    app_env.insert("OE_PASS".into(), "eel-pass-trial".into());
    let mut recipe = recipe(
        &commit,
        vec![
            Service {
                name: "mysql".into(),
                image: MARIADB.into(),
                role: ServiceRole::Database,
                expose: 3306,
                env: db_env,
                volumes: vec![VolumeMount {
                    name: "db".into(),
                    path: "/var/lib/mysql".into(),
                }],
                web_build: None,
                engine: Default::default(),
                command: vec!["mariadbd".into(), "--character-set-server=utf8mb4".into()],
                ready: ReadyCheck::Exec {
                    args: vec![
                        "/usr/local/bin/healthcheck.sh".into(),
                        "--su-mysql".into(),
                        "--connect".into(),
                        "--innodb_initialized".into(),
                    ],
                },
                ready_timeout_ms: 120000,
            },
            Service {
                name: "openemr".into(),
                image: OPENEMR.into(),
                role: ServiceRole::App,
                expose: 443,
                env: app_env,
                volumes: vec![],
                web_build: None,
                engine: Default::default(),
                command: vec![],
                ready: ReadyCheck::Http {
                    path: "/meta/health/readyz".into(),
                    tls: true,
                },
                ready_timeout_ms: 240000,
            },
        ],
        TrialFault::Normal,
        vec![
            check(
                "status",
                "受付",
                CheckMethod::ExitCode,
                Expectation::Zero,
                "",
            ),
            check(
                "ready",
                "応答",
                CheckMethod::Stdout,
                Expectation::Contains,
                "HTTP 200",
            ),
            check(
                "db",
                "保存先",
                CheckMethod::DatabaseQuery,
                Expectation::Contains,
                "SELECT 1",
            ),
        ],
    );
    recipe.http = Some(HttpOp {
        service: "openemr".into(),
        method: "GET".into(),
        path: "/meta/health/readyz".into(),
        body_file: None,
        content_type: None,
    });
    recipe.timeout_ms = 300000;
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe,
        Condition {
            id: "web".into(),
            version: 1,
            text: "固定したイメージでOpenEMRが準備できる".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    assert_eq!(
        result.assessment.verdict,
        Verdict::Satisfied,
        "{}",
        result.assessment.reason
    );
    let trace: ExecutionTrace =
        serde_json::from_str(&fs::read_to_string(result.directory.join("execution.json")).unwrap())
            .unwrap();
    let phase = trace.containers.expect("containers");
    // 実際のOpenEMRからMariaDBへの接続が、中継を通って記録されている。
    let to_db = phase
        .traffic
        .iter()
        .find(|t| t.from == "openemr" && t.to == "mysql")
        .unwrap_or_else(|| {
            panic!(
                "OpenEMRからDBへの接続が記録されていない: {:?}",
                phase.traffic
            )
        });
    assert!(
        to_db.connections >= 1 && to_db.bytes_to_client > 0,
        "{to_db:?}"
    );
    assert!(phase.egress_confirmed);
}

#[test]
#[ignore = "CIのPlaywright導入後にDockerとChromiumで実行"]
fn browser_observes_nginx_and_preserves_screenshots() {
    assert!(engine().is_ok(), "この明示試験にはDockerが必要");
    let (directory, root, commit) = fixture();
    let browsers: serde_json::Value = serde_json::from_str(
        &fs::read_to_string("node_modules/playwright-core/browsers.json").unwrap(),
    )
    .unwrap();
    let version = browsers["browsers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "chromium")
        .unwrap()["browserVersion"]
        .as_str()
        .unwrap();
    let mut trial_check = check(
        "display",
        "表示",
        CheckMethod::BrowserText,
        Expectation::Contains,
        "Welcome to nginx",
    );
    trial_check.target = Some("welcome".into());
    let mut recipe = recipe(
        &commit,
        vec![nginx("web", ServiceRole::App)],
        TrialFault::Normal,
        vec![trial_check],
    );
    recipe.runtime = Runtime::Browser;
    recipe.http = None;
    recipe.browser = Some(serde_json::from_value(serde_json::json!({"service":"web","tls":false,"start_path":"/","expected_version":version,"actions":[{"id":"welcome","action":"read_text","target":{"locator":{"by":"role","role":"heading","name":"Welcome to nginx!"}}}]})).unwrap());
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe,
        Condition {
            id: "web".into(),
            version: 1,
            text: "画面の見出しを読む".into(),
            origin: "合成の接続試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    let trace: ExecutionTrace = read_json(&result.directory.join("execution.json")).unwrap();
    let report = trace.containers.as_ref().unwrap().browser.as_ref().unwrap();
    assert!(report.completed, "{report:?}");
    assert_eq!(
        trace.trial.as_ref().unwrap().results[0].observed,
        Observed::Pass
    );
    assert!(
        result
            .directory
            .join(report.steps[0].screenshot.as_ref().unwrap())
            .is_file()
    );
    assert!(trace.containers.as_ref().unwrap().leftovers.is_empty());
    validate_trace(&trace, &result.manifest).unwrap();
    // 手順ごとの画面の構造は、基盤が検査してから別のファイルへ保存し、記録からは参照だけを持つ。
    assert_eq!(
        report.screens.len(),
        report.steps.len(),
        "{}",
        report.screens_note
    );
    assert!(
        report.screens_note.contains("記録した"),
        "{}",
        report.screens_note
    );
    let browser_directory = result.directory.join("browser");
    let screens = mugiurushi::screen::load(&browser_directory)
        .unwrap()
        .expect("画面の記録");
    assert_eq!(screens.run_id, report.run_id);
    assert_eq!(screens.screens[0].sha256, report.screens[0].sha256);
    assert_eq!(screens.screens[0].sha256.len(), 64);
    let first = screens.screens[0]
        .content
        .as_ref()
        .expect("最初の画面の内容");
    assert_eq!(first.path, "/");
    assert_eq!(first.document.as_ref().map(|d| d.status), Some(200));
    // 操作器が書いた検査前のファイルは残さない。
    assert!(
        !browser_directory
            .join(mugiurushi::screen::RAW_FILE)
            .exists()
    );
}

#[test]
#[ignore = "OpenEMRの画面とMariaDBをCIで操作する。Playwrightと試験用の認証ファイルが必要"]
fn openemr_browser_registers_patient_and_reads_back_visit() {
    assert!(engine().is_ok(), "この明示試験にはDockerが必要");
    assert!(
        std::env::var_os("EEL_BROWSER_SECRETS_FILE").is_some(),
        "試験用の認証ファイルが必要"
    );
    let original: Recipe = read_json(Path::new("examples/openemr-browser/recipe.json")).unwrap();
    let root = fetch_commit(
        Path::new("target/browser-sources"),
        &original.source.repository,
        &original.source.commit,
    )
    .unwrap();
    for fault in [TrialFault::Normal, TrialFault::DatabaseStopped] {
        let mut recipe = original.clone();
        recipe.trial.as_mut().unwrap().fault = fault;
        if fault == TrialFault::DatabaseStopped {
            // 患者登録と受診理由の入力までは済ませ、保存ボタンを押す直前にDBを止める。
            let browser = recipe.browser.as_mut().unwrap();
            let save = browser
                .actions
                .iter()
                .position(|s| s.id == "save-visit")
                .unwrap();
            browser.preparation.extend(browser.actions.drain(..save));
            recipe
                .trial
                .as_mut()
                .unwrap()
                .checks
                .retain(|c| c.id.starts_with("visit-"));
        }
        let label = if fault == TrialFault::Normal {
            "normal"
        } else {
            "database-stopped"
        };
        let result = execute(
            &root,
            &Path::new("evaluations/ci-openemr").join(label),
            recipe,
            Condition {
                id: "openemr".into(),
                version: 1,
                text: "架空の患者と受診理由を保存し再表示できる".into(),
                origin: "実OSSの試験用構成".into(),
                kind: ConditionKind::Custom,
            },
            None,
        )
        .unwrap();
        let trace: ExecutionTrace = read_json(&result.directory.join("execution.json")).unwrap();
        // 実際の正常・DB停止の記録を、同じ固定版の構成へ対応づける。
        let case = Path::new("evaluations/ci-openemr").join(label).join("case");
        let mut w = mugiurushi::workspace::empty_workspace(
            "openemr".into(),
            "OpenEMRの構成と実試験".into(),
        );
        w.architecture_models.push(
            serde_json::from_str(include_str!(
                "../examples/openemr-browser/architecture.json"
            ))
            .unwrap(),
        );
        w.checks.push(mugiurushi::workspace::Check {
            id: "openemr".into(),
            version: 1,
            text: result.manifest.condition.text.clone(),
            origin: result.manifest.condition.origin.clone(),
            source: Some(original.source.clone()),
            observation: Some(original.criterion.clone()),
            rule: mugiurushi::workspace::Rule::ObservedAssertion,
            dependencies: vec!["execution-openemr".into()],
            questions: vec![],
            aspects: vec![],
            link_settings: vec![],
        });
        mugiurushi::workspace::initialize(&case, w, "CI", "実試験と構成の対応づけ").unwrap();
        let mut w = mugiurushi::workspace::change(
            &case,
            mugiurushi::workspace::ChangeRequest {
                expected_revision: 1,
                actor: "CI".into(),
                reason: "実際の試行を取り込む".into(),
                action: mugiurushi::workspace::Change::Import {
                    check_id: "openemr".into(),
                    run_directory: result.directory.to_string_lossy().into_owned(),
                    use_run_context: true,
                },
            },
        )
        .unwrap()
        .workspace;
        for check in &trace.recipe.trial.as_ref().unwrap().checks {
            let object = match check.method {
                CheckMethod::BrowserText => "browser",
                CheckMethod::BrowserResponse => "https",
                CheckMethod::DatabaseRows => "db",
                _ => unreachable!(),
            };
            w = mugiurushi::workspace::change(
                &case,
                mugiurushi::workspace::ChangeRequest {
                    expected_revision: w.revision,
                    actor: "CI".into(),
                    reason: "観測条件を構成要素へ対応づける".into(),
                    action: mugiurushi::workspace::Change::AddArchitectureLink {
                        link: mugiurushi::architecture::Link {
                            id: check.id.clone(),
                            model_id: "openemr-trial".into(),
                            model_version: 1,
                            object_id: object.into(),
                            target: mugiurushi::architecture::Target::Evidence {
                                evidence_id: w.evidence[0].id.clone(),
                                observation_id: check.id.clone(),
                            },
                        },
                    },
                },
            )
            .unwrap()
            .workspace;
        }
        let links = mugiurushi::architecture::links(&w);
        assert_eq!(
            links.len(),
            trace.recipe.trial.as_ref().unwrap().checks.len()
        );
        assert!(links.iter().all(|l| l.applicable && l.current_model));
        if fault == TrialFault::DatabaseStopped {
            assert!(
                links
                    .iter()
                    .filter(|l| l.link.object_id == "db")
                    .all(|l| l.observed == Some(Observed::NotObserved))
            );
        }
        mugiurushi::backup::validate(&mugiurushi::backup::snapshot(&case).unwrap()).unwrap();
        let phase = trace.containers.as_ref().unwrap();
        let browser = phase.browser.as_ref().expect("ブラウザ記録");
        println!("{}",serde_json::to_string(&serde_json::json!({"condition":label,"directory":result.directory,"browser":browser,"database_reads":phase.database_reads,"results":trace.trial.as_ref().unwrap().results,"containers":phase.services.iter().map(|s|(&s.name,s.ready,&s.detail)).collect::<Vec<_>>()})).unwrap());
        assert!(browser.prepared, "{browser:?}");
        assert!(phase.leftovers.is_empty(), "試験用の資源が残った");
        if fault == TrialFault::Normal {
            assert!(browser.completed, "{browser:?}");
            assert!(
                browser.responses.iter().any(|r| r.watch == "visit-reload"
                    && r.request_step.as_deref() == Some("refresh-visit")
                    && r.status == 200),
                "タブ更新によるサーバーからの再読込を確認できない: {:?}",
                browser.responses
            );
            assert!(
                trace
                    .trial
                    .as_ref()
                    .unwrap()
                    .results
                    .iter()
                    .all(|r| r.observed == Observed::Pass),
                "{:?}",
                trace.trial.as_ref().unwrap().results
            );
            assert_eq!(result.assessment.verdict, Verdict::Satisfied);
        } else {
            assert!(trace.trial.as_ref().unwrap().fault_effective);
            assert!(!browser.completed);
            assert!(phase.database_reads.iter().all(|r| !r.performed));
            assert!(
                trace
                    .trial
                    .as_ref()
                    .unwrap()
                    .results
                    .iter()
                    .filter(|r| r.id.ends_with("-row"))
                    .all(|r| r.observed == Observed::NotObserved)
            );
            // HTTPの失敗を観測した場合は条件を満たさない。DBの読取不能は別に未観測として残す。
            assert_ne!(result.assessment.verdict, Verdict::Satisfied);
        }
    }
}

/// コンテナが標準エラーに書いた値も、ログの判定に入る（`docker logs`は対象の標準エラーを自身の標準エラーへ出す）。
#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn container_log_checks_include_the_targets_standard_error() {
    if !docker_available() {
        return;
    }
    let (directory, root, commit) = fixture();
    let mut web = nginx("web", ServiceRole::App);
    web.command = vec![
        "sh".into(),
        "-c".into(),
        "echo EEL-STDERR-MARK >&2; exec nginx -g 'daemon off;'".into(),
    ];
    let mut absent = check(
        "absent",
        "標準エラーの値",
        CheckMethod::ContainerLog,
        Expectation::NotContains,
        "EEL-STDERR-MARK",
    );
    absent.target = Some("web".into());
    let result = execute(
        &root,
        &directory.join("runs"),
        recipe(&commit, vec![web], TrialFault::Normal, vec![absent]),
        Condition {
            id: "web".into(),
            version: 1,
            text: "標準エラーのログを読む".into(),
            origin: "試験".into(),
            kind: ConditionKind::Custom,
        },
        None,
    )
    .unwrap();
    let trace: ExecutionTrace =
        serde_json::from_str(&fs::read_to_string(result.directory.join("execution.json")).unwrap())
            .unwrap();
    let outcome = trace.trial.expect("trial");
    assert_eq!(
        outcome.results[0].observed,
        Observed::Fail,
        "{}",
        outcome.results[0].detail
    );
    let service = &trace.containers.expect("containers").services[0];
    assert!(service.log_excerpt.contains("EEL-STDERR-MARK"));
    assert!(
        service.log_note.contains("最後まで読んだ"),
        "{}",
        service.log_note
    );
}

const POSTGRES: &str =
    "postgres:17-alpine@sha256:3540c0362c809df084ffac0ae69fe794515f9de92692f02178bcdce0ef0302ac";

fn postgres(engine: mugiurushi::database::DatabaseEngine) -> ContainerSpec {
    let mut env = std::collections::BTreeMap::new();
    env.insert("POSTGRES_USER".into(), "clinic".into());
    env.insert("POSTGRES_PASSWORD".into(), "eel-pg-trial".into());
    env.insert("POSTGRES_DB".into(), "records".into());
    ContainerSpec {
        services: vec![Service {
            name: "db".into(),
            image: POSTGRES.into(),
            role: ServiceRole::Database,
            expose: 5432,
            env,
            volumes: vec![],
            web_build: None,
            engine,
            command: vec![],
            ready: ReadyCheck::Exec {
                args: ["pg_isready", "-U", "clinic", "-d", "records"]
                    .map(String::from)
                    .to_vec(),
            },
            ready_timeout_ms: 60000,
        }],
        egress: Egress::Blocked,
        relay: None,
    }
}

/// 対象アプリの代わりに、試験の側でSQLを実行する。基盤の読み出しとは別の経路。
fn psql(container: &str, sql: &str) {
    let output = Command::new("docker")
        .args([
            "exec",
            "--env",
            "PGPASSWORD=eel-pg-trial",
            container,
            "psql",
            "-U",
            "clinic",
            "-d",
            "records",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            sql,
        ])
        .output()
        .expect("docker exec");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn visit_probe(
    match_column: &str,
    match_value: Option<&str>,
) -> mugiurushi::browser::DatabaseProbe {
    mugiurushi::browser::DatabaseProbe {
        id: "visit".into(),
        service: "db".into(),
        database: "records".into(),
        table: "visit".into(),
        match_column: match_column.into(),
        columns: vec!["reason".into(), "patient".into(), "note".into()],
        match_value: match_value.map(String::from),
        before_actions: false,
    }
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn postgres_rows_are_read_when_the_engine_is_declared() {
    use mugiurushi::database::DatabaseEngine;
    if !docker_available() {
        return;
    }
    let mut session = Session::start("pg-read", &postgres(DatabaseEngine::Postgres)).unwrap();
    assert!(session.all_ready());
    let container = container_named("pg-read", "db");
    // 引用符と日本語を含む架空の値。SQLの文字列として解釈されないことも確かめる。
    let value = "eel-test-pg'; DROP TABLE visit;-- 行";
    psql(
        &container,
        "CREATE TABLE visit (id serial PRIMARY KEY, reason text, patient integer, note text)",
    );
    psql(
        &container,
        &format!("INSERT INTO visit (reason, patient) VALUES ($eel${value}$eel$, 7)"),
    );
    let read = session.read_database(&visit_probe("reason", None), value);
    assert!(read.performed, "{}", read.detail);
    assert_eq!(read.exit_code, Some(0), "{}", read.detail);
    // 列はタブ区切り、NULLはMariaDBと同じ表記で返る。
    assert_eq!(read.text, format!("{value}\t7\tNULL\n"));
    // 一致する行がない場合は、読取は成功で0行。
    let absent = session.read_database(&visit_probe("reason", None), "eel-test-absent");
    assert!(absent.performed);
    assert_eq!((absent.exit_code, absent.text.as_str()), (Some(0), ""));
    // 文字列でない列も、文字列として照合できる。
    let by_number = session.read_database(&visit_probe("patient", Some("7")), value);
    assert_eq!(by_number.exit_code, Some(0), "{}", by_number.detail);
    assert!(by_number.text.starts_with(value));
    // 存在しない表は読取の失敗で、0行とは区別される。
    let mut missing_table = visit_probe("reason", None);
    missing_table.table = "no_such_table".into();
    let failed = session.read_database(&missing_table, value);
    assert!(failed.performed);
    assert_ne!(failed.exit_code, Some(0));
    assert!(failed.text.is_empty());
    assert!(
        session
            .query_database()
            .is_some_and(|q| q.contains("応答は 1")),
        "{:?}",
        session.query_database()
    );
    // 故障の操作はDBの種類によらない。
    assert!(session.stop_database().unwrap());
    let stopped = session.read_database(&visit_probe("reason", None), value);
    assert!(!stopped.performed);
    assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn a_postgres_container_without_the_engine_setting_is_not_read_as_zero_rows() {
    use mugiurushi::database::DatabaseEngine;
    if !docker_available() {
        return;
    }
    // 種類を指定しない設定はMariaDBとして読む。PostgreSQLのコンテナにはそのクライアントがない。
    let mut session = Session::start("pg-default", &postgres(DatabaseEngine::default())).unwrap();
    assert!(session.all_ready());
    let container = container_named("pg-default", "db");
    psql(
        &container,
        "CREATE TABLE visit (reason text, patient integer, note text)",
    );
    psql(
        &container,
        "INSERT INTO visit (reason) VALUES ('eel-test-pg')",
    );
    let read = session.read_database(&visit_probe("reason", None), "eel-test-pg");
    assert_ne!(read.exit_code, Some(0), "{}", read.detail);
    assert!(read.text.is_empty());
    assert!(read.detail.contains("行の不存在とは扱わない") || read.detail.contains("起動できない"));
    assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());

    // 読み出しに対応していない種類として宣言した場合は、照会そのものを行わない。
    let mut session = Session::start("pg-other", &postgres(DatabaseEngine::Other)).unwrap();
    let read = session.read_database(&visit_probe("reason", None), "eel-test-pg");
    assert!(!read.performed);
    assert_eq!(read.exit_code, None);
    assert!(read.detail.contains("対応していない"), "{}", read.detail);
    assert!(
        session
            .query_database()
            .is_some_and(|q| q.contains("対応していない"))
    );
    assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());
}

fn tracked(slot: &str, value: &str) -> mugiurushi::tracking::TrackedValue {
    mugiurushi::tracking::TrackedValue {
        slot: slot.into(),
        value: value.into(),
        step: None,
        template: "{{test_value}}".into(),
    }
}

/// 探索の結果を、表・列・値の出所・含む行の数・等しい行の数の組にする。
fn found(scan: &mugiurushi::tracking::DatabaseScan) -> Vec<(String, String, String, u64, u64)> {
    let mut hits: Vec<_> = scan
        .hits
        .iter()
        .map(|h| {
            (
                h.table.clone(),
                h.column.clone(),
                h.slot.clone(),
                h.rows,
                h.exact_rows,
            )
        })
        .collect();
    hits.sort();
    hits
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn postgres_scan_finds_where_a_value_went_without_being_told_the_schema() {
    use mugiurushi::database::DatabaseEngine;
    if !docker_available() {
        return;
    }
    let mut session = Session::start("pg-scan", &postgres(DatabaseEngine::Postgres)).unwrap();
    assert!(session.all_ready());
    let container = container_named("pg-scan", "db");
    let values = [
        tracked("test_value", "EEL-TEST-SCAN-1"),
        tracked("note", "所見 EEL-TEST-SCAN-1-02"),
    ];
    psql(
        &container,
        "CREATE TABLE visit (id serial PRIMARY KEY, reason text, note varchar(200), amount integer);\
         CREATE TABLE audit (entry text);\
         CREATE TABLE empty_table (x text);\
         CREATE TABLE \"Odd Name\" (x text)",
    );
    // 操作の前の状態では、どこにも値がない。
    let before = session.scan_database(&values);
    assert!(before.performed, "{}", before.detail);
    assert!(before.hits.is_empty());
    assert_eq!(before.engine, DatabaseEngine::Postgres);
    assert_eq!(
        (before.service.as_str(), before.database.as_str()),
        ("db", "records")
    );
    assert!(before.tables_scanned >= 3 && before.columns_scanned >= 4);
    // 識別子に空白のある表は調べず、調べなかったことを残す。
    assert!(
        before.skipped.iter().any(|s| s.contains("Odd Name")),
        "{:?}",
        before.skipped
    );

    psql(
        &container,
        "INSERT INTO visit (reason, note, amount) VALUES ('EEL-TEST-SCAN-1', '所見 EEL-TEST-SCAN-1-02', 7);\
         INSERT INTO audit (entry) VALUES ('INSERT note=所見 EEL-TEST-SCAN-1-02 by clerk'), ('unrelated')",
    );
    let after = session.scan_database(&values);
    assert!(after.performed && !after.truncated, "{}", after.detail);
    assert_eq!(
        found(&after),
        [
            // 記録の中に値を含むだけの列は、等しい行が0件になる。
            ("audit".into(), "entry".into(), "note".into(), 1, 0),
            ("audit".into(), "entry".into(), "test_value".into(), 1, 0),
            ("visit".into(), "note".into(), "note".into(), 1, 1),
            ("visit".into(), "note".into(), "test_value".into(), 1, 0),
            ("visit".into(), "reason".into(), "test_value".into(), 1, 1),
        ]
    );
    assert_eq!(
        mugiurushi::tracking::new_hits(Some(&before), &after).len(),
        5
    );
    // DBを止めた後は探索を実行できず、値がなかったとは記録しない。
    assert!(session.stop_database().unwrap());
    let stopped = session.scan_database(&values);
    assert!(!stopped.performed && stopped.hits.is_empty());
    assert!(stopped.detail.contains("停止"), "{}", stopped.detail);
    assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn mariadb_scan_finds_where_a_value_went_without_being_told_the_schema() {
    if !docker_available() {
        return;
    }
    let mut env = std::collections::BTreeMap::new();
    env.insert("MYSQL_ROOT_PASSWORD".into(), "eel-root-trial".into());
    let spec = ContainerSpec {
        services: vec![Service {
            name: "db".into(),
            image: MARIADB.into(),
            role: ServiceRole::Database,
            expose: 3306,
            env,
            volumes: vec![],
            web_build: None,
            engine: Default::default(),
            command: vec![],
            ready: ReadyCheck::Exec {
                args: [
                    "/usr/local/bin/healthcheck.sh",
                    "--su-mysql",
                    "--connect",
                    "--innodb_initialized",
                ]
                .map(String::from)
                .to_vec(),
            },
            ready_timeout_ms: 120000,
        }],
        egress: Egress::Blocked,
        relay: None,
    };
    let mut session = Session::start("maria-scan", &spec).unwrap();
    assert!(session.all_ready());
    let container = container_named("maria-scan", "db");
    let sql = |statement: &str| {
        let output = Command::new("docker")
            .args([
                "exec",
                "--env",
                "MYSQL_PWD=eel-root-trial",
                &container,
                "mariadb",
                "--user=root",
                "-e",
                statement,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    sql("CREATE DATABASE clinic CHARACTER SET utf8mb4; \
         CREATE TABLE clinic.visit (id INT AUTO_INCREMENT PRIMARY KEY, reason VARCHAR(200), note TEXT, amount INT); \
         CREATE TABLE clinic.audit (entry LONGTEXT); \
         CREATE TABLE clinic.legacy (memo VARCHAR(200)) CHARACTER SET latin1");
    let values = [
        tracked("test_value", "EEL-TEST-SCAN-1"),
        tracked("note", "所見 EEL-TEST-SCAN-1-02"),
    ];
    let before = session.scan_database(&values);
    assert!(before.performed, "{}", before.detail);
    assert!(before.hits.is_empty(), "{:?}", before.hits);
    sql(
        "INSERT INTO clinic.visit (reason, note, amount) VALUES ('EEL-TEST-SCAN-1', '所見 EEL-TEST-SCAN-1-02', 7); \
         INSERT INTO clinic.audit (entry) VALUES ('INSERT note=所見 EEL-TEST-SCAN-1-02 by clerk'); \
         INSERT INTO clinic.legacy (memo) VALUES ('EEL-TEST-SCAN-1')",
    );
    let after = session.scan_database(&values);
    assert!(after.performed && !after.truncated, "{}", after.detail);
    assert_eq!(
        found(&after),
        [
            ("audit".into(), "entry".into(), "note".into(), 1, 0),
            ("audit".into(), "entry".into(), "test_value".into(), 1, 0),
            // 文字コードが異なる表でも、英数字の値は見つかる。
            ("legacy".into(), "memo".into(), "test_value".into(), 1, 1),
            ("visit".into(), "note".into(), "note".into(), 1, 1),
            ("visit".into(), "note".into(), "test_value".into(), 1, 0),
            ("visit".into(), "reason".into(), "test_value".into(), 1, 1),
        ],
        "調べなかった表: {:?}",
        after.skipped
    );
    assert!(after.hits.iter().all(|h| h.schema == "clinic"));
    assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());
}

fn mariadb_with_relay(cut: Option<mugiurushi::relay::CutPoint>) -> ContainerSpec {
    let mut env = std::collections::BTreeMap::new();
    env.insert("MYSQL_ROOT_PASSWORD".into(), "eel-root-trial".into());
    ContainerSpec {
        services: vec![Service {
            name: "db".into(),
            image: MARIADB.into(),
            role: ServiceRole::Database,
            expose: 3306,
            env,
            volumes: vec![],
            web_build: None,
            engine: Default::default(),
            command: vec![],
            ready: ReadyCheck::Exec {
                args: [
                    "/usr/local/bin/healthcheck.sh",
                    "--su-mysql",
                    "--connect",
                    "--innodb_initialized",
                ]
                .map(String::from)
                .to_vec(),
            },
            ready_timeout_ms: 120000,
        }],
        egress: Egress::Blocked,
        relay: Some(RelaySetting {
            service: "db".into(),
            protocol: mugiurushi::relay::Protocol::Mysql,
            cut,
        }),
    }
}

/// DBのコンテナの中のクライアントでSQLを実行する。`through_relay` のときは、対象と同じ名前解決で中継を通る。
fn maria(container: &str, through_relay: bool, statement: &str) -> std::process::Output {
    let mut command = Command::new("docker");
    command.args([
        "exec",
        "--env",
        "MYSQL_PWD=eel-root-trial",
        container,
        "mariadb",
        "--user=root",
    ]);
    if through_relay {
        command.args(["--host=db", "--port=3306", "--protocol=tcp", "--skip-ssl"]);
    }
    command.args(["--batch", "--skip-column-names", "-e", statement]);
    command.output().expect("docker exec")
}

fn use_built_binary_as_relay() {
    // 試験の実行ファイルではなく、ビルドした基盤の実行ファイルを中継として動かす。
    // SAFETY: この試験ファイルの中で、同じ値を設定するだけである。
    unsafe { std::env::set_var("EEL_RELAY_BINARY", env!("CARGO_BIN_EXE_mugiurushi")) };
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn relay_records_what_the_target_sent_to_the_database() {
    use mugiurushi::relay::StatementClass;
    if !docker_available() {
        return;
    }
    use_built_binary_as_relay();
    let values = vec![mugiurushi::relay::Tracked {
        slot: "note".into(),
        value: "EEL-TEST-RELAY-1".into(),
    }];
    let mut session =
        Session::start_tracking("relay-rec", &mariadb_with_relay(None), values).unwrap();
    assert!(session.all_ready());
    let container = container_named("relay-rec", "db");
    // 基盤が段階を知らせる前の文は、数えない。対象の起動時の初期化にあたる。
    let early = maria(&container, true, "CREATE DATABASE before_marking");
    assert!(
        early.status.success(),
        "{}",
        String::from_utf8_lossy(&early.stderr)
    );
    session.mark_relay_phase("action").unwrap();
    let output = maria(
        &container,
        true,
        "CREATE DATABASE clinic; \
         CREATE TABLE clinic.visit (id INT AUTO_INCREMENT PRIMARY KEY, note TEXT); \
         INSERT INTO clinic.visit (note) VALUES ('EEL-TEST-RELAY-1'); \
         BEGIN; INSERT INTO clinic.visit (note) VALUES ('second'); COMMIT; \
         SELECT COUNT(*) FROM clinic.visit",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "2");
    let phase = session.snapshot();
    assert!(
        phase.statements_note.contains("記録した"),
        "{}",
        phase.statements_note
    );
    // クライアントが自分で送る文もあるので、試験で送った文を種類と表で探す。
    let sent: Vec<_> = phase
        .statements
        .iter()
        .filter(|s| {
            s.table.as_deref() == Some("visit")
                || matches!(
                    s.class,
                    StatementClass::Begin | StatementClass::Commit | StatementClass::Ddl
                )
        })
        .collect();
    assert!(phase.statements.iter().all(|s| s.phase == "action"));
    // 読み出しは記録に残さず、種類ごとの数にだけ入る。
    assert!(
        phase
            .statements
            .iter()
            .all(|s| s.class != StatementClass::Select)
    );
    assert!(
        phase.statement_totals["select"] >= 1,
        "{:?}",
        phase.statement_totals
    );
    assert_eq!(
        phase.statement_totals["ddl"], 2,
        "段階を知らせる前の文は数えない"
    );
    assert_eq!(phase.statement_totals["insert"], 2);
    let view: Vec<_> = sent
        .iter()
        .map(|s| {
            (
                s.class,
                s.reply.as_deref(),
                s.in_transaction,
                s.commit_point,
                s.slots.len(),
            )
        })
        .collect();
    assert_eq!(
        view,
        [
            (StatementClass::Ddl, Some("ok"), Some(false), true, 0),
            (StatementClass::Ddl, Some("ok"), Some(false), true, 0),
            // 自動コミットの書き込みは、その応答で確定する。追跡する値を含む文には目印が付く。
            (StatementClass::Insert, Some("ok"), Some(false), true, 1),
            (StatementClass::Begin, Some("ok"), Some(true), false, 0),
            // トランザクションの中の書き込みは、COMMITの応答まで確定しない。
            (StatementClass::Insert, Some("ok"), Some(true), false, 0),
            (StatementClass::Commit, Some("ok"), Some(false), true, 0),
        ],
        "{:?}",
        phase.statements
    );
    // 同じ形の文は、値が違っても同じハッシュになり、回数で区別できる。
    let inserts: Vec<_> = sent
        .iter()
        .filter(|s| s.class == StatementClass::Insert)
        .collect();
    assert_eq!(inserts[0].shape, inserts[1].shape);
    assert_eq!((inserts[0].occurrence, inserts[1].occurrence), (1, 2));
    assert_eq!(
        inserts[0].shape,
        mugiurushi::relay::shape("INSERT INTO clinic.visit (note) VALUES ('anything')")
    );
    // SQLの本文と値は、記録に残らない。
    let text = serde_json::to_string(&phase.statements).unwrap();
    assert!(!text.contains("EEL-TEST-RELAY-1") && !text.contains("second"));
    // 既存の接続の集計も、同じ中継の記録から作れる。
    assert!(
        phase
            .traffic
            .iter()
            .any(|t| t.to == "db" && t.connections >= 1),
        "{:?}",
        phase.traffic
    );
    assert!(
        !phase.traffic_note.contains("読み取れない行"),
        "{}",
        phase.traffic_note
    );
    assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());
}

#[test]
#[ignore = "Dockerと固定イメージを使うため、手動・週次の統合試験で実行"]
fn relay_cuts_at_the_chosen_point_and_the_database_state_depends_on_the_point() {
    use mugiurushi::relay::{CutPhase, CutPoint};
    if !docker_available() {
        return;
    }
    use_built_binary_as_relay();
    let insert = "INSERT INTO clinic.visit (note) VALUES ('cut-here')";
    let shape = mugiurushi::relay::shape(insert);
    // 時点ごとに、対象から見た結果と、DBに残った行の数を確かめる。
    for (label, phase, client_succeeds, rows) in [
        // DBへ送る前に切る。対象は失敗を受け取り、DBには残らない。
        ("before", CutPhase::BeforeForward, false, "0"),
        // DBへ送った後、応答を返す前に切る。対象は失敗を受け取るが、DBには残っている。
        ("no-reply", CutPhase::AfterForwardBeforeReply, false, "1"),
        // 応答を返した直後に切る。対象は成功を受け取り、DBにも残っている。
        ("after", CutPhase::AfterReply, true, "1"),
    ] {
        let trial = format!("relay-cut-{label}");
        let spec = mariadb_with_relay(Some(CutPoint {
            shape: shape.clone(),
            occurrence: 1,
            phase,
        }));
        let mut session = Session::start(&trial, &spec).unwrap();
        assert!(session.all_ready());
        let container = container_named(&trial, "db");
        // 準備は中継を通さない。中継が数える文に入らない。
        let setup = maria(
            &container,
            false,
            "CREATE DATABASE clinic; CREATE TABLE clinic.visit (id INT AUTO_INCREMENT PRIMARY KEY, note TEXT)",
        );
        assert!(
            setup.status.success(),
            "{}",
            String::from_utf8_lossy(&setup.stderr)
        );
        session.mark_relay_phase("action").unwrap();
        let output = maria(&container, true, insert);
        assert_eq!(
            output.status.success(),
            client_succeeds,
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let count = maria(&container, false, "SELECT COUNT(*) FROM clinic.visit");
        assert_eq!(
            String::from_utf8_lossy(&count.stdout).trim(),
            rows,
            "{label}"
        );
        let recorded = session.snapshot();
        let cut: Vec<_> = recorded
            .statements
            .iter()
            .filter(|s| s.cut.is_some())
            .collect();
        assert_eq!(cut.len(), 1, "{label}: {:?}", recorded.statements);
        assert_eq!(cut[0].cut, Some(phase));
        assert_eq!(cut[0].table.as_deref(), Some("visit"));
        // DBへ送る前に切った文には、DBの応答がない。送った後に切った文では、中継はDBの成功の応答を見ている。
        assert_eq!(
            cut[0].reply.as_deref(),
            if phase == CutPhase::BeforeForward {
                None
            } else {
                Some("ok")
            },
            "{label}"
        );
        assert_eq!(session.cleanup().unwrap(), Vec::<String>::new());
    }
}
