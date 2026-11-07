//! 運営者が設定した AIエージェント（mugiurushi-ai）を呼ぶ。
//! 接続先と共有トークンはサーバー側の設定だけで、画面からは入力させない。AIが未設定・失敗しても、
//! 確認項目・試験・判定は今までどおり使える。AIの出力は人が採用するまで下書きで、判定には使わない。
//! 送る内容はサーバーが事例から組み立て、まず送らずに内容と SHA-256 を返す（確認）。人が内容を見て、架空のデータ
//! だけであることを確かめたうえで、同じ SHA-256 を添えて送る。組み立て直した内容が確認したものと違えば送らない。
//! 確認項目や自由記入欄に何が書かれているかは麦漆には判断できないため、送ってよいかは人が確かめる。
use crate::coverage::ObservationKind;
use crate::error::invalid;
use crate::workspace::{self, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io, path::PathBuf, time::Duration};

const TOKEN_HEADER: &str = "X-Mugiurushi-AI-Token";
const MAX_RESPONSE: u64 = 2 * 1024 * 1024;
const MAX_REQUEST: usize = 2000;

#[derive(Clone)]
pub struct Config {
    base_url: String,
    token_file: PathBuf,
}

impl Config {
    pub fn new(base_url: &str, token_file: PathBuf) -> io::Result<Self> {
        let refuse = || {
            invalid(
                "AIエージェントの接続先は https://… か、同じ機械の http://127.0.0.1:… にしてください（ユーザー情報・クエリ・フラグメントは使えません）",
            )
        };
        // 文字列の先頭ではなく、URLとして解析したスキームとホストで判定する。共有トークンを平文で外へ送らない。
        let url = url::Url::parse(base_url).map_err(|_| refuse())?;
        let loopback = matches!(
            url.host(),
            Some(url::Host::Ipv4(ip)) if ip.is_loopback()
        ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
            || url.host_str() == Some("localhost");
        let allowed = match url.scheme() {
            "https" => url.host().is_some(),
            "http" => loopback,
            _ => false,
        };
        if !allowed
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || base_url.contains(['@', '?', '#'])
        {
            return Err(refuse());
        }
        Ok(Self {
            base_url: url.as_str().trim_end_matches('/').to_owned(),
            token_file,
        })
    }
    /// 運営者が起動時に指定する。部分的な設定を無視しない。
    pub fn from_env() -> io::Result<Option<Self>> {
        let url = std::env::var("EEL_AGENT_URL").ok();
        let token = std::env::var_os("EEL_AGENT_TOKEN_FILE");
        match (url, token) {
            (None, None) => Ok(None),
            (Some(url), Some(token)) => {
                let config = Self::new(&url, PathBuf::from(token))?;
                config.token()?;
                Ok(Some(config))
            }
            _ => Err(invalid(
                "運営者設定の EEL_AGENT_URL と EEL_AGENT_TOKEN_FILE を両方指定してください",
            )),
        }
    }
    fn token(&self) -> io::Result<String> {
        let token = std::fs::read_to_string(&self.token_file)
            .map_err(|_| invalid("AIエージェントの共有トークンのファイルを読めません"))?
            .trim()
            .to_owned();
        if token.len() < 16 || token.chars().any(char::is_whitespace) {
            return Err(invalid(
                "AIエージェントの共有トークンは空白のない16文字以上にしてください",
            ));
        }
        Ok(token)
    }
    fn post(&self, path: &str, body: &Value) -> io::Result<Value> {
        let token = self.token()?;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .into();
        let bytes = serde_json::to_vec(body).map_err(io::Error::other)?;
        let mut response = agent
            .post(format!("{}{path}", self.base_url))
            .header("Content-Type", "application/json")
            .header(TOKEN_HEADER, &token)
            .send(bytes.as_slice())
            .map_err(|e| io::Error::other(format!("AIエージェントに接続できません: {e}")))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE)
            .read_to_string()
            .map_err(|_| io::Error::other("AIエージェントの応答を読み取れません"))?;
        if status != 200 {
            let detail: String = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["detail"].as_str().map(str::to_owned))
                .unwrap_or_default()
                .replace(&token, "（共有トークン）")
                .chars()
                .take(500)
                .collect();
            return Err(io::Error::other(format!(
                "AIエージェントが処理できませんでした（HTTP {status}）{detail}"
            )));
        }
        serde_json::from_str(&text)
            .map_err(|_| io::Error::other("AIエージェントの応答がJSONではありません"))
    }
}

