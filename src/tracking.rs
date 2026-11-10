//! 入力した試験値の追跡。試験値は試行ごとに一意なので、対象のスキーマを知らなくても、
//! その値がDBのどの表・列に現れたかを基盤が探せる。
//! 見つからなかったことは、保存されていないことを意味しない。暗号化・ハッシュ化された値、
//! 文字列型でない列、上限で打ち切った範囲は見つけられない。
use crate::database::{Client, DatabaseEngine};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    time::{Duration, Instant},
};

pub const MAX_TABLES: usize = 2000;
pub const MAX_COLUMNS: usize = 20000;
const MAX_STATEMENT: usize = 24 * 1024;
const MAX_BATCH: usize = 64 * 1024;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_SKIPPED: usize = 100;
pub const SCAN_BUDGET: Duration = Duration::from_secs(90);

/// 追跡する値。`slot` は値の出所（試験値そのもの、または入力した手順）を表す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackedValue {
    pub slot: String,
    pub value: String,
    /// 値を入力した手順。レシピの文中の試験値そのものには手順がない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// レシピに書くときの形（`{{test_value}}` を含む）。別の試行でも同じレシピを使えるようにする。
    pub template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanHit {
    pub slot: String,
    pub schema: String,
    pub table: String,
    pub column: String,
    /// その値を含む行の数。
    pub rows: u64,
    /// 列の値がその値と等しい行の数。
    pub exact_rows: u64,
}

/// DBの文字列型の列をすべて調べた結果。判定には使わない記録。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseScan {
    /// 照会を実行できたか。偽のときの `hits` の空は、値がなかったことを意味しない。
    pub performed: bool,
    /// 調べたDBのコンテナ、種類、接続したデータベース。
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub engine: DatabaseEngine,
    #[serde(default)]
    pub database: String,
    pub tables_scanned: u32,
    pub columns_scanned: u32,
    /// 調べなかった表や列と、その理由。
    pub skipped: Vec<String>,
    /// 上限か時間で探索を打ち切った。
    pub truncated: bool,
    /// 探索にかかった時間。
    #[serde(default)]
    pub elapsed_ms: u64,
    pub hits: Vec<ScanHit>,
    pub detail: String,
}

impl DatabaseScan {
    pub fn not_performed(detail: impl Into<String>) -> Self {
        Self {
            performed: false,
            service: String::new(),
            engine: DatabaseEngine::default(),
            database: String::new(),
            tables_scanned: 0,
            columns_scanned: 0,
            skipped: vec![],
            truncated: false,
            elapsed_ms: 0,
            hits: vec![],
            detail: detail.into(),
        }
    }
}

/// クライアントを1回起動した結果。
pub struct Executed {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    schema: String,
    table: String,
    columns: Vec<String>,
}

fn skip(skipped: &mut Vec<String>, entry: String) {
    if skipped.len() < MAX_SKIPPED {
        skipped.push(entry);
    }
}

