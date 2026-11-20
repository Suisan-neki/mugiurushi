//! 入力した試験値の追跡。試験値は試行ごとに一意なので、対象のスキーマを知らなくても、
//! その値がDBのどの表・列に現れたかを基盤が探せる。
//! 見つからなかったことは、保存されていないことを意味しない。暗号化・ハッシュ化された値、
//! 文字列型でない列、上限で打ち切った範囲は見つけられない。
use crate::database::{Client, DatabaseEngine};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const MAX_TABLES: usize = 2000;
pub const MAX_COLUMNS: usize = 20000;
const MAX_STATEMENT: usize = 24 * 1024;
const MAX_BATCH: usize = 64 * 1024;
/// クライアントの出力を受け取る上限。超えた分は読まずに、クライアントを止める。
pub const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_SKIPPED: usize = 100;
/// 探索の全体に許す時間。列の一覧の取得、各照会、照会を止める操作を合わせた時間である。
pub const SCAN_BUDGET: Duration = Duration::from_secs(90);
/// 期限に達した照会を止めて、残っていないことを確かめるために取っておく時間の上限。
const CANCEL_RESERVE: Duration = Duration::from_secs(5);

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

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

/// 値が見つかった場所。探索の結果では、その時点の行の数を表す。
/// 値の出現の記録（`ValueAppearance`）では、操作の前後で増えた行の数を表す。
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
    /// 操作の前の探索で数えた、その値を含む行の数と、等しい行の数。探索の結果そのものでは0。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rows_before: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub exact_rows_before: u64,
    /// 操作の前の探索がこの場所を調べていない。行が前からあったのか、操作で現れたのかを区別できない。
    /// このとき `rows` と `exact_rows` は、操作の後の数そのままである。
    #[serde(default, skip_serializing_if = "is_false")]
    pub baseline_unknown: bool,
}

/// 表の名前。探索した範囲を比べるために、文字列ではなく組で持つ。並びは列の一覧の順と同じ。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TableName {
    pub schema: String,
    pub table: String,
}

/// 探索を途中で止めた理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStop {
    /// 探索に許した時間に達した。
    Deadline,
    /// 呼び出し側が停止を求めた。
    StopRequested,
    /// クライアントの出力が上限を超えた。
    OutputLimit,
    /// クライアントを起動できなかった。
    ClientFailed,
}

impl ScanStop {
    fn label(self) -> &'static str {
        match self {
            Self::Deadline => "探索に許した時間に達した",
            Self::StopRequested => "停止を求められた",
            Self::OutputLimit => "クライアントの出力が上限を超えた",
            Self::ClientFailed => "DBのクライアントを起動できなかった",
        }
    }
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
    /// 最後まで調べ終えた表の数と、調べ終えた列の数。途中で止めた場合は、一覧にある数より少ない。
    pub tables_scanned: u32,
    pub columns_scanned: u32,
    /// 列の一覧にあった表の数と、列の数。
    #[serde(default)]
    pub tables_listed: u32,
    #[serde(default)]
    pub columns_listed: u32,
    /// 調べなかった表や列と、その理由。
    pub skipped: Vec<String>,
    /// 一覧の上限、時間、停止の求めのどれかで、調べていない範囲がある。
    pub truncated: bool,
    /// 列の一覧が上限を超え、一覧に入れなかった表や列がある。
    #[serde(default, skip_serializing_if = "is_false")]
    pub listing_truncated: bool,
    /// 途中で止めた理由。最後まで進んだ場合は `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<ScanStop>,
    /// 途中で止めた場合の、調べていない最初の表。一覧の順でこの表以降は調べていない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unexamined_from: Option<TableName>,
    /// 照会に失敗して、調べられなかった表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unexamined_tables: Vec<TableName>,
    /// 調べられなかった表が多く、`unexamined_tables` に入りきらなかった。
    #[serde(default, skip_serializing_if = "is_false")]
    pub unexamined_overflow: bool,
    /// 実行中の照会を止めた場合の、止める操作の結果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<String>,
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
            tables_listed: 0,
            columns_listed: 0,
            skipped: vec![],
            truncated: false,
            listing_truncated: false,
            stopped: None,
            unexamined_from: None,
            unexamined_tables: vec![],
            unexamined_overflow: false,
            cancel: None,
            elapsed_ms: 0,
            hits: vec![],
            detail: detail.into(),
        }
    }

    /// この探索が、指定した表を最後まで調べたか。偽のとき、その表に値がなかったとは言えない。
    /// 範囲の記録がない以前の形式の記録は、打ち切りがあれば調べていないものとして扱う。
    pub fn examined(&self, schema: &str, table: &str) -> bool {
        self.performed
            && !self.listing_truncated
            && !self.unexamined_overflow
            && !self
                .unexamined_tables
                .iter()
                .any(|t| t.schema == schema && t.table == table)
            && match &self.unexamined_from {
                Some(from) => (schema, table) < (from.schema.as_str(), from.table.as_str()),
                None => !self.truncated,
            }
    }
}

/// クライアントの起動が、どう終わったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// クライアントが自分で終了した。
    Exited,
    /// 許した時間に達したので、待つのをやめた。
    Deadline,
    /// 停止を求められたので、待つのをやめた。
    Stopped,
    /// 出力が上限を超えたので、読むのをやめた。
    OutputLimit,
}