pub fn status(config: Option<&Config>) -> Value {
    match config {
        None => json!({
            "status": "not_configured",
            "message": "AIエージェントは設定されていません。確認項目・試験・判定はAIなしで使えます。",
        }),
        Some(_) => json!({
            "status": "configured",
            "message": "AIエージェントの案は下書きです。採用するまで確認項目には入らず、判定には使いません。",
        }),
    }
}

/// 送る前の確認。`confirm` が無ければ送らずに内容を返す。あれば、確認した内容の SHA-256 と、
/// 架空のデータだけであることの人の確認を求める。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirm {
    pub payload_sha256: String,
    pub synthetic_only: bool,
}

/// 画面から受け取るのは確認項目のIDと、任意の追加の依頼と、送る前の確認だけ。送る内容はサーバーが事例から組み立てる。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeRequest {
    pub check_id: String,
    #[serde(default)]
    pub request: Option<String>,
    #[serde(default)]
    pub confirm: Option<Confirm>,
}

/// 確認なら送らずに内容を返し（Ok(Some)）、送ってよい場合だけ Ok(None) を返す。
fn preview_or_check(
    path: &str,
    payload: &Value,
    confirm: Option<&Confirm>,
) -> io::Result<Option<Value>> {
    let bytes = serde_json::to_vec(payload).map_err(io::Error::other)?;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let Some(confirm) = confirm else {
        return Ok(Some(json!({
            "stage": "preview",
            "destination": "運営者が設定したAIエージェント（mugiurushi-ai）と、その先のOpenAI API",
            "path": path,
            "payload": payload,
            "payload_sha256": sha,
            "bytes": bytes.len(),
            "note": "これがそのまま送られる内容です。実患者の情報・認証情報・生のログが含まれていないことを確かめてください。麦漆は内容の安全性を判断しません。初回の接続とデモは架空のデータの事例だけで行います。",
        })));
    };
    if !confirm.synthetic_only {
        return Err(invalid(
            "送る内容が架空のデータだけであることを確かめてから送ってください",
        ));
    }
    if confirm.payload_sha256 != sha {
        return Err(invalid(
            "確認した後に送る内容が変わりました。もう一度確認してください",
        ));
    }
    Ok(None)
}

/// AIの「確かめる部分」の案。人が採用するまで確認項目には入らない。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub text: String,
    pub origin: String,
    pub observation: Option<ObservationKind>,
    pub rationale: String,
    pub unknowns: Vec<String>,
}

fn find_check<'a>(workspace: &'a Workspace, check_id: &str) -> io::Result<&'a workspace::Check> {
    workspace
        .checks
        .iter()
        .find(|c| c.id == check_id)
        .ok_or_else(|| invalid("確認項目が見つかりません"))
}

