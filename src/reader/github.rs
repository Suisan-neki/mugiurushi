//! GitHub以外へ転送せず、本文を上限まで読み取る。対象のコードは実行しない。
use super::{
    invalid,
    snapshot::{self, Entry, MAX_FILE_BYTES, MAX_FILES, Omitted, Snapshot},
};
use serde_json::{Map, Value};
use std::{
    io::{self, Read},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub(super) trait Fetch: Sync {
    fn get(&self, url: &str, limit: usize) -> io::Result<Vec<u8>>;
}
pub(super) struct Github {
    agent: ureq::Agent,
    token: Option<String>,
    deadline: Instant,
}
impl Github {
    pub fn new(deadline: Instant) -> io::Result<Self> {
        Ok(Self {
            agent: ureq::Agent::config_builder()
                .proxy(proxy_from_env()?)
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .build(),
                )
                .https_only(true)
                .max_redirects(0)
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(20)))
                .build()
                .into(),
            token: super::github_token()?,
            deadline,
        })
    }
}
// 従来の読取経路と同じHTTP(S)プロキシを優先する。ALL_PROXYのSOCKSへは切り替えない。
fn proxy_from_env() -> io::Result<Option<ureq::Proxy>> {
    let value = ["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"]
        .into_iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()));
    let Some(value) = value else { return Ok(None) };
    let p = ureq::Proxy::new(&value)
        .map_err(|_| invalid("HTTP(S)プロキシの設定を確認してください。"))?;
    if !matches!(
        p.protocol(),
        ureq::ProxyProtocol::Http | ureq::ProxyProtocol::Https
    ) {
        return Err(invalid("コード読取はHTTP(S)プロキシに対応しています。"));
    }
    let mut builder = ureq::Proxy::builder(p.protocol())
        .host(p.host())
        .port(p.port());
    if let Some(u) = p.username() {
        builder = builder.username(u);
    }
    if let Some(p) = p.password() {
        builder = builder.password(p);
    }
    if let Some(exclusions) = ["no_proxy", "NO_PROXY"]
        .into_iter()
        .find_map(|k| std::env::var(k).ok())
    {
        for part in exclusions.split(',') {
            builder = builder.no_proxy(part.trim());
        }
    }
    builder
        .build()
        .map(Some)
        .map_err(|_| invalid("HTTP(S)プロキシの設定を確認してください。"))
}
impl Fetch for Github {
    fn get(&self, url: &str, limit: usize) -> io::Result<Vec<u8>> {
        let u = url::Url::parse(url).map_err(|_| invalid("読取先のURLが不正です。"))?;
        if u.scheme() != "https"
            || !matches!(
                u.host_str(),
                Some("api.github.com" | "raw.githubusercontent.com")
            )
            || u.port().is_some()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.fragment().is_some()
        {
            return Err(invalid("GitHub以外の接続先は読み取りません。"));
        }
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "コード読取が30秒の待機上限に達しました",
                )
            })?;
        let mut req = self.agent.get(url).header("User-Agent", "mugiurushi");
        if u.host_str() == Some("api.github.com") {
            req = req.header("Accept", "application/vnd.github+json");
            if let Some(t) = &self.token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
        }
        // 通信エラーにURL、ヘッダー、トークンを含めない。
        let mut r = req
            .config()
            .timeout_global(Some(remaining.min(Duration::from_secs(20))))
            .build()
            .call()
            .map_err(|_| {
                invalid("GitHubに接続できませんでした。ネットワークを確認して再実行してください。")
            })?;
        status(r.status().as_u16())?;
        bounded(r.body_mut().as_reader(), limit)
    }
}
pub(super) fn status(n: u16) -> io::Result<()> {
    match n {
        200..=299 => Ok(()),
        403 | 429 => Err(invalid(
            "GitHubの公開APIの利用上限、またはアクセス制限に達しました。時間を置いて再実行してください。",
        )),
        404 => Err(invalid(
            "公開リポジトリまたは指定した版が見つかりません。URLとブランチ名を確認してください。",
        )),
        _ => Err(invalid(format!(
            "GitHubから取得できませんでした（HTTP {n}）。"
        ))),
    }
}
pub(super) fn bounded(reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("GitHubから本文を読み取れませんでした。"))?;
    if bytes.len() > limit {
        return Err(invalid("読取結果のサイズ上限に達しました。"));
    }
    Ok(bytes)
}
fn json(f: &impl Fetch, url: &str) -> io::Result<Value> {
    serde_json::from_slice(&f.get(url, 16 * 1024 * 1024)?)
        .map_err(|_| invalid("GitHubの応答をJSONとして読み取れませんでした。"))
}
fn encoded(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
pub(super) fn text(f: &impl Fetch, s: &Snapshot, path: &str) -> io::Result<String> {
    let e = s
        .inventory
        .iter()
        .find(|x| x.path == path)
        .filter(|e| matches!(e.mode.as_str(), "100644" | "100755"))
        .ok_or_else(|| invalid("このファイルはコードの読取対象ではありません。"))?;
    if e.size.is_some_and(|n| n > MAX_FILE_BYTES) {
        return Err(invalid(
            "128KiBを超えるファイルはこの試作の読取範囲外です。GitHubで確認してください。",
        ));
    }
    let p = path.split('/').map(encoded).collect::<Vec<_>>().join("/");
    let bytes = f.get(
        &format!(
            "https://raw.githubusercontent.com/{}/{}/{p}",
            s.full_name, s.commit
        ),
        MAX_FILE_BYTES,
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|_| invalid("UTF-8のテキストとして読み取れませんでした。"))?;
    if text.contains('\0') {
        return Err(invalid("バイナリファイルは読み取りません。"));
    }
    Ok(text)
}
pub(super) fn file(f: &impl Fetch, s: &mut Snapshot, path: &str) -> io::Result<()> {
    let text = text(f, s, path)?;
    s.files.insert(path.to_owned(), Value::String(text));
    s.omitted.retain(|e| e.path != path);
    Ok(())
}
/// 公開リポジトリの既定のブランチと、ブランチの一覧（名前と最新のコミット）。最初の100件まで。
pub(super) fn branches(f: &impl Fetch, url: &str) -> io::Result<(String, Vec<(String, String)>)> {
    let name = snapshot::repository(url)?;
    let api = format!("https://api.github.com/repos/{name}");
    let repo = json(f, &api)?;
    if repo["private"] != false {
        return Err(invalid("最初の試作は公開リポジトリに対応しています。"));
    }
    let default = repo["default_branch"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let list = json(f, &format!("{api}/branches?per_page=100"))?;
    let branches = list
        .as_array()
        .ok_or_else(|| invalid("GitHubのブランチ一覧を読み取れませんでした。"))?
        .iter()
        .filter_map(|b| {
            let name = b["name"].as_str()?;
            let sha = b["commit"]["sha"].as_str().filter(|s| snapshot::sha(s))?;
            Some((name.to_owned(), sha.to_owned()))
        })
        .collect();
    Ok((default, branches))
}

pub(super) fn scan(f: &impl Fetch, url: &str, reference: &str) -> io::Result<Snapshot> {
    let name = snapshot::repository(url)?;
    let api = format!("https://api.github.com/repos/{name}");
    let repo = json(f, &api)?;
    if repo["private"] != false {
        return Err(invalid("最初の試作は公開リポジトリに対応しています。"));
    }
    let reference = if reference.trim().is_empty() {
        repo["default_branch"]
            .as_str()
            .ok_or_else(|| invalid("既定のブランチを読み取れませんでした。"))?
    } else {
        reference.trim()
    };
    let commit = json(f, &format!("{api}/commits/{}", encoded(reference)))?;
    let sha = commit["sha"]
        .as_str()
        .filter(|s| snapshot::sha(s))
        .ok_or_else(|| invalid("GitHubのコミット情報を読み取れませんでした。"))?;
    let tree = commit["commit"]["tree"]["sha"]
        .as_str()
        .filter(|s| snapshot::sha(s))
        .ok_or_else(|| invalid("GitHubのコミット情報を読み取れませんでした。"))?;
    let tree = json(f, &format!("{api}/git/trees/{tree}?recursive=1"))?;
    let entries = tree["tree"]
        .as_array()
        .ok_or_else(|| invalid("GitHubのファイル一覧を読み取れませんでした。"))?;
    let inventory = entries
        .iter()
        .filter(|e| e["type"] == "blob")
        .map(|v| {
            serde_json::from_value::<Entry>(v.clone())
                .map_err(|_| invalid("GitHubのファイル情報が不正です。"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    let mut s = Snapshot {
        schema: 1,
        full_name: name,
        description: Some(repo["description"].as_str().unwrap_or("").to_owned()),
        reference: reference.to_owned(),
        commit: sha.to_owned(),
        scanned_at: Some(timestamp()?),
        truncated: tree["truncated"] == true,
        inventory,
        files: Map::new(),
        omitted: vec![],
    }
    .validate()?;
    let mut candidates = s
        .inventory
        .iter()
        .filter(|e| snapshot::readable(&e.path) && matches!(e.mode.as_str(), "100644" | "100755"))
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        snapshot::priority(&a.path)
            .cmp(&snapshot::priority(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    let selected = candidates
        .iter()
        .filter(|e| e.size.is_none_or(|n| n <= MAX_FILE_BYTES))
        .take(MAX_FILES)
        .map(|e| e.path.clone())
        .collect::<Vec<_>>();
    s.omitted = candidates
        .iter()
        .filter(|e| !selected.contains(&e.path))
        .map(|e| Omitted {
            path: e.path.clone(),
            reason: if e.size.is_some_and(|n| n > MAX_FILE_BYTES) {
                "サイズ上限"
            } else {
                "初回の読取範囲外"
            }
            .to_owned(),
        })
        .collect();
    // 最大4並列。スレッドは同じ待機期限とHTTPクライアントを共有する。
    // 本文の取得後に順序をそろえ、JSONの配列の順序を通信速度に依存させない。
    let results = std::thread::scope(|scope| -> io::Result<Vec<_>> {
        let mut workers = vec![];
        for index in 0..selected.len().min(4) {
            let snapshot = &s;
            let selected = &selected;
            workers.push(scope.spawn(move || {
                selected
                    .iter()
                    .enumerate()
                    .skip(index)
                    .step_by(4)
                    .map(|(n, p)| (n, p.to_owned(), text(f, snapshot, p)))
                    .collect::<Vec<_>>()
            }));
        }
        let mut results = vec![];
        for worker in workers {
            results.extend(
                worker
                    .join()
                    .map_err(|_| io::Error::other("GitHub本文の読取処理が終了しました。"))?,
            );
        }
        results.sort_by_key(|(n, _, _)| *n);
        Ok(results)
    })?;
    for (_, path, result) in results {
        match result {
            Ok(text) => {
                s.files.insert(path, Value::String(text));
            }
            Err(e) => s.omitted.push(Omitted {
                path,
                reason: e.to_string(),
            }),
        }
    }

    Ok(s)
}
/// 既存の固定版を保って構成に必要な説明・設定・代表的なコードを補う。
/// 初回の辞書順で一つのディレクトリだけに偏らないよう、設定と起動・接続を先に読む。
pub(super) fn architecture(f: &impl Fetch, s: &mut Snapshot) -> io::Result<()> {
    let mut candidates: Vec<_> = s
        .inventory
        .iter()
        .filter(|e| {
            !s.files.contains_key(&e.path)
                && matches!(e.mode.as_str(), "100644" | "100755")
                && e.size.is_none_or(|n| n <= MAX_FILE_BYTES)
        })
        .filter_map(|e| {
            snapshot::architecture_priority(&e.path)
                .map(|priority| (priority, e.path.matches('/').count(), e.path.clone()))
        })
        .collect();
    candidates.sort();
    let mut size: usize = s
        .files
        .values()
        .filter_map(Value::as_str)
        .map(str::len)
        .sum();
    let selected: Vec<_> = candidates
        .into_iter()
        .take(48)
        .filter_map(|(_, _, path)| {
            let estimate = s
                .inventory
                .iter()
                .find(|e| e.path == path)
                .and_then(|e| e.size)
                .unwrap_or(MAX_FILE_BYTES);
            if size + estimate > 6 * 1024 * 1024 {
                None
            } else {
                size += estimate;
                Some(path)
            }
        })
        .collect();
    let results = std::thread::scope(|scope| -> io::Result<Vec<_>> {
        let mut workers = vec![];
        for offset in 0..selected.len().min(4) {
            let selected = &selected;
            let snapshot = &*s;
            workers.push(scope.spawn(move || {
                selected
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .step_by(4)
                    .map(|(i, path)| (i, path.clone(), text(f, snapshot, path)))
                    .collect::<Vec<_>>()
            }));
        }
        let mut results = vec![];
        for worker in workers {
            results.extend(
                worker
                    .join()
                    .map_err(|_| io::Error::other("構成の読取処理が終了しました"))?,
            );
        }
        results.sort_by_key(|(i, _, _)| *i);
        Ok(results)
    })?;
    for (_, path, result) in results {
        match result {
            Ok(text) => {
                s.omitted.retain(|e| e.path != path);
                s.files.insert(path, Value::String(text));
            }
            Err(e) => {
                s.omitted.retain(|e| e.path != path);
                s.omitted.push(Omitted {
                    path,
                    reason: e.to_string(),
                });
            }
        }
    }
    Ok(())
}
fn timestamp() -> io::Result<String> {
    // UTCの暦日へ変換する。対象コードの実行や外部コマンドは使わない。
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let secs = t.as_secs();
    let days = (secs / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + i64::from(m <= 2);
    Ok(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60,
        t.subsec_millis()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;
    struct Fake {
        requests: Mutex<Vec<String>>,
        mode: &'static str,
    }
    impl Fetch for Fake {
        fn get(&self, url: &str, limit: usize) -> io::Result<Vec<u8>> {
            self.requests.lock().unwrap().push(url.to_owned());
            if self.mode == "rate" {
                return status(429).map(|_| vec![]);
            }
            let data = if url == "https://api.github.com/repos/example/project" {
                json!({"private":self.mode=="private","default_branch":"main"})
            } else if url.ends_with("/commits/feature%2Fread") {
                json!({"sha":"a".repeat(40),"commit":{"tree":{"sha":"b".repeat(40)}}})
            } else if url.ends_with(&format!("/git/trees/{}?recursive=1", "b".repeat(40))) {
                let mut entries=(0..30).map(|n|json!({"path":format!("src/{n:02}.rs"),"type":"blob","size":20,"mode":"100644"})).collect::<Vec<_>>();
                entries.extend([
                    json!({"path":"huge.rs","type":"blob","size":MAX_FILE_BYTES+1,"mode":"100644"}),
                    json!({"path":"link.rs","type":"blob","size":10,"mode":"120000"}),
                    json!({"path":"README.md","type":"blob","size":20,"mode":"100644"}),
                    json!({"path":"src","type":"tree","mode":"040000"}),
                ]);
                json!({"tree":entries,"truncated":true})
            } else if url.starts_with(&format!(
                "https://raw.githubusercontent.com/example/project/{}/",
                "a".repeat(40)
            )) {
                let b = match self.mode {
                    "invalid_utf8" => vec![0xff],
                    "binary" => b"x\0y".to_vec(),
                    "oversize" => vec![b'x'; MAX_FILE_BYTES + 1],
                    _ => b"fn main() {}\n".to_vec(),
                };
                return bounded(b.as_slice(), limit);
            } else {
                panic!("unexpected URL: {url}")
            };
            bounded(serde_json::to_vec(&data).unwrap().as_slice(), limit)
        }
    }
    fn fake(mode: &'static str) -> Fake {
        Fake {
            requests: Mutex::new(vec![]),
            mode,
        }
    }
    #[test]
    fn fixed_commit_tree_limits_and_public_only_reading() {
        let f = fake("ok");
        let s = scan(&f, "https://github.com/example/project.git", "feature/read").unwrap();
        assert_eq!(s.commit, "a".repeat(40));
        assert!(s.truncated);
        assert_eq!(s.files.len(), MAX_FILES);
        assert_eq!(s.inventory.len(), 33);
        assert!(s.files.contains_key("README.md"));
        assert!(
            s.omitted
                .iter()
                .any(|e| e.path == "huge.rs" && e.reason == "サイズ上限")
        );
        assert!(!s.files.contains_key("link.rs"));
        assert!(!s.omitted.iter().any(|e| e.path == "link.rs"));
        let requests = f.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|u| u.ends_with("/commits/feature%2Fread"))
        );
        assert_eq!(requests.len(), 3 + MAX_FILES);
        assert!(
            scan(
                &fake("private"),
                "https://github.com/example/project",
                "feature/read"
            )
            .is_err()
        );
        assert!(
            scan(&fake("rate"), "https://github.com/example/project", "main")
                .unwrap_err()
                .to_string()
                .contains("利用上限")
        );
    }
    #[test]
    fn failed_source_is_omitted_and_not_replaced_with_success() {
        for mode in ["invalid_utf8", "binary", "oversize"] {
            let s = scan(
                &fake(mode),
                "https://github.com/example/project",
                "feature/read",
            )
            .unwrap();
            assert!(s.files.is_empty());
            assert_eq!(s.omitted.len(), 32);
            assert!(
                s.omitted
                    .iter()
                    .any(|e| e.path == "README.md" && !e.reason.is_empty())
            );
        }
        assert_eq!(bounded(&b"1234"[..], 4).unwrap(), b"1234");
        assert!(bounded(&b"12345"[..], 4).is_err());
    }
    #[test]
    fn disallowed_hosts_and_expired_deadlines_are_rejected_before_connecting() {
        let client = Github {
            agent: ureq::Agent::config_builder()
                .max_redirects(0)
                .build()
                .into(),
            token: Some("synthetic-secret".into()),
            deadline: Instant::now() - Duration::from_secs(1),
        };
        for url in [
            "http://api.github.com/a",
            "https://evil.example/a",
            "https://api.github.com.evil.example/a",
            "https://user:secret@api.github.com/a",
            "https://api.github.com:8443/a",
            "https://api.github.com/a#fragment",
        ] {
            let e = client.get(url, 20).unwrap_err().to_string();
            assert!(e.contains("GitHub以外"));
            assert!(!e.contains("synthetic-secret"));
        }
        assert_eq!(
            client
                .get("https://api.github.com/a", 20)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        for code in [301, 302, 307, 401, 403, 404, 429, 500] {
            let error = status(code).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
    }
}
