//! 確認項目のどの部分を試験が確かめたかの対応づけを、架空の小さな記録アプリで確かめる。
//! 試験の合否（判定条件がすべて通ったか）と、確認項目に答えたかを分ける。
mod support;
use mugiurushi::{
    coverage::{Aspect, AspectStatus, CoverageStatus, LinkDefinition, ObservationKind, TrialLink},
    execution::*,
    model::*,
    recording::*,
    trial::*,
    workspace::*,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const NOTES: &str = r#"import sys, os
def main(argv):
    directory, command = argv[1], argv[2]
    path = os.path.join(directory, "notes")
    if command == "add":
        with open(path, "a") as f:
            f.write(argv[3] + "\n")
        print("saved " + argv[3])
        return 0
    if command == "claim":
        print("saved " + argv[3])
        return 0
    if command == "list":
        with open(path) as f:
            sys.stdout.write(f.read())
        return 0
    return 2
sys.exit(main(sys.argv))
"#;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("gitを起動できる");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    case: PathBuf,
}
impl Fixture {
    /// 相談文は保存先ではなく外部送信に関するもの。試験は保存だけを扱う。
    fn new(check_text: &str) -> Self {
        let directory = new_run_directory(&std::env::temp_dir().join("evidence-coverage-tests"))
            .unwrap()
            .0;
        let root = directory.join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Synthetic test"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/notes.git",
            ],
        );
        fs::write(root.join("notes.py"), NOTES).unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "v1"]);
        let commit = git(&root, &["rev-parse", "HEAD"]);
        let case = directory.join("case");
        let mut workspace = empty_workspace("coverage".into(), "対応づけの合成試験".into());
        workspace.checks.push(Check {
            id: "concern".into(),
            version: 1,
            text: check_text.into(),
            origin: "合成の相談".into(),
            source: Some(CodeReference {
                repository: "example/notes".into(),
                commit: commit.clone(),
                path: "notes.py".into(),
                line: 1,
                end_line: 1,
            }),
            observation: None,
            rule: Rule::ManualOnly,
            dependencies: vec![],
            questions: vec![],
            aspects: vec![],
        });
        initialize(&case, workspace, "合成の記入者", "相談の登録").unwrap();
        let fixture = Self {
            directory,
            root,
            case,
        };
        fixture.apply(Change::ConfigureObservation {
            check_id: "concern".into(),
            criterion: Criterion {
                assertion: "eel.trial".into(),
                subject: "concern".into(),
            },
        });
        fixture
    }
    fn apply(&self, action: Change) -> WorkspaceView {
        change(
            &self.case,
            ChangeRequest {
                expected_revision: load(&self.case).unwrap().revision,
                actor: "合成の記入者".into(),
                reason: "試験".into(),
                action,
            },
        )
        .unwrap()
    }
    fn recipe(&self, command: &str, spec: TrialSpec) -> Recipe {
        let mut recipe = base_recipe(command, &[], spec);
        recipe.source = load(&self.case).unwrap().checks[0].source.clone().unwrap();
        recipe
    }
    fn run(&self, command: &str, spec: TrialSpec) -> WorkspaceView {
        self.run_recipe(self.recipe(command, spec))
    }
    fn run_recipe(&self, recipe: Recipe) -> WorkspaceView {
        support::run_external(
            &self.case,
            &self.root,
            recipe,
            "合成の記入者".into(),
            "保存の試験".into(),
        )
        .unwrap()
    }
    fn release(&self) -> std::io::Result<WorkspaceView> {
        change(
            &self.case,
            ChangeRequest {
                expected_revision: load(&self.case).unwrap().revision,
                actor: "合成の確認者".into(),
                reason: "採用の判断".into(),
                action: Change::Decide {
                    role: Role::Developer,
                    action: DecisionKind::Release,
                    check_ids: vec!["concern".into()],
                    change_reference: None,
                    confidence: None,
                    elapsed_ms: None,
                },
            },
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn check(id: &str, scope: &str, method: CheckMethod, expect: Expectation) -> TrialCheck {
    TrialCheck {
        id: id.into(),
        scope: scope.into(),
        method,
        expect,
        text: Some(TEST_VALUE.into()),
        target: None,
        relation: None,
    }
}
/// 保存の試験: 標準出力の受付表示、基盤による保存先の読み出し、別プロセスでの読み直し。
fn storage_trial(fault: TrialFault) -> TrialSpec {
    TrialSpec {
        schema_version: 1,
        recovery: None,
        fault,
        storage: Some("notes".into()),
        readback: Some(Readback {
            launch: Default::default(),
            entrypoint: "notes.py".into(),
            args: vec![".".into(), "list".into()],
        }),
        checks: vec![
            check(
                "accepted",
                "受付",
                CheckMethod::Stdout,
                Expectation::Contains,
            ),
            check(
                "written",
                "書き込み",
                CheckMethod::StoredFile,
                Expectation::Contains,
            ),
            check(
                "readback",
                "後からの読み出し",
                CheckMethod::ReadbackOutput,
                Expectation::Contains,
            ),
        ],
    }
}
/// 試験の設定。固定版は対応づけの指紋に含まれないため、ここでは仮の値を置く。
fn base_recipe(command: &str, extra_args: &[&str], spec: TrialSpec) -> Recipe {
    let mut args = vec![".".to_owned(), command.to_owned(), TEST_VALUE.to_owned()];
    args.extend(extra_args.iter().map(|s| s.to_string()));
    Recipe {
        schema_version: 1,
        check_id: "concern".into(),
        source: CodeReference {
            repository: "example/notes".into(),
            commit: "a".repeat(40),
            path: "notes.py".into(),
            line: 1,
            end_line: 1,
        },
        runtime: Runtime::Python3,
        entrypoint: "notes.py".into(),
        args,
        input: serde_json::json!({}),
        criterion: Criterion {
            assertion: "eel.trial".into(),
            subject: "concern".into(),
        },
        timeout_ms: 5000,
        trial: Some(spec),
        launch: Default::default(),
        python: None,
        server: None,
        containers: None,
        http: None,
        browser: None,
    }
}
/// 対応づけは、判定条件の内容とその試験の設定（definition）を添えて登録する。基盤が指紋に変える。
fn aspect_for(
    recipe: &Recipe,
    id: &str,
    text: &str,
    kind: Option<ObservationKind>,
    links: &[&str],
) -> Aspect {
    let spec = recipe.trial.as_ref().unwrap();
    Aspect {
        id: id.into(),
        text: text.into(),
        origin: "合成の記入者".into(),
        observation: kind,
        trial_checks: links
            .iter()
            .map(|s| TrialLink {
                id: s.to_string(),
                fingerprint: String::new(),
                label: String::new(),
                definition: spec
                    .checks
                    .iter()
                    .find(|c| c.id == *s)
                    .cloned()
                    .map(|check| LinkDefinition {
                        check,
                        recipe: recipe.clone(),
                    }),
            })
            .collect(),
    }
}
/// 通常時の保存の試験（`add`）への対応づけ。
fn aspect(id: &str, text: &str, kind: Option<ObservationKind>, links: &[&str]) -> Aspect {
    aspect_for(
        &base_recipe("add", &[], storage_trial(TrialFault::Normal)),
        id,
        text,
        kind,
        links,
    )
}
fn status(view: &WorkspaceView, id: &str) -> AspectStatus {
    view.checks[0]
        .coverage
        .aspects
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("{id}"))
        .status
}

/// 相談と無関係な保存試験が通っても、何を確かめたか未登録なら相談は未通過のまま。
#[test]
fn passing_storage_trial_does_not_answer_an_unrelated_concern() {
    let f = Fixture::new("診療データが意図しない外部へ送信されない");
    let v = f.run("add", storage_trial(TrialFault::Normal));
    assert_eq!(v.checks[0].verdict, Verdict::Satisfied, "試験自体は通る");
    assert!(
        !v.checks[0].verification_passed,
        "試験の合否と相談への答えを分ける"
    );
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::NotDeclared);
    assert!(!v.verification_passed);
    assert_eq!(
        f.release().unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(matches!(v.checks[0].next.kind, ActionKind::LinkAspects));

    // 外部送信の部分を登録しても、基盤はその観測に未対応と示し、通過にはならない。
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect(
                "external",
                "試験用ネットワークの外へ送信されない",
                Some(ObservationKind::ExternalDestination),
                &[],
            ),
            aspect(
                "payload",
                "送信内容に診療データが含まれない",
                Some(ObservationKind::NetworkPayload),
                &[],
            ),
            aspect(
                "traffic",
                "接続の集計に外部宛がない",
                Some(ObservationKind::TrafficSummary),
                &[],
            ),
        ],
    });
    assert_eq!(v.checks[0].verdict, Verdict::Satisfied);
    assert!(!v.checks[0].verification_passed);
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::NotCovered);
    assert_eq!(status(&v, "external"), AspectStatus::Unsupported);
    assert_eq!(status(&v, "payload"), AspectStatus::Unsupported);
    assert_eq!(status(&v, "traffic"), AspectStatus::NotJudged);
    assert_eq!(v.checks[0].coverage.remaining.len(), 3);
    assert!(f.release().is_err());
    assert!(
        v.workspace.checks[0].version == 2,
        "対応づけの登録は条件の版を変えない（観測設定で2になった版のまま）"
    );
    assert!(v.checks[0].evidence[0].applicable);
}

