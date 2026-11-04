//! 確認項目のどの部分を、どの観測で確かめたかの対応づけ。
//! 試験の合否（判定条件がすべて通ったか）とは別に求める。人やAIが述べた対応づけを、
//! 基盤が実行記録の判定条件IDと観測方法に照合し、文章の一致から適否を決めない。
use crate::error::invalid;
use crate::execution::{Criterion, Recipe};
use crate::trial::{CheckMethod, Observed, TrialCheck};
use crate::workspace::{Check, Evidence};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

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

/// 判定条件が依存する試験の設定。起動・引数・入力・故障・保存先・読み直し・待つプロセス・コンテナ・
/// HTTP操作・ブラウザ操作（準備・操作・DBの問い合わせ先）と、復旧の操作の有無を含む。固定版（コミット）、
/// 確認項目のID、復旧後の待機上限の値は含めない。固定版の違いは試行の適用可否で扱う。
fn trial_context(recipe: &Recipe) -> serde_json::Value {
    let mut context = serde_json::json!({
        "runtime": recipe.runtime,
        "launch": recipe.launch,
        "entrypoint": recipe.entrypoint,
        "args": recipe.args,
        "input": recipe.input,
        "fault": recipe.trial.as_ref().map(|t| t.fault),
        "storage": recipe.trial.as_ref().and_then(|t| t.storage.clone()),
        "readback": recipe.trial.as_ref().and_then(|t| t.readback.clone()),
        "python": recipe.python,
        "server": recipe.server,
        "containers": recipe.containers,
        "http": recipe.http,
        "browser": recipe.browser,
    });
    // 復旧の操作（故障を解除して読み直す）は、実行する操作と、その後に読むログ・DBの範囲を変える。
    // 有無だけを含め、待機上限の値は含めない。復旧のない設定は、この項目を持たない（保存済みの指紋を保つ）。
    if recipe.trial.as_ref().is_some_and(|t| t.recovery.is_some()) {
        context["recovery"] = serde_json::json!(true);
    }
    context
}
/// 判定条件の内容（範囲・観測方法・期待・対象・文）と、その判定条件が依存する試験の設定の指紋。
/// 識別子と文面が同じでも、操作対象・引数・観測先などが変われば一致しない。
pub fn trial_check_fingerprint(check: &TrialCheck, recipe: &Recipe) -> String {
    let bytes =
        serde_json::to_vec(&serde_json::json!({"check": check, "context": trial_context(recipe)}))
            .unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}
fn link_label(definition: &TrialCheck, recipe: &Recipe) -> String {
    let fault = recipe
        .trial
        .as_ref()
        .map(|t| t.fault.label())
        .unwrap_or("故障の指定なし");
    format!(
        "{}（{}。期待: {:?}{}{}。{}。起動: {} {}）",
        definition.scope,
        definition.method.source_label(),
        definition.expect,
        definition
            .text
            .as_deref()
            .map(|t| format!(" 「{t}」"))
            .unwrap_or_default(),
        definition
            .target
            .as_deref()
            .map(|t| format!("、対象 {t}"))
            .unwrap_or_default(),
        fault,
        recipe.entrypoint,
        recipe.args.join(" ")
    )
}
/// 登録時に送る判定条件と、それが属する試験の設定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkDefinition {
    pub check: TrialCheck,
    pub recipe: Recipe,
}
/// 試験案を使わない試行（対象プログラムの観測・外部の報告）の判定の指紋。
pub fn criterion_fingerprint(criterion: &Criterion) -> String {
    let bytes = serde_json::to_vec(criterion).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

/// 確かめたい部分に対応づけた判定条件。識別子だけでなく内容の指紋で照合する。
/// 等価性は保存する識別子・指紋・説明で決め、登録時にだけ送る内容は比べない。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialLink {
    pub id: String,
    /// 対応づけた時点の判定条件の内容の指紋。基盤が`definition`から計算する。
    #[serde(default)]
    pub fingerprint: String,
    /// 対応づけた時点の判定条件の説明（範囲と期待）。人が見直すための記録。
    #[serde(default)]
    pub label: String,
    /// 登録時にだけ送る判定条件の内容と試験の設定。基盤が指紋と説明に変えて保存し、本体は残さない。
    /// 送らない対応づけは、保存済みの指紋をそのまま保つ（人が選び直すまで更新しない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<LinkDefinition>,
    /// 対応づけた時点の判定条件と、伏せた試験設定の識別子。基盤が`definition`から作り、
    /// 変更内容を人に示すために使う。この機能より前の対応づけにはない。利用者からは受け取らない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<LinkBasis>,
}
impl PartialEq for TrialLink {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.fingerprint == other.fingerprint
            && self.label == other.label
            && self.basis == other.basis
    }
}
impl Eq for TrialLink {}

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
    /// 対応づける試験の判定条件。実行記録の判定条件の識別子・観測方法・内容の指紋と照合する。
    #[serde(default)]
    pub trial_checks: Vec<TrialLink>,
}

/// 対応づけた時点の判定条件と、その試験設定（秘密になりうる値を伏せた形）の識別子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkBasis {
    pub check: TrialCheck,
    /// `Check.link_settings`に置いた、伏せた試験設定の識別子（その内容のSHA-256）。
    pub settings: String,
}

/// 対応づけた時点の試験設定。同じ試験設定を参照する対応づけで共有する。
/// コンテナの環境変数の値は伏せ、名前だけを残す。指紋は伏せる前の設定から計算するため、
/// 伏せた値の変更は指紋の不一致として検出できるが、変更内容は示せない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsRecord {
    pub id: String,
    pub settings: Value,
}

/// 記録しない値の代わりに置く文字列。
pub const WITHHELD: &str = "（記録しない値）";
const MAX_SETTINGS: usize = 8;
const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

