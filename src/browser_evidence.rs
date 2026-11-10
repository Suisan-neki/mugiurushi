//! 宣言済みの試行・手順の画面写真だけを読む。任意のパスはHTTPで受け付けない。
use crate::{browser::BrowserReport, execution::ExecutionTrace};
use std::{
    fs,
    io::{self, Read},
    path::Path,
};
const MAX_PNG: u64 = 4 * 1024 * 1024;
fn missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "この事例内に対応する画面写真がありません。取り込み元の試行や復元時の有無を確認してください",
    )
}
fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// バックアップに含めない実行フォルダーを参照する。写真の欠落で判定は変えない。
pub fn screenshot(directory: &Path, trace: &ExecutionTrace, step_id: &str) -> io::Result<Vec<u8>> {
    let report = trace
        .containers
        .as_ref()
        .and_then(|c| c.browser.as_ref())
        .ok_or_else(missing)?;
    read(directory, &trace.correlation.run_id, report, step_id)
}
/// 事例の試行が残した画面の構造の記録を読む。試行のフォルダーの外やリンクの先は読まない。
pub fn screens(directory: &Path, trace: &ExecutionTrace) -> io::Result<crate::screen::ScreenFile> {
    let report = trace
        .containers
        .as_ref()
        .and_then(|c| c.browser.as_ref())
        .ok_or_else(no_screens)?;
    read_screens(directory, &trace.correlation.run_id, report)
}
fn no_screens() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "この試行には画面の構造の記録がありません。画面が空だったことは意味しません",
    )
}
fn read_screens(
    directory: &Path,
    run_id: &str,
    report: &BrowserReport,
) -> io::Result<crate::screen::ScreenFile> {
    if !component(run_id) || report.run_id != run_id || report.screens.is_empty() {
        return Err(no_screens());
    }
    let mut path = directory.to_path_buf();
    for part in ["runs", run_id, "browser"] {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|_| no_screens())?
            .file_type()
            .is_symlink()
        {
            return Err(no_screens());
        }
    }
    let file = crate::screen::load(&path)?.ok_or_else(no_screens)?;
    // 試行の記録が持つ参照と、保存された内容のハッシュが一致するものだけを使う。
    let known: std::collections::BTreeSet<_> = report
        .screens
        .iter()
        .map(|s| (&s.phase, &s.step, &s.sha256))
        .collect();
    if file.run_id != run_id
        || file
            .screens
            .iter()
            .any(|s| !known.contains(&(&s.phase, &s.step, &s.sha256)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "画面の構造の記録が、試行の記録と一致しません",
        ));
    }
    Ok(file)
}
fn read(
    directory: &Path,
    run_id: &str,
    report: &BrowserReport,
    step_id: &str,
) -> io::Result<Vec<u8>> {
    if !component(run_id) || !component(step_id) || report.run_id != run_id {
        return Err(missing());
    }
    let step = report
        .steps
        .iter()
        .find(|s| s.id == step_id)
        .ok_or_else(missing)?;
    if !["preparation", "action"].contains(&step.phase.as_str()) {
        return Err(missing());
    }
    let file = format!("{}-{}.png", step.phase, step.id);
    if step.screenshot.as_deref() != Some(format!("browser/{file}").as_str()) {
        return Err(missing());
    }
    let mut path = directory.to_path_buf();
    for part in ["runs", run_id, "browser", &file] {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|_| missing())?
            .file_type()
            .is_symlink()
        {
            return Err(missing());
        }
    }
    let meta = fs::metadata(&path).map_err(|_| missing())?;
    if !meta.is_file() || meta.len() > MAX_PNG {
        return Err(missing());
    }
    let mut bytes = vec![];
    fs::File::open(&path)?
        .take(MAX_PNG + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PNG || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(missing());
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::StepRecord;
    #[test]
    fn only_declared_local_pngs_are_read_and_missing_or_redirected_paths_are_rejected() {
        let (dir, correlation) =
            crate::recording::new_run_directory(&std::env::temp_dir().join("eel-browser-evidence"))
                .unwrap();
        let mut report = BrowserReport {
            launch_args: vec![],
            schema_version: 1,
            run_id: correlation.run_id.clone(),
            version: "test".into(),
            prepared: true,
            completed: true,
            steps: vec![StepRecord {
                canvas: None,
                attribute: None,
                id: "saved".into(),
                phase: "action".into(),
                completed: true,
                text: None,
                screenshot: Some("browser/action-saved.png".into()),
                detail: String::new(),
            }],
            responses: vec![],
            blocked_requests: 0,
            response_limit: false,
            detail: String::new(),
            exchanges: vec![],
            exchange_limit: false,
            screens: vec![],
            screens_note: String::new(),
        };
        let path = dir.join("runs").join(&report.run_id).join("browser");
        fs::create_dir_all(&path).unwrap();
        let png = b"\x89PNG\r\n\x1a\nsynthetic";
        fs::write(path.join("action-saved.png"), png).unwrap();
        assert_eq!(read(&dir, &report.run_id, &report, "saved").unwrap(), png);
        assert!(read(&dir, &report.run_id, &report, "../saved").is_err());
        assert!(read(&dir, "other-run", &report, "saved").is_err());
        report.steps[0].screenshot = Some("../../outside.png".into());
        assert!(read(&dir, &report.run_id, &report, "saved").is_err());
        report.steps[0].screenshot = Some("browser/action-saved.png".into());
        fs::remove_file(path.join("action-saved.png")).unwrap();
        assert!(read(&dir, &report.run_id, &report, "saved").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", path.join("action-saved.png")).unwrap();
            assert!(read(&dir, &report.run_id, &report, "saved").is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn screens_are_read_only_from_the_run_folder_and_must_match_the_report() {
        use crate::screen::{FILE, ScreenRef};
        let (dir, correlation) =
            crate::recording::new_run_directory(&std::env::temp_dir().join("eel-screen-evidence"))
                .unwrap();
        let run_id = correlation.run_id.clone();
        let browser = dir.join("runs").join(&run_id).join("browser");
        fs::create_dir_all(&browser).unwrap();
        let sha = "a".repeat(64);
        fs::write(
            browser.join(FILE),
            serde_json::json!({"schema_version":1,"run_id":run_id,"screens":[
                {"step":"saved","phase":"action","same_as":null,"sha256":sha,
                 "content":{"path":"/","title":"架空","frames":[],"document":null,"truncated":false}}]})
            .to_string(),
        )
        .unwrap();
        let report = |screens: Vec<ScreenRef>| BrowserReport {
            launch_args: vec![],
            schema_version: 1,
            run_id: run_id.clone(),
            version: "test".into(),
            prepared: true,
            completed: true,
            steps: vec![],
            responses: vec![],
            blocked_requests: 0,
            response_limit: false,
            detail: String::new(),
            exchanges: vec![],
            exchange_limit: false,
            screens,
            screens_note: String::new(),
        };
        let reference = |sha256: &str| ScreenRef {
            step: "saved".into(),
            phase: "action".into(),
            sha256: sha256.into(),
        };
        let ok = read_screens(&dir, &run_id, &report(vec![reference(&sha)])).unwrap();
        assert_eq!(ok.screens.len(), 1);
        // 画面を記録していない試行は、空の画面ではなく記録なしとして扱う。
        let none = read_screens(&dir, &run_id, &report(vec![])).unwrap_err();
        assert_eq!(none.kind(), io::ErrorKind::NotFound);
        // 試行の記録と一致しない内容、別の試行のIDは使わない。
        let other = "b".repeat(64);
        assert_eq!(
            read_screens(&dir, &run_id, &report(vec![reference(&other)]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(read_screens(&dir, "../outside", &report(vec![reference(&sha)])).is_err());
        assert!(read_screens(&dir, "other-run", &report(vec![reference(&sha)])).is_err());
        // リンクにすり替えたフォルダーは読まない。
        let moved = dir.join("moved");
        fs::rename(&browser, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, &browser).unwrap();
        assert!(read_screens(&dir, &run_id, &report(vec![reference(&sha)])).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
