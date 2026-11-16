//! 対象とDBの間に置く中継。MariaDB／MySQLのプロトコルを解釈し、対象がDBへ送った文の種類と、
//! コミットの時点を記録する。指定した文の決まった時点で接続を切ることもできる。
//! SQLの本文は記録しない。残すのは、文の種類、表の名前、リテラルを除いた形のハッシュ、
//! 追跡する値を含むかどうかである。
//! 暗号化または圧縮された接続は解釈できない。その接続はそのまま転送し、読めなかったことを記録する。
use crate::error::invalid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Instant,
};

/// 中継の設定を渡す環境変数。JSONで書く。
pub const CONFIG_VARIABLE: &str = "EEL_RELAY";
pub const MAX_STATEMENTS: usize = 5_000;
/// 基盤が、試行の段階（準備、操作）を中継へ知らせるためのファイル。これができるまで、中継は文を数えない。
pub const PHASE_FILE: &str = "/tmp/eel-phase";
const EVENT_PREFIX: &str = "stmt ";
const MAX_PACKET: usize = 16 * 1024 * 1024;
const CLIENT_SSL: u16 = 0x0800;
const CLIENT_COMPRESS: u16 = 0x0020;
const SERVER_STATUS_IN_TRANS: u16 = 0x0001;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// 解釈せずに転送し、接続の集計だけを残す。
    Tcp,
    /// MariaDB／MySQLのプロトコル。
    Mysql,
}

/// 接続を切る時点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutPhase {
    /// 文をDBへ送る前。DBは文を受け取らない。
    BeforeForward,
    /// 文をDBへ送った後、応答を対象へ返す前。DBは処理を終えているが、対象は結果を知らない。
    AfterForwardBeforeReply,
    /// 応答を対象へ返した直後。
    AfterReply,
}

/// 「この形の文の、何回目」で接続を切るかの指定。回数は中継を起動してからの通し。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutPoint {
    pub shape: String,
    pub occurrence: u32,
    pub phase: CutPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tracked {
    pub slot: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: u16,
    pub upstream: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub values: Vec<Tracked>,
    #[serde(default)]
    pub cut: Option<CutPoint>,
}

/// 文の種類。先頭の語で決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementClass {
    Select,
    Insert,
    Update,
    Delete,
    Replace,
    Begin,
    Commit,
    Rollback,
    Set,
    /// 表の定義を変える文。
    Ddl,
    Other,
}

impl StatementClass {
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Insert | Self::Update | Self::Delete | Self::Replace | Self::Ddl
        )
    }
}

/// 対象がDBへ送った文1つの記録。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementEvent {
    pub connection: u64,
    /// 中継を起動してからの通し番号。
    pub ordinal: u64,
    pub t_ms: u64,
    pub class: StatementClass,
    pub table: Option<String>,
    /// リテラルを除いて正規化した文のハッシュ（先頭16桁）。
    pub shape: String,
    /// 同じ形の文の、何回目か。基盤が段階を知らせた後から数える。
    pub occurrence: u32,
    /// 文を送ったときの試行の段階（`preparation`、`action`）。
    #[serde(default)]
    pub phase: String,
    /// 追跡する値のうち、この文に含まれていたもの。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<String>,
    /// 最初の応答。`ok`、`error`、`rows`。応答を見ていなければ `None`。
    #[serde(default)]
    pub reply: Option<String>,
    /// 応答の後、トランザクションの中か。`ok` の応答だけで分かる。
    #[serde(default)]
    pub in_transaction: Option<bool>,
    /// この文の応答で、書き込みが確定した。
    #[serde(default)]
    pub commit_point: bool,
    /// この文で接続を切った場合の、切った時点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<CutPhase>,
}

/// コメントと空白を飛ばした先頭の位置。
fn skip_noise(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("/*") {
            match after.find("*/") {
                Some(end) => rest = after[end + 2..].trim_start(),
                None => return "",
            }
        } else if rest.starts_with("-- ") || rest.starts_with('#') {
            match rest.find('\n') {
                Some(end) => rest = rest[end + 1..].trim_start(),
                None => return "",
            }
        } else {
            return rest;
        }
    }
}