/// 相談と試験が対応し、必要な観測が得られた場合だけ通過する。未定・未対応づけの部分は残る。
#[test]
fn declared_parts_pass_only_when_each_linked_check_was_observed() {
    let f = Fixture::new("追加したメモは保存され、後から読み出せる");
    f.run("add", storage_trial(TrialFault::Normal));
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect(
                "stored",
                "保存先に書き込まれる",
                Some(ObservationKind::StoredFile),
                &["written"],
            ),
            aspect(
                "later",
                "後から読み出せる",
                Some(ObservationKind::ReadbackOutput),
                &["readback"],
            ),
        ],
    });
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::Covered);
    assert!(v.checks[0].verification_passed);
    assert!(v.verification_passed);
    assert!(f.release().is_ok());

    // 観測方法が未定の部分、判定条件を選んでいない部分、コード読取だけの部分は観測済みにならない。
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect(
                "stored",
                "保存先に書き込まれる",
                Some(ObservationKind::StoredFile),
                &["written"],
            ),
            aspect("undefined", "保存先の暗号化", None, &[]),
            aspect(
                "unlinked",
                "後から読み出せる",
                Some(ObservationKind::ReadbackOutput),
                &[],
            ),
            aspect(
                "inspected",
                "保存処理がSQLで書く",
                Some(ObservationKind::CodeInspection),
                &[],
            ),
        ],
    });
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::Partial);
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert_eq!(status(&v, "undefined"), AspectStatus::Undefined);
    assert_eq!(status(&v, "unlinked"), AspectStatus::Unlinked);
    assert_eq!(status(&v, "inspected"), AspectStatus::NotJudged);
    assert!(!v.checks[0].verification_passed);
    assert!(f.release().is_err());

    // 判定条件の観測方法が宣言と違う対応づけは、文章が合っていても観測済みにしない。
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect(
            "stored",
            "保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["accepted"],
        )],
    });
    assert_eq!(status(&v, "stored"), AspectStatus::MethodMismatch);
    assert!(!v.checks[0].verification_passed);
}

