//! 依存の少ないアプリかどうかを、読み取ったコードから判定する（AIは使わない）。
//!
//! 今の版で「試験環境で動かせる」とするのは、Python 3 の標準ライブラリだけで、`if __name__ == "__main__"` を持つ
//! ファイルから起動できるアプリだけ。麦漆は試験で依存パッケージを入れないので（`src/execution.rs`）、外部パッケージを
//! 読み込むコード、依存を宣言したファイル、ほかのサービスと組み合わせる前提のファイルがあるアプリは「動かせない」とし、
//! 理由を返す。この判定はコードを読んだ結果からの推定で、実際に起動できるかは試験の実行で分かる。
use crate::error::invalid;
use crate::understanding::{Store, UnderstandRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;

/// 読み込みを調べるPythonのファイルの上限。これを超えるアプリは「依存の少ないアプリ」として扱わない。
pub const MAX_PYTHON_FILES: usize = 60;
/// 1ファイルで読む文字の上限。
const MAX_CHARS: usize = 200_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnableRequest {
    pub repository: String,
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Assessment {
    pub repository: String,
    pub commit: String,
    /// 試験環境で動かせると判定したか。
    pub runnable: bool,
    /// 動かせる場合の起動のしかた。今は python3 だけ。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<&'static str>,
    /// 起動の候補（`__main__` を持つファイル。試験のファイルは除く）。
    pub entrypoints: Vec<String>,
    /// 動かせない理由。
    pub blockers: Vec<String>,
    /// 標準ライブラリにもリポジトリの中にもない読み込み。
    pub third_party: Vec<String>,
    /// 判定に使ったファイル。
    pub read: Vec<String>,
    /// 判定には使わないが、知らせておくこと。
    pub notes: Vec<String>,
}

const MANIFESTS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "Pipfile",
    "environment.yml",
    "environment.yaml",
];
const SERVICES: &[&str] = &[
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];
const SERVERS: &[&str] = &["http.server", "socketserver", "wsgiref", "asyncio"];

pub fn assess(store: &Store, request: &RunnableRequest) -> io::Result<Assessment> {
    let snapshot = store.load(&request.repository, &request.commit)?;
    let paths: Vec<String> = snapshot["inventory"]
        .as_array()
        .ok_or_else(|| invalid("読取記録のファイル一覧がありません"))?
        .iter()
        .filter_map(|e| e["path"].as_str().map(str::to_owned))
        .collect();
    let read = |path: &str| -> io::Result<String> {
        let file = store.handle(UnderstandRequest::File {
            repository: request.repository.clone(),
            commit: request.commit.clone(),
            path: path.to_owned(),
        })?;
        Ok(file["file"]["text"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(MAX_CHARS)
            .collect())
    };
    judge(&request.repository, &request.commit, &paths, read)
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 試験・例のファイル。起動の候補から除く。
fn is_test(path: &str) -> bool {
    let name = name_of(path);
    name.starts_with("test_")
        || name.ends_with("_test.py")
        || name == "conftest.py"
        || path
            .split('/')
            .any(|p| matches!(p, "tests" | "test" | "examples" | "docs"))
}

/// 判定の本体。ファイルの読み方を外から受け取り、試験では架空のファイルを渡す。
pub fn judge(
    repository: &str,
    commit: &str,
    paths: &[String],
    read: impl Fn(&str) -> io::Result<String>,
) -> io::Result<Assessment> {
    let mut out = Assessment {
        repository: repository.to_owned(),
        commit: commit.to_owned(),
        runnable: false,
        runtime: None,
        entrypoints: vec![],
        blockers: vec![],
        third_party: vec![],
        read: vec![],
        notes: vec![],
    };
    let python: Vec<&String> = paths.iter().filter(|p| p.ends_with(".py")).collect();
    if python.is_empty() {
        out.blockers.push(
            "Pythonのコードがありません。今の版で自動で動かせるのは、Python 3 のアプリだけです"
                .into(),
        );
        return Ok(out);
    }
    if python.len() > MAX_PYTHON_FILES {
        out.blockers.push(format!(
            "Pythonのファイルが{}件あります。依存の少ないアプリとして扱うのは{MAX_PYTHON_FILES}件までです",
            python.len()
        ));
        return Ok(out);
    }
    for path in paths {
        let name = name_of(path);
        if SERVICES.contains(&name) {
            out.blockers.push(format!(
                "{path} があります。ほかのサービスと組み合わせて動かす前提の可能性があります"
            ));
        } else if name == "Dockerfile" {
            out.notes.push(format!(
                "{path} がありますが、試験ではコンテナを使わずに起動します"
            ));
        }
    }
    // 依存の宣言。requirements*.txt と、よく使う宣言のファイル。
    let mut declared = BTreeSet::new();
    for path in paths {
        let name = name_of(path);
        let requirements = name.starts_with("requirements") && name.ends_with(".txt");
        if !requirements && !MANIFESTS.contains(&name) {
            continue;
        }
        let text = read(path)?;
        out.read.push(path.clone());
        for dependency in declared_dependencies(name, &text) {
            declared.insert(dependency);
        }
    }
    if !declared.is_empty() {
        out.blockers.push(format!(
            "依存パッケージの宣言があります（{}）。試験では依存パッケージを入れません",
            declared.iter().cloned().collect::<Vec<_>>().join("、")
        ));
    }
    // リポジトリの中のモジュール名（ファイル名と、最上位・途中のディレクトリ名）。
    let mut local = BTreeSet::new();
    for path in &python {
        for part in path.trim_end_matches(".py").split('/') {
            local.insert(part.to_owned());
        }
    }
    let mut third = BTreeSet::new();
    let mut servers = BTreeSet::new();
    for path in &python {
        // 試験・例のファイルは試験環境で起動しないので、その読み込み（pytest など）は数えない。
        if is_test(path) {
            continue;
        }
        let text = read(path)?;
        out.read.push((*path).clone());
        for module in imports(&text) {
            let top = module.split('.').next().unwrap_or(&module).to_owned();
            if SERVERS
                .iter()
                .any(|s| module == *s || module.starts_with(&format!("{s}.")))
            {
                servers.insert(module.clone());
            }
            if top.is_empty()
                || top == "__future__"
                || STDLIB.contains(&top.as_str())
                || local.contains(&top)
            {
                continue;
            }
            third.insert(top);
        }
        if has_main(&text) {
            out.entrypoints.push((*path).clone());
        }
    }
    out.third_party = third.into_iter().collect();
    if !out.third_party.is_empty() {
        out.blockers.push(format!(
            "標準ライブラリにない読み込みがあります（{}）。試験では依存パッケージを入れません",
            out.third_party.join("、")
        ));
    }
    if out.entrypoints.is_empty() {
        out.blockers.push(
            "起動するファイル（`if __name__ == \"__main__\"` を持つファイル）が見つかりません"
                .into(),
        );
    }
    if !servers.is_empty() {
        out.notes.push(format!(
            "待ち受けるサーバーとして動く可能性があります（{}）。自動の試験は、コマンドとして起動して終了を待つ形だけです",
            servers.into_iter().collect::<Vec<_>>().join("、")
        ));
    }
    out.entrypoints.sort();
    out.entrypoints.truncate(20);
    out.runnable = out.blockers.is_empty();
    if out.runnable {
        out.runtime = Some("python3");
    }
    Ok(out)
}

/// `import a, b.c` と `from a.b import c` の読み込み先。相対の読み込み（`from . import x`）は除く。
fn imports(text: &str) -> Vec<String> {
    let mut out = vec![];
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(rest) = line.strip_prefix("import ") {
            for part in rest.split(',') {
                let name = part.split_whitespace().next().unwrap_or("");
                if is_module(name) {
                    out.push(name.to_owned());
                }
            }
        } else if let Some(rest) = line.strip_prefix("from ") {
            let name = rest.split_whitespace().next().unwrap_or("");
            if !name.starts_with('.') && is_module(name) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

fn is_module(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|p| {
            !p.is_empty()
                && p.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

fn has_main(text: &str) -> bool {
    text.lines().any(|l| {
        let l: String = l.chars().filter(|c| !c.is_whitespace()).collect();
        l.starts_with("if__name__==\"__main__\"") || l.starts_with("if__name__=='__main__'")
    })
}

/// 宣言のファイルから依存パッケージの名前を取り出す。形を読み取れない宣言は、ファイル名を依存として返す。
fn declared_dependencies(name: &str, text: &str) -> Vec<String> {
    let package = |line: &str| -> Option<String> {
        let n: String = line
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        (!n.is_empty()).then_some(n)
    };
    if name.ends_with(".txt") {
        return text
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty() && !l.starts_with('-'))
            .filter_map(package)
            .collect();
    }
    if name == "pyproject.toml" {
        // [project] の dependencies = [ ... ] だけを読む。ほかの表（ビルドの設定など）は依存として数えない。
        let mut out = vec![];
        let mut inside = false;
        for line in text.lines() {
            let t = line.trim();
            if !inside && t.starts_with("dependencies") && t.contains('[') {
                inside = !t.contains(']');
                let after = t.split_once('[').map(|x| x.1).unwrap_or("");
                out.extend(after.split(',').filter_map(|s| {
                    package(s.trim().trim_matches(|c| c == '"' || c == '\'' || c == ']'))
                }));
                continue;
            }
            if inside {
                if t.starts_with(']') {
                    inside = false;
                    continue;
                }
                out.extend(package(
                    t.trim_matches(|c| c == '"' || c == '\'' || c == ','),
                ));
            }
        }
        return out;
    }
    vec![name.to_owned()]
}

/// Python 3.13 の `sys.stdlib_module_names`。
const STDLIB: &[&str] = &[
    "__future__",
    "_abc",
    "_aix_support",
    "_android_support",
    "_apple_support",
    "_ast",
    "_asyncio",
    "_bisect",
    "_blake2",
    "_bz2",
    "_codecs",
    "_codecs_cn",
    "_codecs_hk",
    "_codecs_iso2022",
    "_codecs_jp",
    "_codecs_kr",
    "_codecs_tw",
    "_collections",
    "_collections_abc",
    "_colorize",
    "_compat_pickle",
    "_compression",
    "_contextvars",
    "_csv",
    "_ctypes",
    "_curses",
    "_curses_panel",
    "_datetime",
    "_dbm",
    "_decimal",
    "_elementtree",
    "_frozen_importlib",
    "_frozen_importlib_external",
    "_functools",
    "_gdbm",
    "_hashlib",
    "_heapq",
    "_imp",
    "_interpchannels",
    "_interpqueues",
    "_interpreters",
    "_io",
    "_ios_support",
    "_json",
    "_locale",
    "_lsprof",
    "_lzma",
    "_markupbase",
    "_md5",
    "_multibytecodec",
    "_multiprocessing",
    "_opcode",
    "_opcode_metadata",
    "_operator",
    "_osx_support",
    "_overlapped",
    "_pickle",
    "_posixshmem",
    "_posixsubprocess",
    "_py_abc",
    "_pydatetime",
    "_pydecimal",
    "_pyio",
    "_pylong",
    "_pyrepl",
    "_queue",
    "_random",
    "_scproxy",
    "_sha1",
    "_sha2",
    "_sha3",
    "_signal",
    "_sitebuiltins",
    "_socket",
    "_sqlite3",
    "_sre",
    "_ssl",
    "_stat",
    "_statistics",
    "_string",
    "_strptime",
    "_struct",
    "_suggestions",
    "_symtable",
    "_sysconfig",
    "_thread",
    "_threading_local",
    "_tkinter",
    "_tokenize",
    "_tracemalloc",
    "_typing",
    "_uuid",
    "_warnings",
    "_weakref",
    "_weakrefset",
    "_winapi",
    "_wmi",
    "_zoneinfo",
    "abc",
    "antigravity",
    "argparse",
    "array",
    "ast",
    "asyncio",
    "atexit",
    "base64",
    "bdb",
    "binascii",
    "bisect",
    "builtins",
    "bz2",
    "cProfile",
    "calendar",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "ensurepip",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "genericpath",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "msvcrt",
    "multiprocessing",
    "netrc",
    "nt",
    "ntpath",
    "nturl2path",
    "numbers",
    "opcode",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "pydoc_data",
    "pyexpat",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "sre_compile",
    "sre_constants",
    "sre_parse",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "tempfile",
    "termios",
    "textwrap",
    "this",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(files: &[(&str, &str)]) -> Assessment {
        let map: HashMap<String, String> = files
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        let paths: Vec<String> = files.iter().map(|(p, _)| (*p).to_owned()).collect();
        judge(
            "example/app",
            "0123456789abcdef0123456789abcdef01234567",
            &paths,
            |p| map.get(p).cloned().ok_or_else(|| invalid("なし")),
        )
        .unwrap()
    }

    #[test]
    fn a_standard_library_command_app_is_runnable_with_its_main_files() {
        let a = run(&[
            (
                "monshin.py",
                "import argparse, csv\nfrom pathlib import Path\nfrom store import save\nif __name__ == \"__main__\":\n    main()\n",
            ),
            ("store.py", "import os.path\nfrom . import x\n"),
            (
                "tests/test_app.py",
                "import pytest\nif __name__ == '__main__': pass\n",
            ),
            ("README.md", "# 架空"),
        ]);
        assert!(a.runnable, "{a:?}");
        assert_eq!(a.runtime, Some("python3"));
        assert_eq!(a.entrypoints, vec!["monshin.py"]);
        // 試験のファイルは起動しないので、その読み込み（pytest）は数えない。
        assert!(a.third_party.is_empty());
    }

    #[test]
    fn third_party_imports_declared_dependencies_and_services_make_it_not_runnable() {
        let a = run(&[(
            "app.py",
            "import flask\nif __name__ == '__main__':\n  pass\n",
        )]);
        assert!(!a.runnable && a.third_party == vec!["flask"]);
        let b = run(&[
            (
                "app.py",
                "import json\nif __name__ == '__main__':\n  pass\n",
            ),
            ("requirements.txt", "# コメント\nrequests==2.0\n-e .\n"),
        ]);
        assert!(!b.runnable && b.blockers[0].contains("requests"), "{b:?}");
        let c = run(&[
            (
                "app.py",
                "import json\nif __name__ == '__main__':\n  pass\n",
            ),
            (
                "pyproject.toml",
                "[project]\nname = \"x\"\ndependencies = [\n  \"boto3>=1\",\n]\n[build-system]\nrequires = [\"hatchling\"]\n",
            ),
        ]);
        assert!(
            !c.runnable && c.blockers[0].contains("boto3") && !c.blockers[0].contains("hatchling"),
            "{c:?}"
        );
        let d = run(&[
            (
                "app.py",
                "import json\nif __name__ == '__main__':\n  pass\n",
            ),
            ("docker-compose.yml", "services: {}"),
        ]);
        assert!(!d.runnable && d.blockers[0].contains("docker-compose.yml"));
    }

    #[test]
    fn no_python_no_main_or_too_many_files_are_not_runnable() {
        assert!(!run(&[("index.js", "")]).runnable);
        let a = run(&[("lib.py", "import os\n")]);
        assert!(!a.runnable && a.blockers[0].contains("__main__"));
        let many: Vec<(String, String)> = (0..=MAX_PYTHON_FILES)
            .map(|i| (format!("m{i}.py"), String::new()))
            .collect();
        let refs: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert!(!run(&refs).runnable);
    }

    #[test]
    fn servers_are_noted_but_do_not_block() {
        let a = run(&[(
            "serve.py",
            "from http.server import HTTPServer\nif __name__ == \"__main__\":\n  pass\n",
        )]);
        assert!(a.runnable && a.notes[0].contains("http.server"));
    }
}
