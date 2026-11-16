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

#[test]
fn a_signal_stops_the_trial_process_and_its_descendants_before_exiting() {
    for (signal, code) in [("TERM", 143), ("INT", 130)] {
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
        let mut child =
            spawn_case_execute(&directory.join("work"), &root, &recipe(&commit, &marker));
        // スクリプトが孫を起動してPIDを書くまで待つ。
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
                panic!("{signal}: スクリプトが起動しなかった（終了コード {exit:?}）: {stderr}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(pids.iter().all(|pid| process_alive(*pid)), "{signal}");
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
        let stopped = (0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            pids.iter().all(|pid| !process_alive(*pid))
        });
        assert!(stopped, "{signal}: プロセス {pids:?} が残っている");
    }
}