/// 対応づけの記録と比較に使う試験設定。指紋の対象と同じ項目から、コンテナの環境変数の値を伏せる。
pub fn recorded_settings(recipe: &Recipe) -> Value {
    let mut context = trial_context(recipe);
    if let Some(services) = context
        .pointer_mut("/containers/services")
        .and_then(Value::as_array_mut)
    {
        for service in services {
            if let Some(env) = service.get_mut("env").and_then(Value::as_object_mut) {
                for value in env.values_mut() {
                    *value = Value::String(WITHHELD.into());
                }
            }
        }
    }
    context
}
fn settings_id(settings: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(settings).unwrap_or_default())
    )
}

/// 登録時の入力を保存する形にする。判定条件の内容（`definition`）が送られていれば、基盤が指紋・説明と
/// 対応づけた時点の設定の記録に変える。内容を送らない対応づけは、同じ識別子・指紋の保存済みの対応づけが
/// あればその説明と記録を保ち（利用者が送った説明では上書きしない）、なければ送られた指紋を受け取る
/// （試験案を使わない試行の判定、旧形式）。記録（`basis`）は利用者から受け取らない。
/// 戻り値は保存する部分と、それらが参照する試験設定の記録。
pub fn normalize(
    aspects: Vec<Aspect>,
    stored: &Check,
) -> io::Result<(Vec<Aspect>, Vec<SettingsRecord>)> {
    let stored_links: Vec<&TrialLink> = stored
        .aspects
        .iter()
        .flat_map(|a| &a.trial_checks)
        .collect();
    let mut settings: BTreeMap<String, Value> = BTreeMap::new();
    let mut result = Vec::with_capacity(aspects.len());
    for mut aspect in aspects {
        for link in &mut aspect.trial_checks {
            if let Some(LinkDefinition { check, recipe }) = link.definition.take() {
                let in_recipe = recipe
                    .trial
                    .as_ref()
                    .is_some_and(|t| t.checks.contains(&check));
                if check.id != link.id || !in_recipe {
                    return Err(invalid(
                        "対応づける判定条件の識別子・内容が、送られた試験の設定と一致しません",
                    ));
                }
                link.fingerprint = trial_check_fingerprint(&check, &recipe);
                link.label = link_label(&check, &recipe);
                let recorded = recorded_settings(&recipe);
                let id = settings_id(&recorded);
                settings.insert(id.clone(), recorded);
                link.basis = Some(LinkBasis {
                    check,
                    settings: id,
                });
            } else if let Some(previous) = stored_links
                .iter()
                .find(|l| l.id == link.id && l.fingerprint == link.fingerprint)
            {
                link.label = previous.label.clone();
                link.basis = previous.basis.clone();
                if let Some(basis) = &previous.basis
                    && let Some(record) =
                        stored.link_settings.iter().find(|r| r.id == basis.settings)
                {
                    settings.insert(record.id.clone(), record.settings.clone());
                }
            } else if link.basis.is_some() {
                return Err(invalid(
                    "対応づけた時点の設定の記録は麦漆が作ります。判定条件の内容（definition）を送ってください",
                ));
            }
            if link.fingerprint.len() != 64
                || !link.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
                || link.label.chars().count() > 2000
            {
                return Err(invalid(
                    "対応づける判定条件には、その内容（definition）または内容の指紋が必要です",
                ));
            }
        }
        result.push(aspect);
    }
    let settings: Vec<SettingsRecord> = settings
        .into_iter()
        .map(|(id, settings)| SettingsRecord { id, settings })
        .collect();
    validate(&result)?;
    validate_settings(&result, &settings)?;
    Ok((result, settings))
}