pub fn propose(
    config: Option<&Config>,
    workspace: Workspace,
    request: ProposeRequest,
) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    let check = find_check(&workspace, &request.check_id)?;
    let extra = request.request.unwrap_or_default();
    if extra.chars().count() > MAX_REQUEST {
        return Err(invalid("追加の依頼は2000文字以内にしてください"));
    }
    // 対象のリポジトリ・コミット・パスは案に必要ないため送らない。
    let system =
        "麦漆の確認項目から試験の範囲を決める。対象システムの説明・コード・実行記録は送らない";
    let messages: Vec<Value> = if extra.trim().is_empty() {
        vec![]
    } else {
        vec![json!({"role": "user", "text": extra.trim()})]
    };
    let current: Vec<Value> = check
        .aspects
        .iter()
        .map(|a| json!({"id": a.id, "text": a.text, "observation": a.observation}))
        .collect();
    let payload = json!({"system": system, "concern": check.text, "messages": messages, "current_aspects": current});
    if let Some(preview) =
        preview_or_check("/v1/dialogue/turn", &payload, request.confirm.as_ref())?
    {
        return Ok(preview);
    }
    let answer = config.post("/v1/dialogue/turn", &payload)?;
    let mut proposals = vec![];
    for item in answer["aspects"]
        .as_array()
        .ok_or_else(|| invalid("AIエージェントの案の形が不正です"))?
    {
        let aspect = &item["aspect"];
        // 判定条件への対応づけは人が麦漆で選ぶ。AIの案に含まれていれば受け取らない。
        if aspect["trial_checks"]
            .as_array()
            .is_none_or(|l| !l.is_empty())
        {
            return Err(invalid("判定条件の対応づけを含むAIの案は受け取りません"));
        }
        let observation: Option<ObservationKind> =
            serde_json::from_value(aspect["observation"].clone())
                .map_err(|_| invalid("麦漆にない観測の種類を含むAIの案は受け取りません"))?;
        let text = aspect["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let id = aspect["id"].as_str().unwrap_or_default().to_owned();
        if text.is_empty() || id.is_empty() {
            return Err(invalid("文または識別子のないAIの案は受け取りません"));
        }
        proposals.push(Proposal {
            id,
            text,
            origin: aspect["origin"].as_str().unwrap_or("AIの案").to_owned(),
            observation,
            rationale: item["rationale"].as_str().unwrap_or_default().to_owned(),
            unknowns: strings(&item["unknowns"]),
        });
    }
    // 案の形は確認項目の検査に通るものだけにする（採用時にも set_aspects で同じ検査を行う）。
    crate::coverage::validate(
        &proposals
            .iter()
            .map(|p| crate::coverage::Aspect {
                id: p.id.clone(),
                text: p.text.clone(),
                origin: p.origin.clone(),
                observation: p.observation,
                trial_checks: vec![],
            })
            .collect::<Vec<_>>(),
    )?;
    let out_of_scope: Vec<Value> = answer["out_of_scope"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|o| Some(json!({"text": o["text"].as_str()?, "reason": o["reason"].as_str().unwrap_or("")})))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "stage": "sent",
        "status": "draft",
        "reply": answer["reply"],
        "questions": strings(&answer["questions"]),
        "proposals": proposals,
        "out_of_scope": out_of_scope,
        "model_call": answer["model_call"],
        "note": "AIの案です。採用した部分だけが確認項目に追加され、観測の種類と判定条件を人が選ぶまで未確認のままです。",
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplainRequest {
    pub check_id: String,
    #[serde(default)]
    pub confirm: Option<Confirm>,
}

/// 結果の説明。確認項目の文（Check）と、判定・部分ごとの状態（CheckView）をサーバーが組み合わせて送る。
/// 返す判定・状態は麦漆の値で、AIの文は explanation だけ。
pub fn explain(
    config: Option<&Config>,
    workspace: Workspace,
    request: ExplainRequest,
) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    let text = find_check(&workspace, &request.check_id)?.text.clone();
    let view = workspace::view(workspace);
    let check = view
        .checks
        .iter()
        .find(|c| c.check_id == request.check_id)
        .ok_or_else(|| invalid("確認項目の結果が見つかりません"))?;
    if check.coverage.aspects.is_empty() {
        return Err(invalid(
            "確かめる部分が登録されていないため、部分ごとの説明はできません",
        ));
    }
    let aspects: Vec<Value> = check
        .coverage
        .aspects
        .iter()
        .map(|a| {
            json!({"id": a.id, "text": a.text, "observation_label": a.observation_label,
                "status": a.status, "status_label": a.status_label, "detail": a.detail})
        })
        .collect();
    let payload = json!({
        "check_text": text,
        "verdict": check.verdict,
        "verification_passed": check.verification_passed,
        "coverage": {"status": check.coverage.status, "summary": check.coverage.summary, "aspects": aspects},
    });
    if let Some(preview) =
        preview_or_check("/v1/results/explain", &payload, request.confirm.as_ref())?
    {
        return Ok(preview);
    }
    let answer = config.post("/v1/results/explain", &payload)?;
    let written = answer["aspects"]
        .as_array()
        .ok_or_else(|| invalid("AIエージェントの説明の形が不正です"))?;
    let mut explained = vec![];
    for aspect in &check.coverage.aspects {
        let text = written
            .iter()
            .find(|w| w["id"] == aspect.id.as_str())
            .and_then(|w| w["explanation"].as_str())
            .ok_or_else(|| invalid("説明のない部分がある応答は受け取りません"))?;
        explained.push(json!({
            "id": aspect.id,
            "text": aspect.text,
            // 状態は麦漆の値。AIの応答に状態があっても使わない。
            "status": aspect.status,
            "status_label": aspect.status_label,
            "detail": aspect.detail,
            "explanation": text,
        }));
    }
    if written.len() != explained.len() {
        return Err(invalid(
            "麦漆の結果にない部分の説明を含む応答は受け取りません",
        ));
    }
    Ok(json!({
        "stage": "sent",
        "check_id": check.check_id,
        "verdict": check.verdict,
        "verification_passed": check.verification_passed,
        "coverage_status": check.coverage.status,
        "summary": answer["summary"],
        "aspects": explained,
        "next_steps": strings(&answer["next_steps"]),
        "model_call": answer["model_call"],
        "note": "判定と部分ごとの状態は麦漆の結果です。説明の文はAIによるもので、追加の確認ではありません。",
    }))
}

