//! 基盤が試験用のDBを直接読むときの、DBの種類ごとの差。
//! 種類で変わるのは、クライアントの起動、認証情報の渡し方、識別子の囲み方、値の渡し方の4点だけ。
//! 種類はコンテナの設定で明示する。イメージの名前からは推定しない。
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

/// 期限つきの照会が最初に出す行の目印。続けて、その接続のサーバー側の番号が並ぶ。
pub const SESSION_MARK: &str = "#session";
/// 期限つきの照会の接続に付ける名前。PostgreSQLで、止める相手を取り違えないために使う。
const LIMITED_APPLICATION: &str = "mugiurushi-scan";
/// 接続を止める操作そのものに、サーバー側で許す時間。
const CANCEL_STATEMENT_MS: u64 = 4000;

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

    /// 複数のSQLを、サーバー側の制限時間つきで順に実行する。
    /// 最初に接続の番号を出力するので、ホストの側で待つのをやめた後に、その接続を名指しで止められる。
    /// 制限時間は、実行中の文と、表の施錠を待つ時間の両方に効く。クライアントの接続の制限だけでは、これらは止まらない。
    pub fn run_limited(
        self,
        env: &BTreeMap<String, String>,
        database: &str,
        statements: &[String],
        limit: Duration,
    ) -> Option<Client> {
        let mut client = self.base(env)?;
        let millis = limit.as_millis().clamp(1, 3_600_000) as u64;
        match self {
            Self::Mariadb => {
                let seconds = millis.div_ceil(1000);
                let mut sql = format!(
                    "SET SESSION max_statement_time={}.{:03},lock_wait_timeout={seconds},innodb_lock_wait_timeout={seconds};\
                     SELECT '{SESSION_MARK}',CONNECTION_ID()",
                    millis / 1000,
                    millis % 1000
                );
                for statement in statements {
                    sql.push(';');
                    sql.push_str(statement);
                }
                // 止められた接続をクライアントがつなぎ直して、続きの文を実行しないようにする。
                // 結果は文ごとに書き出させ、途中までの答えと接続の番号を受け取れるようにする。
                client.argv.extend(strings(&[
                    "--skip-reconnect",
                    "--unbuffered",
                    "--batch",
                    "--skip-column-names",
                    "--database",
                    database,
                    "--execute",
                ]));
                client.argv.push(sql);
            }
            Self::Postgres => {
                client.env.push(format!("PGAPPNAME={LIMITED_APPLICATION}"));
                client.env.push(format!(
                    "PGOPTIONS=-c statement_timeout={millis} -c lock_timeout={millis}"
                ));
                client.argv.push(format!("--dbname={database}"));
                client.argv.push("--command".into());
                client
                    .argv
                    .push(format!("SELECT '{SESSION_MARK}', pg_backend_pid()"));
                for statement in statements {
                    client.argv.push("--command".into());
                    client.argv.push(statement.clone());
                }
            }
            Self::Other => return None,
        }
        Some(client)
    }

    /// 期限つきの照会の出力から、接続の番号を読む。
    pub fn session_of(stdout: &str) -> Option<u64> {
        stdout.lines().find_map(|line| {
            let mut fields = line.split('\t');
            (fields.next()? == SESSION_MARK).then_some(())?;
            fields.next()?.trim().parse().ok()
        })
    }

    /// 番号で指した接続を止め、その接続が残っている数を数える。最後の行が `0` なら残っていない。
    /// 番号は `session_of` で読んだ数だけを受け取る。
    pub fn cancel(
        self,
        env: &BTreeMap<String, String>,
        database: &str,
        session: u64,
    ) -> Option<Client> {
        let mut client = self.base(env)?;
        match self {
            Self::Mariadb => {
                let left = format!(
                    "SELECT COUNT(*) FROM information_schema.processlist WHERE id={session}"
                );
                // 止める相手がすでにいない場合の誤りでは止まらず、数えるところまで進む。
                client.argv.extend(strings(&[
                    "--force",
                    "--batch",
                    "--skip-column-names",
                    "--database",
                    database,
                    "--execute",
                ]));
                client.argv.push(format!(
                    "SET SESSION max_statement_time={}.{:03};KILL {session};DO SLEEP(0.2);{left};DO SLEEP(0.6);{left}",
                    CANCEL_STATEMENT_MS / 1000,
                    CANCEL_STATEMENT_MS % 1000
                ));
            }
            Self::Postgres => {
                let mine = format!(
                    "FROM pg_stat_activity WHERE pid={session} AND application_name='{LIMITED_APPLICATION}'"
                );
                client.env.push(format!(
                    "PGOPTIONS=-c statement_timeout={CANCEL_STATEMENT_MS}"
                ));
                client.argv.push(format!("--dbname={database}"));
                for sql in [
                    format!("SELECT pg_terminate_backend(pid) {mine}"),
                    "SELECT pg_sleep(0.2)".to_owned(),
                    format!("SELECT count(*) {mine}"),
                    "SELECT pg_sleep(0.6)".to_owned(),
                    format!("SELECT count(*) {mine}"),
                ] {
                    client.argv.push("--command".into());
                    client.argv.push(sql);
                }
            }
            Self::Other => return None,
        }
        Some(client)
    }

    /// `cancel` の出力から、止めた接続が残っているかを読む。読めない場合は `None`。
    pub fn cancelled(stdout: &str) -> Option<bool> {
        let last = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
        last.trim().parse::<u64>().ok().map(|left| left == 0)
    }

    /// 探索のときに接続するデータベース。MariaDBは表をスキーマ付きで指すので、どこからでも読める。
    pub fn scan_database(self, env: &BTreeMap<String, String>) -> String {
        match self {
            Self::Postgres => env
                .get("POSTGRES_DB")
                .cloned()
                .unwrap_or_else(|| Self::postgres_user(env)),
            Self::Mariadb | Self::Other => "information_schema".into(),
        }
    }

    /// 利用者の表の、文字列型の列を「スキーマ・表・列」のタブ区切りで並べるSQL。
    pub fn text_columns_sql(self) -> &'static str {
        match self {
            Self::Postgres => {
                "SELECT table_schema, table_name, column_name FROM information_schema.columns \
                 WHERE table_schema NOT IN ('pg_catalog','information_schema') \
                 AND data_type IN ('character varying','character','text') \
                 ORDER BY table_schema, table_name, ordinal_position"
            }
            Self::Mariadb | Self::Other => {
                "SELECT table_schema, table_name, column_name FROM information_schema.columns \
                 WHERE table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
                 AND data_type IN ('char','varchar','tinytext','text','mediumtext','longtext') \
                 ORDER BY table_schema, table_name, ordinal_position"
            }
        }
    }

    /// 1つの表を1回走査し、列ごと・値ごとに「その値を含む行の数」と「その値と等しい行の数」を数えるSQL。
    /// 出力は1行で、先頭に目印、続いて列の順・値の順に、含む数・等しい数が並ぶ。値は16進で渡す。
    pub fn count_sql(
        self,
        tag: usize,
        schema: &str,
        table: &str,
        columns: &[String],
        values: &[String],
    ) -> String {
        let counts = columns
            .iter()
            .flat_map(|column| {
                values.iter().map(move |value| {
                    let literal = self.literal(value.as_bytes());
                    // MariaDBでは、列の文字コードが値と違うと比較が失敗する。列を変換してから比べる。
                    let column = match self {
                        Self::Postgres => self.quote(column),
                        Self::Mariadb | Self::Other => {
                            format!("CONVERT({} USING utf8mb4)", self.quote(column))
                        }
                    };
                    match self {
                        Self::Postgres => format!(
                            "COUNT(*) FILTER (WHERE POSITION({literal} IN {column}) > 0),\
                             COUNT(*) FILTER (WHERE {column} = {literal})"
                        ),
                        Self::Mariadb | Self::Other => format!(
                            "COALESCE(SUM(INSTR({column}, {literal}) > 0), 0),\
                             COALESCE(SUM({column} = {literal}), 0)"
                        ),
                    }
                })
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "SELECT {tag},{counts} FROM {}.{}",
            self.quote(schema),
            self.quote(table)
        )
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

    /// 指定した列が値と等しい行を、最大100行読むSQL。`schema` を指定すると、表をスキーマの名前つきで指す。
    pub fn rows_sql(
        self,
        schema: Option<&str>,
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
        let table = match schema {
            Some(schema) => format!("{}.{}", self.quote(schema), self.quote(table)),
            None => self.quote(table),
        };
        format!(
            "SELECT {columns} FROM {table} WHERE {column} = {} LIMIT 100",
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
            DatabaseEngine::Mariadb.rows_sql(None, "t", "m", &["a".into(), "b".into()], "é'"),
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
            DatabaseEngine::Postgres.rows_sql(None, "t", "m", &["a".into()], "x'; DROP TABLE t;--"),
            "SELECT \"a\" FROM \"t\" WHERE \"m\"::text = convert_from(decode('78273b2044524f50205441424c4520743b2d2d','hex'),'UTF8') LIMIT 100"
        );
        // スキーマを指定すると、表をスキーマの名前つきで指す。指定しない場合のSQLは変わらない。
        assert_eq!(
            DatabaseEngine::Postgres.rows_sql(Some("clinic"), "t", "m", &["a".into()], "x"),
            "SELECT \"a\" FROM \"clinic\".\"t\" WHERE \"m\"::text = convert_from(decode('78','hex'),'UTF8') LIMIT 100"
        );
    }

    #[test]
    fn scan_statements_count_each_column_and_value_in_one_pass() {
        let columns = ["a".to_owned(), "b".to_owned()];
        let values = ["x".to_owned(), "y'".to_owned()];
        assert_eq!(
            DatabaseEngine::Mariadb.count_sql(3, "app", "t", &columns[..1], &values),
            "SELECT 3,COALESCE(SUM(INSTR(CONVERT(`a` USING utf8mb4), CONVERT(0x78 USING utf8mb4)) > 0), 0),\
             COALESCE(SUM(CONVERT(`a` USING utf8mb4) = CONVERT(0x78 USING utf8mb4)), 0),\
             COALESCE(SUM(INSTR(CONVERT(`a` USING utf8mb4), CONVERT(0x7927 USING utf8mb4)) > 0), 0),\
             COALESCE(SUM(CONVERT(`a` USING utf8mb4) = CONVERT(0x7927 USING utf8mb4)), 0) FROM `app`.`t`"
        );
        let pg = DatabaseEngine::Postgres.count_sql(0, "public", "t", &columns[..1], &values[..1]);
        assert_eq!(
            pg,
            "SELECT 0,COUNT(*) FILTER (WHERE POSITION(convert_from(decode('78','hex'),'UTF8') IN \"a\") > 0),\
             COUNT(*) FILTER (WHERE \"a\" = convert_from(decode('78','hex'),'UTF8')) FROM \"public\".\"t\""
        );
        assert_eq!(
            DatabaseEngine::Mariadb.scan_database(&env(&[])),
            "information_schema"
        );
        assert_eq!(
            DatabaseEngine::Postgres.scan_database(&env(&[("POSTGRES_DB", "records")])),
            "records"
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

    #[test]
    fn limited_runs_report_their_session_and_carry_the_time_limit_to_the_server() {
        let statements = ["SELECT 1".to_owned(), "SELECT 2".to_owned()];
        let maria = DatabaseEngine::Mariadb
            .run_limited(
                &env(&[]),
                "information_schema",
                &statements,
                Duration::from_millis(2500),
            )
            .unwrap();
        // つなぎ直して続きを実行しない。結果は文ごとに書き出す。
        assert!(maria.argv.iter().any(|a| a == "--skip-reconnect"));
        assert!(maria.argv.iter().any(|a| a == "--unbuffered"));
        assert_eq!(
            maria.argv.last().unwrap(),
            "SET SESSION max_statement_time=2.500,lock_wait_timeout=3,innodb_lock_wait_timeout=3;\
             SELECT '#session',CONNECTION_ID();SELECT 1;SELECT 2"
        );
        let pg = DatabaseEngine::Postgres
            .run_limited(
                &env(&[]),
                "records",
                &statements,
                Duration::from_millis(2500),
            )
            .unwrap();
        assert!(pg.env.iter().any(|e| e == "PGAPPNAME=mugiurushi-scan"));
        assert!(
            pg.env
                .iter()
                .any(|e| e == "PGOPTIONS=-c statement_timeout=2500 -c lock_timeout=2500")
        );
        assert_eq!(
            &pg.argv[pg.argv.len() - 6..],
            [
                "--command",
                "SELECT '#session', pg_backend_pid()",
                "--command",
                "SELECT 1",
                "--command",
                "SELECT 2"
            ]
        );
        // 0の制限時間は「制限なし」を意味するので、渡さない。
        let shortest = DatabaseEngine::Mariadb
            .run_limited(&env(&[]), "d", &statements, Duration::ZERO)
            .unwrap();
        assert!(shortest.argv.last().unwrap().starts_with(
            "SET SESSION max_statement_time=0.001,lock_wait_timeout=1,innodb_lock_wait_timeout=1;"
        ));
        assert!(
            DatabaseEngine::Other
                .run_limited(&env(&[]), "d", &statements, Duration::from_secs(1))
                .is_none()
        );
    }

    #[test]
    fn a_session_is_cancelled_by_its_number_and_the_leftover_is_counted() {
        assert_eq!(
            DatabaseEngine::session_of("#session\t41\n0\t1\t1\n"),
            Some(41)
        );
        assert_eq!(DatabaseEngine::session_of("0\t1\t1\n"), None);
        assert_eq!(DatabaseEngine::session_of("#session\tx\n"), None);
        let maria = DatabaseEngine::Mariadb
            .cancel(&env(&[]), "information_schema", 41)
            .unwrap();
        assert!(maria.argv.iter().any(|a| a == "--force"));
        let sql = maria.argv.last().unwrap();
        assert!(sql.contains(";KILL 41;"), "{sql}");
        assert!(sql.ends_with("SELECT COUNT(*) FROM information_schema.processlist WHERE id=41"));
        let pg = DatabaseEngine::Postgres
            .cancel(&env(&[]), "records", 41)
            .unwrap();
        // 止める操作の接続は、止める相手と同じ名前を名乗らない。
        assert!(!pg.env.iter().any(|e| e.starts_with("PGAPPNAME=")));
        assert!(pg.argv.iter().any(|a| a
            == "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE pid=41 AND application_name='mugiurushi-scan'"));
        assert!(DatabaseEngine::Other.cancel(&env(&[]), "d", 41).is_none());
        // 最後の行が0のときだけ、残っていないと読む。
        assert_eq!(DatabaseEngine::cancelled("1\n0\n"), Some(true));
        assert_eq!(DatabaseEngine::cancelled("t\n\n1\n\n1\n"), Some(false));
        assert_eq!(DatabaseEngine::cancelled(""), None);
        assert_eq!(DatabaseEngine::cancelled("ERROR\n"), None);
    }
}
