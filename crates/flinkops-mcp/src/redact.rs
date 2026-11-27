//! 脱敏（见 `docs/mcp-spec.md §5`）。
//!
//! 目标不是"安全无损"，而是**默认不把凭据带进模型的上下文**：
//! - YAML：删掉 last-applied / managedFields / 两个 podTemplate / lastReconciledSpec，
//!   env 的 value 一律打码
//! - 返回体里带 `redacted: true` 与 `dropped: [...]`，让调用方知道少了什么

use serde_json::Value;

/// YAML **文本级**脱敏：平台返回的是 YAML 字符串，这里按缩进块删除敏感段，
/// 并给 env 的 value 打码。返回值：(脱敏后文本, 被删掉的键)。
pub fn redact_yaml_text(text: &str) -> (String, Vec<String>) {
    const BLOCK_KEYS: &[&str] = &[
        "podTemplate:",
        "managedFields:",
        "kubectl.kubernetes.io/last-applied-configuration:",
        "lastReconciledSpec:",
        "lastStableSpec:",
    ];
    let mut out = String::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut skip_indent: Option<usize> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(bound) = skip_indent {
            if trimmed.is_empty() || indent > bound {
                continue;
            }
            skip_indent = None;
        }
        if BLOCK_KEYS.iter().any(|k| trimmed.starts_with(k)) {
            dropped.push(trimmed.split(':').next().unwrap_or("").to_string());
            out.push_str(&format!("{}# <redacted>\n", " ".repeat(indent)));
            skip_indent = Some(indent);
            continue;
        }
        if trimmed.starts_with("value:") {
            out.push_str(&format!("{}value: \"***\"\n", " ".repeat(indent)));
            if !dropped.iter().any(|d| d == "env.value") {
                dropped.push("env.value".into());
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    (out, dropped)
}

/// 供工具层拼装返回体用：文本 + 是否脱敏 + 被删字段。
pub fn redacted_yaml_payload(text: &str) -> Value {
    let (yaml, dropped) = redact_yaml_text(text);
    serde_json::json!({ "yaml": yaml, "redacted": true, "dropped": dropped })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_blocks_and_masks_env_values() {
        let text = "spec:\n  podTemplate:\n    spec:\n      containers: []\n  jobManager:\n    env:\n      - name: AK\n        value: real\n  parallelism: 2\n";
        let (out, dropped) = redact_yaml_text(text);
        assert!(!out.contains("containers"));
        assert!(out.contains("<redacted>"));
        assert!(!out.contains("value: real"));
        assert!(dropped.iter().any(|d| d == "podTemplate"));
        assert!(out.contains("parallelism: 2"));
    }

    #[test]
    fn keeps_harmless_lines_untouched() {
        let text = "kind: FlinkDeployment\nmetadata:\n  name: demo\n";
        let (out, dropped) = redact_yaml_text(text);
        assert_eq!(out, text);
        assert!(dropped.is_empty());
    }
}
