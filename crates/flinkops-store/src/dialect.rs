//! 数据库方言：决定占位符写法与哪些维护操作可用。
//!
//! 背景：store 层全篇用 `?` 占位符，而 `sqlx::Any` **不会**改写占位符 ——
//! SQLite / MySQL 原生就用 `?`，PostgreSQL 必须写成 `$1`、`$2`。所以只要 DSN 指向
//! Postgres，第一条带参语句就会在 Parse 阶段失败。这里在**发送前**按方言改写，
//! 让同一套 SQL 在三种库上都能跑。
use std::borrow::Cow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
    MySql,
}

impl Dialect {
    /// 从 DSN 判断方言。认不出来直接报错（比"连上了再莫名失败"好）。
    pub fn from_dsn(dsn: &str) -> anyhow::Result<Self> {
        let d = dsn.trim();
        if d.starts_with("sqlite:") {
            Ok(Dialect::Sqlite)
        } else if d.starts_with("postgres://") || d.starts_with("postgresql://") {
            Ok(Dialect::Postgres)
        } else if d.starts_with("mysql://") || d.starts_with("mariadb://") {
            Ok(Dialect::MySql)
        } else {
            anyhow::bail!("无法识别的 FLINKOPS_STORE（{d}）：支持 sqlite://、postgres://、mysql://")
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Dialect::Sqlite => "sqlite",
            Dialect::Postgres => "postgres",
            Dialect::MySql => "mysql",
        }
    }

    /// 是否支持 SQLite 专属维护操作（VACUUM / 文件体积 / 目录创建）。
    pub fn is_sqlite(self) -> bool {
        matches!(self, Dialect::Sqlite)
    }

    /// 建表语句的方言改写（只影响 MySQL）。
    ///
    /// MySQL 的两条硬限制：TTEXT 列不能做主键（要前缀长度）、TEXT 列不能有 DEFAULT。
    /// 其它方言原样返回。
    pub fn ddl(self, sql: &str) -> Cow<'_, str> {
        if self != Dialect::MySql || !sql.contains("TEXT") {
            return Cow::Borrowed(sql);
        }
        // sqlx::Any 把 MySQL 的 TEXT/BLOB 统一映射成 Blob，String 解不出来
        // （实测：mismatched types; Rust type String is not compatible with SQL type BLOB）。
        // 所以 MySQL 上**所有**字符串列都要建成 VARCHAR。
        let mut out = sql.to_string();

        // 1) 主键/唯一键列：191 是 utf8mb4 下的安全前缀长度
        let mut keyed: Vec<String> = Vec::new();
        for marker in ["PRIMARY KEY (", "UNIQUE ("] {
            let mut rest = out.as_str();
            while let Some(pos) = rest.find(marker) {
                let start = pos + marker.len();
                let Some(end) = rest[start..].find(')') else {
                    break;
                };
                for col in rest[start..start + end].split(',') {
                    let col = col.trim();
                    if !col.is_empty() {
                        keyed.push(col.to_string());
                    }
                }
                rest = &rest[start + end..];
            }
        }
        for col in &keyed {
            out = out.replace(&format!("{col} TEXT"), &format!("{col} VARCHAR(191)"));
        }

        // 2) 长文本列：MySQL 的 TEXT 在 sqlx::Any 里会被当成 Blob（String 解不出来），
        //    所以这些列的读取要走 bytes→utf8（见 store::text_col）。这里保持 TEXT，
        //    也顺便避开 VARCHAR 的行宽上限。
        for col in [
            "sql_text",
            "payload",
            "conditions",
            "detail",
            "args",
            "args_json",
            "yaml",
            "tables_json",
            "notes",
            "err_text",
            "last_error",
        ] {
            out = out.replace(&format!("{col} TEXT"), &format!("{col} MEDIUMTEXT"));
        }

        // 3) 带 DEFAULT 的列（MySQL 的 TEXT 不能有默认值）
        out = out.replace("TEXT NOT NULL DEFAULT", "VARCHAR(1024) NOT NULL DEFAULT");

        // 4) 其余字符串列一律 VARCHAR(1024)
        out = out.replace(" TEXT", " VARCHAR(255)");