/// 対応づけた時点の設定の記録を検査する。識別子は内容のSHA-256で、参照されている記録だけを置く。
pub fn validate_settings(aspects: &[Aspect], settings: &[SettingsRecord]) -> io::Result<()> {
    let referenced: BTreeSet<&str> = aspects
        .iter()
        .flat_map(|a| &a.trial_checks)
        .filter_map(|l| l.basis.as_ref().map(|b| b.settings.as_str()))
        .collect();
    let mut ids = BTreeSet::new();
    let mut bytes = 0;
    for record in settings {
        bytes += serde_json::to_vec(&record.settings)
            .map_err(io::Error::other)?
            .len();
        if record.id != settings_id(&record.settings)
            || !ids.insert(record.id.as_str())
            || !referenced.contains(record.id.as_str())
        {
            return Err(invalid("対応づけた時点の設定の記録が不正です"));
        }
    }
    if settings.len() > MAX_SETTINGS || bytes > MAX_SETTINGS_BYTES {
        return Err(invalid(format!(
            "対応づけた試験設定が{MAX_SETTINGS}種類・1MiBを超えます。使っていない対応づけを外してください"
        )));
    }
    for link in aspects.iter().flat_map(|a| &a.trial_checks) {
        if let Some(basis) = &link.basis
            && (basis.check.id != link.id || !ids.contains(basis.settings.as_str()))
        {
            return Err(invalid("対応づけた時点の設定の記録が見つかりません"));
        }
    }
    Ok(())
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
        for link in &a.trial_checks {
            if link.id.is_empty()
                || link.id.len() > 128
                || link.definition.is_some()
                || !links.insert(link.id.as_str())
            {
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
    /// 判定条件の内容の指紋。対応づけた時点のものと照合する。
    pub fingerprint: String,
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
                    fingerprint: trial_check_fingerprint(c, &trace.recipe),
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
            fingerprint: criterion_fingerprint(criterion),
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
            fingerprint: criterion_fingerprint(criterion),
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
    /// 同じ識別子の判定条件はあるが、対応づけた時点と内容（範囲・期待・対象）が違う。
    DefinitionChanged,
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
            Self::DefinitionChanged => {
                "対応づけた後に判定条件の内容が変わった。人が対応づけを確認し直す"
            }
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
    pub trial_checks: Vec<TrialLink>,
    pub status: AspectStatus,
    pub status_label: String,
    pub detail: String,
    /// 内容が変わった対応づけについて、対応づけた時点と試行の設定の違い。旧形式の記録にはない。
    #[serde(default)]
    pub link_changes: Vec<LinkComparison>,
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
/// `all_evidence`は、旧形式の対応づけについて対応づけた時点の設定を探すためだけに使い、判定には使わない。
pub fn assess(check: &Check, evidence: Option<&Evidence>, all_evidence: &[Evidence]) -> Coverage {
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
                link_changes: evidence
                    .map(|e| changed_links(check, a, e, &items, all_evidence))
                    .unwrap_or_default(),
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
        AspectStatus::DefinitionChanged => 4,
        AspectStatus::NotInTrial => 5,
        _ => 6,
    };
    for link in &aspect.trial_checks {
        let id = &link.id;
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
            Some(item) if item.fingerprint != link.fingerprint => (
                AspectStatus::DefinitionChanged,
                format!(
                    "判定条件 {id}（{}）は、対応づけた時点（{}）と内容が違う",
                    item.scope,
                    if link.label.is_empty() {
                        "説明なし"
                    } else {
                        link.label.as_str()
                    }
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

/// 対応づけた時点と現在で、試験設定の一つの値がどう変わったか。値がなければ None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChange {
    /// 人が読む区分（判定条件、ブラウザ操作、DBの問い合わせ先など）。
    pub section: String,
    /// 区分の中の場所。配列は識別子（id・name）があればそれで示す。
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// 現在の設定でも、対応づけた時点と指紋が一致する。
    Unchanged,
    /// 指紋が一致しない。対応づけは適用されない。
    Changed,
    /// 現在の設定に、同じ識別子の判定条件がない。
    NotInTrial,
}

/// 一つの対応づけについて、対応づけた時点の設定と、現在（または試行）の設定の比較。
/// 指紋の一致・不一致は基盤が判定する。違いの一覧は記録した設定どうしの比較で、
/// 記録していない値（コンテナの環境変数の値）の違いは示せない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkComparison {
    pub link_id: String,
    /// 対応づけた時点の指紋。
    pub fingerprint: String,
    pub state: LinkState,
    /// 比較に使った、対応づけた時点の設定の出所。見つからなければ None。
    pub basis_source: Option<String>,
    pub changes: Vec<SettingChange>,
    /// 比較できなかったこと。推測で補わず、そのまま示す。
    pub uncomparable: Vec<String>,
}

const WITHHELD_NOTE: &str = "コンテナの環境変数の値は記録しないため、値の変更は比較していない（名前の追加・削除は表示する）";
const MAX_SHOWN: usize = 300;

/// 対応づけた時点の判定条件と試験設定を探す。保存した記録を優先し、なければ（この機能より前の対応づけ）
/// 同じ確認項目の試行のうち、同じ識別子・同じ指紋の判定条件で実行した記録から復元する。
fn resolve_basis(
    link: &TrialLink,
    check: &Check,
    evidence: &[Evidence],
) -> Option<(TrialCheck, Value, String)> {
    if let Some(basis) = &link.basis
        && let Some(record) = check.link_settings.iter().find(|r| r.id == basis.settings)
    {
        return Some((
            basis.check.clone(),
            record.settings.clone(),
            "対応づけた時点に麦漆が保存した設定".into(),
        ));
    }
    evidence
        .iter()
        .filter(|e| e.check_id == check.id)
        .find_map(|e| {
            let recipe = &e.execution.as_ref()?.recipe;
            let c = recipe.trial.as_ref()?.checks.iter().find(|c| {
                c.id == link.id && trial_check_fingerprint(c, recipe) == link.fingerprint
            })?;
            Some((
                c.clone(),
                recorded_settings(recipe),
                format!("同じ指紋の判定条件で実行した試行 {} の記録", e.id),
            ))
        })
}

/// 対応づけを、現在（または試行）の判定条件と試験設定に照らして比べる。
pub fn compare_link(
    link: &TrialLink,
    check: &Check,
    current: Option<(&TrialCheck, &Recipe)>,
    evidence: &[Evidence],
) -> LinkComparison {
    let mut comparison = LinkComparison {
        link_id: link.id.clone(),
        fingerprint: link.fingerprint.clone(),
        state: LinkState::NotInTrial,
        basis_source: None,
        changes: vec![],
        uncomparable: vec![],
    };
    let Some((current_check, recipe)) = current else {
        return comparison;
    };
    if trial_check_fingerprint(current_check, recipe) == link.fingerprint {
        comparison.state = LinkState::Unchanged;
        return comparison;
    }
    comparison.state = LinkState::Changed;
    let Some((before_check, before_settings, source)) = resolve_basis(link, check, evidence) else {
        comparison.uncomparable.push(
            "対応づけた時点の設定が保存されていない（この機能より前に作った対応づけ）。同じ指紋で実行した試行の記録もないため、何が変わったかは示せない".into(),
        );
        return comparison;
    };
    comparison.basis_source = Some(source);
    let after_settings = recorded_settings(recipe);
    let withheld = has_withheld(&before_settings) || has_withheld(&after_settings);
    comparison.changes = diff(
        &serde_json::json!({"check": before_check, "settings": before_settings}),
        &serde_json::json!({"check": current_check, "settings": after_settings}),
    );
    if comparison.changes.is_empty() {
        comparison.uncomparable.push(if withheld {
            "記録した項目には違いがない。記録しない値（コンテナの環境変数の値）が変わった可能性があるが、麦漆は内容を比較していない".into()
        } else {
            "記録した項目には違いがないが、指紋が一致しない。原因は特定できていない".into()
        });
    } else if withheld {
        comparison.uncomparable.push(WITHHELD_NOTE.into());
    }
    comparison
}

/// 試行で内容が変わった対応づけの比較。試験案を使わない試行は観測名と対象の指紋だけで照合する。
fn changed_links(
    check: &Check,
    aspect: &Aspect,
    evidence: &Evidence,
    items: &[Item],
    all_evidence: &[Evidence],
) -> Vec<LinkComparison> {
    aspect
        .trial_checks
        .iter()
        .filter(|l| {
            items.iter().any(|i| {
                i.id == l.id && Some(i.kind) == aspect.observation && i.fingerprint != l.fingerprint
            })
        })
        .map(|l| {
            let trial = evidence.execution.as_ref().and_then(|t| {
                let c = t.recipe.trial.as_ref()?.checks.iter().find(|c| c.id == l.id)?;
                Some((c, &t.recipe))
            });
            if trial.is_some() {
                return compare_link(l, check, trial, all_evidence);
            }
            LinkComparison {
                link_id: l.id.clone(),
                fingerprint: l.fingerprint.clone(),
                state: LinkState::Changed,
                basis_source: None,
                changes: vec![],
                uncomparable: vec![
                    "試験案を使わない試行の判定（観測名と対象）は指紋だけで照合しており、変更内容は示せない".into(),
                ],
            }
        })
        .collect()
}

/// 画面の確認用。確認項目に保存した対応づけを、これから実行する試験設定と比べる。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkChangesRequest {
    pub check_id: String,
    pub recipe: Recipe,
}
pub fn link_changes(
    workspace: &crate::workspace::Workspace,
    request: &LinkChangesRequest,
) -> io::Result<Vec<LinkComparison>> {
    let check = workspace
        .checks
        .iter()
        .find(|c| c.id == request.check_id)
        .ok_or_else(|| invalid("確認項目が見つかりません"))?;
    let spec = request
        .recipe
        .trial
        .as_ref()
        .ok_or_else(|| invalid("試験案（判定条件）のない設定です"))?;
    let mut seen = BTreeSet::new();
    Ok(check
        .aspects
        .iter()
        .flat_map(|a| &a.trial_checks)
        .filter(|l| seen.insert((l.id.clone(), l.fingerprint.clone())))
        .map(|l| {
            let current = spec
                .checks
                .iter()
                .find(|c| c.id == l.id)
                .map(|c| (c, &request.recipe));
            compare_link(l, check, current, &workspace.evidence)
        })
        .collect())
}

fn has_withheld(settings: &Value) -> bool {
    settings
        .pointer("/containers/services")
        .and_then(Value::as_array)
        .is_some_and(|services| {
            services.iter().any(|s| {
                s.get("env")
                    .and_then(Value::as_object)
                    .is_some_and(|env| !env.is_empty())
            })
        })
}

fn section(path: &str) -> &'static str {
    let rest = match path.strip_prefix("settings.") {
        Some(rest) => rest,
        None => return "判定条件",
    };
    let head = |p: &str| {
        rest == p
            || [".", "[", "（"]
                .iter()
                .any(|sep| rest.starts_with(&format!("{p}{sep}")))
    };
    for (prefix, label) in [
        ("browser.preparation", "ブラウザの準備"),
        ("browser.actions", "ブラウザ操作"),
        ("browser.responses", "観測するHTTP応答"),
        ("browser.database", "DBの問い合わせ先"),
        ("browser.http_reads", "直接要求する読み出し"),
        ("browser.rejection", "拒否の観測"),
        ("browser", "ブラウザの設定"),
        ("runtime", "実行方式"),
        ("launch", "起動のしかた"),
        ("entrypoint", "操作対象（起動するファイル・HTTPのパス）"),
        ("args", "引数"),
        ("input", "入力"),
        ("fault", "故障の条件"),
        ("storage", "保存先"),
        ("readback", "読み直し"),
        ("recovery", "復旧の操作"),
        ("python", "Pythonの読み込み先と依存先"),
        ("server", "先に起動して待つプロセス"),
        ("containers", "試験用コンテナ"),
        ("http", "HTTP操作"),
    ] {
        if head(prefix) {
            return label;
        }
    }
    "その他の設定"
}

fn shown(value: &str) -> String {
    if value.chars().count() <= MAX_SHOWN {
        return value.to_owned();
    }
    let head: String = value.chars().take(MAX_SHOWN).collect();
    format!("{head}…（以下省略）")
}

/// 配列の要素を、すべてが一意の`id`か`name`を持てばその値で示す。
fn keyed(items: &[Value]) -> Option<Vec<String>> {
    ["id", "name"].iter().find_map(|field| {
        let keys: Vec<String> = items
            .iter()
            .map(|v| v.get(*field).and_then(Value::as_str).map(str::to_owned))
            .collect::<Option<_>>()?;
        let unique: BTreeSet<_> = keys.iter().collect();
        (unique.len() == keys.len()).then_some(keys)
    })
}

/// 表示用の文字列。キーが無い（未指定）ときは None。null・空文字列・空配列・空オブジェクトは区別して示す。
fn leaf(value: Option<&Value>) -> Option<String> {
    Some(match value? {
        Value::Null => "null（明示的に値なし）".into(),
        Value::String(s) if s.is_empty() => "\"\"（空文字列）".into(),
        Value::String(s) => shown(s),
        other => shown(&other.to_string()),
    })
}

/// 値の型の名前。表示が同じ文字になる型の変更を示すために使う。
fn kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "未指定",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "真偽値",
        Some(Value::Number(_)) => "数値",
        Some(Value::String(_)) => "文字列",
        Some(Value::Array(_)) => "配列",
        Some(Value::Object(_)) => "オブジェクト",
    }
}

