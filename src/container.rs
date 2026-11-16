//! 試験専用のコンテナを、試行ごとに作って止めて消す。
//! イメージはダイジェスト（`@sha256:` と64桁）付きだけを受け付ける。タグだけでは起動しない。
//! 待ち受けは 127.0.0.1 だけに公開する。Dockerが使えないときはほかの方式へ切り替えない。
use crate::database::DatabaseEngine;
use crate::error::invalid;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    net::{Ipv4Addr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static SESSION: AtomicU64 = AtomicU64::new(1);

pub const PORT_PREFIX: &str = "{{port:";
/// コンテナ1つの準備を待つ上限。初回の起動でDBのスキーマを作る対象は、数分では終わらない
/// （OpenMRS 3.8.0 の初回の起動は、4コアの環境で約13分だった）。
pub const MAX_READY_MS: u64 = 1_800_000;
const MAX_SERVICES: usize = 8;
const MAX_LOG: usize = 8192;
/// コンテナへ置くファイル1件の上限。
const MAX_PLACED_FILE: u64 = 1024 * 1024;
/// コンテナ1つへ置くファイルの件数の上限。
const MAX_PLACED_FILES: usize = 16;
/// コンテナ1つに付ける別名の件数の上限。
const MAX_ALIASES: usize = 8;
/// 記録に残すログの末尾の行数。
const EXCERPT_LINES: usize = 80;
/// 判定に使うログの上限（標準出力・標準エラーそれぞれ）。超えた分は読み捨て、ログは最後まで読めていない扱いにする。
const MAX_FULL_LOG: usize = 4 * 1024 * 1024;
const MAX_BODY: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceRole {
    App,
    Database,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyCheck {
    Tcp,
    Http {
        path: String,
        tls: bool,
    },
    /// コンテナ内でコマンドが成功するまで待つ（MariaDBの healthcheck など）。
    Exec {
        args: Vec<String>,
    },
    /// コンテナが終了コード0で終わるまで待つ。証明書の用意のように、1回動いて終わるコンテナに使う。
    /// 待ち受けを持たないので、中継を置かず、ホストへも公開しない。
    Completed,
}

/// 固定したコミットの中のファイルを、コンテナの中へ置く指定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMount {
    /// リポジトリの最上位からの相対パス。
    pub from: String,
    /// コンテナの中の置き場所（絶対パス）。親のディレクトリがなければ作る。
    pub to: String,
}

/// コンテナへ置いたファイルの記録。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub from: String,
    pub to: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeMount {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    pub image: String,
    pub role: ServiceRole,
    /// コンテナ内の待ち受けポート。ホスト側は基盤が 127.0.0.1 の空きポートを割り当てる。
    /// 終了を待つコンテナ（`ready` が `completed`）では指定しない。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub expose: u16,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeMount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_build: Option<crate::prepared_web::WebBuild>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    pub ready: ReadyCheck,
    pub ready_timeout_ms: u64,
    /// DBの役割のコンテナの種類。指定のない設定はMariaDBとして読み、保存するときも省く。
    #[serde(default, skip_serializing_if = "DatabaseEngine::is_default")]
    pub engine: DatabaseEngine,
    /// 名前を挙げたコンテナの準備ができてから、このコンテナを起動する。
    /// 指定のないコンテナは、役割の順（DB、その他、アプリ）と一覧の順で起動する。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub start_after: Vec<String>,
    /// 固定したコミットの中のファイルを、起動の前にコンテナの中へ置く。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileMount>,
    /// 試験用ネットワークの中で、このコンテナに届く別の名前。ドットを含むホスト名を書ける。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerSpec {
    pub services: Vec<Service>,
    /// 対象のコンテナから外部への通信。指定しなければ遮断する。
    #[serde(default, skip_serializing_if = "Egress::is_blocked")]
    pub egress: Egress,
    /// 対象の手前の中継を、プロトコルを解釈する中継に置き換える指定。DBへ送った文の並びを記録する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<RelaySetting>,
}

/// プロトコルを解釈する中継を置く対象と、その中継に行わせること。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaySetting {
    /// 中継を置く対象の名前。通常はDBのコンテナ。
    pub service: String,
    pub protocol: crate::relay::Protocol,
    /// 指定した文の決まった時点で接続を切る。故障の種類が「中継で接続を切る」の試行で使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<crate::relay::CutPoint>,
}

/// 中継のコンテナの土台。基盤の実行ファイルをこの中で動かすため、同じ世代のglibcを持つイメージにする。
const RELAY_IMAGE: &str =
    "ubuntu@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852";

/// 試験用ネットワークの外部通信。`Allowed`は対象が外部へ通信できることを記録に残す。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Egress {
    #[default]
    Blocked,
    Allowed,
}
impl Egress {
    fn is_blocked(&self) -> bool {
        *self == Self::Blocked
    }
    fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Allowed => "allowed",
        }
    }
}

/// 遮断時に、各対象の前に置く中継コンテナ。ホストの127.0.0.1と対象、対象どうしの接続をここで通し、記録する。対象ではなく基盤が使う。
/// 対象の本体はネットワーク別名`<名前>-real`で動き、ほかの対象は元の名前で中継へ接続する。
const GATEWAY_IMAGE: &str =
    "nginx@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591";
const MAX_TRAFFIC_LOG: usize = 4 * 1024 * 1024;
const MAX_TRAFFIC_ROWS: usize = 64;
/// 対象のコンテナに残す権限。ほかはすべて外す。
const KEPT_CAPABILITIES: [&str; 6] = [
    "CHOWN",
    "DAC_OVERRIDE",
    "FOWNER",
    "SETGID",
    "SETUID",
    "NET_BIND_SERVICE",
];
const PIDS_LIMIT: &str = "2048";

/// コンテナへ基盤が行うHTTP操作。対象プロセスの起動とは別。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpOp {
    pub service: String,
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicePhase {
    pub name: String,
    pub role: ServiceRole,
    pub image: String,
    pub digest: String,
    pub container: String,
    pub port: u16,
    pub started: bool,
    pub ready: bool,
    pub ready_after_ms: Option<u64>,
    pub running_after_stop: bool,
    pub log_excerpt: String,
    /// ログをどこまで読めたかの説明（記録用）。旧形式の記録にはない。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub log_note: String,
    /// 標準出力と標準エラーのログを、上限内で最後まで読めたか。判定にだけ使い、記録には残さない。
    /// 記録から読み直した値は常に偽になり、全文のない記録で「含まない」が通ることはない。
    #[serde(skip)]
    pub log_complete: bool,
    /// 判定に使うログの全文（標準出力と標準エラー）。試行の判定にだけ使い、記録には残さない。
    #[serde(skip)]
    pub log_text: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_build: Option<crate::prepared_web::BuildBasis>,
    /// 起動の前にコンテナへ置いたファイル。古い記録にはない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerPhase {
    pub engine: String,
    pub network: String,
    /// 外部通信の方針（blocked/allowed）。古い記録にはない。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub egress: String,
    /// 基盤が試験用ネットワークを読み出して、外部へ出る経路がないと確認できたか。
    #[serde(default)]
    pub egress_confirmed: bool,
    /// 中継が記録した、対象への接続の集計。古い記録にはない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traffic: Vec<TrafficSummary>,
    /// 通信の記録で観測できたことと、できなかったこと。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub traffic_note: String,

    pub volumes: Vec<String>,
    /// イメージの宣言でできた、設定に書いていないボリューム。試行の後に消す。古い記録にはない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_volumes: Vec<String>,
    pub services: Vec<ServicePhase>,
    pub leftovers: Vec<String>,
    pub database_query: Option<String>,
    pub http: Option<HttpResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<crate::browser::BrowserReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub database_reads: Vec<crate::browser::DatabaseRead>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub database_reads_before: Vec<crate::browser::DatabaseRead>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_reads: Vec<crate::http_probe::HttpRead>,
    pub detail: String,
    /// 追跡する値を、操作の前後にDBの文字列型の列から探した結果。記録のみで、判定には使わない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_scan_before: Option<crate::tracking::DatabaseScan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_scan: Option<crate::tracking::DatabaseScan>,
    /// 追跡した値ごとの、DB・HTTP・画面・ログでの出現。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_trace: Option<crate::tracking::ValueTrace>,
    /// 対象がDBへ送った文の記録。文を記録する中継を置いた試行だけにある。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statements: Vec<crate::relay::StatementEvent>,
    /// 記録に残さない文（読み出しなど）を含めた、種類ごとの数。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub statement_totals: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub statements_note: String,
}

/// 基盤から対象への接続を表す接続元の名前。ブラウザ操作器・HTTP操作・準備確認など。
pub const HOST_CLIENT: &str = "host（基盤・ブラウザ操作器）";

/// 接続元から接続先の対象への接続を集計したもの。通信の中身は含まない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrafficSummary {
    pub from: String,
    pub to: String,
    /// 開始した接続の数（`unfinished`を含む）。
    pub connections: u32,
    /// 閉じた接続のうち、中継の状態が200以外だったもの。接続拒否だけでなく、クライアントの切断や中継停止を含む。アプリの処理の成否ではない。
    pub failed: u32,
    /// 開始したが、記録を取る時点までに閉じなかった接続。接続先が止まって待たされた接続を含む。
    #[serde(default)]
    pub unfinished: u32,
    pub bytes_from_client: u64,
    pub bytes_to_client: u64,
    pub first_at_ms: u64,
    pub last_at_ms: u64,
}