fn identifier(token: &str) -> Option<String> {
    let name = token
        .trim_matches(|c: char| c == '(' || c == ',' || c == ';')
        .rsplit('.')
        .next()?
        .trim_matches('`')
        .trim_matches('"');
    crate::browser::sql_identifier(name).then(|| name.to_owned())
}

/// 文の種類、最初の表の名前、リテラルを除いた形のハッシュを求める。
pub fn classify(sql: &str) -> (StatementClass, Option<String>, String) {
    let body = skip_noise(sql);
    let words: Vec<&str> = body.split_whitespace().take(64).collect();
    let upper: Vec<String> = words.iter().map(|w| w.to_ascii_uppercase()).collect();
    let first = upper.first().map(String::as_str).unwrap_or("");
    let after = |keyword: &str| {
        upper
            .iter()
            .position(|w| w == keyword)
            .and_then(|i| words.get(i + 1))
            .and_then(|w| identifier(w))
    };
    let (class, table) = match first {
        "SELECT" => (StatementClass::Select, after("FROM")),
        "INSERT" => (StatementClass::Insert, after("INTO")),
        "REPLACE" => (StatementClass::Replace, after("INTO")),
        "UPDATE" => (
            StatementClass::Update,
            words
                .iter()
                .skip(1)
                .find(|w| !["LOW_PRIORITY", "IGNORE"].contains(&w.to_ascii_uppercase().as_str()))
                .and_then(|w| identifier(w)),
        ),
        "DELETE" => (StatementClass::Delete, after("FROM")),
        "BEGIN" | "START" => (StatementClass::Begin, None),
        "COMMIT" => (StatementClass::Commit, None),
        "ROLLBACK" => (StatementClass::Rollback, None),
        "SET" => (StatementClass::Set, None),
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" => (StatementClass::Ddl, None),
        _ => (StatementClass::Other, None),
    };
    (class, table, shape(body))
}

