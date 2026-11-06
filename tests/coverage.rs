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
            link_settings: vec![],
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
                basis: None,
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

/// 操作対象を変えた試行では、結果の画面・APIに、対応づけた時点と試行の引数の違いを示す。
/// 別の部分を保存しても、対応づけた時点の記録は変わらない。
#[test]
fn changed_trial_shows_what_changed_and_keeps_the_recorded_basis_until_relinked() {
    let f = Fixture::new("追加したメモは保存される");
    let source = f.recipe("add", storage_trial(TrialFault::Normal)).source;
    let mut patient_a = base_recipe("add", &["patient-a"], storage_trial(TrialFault::Normal));
    patient_a.source = source.clone();
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect_for(
            &patient_a,
            "stored",
            "患者のメモが保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    let saved = v.workspace.checks[0].aspects[0].trial_checks[0].clone();
    assert!(saved.basis.is_some());
    assert_eq!(v.workspace.checks[0].link_settings.len(), 1);

    let mut patient_b = base_recipe("add", &["patient-b"], storage_trial(TrialFault::Normal));
    patient_b.source = source;
    let v = f.run_recipe(patient_b.clone());
    let aspect = &v.checks[0].coverage.aspects[0];
    assert_eq!(aspect.status, AspectStatus::DefinitionChanged);
    assert_eq!(aspect.link_changes.len(), 1);
    let change = &aspect.link_changes[0];
    assert_eq!(change.state, mugiurushi::coverage::LinkState::Changed);
    assert_eq!(change.changes.len(), 1, "{change:?}");
    assert_eq!(change.changes[0].section, "引数");
    assert!(
        change.changes[0]
            .before
            .as_deref()
            .unwrap()
            .contains("patient-a")
    );
    assert!(
        change.changes[0]
            .after
            .as_deref()
            .unwrap()
            .contains("patient-b")
    );
    assert!(!v.checks[0].verification_passed);
    assert!(f.release().is_err());
    // 確認資料には変わった区分の名前だけを載せ、値（操作対象・入力）は載せない。
    let report = mugiurushi::review_report::html(
        &mugiurushi::review_report::build(load(&f.case).unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        report.contains("対応づけた時点から変わった設定：引数"),
        "{report}"
    );
    let at = report
        .find("patient-b")
        .map(|i| &report[i.saturating_sub(400)..(i + 200).min(report.len())]);
    assert!(at.is_none(), "{at:?}");

    // 画面が実行前に使う比較（同じ処理）でも、同じ違いを示す。
    let request = mugiurushi::coverage::LinkChangesRequest {
        check_id: "concern".into(),
        recipe: patient_b.clone(),
    };
    let listed = mugiurushi::coverage::link_changes(&load(&f.case).unwrap(), &request).unwrap();
    assert_eq!(listed, aspect.link_changes);
    let request = mugiurushi::coverage::LinkChangesRequest {
        check_id: "concern".into(),
        recipe: patient_a.clone(),
    };
    let listed = mugiurushi::coverage::link_changes(&load(&f.case).unwrap(), &request).unwrap();
    assert_eq!(listed[0].state, mugiurushi::coverage::LinkState::Unchanged);

    // 別の部分を足して保存する。既存の対応づけは内容を送らず、利用者が説明を書き換えても保存済みの説明と記録を保つ。
    let mut kept = v.workspace.checks[0].aspects.clone();
    kept[0].trial_checks[0].label = "書き換えた説明".into();
    kept[0].trial_checks[0].basis = None;
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
    assert_eq!(v.workspace.checks[0].aspects[0].trial_checks[0], saved);
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);

    // 人が変更を確認して選び直すと、記録も新しい設定に替わり、使われなくなった記録は残さない。
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect_for(
            &patient_b,
            "stored",
            "患者のメモが保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert!(v.checks[0].coverage.aspects[0].link_changes.is_empty());
    assert_eq!(v.workspace.checks[0].link_settings.len(), 1);
    assert_ne!(
        v.workspace.checks[0].link_settings[0].id,
        saved.basis.unwrap().settings
    );
}

/// この機能より前の対応づけ（指紋だけ）は、同じ指紋で実行した試行の記録から時点の設定を復元する。
/// 該当する試行がなければ、比較できないと示す。
#[test]
fn legacy_fingerprint_only_links_use_a_matching_trial_or_say_they_cannot_be_compared() {
    let f = Fixture::new("追加したメモは保存される");
    let source = f.recipe("add", storage_trial(TrialFault::Normal)).source;
    let mut patient_a = base_recipe("add", &["patient-a"], storage_trial(TrialFault::Normal));
    patient_a.source = source.clone();
    let written = patient_a.trial.as_ref().unwrap().checks[1].clone();
    let legacy = |fingerprint: String| Aspect {
        id: "stored".into(),
        text: "患者のメモが保存先に書き込まれる".into(),
        origin: "旧形式の対応づけ".into(),
        observation: Some(ObservationKind::StoredFile),
        trial_checks: vec![TrialLink {
            id: "written".into(),
            fingerprint,
            label: "旧形式の説明".into(),
            definition: None,
            basis: None,
        }],
    };
    let fingerprint = mugiurushi::coverage::trial_check_fingerprint(&written, &patient_a);
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![legacy(fingerprint)],
    });
    assert!(
        v.workspace.checks[0].aspects[0].trial_checks[0]
            .basis
            .is_none()
    );
    let mut patient_b = base_recipe("add", &["patient-b"], storage_trial(TrialFault::Normal));
    patient_b.source = source;
    // 対応づけた時点の設定で実行した記録がまだない。
    let v = f.run_recipe(patient_b.clone());
    let change = &v.checks[0].coverage.aspects[0].link_changes[0];
    assert!(change.basis_source.is_none() && change.changes.is_empty());
    assert!(change.uncomparable[0].contains("保存されていない"));
    // 患者Aの設定で実行した記録があれば、そこから時点の設定を示す（判定には使わない）。
    let v = f.run_recipe(patient_a.clone());
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    let v = f.run_recipe(patient_b);
    assert_eq!(status(&v, "stored"), AspectStatus::DefinitionChanged);
    let change = &v.checks[0].coverage.aspects[0].link_changes[0];
    assert!(
        change
            .basis_source
            .as_deref()
            .unwrap()
            .contains("同じ指紋の判定条件で実行した試行"),
        "{change:?}"
    );
    assert_eq!(change.changes[0].section, "引数");
}

