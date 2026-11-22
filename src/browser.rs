//! 宣言した手順だけを試験用Chromiumで操作する。ログイン準備と試験操作を分ける。
use crate::error::invalid;
use crate::{
    container::{ContainerSpec, Session as Containers},
    recording::write_json_new,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case", deny_unknown_fields)]
pub enum Locator {
    Label { text: String },
    Role { role: String, name: String },
    Css { selector: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default)]
    pub frames: Vec<String>,
    pub locator: Locator,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Literal { value: String },
    Secret { key: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Goto {
        path: String,
    },
    Fill {
        target: Target,
        value: Input,
    },
    Click {
        target: Target,
    },
    Expand {
        target: Target,
        content: Target,
    },
    /// 準備用の案内を閉じ、非表示を確認する。通常のクリック操作は再試行しない。
    Dismiss {
        target: Target,
        content: Target,
    },
    Select {
        target: Target,
        value: String,
    },
    ReadText {
        target: Target,
        #[serde(default)]
        contains: Option<String>,
    },
    ReadCanvas {
        target: Target,
        min_levels: u16,
        min_pixels: u32,
    },
    /// 要素の属性を読む。要素が見つからなければ手順は未完了になる。
    ReadAttribute {
        target: Target,
        name: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectionWatch {
    pub watch: String,
    pub status: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(flatten)]
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_response: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseWatch {
    pub id: String,
    pub path: String,
    pub method: String,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub payload: Option<crate::http_probe::Payload>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseProbe {
    pub id: String,
    pub service: String,
    pub database: String,
    pub table: String,
    /// PostgreSQLで、表のあるスキーマ。省略すると、接続する利用者の検索の順（search_path）で表を探す。
    /// MariaDBでは `database` がこれにあたるので、指定を受け付けない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub match_column: String,
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_value: Option<String>,
    /// 試験値の代わりに、前に書いた読み出しの1行の列の値で行を絞る。外部キーでつながった行
    /// （患者の行のIDから連絡先の行など）を読むために使う。`match_value` とは同時に指定できない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_from: Option<MatchFrom>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub before_actions: bool,
}
/// 別の読み出しの結果の列。参照先は、一覧で前にある読み出しに限る（順に読み、循環させない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchFrom {
    pub read: String,
    /// 参照先の `columns` の位置（0から）。
    pub column: usize,
}
fn is_false(value: &bool) -> bool {
    !value
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSpec {
    pub service: String,
    pub tls: bool,
    pub start_path: String,
    pub expected_version: String,
    #[serde(default)]
    pub preparation: Vec<Step>,
    pub actions: Vec<Step>,
    #[serde(default)]
    pub responses: Vec<ResponseWatch>,
    #[serde(default)]
    pub database: Vec<DatabaseProbe>,
    #[serde(default)]
    pub http_reads: Vec<crate::http_probe::Probe>,
    #[serde(default)]
    pub rejection: Option<RejectionWatch>,
    /// 入力した試験値が、DB・HTTP・画面・ログのどこに現れたかを記録する。記録のみで、判定には使わない。
    #[serde(default, skip_serializing_if = "is_false")]
    pub track_values: bool,
    /// 入力の手順ごとに異なる試験値を割り当て、どの欄の値がどこに現れたかを区別できるようにする。
    #[serde(default, skip_serializing_if = "is_false")]
    pub value_per_step: bool,
    /// 観測から作った項目（手順、応答の観測先、DBの読み出し先、判定条件）の出所。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::tracking::Provenance>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasRead {
    #[serde(default)]
    pub measurement: String,
    pub width: u32,
    pub height: u32,
    pub levels: u16,
    pub non_black_pixels: u32,
    pub rgba_sha256: String,
    pub variation: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub id: String,
    pub phase: String,
    pub completed: bool,
    pub text: Option<String>,
    pub screenshot: Option<String>,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<CanvasRead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<AttributeRead>,
    /// 追跡する値のうち、この手順が完了した時点で画面に表示されていたもの。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visible_slots: Vec<String>,
}
/// 属性の読み取り。要素はあるが属性がない場合は `present: false`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttributeRead {
    pub present: bool,
    pub value: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRecord {
    pub watch: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub body_match: Option<bool>,
    pub request_step: Option<String>,
    #[serde(default)]
    pub payload_sha256: Option<String>,
    /// 記録の対象にしている名前の応答ヘッダー。変更前の記録にはない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    /// 上限で後半を切り捨てて記録した応答ヘッダーの名前。該当がなければ保存しない。
    /// 切り捨てた部分に何があったかは分からないので、判定では「含まない」と扱わない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers_truncated: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserReport {
    pub schema_version: u32,
    pub run_id: String,
    pub version: String,
    #[serde(default)]
    pub launch_args: Vec<String>,
    pub prepared: bool,
    pub completed: bool,
    pub steps: Vec<StepRecord>,
    pub responses: Vec<ResponseRecord>,
    pub blocked_requests: u32,
    #[serde(default)]
    pub response_limit: bool,
    pub detail: String,
    /// 操作中の、対象のオリジンへのHTTPのやり取りの全件。本文とクエリ文字列は持たない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exchanges: Vec<crate::screen::ExchangeRecord>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub exchange_limit: bool,
    /// 手順ごとの画面の構造の記録（`browser/screens.json`）への参照。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<crate::screen::ScreenRef>,
    /// 画面の構造を記録できたか。変更前の記録では空で、画面が空だったことは意味しない。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub screens_note: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseRead {
    pub id: String,
    pub performed: bool,
    pub exit_code: Option<i32>,
    pub text: String,
    pub detail: String,
}

fn id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn bounded(s: &str) -> bool {
    !s.is_empty() && s.len() <= 2048 && !s.contains('\0')
}
fn path(s: &str) -> bool {
    s.starts_with('/')
        && !s.starts_with("//")
        && !s.contains(['\\', '\r', '\n', '\0'])
        && s.len() <= 2048
}
fn target(t: &Target) -> bool {
    t.frames.len() <= 8
        && t.frames.iter().all(|f| bounded(f))
        && match &t.locator {
            Locator::Label { text } => bounded(text),
            Locator::Role { role, name } => {
                [
                    "button", "textbox", "link", "combobox", "checkbox", "menuitem", "tab",
                    "heading",
                ]
                .contains(&role.as_str())
                    && bounded(name)
            }
            Locator::Css { selector } => bounded(selector),
        }
}
/// 読み取る属性の名前。HTMLの属性名に使う文字だけを受け付ける。
pub fn attribute_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':')
}
/// 応答ヘッダーの判定条件の文（`<名前>: <値の一部>`）を、記録の対象の名前と値に分ける。
pub fn header_condition(text: &str) -> Option<(&'static str, &str)> {
    let (name, value) = text.split_once(':')?;
    let name = name.trim().to_ascii_lowercase();
    crate::screen::HEADER_NAMES
        .iter()
        .find(|known| **known == name)
        .map(|known| (*known, value.trim()))
}
pub fn sql_identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub fn validate(spec: &BrowserSpec, containers: &ContainerSpec) -> io::Result<()> {
    if !containers
        .services
        .iter()
        .any(|s| s.name == spec.service && s.ready != crate::container::ReadyCheck::Completed)
        || !path(&spec.start_path)
        || spec.expected_version.is_empty()
        || spec.expected_version.len() > 64
        || !spec
            .expected_version
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.')
        || spec.actions.is_empty()
        || spec.actions.len() + spec.preparation.len() > 96
        || spec.responses.len() > 16
        || spec.database.len() > 16
        || spec.http_reads.len() > 16
    {
        return Err(invalid(
            "ブラウザの操作先、固定した版、手順の上限を確認してください",
        ));
    }
    if spec.value_per_step && !spec.track_values {
        return Err(invalid(
            "手順ごとの試験値は、値の追跡を有効にした試験でだけ使えます",
        ));
    }
    if let Some(provenance) = &spec.provenance {
        crate::tracking::validate_provenance(provenance, spec)?;
    }
    let mut ids = std::collections::BTreeSet::new();
    for s in spec.preparation.iter().chain(&spec.actions) {
        if !id(&s.id) || !ids.insert(&s.id) {
            return Err(invalid("ブラウザの手順には重複しない識別子が必要です"));
        }
        if s.wait_response.as_ref().is_some_and(|watch| {
            !matches!(s.action, Action::Click { .. })
                || !spec.responses.iter().any(|response| response.id == *watch)
        }) {
            return Err(invalid(
                "応答を待つ手順は、登録済みの応答先を指定したクリック操作にしてください",
            ));
        }
        let valid = match &s.action {
            Action::Goto { path: p } => path(p),
            Action::Fill { target: t, value } => {
                target(t)
                    && match value {
                        Input::Literal { value } => value.len() <= 16384 && !value.contains('\0'),
                        Input::Secret { key } => id(key),
                    }
            }
            Action::Click { target: t } => target(t),
            Action::Expand { target: t, content } => target(t) && target(content),
            Action::Dismiss { target: t, content } => {
                target(t)
                    && target(content)
                    && spec.preparation.iter().any(|prepared| prepared.id == s.id)
            }
            Action::ReadText {
                target: t,
                contains,
            } => target(t) && contains.as_ref().is_none_or(|s| bounded(s)),
            Action::Select { target: t, value } => target(t) && bounded(value),
            Action::ReadAttribute { target: t, name } => target(t) && attribute_name(name),
            Action::ReadCanvas {
                target: t,
                min_levels,
                min_pixels,
            } => {
                target(t) && (2..=256).contains(min_levels) && (1..=2_000_000).contains(min_pixels)
            }
        };
        if !valid {
            return Err(invalid("ブラウザの選択要素と入力値を確認してください"));
        }
    }
    ids.clear();
    for w in &spec.responses {
        if !id(&w.id)
            || !ids.insert(&w.id)
            || !path(&w.path)
            || w.path.contains(['?', '#'])
            || !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&w.method.as_str())
            || w.contains.as_ref().is_some_and(|s| !bounded(s))
        {
            return Err(invalid("HTTP応答の観測先を確認してください"));
        }
    }
    ids.clear();
    for probe in &spec.http_reads {
        if !id(&probe.id)
            || !ids.insert(&probe.id)
            || !path(&probe.path)
            || probe.path.contains('#')
            || !containers.services.iter().any(|s| s.name == probe.service)
        {
            return Err(invalid("サーバーの直接読取先を確認してください"));
        }
    }
    if let Some(rejection) = &spec.rejection
        && (!(400..=599).contains(&rejection.status)
            || !spec.responses.iter().any(|w| w.id == rejection.watch))
    {
        return Err(invalid("HTTP拒否を確認する観測先と状態コードが必要です"));
    }
    ids.clear();
    for q in &spec.database {
        if !id(&q.id)
            || !ids.insert(&q.id)
            || !containers
                .services
                .iter()
                .any(|s| s.name == q.service && s.role == crate::container::ServiceRole::Database)
            || ![&q.database, &q.table, &q.match_column]
                .iter()
                .all(|s| sql_identifier(s))
            || q.schema.as_ref().is_some_and(|s| !sql_identifier(s))
            || q.columns.is_empty()
            || q.columns.len() > 16
            || !q.columns.iter().all(|c| sql_identifier(c))
            || q.match_value.as_ref().is_some_and(|value| {
                value.len() > 512
                    || value.contains('\0')
                    || !value.contains(crate::trial::TEST_VALUE)
            })
        {
            return Err(invalid(
                "DBの読取先は試験用DBと英数字・下線の識別子で指定してください",
            ));
        }
        if let Some(from) = &q.match_from {
            // 参照先は一覧で前にあるものに限る。自分自身や後ろを指す指定（循環を含む）は受け付けない。
            let earlier = spec
                .database
                .iter()
                .take_while(|p| !std::ptr::eq(*p, q))
                .find(|p| p.id == from.read);
            let ok = q.match_value.is_none()
                && earlier.is_some_and(|p| {
                    from.column < p.columns.len() && (!q.before_actions || p.before_actions)
                });
            if !ok {
                return Err(invalid(
                    "DBの読取先の match_from は、前にある読取先のIDと、その列の位置を指定してください（match_value とは同時に指定できず、操作前に読む場合は参照先も操作前に読むものに限ります）",
                ));
            }
        }
        if q.schema.is_some()
            && !containers.services.iter().any(|s| {
                s.name == q.service && s.engine == crate::database::DatabaseEngine::Postgres
            })
        {
            return Err(invalid(
                "DBの読取先のスキーマは、DBの種類がPostgreSQLの場合だけ指定できます",
            ));
        }
    }
    Ok(())
}
pub fn rejection_observed(spec: &BrowserSpec, report: &BrowserReport) -> bool {
    let Some(rejection) = &spec.rejection else {
        return false;
    };
    let responses = report
        .responses
        .iter()
        .filter(|r| r.watch == rejection.watch)
        .collect::<Vec<_>>();
    report.prepared
        && !report.response_limit
        && !responses.is_empty()
        && responses.iter().all(|r| {
            r.status == rejection.status
                && r.request_step.as_ref().is_some_and(|id| {
                    spec.actions.iter().any(|s| &s.id == id)
                        && report
                            .steps
                            .iter()
                            .any(|s| &s.id == id && s.phase == "action")
                })
        })
}
/// 列名と検索値を別々に検査し、1件の検索値に一致する行だけを読む。任意のSQLは受け取らない。
/// 検索値は `read_all` が決めたもの（試験値、`match_value`、`match_from` の参照先の値）。
pub fn query(engine: crate::database::DatabaseEngine, q: &DatabaseProbe, value: &str) -> String {
    engine.rows_sql(
        q.schema.as_deref(),
        &q.table,
        &q.match_column,
        &q.columns,
        value,
    )
}

/// 読み出しを一覧の順に行う。`match_from` の読み出しは、同じ回で先に読んだ参照先の結果から
/// 検索値を取る。参照先が1行でない、または列が空・NULLのときは読まず、理由を残す
/// （行が無いこととは扱わない）。
pub fn read_all<'a>(
    probes: impl IntoIterator<Item = &'a DatabaseProbe>,
    test_value: &str,
    mut read: impl FnMut(&DatabaseProbe, &str) -> DatabaseRead,
) -> Vec<DatabaseRead> {
    let mut done: Vec<DatabaseRead> = vec![];
    for probe in probes {
        let value = match &probe.match_from {
            Some(from) => match done.iter().find(|r| r.id == from.read) {
                Some(source) => single_value(source, from.column),
                None => Err(format!(
                    "参照先の読み出し {} を、この回で読んでいない",
                    from.read
                )),
            },
            None => Ok(probe.match_value.as_deref().map_or_else(
                || test_value.to_owned(),
                |value| crate::trial::substitute(value, test_value),
            )),
        };
        done.push(match value {
            Ok(value) => read(probe, &value),
            Err(reason) => DatabaseRead {
                id: probe.id.clone(),
                performed: false,
                exit_code: None,
                text: String::new(),
                detail: format!("DBの行を読み出していない：{reason}"),
            },
        });
    }
    done
}