/// 読み出せなかった（観測なし）と、読み出した結果が期待と異なる（保存されていない）を分ける。
#[test]
fn unreadable_storage_and_missing_record_are_different_statuses() {
    let f = Fixture::new("追加したメモは保存される");
    // 試す状況（読み出しを止める）も試験設定の一部なので、その設定で対応づける。
    let skip = base_recipe("add", &[], storage_trial(TrialFault::SkipStateRead));
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect_for(
                &skip,
                "accepted",
                "受付の表示が出る",
                Some(ObservationKind::Stdout),
                &["accepted"],
            ),
            aspect_for(
                &skip,
                "stored",
                "保存先に書き込まれる",
                Some(ObservationKind::StoredFile),
                &["written"],
            ),
        ],
    });
    let v = f.run("add", storage_trial(TrialFault::SkipStateRead));
    assert_eq!(v.checks[0].verdict, Verdict::Unknown);
    assert_eq!(status(&v, "accepted"), AspectStatus::Observed);
    assert_eq!(status(&v, "stored"), AspectStatus::NotObserved);
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::Partial);

    // 表示は成功と言うが、保存先に記録がない: 表示とDB（保存先）が食い違う。
    // 操作（claim）が変わるので、人がその試験の設定で対応づけ直してから実行する。
    let claim = base_recipe("claim", &[], storage_trial(TrialFault::Normal));
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect_for(
                &claim,
                "accepted",
                "受付の表示が出る",
                Some(ObservationKind::Stdout),
                &["accepted"],
            ),
            aspect_for(
                &claim,
                "stored",
                "保存先に書き込まれる",
                Some(ObservationKind::StoredFile),
                &["written"],
            ),
        ],
    });
    let v = f.run("claim", storage_trial(TrialFault::Normal));
    assert_eq!(v.checks[0].verdict, Verdict::NotSatisfied);
    assert_eq!(status(&v, "accepted"), AspectStatus::Observed);
    assert_eq!(status(&v, "stored"), AspectStatus::Contradicted);
    assert!(!v.checks[0].verification_passed);
    assert!(v.checks[0].coverage.remaining[0].contains("期待と異なる"));
}