/// 表示のときに省略されたか（`shown`の上限を超えたか）。
fn shortened(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(s)) => s.chars().count() > MAX_SHOWN,
        Some(Value::Null) | None => false,
        Some(other) => other.to_string().chars().count() > MAX_SHOWN,
    }
}

/// 二つの値を再帰的に比べる。違いの判定は生の値どうしで行い、表示の省略はその後に行う。
/// キーが無いこと（None）と明示的なnullは区別する。識別子のある配列は要素を識別子で対応させ、
/// 並び順の違いも示す。片方にしかない要素は一つの値として示す。
fn walk(
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    out: &mut Vec<(String, Option<String>, Option<String>)>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                walk(&format!("{path}.{key}"), a.get(key), b.get(key), out);
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b)))
            if !a.is_empty() && !b.is_empty() && a.iter().chain(b).all(Value::is_object) =>
        {
            if let (Some(ka), Some(kb)) = (keyed(a), keyed(b)) {
                if ka != kb {
                    out.push((
                        format!("{path}（順序）"),
                        Some(ka.join(" → ")),
                        Some(kb.join(" → ")),
                    ));
                }
                let keys: BTreeSet<&String> = ka.iter().chain(&kb).collect();
                for key in keys {
                    let before = ka.iter().position(|k| k == key).map(|n| &a[n]);
                    let after = kb.iter().position(|k| k == key).map(|n| &b[n]);
                    walk(&format!("{path}[{key}]"), before, after, out);
                }
            } else {
                for n in 0..a.len().max(b.len()) {
                    walk(&format!("{path}[{}]", n + 1), a.get(n), b.get(n), out);
                }
            }
        }
        _ => {
            // ここに来るのは生の値が異なる場合だけ。表示が同じ文字になっても、変更として残し、理由を添える。
            let (mut a, mut b) = (leaf(before), leaf(after));
            if a == b {
                let (from, to) = (kind(before), kind(after));
                if from != to {
                    // 例: 文字列 "true" と真偽値 true。省略はしていない。
                    a = a.map(|text| format!("{text}（{from}）"));
                    b = b.map(|text| format!("{text}（{to}）"));
                } else if shortened(before) || shortened(after) {
                    b = b.map(|text| {
                        format!("{text}［先頭{MAX_SHOWN}文字は対応づけた時点と同じ。省略した部分が異なる］")
                    });
                } else {
                    b = b.map(|text| format!("{text}［表示は同じだが値が異なる］"));
                }
            }
            out.push((path.into(), a, b));
        }
    }
}

