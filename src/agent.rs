//! 運営者が設定した AIエージェント（mugiurushi-ai）を呼ぶ。
//! 接続先と共有トークンはサーバー側の設定だけで、画面からは入力させない。AIが未設定・失敗しても、
//! 確認項目・試験・判定は今までどおり使える。AIの出力は人が採用するまで下書きで、判定には使わない。
//! 送るのは確認項目の文・部分の文と状態・説明だけで、実行記録の生ログ・画面写真・試験用の認証情報は送らない。
use crate::coverage::ObservationKind;
use crate::error::invalid;
use crate::workspace::{self, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
        let base_url = base_url.trim_end_matches('/').to_owned();
        // 共有トークンを平文で外へ送らない。http は同じ機械の中への接続だけに許す。
        let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
            .iter()
            .any(|p| base_url.starts_with(p));
        if !(base_url.starts_with("https://") || loopback) || base_url.contains('?') {
            return Err(invalid(
                "AIエージェントの接続先は https://… か、同じ機械の http://127.0.0.1:… にしてください",
            ));
        }
        Ok(Self {
            base_url,
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

/// 画面から受け取るのは確認項目のIDと、任意の追加の依頼だけ。送る内容はサーバーが事例から組み立てる。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeRequest {
    pub check_id: String,
    #[serde(default)]
    pub request: Option<String>,
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
    let system = match &check.source {
        Some(s) => format!(
            "確認する対象のコード: {}@{} の {}（{}〜{}行）",
            s.repository, s.commit, s.path, s.line, s.end_line
        ),
        None => "確認する対象のコードは未設定".into(),
    };
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
    let answer = config.post(
        "/v1/dialogue/turn",
        &json!({"system": system, "concern": check.text, "messages": messages, "current_aspects": current}),
    )?;
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
    Ok(json!({
        "status": "draft",
        "reply": answer["reply"],
        "questions": strings(&answer["questions"]),
        "proposals": proposals,
        "out_of_scope": answer["out_of_scope"],
        "model_call": answer["model_call"],
        "note": "AIの案です。採用した部分だけが確認項目に追加され、観測の種類と判定条件を人が選ぶまで未確認のままです。",
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplainRequest {
    pub check_id: String,
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
    let answer = config.post(
        "/v1/results/explain",
        &json!({
            "check_text": text,
            "verdict": check.verdict,
            "verification_passed": check.verification_passed,
            "coverage": {"status": check.coverage.status, "summary": check.coverage.summary, "aspects": aspects},
        }),
    )?;
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
