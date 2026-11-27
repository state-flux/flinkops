//! autoscaler 的 `flinkConfiguration` 键映射表。
//!
//! operator 的 autoscaler 参数是 `flinkConfiguration` 里的字符串键（`job.autoscaler.*`）。
//! 键名写错不会报错，而是被 **operator 静默忽略** —— 现象是"开了 autoscaler 却不生效"。
//! 因此键名集中成常量表并由单测钉死，改动时先红后绿。

use serde::Deserialize;
use serde_json::{Map, Value};

pub const K_ENABLED: &str = "job.autoscaler.scaling.enabled";
pub const K_UTILIZATION: &str = "job.autoscaler.target.utilization";
pub const K_METRICS_WINDOW: &str = "job.autoscaler.metrics.window";
pub const K_STABILIZATION: &str = "job.autoscaler.stabilization.interval";
pub const K_SCALE_UP_FACTOR: &str = "job.autoscaler.scale-up.max-factor";
pub const K_SCALE_DOWN_FACTOR: &str = "job.autoscaler.scale-down.max-factor";
pub const K_VERTEX_MIN: &str = "job.autoscaler.vertex.min-parallelism";
pub const K_VERTEX_MAX: &str = "job.autoscaler.vertex.max-parallelism";

/// 前端可能传、但 operator **不认识**的键。收到就忽略并在响应里说明，
/// 静默丢弃会让用户以为设置生效了。
pub const UNSUPPORTED_INPUTS: &[(&str, &str)] = &[(
    "excludedPeriods",
    "operator 没有 job.autoscaler.excluded.periods 这个键（早期版本写进去会被忽略）",
)];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoscalerBody {
    pub enabled: bool,
    /// 目标利用率（0.1–1.0，默认 0.7）
    pub utilization_target: Option<f64>,
    /// 指标窗口，如 "15m"
    pub metrics_window: Option<String>,
    /// 稳定期，如 "1h"
    pub stabilization_interval: Option<String>,
    /// 单次扩容上限倍数（默认 2.0）
    pub scale_up_max_factor: Option<f64>,
    /// 单次缩容下限倍数（默认 0.6）
    pub scale_down_max_factor: Option<f64>,
    pub vertex_min_parallelism: Option<i64>,
    pub vertex_max_parallelism: Option<i64>,
    /// operator 不支持，保留字段只为"收到时给一句说明"
    pub excluded_periods: Option<String>,
}