/// ① ガイドラインとの照合。画面から受け取るのは、人が書いたシステムの説明（架空のデータだけ）と送る前の確認だけ。
/// コード・実行記録・ログは自動では送らない。返すのは照合の候補で、適合・準拠・安全性の判断ではない。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanRequest {
    pub summary: String,
    #[serde(default)]
    pub confirm: Option<Confirm>,
}

const MAX_SUMMARY: usize = 8000;

pub fn scan(config: Option<&Config>, request: ScanRequest) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    let summary = request.summary.trim();
    if summary.is_empty() {
        return Err(invalid("照合するシステムの説明を書いてください"));
    }
    if summary.chars().count() > MAX_SUMMARY {
        return Err(invalid("システムの説明は8000文字以内にしてください"));
    }
    let payload = json!({"system": {"summary": summary, "excerpts": []}});
    if let Some(preview) = preview_or_check("/v1/scan", &payload, request.confirm.as_ref())? {
        return Ok(preview);
    }
    let answer = config.post("/v1/scan", &payload)?;
    scan_result(&answer)
}

/// 照合の応答を確かめて、麦漆の観測の種類の一覧で試験できるかを付け直す。
fn scan_result(answer: &Value) -> io::Result<Value> {
    let items = answer["findings"]
        .as_array()
        .ok_or_else(|| invalid("AIエージェントの照合の形が不正です"))?;
    let mut findings = vec![];
    for item in items {
        let requirement = item["requirement"].as_str().unwrap_or_default().trim();
        let citations = item["citations"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        // 出典は取り込んだ文書の断片だけ。文書・ページ・SHA-256のない引用を含む候補は受け取らない。
        let cited = citations.iter().all(|c| {
            c["document"].as_str().is_some_and(|d| !d.is_empty())
                && c["page"].as_str().is_some()
                && c["sha256"]
                    .as_str()
                    .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        });
        if requirement.is_empty() || citations.is_empty() || !cited {
            return Err(invalid(
                "要求の文か、文書・ページ・SHA-256のそろった出典がない照合の候補は受け取りません",
            ));
        }
        let method = item["verification"]["method"].as_str().unwrap_or_default();
        // 麦漆の試験で確かめられるかは、麦漆の観測の種類の一覧で決める。AIの応答の値は使わない。
        let kind: Option<ObservationKind> = serde_json::from_value(json!(method)).ok();
        let (support, method_label) = match kind {
            Some(k) => (Some(k.support()), k.label().to_owned()),
            None => (
                None,
                item["verification"]["method_label"]
                    .as_str()
                    .unwrap_or(method)
                    .to_owned(),
            ),
        };
        let executable = support == Some(crate::coverage::Support::Judged);
        findings.push(json!({
            "status": "candidate",
            "requirement": requirement,
            "relevance": item["relevance"],
            "citations": citations,
            "verification": {
                "method": method,
                "method_label": method_label,
                "observation": kind,
                "support": support,
                "support_note": match kind {
                    Some(k) => k.note(),
                    None => "人が確かめる方法。試験が通っても未確認のまま残る",
                },
                "executable_check": executable,
                "how": item["verification"]["how"],
            },
            "unknowns": strings(&item["unknowns"]),
            "patient_data": item["patient_data"].as_bool().unwrap_or(false),
            "priority": item["priority"]
                .as_str()
                .filter(|p| matches!(*p, "high" | "medium" | "low"))
                .unwrap_or("low"),
            "system_refs": strings(&item["system_refs"]),
        }));
    }
    let questions: Vec<Value> = answer["questions"]
        .as_array()
        .map(|q| {
            q.iter()
                .filter_map(|q| {
                    Some(json!({"id": q["id"].as_str()?, "text": q["text"].as_str()?,
                        "why": q["why"].as_str().unwrap_or(""), "options": strings(&q["options"])}))
                })
                .take(5)
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "stage": "sent",
        "status": "candidate",
        "limits": answer["limits"],
        "findings": findings,
        "questions": questions,
        "priority_rule": answer["priority_rule"],
        "not_covered": strings(&answer["not_covered"]),
        "sources": answer["sources"].as_array().cloned().unwrap_or_default(),
        "model_call": answer["model_call"],
        "note": "照合の候補です。適合・準拠・安全性の判断ではなく、確認項目にも判定にも使いません。該当するかと確かめ方は人が確かめます。",
    }))
}