/// 文字列と数値のリテラルを `?` に置き換え、空白と大文字小文字をそろえた形のハッシュ。
pub fn shape(sql: &str) -> String {
    let mut normal = String::with_capacity(sql.len().min(4096));
    let mut chars = sql.chars().take(16 * 1024).peekable();
    let mut space = false;
    while let Some(c) = chars.next() {
        if c == '\'' || c == '"' {
            // 引用符で囲まれた文字列を、閉じる引用符まで読み飛ばす。
            while let Some(d) = chars.next() {
                if d == '\\' {
                    chars.next();
                } else if d == c {
                    if chars.peek() == Some(&c) {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            normal.push('?');
            space = false;
        } else if c.is_ascii_digit()
            && !normal
                .chars()
                .last()
                .is_some_and(|p| p.is_ascii_alphanumeric() || p == '_' || p == '`')
        {
            while chars
                .peek()
                .is_some_and(|d| d.is_ascii_alphanumeric() || *d == '.')
            {
                chars.next();
            }
            normal.push('?');
            space = false;
        } else if c.is_whitespace() {
            space = !normal.is_empty();
        } else {
            if space {
                normal.push(' ');
            }
            space = false;
            normal.extend(c.to_lowercase());
        }
    }
    format!("{:x}", Sha256::digest(normal.as_bytes()))[..16].to_owned()
}

/// サーバーの最初の案内から、暗号化と圧縮の機能を外す。以降の通信を平文に保つためである。
/// 形が読めない場合は何も変えず、偽を返す。
pub fn strip_capabilities(payload: &mut [u8]) -> bool {
    if payload.first() != Some(&10) {
        return false;
    }
    let Some(end) = payload.iter().skip(1).position(|b| *b == 0) else {
        return false;
    };
    // 版の文字列、接続ID（4）、認証データ（8）、詰め物（1）の後に、機能の下位2バイトがある。
    let at = 1 + end + 1 + 4 + 8 + 1;
    if payload.len() < at + 2 {
        return false;
    }
    let lower =
        u16::from_le_bytes([payload[at], payload[at + 1]]) & !(CLIENT_SSL | CLIENT_COMPRESS);
    payload[at..at + 2].copy_from_slice(&lower.to_le_bytes());
    true
}

fn length_encoded(payload: &[u8], at: &mut usize) -> Option<u64> {
    let first = *payload.get(*at)?;
    let (size, value) = match first {
        0..=0xfa => (1, u64::from(first)),
        0xfc => (
            3,
            u64::from(u16::from_le_bytes([
                *payload.get(*at + 1)?,
                *payload.get(*at + 2)?,
            ])),
        ),
        0xfd => {
            let b = payload.get(*at + 1..*at + 4)?;
            (
                4,
                u64::from(b[0]) | u64::from(b[1]) << 8 | u64::from(b[2]) << 16,
            )
        }
        0xfe => (
            9,
            u64::from_le_bytes(payload.get(*at + 1..*at + 9)?.try_into().ok()?),
        ),
        _ => return None,
    };
    *at += size;
    Some(value)
}

/// 応答の最初のパケットを読む。`ok` のときは、トランザクションの中かどうかも返す。
pub fn reply_of(payload: &[u8]) -> (&'static str, Option<bool>) {
    match payload.first() {
        Some(0x00) => {
            let mut at = 1;
            let status = length_encoded(payload, &mut at)
                .and_then(|_| length_encoded(payload, &mut at))
                .and_then(|_| payload.get(at..at + 2))
                .map(|b| u16::from_le_bytes([b[0], b[1]]));
            ("ok", status.map(|s| s & SERVER_STATUS_IN_TRANS != 0))
        }
        Some(0xff) => ("error", None),
        _ => ("rows", None),
    }
}

struct Shared {
    config: Config,
    started: Instant,
    out: Mutex<io::Stdout>,
    counters: Mutex<(u64, u64, BTreeMap<String, u32>)>,
    /// 種類ごとの文の数。記録に残さない文（読み出しなど）も数える。
    totals: Mutex<BTreeMap<String, u64>>,
    /// 最後に読んだ段階と、読んだ時刻。文ごとにファイルを読まないようにする。
    phase: Mutex<(Option<String>, Option<Instant>)>,
}

impl Shared {
    /// 文の記録。接続の集計の行（`{` で始まる）と区別できるよう、目印を付ける。
    fn emit(&self, value: serde_json::Value) {
        self.line(&format!("{EVENT_PREFIX}{value}"));
    }
    fn line(&self, text: &str) {
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{text}");
            let _ = out.flush();
        }
    }
    /// 基盤が知らせた試行の段階。まだ知らされていなければ `None` で、その間の文は数えない。
    /// 対象の起動時の初期化（大量の書き込み）を、試験の操作と混ぜないためである。
    fn phase(&self) -> Option<String> {
        let mut cached = self.phase.lock().expect("phase");
        if cached.1.is_none_or(|at| at.elapsed().as_millis() >= 20) {
            cached.0 = std::fs::read_to_string(PHASE_FILE)
                .ok()
                .map(|p| p.trim().chars().take(32).collect::<String>())
                .filter(|p| !p.is_empty());
            cached.1 = Some(Instant::now());
        }
        cached.0.clone()
    }
    fn totals(&self) {
        let totals = self.totals.lock().expect("totals").clone();
        self.emit(serde_json::json!({"e": "totals", "by_class": totals, "t_ms": self.elapsed()}));
    }
    fn elapsed(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
}

#[derive(Default)]
struct Connection {
    /// 応答を待っている文の通し番号と種類。
    awaiting: Option<(u64, StatementClass)>,
    /// この接続で、次の応答のときに切る時点。
    cut: Option<(u64, CutPhase)>,
    /// 準備した文のIDと、その種類・表・形。
    prepared: BTreeMap<u32, (StatementClass, Option<String>, String, Vec<String>)>,
    /// 準備の応答を待っている文。
    preparing: Option<(StatementClass, Option<String>, String, Vec<String>)>,
    /// 解釈をやめて、そのまま転送する。
    opaque: bool,
    greeted: bool,
}

fn read_packet(stream: &mut TcpStream) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0u8; 4];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length =
        usize::from(header[0]) | usize::from(header[1]) << 8 | usize::from(header[2]) << 16;
    if length > MAX_PACKET {
        return Err(invalid("パケットが大きすぎます"));
    }
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok(Some((header[3], payload)))
}

fn write_packet(stream: &mut TcpStream, sequence: u8, payload: &[u8]) -> io::Result<()> {
    let length = payload.len();
    stream.write_all(&[
        length as u8,
        (length >> 8) as u8,
        (length >> 16) as u8,
        sequence,
    ])?;
    stream.write_all(payload)
}

fn close(a: &TcpStream, b: &TcpStream) {
    let _ = a.shutdown(Shutdown::Both);
    let _ = b.shutdown(Shutdown::Both);
}

/// 解釈しない接続。双方向へそのまま写し、受け取ったバイト数を返す。
fn copy(mut from: TcpStream, mut to: TcpStream) -> u64 {
    let mut total = 0;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buffer[..n]).is_err() {
                    break;
                }
                total += n as u64;
            }
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    total
}