/// 二つの記録の違いを、値ごとに示す。
pub fn diff(before: &Value, after: &Value) -> Vec<SettingChange> {
    let mut out = vec![];
    walk("", Some(before), Some(after), &mut out);
    out.into_iter()
        .map(|(full, before, after)| {
            let full = full.trim_start_matches('.');
            let path = full
                .strip_prefix("settings.")
                .or_else(|| full.strip_prefix("check."))
                .unwrap_or(full);
            SettingChange {
                section: section(full).into(),
                path: path.into(),
                before,
                after,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    fn aspect(kind: Option<ObservationKind>, links: &[&str]) -> Aspect {
        Aspect {
            id: "a".into(),
            text: "合成の部分".into(),
            origin: "試験".into(),
            observation: kind,
            trial_checks: links
                .iter()
                .map(|s| TrialLink {
                    id: s.to_string(),
                    fingerprint: FP.into(),
                    label: String::new(),
                    definition: None,
                    basis: None,
                })
                .collect(),
        }
    }
    fn item(id: &str, kind: ObservationKind, observed: Option<Observed>) -> Item {
        Item {
            id: id.into(),
            kind,
            scope: "範囲".into(),
            fingerprint: FP.into(),
            observed,
        }
    }
    fn stored(aspects: Vec<Aspect>, link_settings: Vec<SettingsRecord>) -> Check {
        Check {
            id: "concern".into(),
            version: 1,
            text: "合成の確認項目".into(),
            origin: "試験".into(),
            source: None,
            observation: None,
            rule: crate::workspace::Rule::ManualOnly,
            dependencies: vec![],
            questions: vec![],
            aspects,
            link_settings,
        }
    }
    fn trial_check(id: &str, text: &str) -> TrialCheck {
        TrialCheck {
            id: id.into(),
            scope: "範囲".into(),
            method: CheckMethod::DatabaseRows,
            expect: crate::trial::Expectation::Contains,
            text: Some(text.into()),
            target: Some("t".into()),
            relation: None,
        }
    }
    fn recipe(checks: Vec<TrialCheck>, args: &[&str]) -> Recipe {
        Recipe {
            schema_version: 1,
            check_id: "concern".into(),
            source: crate::workspace::CodeReference {
                repository: "example/notes".into(),
                commit: "a".repeat(40),
                path: "notes.py".into(),
                line: 1,
                end_line: 1,
            },
            runtime: crate::execution::Runtime::Python3,
            entrypoint: "notes.py".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            input: serde_json::json!({}),
            criterion: Criterion {
                assertion: "eel.trial".into(),
                subject: "concern".into(),
            },
            timeout_ms: 1000,
            trial: Some(crate::trial::TrialSpec {
                schema_version: 1,
                fault: crate::trial::TrialFault::Normal,
                storage: None,
                readback: None,
                checks,
                recovery: None,
            }),
            launch: Default::default(),
            python: None,
            server: None,
            containers: None,
            http: None,
            browser: None,
        }
    }

    #[test]
    fn same_id_with_changed_definition_is_not_observed_and_definitions_become_fingerprints() {
        let a = trial_check("c1", "患者A");
        let b = trial_check("c1", "患者B");
        let ra = recipe(vec![a.clone()], &["patient-a"]);
        let rb = recipe(vec![b.clone()], &["patient-a"]);
        assert_ne!(
            trial_check_fingerprint(&a, &ra),
            trial_check_fingerprint(&b, &rb)
        );
        // 判定条件の文面が同じでも、操作対象（引数）が変われば一致しない。固定版と待機上限の違いは含めない。
        let other_patient = recipe(vec![a.clone()], &["patient-b"]);
        assert_ne!(
            trial_check_fingerprint(&a, &ra),
            trial_check_fingerprint(&a, &other_patient)
        );
        let mut other_commit = ra.clone();
        other_commit.source.commit = "b".repeat(40);
        other_commit.timeout_ms = 9;
        assert_eq!(
            trial_check_fingerprint(&a, &ra),
            trial_check_fingerprint(&a, &other_commit)
        );
        let mut aspect = aspect(Some(ObservationKind::DatabaseRows), &[]);
        aspect.trial_checks.push(TrialLink {
            id: "c1".into(),
            fingerprint: String::new(),
            label: String::new(),
            definition: Some(LinkDefinition {
                check: a.clone(),
                recipe: ra.clone(),
            }),
            basis: None,
        });
        let (saved, settings) = normalize(vec![aspect], &stored(vec![], vec![])).unwrap();
        let link = &saved[0].trial_checks[0];
        assert_eq!(link.fingerprint, trial_check_fingerprint(&a, &ra));
        assert!(link.definition.is_none());
        assert!(link.label.contains("範囲") && link.label.contains("patient-a"));
        assert_eq!(link.basis.as_ref().unwrap().check, a);
        assert_eq!(settings.len(), 1);
        let mut changed = item("c1", ObservationKind::DatabaseRows, Some(Observed::Pass));
        changed.fingerprint = trial_check_fingerprint(&a, &other_patient);
        assert_eq!(
            status_of(&saved[0], true, &[changed]).0,
            AspectStatus::DefinitionChanged
        );
        let mut same = item("c1", ObservationKind::DatabaseRows, Some(Observed::Pass));
        same.fingerprint = trial_check_fingerprint(&a, &ra);
        assert_eq!(
            status_of(&saved[0], true, &[same]).0,
            AspectStatus::Observed
        );
        // 内容を送らない対応づけは保存済みの指紋を保ち、現在の試験に合わせて更新しない。
        let check = stored(saved.clone(), settings.clone());
        let (kept, kept_settings) = normalize(saved.clone(), &check).unwrap();
        assert_eq!(kept[0].trial_checks[0].fingerprint, link.fingerprint);
        assert_eq!(kept[0].trial_checks[0].basis, link.basis);
        assert_eq!(kept_settings, settings);
        // 内容も指紋もない対応づけ、識別子や内容が試験の設定と食い違う対応づけは拒否する。
        let mut bare = super::Aspect {
            trial_checks: vec![TrialLink {
                id: "c1".into(),
                fingerprint: String::new(),
                label: String::new(),
                definition: None,
                basis: None,
            }],
            ..saved[0].clone()
        };
        let empty = stored(vec![], vec![]);
        assert!(normalize(vec![bare.clone()], &empty).is_err());
        bare.trial_checks[0].definition = Some(LinkDefinition {
            check: trial_check("c2", "x"),
            recipe: ra.clone(),
        });
        assert!(normalize(vec![bare.clone()], &empty).is_err());
        bare.trial_checks[0].definition = Some(LinkDefinition {
            check: b.clone(),
            recipe: ra,
        });
        assert!(
            normalize(vec![bare], &empty).is_err(),
            "試験の設定にない内容"
        );
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

    fn bundled(n: usize) -> Recipe {
        let catalog = crate::browser_recipes::catalog().unwrap();
        serde_json::from_value(catalog["recipes"][n]["recipe"].clone()).unwrap()
    }
    fn link_to(recipe: &Recipe, id: &str) -> (Vec<Aspect>, Vec<SettingsRecord>) {
        let check = recipe
            .trial
            .as_ref()
            .unwrap()
            .checks
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .clone();
        let mut a = aspect(Some(ObservationKind::of_method(check.method)), &[]);
        a.trial_checks.push(TrialLink {
            id: id.into(),
            fingerprint: String::new(),
            label: String::new(),
            definition: Some(LinkDefinition {
                check,
                recipe: recipe.clone(),
            }),
            basis: None,
        });
        normalize(vec![a], &stored(vec![], vec![])).unwrap()
    }
    fn current<'a>(recipe: &'a Recipe, id: &str) -> Option<(&'a TrialCheck, &'a Recipe)> {
        recipe
            .trial
            .as_ref()
            .unwrap()
            .checks
            .iter()
            .find(|c| c.id == id)
            .map(|c| (c, recipe))
    }

    /// ブラウザ操作・DBの問い合わせ先・故障の変更を、対応づけた時点と現在の値で示す。
    /// 環境変数の値は記録に残さず、値だけの変更は「比較できない」として示す。
    #[test]
    fn changed_settings_are_listed_with_before_and_after_and_secrets_are_not_recorded() {
        let normal = bundled(0);
        let (aspects, settings) = link_to(&normal, "soap-subjective-row");
        let check = stored(aspects.clone(), settings.clone());
        let saved = serde_json::to_string(&check).unwrap();
        assert!(
            !saved.contains("eel-root-trial") && !saved.contains("eel-pass-trial"),
            "試験用の認証情報を対応づけの記録に残さない"
        );
        assert!(saved.contains("MYSQL_ROOT_PASSWORD") && saved.contains(WITHHELD));
        let link = &aspects[0].trial_checks[0];
        let same = compare_link(link, &check, current(&normal, "soap-subjective-row"), &[]);
        assert_eq!(same.state, LinkState::Unchanged);
        assert!(same.changes.is_empty());

        // DB停止の試験: 故障の条件と、保存前の操作が準備へ移ったことを示す。
        let stopped = bundled(1);
        let c = compare_link(link, &check, current(&stopped, "soap-subjective-row"), &[]);
        assert_eq!(c.state, LinkState::Changed);
        assert_eq!(
            c.basis_source.as_deref(),
            Some("対応づけた時点に麦漆が保存した設定")
        );
        let fault = c
            .changes
            .iter()
            .find(|x| x.section == "故障の条件")
            .unwrap();
        assert_eq!(fault.before.as_deref(), Some("normal"));
        assert_eq!(fault.after.as_deref(), Some("database_stopped"));
        assert!(c.changes.iter().any(|x| x.section == "ブラウザの準備"));
        let listed: Vec<_> = c
            .changes
            .iter()
            .map(|x| (&x.section, &x.path, &x.before, &x.after))
            .collect();
        assert!(
            c.changes
                .iter()
                .any(|x| x.section == "ブラウザ操作" && x.path.ends_with("（順序）")),
            "{listed:#?}"
        );
        assert!(c.uncomparable.iter().any(|u| u.contains("環境変数")));

        // DBの問い合わせ先（表）だけを変える。
        let mut table = normal.clone();
        table.browser.as_mut().unwrap().database[0].table = "other_table".into();
        let c = compare_link(link, &check, current(&table, "soap-subjective-row"), &[]);
        assert_eq!(c.state, LinkState::Changed);
        let changed: Vec<_> = c
            .changes
            .iter()
            .map(|x| (x.section.as_str(), x.after.as_deref()))
            .collect();
        assert_eq!(
            changed,
            vec![("DBの問い合わせ先", Some("other_table"))],
            "{c:?}"
        );

        // 環境変数の値だけを変える。指紋は変わるが、記録した項目には違いがない。
        let mut env = normal.clone();
        env.containers.as_mut().unwrap().services[0]
            .env
            .insert("MYSQL_ROOT_PASSWORD".into(), "eel-other-trial".into());
        let c = compare_link(link, &check, current(&env, "soap-subjective-row"), &[]);
        assert_eq!(c.state, LinkState::Changed);
        assert!(c.changes.is_empty());
        assert!(c.uncomparable[0].contains("記録しない値"), "{c:?}");
        assert!(
            !serde_json::to_string(&c)
                .unwrap()
                .contains("eel-other-trial")
        );

        // 判定条件そのものの変更は「判定条件」の区分で示す。
        let mut expect = normal.clone();
        expect
            .trial
            .as_mut()
            .unwrap()
            .checks
            .iter_mut()
            .find(|c| c.id == "soap-subjective-row")
            .unwrap()
            .expect = crate::trial::Expectation::NotContains;
        let c = compare_link(link, &check, current(&expect, "soap-subjective-row"), &[]);
        assert_eq!(c.changes.len(), 1, "{c:?}");
        assert_eq!(c.changes[0].section, "判定条件");
        assert_eq!(c.changes[0].path, "expect");

        // 試験にない判定条件。
        let c = compare_link(link, &check, None, &[]);
        assert_eq!(c.state, LinkState::NotInTrial);
    }

    /// この機能より前の対応づけ（記録なし）は、推測で補わず比較できないと示す。
    #[test]
    fn legacy_links_without_a_recorded_basis_are_reported_as_uncomparable() {
        let normal = bundled(0);
        let (mut aspects, _) = link_to(&normal, "soap-subjective-row");
        aspects[0].trial_checks[0].basis = None;
        let check = stored(aspects.clone(), vec![]);
        let c = compare_link(
            &aspects[0].trial_checks[0],
            &check,
            current(&bundled(1), "soap-subjective-row"),
            &[],
        );
        assert_eq!(c.state, LinkState::Changed);
        assert!(c.basis_source.is_none() && c.changes.is_empty());
        assert!(c.uncomparable[0].contains("保存されていない"));
    }

    /// 対応づけた時点の記録は利用者から受け取らず、保存済みの対応づけの説明も上書きさせない。
    #[test]
    fn the_basis_and_label_of_a_saved_link_are_kept_by_the_server() {
        let normal = bundled(0);
        let (aspects, settings) = link_to(&normal, "soap-subjective-row");
        let check = stored(aspects.clone(), settings.clone());
        let mut resent = aspects.clone();
        resent[0].trial_checks[0].label = "書き換えた説明".into();
        resent[0].trial_checks[0].basis = None;
        let (kept, kept_settings) = normalize(resent, &check).unwrap();
        assert_eq!(kept[0].trial_checks[0], aspects[0].trial_checks[0]);
        assert_eq!(kept_settings, settings);
        // 保存済みにない対応づけに、利用者が記録を付けて送ることはできない。
        let mut forged = aspects.clone();
        forged[0].trial_checks[0].fingerprint = "f".repeat(64);
        assert!(normalize(forged, &check).is_err());
        // 記録の識別子と内容が合わない保存データは読み込めない。
        let mut tampered = settings.clone();
        tampered[0].settings["fault"] = serde_json::json!("normal_but_edited");
        assert!(validate_settings(&aspects, &tampered).is_err());
        assert!(
            validate_settings(&aspects, &[]).is_err(),
            "参照先のない記録"
        );
        assert!(
            validate_settings(&[], &settings).is_err(),
            "参照されない記録"
        );
    }

    /// 復旧の操作（故障を解除して読み直す）は、実行される操作とログに映る範囲を変えるため指紋に含める。
    /// 復旧後の待機上限の値は含めない（観測の内容ではなく待ち方）。
    #[test]
    fn recovery_operation_is_part_of_the_fingerprint_but_its_wait_limit_is_not() {
        let without = bundled(1);
        assert!(without.trial.as_ref().unwrap().recovery.is_none());
        let mut with = without.clone();
        with.trial.as_mut().unwrap().recovery = Some(crate::trial::Recovery {
            ready_timeout_ms: 180000,
        });
        let mut shorter = with.clone();
        shorter.trial.as_mut().unwrap().recovery = Some(crate::trial::Recovery {
            ready_timeout_ms: 60000,
        });
        let id = "soap-subjective-row";
        let check = without
            .trial
            .as_ref()
            .unwrap()
            .checks
            .iter()
            .find(|c| c.id == id)
            .unwrap()
            .clone();
        assert_ne!(
            trial_check_fingerprint(&check, &without),
            trial_check_fingerprint(&check, &with),
            "復旧の操作の有無で指紋が変わる"
        );
        assert_eq!(
            trial_check_fingerprint(&check, &with),
            trial_check_fingerprint(&check, &shorter),
            "待機上限の値だけでは指紋は変わらない"
        );
        // 復旧のない設定の指紋と記録は、この変更の前と同じ形（保存済みの対応づけが無効にならない）。
        assert!(recorded_settings(&without).get("recovery").is_none());

        // 復旧なしで対応づけ、別の部分を足して保存しても前の指紋を保つ。復旧ありの試行では内容が変わったとする。
        let (aspects, settings) = link_to(&without, id);
        let stored_check = stored(aspects.clone(), settings.clone());
        let mut resent = aspects.clone();
        resent.push(Aspect {
            id: "other".into(),
            ..aspect(None, &[])
        });
        let (kept, _) = normalize(resent, &stored_check).unwrap();
        assert_eq!(kept[0].trial_checks[0], aspects[0].trial_checks[0]);
        let mut changed = item(id, ObservationKind::DatabaseRows, Some(Observed::Pass));
        changed.fingerprint = trial_check_fingerprint(&check, &with);
        assert_eq!(
            status_of(&kept[0], true, &[changed]).0,
            AspectStatus::DefinitionChanged
        );
        // 比較では、復旧の操作が加わったことを操作の差として示す。
        let c = compare_link(
            &aspects[0].trial_checks[0],
            &stored_check,
            current(&with, id),
            &[],
        );
        assert_eq!(c.state, LinkState::Changed);
        let recovery: Vec<_> = c
            .changes
            .iter()
            .filter(|x| x.section == "復旧の操作")
            .collect();
        assert_eq!(recovery.len(), 1, "{c:?}");
        assert_eq!(recovery[0].before, None);
        assert_eq!(recovery[0].after.as_deref(), Some("true"));
        assert!(
            !c.changes
                .iter()
                .any(|x| x.after.as_deref() == Some("180000")),
            "待機上限は記録しない"
        );
    }

    /// 違いの判定は省略・表示の前に生の値で行う。先頭300文字が同じ長い値、null・空配列・空オブジェクト、
    /// キーの未指定と明示的なnullを、「変更なし」として落とさない。
    #[test]
    fn differences_are_decided_on_raw_values_before_shortening_or_empty_values() {
        let long = |tail: &str| format!("{}{tail}", "x".repeat(MAX_SHOWN + 20));
        let changes = diff(
            &serde_json::json!({"settings": {"args": [long("A")]}}),
            &serde_json::json!({"settings": {"args": [long("B")]}}),
        );
        assert_eq!(changes.len(), 1, "末尾だけが違う長い引数: {changes:?}");
        assert_eq!(changes[0].section, "引数");
        assert_ne!(
            changes[0].before, changes[0].after,
            "省略した部分が違うことを示す"
        );
        let text = |v: serde_json::Value| serde_json::json!({"settings": {"input": v}});
        let cases = [
            (
                serde_json::json!({}),
                serde_json::json!({"a": null}),
                "未指定→null",
            ),
            (
                serde_json::json!({"a": null}),
                serde_json::json!({}),
                "null→未指定",
            ),
            (
                serde_json::json!({"a": null}),
                serde_json::json!({"a": []}),
                "null→空配列",
            ),
            (
                serde_json::json!({"a": []}),
                serde_json::json!({"a": {}}),
                "空配列→空オブジェクト",
            ),
            (
                serde_json::json!({"a": {}}),
                serde_json::json!({"a": null}),
                "空オブジェクト→null",
            ),
            (
                serde_json::json!({"a": []}),
                serde_json::json!({}),
                "空配列→未指定",
            ),
            (
                serde_json::json!({"a": ""}),
                serde_json::json!({"a": null}),
                "空文字列→null",
            ),
        ];
        for (before, after, label) in cases {
            let changes = diff(&text(before), &text(after));
            assert_eq!(changes.len(), 1, "{label}: {changes:?}");
            assert_eq!(changes[0].path, "input.a", "{label}");
            assert_eq!(changes[0].section, "入力", "{label}");
            assert_ne!(changes[0].before, changes[0].after, "{label}: 表示も異なる");
        }
        // 同じ値なら変更は出ない。
        assert!(
            diff(
                &text(serde_json::json!({"a": null})),
                &text(serde_json::json!({"a": null}))
            )
            .is_empty()
        );
    }

    /// 表示が同じ文字になる型の変更（文字列と真偽値・数値・配列など）は、型の違いとして示し、
    /// 省略していない値に「省略した部分が異なる」とは書かない。
    #[test]
    fn short_type_changes_are_shown_as_type_changes_not_as_omitted_parts() {
        let input = |v: serde_json::Value| serde_json::json!({"settings": {"input": {"a": v}}});
        for (before, after, from, to) in [
            (
                serde_json::json!("true"),
                serde_json::json!(true),
                "文字列",
                "真偽値",
            ),
            (
                serde_json::json!(42),
                serde_json::json!("42"),
                "数値",
                "文字列",
            ),
            (
                serde_json::json!([]),
                serde_json::json!("[]"),
                "配列",
                "文字列",
            ),
            (
                serde_json::json!({}),
                serde_json::json!("{}"),
                "オブジェクト",
                "文字列",
            ),
        ] {
            let changes = diff(&input(before.clone()), &input(after.clone()));
            assert_eq!(changes.len(), 1, "{before} → {after}: {changes:?}");
            let (b, a) = (
                changes[0].before.as_deref().unwrap(),
                changes[0].after.as_deref().unwrap(),
            );
            assert!(
                b.contains(from) && a.contains(to),
                "{before} → {after}: {b} / {a}"
            );
            assert!(
                !b.contains("省略") && !a.contains("省略"),
                "{before} → {after}: {b} / {a}"
            );
        }
    }
}
