//! 実行ファイルへ終了の合図を送り、試験用のプロセスを残さずに終わることを確かめる。
//! 架空のスクリプトだけを使う。コンテナを使う場合の試験は `tests/containers.rs` にある。
mod support;
use mugiurushi::{execution::*, recording::*, workspace::*};
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
use support::{process_alive, send_signal, spawn_case_execute, wait_within};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

/// 起動すると孫のプロセスを作り、自分と孫のPIDをファイルへ書いて、長く待つスクリプト。
const SCRIPT: &str = "import {spawn} from 'node:child_process';
import fs from 'node:fs';
// 実行器は環境変数を絞って起動するので、孫にはこのスクリプトを動かしているNode.js自身を使う。
const child = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 120000)'], {stdio: 'ignore'});
fs.writeFileSync(process.argv[2], `${process.pid} ${child.pid}`);
setTimeout(() => {}, 120000);
";

fn recipe(commit: &str, marker: &Path) -> Recipe {
    Recipe {
        schema_version: 1,
        check_id: "waits".into(),
        source: CodeReference {
            repository: "example/project".into(),
            commit: commit.into(),
            path: "app.mjs".into(),
            line: 1,
            end_line: 1,
        },
        runtime: Runtime::Node,
        entrypoint: "app.mjs".into(),
        args: vec![marker.to_string_lossy().into_owned()],
        input: serde_json::json!({}),
        criterion: Criterion {
            assertion: "waits".into(),
            subject: "record-1".into(),
        },
        timeout_ms: 120_000,
        trial: None,
        launch: Default::default(),
        python: None,
        server: None,
        containers: None,
        http: None,
        browser: None,
    }
}

/// 架空のリポジトリを作り、スクリプトを実行器で動かして、スクリプトと孫のPIDが書かれるまで待つ。
fn running(label: &str) -> (std::process::Child, Vec<u32>) {
    let directory = new_run_directory(&std::env::temp_dir().join("evidence-shutdown-tests"))
        .unwrap()
        .0;
    let root = directory.join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Synthetic test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/project.git",
        ],
    );
    fs::write(root.join("app.mjs"), SCRIPT).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "synthetic"]);
    let commit = git(&root, &["rev-parse", "HEAD"]);
    let marker = directory.join("pids.txt");
    let mut child = spawn_case_execute(&directory.join("work"), &root, &recipe(&commit, &marker));
    let until = Instant::now() + Duration::from_secs(30);
    let pids: Vec<u32> = loop {
        let found: Vec<u32> = fs::read_to_string(&marker)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|text| text.parse().ok())
            .collect();
        if found.len() == 2 {
            break found;
        }
        // スクリプトが起動する前に実行ファイルが終わった場合は、その理由を出す。
        if child.try_wait().unwrap().is_some() || Instant::now() >= until {
            let (exit, stderr) = wait_within(child, Duration::from_secs(1));
            panic!("{label}: スクリプトが起動しなかった（終了コード {exit:?}）: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(pids.iter().all(|pid| process_alive(*pid)), "{label}");
    (child, pids)
}

/// 指定したプロセスがすべて止まるまで、上限の時間まで待つ。
fn all_stopped(pids: &[u32]) -> bool {
    (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        pids.iter().all(|pid| !process_alive(*pid))
    })
}

#[test]
fn a_signal_stops_the_trial_process_and_its_descendants_before_exiting() {
    for (signal, code) in [("TERM", 143), ("INT", 130)] {
        let (child, pids) = running(signal);
        send_signal(child.id(), signal);
        let (exit, stderr) = wait_within(child, Duration::from_secs(20));
        // 合図で終わったことが、終了コードと標準エラーで分かる。
        assert_eq!(exit, Some(code), "{signal}: {stderr}");
        assert!(
            stderr.contains(&format!("終了の合図（SIG{signal}）")),
            "{signal}: {stderr}"
        );
        assert!(
            stderr.contains("子のプロセスグループ1件"),
            "{signal}: {stderr}"
        );
        // スクリプトと孫の両方が止まっている。
        assert!(
            all_stopped(&pids),
            "{signal}: プロセス {pids:?} が残っている"
        );
    }
}

/// ジョブを取り消す仕組みには、SIGINTの後にSIGTERMを続けて送るものがある。2回目の合図では回収を打ち切らない。
#[test]
fn a_second_signal_during_collection_does_not_cut_it_short() {
    let (child, pids) = running("INT→TERM");
    send_signal(child.id(), "INT");
    // 回収は、子のグループへ終了を求めてから1.5秒待つ。その間に2回目の合図を送る。
    std::thread::sleep(Duration::from_millis(300));
    send_signal(child.id(), "TERM");
    let (exit, stderr) = wait_within(child, Duration::from_secs(20));
    // 最初の合図の終了コードで終わり、回収の報告が出ている。
    assert_eq!(exit, Some(130), "{stderr}");
    assert!(stderr.contains("終了の合図（SIGINT）"), "{stderr}");
    assert!(stderr.contains("残った資源はない"), "{stderr}");
    assert!(stderr.contains("もう一度合図を送ると"), "{stderr}");
    assert!(all_stopped(&pids), "プロセス {pids:?} が残っている");
}