fn slots_in(shared: &Shared, sql: &str) -> Vec<String> {
    shared
        .config
        .values
        .iter()
        .filter(|v| !v.value.is_empty() && sql.contains(&v.value))
        .map(|v| v.slot.clone())
        .collect()
}

/// 文を1つ数え、記録し、この文で切るかどうかを返す。基盤が段階を知らせる前の文は `None`。
/// 記録に残すのは、書き込み、トランザクションの操作、追跡する値を含む文である。
/// それ以外（読み出しなど）は、種類ごとの数だけを残す。
fn statement(
    shared: &Shared,
    connection: u64,
    described: (StatementClass, Option<String>, String, Vec<String>),
) -> Option<(u64, Option<CutPhase>)> {
    let phase = shared.phase()?;
    let (class, table, shape, slots) = described;
    let (ordinal, occurrence) = {
        let mut counters = shared.counters.lock().expect("counters");
        counters.1 += 1;
        let ordinal = counters.1;
        let count = counters.2.entry(shape.clone()).or_insert(0);
        *count += 1;
        (ordinal, *count)
    };
    let name = serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    *shared
        .totals
        .lock()
        .expect("totals")
        .entry(name)
        .or_insert(0) += 1;
    let cut = shared
        .config
        .cut
        .as_ref()
        .filter(|c| c.shape == shape && c.occurrence == occurrence)
        .map(|c| c.phase);
    let recorded = cut.is_some()
        || !slots.is_empty()
        || class.writes()
        || matches!(
            class,
            StatementClass::Begin | StatementClass::Commit | StatementClass::Rollback
        );
    if recorded {
        shared.emit(
            serde_json::json!({"e": "sent", "connection": connection, "ordinal": ordinal,
            "t_ms": shared.elapsed(), "class": class, "table": table, "shape": shape,
            "occurrence": occurrence, "phase": phase, "slots": slots}),
        );
    }
    Some((ordinal, cut))
}