/// クライアントを1回起動した結果。途中でやめた場合の `stdout` は、そこまでに受け取った分である。
pub struct Executed {
    pub ended: Ended,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// クライアントを1回起動する依頼。
pub struct Request<'a> {
    pub client: &'a Client,
    /// この時間を過ぎたら、待つのをやめる。
    pub within: Duration,
    /// 停止を求められていても実行する（実行中の照会を止める操作）。
    pub ignore_stop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    schema: String,
    table: String,
    columns: Vec<String>,
}

impl Piece {
    fn name(&self) -> TableName {
        TableName {
            schema: self.schema.clone(),
            table: self.table.clone(),
        }
    }
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
        if parts.first() == Some(&crate::database::SESSION_MARK) {
            continue;
        }
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

/// 途中でやめた起動のあと、DBの中に残った接続を名指しで止め、残っていないことを数えて確かめる。
/// ホストの側のプロセスを止めるだけでは、コンテナの中のクライアントとDBの中の照会は止まらない。
fn cancel(
    engine: DatabaseEngine,
    env: &BTreeMap<String, String>,
    database: &str,
    stdout: &str,
    within: Duration,
    execute: &mut impl FnMut(&Request) -> io::Result<Executed>,
) -> String {
    let unconfirmed = "サーバー側の制限時間で止まる設定にしてあるが、止まったことは確かめていない";
    let Some(session) = DatabaseEngine::session_of(stdout) else {
        return format!(
            "接続の番号を受け取る前に待つのをやめたため、DBの中の照会を止める操作をしていない。{unconfirmed}"
        );
    };
    let Some(client) = engine.cancel(env, database, session) else {
        return format!("この種類のDBでは、照会を止める操作ができない。{unconfirmed}");
    };
    let request = Request {
        client: &client,
        within: within.max(Duration::from_secs(1)),
        ignore_stop: true,
    };
    match execute(&request) {
        Ok(out) if out.ended == Ended::Exited => match DatabaseEngine::cancelled(&out.stdout) {
            Some(true) => "実行中の照会の接続を止め、DBに残っていないことを確かめた".into(),
            Some(false) => {
                "実行中の照会の接続を止める操作をしたが、接続がまだDBに残っていた".into()
            }
            None => format!(
                "実行中の照会の接続を止める操作の結果を読めなかった。{unconfirmed}: {}",
                out.stderr.chars().take(200).collect::<String>()
            ),
        },
        Ok(_) => format!("実行中の照会の接続を止める操作が、時間内に終わらなかった。{unconfirmed}"),
        Err(error) => {
            format!("実行中の照会の接続を止める操作を起動できなかった（{error}）。{unconfirmed}")
        }
    }
}

/// 文字列型の列をすべて調べ、追跡する値を含む行の数を表・列ごとに数える。
/// `execute` はコンテナの中でクライアントを起動する。照会できない種類では実行しない。
///
/// `budget` は探索の全体に許す時間で、列の一覧の取得と各照会の両方に効く。
/// 実行中の照会には、残り時間をサーバー側の制限時間として渡す。`execute` には同じ残り時間を渡し、
/// 過ぎたら待つのをやめさせる。待つのをやめた照会は、接続を名指しで止める。
/// `stop` が真になった場合も同じ手順で止める。
/// 途中で止めた探索の結果は、調べ終えた範囲の分だけである。調べていない範囲を、値がなかった範囲として扱わない。
pub fn scan(
    engine: DatabaseEngine,
    env: &BTreeMap<String, String>,
    values: &[TrackedValue],
    budget: Duration,
    stop: &AtomicBool,
    mut execute: impl FnMut(&Request) -> io::Result<Executed>,
) -> DatabaseScan {
    let database = engine.scan_database(env);
    if engine.ping(env).is_none() {
        return DatabaseScan::not_performed(format!(
            "DBの種類（{}）は基盤の読み出しに対応していないため、探索していない",
            engine.label()
        ));
    }
    if values.is_empty() || values.iter().any(|v| v.value.is_empty()) {
        return DatabaseScan::not_performed("追跡する値がないため、探索していない");
    }
    let started = Instant::now();
    // 照会に使える時間は、止める操作のための時間を除いた分。全体で `budget` に収める。
    let reserve = CANCEL_RESERVE.min(budget / 3);
    let query_deadline = started + (budget - reserve);
    let hard_deadline = started + budget;
    let left = |deadline: Instant| deadline.saturating_duration_since(Instant::now());
    let elapsed = |started: Instant| started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let stop_of = |ended: Ended| match ended {
        Ended::Deadline => ScanStop::Deadline,
        Ended::Stopped => ScanStop::StopRequested,
        Ended::OutputLimit => ScanStop::OutputLimit,
        Ended::Exited => ScanStop::ClientFailed,
    };
    let unlisted = |reason: ScanStop, cancel: Option<String>, more: &str| DatabaseScan {
        engine,
        database: database.clone(),
        truncated: true,
        stopped: Some(reason),
        cancel,
        elapsed_ms: elapsed(started),
        ..DatabaseScan::not_performed(format!(
            "列の一覧を読み終える前に探索を止めた（{}）。どの表も調べていない{more}",
            reason.label()
        ))
    };
    if stop.load(Ordering::SeqCst) {
        return unlisted(ScanStop::StopRequested, None, "");
    }
    if left(query_deadline).is_zero() {
        return unlisted(ScanStop::Deadline, None, "");
    }
    let Some(list) = engine.run_limited(
        env,
        &database,
        &[engine.text_columns_sql().to_owned()],
        left(query_deadline),
    ) else {
        return DatabaseScan::not_performed("列の一覧を読むクライアントを組み立てられない");
    };
    let listing = match execute(&Request {
        client: &list,
        within: left(query_deadline),
        ignore_stop: false,
    }) {
        Ok(out) if out.ended == Ended::Exited && out.success => out.stdout,
        Ok(out) if out.ended == Ended::Exited => {
            return DatabaseScan::not_performed(format!(
                "列の一覧を読めなかった: {}",
                out.stderr.chars().take(300).collect::<String>()
            ));
        }
        Ok(out) => {
            let cancelled = cancel(
                engine,
                env,
                &database,
                &out.stdout,
                left(hard_deadline),
                &mut execute,
            );
            return unlisted(stop_of(out.ended), Some(cancelled), "");
        }
        Err(error) => {
            return DatabaseScan::not_performed(format!("DBのクライアントを起動できない: {error}"));
        }
    };
    let mut skipped = vec![];
    let mut listing_truncated = false;
    let (pieces, tables_listed, columns_listed) =
        pieces(&listing, values, &mut skipped, &mut listing_truncated);
    let texts: Vec<String> = values.iter().map(|v| v.value.clone()).collect();
    let mut hits = vec![];
    let mut stopped = None;
    let mut cancelled = None;
    let mut failed: Vec<TableName> = vec![];
    let mut unexamined_overflow = false;
    let mut done = vec![false; pieces.len()];
    let mut next = 0;
    while next < pieces.len() {
        if stop.load(Ordering::SeqCst) {
            stopped = Some(ScanStop::StopRequested);
            break;
        }
        let remaining = left(query_deadline);
        if remaining.is_zero() {
            stopped = Some(ScanStop::Deadline);
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
        let Some(client) = engine.run_limited(env, &database, &statements, remaining) else {
            stopped = Some(ScanStop::ClientFailed);
            break;
        };
        let out = match execute(&Request {
            client: &client,
            within: remaining,
            ignore_stop: false,
        }) {
            Ok(out) => out,
            Err(_) => {
                stopped = Some(ScanStop::ClientFailed);
                break;
            }
        };
        // 途中でやめた起動の出力は、最後の行が途中で切れていることがある。改行で終わった行だけを読む。
        let complete = match out.ended {
            Ended::Exited => out.stdout.as_str(),
            _ => out.stdout.rfind('\n').map_or("", |at| &out.stdout[..=at]),
        };
        // 返ってきた行を、目印で表に対応づける。途中で失敗した場合は、その表だけを飛ばして続ける。
        let mut answered = next;
        for line in complete.lines() {
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
                failed.push(piece.name());
            } else {
                done[tag] = true;
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
                                rows_before: 0,
                                exact_rows_before: 0,
                                baseline_unknown: false,
                            });
                        }
                    }
                }
            }
            answered = tag + 1;
        }
        if out.ended != Ended::Exited {
            // 待つのをやめた。コンテナの中のクライアントとDBの中の照会を残さないよう、接続を止める。
            stopped = Some(stop_of(out.ended));
            cancelled = Some(cancel(
                engine,
                env,
                &database,
                &out.stdout,
                left(hard_deadline),
                &mut execute,
            ));
            next = answered;
            break;
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
            failed.push(piece.name());
            answered += 1;
        }
        next = answered;
    }
    // 調べ終えた範囲を数える。表は、分けたすべての部分を調べ終えた場合だけ数える。
    let mut per_table: BTreeMap<TableName, bool> = BTreeMap::new();
    let mut columns_scanned = 0u32;
    for (piece, done) in pieces.iter().zip(&done) {
        let complete = per_table.entry(piece.name()).or_insert(true);
        *complete &= *done;
        if *done {
            columns_scanned += piece.columns.len() as u32;
        }
    }
    let tables_scanned = per_table.values().filter(|complete| **complete).count() as u32;
    let unexamined_from = pieces.get(next).map(Piece::name);
    failed.dedup();
    if failed.len() > MAX_SKIPPED {
        failed.truncate(MAX_SKIPPED);
        unexamined_overflow = true;
    }
    let truncated = listing_truncated || stopped.is_some();
    let mut detail = format!(
        "文字列型の列{columns_listed}件（表{tables_listed}件）のうち、列{columns_scanned}件（表{tables_scanned}件）を調べ終え、値を含む列を{}件見つけた。見つからない列に値がないとは限らない（暗号化・ハッシュ化・文字列型でない列は見つけられない）",
        hits.len(),
    );
    if let Some(reason) = stopped {
        detail.push_str(&format!(
            "。途中で探索を止めた（{}）。調べていない表に値がなかったとは言えない",
            reason.label()
        ));
    }
    if listing_truncated {
        detail.push_str("。列の一覧が上限を超え、一覧に入れなかった表や列がある");
    }
    DatabaseScan {
        performed: true,
        service: String::new(),
        engine,
        database,
        tables_scanned,
        columns_scanned,
        tables_listed,
        columns_listed,
        truncated,
        listing_truncated,
        stopped,
        unexamined_from,
        unexamined_tables: failed,
        unexamined_overflow,
        cancel: cancelled,
        elapsed_ms: elapsed(started),
        detail,
        skipped,
        hits,
    }
}