/// 列の一覧を表ごとにまとめ、1つのSQLが長くなりすぎないよう列を分ける。
fn pieces(
    listing: &str,
    values: &[TrackedValue],
    skipped: &mut Vec<String>,
    truncated: &mut bool,
) -> (Vec<Piece>, u32, u32) {
    let per_column: usize = values.iter().map(|v| 280 + v.value.len() * 4).sum();
    let chunk = (MAX_STATEMENT / per_column.max(1)).max(1);
    let mut tables: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut columns = 0usize;
    for line in listing.lines().filter(|l| !l.trim().is_empty()) {
        let parts: Vec<&str> = line.split('\t').collect();
        let [schema, table, column] = parts[..] else {
            skip(skipped, "列の一覧に読めない行がある".into());
            continue;
        };
        if ![schema, table, column]
            .iter()
            .all(|s| crate::browser::sql_identifier(s))
        {
            skip(
                skipped,
                format!("{schema}.{table}.{column}: 識別子に英数字と下線以外の文字がある"),
            );
            continue;
        }
        let key = (schema.to_owned(), table.to_owned());
        if columns >= MAX_COLUMNS || (!tables.contains_key(&key) && tables.len() >= MAX_TABLES) {
            *truncated = true;
            continue;
        }
        columns += 1;
        tables.entry(key).or_default().push(column.to_owned());
    }
    let table_count = tables.len() as u32;
    let pieces = tables
        .into_iter()
        .flat_map(|((schema, table), columns)| {
            columns
                .chunks(chunk)
                .map(|c| Piece {
                    schema: schema.clone(),
                    table: table.clone(),
                    columns: c.to_vec(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    (pieces, table_count, columns as u32)
}

/// 文字列型の列をすべて調べ、追跡する値を含む行の数を表・列ごとに数える。
/// `execute` はコンテナの中でクライアントを起動する。照会できない種類では実行しない。
pub fn scan(
    engine: DatabaseEngine,
    env: &BTreeMap<String, String>,
    values: &[TrackedValue],
    budget: Duration,
    mut execute: impl FnMut(&Client) -> io::Result<Executed>,
) -> DatabaseScan {
    let database = engine.scan_database(env);
    let Some(list) = engine.run(env, &database, engine.text_columns_sql()) else {
        return DatabaseScan::not_performed(format!(
            "DBの種類（{}）は基盤の読み出しに対応していないため、探索していない",
            engine.label()
        ));
    };
    if values.is_empty() || values.iter().any(|v| v.value.is_empty()) {
        return DatabaseScan::not_performed("追跡する値がないため、探索していない");
    }
    let started = Instant::now();
    let listing = match execute(&list) {
        Ok(out) if out.success && out.stdout.len() <= MAX_OUTPUT => out.stdout,
        Ok(out) => {
            return DatabaseScan::not_performed(format!(
                "列の一覧を読めなかった: {}",
                out.stderr.chars().take(300).collect::<String>()
            ));
        }
        Err(error) => {
            return DatabaseScan::not_performed(format!("DBのクライアントを起動できない: {error}"));
        }
    };
    let mut skipped = vec![];
    let mut truncated = false;
    let (pieces, tables_scanned, columns_scanned) =
        pieces(&listing, values, &mut skipped, &mut truncated);
    let texts: Vec<String> = values.iter().map(|v| v.value.clone()).collect();
    let mut hits = vec![];
    let mut next = 0;
    while next < pieces.len() {
        if started.elapsed() > budget {
            truncated = true;
            break;
        }
        // まとめて実行する範囲を、引数の長さの上限に収める。
        let mut statements = vec![];
        let mut size = 0;
        let mut end = next;
        while end < pieces.len() {
            let piece = &pieces[end];
            let sql = engine.count_sql(end, &piece.schema, &piece.table, &piece.columns, &texts);
            if !statements.is_empty() && size + sql.len() > MAX_BATCH {
                break;
            }
            size += sql.len();
            statements.push(sql);
            end += 1;
        }
        let Some(client) = engine.run_many(env, &database, &statements) else {
            break;
        };
        let out = match execute(&client) {
            Ok(out) if out.stdout.len() <= MAX_OUTPUT => out,
            _ => {
                truncated = true;
                break;
            }
        };
        // 返ってきた行を、目印で表に対応づける。途中で失敗した場合は、その表だけを飛ばして続ける。
        let mut answered = next;
        for line in out.stdout.lines() {
            let mut fields = line.split('\t');
            let Some(tag) = fields.next().and_then(|t| t.trim().parse::<usize>().ok()) else {
                continue;
            };
            if tag != answered || tag >= end {
                break;
            }
            let piece = &pieces[tag];
            let counts: Vec<u64> = fields.filter_map(|f| f.trim().parse().ok()).collect();
            if counts.len() != piece.columns.len() * values.len() * 2 {
                skip(
                    &mut skipped,
                    format!("{}.{}: 数えた結果の形が合わない", piece.schema, piece.table),
                );
            } else {
                for (c, column) in piece.columns.iter().enumerate() {
                    for (v, value) in values.iter().enumerate() {
                        let at = (c * values.len() + v) * 2;
                        if counts[at] > 0 {
                            hits.push(ScanHit {
                                slot: value.slot.clone(),
                                schema: piece.schema.clone(),
                                table: piece.table.clone(),
                                column: column.clone(),
                                rows: counts[at],
                                exact_rows: counts[at + 1],
                            });
                        }
                    }
                }
            }
            answered = tag + 1;
        }
        if answered < end {
            let piece = &pieces[answered];
            skip(
                &mut skipped,
                format!(
                    "{}.{}: 照会に失敗した: {}",
                    piece.schema,
                    piece.table,
                    // クライアントは失敗した文を区切り線で囲んで出す。誤りの行を優先して残す。
                    out.stderr
                        .lines()
                        .find(|l| l.contains("ERROR"))
                        .or_else(|| out
                            .stderr
                            .lines()
                            .find(|l| !l.trim_matches('-').trim().is_empty()))
                        .unwrap_or("")
                        .chars()
                        .take(200)
                        .collect::<String>()
                ),
            );
            answered += 1;
        }
        next = answered;
    }
    DatabaseScan {
        performed: true,
        service: String::new(),
        engine,
        database,
        tables_scanned,
        columns_scanned,
        truncated,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        detail: format!(
            "文字列型の列{columns_scanned}件（表{tables_scanned}件）を調べ、値を含む列を{}件見つけた{}。見つからない列に値がないとは限らない（暗号化・ハッシュ化・文字列型でない列は見つけられない）",
            hits.len(),
            if truncated {
                "。上限か時間で探索を打ち切った"
            } else {
                ""
            }
        ),
        skipped,
        hits,
    }
}

/// 操作の前からあった行を除いた、操作の後に増えた出現。
pub fn new_hits(before: Option<&DatabaseScan>, after: &DatabaseScan) -> Vec<ScanHit> {
    let earlier: BTreeMap<_, u64> = before
        .filter(|b| b.performed)
        .map(|b| {
            b.hits
                .iter()
                .map(|h| ((&h.slot, &h.schema, &h.table, &h.column), h.rows))
                .collect()
        })
        .unwrap_or_default();
    after
        .hits
        .iter()
        .filter_map(|h| {
            let had = earlier
                .get(&(&h.slot, &h.schema, &h.table, &h.column))
                .copied()
                .unwrap_or(0);
            (h.rows > had).then(|| ScanHit {
                rows: h.rows - had,
                ..h.clone()
            })
        })
        .collect()
}

/// 手順ごとの試験値。`value_per_step` のとき、試験値を含む入力の手順に連番を付ける。
pub fn step_values(
    spec: &crate::browser::BrowserSpec,
    test_value: &str,
) -> BTreeMap<String, String> {
    if !spec.value_per_step {
        return BTreeMap::new();
    }
    fills(spec)
        .enumerate()
        .map(|(n, (id, _))| (id.to_owned(), format!("{test_value}-{:02}", n + 1)))
        .collect()
}

/// 試験値を含む文字を入力する手順と、その文。
fn fills(spec: &crate::browser::BrowserSpec) -> impl Iterator<Item = (&str, &str)> {
    spec.preparation
        .iter()
        .chain(&spec.actions)
        .filter_map(|step| match &step.action {
            crate::browser::Action::Fill {
                value: crate::browser::Input::Literal { value },
                ..
            } if value.contains(crate::trial::TEST_VALUE) => {
                Some((step.id.as_str(), value.as_str()))
            }
            _ => None,
        })
}

/// 追跡する値。試験値そのものと、試験値を含む文字を入力した手順ごとの、入力した文字の全体。
/// 追跡を有効にしていない試験では空。
pub fn values_of(spec: &crate::browser::BrowserSpec, test_value: &str) -> Vec<TrackedValue> {
    if !spec.track_values {
        return vec![];
    }
    let per_step = step_values(spec, test_value);
    let mut values = vec![TrackedValue {
        slot: "test_value".into(),
        value: test_value.to_owned(),
        step: None,
        template: crate::trial::TEST_VALUE.into(),
    }];
    for (n, (id, literal)) in fills(spec).enumerate() {
        let (value, template) = match per_step.get(id) {
            Some(own) => (
                literal.replace(crate::trial::TEST_VALUE, own),
                literal.replace(
                    crate::trial::TEST_VALUE,
                    &format!("{}-{:02}", crate::trial::TEST_VALUE, n + 1),
                ),
            ),
            None => (
                literal.replace(crate::trial::TEST_VALUE, test_value),
                literal.to_owned(),
            ),
        };
        values.push(TrackedValue {
            slot: id.to_owned(),
            value,
            step: Some(id.to_owned()),
            template,
        });
    }
    values
}

/// 出所の記録は、レシピに実在する項目だけを指す。
pub fn validate_provenance(
    provenance: &Provenance,
    spec: &crate::browser::BrowserSpec,
) -> io::Result<()> {
    use crate::error::invalid;
    let steps: Vec<&str> = spec
        .preparation
        .iter()
        .chain(&spec.actions)
        .map(|s| s.id.as_str())
        .collect();
    let responses: Vec<&str> = spec
        .responses
        .iter()
        .map(|w| w.id.as_str())
        .chain(spec.http_reads.iter().map(|p| p.id.as_str()))
        .collect();
    let database: Vec<&str> = spec.database.iter().map(|p| p.id.as_str()).collect();
    for (map, known, what) in [
        (&provenance.steps, Some(&steps), "手順"),
        (&provenance.responses, Some(&responses), "応答の観測先"),
        (&provenance.database, Some(&database), "DBの読み出し先"),
        // 判定条件はレシピの別の場所にあるため、ここでは形だけを確かめる。
        (&provenance.checks, None, "判定条件"),
    ] {
        if map.len() > 128 {
            return Err(invalid("出所の記録が多すぎます"));
        }
        for (id, basis) in map {
            if known.is_some_and(|k| !k.contains(&id.as_str())) {
                return Err(invalid(format!(
                    "出所の記録が、レシピにない{what} {id} を指しています"
                )));
            }
            if basis.run_id.is_empty()
                || basis.run_id.len() > 128
                || basis.rules == 0
                || basis.source.is_empty()
                || basis.source.len() > 64
            {
                return Err(invalid(format!("{what} {id} の出所の記録が不正です")));
            }
        }
    }
    Ok(())
}

/// 値を含んでいたHTTPのやり取り。本文は持たない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeRef {
    pub step: String,
    pub method: String,
    pub path: String,
    pub status: u16,
}

/// 追跡した値1つが、どこに現れたか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueAppearance {
    pub slot: String,
    /// 値を入力した手順。
    pub entered_at: Option<String>,
    /// 操作の後に増えた、DBでの出現。
    pub database: Vec<ScanHit>,
    /// 値を要求に含んでいたやり取りと、応答に含んでいたやり取り。
    pub requests: Vec<ExchangeRef>,
    pub responses: Vec<ExchangeRef>,
    /// 入力より後に、表示されている文字列に値が現れた手順。
    pub screens: Vec<String>,
    /// ログ（標準出力・標準エラー）に値が現れたコンテナ。
    pub logs: Vec<String>,
}

/// 試行で追跡した値と、その出現のまとめ。記録のみで、判定には使わない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueTrace {
    pub values: Vec<TrackedValue>,
    pub appearances: Vec<ValueAppearance>,
    /// この記録で分からないこと。出現がないことを、値がないことと読まないための注記。
    pub limits: Vec<String>,
}

/// DB・HTTP・画面・ログの記録から、値ごとの出現をまとめる。
pub fn trace(
    values: &[TrackedValue],
    before: Option<&DatabaseScan>,
    after: Option<&DatabaseScan>,
    browser: Option<&crate::browser::BrowserReport>,
    services: &[crate::container::ServicePhase],
) -> ValueTrace {
    let mut limits = vec![
        "画面は、表示されている文字列だけを調べた。入力欄の中の値と画像の中の文字は調べていない"
            .to_owned(),
        "HTTPは、操作の段階の要求と応答の本文とクエリ文字列を調べた。大きすぎる本文は調べていない"
            .to_owned(),
    ];
    let added = match after {
        Some(scan) if scan.performed => {
            if scan.truncated {
                limits.push("DBの探索を上限か時間で打ち切った。調べていない表がある".into());
            }
            if !scan.skipped.is_empty() {
                limits.push(format!(
                    "DBの探索で調べなかった表や列が{}件ある",
                    scan.skipped.len()
                ));
            }
            if !before.is_some_and(|b| b.performed) {
                limits.push("操作の前の探索がないため、前からあった行を除けていない".into());
            }
            new_hits(before, scan)
        }
        Some(scan) => {
            limits.push(format!("DBを探索できなかった: {}", scan.detail));
            vec![]
        }
        None => {
            limits.push("DBを探索していない".into());
            vec![]
        }
    };
    let unread = browser
        .map(|b| b.exchanges.iter().filter(|e| e.response_unread).count())
        .unwrap_or(0);
    if unread > 0 {
        limits.push(format!(
            "本文を読めなかった応答が{unread}件ある。その応答に値がないとは限らない"
        ));
    }
    if browser.is_some_and(|b| b.exchange_limit) {
        limits.push("HTTPのやり取りの記録が上限に達した。記録していないやり取りがある".into());
    }
    if services.iter().any(|s| !s.log_complete) {
        limits.push("最後まで読めなかったコンテナのログがある".into());
    }
    let steps: Vec<&crate::browser::StepRecord> = browser
        .map(|b| b.steps.iter().collect())
        .unwrap_or_default();
    let appearances = values
        .iter()
        .map(|value| {
            let exchanges = |pick: fn(&crate::screen::ExchangeRecord) -> &Vec<String>| {
                browser
                    .map(|b| {
                        b.exchanges
                            .iter()
                            .filter(|e| pick(e).contains(&value.slot))
                            .map(|e| ExchangeRef {
                                step: e.step.clone(),
                                method: e.method.clone(),
                                path: e.path.clone(),
                                status: e.status,
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            // 入力した手順より後の手順だけを、表示の出現として数える。
            let entered = value
                .step
                .as_ref()
                .and_then(|id| steps.iter().position(|s| &s.id == id));
            ValueAppearance {
                slot: value.slot.clone(),
                entered_at: value.step.clone(),
                database: added
                    .iter()
                    .filter(|h| h.slot == value.slot)
                    .cloned()
                    .collect(),
                requests: exchanges(|e| &e.request_slots),
                responses: exchanges(|e| &e.response_slots),
                screens: steps
                    .iter()
                    .enumerate()
                    .filter(|(i, s)| {
                        entered.is_none_or(|at| *i > at) && s.visible_slots.contains(&value.slot)
                    })
                    .map(|(_, s)| s.id.clone())
                    .collect(),
                logs: services
                    .iter()
                    .filter(|s| s.log_text.contains(&value.value))
                    .map(|s| s.name.clone())
                    .collect(),
            }
        })
        .collect();
    ValueTrace {
        values: values.to_vec(),
        appearances,
        limits,
    }
}

/// 候補を作る規則の版。規則を変えたら上げる。以前の版の候補を新しい版の結果として扱わない。
pub const RULES: u32 = 1;
const MAX_GENERATED: usize = 16;

/// 観測から作った項目の出所。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Basis {
    pub run_id: String,
    pub rules: u32,
    pub source: String,
}

/// レシピの項目のうち、観測から作ったものの出所。項目のIDで引く。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, Basis>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub responses: BTreeMap<String, Basis>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub database: BTreeMap<String, Basis>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checks: BTreeMap<String, Basis>,
}

/// 値の出現から作った、読み出し先と判定条件の候補。人が採用するまで判定には使わない。
#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub rules: u32,
    pub run_id: String,
    pub database: Vec<crate::browser::DatabaseProbe>,
    pub responses: Vec<crate::browser::ResponseWatch>,
    pub checks: Vec<crate::trial::TrialCheck>,
    pub provenance: Provenance,
    /// 候補にしなかった出現と、その理由。
    pub skipped: Vec<String>,
    pub note: String,
}

fn identifier(parts: &[&str], used: &mut std::collections::BTreeSet<String>) -> String {
    let mut id: String = parts
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(56)
        .collect();
    let base = id.clone();
    let mut n = 2;
    while !used.insert(id.clone()) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// 規則の版1。DBで値と等しい列、値を送った要求、コンテナのログから候補を作る。
/// 同じ値が複数の表や列に現れた場合は、すべてを候補にし、1つに絞らない。
pub fn draft(
    run_id: &str,
    trace: &ValueTrace,
    scan: Option<&DatabaseScan>,
    services: &[String],
) -> Draft {
    use crate::trial::{CheckMethod, Expectation, TrialCheck};
    let basis = || Basis {
        run_id: run_id.to_owned(),
        rules: RULES,
        source: "value_trace".into(),
    };
    let mut used = std::collections::BTreeSet::new();
    let mut out = Draft {
        rules: RULES,
        run_id: run_id.to_owned(),
        database: vec![],
        responses: vec![],
        checks: vec![],
        provenance: Provenance::default(),
        skipped: vec![],
        note: "入力した値の出現から作った候補です。採用するまで判定には使いません。値が現れなかった場所に値がないとは限りません".into(),
    };
    let check = |id: &str, scope: String, method, expect, text: &str, target: &str| TrialCheck {
        id: id.to_owned(),
        scope,
        method,
        expect,
        text: Some(text.to_owned()),
        target: Some(target.to_owned()),
        relation: None,
    };
    let mut watched: BTreeMap<(String, String), String> = BTreeMap::new();
    // どれかの値と等しい行があった場所。別の値が「含むだけ」で現れても、候補にしない理由を重ねて出さない。
    let exact: std::collections::BTreeSet<(String, String, String)> = trace
        .appearances
        .iter()
        .flat_map(|a| &a.database)
        .filter(|h| h.exact_rows > 0)
        .map(|h| (h.schema.clone(), h.table.clone(), h.column.clone()))
        .collect();
    // 同じ文字を入力した値は、出現から区別できない。最初の1つだけから候補を作る。
    let mut first_with: BTreeMap<&str, &str> = BTreeMap::new();
    for (value, appearance) in trace.values.iter().zip(&trace.appearances) {
        if let Some(earlier) = first_with.get(value.value.as_str()) {
            out.skipped.push(format!(
                "{}: {earlier} と同じ文字を入力したため、出現を区別できない。手順ごとの試験値（value_per_step）を使うと区別できる",
                value.slot
            ));
            continue;
        }
        first_with.insert(&value.value, &value.slot);
        for hit in &appearance.database {
            let place = format!("{}.{}.{}", hit.schema, hit.table, hit.column);
            let Some(scan) = scan else { continue };
            // 既存の読み出しは、列の値が等しい行を読む。値を含むだけの列は表せない。
            if hit.exact_rows == 0 {
                if exact.contains(&(hit.schema.clone(), hit.table.clone(), hit.column.clone())) {
                    continue;
                }
                out.skipped.push(format!(
                    "{place}: 値を含む行が{}件あるが、列の値と等しい行がない。等値で読む設定では表せない",
                    hit.rows
                ));
                continue;
            }
            let database = match scan.engine {
                DatabaseEngine::Postgres if hit.schema != "public" => {
                    out.skipped.push(format!(
                        "{place}: public以外のスキーマの表は、読み出しの設定で指定できない"
                    ));
                    continue;
                }
                DatabaseEngine::Postgres => scan.database.clone(),
                _ => hit.schema.clone(),
            };
            if out.database.len() >= MAX_GENERATED {
                out.skipped.push(format!(
                    "{place}: 読み出し先の候補が上限の{MAX_GENERATED}件に達した"
                ));
                continue;
            }
            let id = identifier(&["db", &value.slot, &hit.table, &hit.column], &mut used);
            out.database.push(crate::browser::DatabaseProbe {
                id: id.clone(),
                service: scan.service.clone(),
                database,
                table: hit.table.clone(),
                match_column: hit.column.clone(),
                columns: vec![hit.column.clone()],
                match_value: Some(value.template.clone()),
                before_actions: false,
            });
            out.checks.push(check(
                &id,
                format!("{}.{} に入力した値が保存される", hit.table, hit.column),
                CheckMethod::DatabaseRows,
                Expectation::Contains,
                &value.template,
                &id,
            ));
            out.provenance.database.insert(id.clone(), basis());
            out.provenance.checks.insert(id, basis());
        }
        for exchange in &appearance.requests {
            let label = format!("{} {}", exchange.method, exchange.path);
            if !(200..300).contains(&exchange.status) {
                out.skipped.push(format!(
                    "{label}: 値を送った要求への応答がHTTP {}。成功の条件にしていない",
                    exchange.status
                ));
                continue;
            }
            if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&exchange.method.as_str()) {
                out.skipped
                    .push(format!("{label}: 応答の観測先に指定できないメソッド"));
                continue;
            }
            let key = (exchange.method.clone(), exchange.path.clone());
            if watched.contains_key(&key) {
                continue;
            }
            if out.responses.len() >= MAX_GENERATED {
                out.skipped.push(format!(
                    "{label}: 応答の候補が上限の{MAX_GENERATED}件に達した"
                ));
                continue;
            }
            let id = identifier(&["http", &exchange.step], &mut used);
            watched.insert(key, id.clone());
            out.responses.push(crate::browser::ResponseWatch {
                id: id.clone(),
                path: exchange.path.clone(),
                method: exchange.method.clone(),
                contains: None,
                payload: None,
            });
            out.checks.push(check(
                &id,
                format!("入力した値を送った要求（{label}）に応答が返る"),
                CheckMethod::BrowserResponse,
                Expectation::Contains,
                &format!("status={}", exchange.status),
                &id,
            ));
            out.provenance.responses.insert(id.clone(), basis());
            out.provenance.checks.insert(id, basis());
        }
        if !appearance.screens.is_empty() {
            let shown: Vec<&str> = appearance
                .screens
                .iter()
                .take(4)
                .map(String::as_str)
                .collect();
            out.skipped.push(format!(
                "{}: 入力より後の{}件の手順（{}{}）で画面に表示された。画面の条件は読む要素を指定する手順が要るため、候補にしていない",
                value.slot,
                appearance.screens.len(),
                shown.join("、"),
                if appearance.screens.len() > shown.len() { " ほか" } else { "" }
            ));
        }
    }
    // ログの条件は、値ごとではなくコンテナごとに1件。試験値を含むすべての入力を対象にする。
    for service in services {
        let id = identifier(&["log", service], &mut used);
        out.checks.push(check(
            &id,
            format!("{service} のコンテナのログ（標準出力・標準エラー）に入力した値が出ない"),
            CheckMethod::ContainerLog,
            Expectation::NotContains,
            crate::trial::TEST_VALUE,
            service,
        ));
        out.provenance.checks.insert(id, basis());
    }
    if out.checks.len() > 32 {
        let dropped = out.checks.split_off(32);
        for check in dropped {
            out.provenance.checks.remove(&check.id);
            out.skipped
                .push(format!("{}: 判定条件の候補が上限の32件に達した", check.id));
        }
    }
    out
}

/// 事例の試行を指定して、その試行で追跡した値の出現から候補を作る。
/// 値を追跡していない試行は、候補なしではなく誤りとして返す。
pub fn draft_for(case: &crate::workspace::Workspace, evidence_id: &str) -> io::Result<Draft> {
    use crate::error::invalid;
    let trace = case
        .evidence
        .iter()
        .find(|e| e.id == evidence_id)
        .and_then(|e| e.execution.as_ref())
        .ok_or_else(|| invalid("指定した試行が、この事例にありません"))?;
    let phase = trace
        .containers
        .as_ref()
        .ok_or_else(|| invalid("この試行にはコンテナの記録がありません"))?;
    let values = phase.value_trace.as_ref().ok_or_else(|| {
        invalid("この試行は値を追跡していません。レシピのブラウザの設定で track_values を有効にして実行してください")
    })?;
    let services: Vec<String> = phase.services.iter().map(|s| s.name.clone()).collect();
    Ok(draft(
        &trace.correlation.run_id,
        values,
        phase.database_scan.as_ref(),
        &services,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Vec<TrackedValue> {
        vec![
            TrackedValue {
                slot: "test_value".into(),
                value: "EEL-TEST-1".into(),
                step: None,
                template: "{{test_value}}".into(),
            },
            TrackedValue {
                slot: "step-note".into(),
                value: "EEL-TEST-1-02".into(),
                step: Some("note".into()),
                template: "{{test_value}}-02".into(),
            },
        ]
    }

    /// 表ごとの答えを決めておき、目印から表を引いて返す架空のクライアント。
    fn fake(
        listing: &'static str,
        answers: &'static [(&'static str, &'static str)],
        fail: &'static [&'static str],
    ) -> impl FnMut(&Client) -> io::Result<Executed> {
        move |client: &Client| {
            let sql = client.argv.last().unwrap();
            if sql.contains("information_schema.columns") {
                return Ok(Executed {
                    success: true,
                    stdout: listing.into(),
                    stderr: String::new(),
                });
            }
            let mut stdout = String::new();
            for statement in sql.split(';') {
                let tag = statement
                    .trim_start_matches("SELECT ")
                    .split(',')
                    .next()
                    .unwrap();
                if fail.iter().any(|t| statement.ends_with(&format!("`{t}`"))) {
                    return Ok(Executed {
                        success: false,
                        stdout,
                        stderr: "ERROR 1267: Illegal mix of collations".into(),
                    });
                }
                let counts = answers
                    .iter()
                    .find(|(t, _)| statement.ends_with(&format!("`{t}`")))
                    .map(|(_, c)| *c)
                    .unwrap();
                stdout.push_str(&format!("{tag}\t{counts}\n"));
            }
            Ok(Executed {
                success: true,
                stdout,
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn hits_are_reported_per_table_column_and_value() {
        let listing = "app\tvisit\treason\napp\tvisit\tnote\napp\taudit\tquery\n";
        let scan = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values(),
            SCAN_BUDGET,
            // 列ごと・値ごとに「含む数・等しい数」が並ぶ。audit: 列1つ×値2つ。visit: 列2つ×値2つ。
            fake(
                listing,
                &[("audit", "3\t0\t1\t0"), ("visit", "1\t1\t0\t0\t1\t0\t1\t1")],
                &[],
            ),
        );
        assert!(scan.performed && !scan.truncated, "{}", scan.detail);
        assert_eq!((scan.tables_scanned, scan.columns_scanned), (2, 3));
        let found: Vec<_> = scan
            .hits
            .iter()
            .map(|h| {
                (
                    h.table.as_str(),
                    h.column.as_str(),
                    h.slot.as_str(),
                    h.rows,
                    h.exact_rows,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("audit", "query", "test_value", 3, 0),
                ("audit", "query", "step-note", 1, 0),
                ("visit", "reason", "test_value", 1, 1),
                ("visit", "note", "test_value", 1, 0),
                ("visit", "note", "step-note", 1, 1),
            ]
        );
        assert!(scan.skipped.is_empty());
    }

    #[test]
    fn a_failing_table_is_skipped_with_its_reason_and_the_rest_is_still_scanned() {
        let listing = "app\ta\tx\napp\tbroken\tx\napp\tc\tx\napp\tbad name\tx\n";
        let scan = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            SCAN_BUDGET,
            fake(listing, &[("a", "0\t0"), ("c", "2\t1")], &["broken"]),
        );
        assert!(scan.performed);
        assert_eq!(scan.hits.len(), 1);
        assert_eq!((scan.hits[0].table.as_str(), scan.hits[0].rows), ("c", 2));
        assert_eq!(scan.skipped.len(), 2, "{:?}", scan.skipped);
        assert!(
            scan.skipped
                .iter()
                .any(|s| s.contains("bad name") && s.contains("識別子"))
        );
        assert!(
            scan.skipped
                .iter()
                .any(|s| s.contains("app.broken") && s.contains("collations"))
        );
    }

    #[test]
    fn nothing_found_is_distinguished_from_not_performed_and_truncated() {
        // 対応していない種類、値なし、一覧の読取失敗は、探索を実行していない。
        let none = scan(
            DatabaseEngine::Other,
            &BTreeMap::new(),
            &values(),
            SCAN_BUDGET,
            |_| unreachable!(),
        );
        assert!(!none.performed && none.detail.contains("対応していない"));
        let empty = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &[],
            SCAN_BUDGET,
            |_| unreachable!(),
        );
        assert!(!empty.performed);
        let failed = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values(),
            SCAN_BUDGET,
            |_| {
                Ok(Executed {
                    success: false,
                    stdout: String::new(),
                    stderr: "Can't connect".into(),
                })
            },
        );
        assert!(!failed.performed && failed.detail.contains("Can't connect"));
        // 実行して何も見つからない場合は、実行済みで0件。説明に限界を書く。
        let clean = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            SCAN_BUDGET,
            fake("app\ta\tx\n", &[("a", "0\t0")], &[]),
        );
        assert!(clean.performed && clean.hits.is_empty() && !clean.truncated);
        assert!(clean.detail.contains("値がないとは限らない"));
        // 時間の上限を過ぎた場合は、打ち切ったことを記録する。
        let late = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            Duration::ZERO,
            fake("app\ta\tx\n", &[("a", "5\t5")], &[]),
        );
        assert!(late.performed && late.truncated);
        assert!(late.detail.contains("打ち切った"));
    }

    #[test]
    fn rows_that_existed_before_the_operation_are_subtracted() {
        let hit = |table: &str, rows| ScanHit {
            slot: "test_value".into(),
            schema: "app".into(),
            table: table.into(),
            column: "x".into(),
            rows,
            exact_rows: rows,
        };
        let scan_of = |hits: Vec<ScanHit>| DatabaseScan {
            hits,
            ..DatabaseScan::not_performed("")
        };
        let mut before = scan_of(vec![hit("patient", 1), hit("audit", 2)]);
        before.performed = true;
        let after = scan_of(vec![hit("patient", 1), hit("audit", 5), hit("visit", 1)]);
        let added = new_hits(Some(&before), &after);
        let pairs: Vec<_> = added.iter().map(|h| (h.table.as_str(), h.rows)).collect();
        assert_eq!(pairs, [("audit", 3), ("visit", 1)]);
        // 操作の前の探索を実行できなかった場合は、差し引かずにすべてを出す。
        before.performed = false;
        assert_eq!(new_hits(Some(&before), &after).len(), 3);
        assert_eq!(new_hits(None, &after).len(), 3);
    }

    fn service(name: &str, log: &str, complete: bool) -> crate::container::ServicePhase {
        let mut phase: crate::container::ServicePhase = serde_json::from_value(serde_json::json!({
            "name": name, "role": "app", "image": "x", "digest": "sha256:x", "container": name,
            "port": 1, "started": true, "ready": true, "ready_after_ms": 1,
            "running_after_stop": false, "log_excerpt": "", "log_note": "",
            "log_complete": complete, "detail": ""}))
        .unwrap();
        // ログの全文は保存しない項目なので、試行の中の状態として直接入れる。
        phase.log_text = log.into();
        phase
    }

    fn report() -> crate::browser::BrowserReport {
        let exchange = |step: &str,
                        path: &str,
                        status: u16,
                        request: &[&str],
                        response: &[&str]| {
            serde_json::json!({"step": step, "method": "POST", "path": path, "status": status, "resource": "document",
                "headers": {}, "cookies": [], "request_slots": request, "response_slots": response})
        };
        let step = |id: &str, phase: &str, visible: &[&str]| {
            serde_json::json!({"id": id, "phase": phase, "completed": true, "text": null, "screenshot": null,
                "detail": "", "visible_slots": visible})
        };
        serde_json::from_value(serde_json::json!({"schema_version": 1, "run_id": "run-1", "version": "1",
            "prepared": true, "completed": true, "responses": [], "blocked_requests": 0, "detail": "",
            "steps": [step("open", "preparation", &["step-note"]), step("note", "action", &[]),
                step("save", "action", &[]), step("reopen", "action", &["step-note", "test_value"])],
            "exchanges": [exchange("save", "/visit/save", 200, &["step-note"], &[]),
                exchange("save", "/visit/save", 200, &["step-note"], &[]),
                exchange("save", "/audit", 500, &["step-note"], &[]),
                exchange("reopen", "/visit", 200, &[], &["step-note"])]}))
        .unwrap()
    }

    fn scanned(hits: Vec<ScanHit>) -> DatabaseScan {
        DatabaseScan {
            performed: true,
            service: "db".into(),
            hits,
            ..DatabaseScan::not_performed("")
        }
    }

    fn hit(slot: &str, table: &str, column: &str, rows: u64, exact_rows: u64) -> ScanHit {
        ScanHit {
            slot: slot.into(),
            schema: "app".into(),
            table: table.into(),
            column: column.into(),
            rows,
            exact_rows,
        }
    }

    #[test]
    fn appearances_join_database_http_screens_and_logs_per_value() {
        let before = scanned(vec![hit("test_value", "patient", "name", 1, 1)]);
        let after = scanned(vec![
            hit("test_value", "patient", "name", 1, 1),
            hit("step-note", "visit", "note", 1, 1),
            hit("step-note", "audit", "query", 2, 0),
        ]);
        let services = [
            service("web", "saved EEL-TEST-1-02 ok", true),
            service("db", "", true),
        ];
        let traced = trace(
            &values(),
            Some(&before),
            Some(&after),
            Some(&report()),
            &services,
        );
        let note = &traced.appearances[1];
        assert_eq!(note.entered_at.as_deref(), Some("note"));
        assert_eq!(
            note.database
                .iter()
                .map(|h| h.table.as_str())
                .collect::<Vec<_>>(),
            ["visit", "audit"]
        );
        assert_eq!(note.requests.len(), 3);
        assert_eq!(note.responses.len(), 1);
        // 入力した手順より前の表示は、入力の結果ではないので数えない。
        assert_eq!(note.screens, ["reopen"]);
        // ログは値そのもので調べる。試験値は入力した値の先頭にも含まれるので、両方に現れる。
        assert_eq!(note.logs, ["web"]);
        let base = &traced.appearances[0];
        assert!(base.database.is_empty(), "操作の前からあった行は除く");
        assert_eq!(base.screens, ["reopen"]);
        assert_eq!(base.logs, ["web"]);
        assert!(!traced.limits.iter().any(|l| l.contains("打ち切った")));
    }

    #[test]
    fn limits_say_what_was_not_examined() {
        let services = [service("web", "", false)];
        let mut cut = scanned(vec![]);
        cut.truncated = true;
        cut.skipped = vec!["app.t: 照会に失敗した".into()];
        let mut limited = report();
        limited.exchange_limit = true;
        let traced = trace(&values(), None, Some(&cut), Some(&limited), &services);
        for expected in [
            "打ち切った",
            "調べなかった表や列が1件",
            "前からあった行を除けていない",
            "上限に達した",
            "最後まで読めなかった",
        ] {
            assert!(
                traced.limits.iter().any(|l| l.contains(expected)),
                "{expected}"
            );
        }
        let failed = DatabaseScan::not_performed("DBが停止しており、探索できない");
        let traced = trace(&values(), None, Some(&failed), None, &[]);
        assert!(traced.limits.iter().any(|l| l.contains("探索できなかった")));
        assert!(traced.appearances.iter().all(|a| a.database.is_empty()));
        let traced = trace(&values(), None, None, None, &[]);
        assert!(traced.limits.iter().any(|l| l == "DBを探索していない"));
    }

    #[test]
    fn drafts_keep_every_matching_column_and_explain_what_they_leave_out() {
        let after = scanned(vec![
            hit("step-note", "visit", "note", 1, 1),
            hit("step-note", "visit_copy", "note", 1, 1),
            hit("step-note", "audit", "query", 2, 0),
        ]);
        let services = [service("web", "", true), service("db", "", true)];
        let traced = trace(&values(), None, Some(&after), Some(&report()), &services);
        let names: Vec<String> = services.iter().map(|s| s.name.clone()).collect();
        let draft = draft("run-1", &traced, Some(&after), &names);
        assert_eq!(draft.rules, RULES);
        // 同じ値が2つの表にあれば、両方を候補にする。値を含むだけの列は候補にしない。
        let tables: Vec<_> = draft.database.iter().map(|p| p.table.as_str()).collect();
        assert_eq!(tables, ["visit", "visit_copy"]);
        let probe = &draft.database[0];
        assert_eq!(
            (
                probe.service.as_str(),
                probe.database.as_str(),
                probe.match_column.as_str()
            ),
            ("db", "app", "note")
        );
        // 別の試行でも使えるよう、値ではなくレシピ上の形で持つ。
        assert_eq!(probe.match_value.as_deref(), Some("{{test_value}}-02"));
        assert!(
            draft
                .skipped
                .iter()
                .any(|s| s.contains("app.audit.query") && s.contains("等しい行がない"))
        );
        // 同じ要求は1件にまとめ、成功でない応答は条件にしない。
        assert_eq!(draft.responses.len(), 1);
        assert_eq!(draft.responses[0].path, "/visit/save");
        assert!(
            draft
                .skipped
                .iter()
                .any(|s| s.contains("/audit") && s.contains("HTTP 500"))
        );
        assert!(draft.skipped.iter().any(|s| s.contains("画面に表示された")));
        // 判定条件: DB2件、HTTP1件、コンテナごとのログ2件。すべてに出所がある。
        let methods: Vec<_> = draft.checks.iter().map(|c| c.method).collect();
        use crate::trial::CheckMethod::*;
        assert_eq!(
            methods,
            [
                DatabaseRows,
                DatabaseRows,
                BrowserResponse,
                ContainerLog,
                ContainerLog
            ]
        );
        assert_eq!(draft.checks[2].text.as_deref(), Some("status=200"));
        assert_eq!(draft.checks[3].text.as_deref(), Some("{{test_value}}"));
        assert!(draft.checks.iter().all(|c| {
            let basis = &draft.provenance.checks[&c.id];
            basis.run_id == "run-1" && basis.rules == RULES && basis.source == "value_trace"
        }));
        assert_eq!(draft.provenance.database.len(), 2);
        assert_eq!(draft.provenance.responses.len(), 1);
        let ids: std::collections::BTreeSet<_> = draft.checks.iter().map(|c| &c.id).collect();
        assert_eq!(ids.len(), draft.checks.len(), "IDは重複しない");
    }

    #[test]
    fn a_postgres_table_outside_public_and_overflowing_candidates_are_left_out_with_reasons() {
        let mut after = scanned(
            (0..20)
                .map(|i| hit("test_value", &format!("t{i}"), "x", 1, 1))
                .collect(),
        );
        let traced = trace(&values()[..1], None, Some(&after), None, &[]);
        let many = draft("run-1", &traced, Some(&after), &[]);
        assert_eq!(many.database.len(), 16);
        assert_eq!(
            many.skipped
                .iter()
                .filter(|s| s.contains("上限の16件"))
                .count(),
            4
        );

        after.engine = DatabaseEngine::Postgres;
        after.database = "records".into();
        after.hits = vec![hit("test_value", "visit", "note", 1, 1)];
        after.hits[0].schema = "clinic".into();
        let traced = trace(&values()[..1], None, Some(&after), None, &[]);
        let other_schema = draft("run-1", &traced, Some(&after), &[]);
        assert!(other_schema.database.is_empty());
        assert!(
            other_schema
                .skipped
                .iter()
                .any(|s| s.contains("public以外"))
        );
        after.hits[0].schema = "public".into();
        let traced = trace(&values()[..1], None, Some(&after), None, &[]);
        let public = draft("run-1", &traced, Some(&after), &[]);
        assert_eq!(public.database[0].database, "records");
    }

    fn browser_spec(extra: serde_json::Value) -> crate::browser::BrowserSpec {
        let mut value = serde_json::json!({
            "service": "web", "tls": false, "start_path": "/", "expected_version": "1.0",
            "preparation": [
                {"id": "user", "action": "fill", "target": {"locator": {"by": "label", "text": "利用者"}}, "value": {"source": "secret", "key": "user"}},
                {"id": "name", "action": "fill", "target": {"locator": {"by": "label", "text": "氏名"}}, "value": {"source": "literal", "value": "{{test_value}}"}}
            ],
            "actions": [
                {"id": "birth", "action": "fill", "target": {"locator": {"by": "label", "text": "生年月日"}}, "value": {"source": "literal", "value": "1990-01-01"}},
                {"id": "note", "action": "fill", "target": {"locator": {"by": "label", "text": "記録"}}, "value": {"source": "literal", "value": "所見 {{test_value}}"}},
                {"id": "save", "action": "click", "target": {"locator": {"by": "label", "text": "保存"}}}
            ],
            "responses": [{"id": "saved", "path": "/save", "method": "POST"}],
            "database": [{"id": "row", "service": "db", "database": "app", "table": "visit", "match_column": "note", "columns": ["note"]}]
        });
        for (key, v) in extra.as_object().unwrap() {
            value[key] = v.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn tracked_values_are_the_typed_texts_and_are_empty_unless_enabled() {
        let off = browser_spec(serde_json::json!({}));
        assert!(values_of(&off, "EEL-TEST-9").is_empty());
        assert!(step_values(&off, "EEL-TEST-9").is_empty());
        // 既定では保存形に新しい項目が出ない。
        let saved = serde_json::to_value(&off).unwrap();
        for key in ["track_values", "value_per_step", "provenance"] {
            assert!(saved.get(key).is_none(), "{key}");
        }

        // 追跡だけを有効にすると、試験値を含む入力の手順ごとに、入力した文字の全体を追う。
        // 秘匿の入力と、試験値を含まない入力は追わない。
        let on = browser_spec(serde_json::json!({"track_values": true}));
        let values = values_of(&on, "EEL-TEST-9");
        let view: Vec<_> = values
            .iter()
            .map(|v| (v.slot.as_str(), v.value.as_str(), v.template.as_str()))
            .collect();
        assert_eq!(
            view,
            [
                ("test_value", "EEL-TEST-9", "{{test_value}}"),
                ("name", "EEL-TEST-9", "{{test_value}}"),
                ("note", "所見 EEL-TEST-9", "所見 {{test_value}}"),
            ]
        );
        assert!(step_values(&on, "EEL-TEST-9").is_empty());

        // 手順ごとの値を有効にすると、連番が付き、レシピ上の形にも同じ連番が入る。
        let each = browser_spec(serde_json::json!({"track_values": true, "value_per_step": true}));
        let own = step_values(&each, "EEL-TEST-9");
        assert_eq!(own["name"], "EEL-TEST-9-01");
        assert_eq!(own["note"], "EEL-TEST-9-02");
        assert_eq!(own.len(), 2);
        let values = values_of(&each, "EEL-TEST-9");
        assert_eq!(
            (
                values[2].value.as_str(),
                values[2].template.as_str(),
                values[2].step.as_deref()
            ),
            ("所見 EEL-TEST-9-02", "所見 {{test_value}}-02", Some("note"))
        );
        // レシピ上の形に、別の試行の試験値を入れても同じ規則の値になる。
        assert_eq!(
            crate::trial::substitute(&values[2].template, "EEL-TEST-10"),
            "所見 EEL-TEST-10-02"
        );
    }

    #[test]
    fn provenance_must_point_at_items_the_recipe_has() {
        let basis = serde_json::json!({"run_id": "run-1", "rules": 1, "source": "value_trace"});
        let ok = browser_spec(serde_json::json!({"provenance": {
            "steps": {"note": basis}, "responses": {"saved": basis}, "database": {"row": basis}, "checks": {"any": basis}}}));
        validate_provenance(ok.provenance.as_ref().unwrap(), &ok).unwrap();
        for provenance in [
            serde_json::json!({"steps": {"no-such-step": basis}}),
            serde_json::json!({"responses": {"no-such-watch": basis}}),
            serde_json::json!({"database": {"no-such-probe": basis}}),
            serde_json::json!({"checks": {"c": {"run_id": "", "rules": 1, "source": "value_trace"}}}),
            serde_json::json!({"checks": {"c": {"run_id": "run-1", "rules": 0, "source": "value_trace"}}}),
        ] {
            let spec = browser_spec(serde_json::json!({"provenance": provenance.clone()}));
            assert!(
                validate_provenance(spec.provenance.as_ref().unwrap(), &spec).is_err(),
                "{provenance}"
            );
        }
    }

    #[test]
    fn values_typed_with_the_same_text_are_not_duplicated_and_covered_places_are_not_repeated() {
        // 2つの欄に同じ文字を入力し、3つ目の欄にはそれを含む別の文字を入力した場合。
        let values = vec![
            TrackedValue {
                slot: "test_value".into(),
                value: "EEL-1".into(),
                step: None,
                template: "{{test_value}}".into(),
            },
            TrackedValue {
                slot: "name".into(),
                value: "EEL-1".into(),
                step: Some("name".into()),
                template: "{{test_value}}".into(),
            },
            TrackedValue {
                slot: "note".into(),
                value: "所見 EEL-1".into(),
                step: Some("note".into()),
                template: "所見 {{test_value}}".into(),
            },
        ];
        let after = scanned(vec![
            hit("test_value", "patient", "name", 1, 1),
            hit("name", "patient", "name", 1, 1),
            // 所見の列は、短い値を「含むだけ」でも現れる。等しい行は長い値の側にある。
            hit("test_value", "visit", "note", 1, 0),
            hit("name", "visit", "note", 1, 0),
            hit("note", "visit", "note", 1, 1),
        ]);
        let traced = trace(&values, None, Some(&after), None, &[]);
        let draft = draft("run-1", &traced, Some(&after), &[]);
        let places: Vec<_> = draft
            .database
            .iter()
            .map(|p| (p.table.as_str(), p.match_value.as_deref().unwrap()))
            .collect();
        assert_eq!(
            places,
            [
                ("patient", "{{test_value}}"),
                ("visit", "所見 {{test_value}}")
            ]
        );
        assert!(
            draft
                .skipped
                .iter()
                .any(|s| s.starts_with("name:") && s.contains("区別できない"))
        );
        assert!(
            !draft.skipped.iter().any(|s| s.contains("visit.note")),
            "等しい行が別の値で見つかった場所は、候補にしない理由を出さない: {:?}",
            draft.skipped
        );
    }
}