/// 新しい確認項目と一緒に対応づけを送ると、指紋・時点の記録の作成を経ないため拒否する。
#[test]
fn a_new_check_cannot_carry_links() {
    let f = Fixture::new("追加したメモは保存される");
    let mut check = load(&f.case).unwrap().checks[0].clone();
    check.id = "other".into();
    check.aspects = vec![Aspect {
        id: "a".into(),
        text: "合成".into(),
        origin: "合成".into(),
        observation: Some(ObservationKind::StoredFile),
        trial_checks: vec![TrialLink {
            id: "written".into(),
            fingerprint: "a".repeat(64),
            label: "偽の説明".into(),
            definition: None,
            basis: None,
        }],
    }];
    let result = change(
        &f.case,
        ChangeRequest {
            expected_revision: load(&f.case).unwrap().revision,
            actor: "合成の記入者".into(),
            reason: "試験".into(),
            action: Change::AddCheck { check },
        },
    );
    assert!(result.is_err());
}

/// 試験用の偽のAIエージェント（mugiurushi-ai の代わり）。経路ごとに決めた応答を返し、受け取った要求を記録する。
struct FakeAgent {
    url: String,
    token_file: PathBuf,
    received: std::sync::Arc<std::sync::Mutex<Vec<(String, String, serde_json::Value)>>>,
}
impl FakeAgent {
    fn start(directory: &Path, answers: Vec<(&'static str, u16, serde_json::Value)>) -> Self {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let token_file = directory.join("agent-token");
        fs::write(&token_file, "synthetic-agent-token-0123456789\n").unwrap();
        let received = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let log = received.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let (mut length, mut token) = (0, String::new());
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "x-mugiurushi-ai-token" => token = value.trim().to_owned(),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock().unwrap().push((
                    path.clone(),
                    token,
                    serde_json::from_slice(&body).unwrap(),
                ));
                let (status, answer) = answers
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map(|(_, s, a)| (*s, a.to_string()))
                    .unwrap_or((404, "{}".into()));
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
    fn config(&self) -> mugiurushi::agent::Config {
        mugiurushi::agent::Config::new(&self.url, self.token_file.clone()).unwrap()
    }
}

/// 画面と同じく、まず送らずに内容を確かめ、その SHA-256 と架空のデータの確認を添えて送る。
fn propose_confirmed(
    config: &mugiurushi::agent::Config,
    case: &Path,
    request: Option<&str>,
) -> std::io::Result<serde_json::Value> {
    use mugiurushi::agent::{Confirm, ProposeRequest, propose};
    let ask = |confirm| ProposeRequest {
        check_id: "concern".into(),
        request: request.map(str::to_owned),
        confirm,
    };
    let preview = propose(Some(config), load(case).unwrap(), ask(None))?;
    assert_eq!(preview["stage"], "preview");
    let sha = preview["payload_sha256"].as_str().unwrap().to_owned();
    propose(
        Some(config),
        load(case).unwrap(),
        ask(Some(Confirm {
            payload_sha256: sha,
            synthetic_only: true,
        })),
    )
}

fn proposal(text: &str, observation: Option<&str>, links: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "aspect": {"id": format!("ai-{}", text.len()), "text": text, "origin": "mugiurushi-ai の案（試験）",
                   "observation": observation, "trial_checks": links},
        "support": "judged", "rationale": "合成の理由", "unknowns": []
    })
}