/// ヒアリングの回答。照合と試験の案に使う。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HearingAnswer {
    pub question: String,
    pub answer: String,
}

const MAX_FILES: usize = 40;
const MAX_FILE_CHARS: usize = 20_000;
const MAX_TOTAL_CHARS: usize = 400_000;
const NO_SUMMARY: &str = "説明は書かれていない。コードの抜粋から読み取れる範囲で照合する。";

/// 利用者が選んだファイルを、固定コミットの読取記録から読んで抜粋にする。読むのは麦漆に保存した記録だけ。
fn excerpts(
    store: &crate::understanding::Store,
    repository: &str,
    commit: &str,
    files: &[String],
) -> io::Result<Vec<Value>> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err(invalid(format!(
            "照合に使うファイルを1〜{MAX_FILES}件選んでください"
        )));
    }
    let mut total = 0;
    let mut out = vec![];
    for path in files {
        let file = store.handle(crate::understanding::UnderstandRequest::File {
            repository: repository.to_owned(),
            commit: commit.to_owned(),
            path: path.clone(),
        })?;
        let text = file["file"]["text"].as_str().unwrap_or_default();
        let clipped: String = text.chars().take(MAX_FILE_CHARS).collect();
        let note = if clipped.chars().count() < text.chars().count() {
            format!("\n（{MAX_FILE_CHARS}文字で切った。残りは送っていない）")
        } else {
            String::new()
        };
        total += clipped.chars().count();
        if total > MAX_TOTAL_CHARS {
            return Err(invalid(
                "選んだファイルが多すぎます。照合に使うファイルを減らしてください",
            ));
        }
        out.push(json!({"ref": path, "text": clipped + &note}));
    }
    Ok(out)
}

fn system_of(
    store: &crate::understanding::Store,
    repository: &str,
    commit: &str,
    files: &[String],
    summary: &str,
) -> io::Result<Value> {
    let summary = summary.trim();
    if summary.chars().count() > MAX_SUMMARY {
        return Err(invalid("システムの説明は8000文字以内にしてください"));
    }
    Ok(json!({
        "summary": if summary.is_empty() { NO_SUMMARY } else { summary },
        "excerpts": excerpts(store, repository, commit, files)?,
    }))
}

/// 新しい流れの照合。選んだファイルの抜粋と、任意の説明・ヒアリングの回答を送る。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowScanRequest {
    pub repository: String,
    pub commit: String,
    pub files: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub answers: Vec<HearingAnswer>,
    #[serde(default)]
    pub confirm: Option<Confirm>,
}

pub fn flow_scan(
    config: Option<&Config>,
    store: &crate::understanding::Store,
    request: FlowScanRequest,
) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    let system = system_of(
        store,
        &request.repository,
        &request.commit,
        &request.files,
        &request.summary,
    )?;
    let payload = json!({"system": system, "answers": request.answers});
    if let Some(preview) = preview_or_check("/v1/scan", &payload, request.confirm.as_ref())? {
        return Ok(preview);
    }
    scan_result(&config.post("/v1/scan", &payload)?)
}

