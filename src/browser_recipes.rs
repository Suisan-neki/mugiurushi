//! 画面で選ぶブラウザ試験の一覧。運営者が `EEL_RECIPE_DIR` で指定したディレクトリの
//! `*.catalog.json` から作る。選択・閲覧だけでは取得も実行も行わない。
//! 基盤のコードは対象の名前や手順のIDを持たない。派生（故障の追加、判定条件の絞り込み）は
//! 一覧のファイルで宣言し、宣言した手順や判定条件がレシピにない場合は一覧全体を拒否する。
use crate::error::invalid;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

/// 一覧を読むディレクトリを指定する環境変数。未指定のときは空の一覧を返す。
pub const DIRECTORY_VARIABLE: &str = "EEL_RECIPE_DIR";
const MAX_CATALOGS: usize = 32;
const MAX_ENTRIES: usize = 16;
const MAX_FILE: u64 = 1024 * 1024;
const MAX_TEXT: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    /// 一覧のファイルと同じディレクトリから見た、もとになるレシピの相対パス。
    base: String,
    note: String,
    entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    title: String,
    /// もとのレシピの故障を置き換える。
    #[serde(default)]
    fault: Option<crate::trial::TrialFault>,
    /// この手順より前の操作を準備へ移す。故障は準備の後に入るため、この手順の直前に故障が入る。
    #[serde(default)]
    fault_before_step: Option<String>,
    #[serde(default)]
    keep_checks: Option<Keep>,
    #[serde(default)]
    add_checks: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Keep {
    Ids(Vec<String>),
    IdPrefix(String),
}

fn text(value: &str, what: &str) -> io::Result<()> {
    if value.trim().is_empty() || value.len() > MAX_TEXT || value.contains('\0') {
        return Err(invalid(format!(
            "試験の一覧の{what}は1〜{MAX_TEXT}バイトで書いてください"
        )));
    }
    Ok(())
}

fn entry_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// シンボリックリンクをたどらず、上限以内の通常のファイルだけを読む。
fn read_plain(path: &Path) -> io::Result<String> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Err(invalid(format!(
            "{} は通常のファイルではないか、上限を超えています",
            path.display()
        )));
    }
    fs::read_to_string(path)
}

/// 一覧のファイルを、ディレクトリの直下と1段下から名前順に集める。リンクは読まない。
fn catalog_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = vec![];
    let mut visit = |dir: &Path, descend: bool, pending: &mut Vec<PathBuf>| -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_dir() && descend {
                pending.push(path);
            } else if meta.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".catalog.json"))
            {
                found.push(path);
            }
        }
        Ok(())
    };
    let mut children = vec![];
    visit(directory, true, &mut children)?;
    for child in children {
        visit(&child, false, &mut vec![])?;
    }
    found.sort();
    if found.len() > MAX_CATALOGS {
        return Err(invalid(format!(
            "試験の一覧のファイルは{MAX_CATALOGS}件までです"
        )));
    }
    Ok(found)
}

/// 一覧の指定に従って、もとのレシピから1件を作る。対象の名前やIDは引数だけから得る。
fn derive(base: &Value, entry: &Entry) -> io::Result<Value> {
    let mut recipe = base.clone();
    let changes_trial =
        entry.fault.is_some() || entry.keep_checks.is_some() || !entry.add_checks.is_empty();
    if changes_trial && !recipe["trial"]["checks"].is_array() {
        return Err(invalid(format!(
            "{}: もとのレシピに判定条件がありません",
            entry.id
        )));
    }
    if let Some(fault) = entry.fault {
        recipe["trial"]["fault"] = serde_json::to_value(fault).map_err(io::Error::other)?;
    }
    if let Some(step) = &entry.fault_before_step {
        let missing = || {
            invalid(format!(
                "{}: 手順 {step} がもとのレシピの操作にありません",
                entry.id
            ))
        };
        let actions = recipe["browser"]["actions"]
            .as_array_mut()
            .ok_or_else(missing)?;
        let at = actions
            .iter()
            .position(|a| a["id"] == step.as_str())
            .ok_or_else(missing)?;
        let moved: Vec<_> = actions.drain(..at).collect();
        recipe["browser"]["preparation"]
            .as_array_mut()
            .ok_or_else(|| invalid(format!("{}: もとのレシピに準備がありません", entry.id)))?
            .extend(moved);
    }
    if let Some(keep) = &entry.keep_checks {
        let checks = recipe["trial"]["checks"]
            .as_array_mut()
            .ok_or_else(|| invalid("判定条件がありません"))?;
        match keep {
            Keep::Ids(ids) => {
                for id in ids {
                    if !checks.iter().any(|c| c["id"] == id.as_str()) {
                        return Err(invalid(format!(
                            "{}: 判定条件 {id} がもとのレシピにありません",
                            entry.id
                        )));
                    }
                }
                checks.retain(|c| ids.iter().any(|id| c["id"] == id.as_str()));
            }
            Keep::IdPrefix(prefix) => {
                checks.retain(|c| c["id"].as_str().is_some_and(|id| id.starts_with(prefix)));
                if prefix.is_empty() || checks.is_empty() {
                    return Err(invalid(format!(
                        "{}: 「{prefix}」で始まる判定条件がもとのレシピにありません",
                        entry.id
                    )));
                }
            }
        }
    }
    if !entry.add_checks.is_empty() {
        let checks = recipe["trial"]["checks"]
            .as_array_mut()
            .ok_or_else(|| invalid("判定条件がありません"))?;
        for check in &entry.add_checks {
            if checks.iter().any(|c| c["id"] == check["id"]) {
                return Err(invalid(format!(
                    "{}: 足す判定条件のIDがもとのレシピと重複しています",
                    entry.id
                )));
            }
            checks.push(check.clone());
        }
    }
    let parsed: crate::execution::Recipe =
        serde_json::from_value(recipe.clone()).map_err(io::Error::other)?;
    crate::execution::validate_recipe(&parsed)?;
    Ok(recipe)
}