/// AIの案を人が選んで追加し、既存の部分と対応づけを保ったまま、既存の判定条件で試験し、麦漆の結果をAIに説明させる。
/// 返す状態は麦漆の値で、AIの文は説明だけ。
#[test]
fn agent_proposals_are_adopted_by_a_person_and_results_are_explained_with_mugiurushi_statuses() {
    let f = Fixture::new("追加したメモは保存され、後から読み出せる");
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect(
            "stored",
            "保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    let existing = v.workspace.checks[0].aspects.clone();
    let agent = FakeAgent::start(
        &f.directory,
        vec![
            (
                "/v1/dialogue/turn",
                200,
                serde_json::json!({
                    "status": "draft", "reply": "読み出しも確かめます", "questions": ["保存先はどこですか"],
                    "aspects": [proposal("別のプロセスで読み出すと同じ内容が出る", Some("readback_output"), serde_json::json!([])),
                                proposal("外部へ送信されない", Some("external_destination"), serde_json::json!([]))],
                    "out_of_scope": [{"text": "本番環境の保存", "reason": "試験環境だけを扱う"}], "model_call": {"response_id": "fake-1"}
                }),
            ),
            (
                "/v1/results/explain",
                200,
                serde_json::json!({
                    "summary": "AIの要約",
                    // AIが状態を書き換えても使わない。
                    "aspects": [{"id": "stored", "explanation": "保存先を読んだ", "status": "unsupported"},
                                {"id": "ai-57", "explanation": "読み出しも確認した"}],
                    "next_steps": [], "model_call": {"response_id": "fake-2"}
                }),
            ),
        ],
    );
    let config = agent.config();
    // 確認だけでは送らない。
    let preview = mugiurushi::agent::propose(
        Some(&config),
        load(&f.case).unwrap(),
        mugiurushi::agent::ProposeRequest {
            check_id: "concern".into(),
            request: None,
            confirm: None,
        },
    )
    .unwrap();
    assert!(
        agent.received.lock().unwrap().is_empty(),
        "確認の段階では送らない"
    );
    assert_eq!(
        preview["payload"]["concern"],
        "追加したメモは保存され、後から読み出せる"
    );
    let shown = preview["payload"].to_string();
    assert!(
        !shown.contains("example/notes") && !shown.contains("notes.py"),
        "対象のリポジトリ・パスは送らない"
    );
    let proposals = propose_confirmed(&config, &f.case, None).unwrap();
    assert_eq!(proposals["status"], "draft");
    assert_eq!(proposals["out_of_scope"][0]["reason"], "試験環境だけを扱う");
    let sent = agent.received.lock().unwrap()[0].clone();
    assert_eq!(sent.0, "/v1/dialogue/turn");
    assert_eq!(sent.1, "synthetic-agent-token-0123456789");
    assert_eq!(
        sent.2["concern"],
        "追加したメモは保存され、後から読み出せる"
    );
    assert_eq!(sent.2["current_aspects"][0]["id"], "stored");
    // 案の取得だけでは確認項目は変わらない。
    assert_eq!(load(&f.case).unwrap().checks[0].aspects, existing);

    // 人が一つ目の案だけを採用し、既存の判定条件 readback に対応づける。既存の部分は送られた形のまま保つ。
    let chosen = &proposals["proposals"][0];
    assert_eq!(chosen["observation"], "readback_output");
    let mut adopted = aspect_for(
        &base_recipe("add", &[], storage_trial(TrialFault::Normal)),
        chosen["id"].as_str().unwrap(),
        chosen["text"].as_str().unwrap(),
        Some(ObservationKind::ReadbackOutput),
        &["readback"],
    );
    adopted.origin = chosen["origin"].as_str().unwrap().into();
    let mut aspects = existing.clone();
    aspects.push(adopted);
    let v = f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects,
    });
    assert_eq!(
        v.workspace.checks[0].aspects[0], existing[0],
        "既存の部分のIDと対応づけを保つ"
    );
    assert_eq!(
        v.workspace.checks[0].aspects.len(),
        2,
        "採用しなかった案は入らない"
    );

    let v = f.run("add", storage_trial(TrialFault::Normal));
    assert_eq!(status(&v, "stored"), AspectStatus::Observed);
    assert!(v.checks[0].verification_passed);

    let explained = mugiurushi::agent::explain(
        Some(&config),
        load(&f.case).unwrap(),
        mugiurushi::agent::ExplainRequest {
            check_id: "concern".into(),
            confirm: None,
        },
    )
    .unwrap();
    assert_eq!(
        explained["stage"], "preview",
        "結果の説明も、まず送らずに内容を返す"
    );
    assert_eq!(agent.received.lock().unwrap().len(), 1);
    let sha = explained["payload_sha256"].as_str().unwrap().to_owned();
    let explained = mugiurushi::agent::explain(
        Some(&config),
        load(&f.case).unwrap(),
        mugiurushi::agent::ExplainRequest {
            check_id: "concern".into(),
            confirm: Some(mugiurushi::agent::Confirm {
                payload_sha256: sha,
                synthetic_only: true,
            }),
        },
    )
    .unwrap();
    let sent = agent.received.lock().unwrap()[1].clone();
    assert_eq!(
        sent.2["check_text"], "追加したメモは保存され、後から読み出せる",
        "確認項目の文は Check から"
    );
    assert_eq!(
        sent.2["coverage"]["aspects"][0]["status"], "observed",
        "状態は CheckView から"
    );
    assert_eq!(
        explained["aspects"][0]["status"], "observed",
        "AIが書き換えた状態は使わない"
    );
    assert_eq!(explained["aspects"][0]["explanation"], "保存先を読んだ");
    assert_eq!(explained["verification_passed"], true);
    let body = serde_json::to_string(
        &agent
            .received
            .lock()
            .unwrap()
            .iter()
            .map(|r| &r.2)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(
        !body.contains("EEL-TEST") && !body.contains("stdout"),
        "実行記録の生ログ・試験値を送らない"
    );
}

