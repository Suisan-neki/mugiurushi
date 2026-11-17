//! 固定したコミットの検索と読み取りを、GitHubの公開リポジトリで確かめる。ネットワークを使うので、通常の試験とは分ける。
use mugiurushi::code_tree::{CodeTree, Exclusions};
use std::{fs, time::Instant};

fn base(name: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "mugiurushi-code-tree-live-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    base
}

/// 配布物の構成（小さなリポジトリ）。取得、一覧、検索、読み取りが通ることを確かめる。
#[test]
#[ignore = "GitHubからの取得が要る"]
fn a_small_public_repository_can_be_searched_and_read() {
    let base = base("small");
    let tree = CodeTree::open(
        &base,
        "DIGI-UW/openelis-docker",
        "6fb4a9e9223f9e04cafbb916d6d5260801ba819e",
        Exclusions::new(&[]).unwrap(),
    )
    .unwrap();
    let files = tree.files();
    assert!(
        files.contains(&"docker-compose.yml".to_owned()),
        "{files:?}"
    );
    // 配布物の .env（既定のパスワードを含む）は、一覧にも検索の結果にも出ない。
    assert!(!files.iter().any(|f| f == ".env" || f.ends_with("/.env")));
    let result = tree.search("openelisglobal-proxy", false).unwrap();
    assert!(
        result.hits.iter().any(|h| h.path == "docker-compose.yml"),
        "{result:?}"
    );
    assert!(result.hits.iter().all(|h| !h.path.ends_with(".env")));
    let read = tree.read("docker-compose.yml", 1, 20).unwrap();
    assert_eq!(read.start, 1);
    assert!(read.end <= 20 && !read.text.is_empty());
    fs::remove_dir_all(&base).unwrap();
}

/// 大きなリポジトリ（OpenELIS Global 2 の 3.3.0）。コード読みの比較（docs/validation-code-reading-2026-11-16.md）で
/// 根拠になった行を、検索と読み取りでたどれることと、かかった時間を確かめる。
#[test]
#[ignore = "GitHubからの大きな取得（数百MB）が要る"]
fn a_large_repository_can_be_followed_from_the_screen_to_the_request() {
    let base = base("large");
    let started = Instant::now();
    let tree = CodeTree::open(
        &base,
        "DIGI-UW/OpenELIS-Global-2",
        "bcead488e7c6a633113a5981bb2131690c1dbab3",
        Exclusions::new(&[]).unwrap(),
    )
    .unwrap();
    eprintln!(
        "取得と一覧: {:?}、ファイル {}件",
        started.elapsed(),
        tree.files().len()
    );
    let searched = Instant::now();
    let result = tree
        .search("subjectNumberValidationProvider", false)
        .unwrap();
    eprintln!("検索: {:?}、{}件", searched.elapsed(), result.hits.len());
    let form = "frontend/src/components/patient/CreatePatientForm.tsx";
    let hit = result
        .hits
        .iter()
        .find(|h| h.path == form)
        .unwrap_or_else(|| panic!("{form} が検索の結果にない: {result:?}"));
    assert!(hit.text.contains("subjectNumber"), "{hit:?}");
    let read = tree
        .read(form, hit.line.saturating_sub(3).max(1), hit.line + 3)
        .unwrap();
    assert!(
        read.text.contains("subjectNumberValidationProvider"),
        "{}",
        read.text
    );
    // 2回目は取得し直さない。
    let reopened = Instant::now();
    CodeTree::open(
        &base,
        "DIGI-UW/OpenELIS-Global-2",
        "bcead488e7c6a633113a5981bb2131690c1dbab3",
        Exclusions::new(&[]).unwrap(),
    )
    .unwrap();
    eprintln!("2回目に開く: {:?}", reopened.elapsed());
    fs::remove_dir_all(&base).unwrap();
}