/// ディレクトリの一覧のファイルをすべて読み、画面へ返す形にする。
pub fn load(directory: &Path) -> io::Result<Value> {
    let root = fs::canonicalize(directory)
        .map_err(|_| invalid(format!("{DIRECTORY_VARIABLE} のディレクトリを読めません")))?;
    let label = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut recipes = vec![];
    let mut ids = BTreeSet::new();
    for file in catalog_files(&root)? {
        let catalog: Catalog = serde_json::from_str(&read_plain(&file)?)
            .map_err(|e| invalid(format!("{}: {e}", file.display())))?;
        if catalog.schema_version != 1
            || catalog.entries.is_empty()
            || catalog.entries.len() > MAX_ENTRIES
        {
            return Err(invalid(format!(
                "{}: 形式の版は1、試験は1〜{MAX_ENTRIES}件で書いてください",
                file.display()
            )));
        }
        text(&catalog.note, "注記")?;
        let relative = Path::new(&catalog.base);
        if catalog.base.is_empty()
            || !relative
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(invalid(format!(
                "{}: もとのレシピは、一覧と同じディレクトリからの相対パスで書いてください",
                file.display()
            )));
        }
        let directory = file.parent().unwrap_or(&root);
        // 途中のディレクトリがリンクなら、指定したディレクトリの外のファイルを読んでしまう。
        // 最後のファイルは read_plain が確かめるので、ここでは途中のディレクトリを確かめる。
        let mut base_path = directory.to_path_buf();
        let parts: Vec<_> = relative.components().collect();
        for (index, part) in parts.iter().enumerate() {
            base_path.push(part);
            if index + 1 < parts.len() && !fs::symlink_metadata(&base_path)?.is_dir() {
                return Err(invalid(format!(
                    "{}: もとのレシピのパスの途中 {} は、リンクではない通常のディレクトリにしてください",
                    file.display(),
                    base_path.display()
                )));
            }
        }
        let base: Value =
            serde_json::from_str(&read_plain(&base_path)?).map_err(io::Error::other)?;
        let origin = base_path
            .strip_prefix(&root)
            .map_err(io::Error::other)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let origin = format!("{label}/{origin}");
        for entry in &catalog.entries {
            text(&entry.title, "題名")?;
            if !entry_id(&entry.id) || !ids.insert(entry.id.clone()) {
                return Err(invalid(format!(
                    "{}: 試験のID {} は英小文字・数字・ハイフンで、重複なく書いてください",
                    file.display(),
                    entry.id
                )));
            }
            let recipe = derive(&base, entry)?;
            recipes.push(json!({"id":entry.id,"title":entry.title,"recipe":recipe,
                "origin":origin,
                "note":catalog.note}));
        }
    }
    Ok(json!({"schema_version":1,"recipes":recipes}))
}

/// 運営者が指定したディレクトリの一覧。未指定なら空を返し、別の場所を探さない。
pub fn catalog_from(directory: Option<&Path>) -> io::Result<Value> {
    match directory {
        Some(directory) => load(directory),
        None => Ok(json!({"schema_version":1,"recipes":[]})),
    }
}

pub fn catalog() -> io::Result<Value> {
    catalog_from(
        std::env::var_os(DIRECTORY_VARIABLE)
            .map(PathBuf::from)
            .as_deref(),
    )
}