struct Addresses {
    /// 内部ネットワークのIPアドレス → 対象の名前
    clients: BTreeMap<String, String>,
    /// 中継のコンテナ名 → 外部向けネットワークのIPアドレス
    edges: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct TapLine {
    id: u64,
    t: f64,
    client: String,
    local: String,
    status: String,
    #[serde(rename = "in")]
    bytes_in: u64,
    out: u64,
}

fn record_connection(
    rows: &mut BTreeMap<(String, String), TrafficSummary>,
    from: &str,
    to: &str,
    entry: &TapLine,
) {
    let at = (entry.t * 1000.0) as u64;
    let row = rows
        .entry((from.to_owned(), to.to_owned()))
        .or_insert_with(|| TrafficSummary {
            from: from.to_owned(),
            to: to.to_owned(),
            connections: 0,
            failed: 0,
            unfinished: 0,
            bytes_from_client: 0,
            bytes_to_client: 0,
            first_at_ms: at,
            last_at_ms: at,
        });
    row.connections += 1;
    if entry.status != "200" {
        row.failed += 1;
    }
    row.bytes_from_client += entry.bytes_in;
    row.bytes_to_client += entry.out;
    row.first_at_ms = if row.first_at_ms == 0 {
        at
    } else {
        row.first_at_ms.min(at)
    };
    row.last_at_ms = row.last_at_ms.max(at);
}

fn record_unfinished(rows: &mut BTreeMap<(String, String), TrafficSummary>, from: &str, to: &str) {
    let row = rows
        .entry((from.to_owned(), to.to_owned()))
        .or_insert_with(|| TrafficSummary {
            from: from.to_owned(),
            to: to.to_owned(),
            connections: 0,
            failed: 0,
            unfinished: 0,
            bytes_from_client: 0,
            bytes_to_client: 0,
            first_at_ms: 0,
            last_at_ms: 0,
        });
    row.connections += 1;
    row.unfinished += 1;
}

/// 中継のエラーログ（infoレベル）にある「接続を受けた」行から、接続の番号と接続元のIPアドレスを読む。
fn started_connections(errors: &str) -> Vec<(u64, String)> {
    errors
        .lines()
        .filter_map(|line| {
            let rest = line.split_once(": *")?.1;
            let (id, rest) = rest.split_once(" client ")?;
            let (address, _) = rest.split_once(" connected to ")?;
            let ip = address.rsplit_once(':')?.0;
            Some((id.parse().ok()?, ip.to_owned()))
        })
        .collect()
}

/// コンテナが指定のネットワークで持つIPv4アドレス。
fn container_ip(container: &str, network: &str) -> Option<String> {
    let json = docker_text(&[
        "inspect",
        "--format",
        "{{json .NetworkSettings.Networks}}",
        container,
    ])
    .ok()?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let ip = value.get(network)?.get("IPAddress")?.as_str()?;
    (!ip.is_empty()).then(|| ip.to_owned())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResult {
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub tls_insecure: bool,
    pub body_excerpt: String,
    pub detail: String,
}

pub struct Session {
    trial: String,
    network: String,
    edge: String,
    egress: Egress,
    egress_confirmed: bool,
    volumes: Vec<String>,
    /// イメージの宣言（VOLUME）でコンテナの作成時にできた、名前を指定していないボリューム。
    image_volumes: Vec<String>,
    services: Vec<Running>,
    engine: String,
    cleaned: bool,
    /// 文を記録する中継の指定と、中継へ渡す追跡する値。
    relay: Option<(RelaySetting, Vec<crate::relay::Tracked>)>,
    /// 固定したコミットの写し。コンテナへ置くファイルは、ここから読む。
    source: Option<PathBuf>,
}

struct Running {
    spec: Service,
    container: String,
    port: u16,
    digest: String,
    started: bool,
    ready: bool,
    ready_after_ms: Option<u64>,
    detail: String,
    web_build: Option<crate::prepared_web::BuildBasis>,
    tap: Option<String>,
    files: Vec<FileRecord>,
}

pub fn image_digest(image: &str) -> Option<&str> {
    let rest = image.rsplit_once("@sha256:")?.1;
    (rest.len() == 64
        && rest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    .then_some(rest)
}

/// リポジトリの最上位からの相対パス。上の階層と絶対パスは受け付けない。
fn repository_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && !path.starts_with('/')
        && !path.contains(['\0', '\\'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// コンテナの中の絶対パス。ファイルを指すので、`/` で終わらない。
fn container_path(path: &str) -> bool {
    path.len() <= 256
        && !path.contains('\0')
        && path.strip_prefix('/').is_some_and(|rest| {
            !rest.is_empty() && rest.split('/').all(|part| !matches!(part, "" | "." | ".."))
        })
}

/// DNSのホスト名。ラベルは英小文字・数字・ハイフンで、ハイフンで始めも終わりもしない。
fn host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// コンテナを起動する順番。役割の順（DB、その他、アプリ）と一覧の順を基本にし、
/// `start_after` に挙げたコンテナより後になるように遅らせる。順番を決められない指定は拒否する。
fn start_order(services: &[Service]) -> io::Result<Vec<&Service>> {
    let mut waiting: Vec<&Service> = services.iter().collect();
    waiting.sort_by_key(|s| match s.role {
        ServiceRole::Database => 0,
        ServiceRole::Other => 1,
        ServiceRole::App => 2,
    });
    let mut ordered: Vec<&Service> = Vec::with_capacity(waiting.len());
    while !waiting.is_empty() {
        let Some(next) = waiting.iter().position(|s| {
            s.start_after
                .iter()
                .all(|name| ordered.iter().any(|started| &started.name == name))
        }) else {
            return Err(invalid(format!(
                "コンテナの起動の順番を決められません。start_after が互いを待っています: {}",
                waiting
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            )));
        };
        ordered.push(waiting.remove(next));
    }
    Ok(ordered)
}

fn token(value: &str, what: &str, max: usize) -> io::Result<()> {
    if value.is_empty()
        || value.len() > max
        || !value.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(invalid(format!(
            "{what}は、英小文字で始まり英小文字・数字・ハイフンだけの{max}文字以内です"
        )));
    }
    Ok(())
}

pub fn validate_spec(spec: &ContainerSpec) -> io::Result<()> {
    if spec.services.is_empty() || spec.services.len() > MAX_SERVICES {
        return Err(invalid(format!(
            "コンテナは1〜{MAX_SERVICES}件で指定してください"
        )));
    }
    let mut names = std::collections::BTreeSet::new();
    let mut databases = 0;
    for service in &spec.services {
        if let Some(build) = &service.web_build {
            crate::prepared_web::validate(build)?;
        }
        token(&service.name, "コンテナの名前", 32)?;
        if !names.insert(service.name.as_str()) {
            return Err(invalid("コンテナの名前が重複しています"));
        }
        if image_digest(&service.image).is_none()
            || service.image.len() > 256
            || service.image.contains('\0')
            || service.image.chars().any(char::is_whitespace)
        {
            return Err(invalid(format!(
                "イメージ {} はタグだけではなく、@sha256: と64桁のダイジェストを付けて指定してください",
                service.image
            )));
        }
        if service.ready == ReadyCheck::Completed {
            if service.expose != 0 || service.role == ServiceRole::Database {
                return Err(invalid(format!(
                    "終了を待つコンテナ {} には、待ち受けポートとデータベースの役割を指定できません",
                    service.name
                )));
            }
        } else if service.expose == 0 {
            return Err(invalid("コンテナ内の待ち受けポートを指定してください"));
        }
        if service.files.len() > MAX_PLACED_FILES {
            return Err(invalid(format!(
                "コンテナへ置くファイルは{MAX_PLACED_FILES}件以内です"
            )));
        }
        let mut places = std::collections::BTreeSet::new();
        for file in &service.files {
            if !repository_path(&file.from)
                || !container_path(&file.to)
                || tar_names(file.to.trim_start_matches('/')).is_none()
                || !places.insert(file.to.as_str())
            {
                return Err(invalid(format!(
                    "コンテナ {} へ置くファイルは、リポジトリの中の相対パス（from）と、コンテナの中の重複しない絶対パス（to）で指定してください",
                    service.name
                )));
            }
        }
        if service.aliases.len() > MAX_ALIASES || !service.aliases.iter().all(|a| host_name(a)) {
            return Err(invalid(format!(
                "コンテナ {} の別名は、{MAX_ALIASES}件以内のホスト名（英小文字・数字・ハイフンをドットでつないだ形）で指定してください",
                service.name
            )));
        }
        if !(1..=MAX_READY_MS).contains(&service.ready_timeout_ms) {
            return Err(invalid(format!(
                "準備の待機上限は1〜{MAX_READY_MS}ミリ秒です"
            )));
        }
        if service.env.len() > 32
            || service.volumes.len() > 8
            || service.command.len() > 16
            || service
                .command
                .iter()
                .any(|a| a.len() > 1024 || a.contains('\0'))
        {
            return Err(invalid("コンテナの環境変数・ボリューム・引数が多すぎます"));
        }
        for (key, value) in &service.env {
            if key.is_empty()
                || key.len() > 64
                || !key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                || value.len() > 4096
                || value.contains('\0')
            {
                return Err(invalid("環境変数の名前と値を確認してください"));
            }
        }
        let mut waits = std::collections::BTreeSet::new();
        for name in &service.start_after {
            if name == &service.name
                || !waits.insert(name.as_str())
                || !spec.services.iter().any(|s| &s.name == name)
            {
                return Err(invalid(format!(
                    "コンテナ {} の start_after は、ほかのコンテナの名前を重複なく指定してください",
                    service.name
                )));
            }
        }
        for volume in &service.volumes {
            token(&volume.name, "ボリュームの名前", 32)?;
            if volume.path.is_empty()
                || !volume.path.starts_with('/')
                || volume.path.contains('\0')
                || volume.path.len() > 256
            {
                return Err(invalid("ボリュームのコンテナ内パスは / から始めてください"));
            }
        }
        match &service.ready {
            ReadyCheck::Http { path, .. } => http_path(path)?,
            ReadyCheck::Exec { args } => {
                if args.is_empty()
                    || args.len() > 16
                    || args
                        .iter()
                        .any(|a| a.is_empty() || a.len() > 256 || a.contains('\0'))
                {
                    return Err(invalid("準備確認のコマンドを確認してください"));
                }
            }
            ReadyCheck::Tcp | ReadyCheck::Completed => {}
        }
        if service.role == ServiceRole::Database {
            databases += 1;
        } else if !service.engine.is_default() {
            return Err(invalid(
                "DBの種類は、データベースの役割のコンテナにだけ指定してください",
            ));
        }
    }
    if spec.egress == Egress::Blocked
        && spec
            .services
            .iter()
            .any(|s| names.contains(format!("{}-real", s.name).as_str()))
    {
        return Err(invalid("コンテナの名前が中継用の接続先名と重複しています"));
    }
    // 別名は、ほかのコンテナの名前・別名・中継用の接続先名と重ならないこと。重なると、どちらに届くか決まらない。
    let mut taken: std::collections::BTreeSet<String> = spec
        .services
        .iter()
        .flat_map(|s| [s.name.clone(), format!("{}-real", s.name)])
        .collect();
    for service in &spec.services {
        for alias in &service.aliases {
            if !taken.insert(alias.clone()) {
                return Err(invalid(format!(
                    "コンテナ {} の別名 {alias} が、ほかの名前と重複しています",
                    service.name
                )));
            }
        }
    }
    if databases > 1 {
        return Err(invalid("データベースの役割のコンテナは1件にしてください"));
    }
    if let Some(relay) = &spec.relay {
        if spec.egress != Egress::Blocked
            || !spec
                .services
                .iter()
                .any(|s| s.name == relay.service && s.ready != ReadyCheck::Completed)
        {
            return Err(invalid(
                "文を記録する中継は、外部通信を遮断した試験の、実在する対象にだけ置けます",
            ));
        }
        if let Some(cut) = &relay.cut
            && (relay.protocol != crate::relay::Protocol::Mysql
                || cut.occurrence == 0
                || cut.shape.len() != 16
                || !cut.shape.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(invalid(
                "接続を切る時点は、文の形のハッシュ（16桁）と1以上の回数で指定してください",
            ));
        }
    }
    start_order(&spec.services)?;
    Ok(())
}

pub fn validate_http(op: &HttpOp, spec: &ContainerSpec) -> io::Result<()> {
    if !["GET", "POST", "PUT"].contains(&op.method.as_str()) {
        return Err(invalid("HTTPの操作は GET・POST・PUT のいずれかです"));
    }
    http_path(&op.path)?;
    if spec.services.iter().all(|s| s.name != op.service) {
        return Err(invalid(format!(
            "HTTPの操作先 {} は、起動するコンテナの名前と一致しません",
            op.service
        )));
    }
    if let Some(file) = &op.body_file
        && (file.is_empty()
            || file.len() > 1024
            || file.contains('\0')
            || Path::new(file).is_absolute())
    {
        return Err(invalid("HTTPの本文ファイルは固定版の中の相対パスです"));
    }
    if op
        .content_type
        .as_ref()
        .is_some_and(|t| t.len() > 128 || t.contains('\0'))
    {
        return Err(invalid("Content-Type を確認してください"));
    }
    Ok(())
}

fn http_path(path: &str) -> io::Result<()> {
    if !path.starts_with('/')
        || path.len() > 1024
        || path.contains('\0')
        || path
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace())
    {
        return Err(invalid("HTTPのパスは / から始まる空白なしの文字列です"));
    }
    Ok(())
}

/// Dockerのエンジンに接続できるか。失敗してもほかの実行方式へは切り替えない。
pub fn engine() -> Result<String, String> {
    let output = docker(&["version", "--format", "{{.Server.Version}}"]).map_err(|e| {
        format!("Dockerのコマンドを起動できません: {e}。コンテナの試行はほかの方式へ切り替えません")
    })?;
    if !output.status.success() {
        return Err(format!(
            "Dockerのエンジンに接続できません: {}。コンテナの試行はほかの方式へ切り替えません",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if version.is_empty() {
        return Err(
            "Dockerのエンジンの版を取得できません。コンテナの試行はほかの方式へ切り替えません"
                .into(),
        );
    }
    Ok(version)
}

fn docker(args: &[&str]) -> io::Result<Output> {
    let mut command = Command::new("docker");
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null());
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        command.env("DOCKER_HOST", host);
    }
    // コンテナ・ネットワーク・ボリュームを作る呼び出しは、終了の合図の回収と重ならないように行う。
    // 回収が始まった後は、作らない。
    let creates = matches!(args, ["create" | "run", ..])
        || matches!(args, ["network" | "volume", "create", ..]);
    if creates {
        crate::shutdown::creating(|| crate::shutdown::output(&mut command))
    } else {
        crate::shutdown::output(&mut command)
    }
}

/// 期限、停止の合図、出力の上限を付けて `docker` を実行した結果。
struct Bounded {
    ended: crate::tracking::Ended,
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// 標準エラーを受け取る上限。
const MAX_BOUNDED_STDERR: usize = 64 * 1024;

/// `docker` を実行し、期限に達するか、停止を求められるか、出力が上限を超えたら、待つのをやめて止める。
/// 出力は上限までしかメモリに持たない。止めるのはホストの側の `docker` のプロセスだけなので、
/// コンテナの中で始めた処理を止めるのは呼び出し側の役目である。
fn docker_bounded(
    args: &[&str],
    within: Duration,
    stop: Option<&std::sync::atomic::AtomicBool>,
    max_stdout: usize,
) -> io::Result<Bounded> {
    use crate::tracking::Ended;
    use std::{
        io::Read,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let deadline = Instant::now() + within;
    let mut command = Command::new("docker");
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        command.env("DOCKER_HOST", host);
    }
    let mut child = command.spawn()?;
    // 終了の合図を受けたときに止められるように、実行中のPIDを控える。
    let pid = child.id();
    crate::shutdown::hold_command(pid);
    let overflow = Arc::new(AtomicBool::new(false));
    // 上限までを持ち、超えた分は捨てる。上限を超えたことは親へ知らせる。
    let reader = |mut pipe: Box<dyn Read + Send>, limit: usize, flag: Option<Arc<AtomicBool>>| {
        thread::spawn(move || {
            let mut kept = Vec::new();
            let mut buffer = [0u8; 16 * 1024];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        let room = limit.saturating_sub(kept.len());
                        kept.extend_from_slice(&buffer[..size.min(room)]);
                        if size > room
                            && let Some(flag) = &flag
                        {
                            flag.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            kept
        })
    };
    let out = reader(
        Box::new(child.stdout.take().expect("piped stdout")),
        max_stdout,
        Some(overflow.clone()),
    );
    let err = reader(
        Box::new(child.stderr.take().expect("piped stderr")),
        MAX_BOUNDED_STDERR,
        None,
    );
    let (ended, status) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Ended::Exited, Some(status)),
            Ok(None) => {}
            Err(error) => {
                crate::shutdown::release_command(pid);
                return Err(error);
            }
        }
        let ended = if overflow.load(Ordering::SeqCst) {
            Some(Ended::OutputLimit)
        } else if stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
            Some(Ended::Stopped)
        } else if Instant::now() >= deadline {
            Some(Ended::Deadline)
        } else {
            None
        };
        if let Some(ended) = ended {
            let _ = child.kill();
            let _ = child.wait();
            break (ended, None);
        }
        thread::sleep(Duration::from_millis(10));
    };
    crate::shutdown::release_command(pid);
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    // 終了と同時に上限を超えた場合も、出力が欠けているので、上限を超えたものとして扱う。
    let ended = if ended == Ended::Exited && overflow.load(Ordering::SeqCst) {
        Ended::OutputLimit
    } else {
        ended
    };
    Ok(Bounded {
        ended,
        success: ended == Ended::Exited && status.is_some_and(|s| s.success()),
        stdout,
        stderr,
    })
}

/// コンテナの中でDBのクライアントを起動する `docker exec` の引数。
fn exec_args<'a>(container: &'a str, client: &'a crate::database::Client) -> Vec<&'a str> {
    let mut args = vec!["exec"];
    for value in &client.env {
        args.extend(["--env", value.as_str()]);
    }
    args.push(container);
    args.extend(client.argv.iter().map(String::as_str));
    args
}

/// 標準入力へ内容を渡して `docker` を実行する。
fn docker_with_input(args: &[&str], input: &[u8]) -> io::Result<Output> {
    let mut command = Command::new("docker");
    command
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        command.env("DOCKER_HOST", host);
    }
    let mut child = command.spawn()?;
    // 終了の合図を受けたときに止められるように、実行中のPIDを控える。
    let pid = child.id();
    crate::shutdown::hold_command(pid);
    let written = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(input),
        None => Ok(()),
    };
    let output = child.wait_with_output();
    crate::shutdown::release_command(pid);
    written?;
    output
}

