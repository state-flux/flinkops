//! 领域逻辑：基线合并、CRD 校验、spec diff、健康度与指标源选择。
//!
//! 这一层**不依赖 axum / kube**，只吃 `serde_json::Value` 与 types，
//! 所以 CLI、API、MCP 三端复用，测试也不用起集群。
use std::collections::BTreeMap;

use flinkops_types::{Finding, JobSummary, Profile, Severity, SpecDiff};
use serde_json::Value;

// ----------------------------------------------------------- merge

/// 深合并：object 递归，其它类型直接覆盖。数组整体替换。
///
/// `over` 为 `Null` 时保留 `base`：调用方常用 `Value::default()`（即 Null）
/// 表示"没有覆盖项"，早期实现会因此把整份 spec 覆盖成 null。
pub fn merge(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (_, Value::Null) => base.clone(),
        (Value::Object(a), Value::Object(b)) => {
            let mut out = a.clone();
            for (k, v) in b {
                let merged = match out.get(k) {
                    Some(existing) => merge(existing, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        (_, v) => v.clone(),
    }
}

/// 把基线展开成一份 `spec`（作业只覆盖差异项）。
pub fn spec_from_profile(p: &Profile, overrides: &Value) -> Value {
    let mut cfg = serde_json::Map::new();
    for (k, v) in &p.defaults {
        cfg.insert(k.clone(), Value::String(v.clone()));
    }
    cfg.insert(
        "kubernetes.rest-service.exposed.type".into(),
        Value::String(p.rest_exposed.clone()),
    );

    let mut spec = serde_json::json!({
        "image": p.image,
        "flinkVersion": p.flink_version,
        "serviceAccount": p.service_account,
        "flinkConfiguration": Value::Object(cfg),
        "jobManager": { "replicas": 1, "resource": { "cpu": 1, "memory": "2Gi" } },
        "taskManager": { "replicas": 2, "resource": { "cpu": 2, "memory": "4Gi" } },
        "job": { "upgradeMode": "last-state", "state": "running", "parallelism": 2 }
    });

    let name = overrides
        .pointer("/metadata/name")
        .and_then(|v| v.as_str())
        .unwrap_or("<name>");
    if let Some(Value::Object(map)) = spec.pointer_mut("/flinkConfiguration") {
        map.insert(
            "state.checkpoints.dir".into(),
            Value::String(p.checkpoint_prefix.replace("{name}", name)),
        );
        map.insert(
            "state.savepoints.dir".into(),
            Value::String(p.savepoint_prefix.replace("{name}", name)),
        );
        // 键名随 Flink 版本变：1.20 起 state.backend → state.backend.type，
        // restart-strategy → restart-strategy.type。写错键不会报错，只会静默不生效。
        let modern = flink_minor(&p.flink_version)
            .map(|m| m >= 20)
            .unwrap_or(true);
        let backend_key = if modern {
            "state.backend.type"
        } else {
            "state.backend"
        };
        map.insert(backend_key.into(), Value::String(p.state_backend.clone()));
        let (from, to) = if modern {
            ("restart-strategy", "restart-strategy.type")
        } else {
            ("restart-strategy.type", "restart-strategy")
        };
        if let Some(v) = map.remove(from) {
            map.insert(to.into(), v);
        }
    }
    merge(&spec, overrides)
}

/// 从 `v1_15` / `1.15` / `1.15.4` 里取 minor 版本号（15）。
pub fn flink_minor(version: &str) -> Option<u32> {
    let v = version.trim().trim_start_matches('v');
    let mut parts = v.split(['_', '.']);
    let major = parts.next()?;
    let minor = parts.next()?;
    // `v1_15` → major=1, minor=15；`15` 这种单段写法直接当 minor
    if major.parse::<u32>().is_err() {
        return None;
    }
    minor.parse::<u32>().ok().or_else(|| major.parse().ok())
}

// ----------------------------------------------------------- validate

/// 校验时的环境上下文。
///
/// 有些建议是有前提的：比如"把 REST 暴露方式从 NodePort 改成 ClusterIP 走平台反代"，
/// 只有当平台本身能与该集群用 ClusterIP 互通（通常即在集群内）时才成立。
#[derive(Debug, Clone, Copy)]
pub struct Env {
    /// 平台是否与被校验的集群处于同一网络（在集群内运行 = true）
    pub in_cluster: bool,
}

impl Default for Env {
    /// 调用方没有集群上下文时按 true 处理：宁可多提示一条安全建议，也不静默漏掉
    fn default() -> Self {
        Self { in_cluster: true }
    }
}

/// 复刻 `FlinkResourceValidator` 的核心规则，外加平台自己的安全巡检。
pub fn validate(spec: &Value) -> Vec<Finding> {
    validate_with_env(spec, Env::default())
}

/// 带环境上下文的校验：目前只影响"NodePort 暴露"这一条建议是否成立。
pub fn validate_with_env(spec: &Value, env: Env) -> Vec<Finding> {
    let mut out = Vec::new();
    let get = |p: &str| spec.pointer(p);

    if get("/image")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty()
    {
        out.push(Finding::error(
            "IMAGE_REQUIRED",
            "spec.image",
            "镜像不能为空",
        ));
    } else {
        let image = get("/image").and_then(|v| v.as_str()).unwrap_or("").trim();
        // 常见坑：把基线 id / 版本号当镜像填（如 image: general-1.19），
        // 提交后在集群里表现为 ImagePullBackOff，平台却"校验通过"。
        let last = image.rsplit('/').next().unwrap_or(image);
        if image.contains(' ') || (!last.contains(':') && !image.contains('@')) {
            out.push(Finding::error(
                "IMAGE_INVALID",
                "spec.image",
                format!("镜像需要带 tag 或 digest（如 flink:1.15.4），当前为 {image}"),
            ));
        }
    }
    if get("/flinkVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty()
    {
        out.push(Finding::error(
            "VERSION_REQUIRED",
            "spec.flinkVersion",
            "flinkVersion 不能为空（如 v1_19）",
        ));
    } else if let Some(v) = get("/flinkVersion").and_then(|x| x.as_str()) {
        let v = v.trim();
        // operator 的枚举是 v1_15..v2_0；写成 1.19 / v1.19 / v1_9 都会在 webhook 才被拒
        if !is_valid_flink_version(v) {
            out.push(Finding::error(
                "VERSION_INVALID",
                "spec.flinkVersion",
                format!("flinkVersion 需要形如 v1_17 / v1_19（operator 枚举值），当前为 {v}"),
            ));
        }
    }

    // jobManager/taskManager.resource 走 Flink 的 MemorySize：
    // `1024m` 合法（1024 MB），但单独一个 `m`（表单被清空后拼出来的）非法，
    // 以前平台会放过去，到 operator 才报"无法解析内存"。
    for (tpl, label) in [
        ("jobManager", "spec.jobManager.resource"),
        ("taskManager", "spec.taskManager.resource"),
    ] {
        let base = format!("/{tpl}/resource");
        if let Some(cpu) = get(&format!("{base}/cpu")) {
            let num = cpu
                .as_f64()
                .or_else(|| cpu.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
            match num {
                Some(v) if v > 0.0 => {}
                _ => out.push(Finding::error(
                    "CPU_INVALID",
                    &format!("{label}.cpu"),
                    "cpu 必须是 >0 的数字（如 0.5 / 1 / 2000m 不适用本字段）",
                )),
            }
        }
        if let Some(mem) = get(&format!("{base}/memory")) {
            let s = mem.as_str().unwrap_or("").trim();
            if !is_flink_memory(s) {
                out.push(Finding::error(
                    "MEMORY_INVALID",
                    &format!("{label}.memory"),
                    format!("memory 需要「数字+单位」（如 1024m / 2g / 4GiB），当前为 {s:?}"),
                ));
            }
        }
    }

    // session 模式的 FlinkDeployment 没有 job（不跑具体作业），不要求 jar/sql
    // session cluster：没有 job 段（operator 用 mode=native + 无 job 表达，
    // 老版本的 mode=session 枚举已被移除）
    let is_session = spec.get("job").is_none();
    let jar = get("/job/jarURI").and_then(|v| v.as_str()).unwrap_or("");
    let sql = get("/job/sql").and_then(|v| v.as_str()).unwrap_or("");
    if !is_session && jar.is_empty() && sql.is_empty() {
        out.push(Finding::error(
            "JOB_SOURCE_REQUIRED",
            "spec.job",
            "必须提供 jarURI（JAR 作业）或 sql（SQL 作业）",
        ));
    }
    // operator webhook 要求 jarURI 与 sql 恰好一个；SQL 作业不接受 entryClass
    if !jar.is_empty() && !sql.is_empty() {
        out.push(Finding::error(
            "JOB_SOURCE_CONFLICT",
            "spec.job",
            "jarURI 与 sql 只能设一个（operator 会拒）",
        ));
    }
    if !sql.is_empty() && get("/job/entryClass").is_some() {
        out.push(Finding::error(
            "SQL_ENTRYCLASS_CONFLICT",
            "spec.job.entryClass",
            "SQL 作业不能带 entryClass（operator 会拒）",
        ));
    }
    // JM 副本：webhook 要求恰好 1 个（除非配了 HA）
    if !is_session {
        let jm_replicas = get("/jobManager/replicas")
            .and_then(|v| v.as_i64())
            .or_else(|| {
                get("/jobManager/replicas")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.trim().parse::<i64>().ok())
            })
            .unwrap_or(1);
        if jm_replicas < 1 {
            out.push(Finding::error(
                "JM_REPLICAS_INVALID",
                "spec.jobManager.replicas",
                "jobManager.replicas 必须 ≥1",
            ));
        }
    }
    // 资源量校验只在 **podTemplate 的容器 resources** 上做：
    // 那里是标准 k8s Quantity（`1024m` = 1.024 字节，必错）。
    // 而 FlinkDeployment 的 jobManager/taskManager.resource 走 Flink 的 MemorySize，
    // `1024m` 就是 1024 MB，属于合法写法——不能误判。
    for tpl in ["", "/jobManager", "/taskManager"] {
        let path = format!("{tpl}/podTemplate/spec/containers");
        let Some(cs) = spec.pointer(&path).and_then(|v| v.as_array()) else {
            continue;
        };
        for (ci, c) in cs.iter().enumerate() {
            for field in ["requests", "limits"] {
                let Some(mem) = c
                    .pointer(&format!("/resources/{field}/memory"))
                    .and_then(|v| v.as_str())
                else {
                    continue;
                };
                match parse_memory_bytes(mem) {
                    MemParse::Ok => {}
                    MemParse::MilliBytes => out.push(Finding::error(
                        "MEMORY_MILLI_UNIT",
                        &format!("spec{path}[{ci}].resources.{field}.memory"),
                        format!("容器内存写成 {mem}（k8s 里 m=千分之一字节），请用 512Mi / 2Gi"),
                    )),
                    MemParse::Invalid => out.push(Finding::warn(
                        "MEMORY_INVALID",
                        &format!("spec{path}[{ci}].resources.{field}.memory"),
                        format!("容器内存 {mem} 不是合法 Quantity（如 2Gi / 2048Mi）"),
                    )),
                }
            }
        }
    }
    // Flink 时长：没有单位就按毫秒解释，60 会被当成 60ms → checkpoint 风暴
    for key in [
        "execution.checkpointing.interval",
        "execution.checkpointing.timeout",
        "execution.checkpointing.min-pause",
    ] {
        if let Some(v) = get(&format!("/flinkConfiguration/{key}")).and_then(|v| v.as_str()) {
            if duration_missing_unit(v) {
                out.push(Finding::warn(
                    "DURATION_NO_UNIT",
                    &format!("spec.flinkConfiguration.{key}"),
                    format!("{key}={v} 没有单位，Flink 按毫秒解释；如需 60 秒请写 60s"),
                ));
            }
        }
    }

    let parallelism = get("/job/parallelism")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            get("/job/parallelism")
                .and_then(|v| v.as_str())
                .and_then(|s| s.trim().parse::<i64>().ok())
        });
    if !is_session {
        match parallelism {
            Some(p) if p > 0 => {}
            Some(_) => out.push(Finding::error(
                "PARALLELISM_INVALID",
                "spec.job.parallelism",
                "并行度必须大于 0",
            )),
            // 缺省是合法的：Flink 会用 parallelism.default
            None => out.push(Finding::info(
                "PARALLELISM_DEFAULT",
                "spec.job.parallelism",
                "未显式设置并行度，将使用 parallelism.default",
            )),
        }
    }

    match get("/job/upgradeMode").and_then(|v| v.as_str()) {
        Some("stateless") | Some("last-state") | Some("savepoint") | None => {}
        Some(other) => out.push(Finding::error(
            "UPGRADE_MODE_INVALID",
            "spec.job.upgradeMode",
            format!("upgradeMode 只能是 stateless / last-state / savepoint，当前为 {other}"),
        )),
    }
    match get("/job/state").and_then(|v| v.as_str()) {
        Some("running") | Some("suspended") | None => {}
        Some(other) => out.push(Finding::error(
            "STATE_INVALID",
            "spec.job.state",
            format!("state 只能是 running / suspended，当前为 {other}"),
        )),
    }

    // 槽位 vs 并行度：最常见的一处浪费/不足
    let slots = get("/flinkConfiguration/taskmanager.numberOfTaskSlots")
        .and_then(|v| v.as_str())
        .and_then(|v| v.trim().parse::<i64>().ok());
    let tms = get("/taskManager/replicas").and_then(|v| v.as_i64());
    if let (Some(s), Some(t), Some(p)) = (slots, tms, parallelism) {
        let total = s * t;
        if total > 0 && p > total {
            out.push(Finding::warn(
                "SLOTS_TOO_FEW",
                "spec.taskManager.replicas",
                format!(
                    "并行度 {p} 超过槽位总量 {total}（{t}×{s}），作业会等 slot 或只跑部分子任务"
                ),
            ));
        } else if total > 0 && total >= p * 3 {
            out.push(Finding::warn(
                "SLOTS_WASTED",
                "spec.flinkConfiguration.taskmanager.numberOfTaskSlots",
                format!("槽位总量 {total} 是并行度 {p} 的 3 倍以上，存在明显浪费（{t}×{s}）"),
            ));
        }
    }

    // 安全：NodePort 暴露无认证的 Flink UI。
    // 只有平台能与该集群用 ClusterIP 互通（在集群内）时，"改 ClusterIP 走平台反代"
    // 才是可执行的建议；平台在集群外时 NodePort 往往是唯一通路，提示只会误导。
    if env.in_cluster {
        if let Some(exposed) =
            get("/flinkConfiguration/kubernetes.rest-service.exposed.type").and_then(|v| v.as_str())
        {
            if exposed.eq_ignore_ascii_case("NodePort") {
                out.push(Finding::warn(
                "REST_NODEPORT",
                "spec.flinkConfiguration.kubernetes.rest-service.exposed.type",
                    "NodePort 会把无认证的 Flink UI 暴露到节点网络；平台在集群内时可改 ClusterIP 走反代",
                ));
            }
        }
    }

    // 安全：明文凭据。
    // 真实集群把 AK/SK 放在 jobManager.podTemplate.spec.initContainers[].env，
    // 只扫 spec.podTemplate.spec.containers 会 100% 漏报。
    let templates = ["", "/jobManager", "/taskManager"];
    for tpl in templates {
        for ck in ["containers", "initContainers"] {
            let path = format!("{tpl}/podTemplate/spec/{ck}");
            let Some(cs) = spec.pointer(&path).and_then(|v| v.as_array()) else {
                continue;
            };
            for (ci, c) in cs.iter().enumerate() {
                let Some(envs) = c.get("env").and_then(|e| e.as_array()) else {
                    continue;
                };
                for (ei, env) in envs.iter().enumerate() {
                    let name = env.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let upper = name.to_ascii_uppercase();
                    let looks_secret = upper.contains("ACCESS_KEY")
                        || upper.contains("SECRET")
                        || upper.contains("PASSWORD")
                        || upper.contains("TOKEN");
                    if looks_secret && env.get("value").is_some() {
                        out.push(Finding::warn(
                            "PLAINTEXT_CREDENTIAL",
                            &format!("spec{path}[{ci}].env[{ei}]"),
                            format!("{name} 以明文写在 env 里，会进 Git 历史；请改用 secretKeyRef"),
                        ));
                    }
                }
            }
        }
    }

    if get("/job/upgradeMode").and_then(|v| v.as_str()) == Some("savepoint")
        && get("/flinkConfiguration/state.savepoints.dir").is_none()
    {
        out.push(Finding::warn(
            "SAVEPOINT_DIR_MISSING",
            "spec.flinkConfiguration.state.savepoints.dir",
            "upgradeMode=savepoint 但没有配 state.savepoints.dir",
        ));
    }

    if let Some(t) = get("/ingress/template").and_then(|v| v.as_str()) {
        if !t.contains("{{name}}") {
            out.push(Finding::info(
                "INGRESS_TEMPLATE",
                "spec.ingress.template",
                "建议使用 {{name}} 占位符，否则多个作业会抢同一个域名",
            ));
        }
    }

    // 真实集群教训 1：Prometheus 端口写成范围（"9250-9260"）时，
    // 实际监听端口由 Flink 决定，PodMonitor 的 targetPort 会对不上。
    if let Some(port) =
        get("/flinkConfiguration/metrics.reporter.prom.port").and_then(|v| v.as_str())
    {
        if let Some((from, to)) = port.split_once('-') {
            if from.trim() != to.trim() {
                out.push(Finding::warn(
                    "PROM_PORT_RANGE",
                    "spec.flinkConfiguration.metrics.reporter.prom.port",
                    format!("端口写成范围 {port}，实际监听端口不确定；PodMonitor 的 targetPort 会对不上，建议固定成 {from}"),
                ));
            }
        }
    }

    // 键名重组发生在 Flink 1.20：<1.20 只认 state.backend / restart-strategy，
    // >=1.20 才认 .type。判错方向会让配置静默失效（状态后端回落到 hashmap）。
    let version_minor = get("/flinkVersion")
        .and_then(|v| v.as_str())
        .and_then(flink_minor);
    let modern = version_minor.map(|m| m >= 20).unwrap_or(true);
    let old_backend = get("/flinkConfiguration/state.backend").is_some();
    let new_backend = get("/flinkConfiguration/state.backend.type").is_some();
    if old_backend && new_backend {
        out.push(Finding::warn(
            "STATE_BACKEND_DUP",
            "spec.flinkConfiguration",
            "state.backend 与 state.backend.type 同时存在，容易配错；请只保留一个",
        ));
    } else if modern && old_backend {
        out.push(Finding::warn(
            "STATE_BACKEND_LEGACY_KEY",
            "spec.flinkConfiguration.state.backend",
            "flinkVersion≥1.20 请改用 state.backend.type（旧键会被忽略）",
        ));
    } else if !modern && new_backend {
        out.push(Finding::error(
            "STATE_BACKEND_NEW_KEY_UNSUPPORTED",
            "spec.flinkConfiguration.state.backend.type",
            format!(
                "flinkVersion={} 不认识 state.backend.type（1.20 才引入），状态后端会静默失效；请改用 state.backend",
                version_minor.map(|m| format!("1.{m}")).unwrap_or_default()
            ),
        ));
    }
    let old_rs = get("/flinkConfiguration/restart-strategy").is_some();
    let new_rs = get("/flinkConfiguration/restart-strategy.type").is_some();
    if !modern && new_rs && !old_rs {
        out.push(Finding::warn(
            "RESTART_STRATEGY_NEW_KEY_UNSUPPORTED",
            "spec.flinkConfiguration.restart-strategy.type",
            "该 Flink 版本只认 restart-strategy（无 .type 后缀），否则重启策略不生效",
        ));
    }

    out
}

pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Error)
}

