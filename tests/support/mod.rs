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
            trial_checks: vec!["readback".into()],
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
