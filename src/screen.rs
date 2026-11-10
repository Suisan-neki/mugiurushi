//! ブラウザ試験の各手順が完了した時点の、画面の構造とHTTP応答のヘッダーの記録。
//! 操作器が取った記録を型と上限で検査してから保存する。入力欄の値、Cookieの値、本文は持たない。
//! コードの読解（推定）とは別の、実行時の観測として扱う。
use crate::error::invalid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
};

pub const RAW_FILE: &str = "screens-raw.json";
pub const FILE: &str = "screens.json";
pub const MAX_BYTES: u64 = 4 * 1024 * 1024 + 256 * 1024;
const MAX_SCREENS: usize = 96;
const MAX_FRAMES: usize = 32;
const MAX_FIELDS: usize = 300;
const MAX_CONTROLS: usize = 200;
const MAX_LINKS: usize = 200;
const MAX_HEADINGS: usize = 50;
const MAX_TEXT: usize = 256 * 4;
const MAX_HEADER: usize = 512 * 4;
pub const MAX_EXCHANGES: usize = 500;

/// 記録に残す応答ヘッダーの名前。ここにない名前は操作器から受け取らない。
pub const HEADER_NAMES: [&str; 7] = [
    "content-security-policy",
    "strict-transport-security",
    "x-frame-options",
    "x-content-type-options",
    "referrer-policy",
    "cache-control",
    "content-type",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CookieAttributes {
    pub name: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub cookies: Vec<CookieAttributes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldRecord {
    pub tag: String,
    pub input_type: Option<String>,
    pub name: Option<String>,
    pub id: Option<String>,
    pub label: Option<String>,
    pub required: bool,
    pub readonly: bool,
    pub disabled: bool,
    pub autocomplete: Option<String>,
    pub maxlength: Option<u32>,
    pub form_action: Option<String>,
    pub form_method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRecord {
    pub tag: String,
    pub input_type: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameRecord {
    /// 最上位から順の、iframeの位置（`name=…`、`id=…`、`src=…`）。最上位の文書は空。
    pub chain: Vec<String>,
    pub path: String,
    pub headings: Vec<String>,
    pub fields: Vec<FieldRecord>,
    pub controls: Vec<ControlRecord>,
    /// 同じオリジンへのリンクのパス。
    pub links: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenContent {
    pub path: String,
    pub title: String,
    pub frames: Vec<FrameRecord>,
    /// 最上位の文書を最後に読み込んだときの応答。観測していなければ `None`。
    pub document: Option<DocumentResponse>,
    /// 上限を超えて一部を捨てた。
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenRecord {
    pub step: String,
    pub phase: String,
    /// 内容が直前の記録と同じ場合の、その記録の `<段階>:<手順>`。
    pub same_as: Option<String>,
    pub content: Option<ScreenContent>,
    /// 内容のSHA-256。操作器の値は使わず、基盤が計算する。
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenFile {
    pub schema_version: u32,
    pub run_id: String,
    pub screens: Vec<ScreenRecord>,
    #[serde(default)]
    pub truncated: bool,
}

/// 試行の記録に残す、画面の記録への参照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenRef {
    pub step: String,
    pub phase: String,
    pub sha256: String,
}

/// 操作中のHTTPのやり取り1件。本文とクエリ文字列は持たない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeRecord {
    pub step: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub resource: String,
    pub headers: BTreeMap<String, String>,
    pub cookies: Vec<CookieAttributes>,
}

fn short(value: &str, max: usize) -> io::Result<()> {
    if value.len() > max || value.contains('\0') {
        return Err(invalid("画面の記録の文字列が上限を超えています"));
    }
    Ok(())
}

fn optional(value: &Option<String>) -> io::Result<()> {
    value.as_deref().map_or(Ok(()), |v| short(v, MAX_TEXT))
}

pub fn validate_headers(headers: &BTreeMap<String, String>) -> io::Result<()> {
    for (name, value) in headers {
        if !HEADER_NAMES.contains(&name.as_str()) {
            return Err(invalid(format!(
                "記録の対象にしていない応答ヘッダー {name} を受け取りました"
            )));
        }
        short(value, MAX_HEADER)?;
    }
    Ok(())
}

fn validate_cookies(cookies: &[CookieAttributes]) -> io::Result<()> {
    if cookies.len() > 32 {
        return Err(invalid("Cookieの記録が上限を超えています"));
    }
    for cookie in cookies {
        short(&cookie.name, MAX_TEXT)?;
        optional(&cookie.same_site)?;
        optional(&cookie.path)?;
    }
    Ok(())
}

fn validate_content(content: &ScreenContent) -> io::Result<()> {
    short(&content.path, MAX_TEXT)?;
    short(&content.title, MAX_TEXT)?;
    if content.frames.len() > MAX_FRAMES {
        return Err(invalid("画面のフレームの数が上限を超えています"));
    }
    if let Some(document) = &content.document {
        validate_headers(&document.headers)?;
        validate_cookies(&document.cookies)?;
    }
    for frame in &content.frames {
        if frame.chain.len() > 8
            || frame.fields.len() > MAX_FIELDS
            || frame.controls.len() > MAX_CONTROLS
            || frame.links.len() > MAX_LINKS
            || frame.headings.len() > MAX_HEADINGS
        {
            return Err(invalid("画面の要素の数が上限を超えています"));
        }
        short(&frame.path, MAX_TEXT)?;
        for text in frame
            .chain
            .iter()
            .chain(&frame.headings)
            .chain(&frame.links)
        {
            short(text, MAX_TEXT)?;
        }
        for field in &frame.fields {
            short(&field.tag, 32)?;
            for value in [
                &field.input_type,
                &field.name,
                &field.id,
                &field.label,
                &field.autocomplete,
                &field.form_action,
                &field.form_method,
            ] {
                optional(value)?;
            }
        }
        for control in &frame.controls {
            short(&control.tag, 32)?;
            optional(&control.input_type)?;
            short(&control.text, MAX_TEXT)?;
        }
    }
    Ok(())
}

pub fn validate_exchanges(
    exchanges: &[ExchangeRecord],
    action_steps: &BTreeSet<&str>,
) -> io::Result<()> {
    if exchanges.len() > MAX_EXCHANGES {
        return Err(invalid("HTTPのやり取りの記録が上限を超えています"));
    }
    for exchange in exchanges {
        if !action_steps.contains(exchange.step.as_str()) {
            return Err(invalid(
                "HTTPのやり取りが、操作の段階にない手順を指しています",
            ));
        }
        short(&exchange.method, 16)?;
        short(&exchange.path, 2048)?;
        short(&exchange.resource, 32)?;
        validate_headers(&exchange.headers)?;
        validate_cookies(&exchange.cookies)?;
    }
    Ok(())
}

/// 記録を検査し、基盤が内容のハッシュを計算して埋める。`steps` は試行で実行した `(段階, 手順)`。
pub fn validate(
    mut file: ScreenFile,
    run_id: &str,
    steps: &BTreeSet<(String, String)>,
) -> io::Result<ScreenFile> {
    if file.schema_version != 1 || file.run_id != run_id {
        return Err(invalid("画面の記録の試行が一致しません"));
    }
    if file.screens.len() > MAX_SCREENS {
        return Err(invalid("画面の記録の件数が上限を超えています"));
    }
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    for record in &mut file.screens {
        let key = format!("{}:{}", record.phase, record.step);
        if !steps.contains(&(record.phase.clone(), record.step.clone()))
            || hashes.contains_key(&key)
        {
            return Err(invalid(
                "画面の記録が、試行にない手順か重複した手順を指しています",
            ));
        }
        record.sha256 = match (&record.content, &record.same_as) {
            (Some(content), None) => {
                validate_content(content)?;
                format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(content).map_err(io::Error::other)?)
                )
            }
            (None, Some(earlier)) => hashes
                .get(earlier)
                .cloned()
                .ok_or_else(|| invalid("画面の記録が、先行しない記録を参照しています"))?,
            _ => {
                return Err(invalid(
                    "画面の記録は、内容か先行する記録への参照のどちらか一方を持ちます",
                ));
            }
        };
        hashes.insert(key, record.sha256.clone());
    }
    Ok(file)
}

/// 操作器が書いた記録を読み、検査に通ったものだけを保存する。戻り値は参照の一覧と説明。
/// 検査に落ちた記録は保存しない。試行の判定は変えない。
pub fn adopt(
    directory: &Path,
    run_id: &str,
    steps: &BTreeSet<(String, String)>,
) -> (Vec<ScreenRef>, String) {
    let raw = directory.join(RAW_FILE);
    let result = (|| -> io::Result<ScreenFile> {
        let meta = fs::symlink_metadata(&raw)?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return Err(invalid(
                "画面の記録が通常のファイルではないか、上限を超えています",
            ));
        }
        let file: ScreenFile = crate::recording::read_json(&raw)?;
        let file = validate(file, run_id, steps)?;
        crate::recording::write_json_new(&directory.join(FILE), &file)?;
        Ok(file)
    })();
    let _ = fs::remove_file(&raw);
    match result {
        Ok(file) => {
            let refs: Vec<_> = file
                .screens
                .iter()
                .map(|s| ScreenRef {
                    step: s.step.clone(),
                    phase: s.phase.clone(),
                    sha256: s.sha256.clone(),
                })
                .collect();
            let note = format!(
                "手順が完了した時点の画面の構造を{}件記録した{}。入力欄の値は記録していない",
                refs.len(),
                if file.truncated
                    || file
                        .screens
                        .iter()
                        .any(|s| s.content.as_ref().is_some_and(|c| c.truncated))
                {
                    "（上限を超えた分は捨てた）"
                } else {
                    ""
                }
            );
            (refs, note)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            vec![],
            "操作器が画面の構造の記録を残さなかった。画面が空だったことは意味しない".into(),
        ),
        Err(error) => (
            vec![],
            format!("画面の構造の記録を採用できない: {error}。画面が空だったことは意味しない"),
        ),
    }
}

/// 保存済みの画面の記録を読む。記録のない試行は `None`。
pub fn load(directory: &Path) -> io::Result<Option<ScreenFile>> {
    let path = directory.join(FILE);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(meta) if !meta.is_file() || meta.len() > MAX_BYTES => {
            Err(invalid("保存済みの画面の記録を読めません"))
        }
        Ok(_) => crate::recording::read_json(&path).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(fields: usize) -> ScreenContent {
        ScreenContent {
            path: "/visit".into(),
            title: "架空の受診".into(),
            frames: vec![FrameRecord {
                chain: vec![],
                path: "/visit".into(),
                headings: vec!["受診".into()],
                fields: (0..fields)
                    .map(|i| FieldRecord {
                        tag: "input".into(),
                        input_type: Some("password".into()),
                        name: Some(format!("f{i}")),
                        id: None,
                        label: Some("暗証".into()),
                        required: true,
                        readonly: false,
                        disabled: false,
                        autocomplete: Some("off".into()),
                        maxlength: Some(8),
                        form_action: Some("/save".into()),
                        form_method: Some("post".into()),
                    })
                    .collect(),
                controls: vec![],
                links: vec!["/next".into()],
            }],
            document: Some(DocumentResponse {
                status: 200,
                headers: [("cache-control".to_owned(), "no-store".to_owned())].into(),
                cookies: vec![CookieAttributes {
                    name: "session".into(),
                    secure: true,
                    http_only: true,
                    same_site: Some("Strict".into()),
                    path: Some("/".into()),
                }],
            }),
            truncated: false,
        }
    }

    fn record(step: &str, content: Option<ScreenContent>, same_as: Option<&str>) -> ScreenRecord {
        ScreenRecord {
            step: step.into(),
            phase: "action".into(),
            same_as: same_as.map(String::from),
            content,
            sha256: "操作器が書いた値は使わない".into(),
        }
    }

    fn steps(ids: &[&str]) -> BTreeSet<(String, String)> {
        ids.iter()
            .map(|id| ("action".to_owned(), (*id).to_owned()))
            .collect()
    }

    fn file(screens: Vec<ScreenRecord>) -> ScreenFile {
        ScreenFile {
            schema_version: 1,
            run_id: "run-1".into(),
            screens,
            truncated: false,
        }
    }

    #[test]
    fn hashes_are_computed_here_and_repeated_screens_share_one() {
        let checked = validate(
            file(vec![
                record("open", Some(content(1)), None),
                record("wait", None, Some("action:open")),
                record("save", Some(content(2)), None),
            ]),
            "run-1",
            &steps(&["open", "wait", "save"]),
        )
        .unwrap();
        assert_eq!(checked.screens[0].sha256.len(), 64);
        assert_eq!(checked.screens[0].sha256, checked.screens[1].sha256);
        assert_ne!(checked.screens[0].sha256, checked.screens[2].sha256);
    }

    #[test]
    fn records_for_other_runs_unknown_steps_or_over_the_limits_are_rejected() {
        let ok = || file(vec![record("open", Some(content(1)), None)]);
        assert!(validate(ok(), "run-1", &steps(&["open"])).is_ok());
        assert!(validate(ok(), "run-2", &steps(&["open"])).is_err());
        assert!(validate(ok(), "run-1", &steps(&["other"])).is_err());
        let mut version = ok();
        version.schema_version = 2;
        assert!(validate(version, "run-1", &steps(&["open"])).is_err());
        // 同じ手順の重複、先行しない参照、内容と参照の両方・どちらもなし。
        for screens in [
            vec![
                record("open", Some(content(1)), None),
                record("open", Some(content(1)), None),
            ],
            vec![record("open", None, Some("action:later"))],
            vec![record("open", Some(content(1)), Some("action:open"))],
            vec![record("open", None, None)],
            vec![record("open", Some(content(MAX_FIELDS + 1)), None)],
        ] {
            assert!(validate(file(screens), "run-1", &steps(&["open", "later"])).is_err());
        }
        let mut header = content(1);
        header
            .document
            .as_mut()
            .unwrap()
            .headers
            .insert("authorization".into(), "x".into());
        assert!(
            validate(
                file(vec![record("open", Some(header), None)]),
                "run-1",
                &steps(&["open"])
            )
            .is_err()
        );
        let mut long = content(1);
        long.title = "あ".repeat(MAX_TEXT);
        assert!(
            validate(
                file(vec![record("open", Some(long), None)]),
                "run-1",
                &steps(&["open"])
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_fields_such_as_an_input_value_are_not_accepted() {
        let mut value =
            serde_json::to_value(file(vec![record("open", Some(content(1)), None)])).unwrap();
        value["screens"][0]["content"]["frames"][0]["fields"][0]["value"] = "入力した値".into();
        assert!(serde_json::from_value::<ScreenFile>(value).is_err());
    }

    #[test]
    fn a_rejected_or_missing_record_is_not_kept_and_is_not_an_empty_screen() {
        let dir = std::env::temp_dir().join(format!(
            "mugiurushi-screen-{}-{}",
            std::process::id(),
            crate::recording::unix_ms().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let (refs, note) = adopt(&dir, "run-1", &steps(&["open"]));
        assert!(refs.is_empty() && note.contains("画面が空だったことは意味しない"));
        assert!(load(&dir).unwrap().is_none());

        fs::write(
            dir.join(RAW_FILE),
            serde_json::to_vec(&file(vec![record("unknown", Some(content(1)), None)])).unwrap(),
        )
        .unwrap();
        let (refs, note) = adopt(&dir, "run-1", &steps(&["open"]));
        assert!(refs.is_empty() && note.contains("採用できない"), "{note}");
        assert!(!dir.join(RAW_FILE).exists() && !dir.join(FILE).exists());

        fs::write(
            dir.join(RAW_FILE),
            serde_json::to_vec(&file(vec![record("open", Some(content(1)), None)])).unwrap(),
        )
        .unwrap();
        let (refs, note) = adopt(&dir, "run-1", &steps(&["open"]));
        assert_eq!(refs.len(), 1);
        assert!(note.contains("1件記録"));
        let saved = load(&dir).unwrap().unwrap();
        assert_eq!(saved.screens[0].sha256, refs[0].sha256);
        assert!(!dir.join(RAW_FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exchanges_must_belong_to_action_steps_and_carry_only_listed_headers() {
        let exchange = |step: &str, header: &str| ExchangeRecord {
            step: step.into(),
            method: "POST".into(),
            path: "/save".into(),
            status: 200,
            resource: "document".into(),
            headers: [(header.to_owned(), "x".to_owned())].into(),
            cookies: vec![],
        };
        let actions: BTreeSet<&str> = ["save"].into();
        assert!(validate_exchanges(&[exchange("save", "content-type")], &actions).is_ok());
        assert!(validate_exchanges(&[exchange("login", "content-type")], &actions).is_err());
        assert!(validate_exchanges(&[exchange("save", "set-cookie")], &actions).is_err());
        let many = vec![exchange("save", "content-type"); MAX_EXCHANGES + 1];
        assert!(validate_exchanges(&many, &actions).is_err());
    }
}
