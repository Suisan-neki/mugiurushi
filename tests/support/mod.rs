#![allow(dead_code)]
//! 汎用ケースの試験用データ。対象アプリの実行結果とは扱わない。
use mugiurushi::{
    execution::{Criterion, ObservationOutcome},
    model::*,
    recording::new_run_directory,
    reported::*,
    workspace::*,
};
use std::{collections::BTreeMap, path::Path};
pub fn workspace() -> Workspace {
    let mut w = empty_workspace("fixture".into(), "合成の確認事例".into());
    w.context.insert("environment".into(), "test".into());
    w.checks = vec![
        Check {
            id: "save".into(),
            version: 1,
            text: "記録を読み出せる".into(),
            origin: "合成の条件".into(),
            source: None,
            observation: Some(Criterion {
                assertion: "readback".into(),
                subject: "record-1".into(),
            }),
            rule: Rule::ReportedAssertion,
            dependencies: vec!["report-save".into(), "environment".into()],
            questions: vec![],
            aspects: vec![],
            link_settings: vec![],
        },
        Check {
            id: "later-view".into(),
            version: 1,
            text: "担当者が必要なときに閲覧できる".into(),
            origin: "未確認の合成条件".into(),
            source: None,
            observation: None,
            rule: Rule::ManualOnly,
            dependencies: vec![],
            questions: vec![],
            aspects: vec![],
            link_settings: vec![],
        },
    ];
    w
}
pub fn trace(outcomes: &[ObservationOutcome]) -> ReportedTrace {
    let (dir, correlation) =
        new_run_directory(&std::env::temp_dir().join("eel-reported-fixtures")).unwrap();
    std::fs::remove_dir(dir).unwrap();
    ReportedTrace {
        schema_version: 1,
        correlation: correlation.clone(),
        producer: "試験用の報告元".into(),
        origin: "合成記録。対象アプリの実行は未実施".into(),
        completed: true,
        source: None,
        context: BTreeMap::new(),
        observations: outcomes
            .iter()
            .enumerate()
            .map(|(i, outcome)| ReportedObservation {
                sequence: i as u64 + 1,
                correlation: correlation.clone(),
                assertion: "readback".into(),
                subject: "record-1".into(),
                outcome: outcome.clone(),
                detail: "試験用の報告".into(),
                source: None,
            })
            .collect(),
    }
}
pub fn import(path: &Path, trace: ReportedTrace) -> WorkspaceView {
    let w = load(path).unwrap();
    let check = &w.checks[0];
    change(
        path,
        ChangeRequest {
            expected_revision: w.revision,
            actor: "合成の確認者".into(),
            reason: "報告の取り込み".into(),
            action: Change::ImportReported {
                condition: Condition {
                    id: check.id.clone(),
                    version: check.version,
                    text: check.text.clone(),
                    origin: check.origin.clone(),
                    kind: ConditionKind::Custom,
                },
                trace,
            },
        },
    )
    .unwrap()
}
/// 「記録を読み出せる」の一つの部分を、報告観測 `readback` に対応づける宣言。
pub fn readback_aspects(check_id: &str) -> Change {
    Change::SetAspects {
        check_id: check_id.into(),
        aspects: vec![mugiurushi::coverage::Aspect {
            id: "readback-part".into(),
            text: "保存した記録を読み出せる".into(),
            origin: "合成の記入者".into(),
            observation: Some(mugiurushi::coverage::ObservationKind::ReportedObservation),
            trial_checks: vec![mugiurushi::coverage::TrialLink {
                id: "readback".into(),
                fingerprint: mugiurushi::coverage::criterion_fingerprint(&Criterion {
                    assertion: "readback".into(),
                    subject: "record-1".into(),
                }),
                label: "報告観測 readback".into(),
                definition: None,
                basis: None,
            }],
        }],
    }
}
pub fn import_pass(path: &Path) -> WorkspaceView {
    import(path, trace(&[ObservationOutcome::Pass]))
}

// ローカル実行の省略記法は試験専用。本体のAPIでは実行方式を指定する。
pub fn execute(
    repository_root: &Path,
    output: &Path,
    recipe: mugiurushi::execution::Recipe,
    condition: Condition,
    previous_run: Option<String>,
) -> std::io::Result<RunResult> {
    mugiurushi::execution::execute_with_mode(
        repository_root,
        output,
        recipe,
        condition,
        previous_run,
        mugiurushi::execution::ExecutionMode::Local,
    )
}

pub fn run_external(
    directory: &Path,
    repository_root: &Path,
    recipe: mugiurushi::execution::Recipe,
    actor: String,
    reason: String,
) -> std::io::Result<WorkspaceView> {
    mugiurushi::workspace::run_external_mode(
        directory,
        repository_root,
        recipe,
        mugiurushi::execution::ExecutionMode::Local,
        actor,
        reason,
    )
}

/// 事例を作り、実行ファイルの `case execute` を起動して返す。終了は待たない。
/// 実行ファイルへ終了の合図を送る試験で使う。
pub fn spawn_case_execute(
    directory: &Path,
    repository_root: &Path,
    recipe: &mugiurushi::execution::Recipe,
) -> std::process::Child {
    use std::process::{Command, Stdio};
    let binary = env!("CARGO_BIN_EXE_mugiurushi");
    let cli = |args: &[&str]| {
        let output = Command::new(binary)
            .args(args)
            .output()
            .expect("実行ファイル");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("JSONの出力")
    };
    std::fs::create_dir_all(directory).unwrap();
    let case = directory.join("case");
    let write = |name: &str, value: serde_json::Value| {
        let path = directory.join(name);
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        path
    };
    let spec = write(
        "case-spec.json",
        serde_json::json!({"schema_version":1,"revision":1,"id":"signal","title":"終了の合図の試験",
            "statements":[],"context":{},"evidence":[],"decisions":[],"history":[],
            "checks":[{"id":recipe.check_id,"version":1,"text":"架空の条件","origin":"試験",
                "source":recipe.source,"rule":"manual_only","dependencies":[],"questions":[]}]}),
    );
    let view = cli(&[
        "case",
        "init",
        &case.to_string_lossy(),
        "--file",
        &spec.to_string_lossy(),
    ]);
    let configure = write(
        "configure.json",
        serde_json::json!({"expected_revision":view["workspace"]["revision"],"actor":"合成の試験担当",
            "reason":"実行前に観測名と対象を保存",
            "action":{"op":"configure_observation","check_id":recipe.check_id,"criterion":recipe.criterion}}),
    );
    cli(&[
        "case",
        "apply",
        &case.to_string_lossy(),
        &configure.to_string_lossy(),
    ]);
    let file = write("recipe.json", serde_json::to_value(recipe).unwrap());
    Command::new(binary)
        .args(["case", "execute"])
        .arg(&case)
        .arg(&file)
        .arg("--repository-root")
        .arg(repository_root)
        .args([
            "--mode",
            "local",
            "--actor",
            "合成の試験担当",
            "--reason",
            "終了の合図の試験",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("実行ファイルの起動")
}

/// プロセスへ合図を送る。
pub fn send_signal(pid: u32, signal: &str) {
    let status = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} {pid}");
}

/// プロセスが動いているか。
pub fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 子の終了を、上限の時間まで待つ。終了コードと、標準エラーの内容を返す。
pub fn wait_within(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> (Option<i32>, String) {
    use std::io::Read;
    let until = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            panic!("実行ファイルが {limit:?} 以内に終わらなかった");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (status.code(), stderr)
}