/// AIが未設定・失敗・不正な案を返しても、事例は変わらず、既存の検証と採用の判断は今までどおり行える。
#[test]
fn without_a_working_agent_existing_verification_still_works() {
    let f = Fixture::new("追加したメモは保存される");
    f.apply(Change::SetAspects {
        check_id: "concern".into(),
        aspects: vec![aspect(
            "stored",
            "保存先に書き込まれる",
            Some(ObservationKind::StoredFile),
            &["written"],
        )],
    });
    let request = || mugiurushi::agent::ProposeRequest {
        check_id: "concern".into(),
        request: None,
        confirm: None,
    };
    assert!(mugiurushi::agent::propose(None, load(&f.case).unwrap(), request()).is_err());
    let failing = FakeAgent::start(
        &f.directory,
        vec![(
            "/v1/dialogue/turn",
            500,
            serde_json::json!({"detail": "合成の失敗"}),
        )],
    );
    let error = propose_confirmed(&failing.config(), &f.case, None).unwrap_err();
    assert!(error.to_string().contains("HTTP 500"), "{error}");
    for bad in [
        proposal(
            "リンク付き",
            Some("stored_file"),
            serde_json::json!([{"id": "written", "fingerprint": "f"}]),
        ),
        proposal("未知の種類", Some("telepathy"), serde_json::json!([])),
    ] {
        let agent = FakeAgent::start(
            &f.directory,
            vec![(
                "/v1/dialogue/turn",
                200,
                serde_json::json!({"reply": "r", "aspects": [bad], "questions": []}),
            )],
        );
        assert!(propose_confirmed(&agent.config(), &f.case, None).is_err());
    }
    let before = load(&f.case).unwrap().revision;
    let v = f.run("add", storage_trial(TrialFault::Normal));
    assert!(v.checks[0].verification_passed);
    assert!(f.release().is_ok(), "AIなしで採用の判断を記録できる");
    assert!(load(&f.case).unwrap().revision > before);
    assert!(
        mugiurushi::agent::Config::new("http://example.com", f.directory.join("t")).is_err(),
        "外部への http は使わない"
    );
}