/// 操作の前後の探索を比べた、操作の後に増えた出現。
///
/// 値を含む行の数と、値と等しい行の数を、それぞれ前後の差で数える。
/// 操作の前の探索がその表を調べていない場合は、差を計算せず、`baseline_unknown` を付けて操作の後の数を出す。
/// 件数の差では、同じ場所で行の削除と追加が同数あった場合を見分けられない。
pub fn new_hits(before: Option<&DatabaseScan>, after: &DatabaseScan) -> Vec<ScanHit> {
    let earlier: BTreeMap<_, (u64, u64)> = before
        .filter(|b| b.performed)
        .map(|b| {
            b.hits
                .iter()
                .map(|h| {
                    (
                        (&h.slot, &h.schema, &h.table, &h.column),
                        (h.rows, h.exact_rows),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    after
        .hits
        .iter()
        .filter_map(|h| {
            if !before.is_some_and(|b| b.examined(&h.schema, &h.table)) {
                return Some(ScanHit {
                    rows_before: 0,
                    exact_rows_before: 0,
                    baseline_unknown: true,
                    ..h.clone()
                });
            }
            let (rows_before, exact_rows_before) = earlier
                .get(&(&h.slot, &h.schema, &h.table, &h.column))
                .copied()
                .unwrap_or((0, 0));
            let rows = h.rows.saturating_sub(rows_before);
            let exact_rows = h.exact_rows.saturating_sub(exact_rows_before);
            (rows > 0 || exact_rows > 0).then(|| ScanHit {
                rows,
                exact_rows,
                rows_before,
                exact_rows_before,
                baseline_unknown: false,
                ..h.clone()
            })
        })
        .collect()
}

/// 操作の前後の探索の比較で、分からないこと。
fn comparison_limits(before: Option<&DatabaseScan>, after: &DatabaseScan) -> Vec<String> {
    let mut limits = vec![];
    let place = |h: &ScanHit| format!("{}.{}.{}", h.schema, h.table, h.column);
    match before {
        Some(b) if b.performed => {
            limits.push(
                "DBの出現は、操作の前後の行の数の差で数えた。同じ場所で行の削除と追加が同数あった場合と、前からある行が書き換えられた場合は、見分けられない"
                    .into(),
            );
            if b.truncated || !b.unexamined_tables.is_empty() || b.unexamined_overflow {
                limits.push(
                    "操作の前の探索に、調べていない表がある。その表での出現は、前からあった行と区別できていない"
                        .into(),
                );
            }
            let key = |h: &ScanHit| (h.slot.clone(), h.schema.clone(), h.table.clone(), h.column.clone());
            let later: BTreeMap<_, (u64, u64)> = after
                .hits
                .iter()
                .map(|h| (key(h), (h.rows, h.exact_rows)))
                .collect();
            let decreased: Vec<String> = b
                .hits
                .iter()
                .filter(|h| after.examined(&h.schema, &h.table))
                .filter(|h| {
                    let (rows, exact) = later.get(&key(h)).copied().unwrap_or((0, 0));
                    rows < h.rows || exact < h.exact_rows
                })
                .map(place)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !decreased.is_empty() {
                limits.push(format!(
                    "操作の後に、値を含む行か値と等しい行が減った場所がある（{}{}）。増えた行と減った行が打ち消し合っている場合を、件数の差では見分けられない",
                    decreased.iter().take(5).cloned().collect::<Vec<_>>().join("、"),
                    if decreased.len() > 5 { " ほか" } else { "" }
                ));
            }
        }
        _ => limits.push(
            "操作の前の探索がないため、前からあった行を除けていない。DBでの出現を、操作で現れたものとして扱わない"
                .into(),
        ),
    }
    limits
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
                limits.push(format!(
                    "DBの探索を途中で止めた（{}）。調べ終えたのは、一覧にある表{}件のうち{}件である。調べていない表に値がなかったとは言えない",
                    match scan.stopped {
                        Some(reason) => reason.label(),
                        None => "列の一覧が上限を超えた",
                    },
                    scan.tables_listed,
                    scan.tables_scanned
                ));
            }
            if !scan.skipped.is_empty() {
                limits.push(format!(
                    "DBの探索で調べなかった表や列が{}件ある",
                    scan.skipped.len()
                ));
            }
            limits.extend(comparison_limits(before, scan));
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
/// 版2：操作の前から値と等しい行があった場所と、操作の前に調べていない場所からは、DBの候補を作らない。
pub const RULES: u32 = 3;
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

/// DBで値と等しい行が操作で増えた列、値を送った要求、コンテナのログから候補を作る。
/// 同じ値が複数の表や列に現れた場合は、すべてを候補にし、1つに絞らない。
/// DBの候補は、列の値が等しい行を読む設定になる。操作の前から等しい行があった場所では、
/// その設定は前からある行も読むので、今回の保存を確かめる条件にならない。そういう場所は候補にしない。
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
        .filter(|h| h.exact_rows > 0 && h.exact_rows_before == 0 && !h.baseline_unknown)
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
            // 操作の前の探索がこの場所を調べていない。行が操作で現れたとは言えない。
            if hit.baseline_unknown {
                out.skipped.push(format!(
                    "{place}: 操作の前の探索がこの表を調べていないため、値を含む行（{}件）が前からあったのか、操作で現れたのかを区別できない。候補にしていない",
                    hit.rows
                ));
                continue;
            }
            // 既存の読み出しは、列の値が等しい行を読む。値を含むだけの列は表せない。
            if hit.exact_rows == 0 {
                if exact.contains(&(hit.schema.clone(), hit.table.clone(), hit.column.clone())) {
                    continue;
                }
                out.skipped.push(if hit.exact_rows_before > 0 {
                    format!(
                        "{place}: 値を含む行が{}件増えたが、列の値と等しい行は操作の前から{}件あり、増えていない。等値で読む設定は前からある行を読むので、候補にしていない",
                        hit.rows, hit.exact_rows_before
                    )
                } else {
                    format!(
                        "{place}: 値を含む行が{}件増えたが、列の値と等しい行がない。等値で読む設定では表せない",
                        hit.rows
                    )
                });
                continue;
            }
            if hit.exact_rows_before > 0 {
                out.skipped.push(format!(
                    "{place}: 列の値と等しい行が{}件増えたが、操作の前から{}件あった。等値で読む設定では、前からある行と今回の保存を区別できないので、候補にしていない",
                    hit.exact_rows, hit.exact_rows_before
                ));
                continue;
            }
            // PostgreSQLでは、接続するデータベースと表のあるスキーマが別である。
            // public の表は、変更前の候補と同じく、スキーマを書かない。
            let (database, schema) = match scan.engine {
                DatabaseEngine::Postgres => (
                    scan.database.clone(),
                    (hit.schema != "public").then(|| hit.schema.clone()),
                ),
                _ => (hit.schema.clone(), None),
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
                schema,
                match_column: hit.column.clone(),
                columns: vec![hit.column.clone()],
                match_value: Some(value.template.clone()),
                match_from: None,
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

    fn exited(success: bool, stdout: impl Into<String>, stderr: &str) -> Executed {
        Executed {
            ended: Ended::Exited,
            success,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn no_stop() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// 表ごとの答えを決めておき、目印から表を引いて返す架空のクライアント。
    fn fake(
        listing: &'static str,
        answers: &'static [(&'static str, &'static str)],
        fail: &'static [&'static str],
    ) -> impl FnMut(&Request) -> io::Result<Executed> {
        move |request: &Request| {
            let sql = request.client.argv.last().unwrap();
            if sql.contains("information_schema.columns") {
                return Ok(exited(true, format!("#session\t7\n{listing}"), ""));
            }
            let mut stdout = String::new();
            for statement in sql.split(';') {
                if statement.starts_with("SET SESSION ") {
                    continue;
                }
                if statement.contains(crate::database::SESSION_MARK) {
                    stdout.push_str("#session\t7\n");
                    continue;
                }
                let tag = statement
                    .trim_start_matches("SELECT ")
                    .split(',')
                    .next()
                    .unwrap();
                if fail.iter().any(|t| statement.ends_with(&format!("`{t}`"))) {
                    return Ok(exited(
                        false,
                        stdout,
                        "ERROR 1267: Illegal mix of collations",
                    ));
                }
                let counts = answers
                    .iter()
                    .find(|(t, _)| statement.ends_with(&format!("`{t}`")))
                    .map(|(_, c)| *c)
                    .unwrap();
                stdout.push_str(&format!("{tag}\t{counts}\n"));
            }
            Ok(exited(true, stdout, ""))
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
            &no_stop(),
            // 列ごと・値ごとに「含む数・等しい数」が並ぶ。audit: 列1つ×値2つ。visit: 列2つ×値2つ。
            fake(
                listing,
                &[("audit", "3\t0\t1\t0"), ("visit", "1\t1\t0\t0\t1\t0\t1\t1")],
                &[],
            ),
        );
        assert!(scan.performed && !scan.truncated, "{}", scan.detail);
        assert_eq!((scan.tables_scanned, scan.columns_scanned), (2, 3));
        assert_eq!((scan.tables_listed, scan.columns_listed), (2, 3));
        assert!(scan.stopped.is_none() && scan.unexamined_from.is_none() && scan.cancel.is_none());
        assert!(scan.examined("app", "audit") && scan.examined("app", "visit"));
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
            &no_stop(),
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
        // 失敗した表は、調べていない表として残す。ほかの表は調べ終えている。
        assert_eq!(scan.unexamined_tables.len(), 1);
        assert_eq!(scan.unexamined_tables[0].table, "broken");
        assert!(!scan.examined("app", "broken"));
        assert!(scan.examined("app", "a") && scan.examined("app", "c"));
        assert_eq!((scan.tables_listed, scan.tables_scanned), (3, 2));
        assert!(!scan.truncated && scan.stopped.is_none());
    }

    #[test]
    fn nothing_found_is_distinguished_from_not_performed_and_truncated() {
        // 対応していない種類、値なし、一覧の読取失敗は、探索を実行していない。
        let none = scan(
            DatabaseEngine::Other,
            &BTreeMap::new(),
            &values(),
            SCAN_BUDGET,
            &no_stop(),
            |_| unreachable!(),
        );
        assert!(!none.performed && none.detail.contains("対応していない"));
        let empty = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &[],
            SCAN_BUDGET,
            &no_stop(),
            |_| unreachable!(),
        );
        assert!(!empty.performed);
        let failed = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values(),
            SCAN_BUDGET,
            &no_stop(),
            |_| Ok(exited(false, "", "Can't connect")),
        );
        assert!(!failed.performed && failed.detail.contains("Can't connect"));
        // 実行して何も見つからない場合は、実行済みで0件。説明に限界を書く。
        let clean = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            SCAN_BUDGET,
            &no_stop(),
            fake("app\ta\tx\n", &[("a", "0\t0")], &[]),
        );
        assert!(clean.performed && clean.hits.is_empty() && !clean.truncated);
        assert!(clean.detail.contains("値がないとは限らない"));
    }

    /// 途中でやめる起動を1回だけ返し、止める操作の依頼を記録する架空のクライアント。
    struct Interrupted {
        ended: Ended,
        partial: &'static str,
        cancel_output: &'static str,
        seen: std::cell::RefCell<Vec<(String, Duration, bool)>>,
    }

    impl Interrupted {
        fn run(&self, request: &Request) -> io::Result<Executed> {
            let sql = request.client.argv.last().unwrap().clone();
            self.seen
                .borrow_mut()
                .push((sql.clone(), request.within, request.ignore_stop));
            if sql.contains("information_schema.columns") {
                return Ok(exited(
                    true,
                    "#session\t7\napp\ta\tx\napp\tb\tx\napp\tc\tx\n",
                    "",
                ));
            }
            if sql.contains("KILL ") {
                return Ok(exited(true, self.cancel_output, ""));
            }
            Ok(Executed {
                ended: self.ended,
                success: false,
                stdout: self.partial.into(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn a_query_that_does_not_return_in_time_is_cancelled_and_only_finished_tables_count() {
        // 1つ目の表の答えと、2つ目の表の答えの途中までを受け取ったところで、期限に達した。
        let client = Interrupted {
            ended: Ended::Deadline,
            partial: "#session\t7\n0\t1\t1\n1\t4",
            cancel_output: "1\n0\n",
            seen: Default::default(),
        };
        let scan = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            Duration::from_secs(30),
            &no_stop(),
            |request| client.run(request),
        );
        assert!(scan.performed && scan.truncated, "{}", scan.detail);
        assert_eq!(scan.stopped, Some(ScanStop::Deadline));
        // 途中で切れた行は数えない。調べ終えたのは最初の表だけ。
        assert_eq!(scan.hits.len(), 1);
        assert_eq!((scan.hits[0].table.as_str(), scan.hits[0].rows), ("a", 1));
        assert_eq!((scan.tables_listed, scan.tables_scanned), (3, 1));
        assert_eq!((scan.columns_listed, scan.columns_scanned), (3, 1));
        assert_eq!(scan.unexamined_from.as_ref().unwrap().table, "b");
        assert!(scan.examined("app", "a"));
        assert!(!scan.examined("app", "b") && !scan.examined("app", "c"));
        assert!(
            scan.detail.contains("値がなかったとは言えない"),
            "{}",
            scan.detail
        );
        // 受け取った番号の接続を止め、残っていないことを確かめている。
        let seen = client.seen.borrow();
        let (cancel, _, ignore_stop) = seen.last().unwrap();
        assert!(cancel.contains(";KILL 7;") && *ignore_stop, "{cancel}");
        assert!(
            scan.cancel
                .as_ref()
                .unwrap()
                .contains("残っていないことを確かめた")
        );
        // 列の一覧と照会に渡した時間は、止める操作のための時間を除いた残り時間以内。
        for (sql, within, _) in seen.iter().filter(|(sql, _, _)| !sql.contains("KILL ")) {
            assert!(*within <= Duration::from_secs(25), "{within:?}");
            assert!(sql.starts_with("SET SESSION max_statement_time="), "{sql}");
        }
    }

    #[test]
    fn a_stop_request_ends_the_scan_and_the_cancel_step_still_runs() {
        let stop = AtomicBool::new(false);
        let client = Interrupted {
            ended: Ended::Stopped,
            partial: "#session\t7\n",
            cancel_output: "1\n1\n",
            seen: Default::default(),
        };
        let scan = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            Duration::from_secs(30),
            &stop,
            |request| {
                // 照会の実行中に、停止を求められた。
                if !request
                    .client
                    .argv
                    .last()
                    .unwrap()
                    .contains("information_schema")
                {
                    stop.store(true, Ordering::SeqCst);
                }
                client.run(request)
            },
        );
        assert_eq!(scan.stopped, Some(ScanStop::StopRequested));
        assert!(scan.performed && scan.truncated && scan.hits.is_empty());
        assert_eq!((scan.tables_listed, scan.tables_scanned), (3, 0));
        assert_eq!(scan.unexamined_from.as_ref().unwrap().table, "a");
        // 止める操作は、停止を求められていても実行する。接続が残っていた場合は、そう記録する。
        assert!(client.seen.borrow().last().unwrap().2);
        assert!(scan.cancel.as_ref().unwrap().contains("まだDBに残っていた"));
        // 始める前から停止を求められていた場合は、クライアントを起動しない。
        let before = scan_stopped_before_start();
        assert!(!before.performed && before.truncated);
        assert_eq!(before.stopped, Some(ScanStop::StopRequested));
        assert!(before.detail.contains("どの表も調べていない"));
    }

    fn scan_stopped_before_start() -> DatabaseScan {
        scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values(),
            SCAN_BUDGET,
            &AtomicBool::new(true),
            |_| unreachable!(),
        )
    }

    #[test]
    fn an_interrupted_listing_and_an_unknown_session_are_recorded_as_not_examined() {
        // 時間がない場合は、クライアントを起動せずに、調べていないと記録する。
        let late = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            Duration::ZERO,
            &no_stop(),
            |_| unreachable!(),
        );
        assert!(!late.performed && late.truncated && late.hits.is_empty());
        assert_eq!(late.stopped, Some(ScanStop::Deadline));
        assert!(late.detail.contains("どの表も調べていない"));
        assert!(!late.examined("app", "a"));
        // 列の一覧の取得が終わらなかった。接続の番号も受け取っていないので、止めたことを確かめられない。
        let mut calls = 0;
        let listing = scan(
            DatabaseEngine::Postgres,
            &BTreeMap::new(),
            &values()[..1],
            Duration::from_secs(30),
            &no_stop(),
            |_| {
                calls += 1;
                Ok(Executed {
                    ended: Ended::Deadline,
                    success: false,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            },
        );
        assert_eq!(calls, 1);
        assert!(!listing.performed && listing.stopped == Some(ScanStop::Deadline));
        assert!(listing.cancel.as_ref().unwrap().contains("確かめていない"));
        // 出力が上限を超えた場合も、途中で止めたと記録する。
        let big = Interrupted {
            ended: Ended::OutputLimit,
            partial: "#session\t7\n0\t1\t1\n",
            cancel_output: "0\n0\n",
            seen: Default::default(),
        };
        let scan = scan(
            DatabaseEngine::Mariadb,
            &BTreeMap::new(),
            &values()[..1],
            Duration::from_secs(30),
            &no_stop(),
            |request| big.run(request),
        );
        assert_eq!(scan.stopped, Some(ScanStop::OutputLimit));
        assert_eq!((scan.hits.len(), scan.tables_scanned), (1, 1));
    }

    fn at(table: &str, rows: u64, exact_rows: u64) -> ScanHit {
        hit("test_value", table, "body", rows, exact_rows)
    }

    #[test]
    fn rows_from_before_the_operation_are_subtracted_for_both_counts() {
        // 操作の前：値と等しい行が1件。操作の後：値を含むが等しくない行が1件増えた。
        let added = new_hits(
            Some(&scanned(vec![at("note", 1, 1)])),
            &scanned(vec![at("note", 2, 1)]),
        );
        assert_eq!(added.len(), 1);
        assert_eq!((added[0].rows, added[0].exact_rows), (1, 0));
        assert_eq!((added[0].rows_before, added[0].exact_rows_before), (1, 1));
        assert!(!added[0].baseline_unknown);
        // 値と等しい行が新しく増えた。前になかった場所と、前から1件あった場所。
        let added = new_hits(
            Some(&scanned(vec![at("old", 1, 1)])),
            &scanned(vec![at("fresh", 1, 1), at("old", 2, 2)]),
        );
        let found: Vec<_> = added
            .iter()
            .map(|h| (h.table.as_str(), h.rows, h.exact_rows, h.exact_rows_before))
            .collect();
        assert_eq!(found, [("fresh", 1, 1, 0), ("old", 1, 1, 1)]);
        // 前後で変化がない場所は、新しい出現にしない。減った場所も出現にしない。
        let same = scanned(vec![at("note", 3, 2)]);
        assert!(new_hits(Some(&same), &same).is_empty());
        assert!(new_hits(Some(&same), &scanned(vec![at("note", 2, 1)])).is_empty());
        // 含む行の数は同じで、等しい行だけが増えた（前からある行が書き換えられた）。
        let rewritten = new_hits(
            Some(&scanned(vec![at("note", 1, 0)])),
            &scanned(vec![at("note", 1, 1)]),
        );
        assert_eq!((rewritten[0].rows, rewritten[0].exact_rows), (0, 1));
    }

    #[test]
    fn places_the_earlier_scan_did_not_examine_are_not_called_new() {
        let after = scanned(vec![at("a", 1, 1), at("m", 1, 1), at("z", 2, 2)]);
        let unknown = |before: Option<&DatabaseScan>| -> Vec<(String, bool, u64)> {
            new_hits(before, &after)
                .into_iter()
                .map(|h| (h.table, h.baseline_unknown, h.exact_rows))
                .collect()
        };
        let all_unknown = [
            ("a".to_owned(), true, 1),
            ("m".to_owned(), true, 1),
            ("z".to_owned(), true, 2),
        ];
        // 操作の前の探索がない、または実行できなかった。
        assert_eq!(unknown(None), all_unknown);
        assert_eq!(
            unknown(Some(&DatabaseScan::not_performed("DBが停止"))),
            all_unknown
        );
        // 操作の前の探索が、表 m の手前で止まった。m 以降は比べられない。
        let mut stopped = scanned(vec![]);
        stopped.truncated = true;
        stopped.stopped = Some(ScanStop::Deadline);
        stopped.unexamined_from = Some(TableName {
            schema: "app".into(),
            table: "m".into(),
        });
        assert_eq!(
            unknown(Some(&stopped)),
            [
                ("a".to_owned(), false, 1),
                ("m".to_owned(), true, 1),
                ("z".to_owned(), true, 2)
            ]
        );
        // 操作の前の探索で、表 z の照会が失敗した。z だけが比べられない。
        let mut failed = scanned(vec![at("z", 1, 1)]);
        failed.unexamined_tables = vec![TableName {
            schema: "app".into(),
            table: "z".into(),
        }];
        assert_eq!(
            unknown(Some(&failed)),
            [
                ("a".to_owned(), false, 1),
                ("m".to_owned(), false, 1),
                ("z".to_owned(), true, 2)
            ]
        );
        // 一覧が上限を超えた探索と、範囲の記録がない打ち切りは、どの表も比べられない。
        let mut listing = scanned(vec![]);
        listing.listing_truncated = true;
        listing.truncated = true;
        assert_eq!(unknown(Some(&listing)), all_unknown);
        let mut old_format = scanned(vec![]);
        old_format.truncated = true;
        assert_eq!(unknown(Some(&old_format)), all_unknown);
    }

    #[test]
    fn candidates_are_not_made_from_rows_that_were_there_before_or_cannot_be_compared() {
        let value = values()[..1].to_vec();
        let draft_of = |before: Option<&DatabaseScan>, after: &DatabaseScan| {
            let trace = trace(&value, before, Some(after), None, &[]);
            (draft("run-1", &trace, Some(after), &[]), trace)
        };
        // 指摘の例：前から等しい行が1件あり、操作で増えたのは「含むだけ」の行。
        let (made, traced) = draft_of(
            Some(&scanned(vec![at("note", 1, 1)])),
            &scanned(vec![at("note", 2, 1)]),
        );
        assert!(
            made.database.is_empty() && made.checks.is_empty(),
            "{:?}",
            made.database
        );
        assert!(
            made.skipped
                .iter()
                .any(|s| s.contains("app.note.body") && s.contains("操作の前から1件"))
        );
        assert!(
            traced
                .limits
                .iter()
                .any(|l| l.contains("行の数の差で数えた"))
        );
        // 前から等しい行がある場所で、等しい行が増えた。等値の条件は前からある行も読むので、候補にしない。
        let (made, _) = draft_of(
            Some(&scanned(vec![at("note", 1, 1)])),
            &scanned(vec![at("note", 2, 2)]),
        );
        assert!(made.database.is_empty());
        assert!(made.skipped.iter().any(|s| s.contains("区別できない")));
        // 前になかった場所に、等しい行が増えた。同じ値が2つの列に現れた場合は、両方を候補にする。
        let (made, _) = draft_of(
            Some(&scanned(vec![])),
            &scanned(vec![
                at("note", 1, 1),
                hit("test_value", "audit", "text", 1, 1),
            ]),
        );
        let places: Vec<_> = made
            .database
            .iter()
            .map(|p| (p.table.as_str(), p.match_column.as_str()))
            .collect();
        assert_eq!(places, [("note", "body"), ("audit", "text")]);
        assert_eq!(made.rules, RULES);
        // 前後で変化がなければ、候補も、候補にしない理由もない。
        let same = scanned(vec![at("note", 1, 1)]);
        let (made, traced) = draft_of(Some(&same), &same);
        assert!(made.database.is_empty() && made.skipped.is_empty());
        assert!(traced.appearances[0].database.is_empty());
        // 操作の前の探索が不完全な場合は、候補にせず、理由と限界を残す。
        for before in [None, Some(DatabaseScan::not_performed("DBが停止"))] {
            let (made, traced) = draft_of(before.as_ref(), &scanned(vec![at("note", 1, 1)]));
            assert!(made.database.is_empty());
            assert!(
                made.skipped
                    .iter()
                    .any(|s| s.contains("操作の前の探索がこの表を調べていない"))
            );
            assert!(traced.appearances[0].database[0].baseline_unknown);
            assert!(
                traced
                    .limits
                    .iter()
                    .any(|l| l.contains("操作の前の探索がない"))
            );
        }
        let mut partial = scanned(vec![]);
        partial.truncated = true;
        partial.stopped = Some(ScanStop::Deadline);
        partial.unexamined_from = Some(TableName {
            schema: "app".into(),
            table: "note".into(),
        });
        let (made, traced) = draft_of(
            Some(&partial),
            &scanned(vec![at("audit", 1, 1), at("note", 1, 1)]),
        );
        assert_eq!(made.database.len(), 1);
        assert_eq!(made.database[0].table, "audit");
        assert!(
            traced
                .limits
                .iter()
                .any(|l| l.contains("操作の前の探索に、調べていない表がある"))
        );
        // 行が減った場所があることは、限界として残す。
        let (_, traced) = draft_of(
            Some(&scanned(vec![at("note", 2, 1)])),
            &scanned(vec![at("note", 1, 1)]),
        );
        assert!(
            traced
                .limits
                .iter()
                .any(|l| l.contains("減った場所がある（app.note.body）"))
        );
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
            rows_before: 0,
            exact_rows_before: 0,
            baseline_unknown: false,
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
        assert!(!traced.limits.iter().any(|l| l.contains("途中で止めた")));
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
            "途中で止めた",
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
        let traced = trace(
            &values(),
            Some(&scanned(vec![])),
            Some(&after),
            Some(&report()),
            &services,
        );
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
        let traced = trace(
            &values()[..1],
            Some(&scanned(vec![])),
            Some(&after),
            None,
            &[],
        );
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
        let traced = trace(
            &values()[..1],
            Some(&scanned(vec![])),
            Some(&after),
            None,
            &[],
        );
        // public以外のスキーマの表は、スキーマの名前つきで候補にする。
        let other_schema = draft("run-1", &traced, Some(&after), &[]);
        assert_eq!(other_schema.database.len(), 1);
        assert_eq!(other_schema.database[0].schema.as_deref(), Some("clinic"));
        assert!(
            other_schema.skipped.is_empty(),
            "{:?}",
            other_schema.skipped
        );
        after.hits[0].schema = "public".into();
        let traced = trace(
            &values()[..1],
            Some(&scanned(vec![])),
            Some(&after),
            None,
            &[],
        );
        let public = draft("run-1", &traced, Some(&after), &[]);
        assert_eq!(public.database[0].database, "records");
        assert_eq!(public.database[0].schema, None);
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
        let traced = trace(&values, Some(&scanned(vec![])), Some(&after), None, &[]);
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