/// 回収が終わらない場合に止める手段として、3回目の合図では回収を待たずに終わる。
#[test]
fn a_third_signal_ends_without_waiting_for_collection() {
    let (child, pids) = running("INT→TERM→TERM");
    send_signal(child.id(), "INT");
    // 合図どうしの間を空ける。同じ種類の合図は、受け取る前に重なると1回にまとまる。
    std::thread::sleep(Duration::from_millis(300));
    send_signal(child.id(), "TERM");
    std::thread::sleep(Duration::from_millis(300));
    send_signal(child.id(), "TERM");
    let (exit, stderr) = wait_within(child, Duration::from_secs(20));
    // 3回目の合図の終了コードで終わり、回収の報告は出ない。
    assert_eq!(exit, Some(143), "{stderr}");
    assert!(!stderr.contains("終了の合図（"), "{stderr}");
    // 試験の後始末。回収が途中で終わったので、残っているプロセスがあれば止める。
    for pid in &pids {
        if process_alive(*pid) {
            send_signal(*pid, "KILL");
        }
    }
    assert!(all_stopped(&pids), "プロセス {pids:?} が残っている");
}

/// 架空の `docker` を置いて固定環境の実行を始め、コンテナを控えた状態でSIGTERMを送る。
/// 終了コード、標準エラー、架空の `docker` が受けた呼び出しの記録を返す。
/// 親の環境には `DOCKER_HOST` を設定する。固定環境の実行は、この変数を `docker` へ渡さない。
fn stop_an_environment_run(reachable: bool) -> (Option<i32>, String, String) {
    use std::os::unix::fs::PermissionsExt;
    let directory = new_run_directory(&std::env::temp_dir().join("evidence-shutdown-tests"))
        .unwrap()
        .0;
    let root = directory.join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "Synthetic test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/example/project.git",
        ],
    );
    fs::write(root.join("README.md"), "架空の対象\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "synthetic"]);
    let commit = git(&root, &["rev-parse", "HEAD"]);
    // 既存の例の準備設定を写し、対象だけを架空のリポジトリに変える。
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/monai-cpu");
    let settings = directory.join("settings");
    fs::create_dir(&settings).unwrap();
    fs::copy(example.join("infer.py"), settings.join("infer.py")).unwrap();
    let mut recipe: serde_json::Value =
        serde_json::from_slice(&fs::read(example.join("recipe.json")).unwrap()).unwrap();
    recipe["source"]["repository"] = "example/project".into();
    recipe["source"]["commit"] = commit.into();
    fs::write(
        settings.join("recipe.json"),
        serde_json::to_vec(&recipe).unwrap(),
    )
    .unwrap();
    // 架空の `docker`。受けた呼び出しと `DOCKER_HOST` を記録する。コンテナの開始では待ち続ける。
    let log = directory.join("docker.log");
    let failing = "echo 'Cannot connect to the Docker daemon' >&2; exit 1";
    let (rm, ps) = if reachable {
        ("exit 0", "exit 0")
    } else {
        (failing, failing)
    };
    let bin = directory.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(
        bin.join("docker"),
        format!(
            "#!/bin/sh\necho \"$1 host=${{DOCKER_HOST:-}}\" >> '{}'\ncase \"$1\" in\n  version) echo 0.0.0 ;;\n  image) echo sha256:synthetic ;;\n  create) echo synthetic ;;\n  start) exec sleep 60 ;;\n  rm) {rm} ;;\n  ps) {ps} ;;\n  *) exit 0 ;;\nesac\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(bin.join("docker"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_mugiurushi"))
        .args(["environment", "run"])
        .arg(settings.join("recipe.json"))
        .arg("--repository-root")
        .arg(&root)
        .arg("--output")
        .arg(directory.join("runs"))
        .env("PATH", path)
        .env("DOCKER_HOST", "unix:///nonexistent/eel-test.sock")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // コンテナを開始する呼び出しが届くまで待つ。この時点で、実行器はコンテナを控えている。
    let until = Instant::now() + Duration::from_secs(30);
    while !fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .any(|line| line.starts_with("start "))
    {
        if child.try_wait().unwrap().is_some() || Instant::now() >= until {
            let (exit, stderr) = wait_within(child, Duration::from_secs(1));
            panic!("コンテナの開始まで進まなかった（終了コード {exit:?}）: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    send_signal(child.id(), "TERM");
    let (exit, stderr) = wait_within(child, Duration::from_secs(20));
    (exit, stderr, fs::read_to_string(&log).unwrap())
}

#[test]
fn a_held_environment_container_is_removed_through_the_connection_that_created_it() {
    let (exit, stderr, log) = stop_an_environment_run(true);
    assert_eq!(exit, Some(143), "{stderr}");
    assert!(stderr.contains("コンテナ1件"), "{stderr}");
    assert!(stderr.contains("残った資源はない"), "{stderr}");
    // 消す呼び出しと、残りを確かめる呼び出しが届いている。
    assert!(log.lines().any(|line| line.starts_with("rm ")), "{log}");
    assert!(log.lines().any(|line| line.starts_with("ps ")), "{log}");
    // 作るときも消すときも、親の `DOCKER_HOST` を渡していない。同じ接続先を使っている。
    assert!(log.lines().all(|line| line.ends_with("host=")), "{log}");
}

#[test]
fn a_failed_docker_query_is_not_reported_as_nothing_left() {
    let (exit, stderr, _) = stop_an_environment_run(false);
    assert_eq!(exit, Some(143), "{stderr}");
    assert!(
        stderr.contains("残りを確かめられなかった資源: コンテナ eel-env-"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Cannot connect to the Docker daemon"),
        "{stderr}"
    );
    assert!(!stderr.contains("残った資源はない"), "{stderr}");
}
