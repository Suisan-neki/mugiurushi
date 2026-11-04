//! 画面で選ぶ同梱ブラウザ試験。選択・閲覧だけでは取得も実行も行わない。
use serde_json::{Value, json};
use std::io;

pub fn catalog() -> io::Result<Value> {
    let normal: Value =
        serde_json::from_str(include_str!("../examples/openemr-browser/soap-recipe.json"))
            .map_err(io::Error::other)?;
    let mut stopped = normal.clone();
    stopped["trial"]["fault"] = json!("database_stopped");
    let actions = stopped["browser"]["actions"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("SOAP試験の操作がありません"))?;
    let save = actions
        .iter()
        .position(|a| a["id"] == "save-soap")
        .ok_or_else(|| io::Error::other("SOAPの保存操作がありません"))?;
    let preparation: Vec<_> = actions.drain(..save).collect();
    stopped["browser"]["preparation"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("SOAP試験の準備がありません"))?
        .extend(preparation);
    stopped["trial"]["checks"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("SOAP試験の条件がありません"))?
        .retain(|c| c["id"].as_str().is_some_and(|id| id.starts_with("soap-")));
    // 保存の操作は通常時と同じ。保存できたこと（DBの行）を前提に、アプリとDBのコンテナの
    // 標準出力・標準エラーのログに試験値が出ないかを確かめる。保存できていなければ、
    // ログに無いことは何も示さないため、DBの行の条件を残す。
    let mut logs = normal.clone();
    let checks = logs["trial"]["checks"]
        .as_array_mut()
        .ok_or_else(|| io::Error::other("SOAP試験の条件がありません"))?;
    checks.retain(|c| c["id"] == "soap-subjective-row");
    if checks.len() != 1 {
        return Err(io::Error::other("SOAPの保存を確かめる条件がありません"));
    }
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
        let parsed: crate::execution::Recipe =
            serde_json::from_value(recipe.clone()).map_err(io::Error::other)?;
        crate::execution::validate_recipe(&parsed)?;
        let origin = "examples/openemr-browser/soap-recipe.json";
        recipes.push(json!({"id":id,"title":title,"recipe":recipe,
            "origin":origin,
            "note":"架空の患者と診療記録で試します。Gitの参照版と、実行するイメージのコードが同一であることは未確認です。"}));
    }
    Ok(json!({"schema_version":1,"recipes":recipes}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log_recipe_keeps_the_save_precondition_and_checks_both_containers() {
        let value = catalog().unwrap();
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
        let value = catalog().unwrap();
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
}
