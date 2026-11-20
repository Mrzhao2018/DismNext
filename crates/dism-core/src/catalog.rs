use crate::diskmap::is_protected_recycle_target;
use crate::evaluator::expand_expression;
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const EMBEDDED_CATALOG_JSON: &str = include_str!("../../../catalog/rules.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalizedText {
    pub zh: String,
    pub en: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Condition {
    True,
    False,
    And {
        children: Vec<Condition>,
    },
    Or {
        children: Vec<Condition>,
    },
    Not {
        children: Vec<Condition>,
    },
    OSVersion {
        compare: String,
        version: String,
    },
    FileExist {
        file_path: String,
        wow64: Option<String>,
    },
    FileVersion {
        file_path: String,
        compare: String,
        version: String,
    },
    ImageFlags {
        mask: u32,
    },
    Arch {
        arch: u32,
    },
    RegExist {
        key: String,
        value: Option<String>,
        reg_type: Option<String>,
        data: Option<String>,
        compare: Option<String>,
        wow64: Option<String>,
    },
    QueryServiceStart {
        name: String,
        start_type: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    General {
        root_path: String,
        flags: String,
        id: Option<String>,
        queries: Vec<String>,
        excluded: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_age_days: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        directory_markers: Vec<String>,
    },
    VersionedDirectory {
        root_path: String,
        version_file: String,
    },
    Custom {
        proc_name: String,
        flags: Option<String>,
    },
    RegWrite {
        key: String,
        value: Option<String>,
        reg_type: Option<String>,
        data: Option<String>,
        operator: String,
        wow64: Option<String>,
        fun: bool,
    },
    RegDelete {
        key: String,
        value: Option<String>,
        reg_type: Option<String>,
        data: Option<String>,
        skip_error: Option<i32>,
        wow64: Option<String>,
    },
    RegMove {
        key: String,
        new_key: String,
        value: Option<String>,
        new_value: Option<String>,
        skip_error: Option<i32>,
        wow64: Option<String>,
    },
    SetServiceStart {
        name: String,
        start_type: u32,
    },
    ExplorerNotify {
        notify_type: String,
        msg: Option<String>,
        w_param: Option<String>,
        l_param: Option<String>,
        cmd: Option<String>,
        explorer: bool,
    },
    FileCreateByZIP {
        path: String,
        zip_file: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupScan {
    pub work: u32,
    pub scan_type: Option<String>,
    pub name_override: Option<LocalizedText>,
    pub description_override: Option<LocalizedText>,
    pub warning_override: Option<LocalizedText>,
    pub applicable: Condition,
    pub named_applicables: HashMap<String, Condition>,
    pub actions: Vec<Action>,
    pub rescan: bool,
    pub restart: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupRule {
    pub id: String,
    pub name: LocalizedText,
    pub group: LocalizedText,
    pub description: LocalizedText,
    pub warning: Option<LocalizedText>,
    pub level: u8,
    pub work: Option<u32>,
    pub source: String,
    pub scans: Vec<CleanupScan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropdownOption {
    pub name: LocalizedText,
    pub applicable: Condition,
    pub actions: Vec<Action>,
    pub warning: Option<LocalizedText>,
    pub restart: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum OptimizationScope {
    #[serde(rename = "checkbox")]
    CheckBox {
        state: Condition,
        true_actions: Vec<Action>,
        true_warning: Option<LocalizedText>,
        true_restart: Option<String>,
        false_actions: Vec<Action>,
        false_warning: Option<LocalizedText>,
        false_restart: Option<String>,
    },
    #[serde(rename = "combo")]
    Combo { dropdowns: Vec<DropdownOption> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationItem {
    pub id: String,
    pub name: LocalizedText,
    pub item_type: String,
    pub source: String,
    pub applicable: Condition,
    pub current: Option<OptimizationScope>,
    pub default: Option<OptimizationScope>,
    pub system: Option<OptimizationScope>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationGroup {
    pub id: String,
    pub name: LocalizedText,
    pub applicable: Condition,
    pub items: Vec<OptimizationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomRuleInput {
    pub id: Option<String>,
    pub name_zh: String,
    pub name_en: Option<String>,
    pub description_zh: Option<String>,
    pub description_en: Option<String>,
    pub level: Option<u8>,
    pub root_path: String,
    pub queries: Vec<String>,
    pub excluded: Vec<String>,
    pub flags: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub schema_version: u32,
    pub base_version: String,
    pub cleanup_rules: Vec<CleanupRule>,
    pub optimization_groups: Vec<OptimizationGroup>,
}

impl Catalog {
    /// Loads an explicit catalog or the embedded baseline,
    /// and merges user-defined rules from `data/custom_rules.json`.
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let mut catalog: Self = if let Some(p) = explicit_path {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("Failed to read catalog at {}", p.display()))?;
            serde_json::from_str(&text)
                .with_context(|| format!("Failed to parse catalog JSON at {}", p.display()))?
        } else {
            serde_json::from_str(EMBEDDED_CATALOG_JSON)
                .context("Failed to parse embedded catalog/rules.json")?
        };

        let mut existing_ids: HashSet<String> =
            catalog.cleanup_rules.iter().map(|r| r.id.clone()).collect();
        for custom_rule in Self::load_custom_rules() {
            if existing_ids.insert(custom_rule.id.clone()) {
                catalog.cleanup_rules.push(custom_rule);
            }
        }

        Ok(catalog)
    }

    /// Resolves the path to user-defined cleanup rules (`data/custom_rules.json`).
    pub fn custom_rules_path() -> PathBuf {
        let cwd_path = PathBuf::from("data").join("custom_rules.json");
        if cwd_path.exists() {
            return cwd_path;
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let exe_data = dir.join("data").join("custom_rules.json");
                if exe_data.exists() {
                    return exe_data;
                }
                if Path::new("data").exists() {
                    return cwd_path;
                }
                return exe_data;
            }
        }
        cwd_path
    }

    /// Builds and safety-validates a `CleanupRule` from a user custom-rule form input.
    pub fn build_custom_rule(input: &CustomRuleInput) -> Result<CleanupRule> {
        let name_zh = input.name_zh.trim();
        if name_zh.is_empty() {
            return Err(anyhow!("Custom rule name must not be empty"));
        }
        let raw_root = input.root_path.trim();
        if raw_root.is_empty() {
            return Err(anyhow!("Custom rule root_path must not be empty"));
        }

        let queries: Vec<String> = input
            .queries
            .iter()
            .map(|q| q.trim().to_string())
            .filter(|q| !q.is_empty())
            .collect();
        let excluded: Vec<String> = input
            .excluded
            .iter()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();

        validate_custom_action(raw_root, &queries)?;

        let id = input
            .id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let ts = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
                format!("custom-{}", ts)
            });
        validate_custom_id(&id)?;

        let name_en = input
            .name_en
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(name_zh)
            .to_string();
        let desc_zh = input
            .description_zh
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if queries.is_empty() {
                    format!("自定义目录清理：{}", raw_root)
                } else {
                    format!("自定义匹配清理：{} ({})", raw_root, queries.join(", "))
                }
            });
        let desc_en = input
            .description_en
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Custom cleanup target: {}", raw_root));

        let flags = input
            .flags
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Directory|File")
            .to_string();

        let level = input.level.unwrap_or(2).clamp(1, 3);

        Ok(CleanupRule {
            id,
            name: LocalizedText {
                zh: name_zh.to_string(),
                en: name_en,
            },
            group: LocalizedText {
                zh: "自定义规则".to_string(),
                en: "Custom Rules".to_string(),
            },
            description: LocalizedText {
                zh: desc_zh,
                en: desc_en,
            },
            warning: None,
            level,
            work: None,
            source: "custom".to_string(),
            scans: vec![CleanupScan {
                work: 10,
                scan_type: None,
                name_override: None,
                description_override: None,
                warning_override: None,
                applicable: Condition::True,
                named_applicables: HashMap::new(),
                actions: vec![Action::General {
                    root_path: raw_root.to_string(),
                    flags,
                    id: None,
                    queries,
                    excluded,
                    min_age_days: None,
                    directory_markers: Vec::new(),
                }],
                rescan: false,
                restart: None,
            }],
        })
    }

    pub fn load_custom_rules() -> Vec<CleanupRule> {
        load_custom_rules_from(&Self::custom_rules_path())
    }

    pub fn save_custom_rule(input: CustomRuleInput) -> Result<CleanupRule> {
        save_custom_rule_to(&Self::custom_rules_path(), input)
    }

    pub fn delete_custom_rule(rule_id: &str) -> Result<()> {
        delete_custom_rule_from(&Self::custom_rules_path(), rule_id)
    }
}

fn load_custom_rules_from(path: &Path) -> Vec<CleanupRule> {
    read_custom_rules(path)
        .unwrap_or_default()
        .into_iter()
        .filter(|rule| {
            rule.source == "custom"
                && validate_custom_id(&rule.id).is_ok()
                && !rule.scans.is_empty()
                && rule.scans.iter().all(|scan| {
                    !scan.actions.is_empty()
                        && scan.actions.iter().all(|action| {
                            matches!(action, Action::General { root_path, queries, .. }
                            if validate_custom_action(root_path, queries).is_ok())
                        })
                })
        })
        .collect()
}

fn read_custom_rules(path: &Path) -> Result<Vec<CleanupRule>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse custom rules at {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to read custom rules at {}", path.display()))
        }
    }
}

fn save_custom_rule_to(path: &Path, input: CustomRuleInput) -> Result<CleanupRule> {
    let rule = Catalog::build_custom_rule(&input)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut existing = read_custom_rules(path)?;
    if let Some(pos) = existing.iter().position(|r| r.id == rule.id) {
        existing[pos] = rule.clone();
    } else {
        existing.push(rule.clone());
    }
    std::fs::write(path, serde_json::to_string_pretty(&existing)?)?;
    Ok(rule)
}

fn delete_custom_rule_from(path: &Path, rule_id: &str) -> Result<()> {
    validate_custom_id(rule_id)?;
    let mut existing = read_custom_rules(path)?;
    let before = existing.len();
    existing.retain(|r| r.id != rule_id);
    if existing.len() == before {
        return Err(anyhow!("Custom rule '{}' not found", rule_id));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&existing)?)?;
    Ok(())
}

fn validate_custom_id(id: &str) -> Result<()> {
    ensure!(
        id.starts_with("custom-") && id.len() > "custom-".len(),
        "Custom rule IDs must start with 'custom-'"
    );
    Ok(())
}

fn validate_custom_action(root_path: &str, queries: &[String]) -> Result<()> {
    let expanded = expand_expression(root_path, None)
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| anyhow!("Could not resolve custom cleanup root '{root_path}'"))?;
    let normalized = expanded.trim().replace('/', "\\");
    let mut root = normalized.as_str();
    if queries.is_empty() {
        if let Some((parent, leaf)) = root.rsplit_once('\\') {
            if leaf.contains(['*', '?']) {
                root = parent;
            }
        }
    }
    let system_drive_pattern;
    if let Some(suffix) = root.strip_prefix(r"*\") {
        let windows_dir = crate::system_tools::windows_dir()?;
        let volume = windows_dir.ancestors().last().unwrap_or(&windows_dir);
        system_drive_pattern = volume.join(suffix);
        root = system_drive_pattern
            .to_str()
            .ok_or_else(|| anyhow!("Invalid custom cleanup root"))?;
    }
    ensure!(
        !root.is_empty() && root != "*" && !is_protected_recycle_target(Path::new(root)),
        "Custom cleanup root '{root_path}' is protected"
    );
    ensure!(
        queries.iter().all(|query| {
            !query.contains([':', '\0'])
                && !query.starts_with(['\\', '/'])
                && query
                    .split(['\\', '/'])
                    .all(|part| !part.ends_with(['.', ' ']))
        }),
        "Custom rule queries must stay within the cleanup root"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_rule_validation_and_persistence_sandbox() {
        let base = std::env::temp_dir().join(format!("dismnext_custom_rule_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let file_path = base.join("custom_rules.json");

        // Reject unsafe bare system root without queries
        let bad = CustomRuleInput {
            id: None,
            name_zh: "Danger".into(),
            name_en: None,
            description_zh: None,
            description_en: None,
            level: Some(2),
            root_path: r"%SystemRoot%".into(),
            queries: vec![],
            excluded: vec![],
            flags: None,
        };
        assert!(Catalog::build_custom_rule(&bad).is_err());
        assert!(validate_custom_action(r"C:\", &["*".into()]).is_err());
        assert!(validate_custom_action(r"%SystemRoot%\.", &["*".into()]).is_err());
        assert!(validate_custom_action(r"*\Windows", &["*".into()]).is_err());
        assert!(validate_custom_action(r"C:\example", &[r"..\*".into()]).is_err());
        assert!(validate_custom_id("clean-042").is_err());

        // Safe custom directory saves, loads, and deletes cleanly
        let ok_in = CustomRuleInput {
            id: Some("custom-test-01".into()),
            name_zh: "我的构建日志".into(),
            name_en: Some("Build Logs".into()),
            description_zh: None,
            description_en: None,
            level: Some(3),
            root_path: base.to_string_lossy().into_owned(),
            queries: vec!["*.log".into()],
            excluded: vec!["keep.log".into()],
            flags: Some("File".into()),
        };
        let saved = save_custom_rule_to(&file_path, ok_in).unwrap();
        assert_eq!(saved.id, "custom-test-01");
        assert_eq!(saved.source, "custom");

        let loaded = load_custom_rules_from(&file_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name.en, "Build Logs");

        delete_custom_rule_from(&file_path, "custom-test-01").unwrap();
        assert!(load_custom_rules_from(&file_path).is_empty());

        let _ = std::fs::remove_dir_all(&base);
    }
}