/// ガイドラインとの照合。送るのは人が書いた説明だけで、まず送らずに内容を返す。麦漆の試験で確かめられるかは、
/// AIの値ではなく麦漆の観測の種類の一覧で決める。出典の欠けた候補は受け取らない。
#[test]
fn guideline_scan_sends_only_the_written_description_and_uses_mugiurushi_support() {
    use mugiurushi::agent::{Confirm, ScanRequest, scan};
    let f = Fixture::new("追加したメモは保存される");
    let sha = "a".repeat(64);
    let citation = serde_json::json!({"passage_id": "doc:p3:1", "source": "架空の指針", "publisher": "架空の公開元",
        "document": "架空の指針 本編", "version": null, "page": "3", "origin": "https://example.invalid/a.pdf",
        "sha256": sha, "fetched_at": "2026-11-06T00:00:00+00:00", "excerpt": "アクセスの記録を残す"});
    let finding = |method: &str, executable: bool, citations: serde_json::Value| {
        serde_json::json!({
        "status": "candidate", "requirement": "アクセスの記録を残す", "relevance": "likely", "citations": citations,
        "system_refs": [], "unknowns": ["保存期間"],
        // AIの判定可否の値は使わない。
        "verification": {"method": method, "method_label": "AIの表示名", "support": "judged", "executable_check": executable, "how": "記録を読む"}})
    };
    let answer = |findings| {
        serde_json::json!({"status": "candidate", "limits": "照合の候補", "findings": findings,
        "not_covered": ["版の確認"], "sources": [{"source_id": "s", "title": "架空の指針", "status": "used", "status_label": "使った", "documents": []}],
        "model_call": {"response_id": "fake-scan"}})
    };
    let agent = FakeAgent::start(
        &f.directory,
        vec![(
            "/v1/scan",
            200,
            answer(serde_json::json!([
                finding("container_log", true, serde_json::json!([citation.clone()])),
                finding(
                    "code_inspection",
                    true,
                    serde_json::json!([citation.clone()])
                ),
                finding(
                    "document_review",
                    true,
                    serde_json::json!([citation.clone()])
                ),
            ])),
        )],
    );
    let config = agent.config();
    let ask = |summary: &str, confirm| ScanRequest {
        summary: summary.into(),
        confirm,
    };
    assert!(scan(Some(&config), ask("  ", None)).is_err());
    assert!(scan(None, ask("架空の予約システム", None)).is_err());
    let preview = scan(Some(&config), ask("架空の予約システム", None)).unwrap();
    assert_eq!(preview["stage"], "preview");
    assert_eq!(
        preview["payload"],
        serde_json::json!({"system": {"summary": "架空の予約システム", "excerpts": []}}),
        "送るのは書いた説明だけ"
    );
    assert!(
        agent.received.lock().unwrap().is_empty(),
        "確認の段階では送らない"
    );
    let sha256 = preview["payload_sha256"].as_str().unwrap().to_owned();
    assert!(
        scan(
            Some(&config),
            ask(
                "架空の予約システム（変更）",
                Some(Confirm {
                    payload_sha256: sha256.clone(),
                    synthetic_only: true
                })
            )
        )
        .is_err(),
        "確認した内容と違えば送らない"
    );
    let result = scan(
        Some(&config),
        ask(
            "架空の予約システム",
            Some(Confirm {
                payload_sha256: sha256,
                synthetic_only: true,
            }),
        ),
    )
    .unwrap();
    let sent = agent.received.lock().unwrap()[0].clone();
    assert_eq!(sent.0, "/v1/scan");
    assert_eq!(sent.2["system"]["summary"], "架空の予約システム");
    let executable: Vec<_> = result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["verification"]["executable_check"].clone())
        .collect();
    assert_eq!(
        executable,
        [true, false, false],
        "推定・人による確認は試験で確かめられない"
    );
    assert_eq!(
        result["findings"][1]["verification"]["support"],
        "inference"
    );
    assert!(result["findings"][2]["verification"]["support"].is_null());
    assert_eq!(result["findings"][0]["citations"][0]["page"], "3");
    assert_eq!(result["sources"][0]["status"], "used");
    assert_eq!(
        load(&f.case).unwrap().checks[0].aspects,
        vec![],
        "照合は確認項目を変えない"
    );

    for citations in [
        serde_json::json!([]),
        serde_json::json!([{"document": "x", "page": "1", "sha256": "short"}]),
    ] {
        let bad = FakeAgent::start(
            &f.directory,
            vec![(
                "/v1/scan",
                200,
                answer(serde_json::json!([finding(
                    "container_log",
                    true,
                    citations
                )])),
            )],
        );
        let config = bad.config();
        let preview = scan(Some(&config), ask("架空", None)).unwrap();
        let sha = preview["payload_sha256"].as_str().unwrap().to_owned();
        assert!(
            scan(
                Some(&config),
                ask(
                    "架空",
                    Some(Confirm {
                        payload_sha256: sha,
                        synthetic_only: true
                    })
                )
            )
            .is_err(),
            "出典の欠けた候補は受け取らない"
        );
    }
}