/// 把请求体映射成要写进 `spec.flinkConfiguration` 的键值。
///
/// 返回 `Err(String)` 表示值非法（handler 转 400）；返回 `ignored` 表示
/// 收到了 operator 不认识的输入。
pub fn build_config(body: &AutoscalerBody) -> Result<(Map<String, Value>, Vec<String>), String> {
    let mut cfg = Map::new();
    let mut ignored = Vec::new();

    cfg.insert(
        K_ENABLED.to_string(),
        Value::String(if body.enabled { "true" } else { "false" }.to_string()),
    );

    if let Some(t) = body.utilization_target {
        if !(0.1..=1.0).contains(&t) {
            return Err("utilizationTarget 需在 0.1–1.0".into());
        }
        cfg.insert(K_UTILIZATION.to_string(), Value::String(t.to_string()));
    }
    if let Some(w) = &body.metrics_window {
        if w.trim().is_empty() {
            return Err("metricsWindow 不能为空".into());
        }
        cfg.insert(K_METRICS_WINDOW.to_string(), Value::String(w.clone()));
    }
    if let Some(s) = &body.stabilization_interval {
        if s.trim().is_empty() {
            return Err("stabilizationInterval 不能为空".into());
        }
        cfg.insert(K_STABILIZATION.to_string(), Value::String(s.clone()));
    }
    if let Some(f) = body.scale_up_max_factor {
        if f < 1.0 {
            return Err("scaleUpMaxFactor 不能小于 1.0".into());
        }
        cfg.insert(K_SCALE_UP_FACTOR.to_string(), Value::String(f.to_string()));
    }
    if let Some(f) = body.scale_down_max_factor {
        if !(0.0..=1.0).contains(&f) {
            return Err("scaleDownMaxFactor 需在 0–1.0".into());
        }
        cfg.insert(
            K_SCALE_DOWN_FACTOR.to_string(),
            Value::String(f.to_string()),
        );
    }

    match (body.vertex_min_parallelism, body.vertex_max_parallelism) {
        (Some(min), Some(max)) if min > max => {
            return Err("vertexMinParallelism 不能大于 vertexMaxParallelism".into());
        }
        _ => {}
    }
    if let Some(p) = body.vertex_min_parallelism {
        if p <= 0 {
            return Err("vertexMinParallelism 必须大于 0".into());
        }
        cfg.insert(K_VERTEX_MIN.to_string(), Value::String(p.to_string()));
    }
    if let Some(p) = body.vertex_max_parallelism {
        if p <= 0 {
            return Err("vertexMaxParallelism 必须大于 0".into());
        }
        cfg.insert(K_VERTEX_MAX.to_string(), Value::String(p.to_string()));
    }

    if body.excluded_periods.is_some() {
        for (field, why) in UNSUPPORTED_INPUTS {
            ignored.push(format!("{field}：{why}"));
        }
    }

    Ok((cfg, ignored))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> AutoscalerBody {
        AutoscalerBody {
            enabled: true,
            utilization_target: None,
            metrics_window: None,
            stabilization_interval: None,
            scale_up_max_factor: None,
            scale_down_max_factor: None,
            vertex_min_parallelism: None,
            vertex_max_parallelism: None,
            excluded_periods: None,
        }
    }

    #[test]
    fn key_table_matches_operator_names() {
        // 官方键名（写错会被 operator 静默忽略，所以逐条钉死）
        assert_eq!(K_ENABLED, "job.autoscaler.scaling.enabled");
        assert_eq!(K_UTILIZATION, "job.autoscaler.target.utilization");
        assert_eq!(K_METRICS_WINDOW, "job.autoscaler.metrics.window");
        assert_eq!(K_STABILIZATION, "job.autoscaler.stabilization.interval");
        assert_eq!(K_SCALE_UP_FACTOR, "job.autoscaler.scale-up.max-factor");
        assert_eq!(K_SCALE_DOWN_FACTOR, "job.autoscaler.scale-down.max-factor");
        assert_eq!(K_VERTEX_MIN, "job.autoscaler.vertex.min-parallelism");
        assert_eq!(K_VERTEX_MAX, "job.autoscaler.vertex.max-parallelism");
    }

    #[test]
    fn enabled_flag_is_written_as_string() {
        let mut b = body();
        b.enabled = false;
        let (cfg, _) = build_config(&b).unwrap();
        assert_eq!(cfg.get(K_ENABLED).unwrap(), &Value::String("false".into()));
        assert!(!cfg.contains_key("job.autoscaler.enabled"), "旧键不能再写");
    }

    #[test]
    fn utilization_goes_to_target_utilization_not_the_old_wrong_key() {
        let mut b = body();
        b.utilization_target = Some(0.75);
        let (cfg, _) = build_config(&b).unwrap();
        assert_eq!(
            cfg.get(K_UTILIZATION).unwrap(),
            &Value::String("0.75".into())
        );
        assert!(!cfg.contains_key("job.autoscaler.utilization.target"));
    }

    #[test]
    fn excluded_periods_is_ignored_but_reported() {
        let mut b = body();
        b.excluded_periods = Some("12:00-13:00".into());
        let (cfg, ignored) = build_config(&b).unwrap();
        assert!(!cfg.contains_key("job.autoscaler.excluded.periods"));
        assert_eq!(ignored.len(), 1);
        assert!(ignored[0].contains("excludedPeriods"));
    }

    #[test]
    fn invalid_values_are_rejected() {
        let mut b = body();
        b.utilization_target = Some(1.5);
        assert!(build_config(&b).is_err());

        let mut b = body();
        b.scale_down_max_factor = Some(1.5);
        assert!(build_config(&b).is_err());

        let mut b = body();
        b.vertex_min_parallelism = Some(4);
        b.vertex_max_parallelism = Some(2);
        assert!(build_config(&b).is_err());

        let mut b = body();
        b.vertex_min_parallelism = Some(0);
        assert!(build_config(&b).is_err());
    }

    #[test]
    fn full_body_produces_all_eight_keys() {
        let b = AutoscalerBody {
            enabled: true,
            utilization_target: Some(0.7),
            metrics_window: Some("15m".into()),
            stabilization_interval: Some("1h".into()),
            scale_up_max_factor: Some(2.0),
            scale_down_max_factor: Some(0.6),
            vertex_min_parallelism: Some(1),
            vertex_max_parallelism: Some(8),
            excluded_periods: None,
        };
        let (cfg, ignored) = build_config(&b).unwrap();
        assert_eq!(cfg.len(), 8, "八个官方键都要写");
        assert!(ignored.is_empty());
    }
}