/// 校验完整对象：`metadata`（名称 / 命名空间 / 标签）+ `spec` 规则。
///
/// `validate(spec)` 只能看到 spec，metadata 级错误（名称非法、label 超长）
/// 在提交路径上会被漏掉，因此提交/校验入口统一走这个函数。
pub fn validate_object(obj: &Value) -> Vec<Finding> {
    validate_object_with_env(obj, Env::default())
}

/// 带环境上下文的整对象校验（提交/校验入口用这个，才能把集群内外的差异带进来）。
pub fn validate_object_with_env(obj: &Value, env: Env) -> Vec<Finding> {
    let mut out = Vec::new();

    match obj.pointer("/metadata/name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => {
            if !is_dns1123(n) {
                out.push(Finding::error(
                    "NAME_INVALID",
                    "metadata.name",
                    format!("{n} 不是合法 DNS-1123 名称（小写字母/数字/-，且不以 - 开头结尾）"),
                ));
            }
        }
        _ => out.push(Finding::error(
            "NAME_REQUIRED",
            "metadata.name",
            "metadata.name 不能为空",
        )),
    }

    match obj.pointer("/metadata/namespace").and_then(|v| v.as_str()) {
        Some(ns) if !ns.is_empty() => {
            if !is_dns1123(ns) {
                out.push(Finding::error(
                    "NAMESPACE_INVALID",
                    "metadata.namespace",
                    format!("{ns} 不是合法命名空间名"),
                ));
            }
        }
        _ => out.push(Finding::error(
            "NAMESPACE_REQUIRED",
            "metadata.namespace",
            "metadata.namespace 不能为空",
        )),
    }

    if let Some(labels) = obj.pointer("/metadata/labels").and_then(|v| v.as_object()) {
        for (k, v) in labels {
            let ok = v
                .as_str()
                .map(|s| {
                    s.len() <= 63
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                })
                .unwrap_or(false);
            if !ok || k.len() > 63 {
                out.push(Finding::error(
                    "LABEL_INVALID",
                    &format!("metadata.labels.{k}"),
                    "label 键值需 ≤63 字符，值只能包含字母数字与 -_.",
                ));
            }
        }
    }

    let spec = obj.get("spec").cloned().unwrap_or(Value::Null);
    out.extend(validate_with_env(&spec, env));
    out
}