/// 試験用ネットワークの中で、対象に届く名前。コンテナの名前と別名。
fn public_names(service: &Service) -> Vec<String> {
    std::iter::once(service.name.clone())
        .chain(service.aliases.iter().cloned())
        .collect()
}

/// tarの見出しに入る形へ、パスを名前（100バイトまで）と前半（155バイトまで）に分ける。
fn tar_names(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    path.match_indices('/')
        .map(|(at, _)| (&path[..at], &path[at + 1..]))
        .find(|(prefix, name)| prefix.len() <= 155 && !name.is_empty() && name.len() <= 100)
}

/// ファイルだけを入れたtar（ustar）を作る。所有者はroot、時刻は0にして、同じ入力から同じ内容を作る。
fn tar_archive(entries: &[(String, Vec<u8>, bool)]) -> io::Result<Vec<u8>> {
    fn octal(field: &mut [u8], value: u64) {
        let text = format!("{value:0width$o}", width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
    }
    let mut out = vec![];
    for (path, bytes, executable) in entries {
        let (prefix, name) =
            tar_names(path).ok_or_else(|| invalid("コンテナの中の置き場所が長すぎます"))?;
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        octal(
            &mut header[100..108],
            if *executable { 0o755 } else { 0o644 },
        );
        octal(&mut header[108..116], 0);
        octal(&mut header[116..124], 0);
        octal(&mut header[124..136], bytes.len() as u64);
        octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        let sum: u64 = header.iter().map(|b| u64::from(*b)).sum();
        let text = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(text.as_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(bytes);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    Ok(out)
}

fn docker_text(args: &[&str]) -> io::Result<String> {
    let output = docker(args)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(io::Error::other(format!(
            "docker {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

pub fn placeholder(name: &str) -> String {
    format!("{PORT_PREFIX}{name}}}")
}

pub fn substitute_ports(value: &str, ports: &BTreeMap<String, u16>) -> String {
    let mut out = value.to_owned();
    for (name, port) in ports {
        out = out.replace(&placeholder(name), &port.to_string());
    }
    out
}

impl Session {
    pub fn start(trial: &str, spec: &ContainerSpec) -> io::Result<Self> {
        Self::start_tracking(trial, spec, vec![], None)
    }

    /// 追跡する値を、文を記録する中継へ渡して起動する。中継を置かない設定では値を使わない。
    /// `source` は固定したコミットの写しで、コンテナへ置くファイルの出所になる。
    pub fn start_tracking(
        trial: &str,
        spec: &ContainerSpec,
        values: Vec<crate::relay::Tracked>,
        source: Option<&Path>,
    ) -> io::Result<Self> {
        validate_spec(spec)?;
        if source.is_none() && spec.services.iter().any(|s| !s.files.is_empty()) {
            return Err(invalid(
                "コンテナへ置くファイルの出所（固定したコミットの写し）がありません",
            ));
        }
        let engine = engine().map_err(io::Error::other)?;
        let trial = format!(
            "{}-{}",
            sanitize_trial(trial),
            SESSION.fetch_add(1, Ordering::Relaxed)
        );
        let network = format!("eel-{trial}");
        // 終了の合図を受けたときにも、この試行の資源を回収できるように控える。
        crate::shutdown::hold_trial(&trial);
        let mut session = Self {
            trial: trial.clone(),
            network: network.clone(),
            edge: String::new(),
            egress: spec.egress,
            egress_confirmed: false,
            volumes: vec![],
            image_volumes: vec![],
            services: vec![],
            engine,
            cleaned: false,
            relay: spec.relay.clone().map(|setting| (setting, values)),
            source: source.map(Path::to_path_buf),
        };
        let mut create = vec!["network", "create", "--label", "eel.managed=1"];
        let label = format!("eel.trial={trial}");
        create.extend(["--label", &label]);
        if spec.egress == Egress::Blocked {
            create.push("--internal");
        }
        create.push(&network);
        docker_text(&create)?;
        if spec.egress == Egress::Blocked {
            let internal =
                docker_text(&["network", "inspect", "--format", "{{.Internal}}", &network]);
            session.egress_confirmed = internal.is_ok_and(|v| v == "true");
            if !session.egress_confirmed {
                let _ = session.cleanup();
                return Err(io::Error::other(
                    "試験用ネットワークが外部通信を遮断する設定になっていません。実行を止めました",
                ));
            }
            let edge = format!("{network}-edge");
            docker_text(&[
                "network",
                "create",
                "--label",
                "eel.managed=1",
                "--label",
                &label,
                &edge,
            ])?;
            session.edge = edge;
        }
        let mut volume_names = std::collections::BTreeSet::new();
        for service in &spec.services {
            for volume in &service.volumes {
                if volume_names.insert(volume.name.as_str()) {
                    let full = format!("eel-{trial}-{}", volume.name);
                    docker_text(&[
                        "volume",
                        "create",
                        "--label",
                        "eel.managed=1",
                        "--label",
                        &format!("eel.trial={trial}"),
                        &full,
                    ])?;
                    session.volumes.push(full);
                }
            }
        }
        for service in start_order(&spec.services)? {
            match session.run(service) {
                Ok(()) => {
                    if session.services.last().is_some_and(|s| !s.ready) {
                        break;
                    }
                }
                Err(error) => {
                    let _ = session.cleanup();
                    return Err(error);
                }
            }
        }
        Ok(session)
    }

    fn run(&mut self, service: &Service) -> io::Result<()> {
        let assets = service
            .web_build
            .as_ref()
            .map(|build| {
                let root = std::env::var_os("EEL_PREPARED_ENVIRONMENTS").ok_or_else(|| {
                    invalid("ビルド済みファイルを使うにはEEL_PREPARED_ENVIRONMENTSが必要です")
                })?;
                crate::prepared_web::snapshot(Path::new(&root), build)
            })
            .transpose()?;
        // 終了の合図を受けて回収を始めた後は、イメージの取得を始めない。
        // コンテナを作る呼び出しは、`docker` の中でも確かめている。
        if crate::shutdown::stopping() {
            return Err(io::Error::other(
                "終了の合図を受けたので、コンテナを起動しない",
            ));
        }
        let digest = pull(&service.image)?;
        // 終了を待つコンテナは待ち受けを持たない。ポートを割り当てず、中継も置かない。
        let listens = service.ready != ReadyCheck::Completed;
        let port = if listens {
            crate::server::free_port()?
        } else {
            0
        };
        let container = format!("eel-{}-{}", self.trial, service.name);
        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            container.clone(),
            "--label".into(),
            "eel.managed=1".into(),
            "--label".into(),
            format!("eel.trial={}", self.trial),
            "--label".into(),
            format!("eel.role={}", role_label(service.role)),
            "--network".into(),
            self.network.clone(),
            "--restart".into(),
            "no".into(),
        ];
        for alias in self.container_aliases(service) {
            args.push("--network-alias".into());
            args.push(alias);
        }
        if self.egress == Egress::Allowed && listens {
            args.push("--publish".into());
            args.push(format!("127.0.0.1:{port}:{}", service.expose));
        }
        harden(&mut args);
        for (key, value) in &service.env {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
        for volume in &service.volumes {
            args.push("--mount".into());
            args.push(format!(
                "type=volume,source=eel-{}-{},target={}",
                self.trial, volume.name, volume.path
            ));
        }
        args.push(service.image.clone());
        args.extend(service.command.iter().cloned());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        // ファイルを置く場合は、作成・複写・起動の順に行う。起動の前に置かないと、対象が読めない。
        let files = if assets.is_some() || !service.files.is_empty() {
            let mut create = vec!["create"];
            create.extend(arg_refs.iter().copied().skip(2));
            docker_text(&create)?;
            let placed = match &assets {
                Some(assets) => docker_text(&[
                    "cp",
                    &format!("{}/.", assets.directory.display()),
                    &format!("{container}:/usr/share/nginx/html"),
                ])
                .map(|_| ()),
                None => Ok(()),
            }
            .and_then(|()| self.put_files(&container, &service.files))
            .and_then(|records| docker_text(&["start", &container]).map(|_| records));
            match placed {
                Ok(records) => records,
                Err(error) => {
                    let _ = remove_container(&container);
                    return Err(error);
                }
            }
        } else {
            docker_text(&arg_refs)?;
            vec![]
        };
        self.note_image_volumes(&container);
        let tap = if self.egress == Egress::Blocked && listens {
            let started = match &self.relay {
                Some((setting, values)) if setting.service == service.name => {
                    self.start_relay(service, port, setting, values)
                }
                _ => self.start_tap(service, port),
            };
            match started {
                Ok(name) => Some(name),
                Err(error) => {
                    let _ = remove_container(&container);
                    return Err(error);
                }
            }
        } else {
            None
        };
        let mut running = Running {
            spec: service.clone(),
            container,
            port,
            digest,
            started: true,
            ready: false,
            ready_after_ms: None,
            detail: String::new(),
            web_build: assets.as_ref().map(|s| s.basis.clone()),
            tap,
            files,
        };
        match wait_ready(&running) {
            Ok(ms) => {
                running.ready = true;
                running.ready_after_ms = Some(ms);
                running.detail = if listens {
                    format!("127.0.0.1:{port} で準備できた（{ms}ms）")
                } else {
                    format!("終了コード0で終わった（{ms}ms）")
                };
            }
            Err(error) => {
                running.detail = error.to_string();
                self.services.push(running);
                return Ok(());
            }
        }
        self.services.push(running);
        Ok(())
    }

    /// コンテナに載っているボリュームのうち、設定に書いていないものを控える。
    /// 読み出せなかった場合は控えられないが、コンテナを消すときに `-v` で消す。
    fn note_image_volumes(&mut self, container: &str) {
        let Ok(text) = docker_text(&[
            "inspect",
            "--format",
            "{{range .Mounts}}{{if eq .Type \"volume\"}}{{println .Name}}{{end}}{{end}}",
            container,
        ]) else {
            return;
        };
        for name in text.split_whitespace() {
            if !self.volumes.iter().any(|v| v == name)
                && !self.image_volumes.iter().any(|v| v == name)
            {
                self.image_volumes.push(name.to_owned());
            }
        }
    }

    /// 対象の本体に付ける名前。遮断時は、元の名前と別名を中継が受け持つので、本体には付けない。
    fn container_aliases(&self, service: &Service) -> Vec<String> {
        match self.egress {
            Egress::Blocked => vec![self.alias(&service.name)],
            Egress::Allowed => public_names(service),
        }
    }

    /// 固定したコミットの写しから、指定したファイルをコンテナの中へ置く。置いたファイルの記録を返す。
    /// ファイルだけを入れたtarを `docker cp` へ渡す。親のディレクトリがなければ作られ、
    /// 既にあるディレクトリの権限と所有者は変わらない。
    fn put_files(&self, container: &str, files: &[FileMount]) -> io::Result<Vec<FileRecord>> {
        if files.is_empty() {
            return Ok(vec![]);
        }
        let source = self
            .source
            .as_ref()
            .ok_or_else(|| invalid("コンテナへ置くファイルの出所がありません"))?;
        let mut records = vec![];
        let mut entries = vec![];
        for file in files {
            let path = source.join(&file.from);
            let metadata = fs::symlink_metadata(&path).map_err(|_| {
                invalid(format!(
                    "コンテナへ置くファイル {} が、固定したコミットの写しにありません",
                    file.from
                ))
            })?;
            if !metadata.is_file() || metadata.len() > MAX_PLACED_FILE {
                return Err(invalid(format!(
                    "コンテナへ置くファイル {} は、{MAX_PLACED_FILE}バイト以内の通常のファイルにしてください",
                    file.from
                )));
            }
            let bytes = fs::read(&path)?;
            use sha2::{Digest, Sha256};
            records.push(FileRecord {
                from: file.from.clone(),
                to: file.to.clone(),
                bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            });
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            entries.push((
                file.to.trim_start_matches('/').to_owned(),
                bytes,
                executable,
            ));
        }
        let archive = tar_archive(&entries)?;
        let output = docker_with_input(&["cp", "-", &format!("{container}:/")], &archive)?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "コンテナ {container} へファイルを置けません: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(records)
    }

    /// 対象の本体のネットワーク別名。遮断時は中継が元の名前を受け持つ。
    fn alias(&self, name: &str) -> String {
        match self.egress {
            Egress::Blocked => format!("{name}-real"),
            Egress::Allowed => name.to_owned(),
        }
    }

    /// 対象の前に置く中継。ホストの127.0.0.1と対象、対象どうしの接続を通し、接続ごとの結果を標準出力へ残す。
    fn start_tap(&self, service: &Service, port: u16) -> io::Result<String> {
        pull(GATEWAY_IMAGE)?;
        let name = format!("eel-{}-{}-tap", self.trial, service.name);
        let config = format!(
            "user root;pid /tmp/tap.pid;error_log /dev/stderr info;events{{}}stream{{resolver 127.0.0.11 valid=1s;resolver_timeout 2s;log_format eel escape=json \"{{\\\"id\\\":$connection,\\\"t\\\":$msec,\\\"client\\\":\\\"$remote_addr\\\",\\\"local\\\":\\\"$server_addr\\\",\\\"status\\\":\\\"$status\\\",\\\"in\\\":$bytes_received,\\\"out\\\":$bytes_sent}}\";access_log /dev/stdout eel;server{{listen {expose};set $upstream {real}:{expose};proxy_half_close on;proxy_pass $upstream;}}}}",
            expose = service.expose,
            real = self.alias(&service.name),
        );
        let script = format!(
            "printf '%s' '{config}' > /tmp/tap.conf && exec nginx -c /tmp/tap.conf -g 'daemon off;'"
        );
        let mut args: Vec<String> = vec![
            "create".into(),
            "--name".into(),
            name.clone(),
            "--label".into(),
            "eel.managed=1".into(),
            "--label".into(),
            format!("eel.trial={}", self.trial),
            "--label".into(),
            "eel.role=tap".into(),
            "--network".into(),
            self.edge.clone(),
            "--publish".into(),
            format!("127.0.0.1:{port}:{}", service.expose),
            "--restart".into(),
            "no".into(),
        ];
        harden(&mut args);
        args.extend([
            "--entrypoint".into(),
            "sh".into(),
            GATEWAY_IMAGE.into(),
            "-c".into(),
            script,
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        docker_text(&refs)?;
        let started = self
            .connect_as(service, &name)
            .and_then(|_| docker_text(&["start", &name]));
        if let Err(error) = started {
            let _ = remove_container(&name);
            return Err(error);
        }
        Ok(name)
    }

    /// 中継を試験用ネットワークへつなぎ、対象の名前と別名を受け持たせる。
    fn connect_as(&self, service: &Service, tap: &str) -> io::Result<String> {
        let names = public_names(service);
        let mut args = vec!["network", "connect"];
        for name in &names {
            args.push("--alias");
            args.push(name);
        }
        args.push(&self.network);
        args.push(tap);
        docker_text(&args)
    }

    /// プロトコルを解釈する中継を、既存の中継と同じ位置に置く。基盤の実行ファイルをコンテナへ入れて動かす。
    fn start_relay(
        &self,
        service: &Service,
        port: u16,
        setting: &RelaySetting,
        values: &[crate::relay::Tracked],
    ) -> io::Result<String> {
        pull(RELAY_IMAGE)?;
        let binary = match std::env::var_os("EEL_RELAY_BINARY") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe()?,
        };
        let name = format!("eel-{}-{}-tap", self.trial, service.name);
        let config = serde_json::to_string(&crate::relay::Config {
            listen: service.expose,
            upstream: format!("{}:{}", self.alias(&service.name), service.expose),
            protocol: setting.protocol,
            values: values.to_vec(),
            cut: setting.cut.clone(),
        })
        .map_err(io::Error::other)?;
        let mut args: Vec<String> = vec![
            "create".into(),
            "--name".into(),
            name.clone(),
            "--label".into(),
            "eel.managed=1".into(),
            "--label".into(),
            format!("eel.trial={}", self.trial),
            "--label".into(),
            "eel.role=tap".into(),
            "--network".into(),
            self.edge.clone(),
            "--publish".into(),
            format!("127.0.0.1:{port}:{}", service.expose),
            "--restart".into(),
            "no".into(),
            "--env".into(),
            format!("{}={config}", crate::relay::CONFIG_VARIABLE),
        ];
        harden(&mut args);
        args.extend([
            "--entrypoint".into(),
            "/relay".into(),
            RELAY_IMAGE.into(),
            "relay".into(),
        ]);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        docker_text(&refs)?;
        let started = docker_text(&["cp", &binary.to_string_lossy(), &format!("{name}:/relay")])
            .and_then(|_| self.connect_as(service, &name))
            .and_then(|_| docker_text(&["start", &name]));
        // 実行ファイルがコンテナの中で動かない場合（別のOS向け、glibcの世代の違い）は、すぐに終了する。
        let ready = started.and_then(|_| {
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                let logs = docker(&["logs", &name])?;
                if String::from_utf8_lossy(&logs.stdout).contains("\"e\":\"ready\"") {
                    return Ok(());
                }
                if !inspect_running(&name).unwrap_or(false) || Instant::now() >= until {
                    return Err(io::Error::other(format!(
                        "文を記録する中継を起動できません。基盤の実行ファイルが中継のコンテナの中で動くことを確認してください（EEL_RELAY_BINARY でLinux向けの実行ファイルを指定できます）: {}",
                        String::from_utf8_lossy(&logs.stderr)
                            .chars()
                            .take(300)
                            .collect::<String>()
                    )));
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        if let Err(error) = ready {
            let _ = remove_container(&name);
            return Err(error);
        }
        Ok(name)
    }

    /// 文を記録する中継の記録を読む。中継を置いていない試行では `None`。
    fn statements(
        &self,
    ) -> Option<(
        Vec<crate::relay::StatementEvent>,
        BTreeMap<String, u64>,
        String,
    )> {
        let (setting, _) = self.relay.as_ref()?;
        let tap = self
            .services
            .iter()
            .find(|s| s.spec.name == setting.service)?
            .tap
            .as_ref()?;
        let output = match docker(&["logs", tap]) {
            Ok(output) if output.status.success() => output,
            _ => {
                return Some((
                    vec![],
                    BTreeMap::new(),
                    "中継の記録を取得できない。文がなかったことは意味しない".into(),
                ));
            }
        };
        let read = crate::relay::read_log(&String::from_utf8_lossy(&output.stdout));
        let total: u64 = read.totals.values().sum();
        let mut note = format!(
            "準備と操作の間に、対象が {} へ送った文は{total}件。そのうち、書き込み・トランザクションの操作・追跡する値を含む文の{}件を記録した。SQLの本文は記録していない",
            setting.service,
            read.statements.len()
        );
        if total == 0 {
            note.push_str("。基盤が段階を知らせる前の文は数えていない");
        }
        if !read.opaque.is_empty() {
            note.push_str(&format!(
                "。暗号化などで読めなかった接続が{}件あり、その中の文は記録していない",
                read.opaque.len()
            ));
        }
        if read.truncated {
            note.push_str("。上限を超えた分は捨てた");
        }
        Some((read.statements, read.totals, note))
    }

    /// 文を記録する中継へ、試行の段階を知らせる。中継は、知らされた後の文だけを数える。
    /// 対象の起動時の初期化を、試験の操作と混ぜないためである。中継を置いていない試行では何もしない。
    pub fn mark_relay_phase(&self, phase: &str) -> io::Result<()> {
        let Some((setting, _)) = &self.relay else {
            return Ok(());
        };
        let Some(tap) = self
            .services
            .iter()
            .find(|s| s.spec.name == setting.service)
            .and_then(|s| s.tap.as_ref())
        else {
            return Ok(());
        };
        if !["preparation", "action"].contains(&phase) {
            return Err(invalid("中継へ知らせる段階は preparation か action です"));
        }
        docker_text(&[
            "exec",
            tap,
            "sh",
            "-c",
            &format!("printf %s {phase} > {}", crate::relay::PHASE_FILE),
        ])?;
        // 中継は段階を短い間隔で読み直す。読み直すまで待ってから、対象を操作する。
        thread::sleep(Duration::from_millis(60));
        Ok(())
    }

    /// 中継が記録した接続を、接続元と接続先の対象ごとに集計する。
    fn traffic(&self, addresses: &Addresses) -> (Vec<TrafficSummary>, String) {
        if self.egress == Egress::Allowed {
            return (
                vec![],
                "外部通信を許可した試行では、対象どうしの通信を記録していない".into(),
            );
        }
        let clients = &addresses.clients;
        let mut rows: BTreeMap<(String, String), TrafficSummary> = BTreeMap::new();
        let mut unparsed = 0usize;
        let mut truncated = false;
        let mut unavailable = vec![];
        for service in &self.services {
            let Some(tap) = &service.tap else { continue };
            let edge_ip = addresses.edges.get(tap);
            let output = match docker(&["logs", tap]) {
                Ok(output) if output.status.success() => output,
                _ => {
                    unavailable.push(service.spec.name.as_str());
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&output.stdout);
            let errors = String::from_utf8_lossy(&output.stderr);
            if text.len() > MAX_TRAFFIC_LOG || errors.len() > MAX_TRAFFIC_LOG {
                truncated = true;
            }
            let text: String = text.chars().take(MAX_TRAFFIC_LOG).collect();
            let errors: String = errors.chars().take(MAX_TRAFFIC_LOG).collect();
            let mut finished = std::collections::BTreeSet::new();
            for line in text.lines().filter(|l| l.starts_with('{')) {
                match serde_json::from_str::<TapLine>(line) {
                    Ok(entry) => {
                        finished.insert(entry.id);
                        let from = match clients.get(&entry.client) {
                            Some(name) => name.clone(),
                            None if edge_ip.is_some_and(|ip| *ip == entry.local) => {
                                HOST_CLIENT.to_owned()
                            }
                            None => format!("不明（{}）", entry.client),
                        };
                        record_connection(&mut rows, &from, &service.spec.name, &entry);
                    }
                    Err(_) => unparsed += 1,
                }
            }
            for (id, client) in started_connections(&errors) {
                if !finished.contains(&id) {
                    let from = clients
                        .get(&client)
                        .cloned()
                        .unwrap_or_else(|| format!("不明（{client}）"));
                    record_unfinished(&mut rows, &from, &service.spec.name);
                }
            }
        }
        let mut summary: Vec<TrafficSummary> = rows.into_values().collect();
        let omitted_rows = summary.len().saturating_sub(MAX_TRAFFIC_ROWS);
        summary.truncate(MAX_TRAFFIC_ROWS);
        let mut note = String::from(
            "中継を通った接続（接続元・接続先の対象、中継の状態、バイト数）を記録した。失敗件数は中継の状態が200以外の接続で、クライアントの切断や中継停止も含み、アプリの処理の成否ではない。閉じなかった接続は件数だけで、成否とバイト数は分からない。中継を通らない通信と、通信の中身は記録していない",
        );
        if !unavailable.is_empty() {
            note.push_str(&format!(
                "。中継ログを取得できず未観測の対象: {}",
                unavailable.join("、")
            ));
        }
        if omitted_rows > 0 {
            note.push_str(&format!("。集計の上限により{omitted_rows}組を省略した"));
        }
        if unparsed > 0 {
            note.push_str(&format!("。読み取れない行が{unparsed}件あった"));
        }
        if truncated {
            note.push_str("。ログが上限を超えたため後半を省略した");
        }
        (summary, note)
    }

    /// 中継と対象を止める前に、IPアドレスと名前の対応を読む。止めた後は読めない。
    fn addresses(&self) -> Addresses {
        Addresses {
            clients: self
                .services
                .iter()
                .filter_map(|s| {
                    container_ip(&s.container, &self.network).map(|ip| (ip, s.spec.name.clone()))
                })
                .collect(),
            edges: self
                .services
                .iter()
                .filter_map(|s| {
                    let tap = s.tap.as_ref()?;
                    Some((tap.clone(), container_ip(tap, &self.edge)?))
                })
                .collect(),
        }
    }

    /// ホストへ公開したポート。終了を待つコンテナは待ち受けを持たないので、含めない。
    pub fn ports(&self) -> BTreeMap<String, u16> {
        self.services
            .iter()
            .filter(|s| s.port != 0)
            .map(|s| (s.spec.name.clone(), s.port))
            .collect()
    }

    pub fn port_of(&self, name: &str) -> io::Result<u16> {
        self.services
            .iter()
            .find(|s| s.spec.name == name && s.port != 0)
            .map(|s| s.port)
            .ok_or_else(|| invalid(format!("コンテナ {name} のポートがありません")))
    }

    pub fn all_ready(&self) -> bool {
        self.services.iter().all(|s| s.ready)
    }

    pub fn stop_database(&mut self) -> io::Result<bool> {
        let Some(index) = self
            .services
            .iter()
            .position(|s| s.spec.role == ServiceRole::Database)
        else {
            return Err(invalid("データベースの役割のコンテナがありません"));
        };
        docker_text(&["stop", "-t", "5", &self.services[index].container])?;
        let running = inspect_running(&self.services[index].container)?;
        self.services[index].detail = if running {
            "データベースのコンテナを止めたが、まだ動いている".into()
        } else {
            "データベースのコンテナを止めた".into()
        };
        Ok(!running)
    }

    pub fn disconnect_database(&mut self) -> io::Result<bool> {
        let Some(index) = self
            .services
            .iter()
            .position(|s| s.spec.role == ServiceRole::Database)
        else {
            return Err(invalid("データベースの役割のコンテナがありません"));
        };
        docker_text(&[
            "network",
            "disconnect",
            &self.network,
            &self.services[index].container,
        ])?;
        let connected = inspect_network(&self.services[index].container, &self.network)?;
        self.services[index].detail = if connected {
            "データベースを試験用ネットワークから外したが、まだ接続されている".into()
        } else {
            "データベースを試験用ネットワークから外した".into()
        };
        Ok(!connected)
    }

    pub fn remount_database_read_only(&mut self) -> io::Result<bool> {
        let index = self.recreate_database(true)?;
        let probe_path = format!(
            "{}/.eel-write-probe",
            self.services[index].spec.volumes[0].path
        );
        let writable = self.writable(index, &probe_path);
        self.services[index].detail = if writable {
            "保存先のボリュームを読み取り専用で載せ直したが、コンテナ内から書き込めた".into()
        } else {
            "保存先のボリュームを読み取り専用で載せ直した".into()
        };
        Ok(!writable)
    }

    fn writable(&self, index: usize, probe_path: &str) -> bool {
        docker(&["exec", &self.services[index].container, "touch", probe_path])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// データベースのコンテナを消して、同じボリュームで作り直す。`read_only`なら読み取り専用で載せる。
    fn recreate_database(&mut self, read_only: bool) -> io::Result<usize> {
        let Some(index) = self
            .services
            .iter()
            .position(|s| s.spec.role == ServiceRole::Database)
        else {
            return Err(invalid("データベースの役割のコンテナがありません"));
        };
        if self.services[index].spec.volumes.is_empty() {
            return Err(invalid("読み取り専用にするボリュームがありません"));
        }
        remove_container(&self.services[index].container)?;
        let spec = self.services[index].spec.clone();
        let port = self.services[index].port;
        let container = self.services[index].container.clone();
        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            container.clone(),
            "--label".into(),
            "eel.managed=1".into(),
            "--label".into(),
            format!("eel.trial={}", self.trial),
            "--network".into(),
            self.network.clone(),
            "--restart".into(),
            "no".into(),
        ];
        for alias in self.container_aliases(&spec) {
            args.push("--network-alias".into());
            args.push(alias);
        }
        if self.egress == Egress::Allowed {
            args.push("--publish".into());
            args.push(format!("127.0.0.1:{port}:{}", spec.expose));
        }
        harden(&mut args);
        for (key, value) in &spec.env {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
        for volume in &spec.volumes {
            args.push("--mount".into());
            args.push(format!(
                "type=volume,source=eel-{}-{},target={}{}",
                self.trial,
                volume.name,
                volume.path,
                if read_only { ",readonly" } else { "" }
            ));
        }
        args.push(spec.image.clone());
        args.extend(spec.command.iter().cloned());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        if spec.files.is_empty() {
            docker_text(&arg_refs)?;
        } else {
            // 作り直したコンテナにも、同じファイルを置いてから起動する。
            let mut create = vec!["create"];
            create.extend(arg_refs.iter().copied().skip(2));
            docker_text(&create)?;
            self.put_files(&container, &spec.files)?;
            docker_text(&["start", &container])?;
        }
        self.note_image_volumes(&container);
        Ok(index)
    }

    /// 故障を解除する。解除後の状態を読み出して確かめ、確かめられた場合だけ`true`を返す。
    pub fn restore_database(&mut self, fault: crate::trial::TrialFault) -> io::Result<bool> {
        use crate::trial::TrialFault;
        let Some(index) = self
            .services
            .iter()
            .position(|s| s.spec.role == ServiceRole::Database)
        else {
            return Err(invalid("データベースの役割のコンテナがありません"));
        };
        let (effective, detail) = match fault {
            TrialFault::DatabaseStopped => {
                docker_text(&["start", &self.services[index].container])?;
                let running = inspect_running(&self.services[index].container)?;
                (running, "データベースのコンテナを起動し直した")
            }
            TrialFault::DatabaseUnreachable => {
                let aliases = self.container_aliases(&self.services[index].spec);
                let mut args = vec!["network", "connect"];
                for alias in &aliases {
                    args.push("--alias");
                    args.push(alias);
                }
                args.push(&self.network);
                args.push(&self.services[index].container);
                docker_text(&args)?;
                let connected = inspect_network(&self.services[index].container, &self.network)?;
                (connected, "データベースを試験用ネットワークへ接続し直した")
            }
            TrialFault::VolumeReadOnly => {
                let index = self.recreate_database(false)?;
                let probe_path = format!(
                    "{}/.eel-write-probe",
                    self.services[index].spec.volumes[0].path
                );
                // 起動直後はまだ受け付けないことがあるため、書き込めるまで短く待つ。
                let writable = (0..20).any(|_| {
                    let ok = self.writable(index, &probe_path);
                    if !ok {
                        thread::sleep(Duration::from_millis(500));
                    }
                    ok
                });
                (writable, "保存先のボリュームを書き込み可能で載せ直した")
            }
            _ => return Err(invalid("この故障には復旧の操作がありません")),
        };
        self.services[index].detail = if effective {
            format!("{detail}。解除後の状態を確かめた")
        } else {
            format!("{detail}が、解除後の状態を確かめられなかった")
        };
        Ok(effective)
    }

    /// 復旧後に、各コンテナが準備できるまで待つ。準備できなかったことも記録する。
    pub fn settle(&mut self, timeout_ms: u64) -> Vec<crate::trial::RecoveredService> {
        let timeout = Duration::from_millis(timeout_ms);
        let mut out = vec![];
        for service in &mut self.services {
            let result = wait_ready_for(service, timeout);
            let (ready, ms, detail) = match result {
                Ok(ms) => (true, Some(ms), format!("復旧後に準備できた（{ms}ms）")),
                Err(error) => (false, None, error.to_string()),
            };
            service.ready = ready;
            out.push(crate::trial::RecoveredService {
                name: service.spec.name.clone(),
                ready,
                ready_after_ms: ms,
                detail,
            });
        }
        out
    }

    pub fn query_database(&self) -> Option<String> {
        let service = self
            .services
            .iter()
            .find(|s| s.spec.role == ServiceRole::Database)?;
        if !inspect_running(&service.container).ok()? {
            return Some("データベースのコンテナは動いていない".into());
        }
        let Some(client) = service.spec.engine.ping(&service.spec.env) else {
            return Some(format!(
                "DBの種類（{}）は基盤の読み出しに対応していないため、照会していない",
                service.spec.engine.label()
            ));
        };
        let output = docker(&exec_args(&service.container, &client)).ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Some(if output.status.success() {
            format!("基盤がデータベースへ SELECT 1 を送り、応答は {stdout}")
        } else {
            format!(
                "基盤がデータベースへ SELECT 1 を送ったが失敗した: {}",
                if stderr.is_empty() { stdout } else { stderr }
            )
        })
    }

    /// 追跡する値を、DBの文字列型の列すべてから探す。読取専用の照会だけを行う。
    pub fn scan_database(
        &self,
        values: &[crate::tracking::TrackedValue],
    ) -> crate::tracking::DatabaseScan {
        // 実行中の試行に停止を求める経路は、基盤にまだない。探索は期限だけで止まる。
        self.scan_database_within(
            values,
            crate::tracking::SCAN_BUDGET,
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    /// 探索の全体に許す時間と、停止の合図を指定して探す。
    /// 時間に達するか、`stop` が真になると、実行中の照会をDBの側で止めてから戻る。
    pub fn scan_database_within(
        &self,
        values: &[crate::tracking::TrackedValue],
        budget: Duration,
        stop: &std::sync::atomic::AtomicBool,
    ) -> crate::tracking::DatabaseScan {
        use crate::tracking::{DatabaseScan, Executed};
        let Some(service) = self
            .services
            .iter()
            .find(|s| s.spec.role == ServiceRole::Database)
        else {
            return DatabaseScan::not_performed("データベースの役割のコンテナがない");
        };
        if !inspect_running(&service.container).unwrap_or(false) {
            return DatabaseScan::not_performed("DBが停止しており、探索できない");
        }
        let mut scan = crate::tracking::scan(
            service.spec.engine,
            &service.spec.env,
            values,
            budget,
            stop,
            |request| {
                let output = docker_bounded(
                    &exec_args(&service.container, request.client),
                    request.within,
                    (!request.ignore_stop).then_some(stop),
                    crate::tracking::MAX_OUTPUT,
                )?;
                Ok(Executed {
                    ended: output.ended,
                    success: output.success,
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            },
        );
        scan.service = service.spec.name.clone();
        scan
    }

    /// 試験値に一致する行だけを、読取専用の照会で読む。
    pub fn read_database(
        &self,
        probe: &crate::browser::DatabaseProbe,
        test_value: &str,
    ) -> crate::browser::DatabaseRead {
        let mut read = crate::browser::DatabaseRead {
            id: probe.id.clone(),
            performed: false,
            exit_code: None,
            text: String::new(),
            detail: "DBの行を読み出していない".into(),
        };
        let Some(service) = self
            .services
            .iter()
            .find(|s| s.spec.name == probe.service && s.spec.role == ServiceRole::Database)
        else {
            return read;
        };
        if !inspect_running(&service.container).unwrap_or(false) {
            read.detail = "DBが停止しており、保存した行を読めない".into();
            return read;
        }
        if ![&probe.database, &probe.table, &probe.match_column]
            .iter()
            .all(|s| crate::browser::sql_identifier(s))
            || probe
                .schema
                .as_ref()
                .is_some_and(|s| !crate::browser::sql_identifier(s))
            || probe.columns.is_empty()
            || !probe
                .columns
                .iter()
                .all(|s| crate::browser::sql_identifier(s))
        {
            read.detail = "DBの識別子が不正".into();
            return read;
        }
        let sql = crate::browser::query(service.spec.engine, probe, test_value);
        let Some(client) = service
            .spec
            .engine
            .run(&service.spec.env, &probe.database, &sql)
        else {
            read.detail = format!(
                "DBの種類（{}）は基盤の読み出しに対応していないため、行を読んでいない。行の不存在とは扱わない",
                service.spec.engine.label()
            );
            return read;
        };
        match docker(&exec_args(&service.container, &client)) {
            Ok(output) => {
                read.performed = true;
                read.exit_code = output.status.code();
                if output.status.success() && output.stdout.len() <= 8192 {
                    read.text = String::from_utf8_lossy(&output.stdout).into_owned();
                    read.detail = "基盤が試験値に一致するDBの行を直接読み出した".into();
                } else {
                    read.exit_code = read.exit_code.filter(|c| *c != 0);
                    read.detail =
                        "DBの読取が失敗したか出力上限に達した。行の不存在とは扱わない".into();
                }
            }
            Err(_) => read.detail = "DBの読取を起動できない".into(),
        }
        read
    }

    pub fn request(&self, op: &HttpOp, snapshot: &Path, timeout_ms: u64) -> io::Result<HttpResult> {
        let running = self
            .services
            .iter()
            .find(|s| s.spec.name == op.service)
            .ok_or_else(|| invalid(format!("操作先のコンテナ {} がありません", op.service)))?;
        let tls = matches!(running.spec.ready, ReadyCheck::Http { tls: true, .. });
        let body = match &op.body_file {
            Some(path) => Some(fs::read(snapshot.join(path))?),
            None => None,
        };
        if body.as_ref().is_some_and(|b| b.len() > MAX_BODY) {
            return Err(invalid("HTTPの本文が1MiBを超えています"));
        }
        http_request(
            running.port,
            tls,
            &op.method,
            &op.path,
            op.content_type.as_deref(),
            body.as_deref(),
            timeout_ms,
        )
    }

    pub fn snapshot(&self) -> ContainerPhase {
        let addresses = self.addresses();
        if self.egress == Egress::Blocked {
            for service in &self.services {
                if let Some(tap) = &service.tap {
                    let _ = docker(&["stop", "-t", "2", tap]);
                }
            }
        }
        let (traffic, traffic_note) = self.traffic(&addresses);
        let (statements, statement_totals, statements_note) = self.statements().unwrap_or_default();
        for service in &self.services {
            let _ = docker(&["stop", "-t", "2", &service.container]);
        }
        let leftovers = labeled(&self.trial);
        ContainerPhase {
            engine: self.engine.clone(),
            network: self.network.clone(),
            egress: self.egress.label().into(),
            egress_confirmed: self.egress_confirmed,
            traffic,
            traffic_note,
            volumes: self.volumes.clone(),
            image_volumes: self.image_volumes.clone(),
            services: self
                .services
                .iter()
                .map(|s| {
                    let (log_text, log_complete) = read_logs(&s.container);
                    ServicePhase {
                        name: s.spec.name.clone(),
                        role: s.spec.role,
                        image: s.spec.image.clone(),
                        digest: s.digest.clone(),
                        container: s.container.clone(),
                        port: s.port,
                        started: s.started,
                        ready: s.ready,
                        ready_after_ms: s.ready_after_ms,
                        running_after_stop: inspect_running(&s.container).unwrap_or(false),
                        log_excerpt: excerpt(&s.container, &log_text),
                        log_note: if log_complete {
                            format!(
                                "標準出力・標準エラーのログを最後まで読んだ（{}バイト）",
                                log_text.len()
                            )
                        } else {
                            "ログを最後まで読めていない（上限超過または読み出しの失敗）".into()
                        },
                        log_complete,
                        log_text,
                        detail: s.detail.clone(),
                        web_build: s.web_build.clone(),
                        files: s.files.clone(),
                    }
                })
                .collect(),
            leftovers: leftovers.clone(),
            database_query: None,
            http: None,
            browser: None,
            database_reads: vec![],
            database_reads_before: vec![],
            http_reads: vec![],
            database_scan_before: None,
            database_scan: None,
            value_trace: None,
            statements,
            statement_totals,
            statements_note,
            detail: if leftovers.is_empty() {
                "試行用のコンテナ・ネットワーク・ボリュームをこの試行だけで使っている".into()
            } else {
                format!("基盤が管理する試験用の資源: {}", leftovers.join("、"))
            },
        }
    }

    pub fn cleanup(&mut self) -> io::Result<Vec<String>> {
        for service in self.services.iter().rev() {
            if let Some(tap) = &service.tap {
                let _ = remove_container(tap);
            }
            let _ = remove_container(&service.container);
        }
        if !self.edge.is_empty() {
            let _ = docker(&["network", "rm", &self.edge]);
        }
        if !self.network.is_empty() {
            let _ = docker(&["network", "rm", &self.network]);
        }
        for volume in self.volumes.iter().rev() {
            let _ = docker(&["volume", "rm", volume]);
        }
        self.cleaned = true;
        crate::shutdown::release_trial(&self.trial);
        let mut leftovers = labeled(&self.trial);
        // イメージの宣言でできたボリュームには、試行のラベルが付かない。控えた名前で確かめる。
        for volume in &self.image_volumes {
            if volume_exists(volume) {
                leftovers.push(format!("volume:{volume}"));
            }
        }
        if leftovers.is_empty() {
            Ok(leftovers)
        } else {
            Err(io::Error::other(format!(
                "試行の後に試験用のコンテナまたはボリュームが残った: {}",
                leftovers.join("、")
            )))
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

/// 対象とゲートウェイのコンテナから、既定で付く権限を減らす。
fn harden(args: &mut Vec<String>) {
    args.extend(
        [
            "--security-opt",
            "no-new-privileges",
            "--cap-drop",
            "ALL",
            "--pids-limit",
            PIDS_LIMIT,
        ]
        .map(String::from),
    );
    for capability in KEPT_CAPABILITIES {
        args.push("--cap-add".into());
        args.push(capability.into());
    }
}

fn role_label(role: ServiceRole) -> &'static str {
    match role {
        ServiceRole::App => "app",
        ServiceRole::Database => "database",
        ServiceRole::Other => "other",
    }
}

fn sanitize_trial(trial: &str) -> String {
    let trimmed = trial.trim_start_matches("run-");
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(40)
        .collect()
}

fn pull(image: &str) -> io::Result<String> {
    docker_text(&["pull", "--quiet", image])?;
    let digest = docker_text(&[
        "image",
        "inspect",
        "--format",
        "{{if .RepoDigests}}{{index .RepoDigests 0}}{{else}}{{.Id}}{{end}}",
        image,
    ])?;
    if digest.is_empty() {
        return Err(io::Error::other("イメージのダイジェストを記録できません"));
    }
    Ok(digest)
}

fn wait_ready(running: &Running) -> io::Result<u64> {
    wait_ready_for(
        running,
        Duration::from_millis(running.spec.ready_timeout_ms),
    )
}

fn wait_ready_for(running: &Running, timeout: Duration) -> io::Result<u64> {
    if running.spec.ready == ReadyCheck::Completed {
        return wait_completed(running, timeout);
    }
    let started = Instant::now();
    loop {
        if !inspect_running(&running.container)? {
            return Err(io::Error::other(format!(
                "コンテナ {} が準備の前に終了した: {}",
                running.spec.name,
                logs(&running.container)
            )));
        }
        let ok = match &running.spec.ready {
            ReadyCheck::Tcp => {
                crate::server::listening(running.port)
                    && (running.tap.is_none() || target_holds_connection(running.port))
            }
            ReadyCheck::Http { path, tls } => {
                http_request(running.port, *tls, "GET", path, None, None, 5000)
                    .ok()
                    .and_then(|r| r.status)
                    .is_some_and(|s| (200..400).contains(&s))
            }
            ReadyCheck::Exec { args } => {
                let mut cmd = vec!["exec", running.container.as_str()];
                cmd.extend(args.iter().map(String::as_str));
                docker(&cmd).map(|o| o.status.success()).unwrap_or(false)
            }
            ReadyCheck::Completed => unreachable!("終了を待つコンテナは wait_completed で待つ"),
        };
        if ok {
            return Ok(started.elapsed().as_millis() as u64);
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "コンテナ {} が {}ms 以内に準備できなかった: {}",
                running.spec.name,
                timeout.as_millis(),
                logs(&running.container)
            )));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// コンテナが終わるまで待つ。終了コード0だけを、準備できたとみなす。
fn wait_completed(running: &Running, timeout: Duration) -> io::Result<u64> {
    let started = Instant::now();
    loop {
        let state = docker_text(&[
            "inspect",
            "--format",
            "{{.State.Status}} {{.State.ExitCode}}",
            &running.container,
        ])?;
        match state.split_once(' ') {
            Some(("exited", "0")) => return Ok(started.elapsed().as_millis() as u64),
            Some(("exited" | "dead", code)) => {
                return Err(io::Error::other(format!(
                    "コンテナ {} が終了コード {code} で終わった: {}",
                    running.spec.name,
                    logs(&running.container)
                )));
            }
            _ => {}
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::other(format!(
                "コンテナ {} が {}ms 以内に終わらなかった: {}",
                running.spec.name,
                timeout.as_millis(),
                logs(&running.container)
            )));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// 中継は常に接続を受け付けるため、対象へつなげないときは直後に閉じられる。直後に閉じられなければ到達できたとみなす。
fn target_holds_connection(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => false,
        Ok(_) => true,
        Err(e) => matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ),
    }
}

fn inspect_running(container: &str) -> io::Result<bool> {
    match docker_text(&["inspect", "--format", "{{.State.Running}}", container]) {
        Ok(value) => Ok(value == "true"),
        Err(_) => Ok(false),
    }
}

fn inspect_network(container: &str, network: &str) -> io::Result<bool> {
    let value = docker_text(&[
        "inspect",
        "--format",
        "{{json .NetworkSettings.Networks}}",
        container,
    ])
    .unwrap_or_default();
    Ok(value.contains(network))
}

fn logs(container: &str) -> String {
    match recent_logs(container) {
        Some(recent) => tail(&recent),
        None => tail(&read_logs(container).0),
    }
}

/// 記録に残すログの末尾。対象が最後に書いた行を読めた場合はそれを使う。
/// `text`（`read_logs` の結果）は、標準出力の全文の後ろに標準エラーの全文が続き、上限で打ち切られる。
/// その末尾は、対象が最後に書いた行とは限らない。読み直せなかった場合だけ、`text` の末尾を使う。
fn excerpt(container: &str, text: &str) -> String {
    match recent_logs(container) {
        Some(recent) => tail(&recent),
        None => tail(text),
    }
}

/// 対象が最後に書いた行（EXCERPT_LINES行）を、標準出力と標準エラーを合わせて、書いた順のまま読む。
/// 2つの出力を同じパイプで受けるので、`docker logs` が出した順が保たれる。
fn recent_logs(container: &str) -> Option<String> {
    let (reader, writer) = io::pipe().ok()?;
    let mut command = Command::new("docker");
    command
        .args(["logs", "--tail", &EXCERPT_LINES.to_string(), container])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(writer.try_clone().ok()?)
        .stderr(writer);
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        command.env("DOCKER_HOST", host);
    }
    let mut child = command.spawn().ok()?;
    // 書き込み側をこのプロセスが持ったままだと、読み出しが終わらない。
    drop(command);
    let (bytes, _) = drain(reader);
    child
        .wait()
        .ok()?
        .success()
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// 表示と記録に残すログの末尾（EXCERPT_LINES行、MAX_LOG文字まで）。
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let joined = lines[lines.len().saturating_sub(EXCERPT_LINES)..].join("\n");
    let skip = joined.chars().count().saturating_sub(MAX_LOG);
    joined.chars().skip(skip).collect()
}

/// 上限までを残し、残りは読み捨てる。読み捨てないと、もう一方の出力を待つ間に対象が書き込みで止まる。
fn drain(mut reader: impl Read) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut whole = true;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return (kept, whole),
            Ok(n) => {
                let room = MAX_FULL_LOG.saturating_sub(kept.len());
                kept.extend_from_slice(&buffer[..n.min(room)]);
                whole &= n <= room;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return (kept, false),
        }
    }
}

/// コンテナのログを、標準出力と標準エラーの両方から読む。`docker logs`は対象の標準エラーを
/// 自身の標準エラーへ出すため、両方を読まないと対象の標準エラーが抜ける。
/// 上限を超えた・読めなかった場合は、最後まで読めていない（偽）を返す。
/// ログの保存方式（ログドライバー）の設定で古い行が消えている場合は、ここでは検出できない。
fn read_logs(container: &str) -> (String, bool) {
    let mut command = Command::new("docker");
    command
        .args(["logs", container])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        command.env("DOCKER_HOST", host);
    }
    let Ok(mut child) = command.spawn() else {
        return (String::new(), false);
    };
    let stdout = child.stdout.take().map(|p| thread::spawn(move || drain(p)));
    let stderr = child.stderr.take().map(|p| thread::spawn(move || drain(p)));
    let collect = |handle: Option<thread::JoinHandle<(Vec<u8>, bool)>>| {
        handle
            .and_then(|h| h.join().ok())
            .unwrap_or((Vec::new(), false))
    };
    let (out, out_whole) = collect(stdout);
    let (err, err_whole) = collect(stderr);
    let succeeded = child.wait().is_ok_and(|s| s.success());
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if !err.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&err));
    }
    (text, succeeded && out_whole && err_whole)
}

/// コンテナを消す。イメージの宣言でできた、名前を指定していないボリュームも一緒に消す。
/// `-v` を付けないと、そのボリュームは試行のラベルを持たないまま残る。
/// 名前を付けて作ったボリュームは `-v` では消えない。
fn remove_container(name: &str) -> io::Result<String> {
    docker_text(&["rm", "-f", "-v", name])
}

/// 名前で指すコンテナを消す。消えたか、もともとなければ真を返す。
pub(crate) fn remove_named(name: &str) -> bool {
    let _ = remove_container(name);
    docker(&["inspect", "--format", "{{.Id}}", name]).is_ok_and(|output| !output.status.success())
}

/// 試行のラベルを持つコンテナ・ネットワーク・ボリュームを消し、残ったものを返す。
/// 終了の合図を受けたときに、試行を動かしているスレッドとは別のスレッドから呼ぶ。
/// 試行の側が資源を作っている途中かもしれないので、残りがなくなるまで数回くり返す。
pub(crate) fn remove_labeled(trial: &str) -> Vec<String> {
    for _ in 0..3 {
        let found = labeled(trial);
        if found.is_empty() {
            break;
        }
        for item in &found {
            match item.split_once(':') {
                Some(("container", id)) => {
                    let _ = remove_container(id);
                }
                Some(("network", id)) => {
                    let _ = docker(&["network", "rm", id]);
                }
                Some(("volume", name)) => {
                    let _ = docker(&["volume", "rm", name]);
                }
                _ => {}
            }
        }
    }
    labeled(trial)
}

fn volume_exists(name: &str) -> bool {
    docker(&["volume", "inspect", "--format", "{{.Name}}", name])
        .is_ok_and(|output| output.status.success())
}

fn labeled(trial: &str) -> Vec<String> {
    let filter = format!("label=eel.trial={trial}");
    let mut found = vec![];
    for (kind, args) in [
        ("container", &["ps", "-aq", "--filter", &filter][..]),
        ("network", &["network", "ls", "-q", "--filter", &filter][..]),
        ("volume", &["volume", "ls", "-q", "--filter", &filter][..]),
    ] {
        if let Ok(text) = docker_text(args) {
            for id in text.split_whitespace() {
                found.push(format!("{kind}:{id}"));
            }
        }
    }
    found
}

fn http_request(
    port: u16,
    tls: bool,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
    timeout_ms: u64,
) -> io::Result<HttpResult> {
    let url = format!(
        "{}://127.0.0.1:{port}{path}",
        if tls { "https" } else { "http" }
    );
    if tls {
        return curl_request(&url, method, content_type, body, timeout_ms);
    }
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(timeout_ms.min(5000)),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(timeout_ms.min(15000))))?;
    stream.set_write_timeout(Some(Duration::from_millis(5000)))?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(kind) = content_type {
        request.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    let payload = body.unwrap_or(b"");
    request.push_str(&format!("Content-Length: {}\r\n\r\n", payload.len()));
    stream.write_all(request.as_bytes())?;
    if !payload.is_empty() {
        stream.write_all(payload)?;
    }
    // 送信側は閉じない。要求の終わりは Content-Length で分かる。応答を読む前に送信側を閉じると、
    // 要求を別のサーバーへ転送するnginxは、利用者が中断したとみなして転送をやめる（状態499）。
    let mut buf = Vec::new();
    stream.take(MAX_BODY as u64 + 8192).read_to_end(&mut buf)?;
    parse_http(&url, method, false, &buf)
}

fn curl_request(
    url: &str,
    method: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
    timeout_ms: u64,
) -> io::Result<HttpResult> {
    let dir = tempfile()?;
    let header_path = dir.join("headers");
    let body_path = dir.join("body");
    let payload = dir.join("payload");
    if let Some(bytes) = body {
        fs::write(&payload, bytes)?;
    }
    let seconds = ((timeout_ms / 1000).max(1)).to_string();
    let mut args = vec![
        "-sS".into(),
        "-D".into(),
        header_path.to_string_lossy().into_owned(),
        "-o".into(),
        body_path.to_string_lossy().into_owned(),
        "--max-time".into(),
        seconds,
        "--connect-timeout".into(),
        "5".into(),
        "-k".into(),
        "-X".into(),
        method.to_owned(),
    ];
    if let Some(kind) = content_type {
        args.push("-H".into());
        args.push(format!("Content-Type: {kind}"));
    }
    if body.is_some() {
        args.push("--data-binary".into());
        args.push(format!("@{}", payload.display()));
    }
    args.push(url.to_owned());
    let mut command = Command::new("curl");
    command
        .args(&args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null());
    let output = command.output()?;
    let headers = fs::read(&header_path).unwrap_or_default();
    let body = fs::read(&body_path).unwrap_or_default();
    let mut combined = headers;
    combined.extend_from_slice(&body);
    let mut result = parse_http(url, method, true, &combined)?;
    if !output.status.success() && result.status.is_none() {
        result.detail = format!(
            "curlが失敗した: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let _ = fs::remove_dir_all(dir);
    Ok(result)
}

fn tempfile() -> io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "eel-http-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir(&path)?;
    Ok(path)
}

fn parse_http(url: &str, method: &str, tls_insecure: bool, raw: &[u8]) -> io::Result<HttpResult> {
    let text = String::from_utf8_lossy(raw);
    let status = text
        .lines()
        .find(|l| l.starts_with("HTTP/"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok());
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or(text.split("\n\n").nth(1).unwrap_or(""));
    let excerpt: String = body.chars().take(2000).collect();
    Ok(HttpResult {
        method: method.into(),
        url: url.into(),
        status,
        tls_insecure,
        body_excerpt: excerpt.clone(),
        detail: match status {
            Some(code) => format!(
                "基盤が {} {} へ要求し、HTTP {code} を受け取った{}",
                method,
                url,
                if tls_insecure {
                    "（試験用の自己署名証明書のため、証明書の検証はしていない）"
                } else {
                    ""
                }
            ),
            None => "HTTPの状態コードを読み取れなかった".into(),
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn logs_are_kept_up_to_the_limit_and_overflow_is_reported() {
        let (kept, whole) = drain(std::io::Cursor::new(b"line\n".repeat(3)));
        assert!(whole);
        assert_eq!(kept, b"line\n".repeat(3));
        let large = vec![b'x'; MAX_FULL_LOG + 10];
        let (kept, whole) = drain(std::io::Cursor::new(large));
        assert!(!whole, "上限を超えたログは最後まで読めていない扱い");
        assert_eq!(kept.len(), MAX_FULL_LOG);
        let text: String = (1..=100).map(|n| format!("{n}\n")).collect();
        let excerpt = tail(&text);
        assert!(
            excerpt.starts_with("21\n") && excerpt.ends_with("100"),
            "{excerpt}"
        );
    }

    use super::*;

    #[test]
    fn egress_defaults_to_blocked_and_is_omitted_from_old_settings() {
        let spec: ContainerSpec = serde_json::from_str(r#"{"services":[]}"#).unwrap();
        assert_eq!(spec.egress, Egress::Blocked);
        assert!(!serde_json::to_string(&spec).unwrap().contains("egress"));
        let allowed: ContainerSpec =
            serde_json::from_str(r#"{"services":[],"egress":"allowed"}"#).unwrap();
        assert!(serde_json::to_string(&allowed).unwrap().contains("allowed"));
    }

    #[test]
    fn connections_are_summarized_by_client_and_target() {
        let line = |t: f64, status: &str, bytes_in: u64, out: u64| {
            serde_json::from_str::<TapLine>(&format!(
                r#"{{"id":1,"t":{t},"client":"10.0.0.2","local":"10.0.0.9","status":"{status}","in":{bytes_in},"out":{out}}}"#
            ))
            .unwrap()
        };
        let mut rows = BTreeMap::new();
        record_connection(&mut rows, "app", "db", &line(1000.5, "200", 10, 100));
        record_connection(&mut rows, "app", "db", &line(1002.0, "500", 0, 0));
        record_connection(&mut rows, HOST_CLIENT, "app", &line(999.0, "200", 5, 50));
        let app_to_db = &rows[&("app".to_owned(), "db".to_owned())];
        assert_eq!(app_to_db.connections, 2);
        assert_eq!(app_to_db.failed, 1);
        assert_eq!(app_to_db.bytes_from_client, 10);
        assert_eq!(app_to_db.bytes_to_client, 100);
        assert_eq!(app_to_db.first_at_ms, 1_000_500);
        assert_eq!(app_to_db.last_at_ms, 1_002_000);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn unfinished_connections_are_read_from_the_error_log() {
        let log = "2026/10/25 15:26:28 [info] 7#7: *4 client 172.18.0.4:58172 connected to 0.0.0.0:80\n\
                   2026/10/25 15:26:28 [info] 7#7: *4 proxy 172.18.0.3:1 connected to 172.18.0.2:80\n\
                   2026/10/25 15:26:29 [notice] 1#1: start worker process 7\n";
        assert_eq!(started_connections(log), vec![(4, "172.18.0.4".to_owned())]);
        let mut rows = BTreeMap::new();
        record_unfinished(&mut rows, "web", "back");
        let row = &rows[&("web".to_owned(), "back".to_owned())];
        assert_eq!((row.connections, row.unfinished, row.failed), (1, 1, 0));
    }

    #[test]
    fn hardening_drops_all_capabilities_then_adds_the_kept_ones() {
        let mut args = vec![];
        harden(&mut args);
        let text = args.join(" ");
        assert!(text.contains("--cap-drop ALL"));
        assert!(text.contains("no-new-privileges"));
        assert!(!text.contains("--cap-add SYS_ADMIN"));
        assert_eq!(text.matches("--cap-add").count(), KEPT_CAPABILITIES.len());
    }

    #[test]
    fn engine_is_only_for_the_database_role_and_is_omitted_when_default() {
        let recipe: serde_json::Value =
            serde_json::from_str(include_str!("../examples/openemr-browser/soap-recipe.json"))
                .unwrap();
        // 種類を書いていない既存の設定は、保存し直しても種類の項目を持たない。
        // 判定条件の指紋はこの保存形から計算するため、既存の指紋が変わらない。
        let spec: ContainerSpec = serde_json::from_value(recipe["containers"].clone()).unwrap();
        assert!(spec.services.iter().all(|s| s.engine.is_default()));
        assert!(
            serde_json::to_value(&spec).unwrap()["services"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s.get("engine").is_none())
        );
        validate_spec(&spec).unwrap();

        let mut declared = spec.clone();
        declared.services[0].engine = DatabaseEngine::Postgres;
        validate_spec(&declared).unwrap();
        assert_eq!(
            serde_json::to_value(&declared).unwrap()["services"][0]["engine"],
            "postgres"
        );
        // DBではないコンテナへの指定と、未知の種類は拒否する。
        let mut misplaced = spec.clone();
        misplaced.services[1].engine = DatabaseEngine::Postgres;
        assert!(validate_spec(&misplaced).is_err());
        let mut unknown = recipe["containers"].clone();
        unknown["services"][0]["engine"] = "oracle".into();
        assert!(serde_json::from_value::<ContainerSpec>(unknown).is_err());
    }

    #[test]
    fn a_statement_relay_needs_a_blocked_network_a_known_service_and_a_well_formed_cut() {
        let recipe: serde_json::Value =
            serde_json::from_str(include_str!("../examples/openemr-browser/soap-recipe.json"))
                .unwrap();
        let base: ContainerSpec = serde_json::from_value(recipe["containers"].clone()).unwrap();
        // 中継を指定しない既存の設定は、保存し直しても項目を持たない。
        assert!(serde_json::to_value(&base).unwrap().get("relay").is_none());
        let with = |relay: serde_json::Value, egress: Egress| {
            let mut spec = base.clone();
            spec.egress = egress;
            spec.relay = Some(serde_json::from_value(relay).unwrap());
            validate_spec(&spec)
        };
        let shape = "0123456789abcdef";
        with(
            serde_json::json!({"service": "mysql", "protocol": "mysql"}),
            Egress::Blocked,
        )
        .unwrap();
        with(
            serde_json::json!({"service": "mysql", "protocol": "mysql",
                "cut": {"shape": shape, "occurrence": 2, "phase": "after_forward_before_reply"}}),
            Egress::Blocked,
        )
        .unwrap();
        // 外部通信を許可した試験には中継がないので、置けない。
        assert!(
            with(
                serde_json::json!({"service": "mysql", "protocol": "mysql"}),
                Egress::Allowed
            )
            .is_err()
        );
        assert!(
            with(
                serde_json::json!({"service": "no-such", "protocol": "mysql"}),
                Egress::Blocked
            )
            .is_err()
        );
        for cut in [
            serde_json::json!({"shape": "short", "occurrence": 1, "phase": "after_reply"}),
            serde_json::json!({"shape": "zzzzzzzzzzzzzzzz", "occurrence": 1, "phase": "after_reply"}),
            serde_json::json!({"shape": shape, "occurrence": 0, "phase": "after_reply"}),
        ] {
            assert!(
                with(
                    serde_json::json!({"service": "mysql", "protocol": "mysql", "cut": cut}),
                    Egress::Blocked
                )
                .is_err(),
                "{cut}"
            );
        }
        // 解釈しない中継では、文を数えられないので、切る時点を指定できない。
        assert!(
            with(
                serde_json::json!({"service": "mysql", "protocol": "tcp",
                    "cut": {"shape": shape, "occurrence": 1, "phase": "after_reply"}}),
                Egress::Blocked
            )
            .is_err()
        );
        // 未知の時点と未知の項目は、読み込みの段階で拒否する。
        for relay in [
            serde_json::json!({"service": "mysql", "protocol": "mysql",
                "cut": {"shape": shape, "occurrence": 1, "phase": "during_reply"}}),
            serde_json::json!({"service": "mysql", "protocol": "mysql", "sql": "DROP"}),
            serde_json::json!({"service": "mysql", "protocol": "oracle"}),
        ] {
            assert!(serde_json::from_value::<RelaySetting>(relay).is_err());
        }
    }

    #[test]
    fn digest_is_required() {
        assert!(image_digest("nginx:1.27.4-alpine").is_none());
        assert!(image_digest(
            "nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591"
        )
        .is_some());
        let spec = ContainerSpec {
            services: vec![Service {
                name: "web".into(),
                image: "nginx:1.27.4-alpine".into(),
                role: ServiceRole::App,
                expose: 80,
                env: BTreeMap::new(),
                volumes: vec![],
                web_build: None,
                start_after: vec![],
                files: vec![],
                aliases: vec![],
                engine: DatabaseEngine::default(),
                command: vec![],
                ready: ReadyCheck::Tcp,
                ready_timeout_ms: 5000,
            }],
            egress: Egress::default(),
            relay: None,
        };
        assert!(validate_spec(&spec).is_err());
    }

    #[test]
    fn service_names_cannot_shadow_another_services_real_alias() {
        let mut spec: ContainerSpec = serde_json::from_str(
            r#"{"services":[{"name":"app","image":"nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591","role":"app","expose":80,"ready":"tcp","ready_timeout_ms":5000}]}"#,
        ).unwrap();
        let mut other = spec.services[0].clone();
        other.name = "app-real".into();
        spec.services.push(other);
        assert!(
            validate_spec(&spec)
                .unwrap_err()
                .to_string()
                .contains("中継用")
        );
        spec.egress = Egress::Allowed;
        assert!(validate_spec(&spec).is_ok());
    }

    fn named(names: &[(&str, &str, &[&str])]) -> ContainerSpec {
        let services: Vec<serde_json::Value> = names
            .iter()
            .map(|(name, role, after)| {
                serde_json::json!({
                    "name": name,
                    "image": "nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591",
                    "role": role, "expose": 80, "ready": "tcp", "ready_timeout_ms": 5000,
                    "start_after": after,
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({ "services": services })).unwrap()
    }

    fn order(spec: &ContainerSpec) -> Vec<&str> {
        start_order(&spec.services)
            .unwrap()
            .iter()
            .map(|s| s.name.as_str())
            .collect()
    }

    #[test]
    fn start_order_follows_roles_unless_a_service_waits_for_another() {
        // 指定がなければ、役割の順（DB、その他、アプリ）と一覧の順で起動する。
        let plain = named(&[
            ("web", "app", &[]),
            ("gateway", "other", &[]),
            ("db", "database", &[]),
            ("cache", "other", &[]),
        ]);
        validate_spec(&plain).unwrap();
        assert_eq!(order(&plain), ["db", "gateway", "cache", "web"]);
        // 指定のない設定の保存形は変わらない。
        assert!(
            !serde_json::to_string(&plain)
                .unwrap()
                .contains("start_after")
        );
        // 入口（その他）は、アプリの準備ができてから起動する。ほかの順番は変わらない。
        let waits = named(&[
            ("web", "app", &[]),
            ("gateway", "other", &["web"]),
            ("db", "database", &[]),
            ("cache", "other", &[]),
        ]);
        validate_spec(&waits).unwrap();
        assert_eq!(order(&waits), ["db", "cache", "web", "gateway"]);
        // 待つ相手がさらに別のコンテナを待つ場合も、順に起動する。
        let chained = named(&[
            ("db", "database", &["seed"]),
            ("seed", "other", &["certs"]),
            ("certs", "app", &[]),
        ]);
        validate_spec(&chained).unwrap();
        assert_eq!(order(&chained), ["certs", "seed", "db"]);
    }

    fn one(extra: serde_json::Value) -> ContainerSpec {
        let mut service = serde_json::json!({
            "name": "app",
            "image": "nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591",
            "role": "app", "expose": 80, "ready": "tcp", "ready_timeout_ms": 5000,
        });
        for (key, value) in extra.as_object().unwrap() {
            service[key] = value.clone();
        }
        serde_json::from_value(serde_json::json!({ "services": [service] })).unwrap()
    }

    #[test]
    fn files_take_repository_paths_and_absolute_container_paths() {
        let plain = one(serde_json::json!({}));
        validate_spec(&plain).unwrap();
        // 指定のない設定の保存形は変わらない。
        let saved = serde_json::to_string(&plain).unwrap();
        assert!(saved.contains("\"expose\":80"));
        for absent in ["files", "aliases", "completed"] {
            assert!(!saved.contains(absent), "{absent}: {saved}");
        }
        let file =
            |from: &str, to: &str| one(serde_json::json!({"files": [{"from": from, "to": to}]}));
        validate_spec(&file(
            "configs/nginx/nginx.conf",
            "/etc/nginx/nginx.conf.template",
        ))
        .unwrap();
        for (from, to) in [
            ("/etc/passwd", "/etc/x"),
            ("../outside", "/etc/x"),
            ("configs/../../outside", "/etc/x"),
            ("configs//x", "/etc/x"),
            ("", "/etc/x"),
            ("configs/x", "etc/x"),
            ("configs/x", "/etc/../x"),
            ("configs/x", "/etc/x/"),
            ("configs/x", "/"),
        ] {
            assert!(validate_spec(&file(from, to)).is_err(), "{from} -> {to}");
        }
        // 同じ置き場所を2回は指定できない。件数にも上限がある。
        let twice = one(serde_json::json!({"files": [
            {"from": "a", "to": "/etc/x"}, {"from": "b", "to": "/etc/x"}
        ]}));
        assert!(validate_spec(&twice).is_err());
        let many: Vec<_> = (0..=MAX_PLACED_FILES)
            .map(|i| serde_json::json!({"from": format!("f{i}"), "to": format!("/etc/f{i}")}))
            .collect();
        assert!(validate_spec(&one(serde_json::json!({"files": many}))).is_err());
        // 出所の写しを渡さずに、ファイルを置く設定は起動できない。
        let error = Session::start("no-source", &file("a", "/etc/a"))
            .err()
            .expect("拒否")
            .to_string();
        assert!(error.contains("出所"), "{error}");
    }

    #[test]
    fn aliases_are_host_names_that_do_not_collide() {
        let alias = |names: &[&str]| one(serde_json::json!({"aliases": names}));
        validate_spec(&alias(&["oe.openelis.org", "oe-2"])).unwrap();
        for bad in [
            "",
            "Upper.example",
            "under_score",
            "-start.example",
            "end-.example",
            "a..b",
            "a b",
        ] {
            assert!(validate_spec(&alias(&[bad])).is_err(), "{bad}");
        }
        // 自分やほかのコンテナの名前、中継用の接続先名、ほかの別名とは重ねられない。
        for taken in ["app", "app-real"] {
            let error = validate_spec(&alias(&[taken])).unwrap_err().to_string();
            assert!(error.contains("重複"), "{error}");
        }
        assert!(validate_spec(&alias(&["x.example", "x.example"])).is_err());
        let mut two = named(&[("web", "app", &[]), ("db", "database", &[])]);
        two.services[0].aliases = vec!["shared.example".into()];
        two.services[1].aliases = vec!["shared.example".into()];
        assert!(validate_spec(&two).is_err());
        two.services[1].aliases = vec!["db.example".into()];
        validate_spec(&two).unwrap();
    }

    #[test]
    fn a_service_that_runs_to_completion_has_no_port_and_cannot_be_the_database() {
        let completed: ContainerSpec = serde_json::from_value(serde_json::json!({"services": [{
            "name": "certs",
            "image": "nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591",
            "role": "other", "ready": "completed", "ready_timeout_ms": 5000,
        }]}))
        .unwrap();
        validate_spec(&completed).unwrap();
        let saved = serde_json::to_string(&completed).unwrap();
        assert!(saved.contains("\"ready\":\"completed\"") && !saved.contains("expose"));
        let mut with_port = completed.clone();
        with_port.services[0].expose = 80;
        assert!(validate_spec(&with_port).is_err());
        let mut database = completed.clone();
        database.services[0].role = ServiceRole::Database;
        assert!(validate_spec(&database).is_err());
        // 待ち受けるコンテナには、これまでどおりポートが要る。
        let mut listening = completed;
        listening.services[0].ready = ReadyCheck::Tcp;
        assert!(validate_spec(&listening).is_err());
    }

    #[test]
    fn tar_archive_holds_only_the_files_with_fixed_owner_and_time() {
        let long = format!("{}/{}", "d".repeat(120), "f".repeat(90));
        let archive = tar_archive(&[
            (
                "run/secrets/common.properties".into(),
                b"a=b\n".to_vec(),
                false,
            ),
            (long.clone(), vec![7u8; 600], true),
        ])
        .unwrap();
        // 見出し1件、内容は512バイトの倍数に切り上げ、末尾に空の2ブロック。
        assert_eq!(archive.len(), 512 + 512 + 512 + 1024 + 1024);
        let field = |block: &[u8], range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&block[range])
                .trim_end_matches(['\0', ' '])
                .to_owned()
        };
        let first = &archive[..512];
        assert_eq!(field(first, 0..100), "run/secrets/common.properties");
        assert_eq!(field(first, 100..108), "0000644");
        assert_eq!(
            (field(first, 108..116), field(first, 116..124)),
            ("0000000".into(), "0000000".into())
        );
        assert_eq!(field(first, 124..136), "00000000004");
        assert_eq!(field(first, 136..148), "00000000000");
        assert_eq!(first[156], b'0');
        assert_eq!(&first[257..262], b"ustar");
        // 検査用の合計は、合計の欄を空白として数えた値。
        let mut blank = first.to_vec();
        blank[148..156].fill(b' ');
        let sum: u64 = blank.iter().map(|b| u64::from(*b)).sum();
        assert_eq!(field(first, 148..154), format!("{sum:06o}"));
        assert_eq!(&archive[512..516], b"a=b\n");
        // 100バイトを超えるパスは、名前と前半に分ける。
        let second = &archive[1024..1536];
        assert_eq!(field(second, 0..100), "f".repeat(90));
        assert_eq!(field(second, 345..500), "d".repeat(120));
        assert_eq!(field(second, 100..108), "0000755");
        assert!(tar_archive(&[("x".repeat(101), vec![], false)]).is_err());
    }

    #[test]
    fn start_after_rejects_unknown_names_itself_duplicates_and_cycles() {
        for (spec, expected) in [
            (
                named(&[("web", "app", &["missing"])]),
                "ほかのコンテナの名前",
            ),
            (named(&[("web", "app", &["web"])]), "ほかのコンテナの名前"),
            (
                named(&[("web", "app", &["db", "db"]), ("db", "database", &[])]),
                "ほかのコンテナの名前",
            ),
            (
                named(&[("web", "app", &["gateway"]), ("gateway", "other", &["web"])]),
                "互いを待っています",
            ),
        ] {
            let error = validate_spec(&spec).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn ready_timeout_accepts_thirty_minutes_and_rejects_more() {
        let mut spec: ContainerSpec = serde_json::from_str(
            r#"{"services":[{"name":"app","image":"nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591","role":"app","expose":80,"ready":"tcp","ready_timeout_ms":1800000}]}"#,
        ).unwrap();
        assert!(validate_spec(&spec).is_ok());
        for rejected in [0, 1_800_001] {
            spec.services[0].ready_timeout_ms = rejected;
            assert!(
                validate_spec(&spec)
                    .unwrap_err()
                    .to_string()
                    .contains("1〜1800000ミリ秒"),
                "{rejected}"
            );
        }
    }

    #[test]
    fn http_target_must_exist() {
        let spec = ContainerSpec {
            services: vec![Service {
                name: "web".into(),
                image: "nginx:1.27.4-alpine@sha256:4ff102c5d78d254a6f0da062b3cf39eaf07f01eec0927fd21e219d0af8bc0591".into(),
                role: ServiceRole::App,
                expose: 80,
                env: BTreeMap::new(),
                volumes: vec![],
            web_build: None,
            start_after: vec![],
            files: vec![],
            aliases: vec![],
            engine: DatabaseEngine::default(),
                command: vec![],
                ready: ReadyCheck::Http { path: "/".into(), tls: false },
                ready_timeout_ms: 5000,
            }],
            egress: Egress::default(),
            relay: None,
        };
        assert!(validate_spec(&spec).is_ok());
        assert!(
            validate_http(
                &HttpOp {
                    service: "missing".into(),
                    method: "GET".into(),
                    path: "/".into(),
                    body_file: None,
                    content_type: None,
                },
                &spec
            )
            .is_err()
        );
    }
}