fn serve_mysql(client: TcpStream, server: TcpStream, shared: Arc<Shared>, id: u64) -> (u64, u64) {
    let state = Arc::new(Mutex::new(Connection::default()));
    let (mut client_read, mut client_write) = match (client.try_clone(), client.try_clone()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return (0, 0),
    };
    let (mut server_read, mut server_write) = match (server.try_clone(), server.try_clone()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return (0, 0),
    };
    // DBから対象への向き。
    let downstream = {
        let (state, shared, client, server) = (
            state.clone(),
            shared.clone(),
            client.try_clone(),
            server.try_clone(),
        );
        std::thread::spawn(move || -> u64 {
            let (Ok(client), Ok(server)) = (client, server) else {
                return 0;
            };
            let mut sent = 0u64;
            loop {
                if state.lock().expect("state").opaque {
                    return sent + copy(server_read, client_write);
                }
                let Ok(Some((sequence, mut payload))) = read_packet(&mut server_read) else {
                    break;
                };
                let mut cut_after = None;
                {
                    let mut state = state.lock().expect("state");
                    if !state.greeted {
                        state.greeted = true;
                        if !strip_capabilities(&mut payload) {
                            state.opaque = true;
                            shared.emit(serde_json::json!({"e": "opaque", "connection": id,
                                "reason": "サーバーの最初の案内を読めない"}));
                        }
                    } else if let Some(described) = state.preparing.take() {
                        // 準備の応答。成功なら、文のIDと種類を対応づける。
                        if payload.first() == Some(&0x00) && payload.len() >= 5 {
                            let statement_id = u32::from_le_bytes([
                                payload[1], payload[2], payload[3], payload[4],
                            ]);
                            if state.prepared.len() < 4096 {
                                state.prepared.insert(statement_id, described);
                            }
                        }
                    } else if let Some((ordinal, class)) = state.awaiting.take() {
                        let (reply, in_transaction) = reply_of(&payload);
                        // 応答が成功で、トランザクションの外にあれば、この時点で書き込みが確定している。
                        let commit_point = reply == "ok"
                            && in_transaction == Some(false)
                            && (class.writes() || class == StatementClass::Commit);
                        shared.emit(serde_json::json!({"e": "reply", "ordinal": ordinal, "t_ms": shared.elapsed(),
                            "reply": reply, "in_transaction": in_transaction, "commit_point": commit_point}));
                        if let Some((target, phase)) = state.cut
                            && target == ordinal
                        {
                            state.cut = None;
                            shared.emit(
                                serde_json::json!({"e": "cut", "ordinal": ordinal, "phase": phase,
                                "t_ms": shared.elapsed()}),
                            );
                            if phase == CutPhase::AfterForwardBeforeReply {
                                close(&client, &server);
                                return sent;
                            }
                            cut_after = Some(());
                        }
                    }
                }
                if write_packet(&mut client_write, sequence, &payload).is_err() {
                    break;
                }
                sent += payload.len() as u64 + 4;
                if cut_after.is_some() {
                    close(&client, &server);
                    return sent;
                }
            }
            close(&client, &server);
            sent
        })
    };
    // 対象からDBへの向き。
    let mut received = 0u64;
    loop {
        if state.lock().expect("state").opaque {
            received += copy(client_read, server_write);
            break;
        }
        let Ok(Some((sequence, payload))) = read_packet(&mut client_read) else {
            break;
        };
        received += payload.len() as u64 + 4;
        let mut cut_before = false;
        {
            let mut state = state.lock().expect("state");
            if sequence != 0 {
                // 接続の開始のやり取り。暗号化の要求（機能だけを送る短いパケット）なら、解釈をやめる。
                if payload.len() == 32
                    && u16::from_le_bytes([payload[0], payload[1]]) & CLIENT_SSL != 0
                {
                    state.opaque = true;
                    shared.emit(serde_json::json!({"e": "opaque", "connection": id,
                        "reason": "対象が暗号化を要求した"}));
                }
            } else {
                let text = || String::from_utf8_lossy(&payload[1..]).into_owned();
                let described = match payload.first() {
                    Some(0x03) => {
                        let sql = text();
                        let (class, table, shape) = classify(&sql);
                        Some((class, table, shape, slots_in(&shared, &sql)))
                    }
                    Some(0x16) => {
                        let sql = text();
                        let (class, table, shape) = classify(&sql);
                        state.preparing = Some((class, table, shape, slots_in(&shared, &sql)));
                        None
                    }
                    Some(0x17) if payload.len() >= 5 => {
                        let statement_id =
                            u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
                        // 準備した文の値は、実行のパケットに入る。値の出現は、実行のパケットで調べる。
                        state.prepared.get(&statement_id).cloned().map(|mut d| {
                            let body = String::from_utf8_lossy(&payload[5..]);
                            for slot in slots_in(&shared, &body) {
                                if !d.3.contains(&slot) {
                                    d.3.push(slot);
                                }
                            }
                            d
                        })
                    }
                    Some(0x19) if payload.len() >= 5 => {
                        let statement_id =
                            u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
                        state.prepared.remove(&statement_id);
                        None
                    }
                    _ => None,
                };
                if let Some((class, Some((ordinal, cut)))) =
                    described.map(|d| (d.0, statement(&shared, id, d)))
                {
                    match cut {
                        Some(CutPhase::BeforeForward) => {
                            shared.emit(serde_json::json!({"e": "cut", "ordinal": ordinal,
                                "phase": CutPhase::BeforeForward, "t_ms": shared.elapsed()}));
                            cut_before = true;
                        }
                        Some(phase) => state.cut = Some((ordinal, phase)),
                        None => {}
                    }
                    state.awaiting = Some((ordinal, class));
                }
            }
        }
        if cut_before {
            break;
        }
        if write_packet(&mut server_write, sequence, &payload).is_err() {
            break;
        }
    }
    close(&client, &server);
    let sent = downstream.join().unwrap_or(0);
    (received, sent)
}