/// 新しい流れ：選んだファイルの抜粋を送る照合、ヒアリングの質問と優先度、試験の案の検査、流れの記録。
#[test]
fn guideline_flow_reads_chosen_files_and_validates_plans_and_answers() {
    use mugiurushi::agent::{
        AskRequest, Confirm, FlowSave, FlowScanRequest, PlanFinding, PlanRequest, ask, flow_read,
        flow_save, flow_scan, plan,
    };
    let f = Fixture::new("照合の流れ");
    let commit = "a".repeat(40);
    let snapshots = f.directory.join("snapshots");
    fs::create_dir_all(snapshots.join("example/clinic")).unwrap();
    fs::write(
        snapshots.join(format!("example/clinic/{commit}.json")),
        serde_json::json!({"schema": 1, "fullName": "example/clinic", "reference": commit, "commit": commit,
            "inventory": [{"path": "app/auth.py", "mode": "100644"}, {"path": "README.md", "mode": "100644"}],
            "files": {"app/auth.py": "def login(user, password):\n    return True\n", "README.md": "架空の診療所"}, "omitted": []})
        .to_string(),
    )
    .unwrap();
    let store = mugiurushi::understanding::Store::new(snapshots).unwrap();
    let sha = "b".repeat(64);
    let citation = serde_json::json!({"passage_id": "p:1", "source_id": "provider-security", "document": "架空の指針", "page": "37", "sha256": sha, "origin": "x", "excerpt": "多要素認証"});
    let agent = FakeAgent::start(
        &f.directory,
        vec![
            (
                "/v1/scan",
                200,
                serde_json::json!({"findings": [{"requirement": "二要素認証", "relevance": "likely", "citations": [citation.clone()], "system_refs": ["app/auth.py"],
                "verification": {"method": "browser_text", "how": "ログイン画面を見る"}, "unknowns": [], "patient_data": true, "priority": "high"}],
                "questions": [{"id": "q1", "text": "院外から使いますか", "why": "外部接続の項目に使う", "options": ["はい", "いいえ"]}], "sources": [], "not_covered": [], "model_call": {"response_id": "fake-scan"}}),
            ),
            (
                "/v1/plan",
                200,
                serde_json::json!({"tests": [{"id": "t-1", "name": "ログイン", "kind": "browser", "finding_ids": ["f-1"], "steps": ["ログインする"], "pass": ["2つ目の認証を求められる"], "observation": "browser_text"}],
                "not_planned": [], "model_call": {"response_id": "fake-plan"}}),
            ),
            (
                "/v1/ask",
                200,
                serde_json::json!({"answer": "保存する", "citations": [{"document": "x", "page": "1", "sha256": "short"}]}),
            ),
        ],
    );
    let config = agent.config();
    let request = |files: Vec<&str>, confirm| FlowScanRequest {
        repository: "example/clinic".into(),
        commit: commit.clone(),
        files: files.into_iter().map(str::to_owned).collect(),
        summary: String::new(),
        answers: vec![],
        confirm,
    };
    assert!(
        flow_scan(Some(&config), &store, request(vec![], None)).is_err(),
        "ファイルを選ばない照合は送らない"
    );
    assert!(
        flow_scan(
            Some(&config),
            &store,
            request(vec!["app/auth.py"; 41], None)
        )
        .is_err()
    );
    let preview = flow_scan(Some(&config), &store, request(vec!["app/auth.py"], None)).unwrap();
    assert_eq!(
        preview["payload"]["system"]["excerpts"][0]["ref"],
        "app/auth.py"
    );
    assert!(
        preview["payload"]["system"]["excerpts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("def login"),
        "読取記録のファイルを送る"
    );
    assert!(
        !preview["payload"].to_string().contains("架空の診療所"),
        "選んでいないファイルは送らない"
    );
    assert!(
        agent.received.lock().unwrap().is_empty(),
        "確認の段階では送らない"
    );
    let sha_preview = preview["payload_sha256"].as_str().unwrap().to_owned();
    let scanned = flow_scan(
        Some(&config),
        &store,
        request(
            vec!["app/auth.py"],
            Some(Confirm {
                payload_sha256: sha_preview,
                synthetic_only: true,
            }),
        ),
    )
    .unwrap();
    assert_eq!(scanned["findings"][0]["priority"], "high");
    assert_eq!(scanned["findings"][0]["patient_data"], true);
    assert_eq!(
        scanned["findings"][0]["verification"]["executable_check"],
        true
    );
    assert_eq!(scanned["questions"][0]["id"], "q1");

    let plan_request = |findings: Vec<&str>, confirm| PlanRequest {
        repository: "example/clinic".into(),
        commit: commit.clone(),
        files: vec!["app/auth.py".into()],
        summary: String::new(),
        answers: vec![],
        findings: findings
            .into_iter()
            .map(|id| PlanFinding {
                id: id.into(),
                requirement: "二要素認証".into(),
                how: String::new(),
            })
            .collect(),
        confirm,
    };
    let preview = plan(Some(&config), &store, plan_request(vec!["f-1"], None)).unwrap();
    let sha_plan = preview["payload_sha256"].as_str().unwrap().to_owned();
    let planned = plan(
        Some(&config),
        &store,
        plan_request(
            vec!["f-1"],
            Some(Confirm {
                payload_sha256: sha_plan,
                synthetic_only: true,
            }),
        ),
    )
    .unwrap();
    assert_eq!(
        planned["tests"][0]["finding_ids"],
        serde_json::json!(["f-1"])
    );
    assert_eq!(planned["tests"][0]["observation"], "browser_text");
    // 渡していない候補を含む案は受け取らない（案は f-1 を含むが、渡した候補は f-2 だけ）。
    let preview = plan(Some(&config), &store, plan_request(vec!["f-2"], None)).unwrap();
    let sha_bad = preview["payload_sha256"].as_str().unwrap().to_owned();
    assert!(
        plan(
            Some(&config),
            &store,
            plan_request(
                vec!["f-2"],
                Some(Confirm {
                    payload_sha256: sha_bad,
                    synthetic_only: true
                })
            )
        )
        .is_err()
    );

    let preview = ask(
        Some(&config),
        AskRequest {
            question: "ログは？".into(),
            confirm: None,
        },
    )
    .unwrap();
    let sha_ask = preview["payload_sha256"].as_str().unwrap().to_owned();
    assert!(
        ask(
            Some(&config),
            AskRequest {
                question: "ログは？".into(),
                confirm: Some(Confirm {
                    payload_sha256: sha_ask,
                    synthetic_only: true
                })
            }
        )
        .is_err(),
        "SHA-256のそろわない出典の回答は受け取らない"
    );

    assert_eq!(
        flow_read(&f.directory).unwrap()["records"],
        serde_json::json!({})
    );
    assert!(
        flow_save(
            &f.directory,
            FlowSave {
                repository: "example/clinic".into(),
                commit: "short".into(),
                record: serde_json::json!({})
            }
        )
        .is_err()
    );
    let saved = flow_save(
        &f.directory,
        FlowSave {
            repository: "example/clinic".into(),
            commit: commit.clone(),
            record: serde_json::json!({"files": ["app/auth.py"]}),
        },
    )
    .unwrap();
    assert_eq!(
        saved["records"][format!("example/clinic@{commit}")]["record"]["files"][0],
        "app/auth.py"
    );
    assert_eq!(flow_read(&f.directory).unwrap(), saved);
}