#[cfg(test)]
pub(crate) fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一覧をファイルへ移す前の実装。移行後の出力が同じであることを確かめる基準として残す。
    fn embedded_before_externalizing() -> Value {
        let normal: Value =
            serde_json::from_str(include_str!("../examples/openemr-browser/soap-recipe.json"))
                .unwrap();
        let mut stopped = normal.clone();
        stopped["trial"]["fault"] = json!("database_stopped");
        let actions = stopped["browser"]["actions"].as_array_mut().unwrap();
        let save = actions.iter().position(|a| a["id"] == "save-soap").unwrap();
        let preparation: Vec<_> = actions.drain(..save).collect();
        stopped["browser"]["preparation"]
            .as_array_mut()
            .unwrap()
            .extend(preparation);
        stopped["trial"]["checks"]
            .as_array_mut()
            .unwrap()
            .retain(|c| c["id"].as_str().is_some_and(|id| id.starts_with("soap-")));
        let mut logs = normal.clone();
        let checks = logs["trial"]["checks"].as_array_mut().unwrap();
        checks.retain(|c| c["id"] == "soap-subjective-row");
        for (id, scope, target) in [
            (
                "soap-log-app",
                "OpenEMRのコンテナのログ（標準出力・標準エラー）に試験値が出ない",
                "openemr",
            ),
            (
                "soap-log-db",
                "DBのコンテナのログ（標準出力・標準エラー）に試験値が出ない",
                "mysql",
            ),
        ] {
            checks.push(
                json!({"id":id,"scope":scope,"method":"container_log","target":target,
                "expect":"not_contains","text":"{{test_value}}"}),
            );
        }
        let mut recipes = vec![];
        for (id, title, recipe) in [
            ("openemr-soap", "OpenEMR：SOAPの保存と再表示", normal),
            (
                "openemr-soap-db-stopped",
                "OpenEMR：SOAPの保存直前にDBを停止",
                stopped,
            ),
            (
                "openemr-soap-logs",
                "OpenEMR：SOAPの保存でログに試験値が出ないか",
                logs,
            ),
        ] {
            recipes.push(json!({"id":id,"title":title,"recipe":recipe,
                "origin":"examples/openemr-browser/soap-recipe.json",
                "note":"架空の患者と診療記録で試します。Gitの参照版と、実行するイメージのコードが同一であることは未確認です。"}));
        }
        json!({"schema_version":1,"recipes":recipes})
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mugiurushi-catalog-{name}-{}-{}",
            std::process::id(),
            crate::recording::unix_ms().unwrap()
        ));
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::copy(
            examples().join("openemr-browser/soap-recipe.json"),
            dir.join("target/recipe.json"),
        )
        .unwrap();
        dir
    }

    fn write(dir: &Path, entries: Value) {
        fs::write(
            dir.join("target/one.catalog.json"),
            serde_json::to_vec(
                &json!({"schema_version":1,"base":"recipe.json","note":"架空の注記","entries":entries}),
            )
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn file_catalog_matches_the_embedded_output_byte_for_byte() {
        let from_files = load(&examples()).unwrap();
        let before = embedded_before_externalizing();
        assert_eq!(from_files, before);
        // キーの順序も含めて、画面へ返すJSONが変わらない。
        assert_eq!(
            serde_json::to_string(&from_files).unwrap(),
            serde_json::to_string(&before).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_base_recipe_behind_a_linked_directory_is_rejected() {
        // 一覧のあるディレクトリの外に、形の正しいレシピを置く。
        let dir = temp("linked-dir");
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::copy(dir.join("target/recipe.json"), outside.join("recipe.json")).unwrap();
        let catalog = |base: &str| {
            fs::write(
                dir.join("target/one.catalog.json"),
                serde_json::to_vec(&json!({"schema_version":1,"base":base,"note":"架空の注記",
                    "entries":[{"id":"linked","title":"リンクの先"}]}))
                .unwrap(),
            )
            .unwrap();
        };
        // 途中のディレクトリが通常のディレクトリなら読める。
        fs::create_dir_all(dir.join("target/plain")).unwrap();
        fs::copy(
            dir.join("target/recipe.json"),
            dir.join("target/plain/recipe.json"),
        )
        .unwrap();
        catalog("plain/recipe.json");
        assert_eq!(
            load(&dir.join("target")).unwrap()["recipes"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // 途中のディレクトリが、外を指すリンクなら拒否する。最後のファイルは通常のファイルである。
        std::os::unix::fs::symlink(&outside, dir.join("target/linked")).unwrap();
        catalog("linked/recipe.json");
        let error = load(&dir.join("target")).unwrap_err().to_string();
        assert!(
            error.contains("リンクではない通常のディレクトリ"),
            "{error}"
        );
    }

    #[test]
    fn an_unset_directory_lists_nothing_instead_of_a_built_in_target() {
        let value = catalog_from(None).unwrap();
        assert_eq!(value["recipes"].as_array().unwrap().len(), 0);
        assert!(load(Path::new("/nonexistent/mugiurushi-recipes")).is_err());
    }

    #[test]
    fn log_recipe_keeps_the_save_precondition_and_checks_both_containers() {
        let value = load(&examples()).unwrap();
        let logs = value["recipes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "openemr-soap-logs")
            .unwrap();
        let checks = logs["recipe"]["trial"]["checks"].as_array().unwrap();
        let ids: Vec<_> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["soap-subjective-row", "soap-log-app", "soap-log-db"]);
        assert!(checks[1..].iter().all(|c| c["method"] == "container_log"
            && c["expect"] == "not_contains"
            && c["text"] == "{{test_value}}"));
        assert_eq!(
            logs["recipe"]["browser"],
            value["recipes"][0]["recipe"]["browser"]
        );
    }

    #[test]
    fn stopping_database_prepares_soap_but_still_attempts_save() {
        let value = load(&examples()).unwrap();
        let normal = &value["recipes"][0]["recipe"];
        let stopped = &value["recipes"][1]["recipe"];
        assert_eq!(normal["trial"]["fault"], "normal");
        assert_eq!(stopped["trial"]["fault"], "database_stopped");
        assert_eq!(stopped["browser"]["actions"][0]["id"], "save-soap");
        assert!(
            stopped["browser"]["preparation"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["id"] != "save-soap")
        );
        assert_eq!(normal["source"], stopped["source"]);
        assert_eq!(normal["containers"], stopped["containers"]);
        assert!(
            stopped["trial"]["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["id"].as_str().unwrap().starts_with("soap-"))
        );
    }

    #[test]
    fn a_declared_step_or_check_that_the_recipe_lacks_is_rejected() {
        let dir = temp("missing");
        for entries in [
            json!([{"id":"a","title":"架空","fault_before_step":"no-such-step"}]),
            json!([{"id":"a","title":"架空","keep_checks":{"ids":["no-such-check"]}}]),
            json!([{"id":"a","title":"架空","keep_checks":{"id_prefix":"no-such-"}}]),
            json!([{"id":"a","title":"架空","keep_checks":{"id_prefix":""}}]),
            // 足す条件のIDが既存と重なる。
            json!([{"id":"a","title":"架空","add_checks":[{"id":"soap-subjective-row",
                "scope":"架空","method":"container_log","expect":"not_contains","text":"x"}]}]),
            // 判定に使えない組み合わせの条件は、レシピの検査で落ちる。
            json!([{"id":"a","title":"架空","add_checks":[{"id":"new",
                "scope":"架空","method":"exit_code","expect":"contains","text":"x"}]}]),
            json!([{"id":"a","title":"架空"},{"id":"a","title":"架空"}]),
            json!([{"id":"Bad ID","title":"架空"}]),
            json!([{"id":"a","title":""}]),
            json!([{"id":"a","title":"架空","unknown":true}]),
            json!([]),
        ] {
            write(&dir, entries.clone());
            assert!(load(&dir).is_err(), "{entries}");
        }
        write(&dir, json!([{"id":"a","title":"架空"}]));
        let ok = load(&dir).unwrap();
        assert_eq!(ok["recipes"][0]["note"], "架空の注記");
        assert!(
            ok["recipes"][0]["origin"]
                .as_str()
                .unwrap()
                .ends_with("/target/recipe.json")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_base_outside_the_catalog_directory_or_behind_a_link_is_rejected() {
        let dir = temp("escape");
        for base in [
            "../target/recipe.json",
            "/etc/hostname",
            "",
            "./recipe.json",
        ] {
            fs::write(
                dir.join("target/one.catalog.json"),
                serde_json::to_vec(&json!({"schema_version":1,"base":base,"note":"架空",
                    "entries":[{"id":"a","title":"架空"}]}))
                .unwrap(),
            )
            .unwrap();
            assert!(load(&dir).is_err(), "{base}");
        }
        std::os::unix::fs::symlink(dir.join("target/recipe.json"), dir.join("target/link.json"))
            .unwrap();
        fs::write(
            dir.join("target/one.catalog.json"),
            serde_json::to_vec(&json!({"schema_version":1,"base":"link.json","note":"架空",
                "entries":[{"id":"a","title":"架空"}]}))
            .unwrap(),
        )
        .unwrap();
        assert!(load(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