/// 中継を起動する。接続ごとにスレッドを使う。試験用の少数の接続だけを想定している。
pub fn serve(config: Config) -> io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", config.listen))?;
    let shared = Arc::new(Shared {
        config,
        started: Instant::now(),
        out: Mutex::new(io::stdout()),
        counters: Mutex::new((0, 0, BTreeMap::new())),
        totals: Mutex::new(BTreeMap::new()),
        phase: Mutex::new((None, None)),
    });
    shared.emit(serde_json::json!({"e": "ready", "protocol": shared.config.protocol}));
    for client in listener.incoming() {
        let Ok(client) = client else { continue };
        let shared = shared.clone();
        std::thread::spawn(move || {
            let id = {
                let mut counters = shared.counters.lock().expect("counters");
                counters.0 += 1;
                counters.0
            };
            let peer = client
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            let local = client
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            // 既存の接続の集計が読む形に合わせて、接続の開始を標準エラーへ残す。
            eprintln!("relay [info] 1#1: *{id} client {peer} connected to {local}");
            let started = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let (status, received, sent) = match TcpStream::connect(&shared.config.upstream) {
                Err(_) => {
                    let _ = client.shutdown(Shutdown::Both);
                    ("502", 0, 0)
                }
                Ok(server) => {
                    let _ = client.set_nodelay(true);
                    let _ = server.set_nodelay(true);
                    let (received, sent) = match shared.config.protocol {
                        Protocol::Mysql => serve_mysql(client, server, shared.clone(), id),
                        Protocol::Tcp => {
                            let (c2, s2) = (client.try_clone(), server.try_clone());
                            match (c2, s2) {
                                (Ok(c2), Ok(s2)) => {
                                    let down = std::thread::spawn(move || copy(s2, c2));
                                    let received = copy(client, server);
                                    (received, down.join().unwrap_or(0))
                                }
                                _ => (0, 0),
                            }
                        }
                    };
                    ("200", received, sent)
                }
            };
            // 記録に残さない文を含めた、種類ごとの数。接続が終わるたびに、その時点の合計を出す。
            shared.totals();
            // 接続の終了は、既存の中継と同じ項目で標準出力へ残す。
            let ip = peer.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(&peer);
            shared.line(
                &serde_json::json!({"id": id, "t": started, "client": ip,
                "local": local.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(&local),
                "status": status, "in": received, "out": sent})
                .to_string(),
            );
        });
    }
    Ok(())
}

/// 中継の標準出力から、文の記録を組み立てる。件数の上限を超えた分は捨て、捨てたことを返す。
pub fn read_log(stdout: &str) -> LogRead {
    let mut events: BTreeMap<u64, StatementEvent> = BTreeMap::new();
    let mut opaque = vec![];
    let mut truncated = false;
    let mut totals = BTreeMap::new();
    for line in stdout.lines() {
        let Some(Ok(value)) = line
            .strip_prefix(EVENT_PREFIX)
            .map(serde_json::from_str::<serde_json::Value>)
        else {
            continue;
        };
        let ordinal = value["ordinal"].as_u64().unwrap_or(0);
        match value["e"].as_str() {
            Some("sent") => {
                if events.len() >= MAX_STATEMENTS {
                    truncated = true;
                    continue;
                }
                let mut object = value.clone();
                object.as_object_mut().map(|o| o.remove("e"));
                if let Ok(event) = serde_json::from_value::<StatementEvent>(object) {
                    events.insert(ordinal, event);
                }
            }
            Some("reply") => {
                if let Some(event) = events.get_mut(&ordinal) {
                    event.reply = value["reply"].as_str().map(str::to_owned);
                    event.in_transaction = value["in_transaction"].as_bool();
                    event.commit_point = value["commit_point"].as_bool().unwrap_or(false);
                }
            }
            Some("cut") => {
                if let Some(event) = events.get_mut(&ordinal) {
                    event.cut = serde_json::from_value(value["phase"].clone()).ok();
                }
            }
            Some("opaque") => opaque.extend(value["connection"].as_u64()),
            // 合計は累積で出るので、最後のものを使う。
            Some("totals") => {
                if let Ok(by_class) = serde_json::from_value(value["by_class"].clone()) {
                    totals = by_class;
                }
            }
            _ => {}
        }
    }
    LogRead {
        statements: events.into_values().collect(),
        opaque,
        truncated,
        totals,
    }
}