/// 試験や版を変えた後、古い試行の観測を現在の答えとして流用しない。
#[test]
fn old_observations_are_not_reused_after_the_trial_or_version_changes() {
    let f = Fixture::new("追加したメモは保存され、後から読み出せる");
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect(
                "stored",
                "保存先に書き込まれる",
                Some(ObservationKind::StoredFile),
                &["written"],
            ),
            aspect(
                "later",
                "後から読み出せる",
                Some(ObservationKind::ReadbackOutput),
                &["readback"],
            ),
        ],
    });
    let v = f.run("add", storage_trial(TrialFault::Normal));
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::Covered);

    // 読み直しを含まない試験へ変えると、前の試行の読み直しは答えに使わない。
    let mut narrower = storage_trial(TrialFault::Normal);
    narrower.readback = None;
    narrower.checks.retain(|c| c.id != "readback");
    let v = f.run("add", narrower.clone());
    assert_eq!(v.checks[0].verdict, Verdict::Satisfied);
    // 読み直しを外すと試験の設定が変わるため、残る判定条件への対応づけも確認し直す。
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);
    assert_eq!(status(&v, "later"), AspectStatus::NotInTrial);
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::NotCovered);
    assert!(!v.checks[0].verification_passed);
    // 残る判定条件だけ現在の設定で選び直す。読み直しの対応づけは保存済みのまま（この試験にはない）。
    let narrow = base_recipe("add", &[], narrower);
    let later = v.workspace.checks[0].aspects[1].clone();
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![
            aspect_for(
                &narrow,
                "stored",
                "保存先に書き込まれる",
                Some(ObservationKind::StoredFile),
                &["written"],
            ),
            later,
        ],
    });
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert_eq!(status(&v, "later"), AspectStatus::NotInTrial);
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::Partial);

    // 参照する版を変えると、試行は適用できず、どの部分も観測済みにしない。
    let mut other = v.workspace.checks[0].source.clone().unwrap();
    other.commit = "b".repeat(40);
    let v = f.apply(Change::BindSource {
        check_id: "concern".into(),
        source: other,
    });
    assert_eq!(v.checks[0].verdict, Verdict::Unknown);
    assert_eq!(status(&v, "stored"), AspectStatus::NoEvidence);
    assert_eq!(status(&v, "later"), AspectStatus::NoEvidence);
    assert_eq!(v.checks[0].coverage.status, CoverageStatus::NotCovered);
}

/// 不正な対応づけは拒否し、事例を変えない。未回答の条件があれば試験が通っても判定しない。
#[test]
fn invalid_links_are_rejected_and_unanswered_conditions_stay_unknown() {
    let f = Fixture::new("追加したメモは保存される");
    let before = load(&f.case).unwrap().revision;
    for aspects in [
        vec![aspect(
            "a",
            "判定に使えない種類へ対応づける",
            Some(ObservationKind::NetworkPayload),
            &["written"],
        )],
        vec![aspect(
            "a",
            "種類が未定なのに対応づける",
            None,
            &["written"],
        )],
        vec![
            aspect("a", "重複", None, &[]),
            aspect("a", "重複", None, &[]),
        ],
        vec![aspect("bad id", "識別子が不正", None, &[])],
        vec![aspect("a", " ", None, &[])],
        vec![aspect(
            "a",
            "内容も指紋もない対応づけ",
            Some(ObservationKind::StoredFile),
            &["nowhere"],
        )],
    ] {
        assert!(
            change(
                &f.case,
                ChangeRequest {
                    expected_revision: before,
                    actor: "合成の記入者".into(),
                    reason: "不正な対応づけ".into(),
                    action: Change::SetAspects {
                        check_id: "concern".into(),
                        aspects,
                    },
                },
            )
            .is_err()
        );
    }
    assert_eq!(load(&f.case).unwrap().revision, before);
    assert!(
        change(
            &f.case,
            ChangeRequest {
                expected_revision: before,
                actor: "合成の記入者".into(),
                reason: "存在しない確認項目".into(),
                action: Change::SetAspects {
                    check_id: "missing".into(),
                    aspects: vec![],
                },
            },
        )
        .is_err()
    );
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect(
            "stored",
            "保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    f.apply(Change::AddQuestion {
        check_id: "concern".into(),
        question: Question {
            id: "where".into(),
            text: "本番の保存先はどこか".into(),
            answer: None,
            origin: None,
        },
    });
    let v = f.run("add", storage_trial(TrialFault::Normal));
    assert_eq!(
        v.checks[0].verdict,
        Verdict::Unknown,
        "条件が未回答なら判定しない"
    );
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert!(!v.checks[0].verification_passed);
    let saved = serde_json::to_value(load(&f.case).unwrap()).unwrap();
    assert_eq!(
        saved["checks"][0]["aspects"][0]["observation"],
        "stored_file"
    );
    let link = &saved["checks"][0]["aspects"][0]["trial_checks"][0];
    assert_eq!(link["id"], "written");
    assert_eq!(link["fingerprint"].as_str().unwrap().len(), 64);
    assert!(link.get("definition").is_none());
}

