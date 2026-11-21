//! 登録するリポジトリのブランチの一覧。画面で、ブランチを入力させずに選ばせるために使う。
//!
//! GitHubの公開リポジトリはGitHubのAPIで読む。GitHubから読めないとき（架空のデモ用のリポジトリや、ネットワークに
//! つながらないとき）は、麦漆が試験用に取得したリポジトリ（データの保存先の `repositories`）にあるブランチを返す。
//! どちらから読んだかを `source` で示す。一覧は読んだ時点のもので、読み込むときにコミットを固定する。
use crate::error::invalid;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io, path::Path, process::Command};

/// 既定のブランチと、ブランチ（名前・最新のコミット）の一覧。
type Branches = (String, Vec<(String, String)>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchesRequest {
    pub url: String,
}

pub fn list(request: &BranchesRequest, repositories: &Path) -> io::Result<Value> {
    let repository = crate::reader::repository_name(&request.url)?;
    let (source, default, branches) = match crate::reader::branches(&request.url) {
        Ok((default, branches)) => ("github", default, branches),
        Err(github) => match local(repositories, &repository) {
            Ok(Some((default, branches))) => ("local", default, branches),
            Ok(None) => return Err(github),
            Err(error) => {
                return Err(io::Error::new(
                    github.kind(),
                    format!("{github}（試験用に取得したリポジトリも読めませんでした: {error}）"),
                ));
            }
        },
    };
    if branches.is_empty() {
        return Err(invalid("ブランチが見つかりませんでした"));
    }
    Ok(json!({
        "repository": repository,
        "source": source,
        "default_branch": default,
        "branches": branches.iter().map(|(name, commit)| json!({"name": name, "commit": commit})).collect::<Vec<_>>(),
    }))
}

/// 試験用に取得したリポジトリ（bare）のブランチ。リポジトリがなければ `None`。
fn local(base: &Path, repository: &str) -> io::Result<Option<Branches>> {
    let dir = base.join(repository);
    if !dir.join("HEAD").is_file() {
        return Ok(None);
    }
    let git = |args: &[&str]| -> io::Result<String> {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(&dir)
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let refs = git(&[
        "for-each-ref",
        "--format=%(refname:short)\t%(objectname)",
        "refs/heads",
    ])?;
    let branches: Vec<(String, String)> = refs
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, sha)| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|(n, s)| (n.to_owned(), s.to_owned()))
        .collect();
    if branches.is_empty() {
        return Ok(None);
    }
    let default = git(&["symbolic-ref", "--short", "HEAD"])
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    Ok(Some((default, branches)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mugiurushi-branches-{name}-{}-{}",
            std::process::id(),
            crate::recording::unix_ms().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn local_branches_come_from_a_fetched_bare_repository() {
        let temp = temp("local");
        let work = temp.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            assert!(
                Command::new("git")
                    .current_dir(dir)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&work, &["init", "-q", "-b", "main"]);
        run(
            &work,
            &[
                "-c",
                "user.name=架空",
                "-c",
                "user.email=a@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "最初",
            ],
        );
        run(&work, &["branch", "fix"]);
        let base = temp.join("repositories");
        std::fs::create_dir_all(base.join("example")).unwrap();
        run(
            &temp,
            &[
                "clone",
                "-q",
                "--bare",
                "work",
                "repositories/example/monshin",
            ],
        );
        let (default, branches) = local(&base, "example/monshin").unwrap().unwrap();
        assert_eq!(default, "main");
        assert_eq!(
            branches.iter().map(|b| b.0.as_str()).collect::<Vec<_>>(),
            vec!["fix", "main"]
        );
        assert!(local(&base, "example/none").unwrap().is_none());
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn a_url_with_credentials_or_a_file_path_is_rejected_before_reading() {
        let temp = temp("reject");
        for url in [
            "https://user:x@github.com/a/b",
            "https://github.com/a/b/blob/main/x.py",
            "http://github.com/a/b",
        ] {
            assert!(list(&BranchesRequest { url: url.into() }, &temp).is_err());
        }
    }
}
