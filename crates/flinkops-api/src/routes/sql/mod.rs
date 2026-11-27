//! SQL 工作台执行：走 Flink 1.16+ 的 SQL Gateway REST。
//!
//! 链路：找会话集群的 gateway Service（nodePort）→ 开 session → 提交语句 →
//! 轮询结果 → 关 session。gateway 是独立进程，和 JobManager REST 不是一个地址。
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use flinkops_flink_rest::SqlGateway;
use flinkops_types::{crd, Error};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::extract::AuthUser;

use crate::state::AppState;

mod catalog;
mod deploy;
mod execute;
mod jobs;

pub use catalog::*;
pub use deploy::*;
pub use execute::*;
pub use jobs::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_multi_statement_script() {
        let sql = "CREATE TABLE src (id INT) WITH ('connector'='datagen');\n\
                   CREATE TABLE snk (id INT) WITH ('connector'='print');\n\
                   INSERT INTO snk SELECT * FROM src;";
        let got = split_statements(sql);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("CREATE TABLE src"));
        assert!(got[2].starts_with("INSERT INTO snk"));
    }

    #[test]
    fn split_ignores_semicolons_in_literals_and_comments() {
        let sql = "INSERT INTO t VALUES ('a;b', 'it''s;ok') -- tail; comment\n;\
                   /* block; comment */ SELECT ';;';";
        let got = split_statements(sql);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("a;b"), "{}", got[0]);
        assert!(got[1].starts_with("SELECT"), "{}", got[1]);
        assert_eq!(split_statements("  \n;;  ").len(), 0);
    }

    #[test]
    fn app_template_renders_to_valid_yaml() {
        let args = json!(["--sql-file", "/opt/flink/usrlib/sql-scripts/job.sql"]);
        let vars = deploy_app_vars(DeployAppVars {
            name: "demo-app",
            ns: "flink",
            image: "repo/flink:1.16",
            flink_version: "v1_16",
            service_account: "flink",
            slots: "2",
            runner: "local:///opt/flink/usrlib/sql-runner.jar",
            args: &args,
            parallelism: 1,
            upgrade_mode: "stateless",
            sql_config_map: "demo-app-sql",
        });
        let rendered = crate::routes::deploy_templates::render(
            crate::routes::deploy_templates::BUILTIN_SQL_APP,
            &vars,
        );
        let missing = crate::routes::deploy_templates::unresolved(&rendered);
        assert!(missing.is_empty(), "还有没替换的占位符：{missing:?}");
        let v: Value = serde_yaml::from_str(&rendered).expect("渲染结果必须是合法 YAML");
        assert_eq!(v["kind"], "FlinkDeployment");
        assert_eq!(v["metadata"]["name"], "demo-app");
        assert_eq!(
            v["spec"]["job"]["jarURI"],
            "local:///opt/flink/usrlib/sql-runner.jar"
        );
        assert_eq!(v["spec"]["job"]["args"][0], "--sql-file");
        assert_eq!(v["spec"]["taskManager"]["replicas"], 1);
        assert_eq!(
            v["spec"]["flinkConfiguration"]["taskmanager.numberOfTaskSlots"],
            "2"
        );
    }

    #[test]
    fn yaml_values_are_escaped_not_quoted() {
        // 模板本身已经给字符串占了引号位置，所以变量只做转义
        assert_eq!(yaml_text("repo/flink:1.16"), "repo/flink:1.16");
        assert_eq!(yaml_text("a\"b"), "a\\\"b");
    }
}
