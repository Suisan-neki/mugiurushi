//! 基盤が試験用のDBを直接読むときの、DBの種類ごとの差。
//! 種類で変わるのは、クライアントの起動、認証情報の渡し方、識別子の囲み方、値の渡し方の4点だけ。
//! 種類はコンテナの設定で明示する。イメージの名前からは推定しない。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseEngine {
    /// 指定のない既存の設定はこの種類として読む。
    #[default]
    Mariadb,
    Postgres,
    /// 基盤が読み出しに対応していない種類。起動と故障の操作はできるが、行は読まない。
    Other,
}

/// コンテナの中で起動するクライアント。`env` は `docker exec --env` で渡す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub env: Vec<String>,
    pub argv: Vec<String>,
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_owned()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl DatabaseEngine {
    pub fn is_default(&self) -> bool {
        *self == Self::Mariadb
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mariadb => "MariaDB",
            Self::Postgres => "PostgreSQL",
            Self::Other => "基盤が読み出しに対応していない種類",
        }
    }

    fn postgres_user(env: &BTreeMap<String, String>) -> String {
        env.get("POSTGRES_USER")
            .cloned()
            .unwrap_or_else(|| "postgres".into())
    }

    fn base(self, env: &BTreeMap<String, String>) -> Option<Client> {
        match self {
            Self::Mariadb => {
                let password = env
                    .get("MYSQL_ROOT_PASSWORD")
                    .or_else(|| env.get("MARIADB_ROOT_PASSWORD"));
                Some(Client {
                    env: vec![format!(
                        "MYSQL_PWD={}",
                        password.map(String::as_str).unwrap_or("")
                    )],
                    argv: strings(&["mariadb", "--user=root", "--connect-timeout=5"]),
                })
            }
            Self::Postgres => {
                let user = Self::postgres_user(env);
                let mut argv = strings(&[
                    "psql",
                    "--no-psqlrc",
                    "--no-password",
                    "--quiet",
                    "--no-align",
                    "--tuples-only",
                    "--field-separator=\t",
                    "--pset=null=NULL",
                    "--set=ON_ERROR_STOP=1",
                ]);
                argv.push(format!("--username={user}"));
                Some(Client {
                    env: vec![
                        format!(
                            "PGPASSWORD={}",
                            env.get("POSTGRES_PASSWORD")
                                .map(String::as_str)
                                .unwrap_or("")
                        ),
                        "PGCONNECT_TIMEOUT=5".into(),
                    ],
                    argv,
                })
            }
            Self::Other => None,
        }
    }

    /// 接続できることだけを確かめる照会。読めない種類は `None`。
    pub fn ping(self, env: &BTreeMap<String, String>) -> Option<Client> {
        let mut client = self.base(env)?;
        match self {
            Self::Mariadb => client.argv.extend(strings(&["-N", "-e", "SELECT 1"])),
            Self::Postgres => {
                let database = env
                    .get("POSTGRES_DB")
                    .cloned()
                    .unwrap_or_else(|| Self::postgres_user(env));
                client.argv.push(format!("--dbname={database}"));
                client.argv.extend(strings(&["--command", "SELECT 1"]));
            }
            Self::Other => return None,
        }
        Some(client)
    }

    /// 指定したデータベースでSQLを1つ実行し、行をタブ区切りで出力するクライアント。
    pub fn run(self, env: &BTreeMap<String, String>, database: &str, sql: &str) -> Option<Client> {
        let mut client = self.base(env)?;
        match self {
            Self::Mariadb => {
                client.argv.extend(strings(&[
                    "--batch",
                    "--skip-column-names",
                    "--database",
                    database,
                    "--execute",
                    sql,
                ]));
            }
            Self::Postgres => {
                client.argv.push(format!("--dbname={database}"));
                client.argv.extend(strings(&["--command", sql]));
            }
            Self::Other => return None,
        }
        Some(client)
    }

    /// 検査済みの識別子を囲む。呼び出し側が `browser::sql_identifier` で文字を限定する。
    pub fn quote(self, identifier: &str) -> String {
        match self {
            Self::Postgres => format!("\"{identifier}\""),
            Self::Mariadb | Self::Other => format!("`{identifier}`"),
        }
    }

    /// 値をSQLへ文字列として埋め込まず、16進の形で渡す。
    pub fn literal(self, value: &[u8]) -> String {
        match self {
            Self::Postgres => format!("convert_from(decode('{}','hex'),'UTF8')", hex(value)),
            Self::Mariadb | Self::Other => format!("CONVERT(0x{} USING utf8mb4)", hex(value)),
        }
    }

    /// 指定した列が値と等しい行を、最大100行読むSQL。
    pub fn rows_sql(
        self,
        table: &str,
        match_column: &str,
        columns: &[String],
        value: &str,
    ) -> String {
        let columns = columns
            .iter()
            .map(|c| self.quote(c))
            .collect::<Vec<_>>()
            .join(",");
        let column = match self {
            // 文字列でない列も、MariaDBと同じく文字列として比べる。
            Self::Postgres => format!("{}::text", self.quote(match_column)),
            Self::Mariadb | Self::Other => self.quote(match_column),
        };
        format!(
            "SELECT {columns} FROM {} WHERE {column} = {} LIMIT 100",
            self.quote(table),
            self.literal(value.as_bytes())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn mariadb_commands_and_sql_are_unchanged_from_before_the_engine_setting() {
        let e = env(&[("MYSQL_ROOT_PASSWORD", "eel-root-trial")]);
        let ping = DatabaseEngine::Mariadb.ping(&e).unwrap();
        assert_eq!(ping.env, ["MYSQL_PWD=eel-root-trial"]);
        assert_eq!(
            ping.argv,
            [
                "mariadb",
                "--user=root",
                "--connect-timeout=5",
                "-N",
                "-e",
                "SELECT 1"
            ]
        );
        let run = DatabaseEngine::Mariadb.run(&e, "app", "SELECT 2").unwrap();
        assert_eq!(
            run.argv,
            [
                "mariadb",
                "--user=root",
                "--connect-timeout=5",
                "--batch",
                "--skip-column-names",
                "--database",
                "app",
                "--execute",
                "SELECT 2"
            ]
        );
        let alternative = env(&[("MARIADB_ROOT_PASSWORD", "other")]);
        assert_eq!(
            DatabaseEngine::Mariadb.ping(&alternative).unwrap().env,
            ["MYSQL_PWD=other"]
        );
        assert_eq!(
            DatabaseEngine::Mariadb.rows_sql("t", "m", &["a".into(), "b".into()], "é'"),
            "SELECT `a`,`b` FROM `t` WHERE `m` = CONVERT(0xc3a927 USING utf8mb4) LIMIT 100"
        );
    }

    #[test]
    fn postgres_uses_its_own_client_quoting_and_literal() {
        let e = env(&[
            ("POSTGRES_USER", "clinic"),
            ("POSTGRES_PASSWORD", "eel-pg-trial"),
            ("POSTGRES_DB", "records"),
        ]);
        let ping = DatabaseEngine::Postgres.ping(&e).unwrap();
        assert_eq!(ping.env, ["PGPASSWORD=eel-pg-trial", "PGCONNECT_TIMEOUT=5"]);
        assert_eq!(ping.argv[0], "psql");
        assert!(ping.argv.contains(&"--username=clinic".to_owned()));
        assert!(ping.argv.contains(&"--dbname=records".to_owned()));
        assert!(ping.argv.contains(&"--no-password".to_owned()));
        let run = DatabaseEngine::Postgres
            .run(&e, "other", "SELECT 2")
            .unwrap();
        assert!(run.argv.contains(&"--dbname=other".to_owned()));
        assert_eq!(&run.argv[run.argv.len() - 2..], ["--command", "SELECT 2"]);
        // 既定のユーザーとデータベースは公式イメージの既定に合わせる。
        let defaults = DatabaseEngine::Postgres.ping(&env(&[])).unwrap();
        assert!(defaults.argv.contains(&"--username=postgres".to_owned()));
        assert!(defaults.argv.contains(&"--dbname=postgres".to_owned()));
        // 値は引用符を含んでも16進で渡り、SQLの文字列として解釈されない。
        assert_eq!(
            DatabaseEngine::Postgres.rows_sql("t", "m", &["a".into()], "x'; DROP TABLE t;--"),
            "SELECT \"a\" FROM \"t\" WHERE \"m\"::text = convert_from(decode('78273b2044524f50205441424c4520743b2d2d','hex'),'UTF8') LIMIT 100"
        );
    }

    #[test]
    fn an_unsupported_engine_has_no_client_and_the_default_is_omitted_when_saved() {
        assert!(DatabaseEngine::Other.ping(&env(&[])).is_none());
        assert!(
            DatabaseEngine::Other
                .run(&env(&[]), "d", "SELECT 1")
                .is_none()
        );
        assert!(DatabaseEngine::default().is_default());
        assert!(!DatabaseEngine::Postgres.is_default());
        assert_eq!(
            serde_json::from_str::<DatabaseEngine>("\"postgres\"").unwrap(),
            DatabaseEngine::Postgres
        );
        assert!(serde_json::from_str::<DatabaseEngine>("\"oracle\"").is_err());
    }
}
