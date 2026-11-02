//! 確認項目のどの部分を、どの観測で確かめたかの対応づけ。
//! 試験の合否（判定条件がすべて通ったか）とは別に求める。人やAIが述べた対応づけを、
//! 基盤が実行記録の判定条件IDと観測方法に照合し、文章の一致から適否を決めない。
use crate::error::invalid;
use crate::trial::{CheckMethod, Observed};
use crate::workspace::{Check, Evidence};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io};

pub const MAX_ASPECTS: usize = 32;
const MAX_LINKS: usize = 32;

/// 観測の種類。基盤が判定に使える種類と、宣言はできるが基盤が未対応の種類を一つの一覧で持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    ExitCode,
    Stdout,
    Stderr,
    StoredFile,
    ReadbackOutput,
    ServerOutput,
    ContainerLog,
    DatabaseQuery,
    BrowserText,
    BrowserResponse,
    BrowserCanvas,
    ServerHttp,
    DatabaseRows,
    DatabaseRelation,
    /// 対象プログラムが出力した観測（observations.jsonl）。試験案を使わない試行。
    ProgramObservation,
    /// 外部の報告元から取り込んだ観測。基盤は実行していない。
    ReportedObservation,
    /// 中継が記録した接続の集計。記録は残るが、判定条件には使えない。
    TrafficSummary,
    /// 人またはAIがコードを読んだ推定。実行の観測ではない。
    CodeInspection,
    /// 通信の中身（送信した内容そのもの）。基盤は記録していない。
    NetworkPayload,
    /// 操作から個々のDB書き込みまでを対応づける分散トレース。基盤は未対応。
    DistributedTrace,
    /// 試験用ネットワークの外にある送信先の特定。基盤は未対応。
    ExternalDestination,
    /// 本番環境での保存・動作。基盤は試験環境だけを扱う。
    ProductionEnvironment,
}

/// その種類の観測を、基盤がどこまで扱えるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// 試験の判定条件として観測し、結果を期待と照合できる。
    Judged,
    /// 試行の記録には残るが、判定条件にはできない。
    RecordedOnly,
    /// 推定や申告であり、実行の観測ではない。
    Inference,
    /// 基盤が観測できない。
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct KindInfo {
    pub id: ObservationKind,
    pub label: &'static str,
    pub support: Support,
    pub note: &'static str,
}