/// 中継の記録を読んだ結果。
pub struct LogRead {
    /// 書き込み、トランザクションの操作、追跡する値を含む文。
    pub statements: Vec<StatementEvent>,
    /// 暗号化などで読めなかった接続。
    pub opaque: Vec<u64>,
    /// 件数の上限を超えて捨てた。
    pub truncated: bool,
    /// 記録に残さない文を含めた、種類ごとの数。
    pub totals: BTreeMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_are_classified_by_their_first_word_and_table() {
        let of = |sql: &str| {
            let (class, table, _) = classify(sql);
            (class, table)
        };
        assert_eq!(
            of("INSERT INTO `form_soap` (pid, subjective) VALUES (1, 'x')"),
            (StatementClass::Insert, Some("form_soap".into()))
        );
        assert_eq!(
            of("  /* note */ insert ignore into openemr.log (event) values ('y')"),
            (StatementClass::Insert, Some("log".into()))
        );
        assert_eq!(
            of("UPDATE patient_data SET lname = 'z' WHERE pid = 3"),
            (StatementClass::Update, Some("patient_data".into()))
        );
        assert_eq!(
            of("DELETE FROM form_soap WHERE id=9"),
            (StatementClass::Delete, Some("form_soap".into()))
        );
        assert_eq!(
            of("select a, b\nfrom visit v where v.id = 1"),
            (StatementClass::Select, Some("visit".into()))
        );
        assert_eq!(of("-- c\nCOMMIT"), (StatementClass::Commit, None));
        assert_eq!(of("START TRANSACTION"), (StatementClass::Begin, None));
        assert_eq!(of("SET NAMES utf8mb4"), (StatementClass::Set, None));
        assert_eq!(of("ALTER TABLE t ADD c INT"), (StatementClass::Ddl, None));
        assert_eq!(of("SHOW TABLES"), (StatementClass::Other, None));
        assert_eq!(of(""), (StatementClass::Other, None));
        // 識別子として読めない名前は、表の名前にしない。
        assert_eq!(
            of("INSERT INTO `odd name` VALUES (1)").0,
            StatementClass::Insert
        );
        assert!(StatementClass::Insert.writes() && !StatementClass::Select.writes());
        assert!(!StatementClass::Commit.writes());
    }

    #[test]
    fn the_shape_ignores_literals_case_and_spacing_but_not_structure() {
        let a = shape("INSERT INTO form_soap (pid, subjective) VALUES (12, 'EEL-TEST-1 it''s')");
        let b = shape("insert  into form_soap (pid, subjective)\n values (7,\"other \\\" text\")");
        let c = shape("INSERT INTO form_soap (pid, objective) VALUES (12, 'x')");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
        // 識別子の中の数字は、リテラルとして扱わない。
        assert_ne!(shape("SELECT * FROM t1"), shape("SELECT * FROM t2"));
        assert_eq!(
            shape("SELECT * FROM t WHERE id = 5"),
            shape("SELECT * FROM t WHERE id = 60")
        );
    }

    fn greeting() -> Vec<u8> {
        let mut payload = vec![10];
        payload.extend(b"11.4.2-MariaDB\0");
        payload.extend([1, 0, 0, 0]);
        payload.extend(b"abcdefgh");
        payload.push(0);
        payload.extend(0xffffu16.to_le_bytes());
        payload.extend([45, 2, 0, 0xff, 0xff]);
        payload
    }

    #[test]
    fn encryption_and_compression_are_removed_from_the_server_greeting() {
        let mut payload = greeting();
        let at = 1 + b"11.4.2-MariaDB\0".len() + 4 + 8 + 1;
        assert!(strip_capabilities(&mut payload));
        let lower = u16::from_le_bytes([payload[at], payload[at + 1]]);
        assert_eq!(lower & CLIENT_SSL, 0);
        assert_eq!(lower & CLIENT_COMPRESS, 0);
        assert_eq!(lower | CLIENT_SSL | CLIENT_COMPRESS, 0xffff);
        // 形の違う案内は変えない。
        let mut other = vec![9, 1, 2, 3];
        assert!(!strip_capabilities(&mut other));
        assert_eq!(other, [9, 1, 2, 3]);
        let mut short = vec![10, b'x', 0, 1];
        assert!(!strip_capabilities(&mut short));
    }