        if out == sql {
            Cow::Borrowed(sql)
        } else {
            Cow::Owned(out)
        }
    }

    /// 把 `?` 占位符改写成该方言的形式。
    ///
    /// 只改**占位符**：跳过单引号字符串、双引号标识符、`--` 行注释与 `/* */` 块注释，
    /// 避免把 `"a?b"` 这类字面量里的问号也替换掉。SQLite / MySQL 原样返回（零拷贝）。
    pub fn placeholders(self, sql: &str) -> Cow<'_, str> {
        if self != Dialect::Postgres || !sql.contains('?') {
            return Cow::Borrowed(sql);
        }

        let mut out = String::with_capacity(sql.len() + 8);
        let mut n = 0usize;
        let bytes = sql.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let c = bytes[i] as char;
            match c {
                '\'' | '"' => {
                    // 字符串/标识符：整段原样搬运，支持 '' 与 "" 转义
                    out.push(c);
                    i += 1;
                    while i < bytes.len() {
                        let ch = bytes[i] as char;
                        out.push(ch);
                        if ch == c {
                            if i + 1 < bytes.len() && bytes[i + 1] as char == c {
                                out.push(c);
                                i += 2;
                                continue;
                            }
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                }
                '-' if i + 1 < bytes.len() && bytes[i + 1] as char == '-' => {
                    while i < bytes.len() && bytes[i] as char != '\n' {
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                }
                '/' if i + 1 < bytes.len() && bytes[i + 1] as char == '*' => {
                    while i < bytes.len() {
                        out.push(bytes[i] as char);
                        if bytes[i] as char == '*'
                            && i + 1 < bytes.len()
                            && bytes[i + 1] as char == '/'
                        {
                            out.push('/');
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                '?' => {
                    n += 1;
                    out.push('$');
                    out.push_str(&n.to_string());
                    i += 1;
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        Cow::Owned(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dialect_from_dsn() {
        assert_eq!(
            Dialect::from_dsn("sqlite:///tmp/x.db").unwrap(),
            Dialect::Sqlite
        );
        assert_eq!(
            Dialect::from_dsn("postgres://u:p@h:5432/db").unwrap(),
            Dialect::Postgres
        );
        assert_eq!(
            Dialect::from_dsn("postgresql://u@h/db").unwrap(),
            Dialect::Postgres
        );
        assert_eq!(Dialect::from_dsn("mysql://u@h/db").unwrap(), Dialect::MySql);
        assert!(Dialect::from_dsn("file:local.db").is_err());
    }

    #[test]
    fn sqlite_and_mysql_keep_question_marks() {
        let sql = "SELECT a FROM t WHERE b = ? AND c = ?";
        assert_eq!(Dialect::Sqlite.placeholders(sql), sql);
        assert_eq!(Dialect::MySql.placeholders(sql), sql);
    }

    #[test]
    fn postgres_numbers_placeholders_in_order() {
        let sql = "UPDATE t SET a = ?, b = ?, c = ? WHERE id = ?";
        assert_eq!(
            Dialect::Postgres.placeholders(sql),
            "UPDATE t SET a = $1, b = $2, c = $3 WHERE id = $4"
        );
    }

    #[test]
    fn postgres_ignores_question_marks_in_literals_and_comments() {
        let sql = "SELECT '?' AS q, \"col?\" FROM t -- why?\n WHERE a = ? /* ? */ AND b = ?";
        assert_eq!(
            Dialect::Postgres.placeholders(sql),
            "SELECT '?' AS q, \"col?\" FROM t -- why?\n WHERE a = $1 /* ? */ AND b = $2"
        );
    }

    #[test]
    fn postgres_handles_escaped_quotes() {
        let sql = "SELECT 'it''s ?' , x FROM t WHERE y = ?";
        assert_eq!(
            Dialect::Postgres.placeholders(sql),
            "SELECT 'it''s ?' , x FROM t WHERE y = $1"
        );
    }

    #[test]
    fn no_placeholder_is_zero_copy() {
        assert!(matches!(
            Dialect::Postgres.placeholders("SELECT 1"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn insert_with_many_placeholders() {
        let sql = "INSERT INTO t (a,b,c,d) VALUES (?,?,?,?)";
        assert_eq!(
            Dialect::Postgres.placeholders(sql),
            "INSERT INTO t (a,b,c,d) VALUES ($1,$2,$3,$4)"
        );
    }
}