impl ObservationKind {
    pub const ALL: [ObservationKind; 22] = [
        Self::ExitCode,
        Self::Stdout,
        Self::Stderr,
        Self::StoredFile,
        Self::ReadbackOutput,
        Self::ServerOutput,
        Self::ContainerLog,
        Self::DatabaseQuery,
        Self::BrowserText,
        Self::BrowserResponse,
        Self::BrowserCanvas,
        Self::ServerHttp,
        Self::DatabaseRows,
        Self::DatabaseRelation,
        Self::ProgramObservation,
        Self::ReportedObservation,
        Self::TrafficSummary,
        Self::CodeInspection,
        Self::NetworkPayload,
        Self::DistributedTrace,
        Self::ExternalDestination,
        Self::ProductionEnvironment,
    ];
    pub fn of_method(method: CheckMethod) -> Self {
        match method {
            CheckMethod::ExitCode => Self::ExitCode,
            CheckMethod::Stdout => Self::Stdout,
            CheckMethod::Stderr => Self::Stderr,
            CheckMethod::StoredFile => Self::StoredFile,
            CheckMethod::ReadbackOutput => Self::ReadbackOutput,
            CheckMethod::ServerOutput => Self::ServerOutput,
            CheckMethod::ContainerLog => Self::ContainerLog,
            CheckMethod::DatabaseQuery => Self::DatabaseQuery,
            CheckMethod::BrowserText => Self::BrowserText,
            CheckMethod::BrowserResponse => Self::BrowserResponse,
            CheckMethod::BrowserCanvas => Self::BrowserCanvas,
            CheckMethod::ServerHttp => Self::ServerHttp,
            CheckMethod::DatabaseRows => Self::DatabaseRows,
            CheckMethod::DatabaseRelation => Self::DatabaseRelation,
        }
    }
    pub fn support(self) -> Support {
        match self {
            Self::ExitCode
            | Self::Stdout
            | Self::Stderr
            | Self::StoredFile
            | Self::ReadbackOutput
            | Self::ServerOutput
            | Self::ContainerLog
            | Self::DatabaseQuery
            | Self::BrowserText
            | Self::BrowserResponse
            | Self::BrowserCanvas
            | Self::ServerHttp
            | Self::DatabaseRows
            | Self::DatabaseRelation
            | Self::ProgramObservation
            | Self::ReportedObservation => Support::Judged,
            Self::TrafficSummary => Support::RecordedOnly,
            Self::CodeInspection => Support::Inference,
            Self::NetworkPayload
            | Self::DistributedTrace
            | Self::ExternalDestination
            | Self::ProductionEnvironment => Support::Unsupported,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::ExitCode => "対象プログラムの終了コード",
            Self::Stdout => "対象プログラムの標準出力",
            Self::Stderr => "対象プログラムの標準エラー",
            Self::StoredFile => "基盤が保存先のファイルを直接読み出した内容",
            Self::ReadbackOutput => "別のプロセスで対象の読み出し機能を動かした出力",
            Self::ServerOutput => "待つプロセス（受信側など）の出力",
            Self::ContainerLog => "試験用コンテナのログ",
            Self::DatabaseQuery => "基盤が試験用DBへ送った問い合わせの結果",
            Self::BrowserText => "試験用ブラウザで読み出した画面の文字列",
            Self::BrowserResponse => "試験操作中にブラウザが受け取ったHTTP応答",
            Self::BrowserCanvas => "canvasの画面写真から読み出した画素",
            Self::ServerHttp => "基盤が試験用サーバーへ直接要求した応答",
            Self::DatabaseRows => "基盤が試験値に一致する試験用DBの行を直接読み出した結果",
            Self::DatabaseRelation => "基盤が読み出した試験用DBの行どうしの対応",
            Self::ProgramObservation => "対象プログラムが出力した観測記録",
            Self::ReportedObservation => "外部の報告元から取り込んだ観測",
            Self::TrafficSummary => "中継が記録した接続の集計（接続元・接続先・件数・バイト数）",
            Self::CodeInspection => "コードを読んだ推定（人またはAI）",
            Self::NetworkPayload => "通信の中身そのもの",
            Self::DistributedTrace => "操作から個々のDB書き込みまでを対応づける分散トレース",
            Self::ExternalDestination => "試験用ネットワークの外にある送信先の特定",
            Self::ProductionEnvironment => "本番環境での保存・動作",
        }
    }
    pub fn note(self) -> &'static str {
        match self.support() {
            Support::Judged => "試験の判定条件として観測し、期待と照合できる",
            Support::RecordedOnly => {
                "試行の記録に残るが、判定条件にはできない。通信の中身は含まない"
            }
            Support::Inference => "実行の観測ではない。この種類だけでは確認済みにならない",
            Support::Unsupported => "基盤が観測できない。この部分は未確認として残る",
        }
    }
    pub fn info(self) -> KindInfo {
        KindInfo {
            id: self,
            label: self.label(),
            support: self.support(),
            note: self.note(),
        }
    }
}

/// 基盤が扱える観測の種類の一覧。画面と記録が同じ一覧を使う。
pub fn catalog() -> Vec<KindInfo> {
    ObservationKind::ALL.iter().map(|k| k.info()).collect()
}

/// 確認項目のうち、確かめたい一つの部分と、それをどの観測で確かめるかの宣言。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Aspect {
    pub id: String,
    /// 確かめたい部分。人の言葉か、AIの案を人が確認して残した文。
    pub text: String,
    /// 出所（記入者、AIの整理案など）。
    pub origin: String,
    /// この部分を確かめるのに必要な観測の種類。未定なら None。
    #[serde(default)]
    pub observation: Option<ObservationKind>,
    /// 対応づける試験の判定条件ID。実行記録の判定条件と照合する。
    #[serde(default)]
    pub trial_checks: Vec<String>,
}