/// 同じ識別子・同じ観測方法の判定条件でも、対象や期待が変わった試験には前の対応づけを適用しない。
#[test]
fn same_check_id_with_a_different_definition_requires_a_new_link() {
    let f = Fixture::new("追加したメモは保存される");
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect(
            "stored",
            "保存先に試験用の値が書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    let v = f.run("add", storage_trial(TrialFault::Normal));
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert!(v.checks[0].verification_passed);

    // 判定条件 written の識別子と観測方法はそのまま、期待を「含まない」に変えた試験。
    let mut changed = storage_trial(TrialFault::Normal);
    changed.readback = None;
    changed.checks.retain(|c| c.id != "readback");
    changed
        .checks
        .iter_mut()
        .find(|c| c.id == "written")
        .unwrap()
        .expect = Expectation::NotContains;
    let v = f.run("claim", changed);
    assert_eq!(
        v.checks[0].verdict,
        Verdict::Satisfied,
        "変えた試験自体は通る"
    );
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);
    assert!(!v.checks[0].verification_passed);
    assert!(v.checks[0].coverage.remaining[0].contains("内容が違う"));
    assert!(f.release().is_err());

    // 範囲の文だけが変わっても、内容の指紋は一致しない。
    let mut renamed = storage_trial(TrialFault::Normal);
    renamed
        .checks
        .iter_mut()
        .find(|c| c.id == "written")
        .unwrap()
        .scope = "患者Bの書き込み".into();
    let v = f.run("add", renamed);
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);
    let label = &v.checks[0].coverage.aspects[0].trial_checks[0].label;
    assert!(label.contains("書き込み"), "{label}");
}

/// 判定条件の識別子・文面は同じまま、操作対象（引数）だけが変わった試験には前の対応づけを適用しない。
/// 別の部分を保存しても、変わった判定条件への対応づけは人が選び直すまで以前の指紋を保つ。
#[test]
fn changing_only_the_operated_target_requires_relinking_and_saving_other_parts_keeps_old_links() {
    let f = Fixture::new("追加したメモは保存される");
    let patient_a = base_recipe("add", &["patient-a"], storage_trial(TrialFault::Normal));
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect_for(
            &patient_a,
            "stored",
            "患者のメモが保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    let mut run_a = patient_a.clone();
    run_a.source = f.recipe("add", storage_trial(TrialFault::Normal)).source;
    let v = f.run_recipe(run_a);
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert!(v.checks[0].verification_passed);
    let saved_link = v.workspace.checks[0].aspects[0].trial_checks[0].clone();
    assert!(
        saved_link.label.contains("patient-a"),
        "{}",
        saved_link.label
    );

    // 判定条件 written は同じ内容のまま、操作対象を患者Bに変える。
    let mut run_b = base_recipe("add", &["patient-b"], storage_trial(TrialFault::Normal));
    run_b.source = f.recipe("add", storage_trial(TrialFault::Normal)).source;
    let v = f.run_recipe(run_b.clone());
    assert_eq!(v.checks[0].verdict, Verdict::Satisfied, "試験自体は通る");
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);
    assert!(!v.checks[0].verification_passed);
    assert!(f.release().is_err());

    // 別の部分を追加して保存する。既存の対応づけは内容を送らないので、以前の指紋のまま。
    let mut kept = v.workspace.checks[0].aspects.clone();
    kept.push(Aspect {
        id: "note".into(),
        text: "受付の表示が出る".into(),
        origin: "合成の記入者".into(),
        observation: Some(ObservationKind::Stdout),
        trial_checks: vec![],
    });
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: kept,
    });
    assert_eq!(
        v.workspace.checks[0].aspects[0].trial_checks[0].fingerprint, saved_link.fingerprint,
        "別の部分の保存で、変わった判定条件への対応づけを更新しない"
    );
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);
    assert!(!v.checks[0].verification_passed);

    // 人が患者Bの試験の設定で選び直すと、その試行で観測済みになる。
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect_for(
            &run_b,
            "stored",
            "患者のメモが保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert!(v.checks[0].verification_passed);
}