/// DNS-1123 label（K8s 对 name/namespace 的要求）。
fn is_dns1123(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// k8s Quantity 内存写法判定。
pub enum MemParse {
    Ok,
    /// `1024m`：m 是千分之一字节，是"以为是 1024MB"的典型笔误
    MilliBytes,
    Invalid,
}

/// operator 的 `spec.flinkVersion` 枚举形如 `v1_15` … `v2_0`。
/// 写成 `1.19` / `v1.19` / `v1_9` 都会在 webhook 才被拒，这里提前拦。
fn is_valid_flink_version(v: &str) -> bool {
    let Some(rest) = v.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = rest.split('_').collect();
    parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Flink MemorySize：`数字 + 单位`。
/// 合法单位：b/kb/mb/gb/tb、k/m/g/t、ki/mi/gi/ti（大小写不敏感）。
/// 单独一个 `m`（表单清空后拼出来的 `"m"`）必须判非法。
fn is_flink_memory(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    let digits: String = lower
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() || digits.parse::<f64>().is_err() {
        return false;
    }
    let unit = lower[digits.len()..].trim();
    matches!(
        unit,
        "b" | "kb" | "mb" | "gb" | "tb" | "k" | "m" | "g" | "t" | "ki" | "mi" | "gi" | "ti"
    )
}
pub fn parse_memory_bytes(s: &str) -> MemParse {
    let t = s.trim();
    if t.is_empty() {
        return MemParse::Invalid;
    }
    let num_end = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(num_end);
    if num.parse::<f64>().is_err() {
        return MemParse::Invalid;
    }
    match unit.trim() {
        "" | "Ki" | "Mi" | "Gi" | "Ti" | "Pi" | "Ei" | "K" | "M" | "G" | "T" | "P" | "E" => {
            MemParse::Ok
        }
        "m" => MemParse::MilliBytes,
        _ => MemParse::Invalid,
    }
}

pub fn parse_cpu_millis(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(m) = t.strip_suffix('m') {
        return m.trim().parse::<f64>().ok();
    }
    t.parse::<f64>().ok()
}

/// Flink `Duration` 无单位时按毫秒解释：纯数字（如 `60`）几乎都是"想写 60 秒"。
pub fn duration_missing_unit(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

// ----------------------------------------------------------- diff

/// 递归比较两份 spec，返回点号路径的差异。
pub fn diff(before: &Value, after: &Value) -> Vec<SpecDiff> {
    let mut out = Vec::new();
    walk("", before, after, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn walk(path: &str, a: &Value, b: &Value, out: &mut Vec<SpecDiff>) {
    match (a, b) {
        (Value::Object(ma), Value::Object(mb)) => {
            let mut keys: BTreeMap<&String, ()> = BTreeMap::new();
            for k in ma.keys().chain(mb.keys()) {
                keys.insert(k, ());
            }
            for k in keys.keys() {
                let p = if path.is_empty() {
                    (*k).clone()
                } else {
                    format!("{path}.{k}")
                };
                match (ma.get(*k), mb.get(*k)) {
                    (Some(x), Some(y)) => walk(&p, x, y, out),
                    (Some(x), None) => out.push(SpecDiff {
                        path: p,
                        before: Some(brief(x)),
                        after: None,
                    }),
                    (None, Some(y)) => out.push(SpecDiff {
                        path: p,
                        before: None,
                        after: Some(brief(y)),
                    }),
                    (None, None) => {}
                }
            }
        }
        _ => {
            if a != b {
                out.push(SpecDiff {
                    path: path.to_string(),
                    before: Some(brief(a)),
                    after: Some(brief(b)),
                });
            }
        }
    }
}

fn brief(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ----------------------------------------------------------- health

/// 列表里那颗"健康度"点：绿=Running 条件为真（或无 conditions 时 lifecycle=STABLE），
/// 黄=调谐中/未确认，红=失败。判定实体在 `JobSummary::health_level`。
pub fn health(summary: &JobSummary) -> &'static str {
    summary.health_level()
}

/// 指标源选择：REST 优先，Prometheus 只做历史。
pub fn metrics_source(has_prometheus: bool, rest_ok: bool) -> &'static str {
    if rest_ok {
        "flink-rest"
    } else if has_prometheus {
        "prometheus"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_is_deep_and_idempotent() {
        let base = json!({ "a": { "x": 1, "y": 2 }, "b": [1, 2] });
        let over = json!({ "a": { "y": 3 }, "c": true });
        let once = merge(&base, &over);
        assert_eq!(once.pointer("/a/x"), Some(&json!(1)));
        assert_eq!(once.pointer("/a/y"), Some(&json!(3)));
        assert_eq!(once.pointer("/c"), Some(&json!(true)));
        assert_eq!(merge(&once, &over), once, "合并必须幂等");
    }

    #[test]
    fn profile_expands_paths() {
        let p = Profile::default();
        let spec = spec_from_profile(&p, &json!({ "metadata": { "name": "orders" } }));
        assert_eq!(
            spec.pointer("/flinkConfiguration/state.checkpoints.dir")
                .unwrap(),
            &json!("s3://flink-state/checkpoints/orders")
        );
    }

    #[test]
    fn validate_flags_slot_mismatch_and_nodeport() {
        let spec = json!({
            "image": "flink:1.19.1", "flinkVersion": "v1_19",
            "flinkConfiguration": {
                "taskmanager.numberOfTaskSlots": "2",
                "kubernetes.rest-service.exposed.type": "NodePort"
            },
            "taskManager": { "replicas": 2 },
            "job": { "jarURI": "s3://j", "parallelism": 8 }
        });
        let f = validate(&spec);
        assert!(f.iter().any(|x| x.code == "SLOTS_TOO_FEW"));
        assert!(f.iter().any(|x| x.code == "REST_NODEPORT"));
        assert!(!has_errors(&f), "这两条是 warn，不是 error");
    }

    /// 把基线 id 当镜像填（image: general-1.19）必须拦成 error：
    /// 集群侧表现是 ImagePullBackOff，平台却"校验通过"。
    #[test]
    fn validate_rejects_untagged_image() {
        let spec = json!({
            "image": "general-1.19", "flinkVersion": "v1_19",
            "job": { "jarURI": "s3://j", "parallelism": 1 }
        });
        let f = validate(&spec);
        assert!(has_errors(&f));
        assert!(f.iter().any(|x| x.code == "IMAGE_INVALID"), "{f:?}");
    }

    /// overrides 缺省是 Value::Null（serde default），不能把整份 spec 覆盖成 null
    #[test]
    fn merge_ignores_null_override() {
        let base = json!({ "a": 1, "b": { "c": 2 } });
        assert_eq!(merge(&base, &Value::Null), base);
        assert_eq!(
            merge(&base, &json!({ "b": { "c": 3 } }))["b"]["c"],
            json!(3)
        );
    }

    /// metadata 级错误（名称/命名空间/标签）只在完整对象上校验得到
    #[test]
    fn validate_object_checks_metadata() {
        let obj = json!({
            "apiVersion": "flink.apache.org/v1beta1",
            "kind": "FlinkDeployment",
            "metadata": { "name": "Bad_Name", "namespace": "flink",
                          "labels": { "team": "data science" } },
            "spec": { "image": "flink:1.19.1", "flinkVersion": "v1_19",
                      "job": { "jarURI": "s3://j", "parallelism": 1 } }
        });
        let f = validate_object(&obj);
        assert!(f.iter().any(|x| x.code == "NAME_INVALID"), "{f:?}");
        assert!(f.iter().any(|x| x.code == "LABEL_INVALID"), "{f:?}");
    }

    #[test]
    fn session_mode_needs_no_job() {
        let spec = serde_json::json!({
            "mode": "native",
            "image": "img", "flinkVersion": "v1_19",
            "flinkConfiguration": { "taskmanager.numberOfTaskSlots": "4" },
            "taskManager": { "replicas": 2 }
        });
        let f = validate(&spec);
        assert!(
            !f.iter()
                .any(|x| x.code == "JOB_SOURCE_REQUIRED" || x.code == "PARALLELISM_INVALID"),
            "session 模式不该要求 job 源与并行度: {f:?}"
        );
    }

    #[test]
    fn validate_rejects_missing_source() {
        let f = validate(
            &json!({ "image": "i", "flinkVersion": "v1_19", "job": { "parallelism": 1 } }),
        );
        assert!(f
            .iter()
            .any(|x| x.code == "JOB_SOURCE_REQUIRED" && x.severity == Severity::Error));
        assert!(has_errors(&f));
    }

    #[test]
    fn diff_reports_paths() {
        let a = json!({ "job": { "parallelism": 4 }, "image": "x" });
        let b = json!({ "job": { "parallelism": 8 }, "image": "x" });
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, "job.parallelism");
    }

    /// NodePort 提示只在"平台能与该集群用 ClusterIP 互通"时成立。
    /// 平台在集群外（本地开发连远端集群）时，NodePort 往往是唯一通路，提示是噪音。
    #[test]
    fn nodeport_hint_only_when_platform_is_in_cluster() {
        let spec = json!({
            "image": "repo/app:1.0",
            "flinkVersion": "v1_19",
            "flinkConfiguration": {
                "kubernetes.rest-service.exposed.type": "NodePort"
            },
            "job": { "jarURI": "s3://j", "parallelism": 1 }
        });

        let inside = validate_with_env(&spec, Env { in_cluster: true });
        assert!(
            inside.iter().any(|f| f.code == "REST_NODEPORT"),
            "集群内应提示：{inside:?}"
        );

        let outside = validate_with_env(&spec, Env { in_cluster: false });
        assert!(
            !outside.iter().any(|f| f.code == "REST_NODEPORT"),
            "集群外不该提示：{outside:?}"
        );
    }
}