pub fn validate(aspects: &[Aspect]) -> io::Result<()> {
    if aspects.len() > MAX_ASPECTS {
        return Err(invalid(format!("確かめる部分は{MAX_ASPECTS}件以内です")));
    }
    let mut ids = BTreeSet::new();
    for a in aspects {
        if a.id.is_empty()
            || a.id.len() > 128
            || !a
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !ids.insert(a.id.as_str())
        {
            return Err(invalid("確かめる部分の識別子が重複または不正です"));
        }
        if a.text.trim().is_empty()
            || a.text.chars().count() > 2000
            || a.origin.trim().is_empty()
            || a.origin.chars().count() > 2000
        {
            return Err(invalid(
                "確かめる部分には空でない2000文字以内の文と出所が必要です",
            ));
        }
        if a.trial_checks.len() > MAX_LINKS {
            return Err(invalid("対応づける判定条件が多すぎます"));
        }
        let mut links = BTreeSet::new();
        for id in &a.trial_checks {
            if id.is_empty() || id.len() > 128 || !links.insert(id.as_str()) {
                return Err(invalid("対応づける判定条件の識別子が重複または不正です"));
            }
        }
        if !a.trial_checks.is_empty()
            && a.observation.is_none_or(|k| k.support() != Support::Judged)
        {
            return Err(invalid(
                "判定条件を対応づけるには、判定に使える観測の種類を指定してください",
            ));
        }
    }
    Ok(())
}

/// 一つの試行の中で、対応づけの相手になる観測。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub id: String,
    pub kind: ObservationKind,
    pub scope: String,
    /// 試行の結果。結果が保存されていなければ None。
    pub observed: Option<Observed>,
}

/// 試行の記録から、判定条件ごとの観測を取り出す。
pub fn items(evidence: &Evidence) -> Vec<Item> {
    if let Some(trace) = &evidence.execution {
        if let Some(spec) = &trace.recipe.trial {
            return spec
                .checks
                .iter()
                .map(|c| Item {
                    id: c.id.clone(),
                    kind: ObservationKind::of_method(c.method),
                    scope: c.scope.clone(),
                    observed: trace
                        .trial
                        .as_ref()
                        .and_then(|o| o.results.iter().find(|r| r.id == c.id).map(|r| r.observed)),
                })
                .collect();
        }
        let Some(criterion) = &evidence.observation else {
            return vec![];
        };
        let outcomes: Vec<_> = trace
            .observations
            .iter()
            .filter(|o| o.assertion == criterion.assertion && o.subject == criterion.subject)
            .map(|o| o.outcome.clone())
            .collect();
        return vec![Item {
            id: criterion.assertion.clone(),
            kind: ObservationKind::ProgramObservation,
            scope: criterion.subject.clone(),
            observed: combine(&outcomes),
        }];
    }
    if let (Some(trace), Some(criterion)) = (&evidence.reported, &evidence.observation) {
        let outcomes: Vec<_> = trace
            .observations
            .iter()
            .filter(|o| o.assertion == criterion.assertion && o.subject == criterion.subject)
            .map(|o| o.outcome.clone())
            .collect();
        return vec![Item {
            id: criterion.assertion.clone(),
            kind: ObservationKind::ReportedObservation,
            scope: criterion.subject.clone(),
            observed: combine(&outcomes),
        }];
    }
    vec![]
}