/// 読み出しの結果がちょうど1行のとき、その行の指定した列の値を返す。
fn single_value(read: &DatabaseRead, column: usize) -> Result<String, String> {
    if !read.performed || read.exit_code != Some(0) {
        return Err(format!("参照先の読み出し {} が失敗した", read.id));
    }
    let rows: Vec<_> = read.text.lines().filter(|l| !l.is_empty()).collect();
    if rows.len() != 1 {
        return Err(format!(
            "参照先の読み出し {} の行が1行ではない（{}行）",
            read.id,
            rows.len()
        ));
    }
    match rows[0].split('\t').nth(column) {
        Some(value) if !value.is_empty() && value != "NULL" => Ok(value.to_owned()),
        _ => Err(format!(
            "参照先の読み出し {} の列 {column} が空かNULL",
            read.id
        )),
    }
}
fn unavailable(run_id: &str, detail: &str) -> BrowserReport {
    BrowserReport {
        schema_version: 1,
        run_id: run_id.into(),
        version: String::new(),
        launch_args: vec![],
        prepared: false,
        completed: false,
        steps: vec![],
        responses: vec![],
        blocked_requests: 0,
        response_limit: false,
        detail: detail.into(),
        exchanges: vec![],
        exchange_limit: false,
        screens: vec![],
        screens_note: String::new(),
    }
}
pub struct Session {
    child: crate::process::ChildGuard,
    directory: PathBuf,
    deadline: Instant,
    report: BrowserReport,
}
impl Session {
    pub fn prepare(
        spec: &BrowserSpec,
        containers: &Containers,
        directory: &Path,
        run_id: &str,
        test_value: &str,
        timeout_ms: u64,
    ) -> io::Result<Self> {
        let module = match std::env::var_os("EEL_PLAYWRIGHT_MODULE") {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir()?.join("node_modules/playwright/index.mjs"),
        };
        let module=fs::canonicalize(module).map_err(|_|invalid("ブラウザ試験にはサーバー側のPlaywrightが必要です。npm ciを実行するかEEL_PLAYWRIGHT_MODULEを指定してください"))?;
        let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("node"))
            .find(|p| p.is_file())
            .ok_or_else(|| invalid("ブラウザ試験にはNode.jsが必要です"))?;
        let directory = directory.join("browser");
        fs::create_dir(&directory)?;
        let directory = fs::canonicalize(directory)?;
        let origin = format!(
            "{}://127.0.0.1:{}",
            if spec.tls { "https" } else { "http" },
            containers.port_of(&spec.service)?
        );
        let origins = containers
            .ports()
            .values()
            .flat_map(|p| {
                [
                    format!("http://127.0.0.1:{p}"),
                    format!("https://127.0.0.1:{p}"),
                ]
            })
            .collect::<Vec<_>>();
        // 追跡する値と、手順ごとの試験値を操作器へ渡す。追跡しない試験では空で、従来と同じ動作になる。
        let tracked = crate::tracking::values_of(spec, test_value);
        let config = serde_json::json!({"schema_version":1,"spec":spec,"run_id":run_id,"test_value":test_value,"origin":origin,"origins":origins,"timeout_ms":timeout_ms,
            "tracked": tracked.iter().map(|t| serde_json::json!({"slot": t.slot, "value": t.value})).collect::<Vec<_>>(),
            "step_values": crate::tracking::step_values(spec, test_value)});
        write_json_new(&directory.join("request.json"), &config)?;
        fs::write(
            directory.join("runner.mjs"),
            include_str!("../tools/browser-runner.mjs"),
        )?;
        let mut cmd = Command::new(node);
        cmd.env_clear()
            .env("HOME", &directory)
            .env("EEL_PLAYWRIGHT_MODULE", module);
        if spec
            .actions
            .iter()
            .chain(&spec.preparation)
            .any(|s| matches!(s.action, Action::ReadCanvas { .. }))
        {
            let png = std::env::var_os("EEL_PNGJS_MODULE")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?.join("node_modules/pngjs/lib/png.js"));
            cmd.env(
                "EEL_PNGJS_MODULE",
                fs::canonicalize(png).map_err(|_| {
                    invalid("canvasの画素の観測にはnpm ciでpngjsを用意してください")
                })?,
            );
        }
        // HOMEは試行用に分ける。Playwrightの導入済みブラウザはホストの固定したキャッシュから探す。
        if std::env::var_os("PLAYWRIGHT_BROWSERS_PATH").is_none() {
            let cache = if cfg!(target_os = "macos") {
                std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Caches"))
            } else {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
            };
            if let Some(cache) = cache {
                cmd.env("PLAYWRIGHT_BROWSERS_PATH", cache.join("ms-playwright"));
            }
        }
        for key in [
            "PATH",
            "PLAYWRIGHT_BROWSERS_PATH",
            "CHROME_DEVEL_SANDBOX",
            "EEL_BROWSER_EXECUTABLE",
            "EEL_BROWSER_EXTRA_ARGS",
        ] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        // 子プロセスの作業場所は試行ごとに変わる。相対パスはサーバーの起動場所で解決する。
        if let Some(path) = std::env::var_os("EEL_BROWSER_SECRETS_FILE") {
            cmd.env(
                "EEL_BROWSER_SECRETS_FILE",
                fs::canonicalize(path).map_err(|_| {
                    invalid("EEL_BROWSER_SECRETS_FILEで指定した試験用の秘密値ファイルを読めません")
                })?,
            );
        }
        cmd.arg(directory.join("runner.mjs"))
            .arg(directory.join("request.json"))
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(fs::File::create(directory.join("stdout.txt"))?)
            .stderr(fs::File::create(directory.join("stderr.txt"))?);
        let child = crate::process::ChildGuard::spawn(&mut cmd)?;
        let mut session = Self {
            child,
            directory,
            deadline: Instant::now() + Duration::from_millis(timeout_ms.saturating_add(2000)),
            report: unavailable(run_id, "ブラウザの準備を確認していない"),
        };
        if let Some(report) = session.wait("ready.json")? {
            session.report = report;
        }
        Ok(session)
    }
    fn wait(&mut self, file: &str) -> io::Result<Option<BrowserReport>> {
        loop {
            let path = self.directory.join(file);
            if path.is_file() {
                let len = fs::metadata(&path)?.len();
                if len > 1024 * 1024 {
                    return Err(invalid("ブラウザの記録が1MiBを超えました"));
                }
                let report: BrowserReport = crate::recording::read_json(&path)?;
                if report.schema_version != 1 || report.run_id != self.report.run_id {
                    return Err(invalid("ブラウザ記録の試行が一致しません"));
                }
                let actions = report
                    .steps
                    .iter()
                    .filter(|s| s.phase == "action")
                    .map(|s| s.id.as_str())
                    .collect();
                crate::screen::validate_exchanges(&report.exchanges, &actions)?;
                for response in &report.responses {
                    if let Some(headers) = &response.headers {
                        crate::screen::validate_headers(headers)?;
                    }
                    // 切り捨てたと記録するのは、記録したヘッダーだけである。
                    if response.headers_truncated.iter().any(|name| {
                        !response
                            .headers
                            .as_ref()
                            .is_some_and(|headers| headers.contains_key(name))
                    }) {
                        return Err(invalid(
                            "切り捨てたと記録された応答ヘッダーが、記録の中にありません",
                        ));
                    }
                }
                return Ok(Some(report));
            }
            if self.child.try_wait()?.is_some() {
                self.report.detail="ブラウザの実行環境が記録を返さず終了した。ブラウザの導入と版を確認してください".into();
                return Ok(None);
            }
            if Instant::now() >= self.deadline {
                self.report.detail = "ブラウザ試験が待機上限に達した".into();
                return Ok(None);
            }
            if ["stdout.txt", "stderr.txt"]
                .iter()
                .any(|p| fs::metadata(self.directory.join(p)).is_ok_and(|m| m.len() > 64 * 1024))
            {
                return Err(invalid("ブラウザ試験の出力上限に達しました"));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    pub fn prepared(&self) -> bool {
        self.report.prepared
    }
    pub fn run(&mut self) -> io::Result<BrowserReport> {
        if self.report.prepared {
            write_json_new(
                &self.directory.join("continue.json"),
                &serde_json::json!({"run_id":self.report.run_id,"proceed":true}),
            )?;
            if let Some(report) = self.wait("result.json")? {
                self.report = report;
            }
            let steps = self
                .report
                .steps
                .iter()
                .map(|s| (s.phase.clone(), s.id.clone()))
                .collect();
            (self.report.screens, self.report.screens_note) =
                crate::screen::adopt(&self.directory, &self.report.run_id, &steps);
        }
        Ok(self.report.clone())
    }
    pub fn report(&self) -> BrowserReport {
        self.report.clone()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::write(self.directory.join("stop"), b"stop");
        for attempt in 0..80 {
            // PlaywrightはChromiumを別グループで起動する。起動完了前も
            // SIGTERMのハンドラーで回収させ、2回目は終了を強制する。
            if matches!(attempt, 0 | 20) {
                let _ = crate::process::terminate(&mut self.child);
            }
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = crate::process::kill_group(&mut self.child);
        let _ = self.child.wait();
    }
}

/// 一つの観測元だけで条件を確かめる。欠落や相反する応答を成功で補わない。
pub fn evaluate(
    check: &crate::trial::TrialCheck,
    phase: Option<&crate::container::ContainerPhase>,
    text: &str,
) -> (crate::trial::Observed, String) {
    use crate::trial::{CheckMethod, Expectation, Observed};
    let missing = || {
        (
            Observed::NotObserved,
            "指定した観測を取得できていない".to_owned(),
        )
    };
    let Some(phase) = phase else {
        return missing();
    };
    if check.method == CheckMethod::DatabaseRelation {
        let Some(relation) = &check.relation else {
            return missing();
        };
        let column = |id: &str, index: usize, before: bool| -> Option<Option<String>> {
            let reads = if before {
                &phase.database_reads_before
            } else {
                &phase.database_reads
            };
            let read = reads.iter().find(|q| q.id == id)?;
            if !read.performed || read.exit_code != Some(0) {
                return None;
            }
            let mut rows = read.text.lines();
            let Some(row) = rows.next() else {
                return Some(None);
            };
            if rows.next().is_some() {
                return None;
            }
            let value = row.split('\t').nth(index)?;
            if value.is_empty() || value == "NULL" {
                return None;
            }
            Some(Some(value.to_owned()))
        };
        return match (
            column(&relation.left, relation.left_column, relation.left_before),
            column(&relation.right, relation.right_column, false),
        ) {
            (None, _) | (_, None) => (
                Observed::NotObserved,
                "DBの読取失敗、複数行、欠落した列またはNULLにより対応を確認できない".into(),
            ),
            (Some(None), _) | (_, Some(None)) => (
                Observed::Fail,
                "照合に必要なDBの行が見つからなかった".into(),
            ),
            (Some(Some(left)), Some(Some(right))) => {
                let equal = left == right;
                let observed = if equal == (check.expect == Expectation::Equal) {
                    Observed::Pass
                } else {
                    Observed::Fail
                };
                (
                    observed,
                    if equal {
                        "指定したDBの行の値は一致した"
                    } else {
                        "指定したDBの行の値は一致しなかった"
                    }
                    .into(),
                )
            }
        };
    }
    let Some(target) = check.target.as_deref() else {
        return missing();
    };
    let result = |has: bool| {
        (
            if has == (check.expect == Expectation::Contains) {
                Observed::Pass
            } else {
                Observed::Fail
            },
            format!(
                "観測 {target} に条件の文字列が{}",
                if has {
                    "含まれる"
                } else {
                    "含まれない"
                }
            ),
        )
    };
    match check.method {
        CheckMethod::BrowserCanvas => phase
            .browser
            .as_ref()
            .and_then(|b| {
                b.steps
                    .iter()
                    .find(|s| s.id == target && s.phase == "action" && s.completed)
            })
            .and_then(|s| s.canvas.as_ref())
            .map(|canvas| result(format!("variation={}", canvas.variation).contains(text)))
            .unwrap_or_else(missing),
        CheckMethod::ServerHttp => phase
            .http_reads
            .iter()
            .find(|r| r.id == target && r.performed)
            .map(|r| {
                if text.starts_with("sha256=") && r.sha256.is_none() {
                    missing()
                } else {
                    result(
                        format!(
                            "status={} sha256={}",
                            r.status.map(|s| s.to_string()).unwrap_or_default(),
                            r.sha256.as_deref().unwrap_or("")
                        )
                        .contains(text),
                    )
                }
            })
            .unwrap_or_else(missing),
        CheckMethod::BrowserText => {
            let Some(step) = phase.browser.as_ref().and_then(|b| {
                b.steps
                    .iter()
                    .find(|s| s.id == target && s.phase == "action" && s.completed)
            }) else {
                return missing();
            };
            step.text
                .as_ref()
                .map(|t| result(t.contains(text)))
                .unwrap_or_else(missing)
        }
        CheckMethod::BrowserAttribute => {
            let Some(read) = phase.browser.as_ref().and_then(|b| {
                b.steps
                    .iter()
                    .find(|s| s.id == target && s.phase == "action" && s.completed)
                    .and_then(|s| s.attribute.as_ref())
            }) else {
                return missing();
            };
            let positive = matches!(check.expect, Expectation::Contains | Expectation::Equal);
            let matched = read
                .value
                .as_deref()
                .is_some_and(|value| match check.expect {
                    Expectation::Equal | Expectation::NotEqual => value == text,
                    _ => value.contains(text),
                });
            (
                if matched == positive {
                    Observed::Pass
                } else {
                    Observed::Fail
                },
                match (&read.value, matched) {
                    (None, _) => format!("手順 {target} の要素に、指定した属性がない"),
                    (Some(_), true) => {
                        format!("手順 {target} で読んだ属性の値は条件の文字列に合う")
                    }
                    (Some(_), false) => {
                        format!("手順 {target} で読んだ属性の値は条件の文字列に合わない")
                    }
                },
            )
        }
        CheckMethod::BrowserHeader => {
            let (Some(browser), Some((name, part))) = (&phase.browser, header_condition(text))
            else {
                return missing();
            };
            if browser.response_limit {
                return (
                    Observed::NotObserved,
                    "HTTP応答の記録上限に達し、全ての応答を照合できない".into(),
                );
            }
            let responses = browser
                .responses
                .iter()
                .filter(|r| {
                    r.watch == target
                        && r.request_step.as_ref().is_some_and(|id| {
                            browser
                                .steps
                                .iter()
                                .any(|s| &s.id == id && s.phase == "action")
                        })
                })
                .collect::<Vec<_>>();
            // ヘッダーを記録していない応答（変更前の記録）は、ヘッダーがないとは扱わない。
            if responses.is_empty() || responses.iter().any(|r| r.headers.is_none()) {
                return missing();
            }
            // 記録した部分に条件の文字があれば「含む」。なくても、後半を切り捨てたヘッダーでは、
            // 切り捨てた部分にないとは言えないので、判定しない。
            let outcomes = responses
                .iter()
                .map(|r| {
                    let found = r
                        .headers
                        .as_ref()
                        .and_then(|h| h.get(name))
                        .is_some_and(|value| value.contains(part));
                    (found || !r.headers_truncated.iter().any(|n| n == name)).then_some(found)
                })
                .collect::<Vec<_>>();
            let Some(values) = outcomes.into_iter().collect::<Option<Vec<bool>>>() else {
                return (
                    Observed::NotObserved,
                    format!(
                        "観測 {target} の応答のヘッダー {name} は、上限で後半を切り捨てて記録した。切り捨てた部分に条件の文字があるかを判定できない"
                    ),
                );
            };
            if values.iter().any(|v| *v != values[0]) {
                return (
                    Observed::NotObserved,
                    "同じ観測先の応答で、ヘッダーの条件が相反する".into(),
                );
            }
            (
                if values[0] == (check.expect == Expectation::Contains) {
                    Observed::Pass
                } else {
                    Observed::Fail
                },
                format!(
                    "観測 {target} の応答に、条件に合うヘッダー {name} が{}",
                    if values[0] { "ある" } else { "ない" }
                ),
            )
        }
        CheckMethod::BrowserResponse => {
            let Some(browser) = &phase.browser else {
                return missing();
            };
            if browser.response_limit {
                return (
                    Observed::NotObserved,
                    "HTTP応答の記録上限に達し、全ての応答を照合できない".into(),
                );
            }
            let responses = browser
                .responses
                .iter()
                .filter(|r| {
                    r.watch == target
                        && r.request_step.as_ref().is_some_and(|id| {
                            browser
                                .steps
                                .iter()
                                .any(|s| &s.id == id && s.phase == "action")
                        })
                })
                .collect::<Vec<_>>();
            if responses.is_empty() {
                return missing();
            }
            let values = responses
                .iter()
                .map(|r| {
                    format!(
                        "status={} body_match={} sha256={}",
                        r.status,
                        r.body_match
                            .map(|b| b.to_string())
                            .unwrap_or("unobserved".into()),
                        r.payload_sha256.as_deref().unwrap_or("unobserved")
                    )
                    .contains(text)
                })
                .collect::<Vec<_>>();
            if values.iter().any(|v| *v != values[0])
                || (text.starts_with("body_match=")
                    && responses.iter().any(|r| r.body_match.is_none()))
                || (text.starts_with("sha256=")
                    && responses.iter().any(|r| r.payload_sha256.is_none()))
            {
                (
                    Observed::NotObserved,
                    "HTTP応答の条件が相反するか、本文を観測できていない".into(),
                )
            } else {
                result(values[0])
            }
        }
        CheckMethod::DatabaseRows => phase
            .database_reads
            .iter()
            .find(|q| q.id == target && q.performed && q.exit_code == Some(0))
            .map(|q| {
                if matches!(check.expect, Expectation::Zero | Expectation::Nonzero) {
                    let rows = q.text.lines().filter(|l| !l.is_empty()).count();
                    let pass = (rows == 0) == (check.expect == Expectation::Zero);
                    (
                        if pass { Observed::Pass } else { Observed::Fail },
                        format!("観測 {target} で読み出した行は{rows}行"),
                    )
                } else {
                    result(q.text.contains(text))
                }
            })
            .unwrap_or_else(missing),
        _ => missing(),
    }
}