    #[test]
    fn the_first_reply_packet_tells_success_error_or_rows_and_the_transaction_state() {
        // OK: 影響した行1、最後のID 5、状態は自動コミット（0x0002）。
        assert_eq!(
            reply_of(&[0x00, 1, 5, 0x02, 0x00, 0, 0]),
            ("ok", Some(false))
        );
        // OK: トランザクションの中（0x0001）。
        assert_eq!(
            reply_of(&[0x00, 0, 0, 0x01, 0x00, 0, 0]),
            ("ok", Some(true))
        );
        // 長さ付きの整数が3バイトの形でも読める。
        assert_eq!(
            reply_of(&[0x00, 0xfc, 0x10, 0x27, 0, 0x03, 0x00]),
            ("ok", Some(true))
        );
        assert_eq!(reply_of(&[0xff, 0x48, 0x04]), ("error", None));
        assert_eq!(reply_of(&[0x03]), ("rows", None));
        // 途中で切れたOKは、成功とだけ分かる。
        assert_eq!(reply_of(&[0x00, 1]), ("ok", None));
    }

    #[test]
    fn the_log_is_joined_per_statement_and_unknown_lines_are_ignored() {
        let log = r#"stmt {"e":"ready","protocol":"mysql"}
stmt {"e":"sent","connection":1,"ordinal":1,"t_ms":5,"class":"begin","table":null,"shape":"aaaaaaaaaaaaaaaa","occurrence":1,"slots":[]}
stmt {"e":"reply","ordinal":1,"t_ms":6,"reply":"ok","in_transaction":true,"commit_point":false}
stmt {"e":"totals","by_class":{"select":3},"t_ms":6}
stmt {"e":"sent","connection":1,"ordinal":2,"t_ms":7,"class":"insert","table":"form_soap","shape":"bbbbbbbbbbbbbbbb","occurrence":1,"phase":"action","slots":["fill-plan"]}
stmt {"e":"reply","ordinal":2,"t_ms":8,"reply":"ok","in_transaction":true,"commit_point":false}
stmt {"e":"sent","connection":1,"ordinal":3,"t_ms":9,"class":"commit","table":null,"shape":"cccccccccccccccc","occurrence":1,"slots":[]}
stmt {"e":"reply","ordinal":3,"t_ms":10,"reply":"ok","in_transaction":false,"commit_point":true}
stmt {"e":"cut","ordinal":3,"phase":"after_forward_before_reply","t_ms":10}
stmt not json
{"id":1,"t":0.005,"client":"172.18.0.3","local":"172.18.0.2","status":"200","in":120,"out":300}
stmt {"e":"opaque","connection":2,"reason":"対象が暗号化を要求した"}
stmt {"e":"totals","by_class":{"begin":1,"commit":1,"insert":1,"select":41},"t_ms":12}
"#;
        let read = read_log(log);
        let (events, opaque) = (read.statements, read.opaque);
        assert_eq!(events.len(), 3);
        assert!(!read.truncated);
        assert_eq!(opaque, [2]);
        // 記録に残さない読み出しも、種類ごとの数には入る。合計は最後のものを使う。
        assert_eq!(read.totals["select"], 41);
        assert_eq!(read.totals["insert"], 1);
        assert_eq!(events[1].phase, "action");
        assert_eq!(events[1].class, StatementClass::Insert);
        assert_eq!(events[1].slots, ["fill-plan"]);
        assert_eq!(events[1].reply.as_deref(), Some("ok"));
        assert_eq!(events[1].in_transaction, Some(true));
        assert!(!events[1].commit_point);
        // 応答を返す前に切った文では、DBの応答は中継が見ているが、対象へは返していない。
        assert_eq!(events[2].reply.as_deref(), Some("ok"));
        assert!(events[2].commit_point);
        assert_eq!(events[2].cut, Some(CutPhase::AfterForwardBeforeReply));
    }
}