fn combine(outcomes: &[crate::execution::ObservationOutcome]) -> Option<Observed> {
    use crate::execution::ObservationOutcome;
    let pass = outcomes.contains(&ObservationOutcome::Pass);
    let fail = outcomes.contains(&ObservationOutcome::Fail);
    match (pass, fail) {
        (true, false) => Some(Observed::Pass),
        (false, true) => Some(Observed::Fail),
        (true, true) => None,
        (false, false) => Some(Observed::NotObserved),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AspectStatus {
    /// 対応づけた判定条件がすべて期待どおりに観測された。
    Observed,
    /// 対応づけた判定条件のいずれかが期待と異なった。
    Contradicted,
    /// 対応づけた判定条件のいずれかを観測できていない、または結果が食い違う。
    NotObserved,
    /// 対応づけた判定条件が、最新の試行にはない。
    NotInTrial,
    /// 判定条件の観測方法が、宣言した観測の種類と違う。
    MethodMismatch,
    /// 観測の種類はあるが、どの判定条件が答えるか未対応づけ。
    Unlinked,
    /// 観測の種類が未定。
    Undefined,
    /// 基盤が観測できない種類。
    Unsupported,
    /// 記録はあるが判定条件にできない種類、または推定のみ。
    NotJudged,
    /// この確認項目に適用できる試行がない。
    NoEvidence,
}
impl AspectStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Observed => "麦漆が観測した",
            Self::Contradicted => "期待と異なる観測",
            Self::NotObserved => "観測できていない",
            Self::NotInTrial => "最新の試行に対応する判定条件がない",
            Self::MethodMismatch => "宣言した観測の種類と判定条件が合わない",
            Self::Unlinked => "どの判定条件が答えるか未対応づけ",
            Self::Undefined => "観測方法が未定",
            Self::Unsupported => "基盤が観測できない",
            Self::NotJudged => "記録や推定だけで、麦漆は判定していない",
            Self::NoEvidence => "適用できる試行がまだない",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AspectView {
    pub id: String,
    pub text: String,
    pub origin: String,
    pub observation: Option<ObservationKind>,
    pub observation_label: String,
    pub support: Option<Support>,
    pub trial_checks: Vec<String>,
    pub status: AspectStatus,
    pub status_label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// 確かめる部分がまだ登録されていない。試験が何に答えるか決まっていない。
    NotDeclared,
    /// 登録した部分がすべて観測された。
    Covered,
    /// 観測された部分と、未観測・未対応・期待と異なる部分が混在する。
    Partial,
    /// 観測された部分がない。
    NotCovered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub status: CoverageStatus,
    pub aspects: Vec<AspectView>,
    /// 観測した部分の数と、登録した部分の数。
    pub observed: usize,
    pub declared: usize,
    pub summary: String,
    /// 未確認として残る部分の説明。
    pub remaining: Vec<String>,
}

const NOT_DECLARED: &str = "この確認項目のどの部分を試験が確かめるか、まだ登録されていない。試験の合否だけでは確認項目に答えたことにならない";

/// 確かめる部分が未登録の状態。旧形式の記録を読むときの既定値。
pub fn undeclared() -> Coverage {
    Coverage {
        status: CoverageStatus::NotDeclared,
        aspects: vec![],
        observed: 0,
        declared: 0,
        summary: NOT_DECLARED.into(),
        remaining: vec![],
    }
}

/// 確認項目の各部分を、適用できる最新の試行と照合する。`evidence`は適用できる試行だけを渡す。
pub fn assess(check: &Check, evidence: Option<&Evidence>) -> Coverage {
    if check.aspects.is_empty() {
        return undeclared();
    }
    let items = evidence.map(items).unwrap_or_default();
    let aspects: Vec<AspectView> = check
        .aspects
        .iter()
        .map(|a| {
            let (status, detail) = status_of(a, evidence.is_some(), &items);
            AspectView {
                id: a.id.clone(),
                text: a.text.clone(),
                origin: a.origin.clone(),
                observation: a.observation,
                observation_label: a
                    .observation
                    .map(|k| k.label().to_owned())
                    .unwrap_or_else(|| "未定".to_owned()),
                support: a.observation.map(|k| k.support()),
                trial_checks: a.trial_checks.clone(),
                status_label: status.label().to_owned(),
                status,
                detail,
            }
        })
        .collect();
    let declared = aspects.len();
    let observed = aspects
        .iter()
        .filter(|a| a.status == AspectStatus::Observed)
        .count();
    let remaining: Vec<String> = aspects
        .iter()
        .filter(|a| a.status != AspectStatus::Observed)
        .map(|a| format!("{}: {}（{}）", a.text, a.status_label, a.detail))
        .collect();
    let (status, summary) = if observed == declared {
        (
            CoverageStatus::Covered,
            format!("登録した{declared}件の部分をすべて、対応づけた判定条件で観測した"),
        )
    } else if observed == 0 {
        (
            CoverageStatus::NotCovered,
            format!("登録した{declared}件の部分のうち、観測できた部分はない"),
        )
    } else {
        (
            CoverageStatus::Partial,
            format!("登録した{declared}件のうち{observed}件を観測した。残りは未確認"),
        )
    };
    Coverage {
        status,
        aspects,
        observed,
        declared,
        summary,
        remaining,
    }
}

fn status_of(aspect: &Aspect, has_evidence: bool, items: &[Item]) -> (AspectStatus, String) {
    let Some(kind) = aspect.observation else {
        return (
            AspectStatus::Undefined,
            "どの観測で確かめるか決まっていない".into(),
        );
    };
    match kind.support() {
        Support::Unsupported => {
            return (
                AspectStatus::Unsupported,
                format!("{}は基盤が観測できない", kind.label()),
            );
        }
        Support::RecordedOnly | Support::Inference => {
            return (AspectStatus::NotJudged, kind.note().into());
        }
        Support::Judged => (),
    }
    if aspect.trial_checks.is_empty() {
        return (
            AspectStatus::Unlinked,
            "この部分に答える判定条件を選んでいない".into(),
        );
    }
    if !has_evidence {
        return (
            AspectStatus::NoEvidence,
            "この確認項目に適用できる試行がまだない".into(),
        );
    }
    let mut details = vec![];
    let mut worst = AspectStatus::Observed;
    let rank = |s: AspectStatus| match s {
        AspectStatus::Observed => 0,
        AspectStatus::NotObserved => 1,
        AspectStatus::Contradicted => 2,
        AspectStatus::MethodMismatch => 3,
        AspectStatus::NotInTrial => 4,
        _ => 5,
    };
    for id in &aspect.trial_checks {
        let (status, detail) = match items.iter().find(|i| &i.id == id) {
            None => (
                AspectStatus::NotInTrial,
                format!("判定条件 {id} は最新の試行にない"),
            ),
            Some(item) if item.kind != kind => (
                AspectStatus::MethodMismatch,
                format!(
                    "判定条件 {id} の観測方法は「{}」で、宣言した「{}」と違う",
                    item.kind.label(),
                    kind.label()
                ),
            ),
            Some(item) => match item.observed {
                Some(Observed::Pass) => (
                    AspectStatus::Observed,
                    format!("判定条件 {id}（{}）: 期待どおり", item.scope),
                ),
                Some(Observed::Fail) => (
                    AspectStatus::Contradicted,
                    format!("判定条件 {id}（{}）: 期待と異なる", item.scope),
                ),
                Some(Observed::NotObserved) => (
                    AspectStatus::NotObserved,
                    format!("判定条件 {id}（{}）: 観測できていない", item.scope),
                ),
                None => (
                    AspectStatus::NotObserved,
                    format!(
                        "判定条件 {id}（{}）: 結果が保存されていないか食い違う",
                        item.scope
                    ),
                ),
            },
        };
        if rank(status) > rank(worst) {
            worst = status;
        }
        details.push(detail);
    }
    (worst, details.join("、"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aspect(kind: Option<ObservationKind>, links: &[&str]) -> Aspect {
        Aspect {
            id: "a".into(),
            text: "合成の部分".into(),
            origin: "試験".into(),
            observation: kind,
            trial_checks: links.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn item(id: &str, kind: ObservationKind, observed: Option<Observed>) -> Item {
        Item {
            id: id.into(),
            kind,
            scope: "範囲".into(),
            observed,
        }
    }

    #[test]
    fn every_kind_is_listed_once_with_its_support() {
        let catalog = catalog();
        assert_eq!(catalog.len(), ObservationKind::ALL.len());
        let ids: BTreeSet<_> = catalog.iter().map(|k| format!("{:?}", k.id)).collect();
        assert_eq!(ids.len(), catalog.len());
        assert_eq!(
            ObservationKind::NetworkPayload.support(),
            Support::Unsupported
        );
        assert_eq!(
            ObservationKind::TrafficSummary.support(),
            Support::RecordedOnly
        );
        assert_eq!(
            ObservationKind::CodeInspection.support(),
            Support::Inference
        );
        for method in [
            CheckMethod::DatabaseRows,
            CheckMethod::BrowserText,
            CheckMethod::ExitCode,
        ] {
            assert_eq!(
                ObservationKind::of_method(method).support(),
                Support::Judged
            );
        }
    }

    #[test]
    fn unsupported_undefined_and_unlinked_aspects_are_never_observed() {
        let items = vec![item(
            "x",
            ObservationKind::DatabaseRows,
            Some(Observed::Pass),
        )];
        assert_eq!(
            status_of(&aspect(None, &[]), true, &items).0,
            AspectStatus::Undefined
        );
        assert_eq!(
            status_of(
                &aspect(Some(ObservationKind::NetworkPayload), &[]),
                true,
                &items
            )
            .0,
            AspectStatus::Unsupported
        );
        assert_eq!(
            status_of(
                &aspect(Some(ObservationKind::TrafficSummary), &[]),
                true,
                &items
            )
            .0,
            AspectStatus::NotJudged
        );
        assert_eq!(
            status_of(
                &aspect(Some(ObservationKind::CodeInspection), &[]),
                true,
                &items
            )
            .0,
            AspectStatus::NotJudged
        );
        assert_eq!(
            status_of(
                &aspect(Some(ObservationKind::DatabaseRows), &[]),
                true,
                &items
            )
            .0,
            AspectStatus::Unlinked
        );
        assert_eq!(
            status_of(
                &aspect(Some(ObservationKind::DatabaseRows), &["x"]),
                false,
                &[]
            )
            .0,
            AspectStatus::NoEvidence
        );
    }

    #[test]
    fn linked_checks_are_matched_by_id_and_method_and_the_worst_result_wins() {
        let items = vec![
            item("rows", ObservationKind::DatabaseRows, Some(Observed::Pass)),
            item("text", ObservationKind::BrowserText, Some(Observed::Pass)),
            item(
                "missing",
                ObservationKind::DatabaseRows,
                Some(Observed::NotObserved),
            ),
            item("bad", ObservationKind::DatabaseRows, Some(Observed::Fail)),
        ];
        let kind = Some(ObservationKind::DatabaseRows);
        assert_eq!(
            status_of(&aspect(kind, &["rows"]), true, &items).0,
            AspectStatus::Observed
        );
        assert_eq!(
            status_of(&aspect(kind, &["rows", "missing"]), true, &items).0,
            AspectStatus::NotObserved
        );
        assert_eq!(
            status_of(&aspect(kind, &["rows", "bad"]), true, &items).0,
            AspectStatus::Contradicted
        );
        assert_eq!(
            status_of(&aspect(kind, &["text"]), true, &items).0,
            AspectStatus::MethodMismatch
        );
        assert_eq!(
            status_of(&aspect(kind, &["rows", "gone"]), true, &items).0,
            AspectStatus::NotInTrial
        );
    }

    #[test]
    fn validation_rejects_links_without_a_judged_kind_and_duplicate_ids() {
        assert!(validate(&[aspect(Some(ObservationKind::DatabaseRows), &["x"])]).is_ok());
        assert!(validate(&[aspect(None, &["x"])]).is_err());
        assert!(validate(&[aspect(Some(ObservationKind::NetworkPayload), &["x"])]).is_err());
        assert!(validate(&[aspect(None, &[]), aspect(None, &[])]).is_err());
        assert!(validate(&[aspect(Some(ObservationKind::DatabaseRows), &["x", "x"])]).is_err());
        let mut empty = aspect(None, &[]);
        empty.text = " ".into();
        assert!(validate(&[empty]).is_err());
    }
}