/// 利用者からの質問。取り込んだ文書だけを根拠に答える。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskRequest {
    pub question: String,
    #[serde(default)]
    pub confirm: Option<Confirm>,
}

pub fn ask(config: Option<&Config>, request: AskRequest) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    let question = request.question.trim();
    if question.is_empty() || question.chars().count() > MAX_REQUEST {
        return Err(invalid("質問は1〜2000文字で書いてください"));
    }
    let payload = json!({"question": question});
    if let Some(preview) = preview_or_check("/v1/ask", &payload, request.confirm.as_ref())? {
        return Ok(preview);
    }
    let answer = config.post("/v1/ask", &payload)?;
    let citations = answer["citations"].as_array().cloned().unwrap_or_default();
    if !citations.iter().all(cited) {
        return Err(invalid(
            "文書・ページ・SHA-256のそろわない出典を含む回答は受け取りません",
        ));
    }
    Ok(json!({
        "stage": "sent",
        "answer": answer["answer"].as_str().unwrap_or_default(),
        "citations": citations,
        "grounded": !citations.is_empty(),
        "model_call": answer["model_call"],
    }))
}

fn cited(c: &Value) -> bool {
    c["document"].as_str().is_some_and(|d| !d.is_empty())
        && c["page"].as_str().is_some()
        && c["sha256"]
            .as_str()
            .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// 試験の案に渡す照合の候補。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanFinding {
    pub id: String,
    pub requirement: String,
    #[serde(default)]
    pub how: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRequest {
    pub repository: String,
    pub commit: String,
    pub files: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub answers: Vec<HearingAnswer>,
    pub findings: Vec<PlanFinding>,
    #[serde(default)]
    pub confirm: Option<Confirm>,
}

/// 試験の案。人が了承するまで実行されない。観測の種類は麦漆で判定に使える種類だけを受け取る。
pub fn plan(
    config: Option<&Config>,
    store: &crate::understanding::Store,
    request: PlanRequest,
) -> io::Result<Value> {
    let config = config.ok_or_else(|| invalid("AIエージェントは設定されていません"))?;
    if request.findings.is_empty() || request.findings.len() > 24 {
        return Err(invalid("試験の案を作る候補を1〜24件選んでください"));
    }
    let system = system_of(
        store,
        &request.repository,
        &request.commit,
        &request.files,
        &request.summary,
    )?;
    let payload =
        json!({"system": system, "findings": request.findings, "answers": request.answers});
    if let Some(preview) = preview_or_check("/v1/plan", &payload, request.confirm.as_ref())? {
        return Ok(preview);
    }
    let answer = config.post("/v1/plan", &payload)?;
    let known: std::collections::BTreeSet<&str> =
        request.findings.iter().map(|f| f.id.as_str()).collect();
    let mut used = std::collections::BTreeSet::new();
    let mut tests = vec![];
    for t in answer["tests"]
        .as_array()
        .ok_or_else(|| invalid("AIエージェントの試験の案の形が不正です"))?
    {
        let ids = strings(&t["finding_ids"]);
        if ids.is_empty()
            || ids
                .iter()
                .any(|i| !known.contains(i.as_str()) || !used.insert(i.clone()))
        {
            return Err(invalid(
                "候補のない試験、渡していない候補、または二つの試験に入った候補を含む案は受け取りません",
            ));
        }
        let observation: ObservationKind = serde_json::from_value(t["observation"].clone())
            .map_err(|_| invalid("麦漆にない観測の種類を含む試験の案は受け取りません"))?;
        if observation.support() != crate::coverage::Support::Judged {
            return Err(invalid(
                "判定に使えない観測の種類を含む試験の案は受け取りません",
            ));
        }
        let (steps, pass) = (strings(&t["steps"]), strings(&t["pass"]));
        if steps.is_empty() || pass.is_empty() {
            return Err(invalid("手順か合格の条件のない試験の案は受け取りません"));
        }
        tests.push(json!({
            "id": t["id"], "name": t["name"], "kind": t["kind"], "finding_ids": ids,
            "steps": steps, "pass": pass, "observation": observation,
            "observation_label": observation.label(),
        }));
    }
    Ok(json!({
        "stage": "sent",
        "status": "draft",
        "tests": tests,
        "not_planned": answer["not_planned"].as_array().cloned().unwrap_or_default(),
        "model_call": answer["model_call"],
    }))
}

const FLOW_FILE: &str = "flow.json";
const MAX_FLOW_BYTES: usize = 2 * 1024 * 1024;

/// 新しい流れの途中の記録（照合の結果・ヒアリング・試験の案・了承した試験・結果の控え）。
/// 判定と観測の状態は事例の記録が正で、ここには画面の流れを続けるための控えだけを置く。
pub fn flow_read(directory: &std::path::Path) -> io::Result<Value> {
    match std::fs::read_to_string(directory.join(FLOW_FILE)) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| invalid("流れの記録を読み取れません")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(json!({"records": {}})),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSave {
    pub repository: String,
    pub commit: String,
    pub record: Value,
}

pub fn flow_save(directory: &std::path::Path, request: FlowSave) -> io::Result<Value> {
    if !request.record.is_object()
        || request.commit.len() != 40
        || !request.commit.bytes().all(|b| b.is_ascii_hexdigit())
        || request.repository.is_empty()
    {
        return Err(invalid("流れの記録の形が不正です"));
    }
    let mut all = flow_read(directory)?;
    let key = format!("{}@{}", request.repository, request.commit);
    all["records"][key] = json!({"repository": request.repository, "commit": request.commit, "record": request.record});
    let bytes = serde_json::to_vec(&all).map_err(io::Error::other)?;
    if bytes.len() > MAX_FLOW_BYTES {
        return Err(invalid("流れの記録が2MiBを超えます"));
    }
    let path = directory.join(FLOW_FILE);
    let temporary = directory.join(format!(".{FLOW_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&temporary, &bytes)?;
    std::fs::rename(&temporary, &path)?;
    Ok(all)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str) -> io::Result<Config> {
        Config::new(url, PathBuf::from("unused"))
    }

    /// 共有トークンを平文で外へ送らない。接続先はURLとして解析したスキームとホストで判定する。
    #[test]
    fn plain_http_is_allowed_only_to_loopback_hosts_parsed_as_urls() {
        for ok in [
            "https://agent.example/",
            "https://agent.example/base",
            "http://127.0.0.1:8787",
            "http://127.0.0.1:8787/",
            "http://localhost:8787",
            "http://[::1]:8787",
        ] {
            assert!(config(ok).is_ok(), "{ok}");
        }
        for bad in [
            // 接頭辞は 127.0.0.1・localhost でも、標準のURLの接続先は outside.example。
            "http://localhost:80@outside.example",
            "http://127.0.0.1:8787@outside.example/",
            "http://user:pass@127.0.0.1:8787",
            "https://user@agent.example",
            "http://outside.example",
            "http://127.0.0.1.outside.example",
            "http://localhost.outside.example:8787",
            "https://agent.example/?x=1",
            "https://agent.example/#x",
            "ftp://agent.example",
            "agent.example",
            "",
        ] {
            assert!(config(bad).is_err(), "{bad}");
        }
        assert_eq!(
            config("http://127.0.0.1:8787/").unwrap().base_url,
            "http://127.0.0.1:8787"
        );
    }

    #[test]
    fn sending_requires_the_confirmed_hash_and_the_synthetic_data_confirmation() {
        let payload = json!({"concern": "架空の確認項目"});
        let preview = preview_or_check("/v1/x", &payload, None).unwrap().unwrap();
        assert_eq!(preview["stage"], "preview");
        assert_eq!(preview["payload"], payload);
        let sha = preview["payload_sha256"].as_str().unwrap().to_owned();
        let confirm = |sha: &str, synthetic_only| Confirm {
            payload_sha256: sha.into(),
            synthetic_only,
        };
        assert!(
            preview_or_check("/v1/x", &payload, Some(&confirm(&sha, true)))
                .unwrap()
                .is_none()
        );
        assert!(preview_or_check("/v1/x", &payload, Some(&confirm(&sha, false))).is_err());
        let changed = json!({"concern": "確認の後に変わった"});
        assert!(preview_or_check("/v1/x", &changed, Some(&confirm(&sha, true))).is_err());
    }
}
