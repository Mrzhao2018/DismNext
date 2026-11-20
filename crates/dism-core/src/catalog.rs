use crate::diskmap::is_protected_recycle_target;
use crate::evaluator::{expand_expression, has_balanced_inline_calls};
use crate::win_util::{has_reparse_point_in_ancestors, is_local_drive_path, is_unc_path};
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const EMBEDDED_CATALOG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rules.json.gz"));

pub(crate) fn parse_embedded_catalog() -> Result<Catalog> {
    use std::io::Read;

    let mut json = Vec::new();
    flate2::read::GzDecoder::new(EMBEDDED_CATALOG)
        .read_to_end(&mut json)
        .context("Failed to decompress the embedded rule catalog")?;
    serde_json::from_slice(&json).context("Failed to parse embedded catalog/rules.json")
}

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
    CheckBox(Box<OptimizationCheckBox>),
    #[serde(rename = "combo")]
    Combo { dropdowns: Vec<DropdownOption> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationCheckBox {
    pub state: Condition,
    pub true_actions: Vec<Action>,
    pub true_warning: Option<LocalizedText>,
    pub true_restart: Option<String>,
    pub false_actions: Vec<Action>,
    pub false_warning: Option<LocalizedText>,
    pub false_restart: Option<String>,
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
            parse_embedded_catalog()?
        };

        validate_catalog_baseline(&catalog)?;

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
        crate::storage::data_directories()
            .primary
            .join("custom_rules.json")
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
                static CUSTOM_RULE_SEQ: AtomicU64 = AtomicU64::new(1);
                let ts = chrono::Local::now().format("%Y%m%d%H%M%S%3f").to_string();
                let seq = CUSTOM_RULE_SEQ.fetch_add(1, Ordering::Relaxed);
                format!("custom-{ts}-{seq}")
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
        let primary = Self::custom_rules_path();
        let legacy = legacy_custom_rules_path();
        load_custom_rules_from(custom_rules_source(&primary, legacy.as_deref()))
    }

    pub fn save_custom_rule(input: CustomRuleInput) -> Result<CleanupRule> {
        save_custom_rule_to(
            &Self::custom_rules_path(),
            legacy_custom_rules_path().as_deref(),
            input,
        )
    }

    pub fn delete_custom_rule(rule_id: &str) -> Result<()> {
        delete_custom_rule_from(
            &Self::custom_rules_path(),
            legacy_custom_rules_path().as_deref(),
            rule_id,
        )
    }
}

fn legacy_custom_rules_path() -> Option<PathBuf> {
    crate::storage::data_directories()
        .legacy
        .iter()
        .map(|path| path.join("custom_rules.json"))
        .find(|path| path.exists())
}

fn custom_rules_source<'a>(path: &'a Path, legacy: Option<&'a Path>) -> &'a Path {
    if matches!(path.try_exists(), Ok(false)) {
        legacy.unwrap_or(path)
    } else {
        path
    }
}

fn load_custom_rules_from(path: &Path) -> Vec<CleanupRule> {
    read_custom_rules(path)
        .unwrap_or_else(|error| {
            eprintln!("Custom rules could not be loaded: {error:#}");
            Vec::new()
        })
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

fn save_custom_rule_to(
    path: &Path,
    legacy: Option<&Path>,
    input: CustomRuleInput,
) -> Result<CleanupRule> {
    let rule = Catalog::build_custom_rule(&input)?;
    edit_custom_rules(path, legacy, |rules| {
        if let Some(pos) = rules.iter().position(|r| r.id == rule.id) {
            rules[pos] = rule.clone();
        } else {
            rules.push(rule.clone());
        }
        Ok(())
    })?;
    Ok(rule)
}

fn delete_custom_rule_from(path: &Path, legacy: Option<&Path>, rule_id: &str) -> Result<()> {
    validate_custom_id(rule_id)?;
    edit_custom_rules(path, legacy, |rules| {
        let before = rules.len();
        rules.retain(|r| r.id != rule_id);
        ensure!(rules.len() != before, "Custom rule '{rule_id}' not found");
        Ok(())
    })
}

fn edit_custom_rules(
    path: &Path,
    legacy: Option<&Path>,
    edit: impl FnOnce(&mut Vec<CleanupRule>) -> Result<()>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut rules = read_custom_rules(custom_rules_source(path, legacy))?;
    edit(&mut rules)?;
    crate::storage::write_atomic(path, &serde_json::to_vec_pretty(&rules)?)
        .with_context(|| format!("Failed to save custom rules at {}", path.display()))
}

fn validate_custom_id(id: &str) -> Result<()> {
    ensure!(
        id.starts_with("custom-") && id.len() > "custom-".len(),
        "Custom rule IDs must start with 'custom-'"
    );
    Ok(())
}

fn validate_catalog_baseline(catalog: &Catalog) -> Result<()> {
    ensure!(
        catalog.schema_version == 1,
        "Unsupported catalog schema_version {}",
        catalog.schema_version
    );
    let mut ids = HashSet::new();
    for id in catalog.cleanup_rules.iter().map(|rule| &rule.id).chain(
        catalog.optimization_groups.iter().flat_map(|group| {
            std::iter::once(&group.id).chain(group.items.iter().map(|item| &item.id))
        }),
    ) {
        ensure!(
            !id.trim().is_empty() && ids.insert(id.as_str()),
            "Duplicate or empty catalog ID: '{id}'"
        );
    }
    let val = serde_json::to_value(catalog)?;
    check_catalog_value_expressions(&val, false)?;
    Ok(())
}

fn check_catalog_value_expressions(
    value: &serde_json::Value,
    interactive_data: bool,
) -> Result<()> {
    match value {
        serde_json::Value::String(text) => {
            ensure!(
                has_balanced_inline_calls(text),
                "Unclosed inline expression in catalog: {text}"
            );
            ensure!(
                interactive_data || !text.contains("?ColorDialog("),
                "Unmarked ColorDialog expression in catalog: {text}"
            );
        }
        serde_json::Value::Array(values) => {
            for item in values {
                check_catalog_value_expressions(item, false)?;
            }
        }
        serde_json::Value::Object(fields) => {
            let interactive = fields.get("type").and_then(serde_json::Value::as_str)
                == Some("RegWrite")
                && fields.get("fun").and_then(serde_json::Value::as_bool) == Some(true);
            for (name, v) in fields {
                check_catalog_value_expressions(v, interactive && name == "data")?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_custom_action(root_path: &str, queries: &[String]) -> Result<()> {
    ensure!(
        !is_unc_path(Path::new(root_path.trim())),
        "Custom cleanup root '{root_path}' cannot be a network/UNC path"
    );
    let expanded = expand_expression(root_path, None)
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| anyhow!("Could not resolve custom cleanup root '{root_path}'"))?;
    let normalized = expanded.trim().replace('/', "\\");
    ensure!(
        !is_unc_path(Path::new(&normalized)),
        "Custom cleanup root '{root_path}' cannot be a network/UNC path"
    );
    ensure!(
        !Path::new(&normalized)
            .components()
            .any(|part| matches!(part, Component::ParentDir)),
        "Custom cleanup root '{root_path}' must not contain '..'"
    );

    let mut root_buf = normalized.clone();
    if let Some(suffix) = root_buf.strip_prefix(r"*\") {
        let windows_dir = crate::system_tools::windows_dir()?;
        let volume = windows_dir.ancestors().last().unwrap_or(&windows_dir);
        root_buf = volume.join(suffix).to_string_lossy().into_owned();
    }

    // Always evaluate the deepest static prefix before any wildcard (`*` / `?`),
    // regardless of whether `queries` is empty or non-empty.
    let prefix = static_root_prefix(&root_buf);
    ensure!(
        !prefix.is_empty(),
        "Custom cleanup root '{root_path}' has no static directory"
    );
    let prefix = std::path::absolute(prefix)?;
    ensure!(
        is_local_drive_path(&prefix)
            && !is_protected_recycle_target(&prefix)
            && !has_reparse_point_in_ancestors(&prefix, true),
        "Custom cleanup root '{root_path}' is protected"
    );
    ensure!(
        queries.iter().all(|query| {
            !query.contains([':', '\0'])
                && !query.starts_with(['\\', '/'])
                && query
                    .split(['\\', '/'])
                    .all(|part| part != ".." && !part.ends_with(['.', ' ']))
        }),
        "Custom rule queries must stay within the cleanup root"
    );
    Ok(())
}

fn static_root_prefix(root: &str) -> String {
    let mut parts = Vec::new();
    for seg in root.split('\\') {
        if seg.contains(['*', '?']) {
            break;
        }
        parts.push(seg);
    }
    let mut joined = parts.join("\\");
    if joined.len() == 2 && joined.ends_with(':') {
        joined.push('\\');
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::has_balanced_inline_calls;
    use serde_json::Value;
    use std::collections::HashSet;

    #[test]
    fn embedded_catalog_has_unique_ids_and_valid_inline_expressions() {
        let catalog = parse_embedded_catalog().unwrap();
        assert!(!catalog.cleanup_rules.is_empty());
        assert!(!catalog.optimization_groups.is_empty());
        let mut ids = HashSet::new();
        for id in catalog.cleanup_rules.iter().map(|rule| &rule.id).chain(
            catalog.optimization_groups.iter().flat_map(|group| {
                std::iter::once(&group.id).chain(group.items.iter().map(|item| &item.id))
            }),
        ) {
            assert!(
                !id.is_empty() && ids.insert(id),
                "Duplicate or empty ID: {id}"
            );
        }
        validate_expressions(&serde_json::to_value(catalog).unwrap(), false);
    }

    fn validate_expressions(value: &Value, interactive_data: bool) {
        match value {
            Value::String(text) => {
                assert!(
                    has_balanced_inline_calls(text),
                    "Unclosed inline call: {text}"
                );
                assert!(
                    interactive_data || !text.contains("?ColorDialog("),
                    "Unmarked dialog: {text}"
                );
            }
            Value::Array(values) => {
                for value in values {
                    validate_expressions(value, false);
                }
            }
            Value::Object(fields) => {
                let interactive = fields.get("type").and_then(Value::as_str) == Some("RegWrite")
                    && fields.get("fun").and_then(Value::as_bool) == Some(true);
                for (name, value) in fields {
                    validate_expressions(value, interactive && name == "data");
                }
            }
            _ => {}
        }
    }

    #[test]
    fn test_custom_rule_validation_and_persistence_sandbox() {
        let base =
            std::env::temp_dir().join(format!("dismnext_custom_rule_{}", std::process::id()));
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
        assert!(validate_custom_action(r"%SystemRoot%\*", &["*".into()]).is_err());
        assert!(validate_custom_action(r"%SystemRoot%\Servicing\*", &[]).is_err());
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
        let saved = save_custom_rule_to(&file_path, None, ok_in.clone()).unwrap();
        assert_eq!(saved.id, "custom-test-01");
        assert_eq!(saved.source, "custom");

        let loaded = load_custom_rules_from(&file_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name.en, "Build Logs");

        let legacy = base.join("legacy.json");
        std::fs::rename(&file_path, &legacy).unwrap();
        let original = std::fs::read(&legacy).unwrap();
        assert_eq!(custom_rules_source(&file_path, Some(&legacy)), legacy);
        delete_custom_rule_from(&file_path, Some(&legacy), "custom-test-01").unwrap();
        assert!(load_custom_rules_from(&file_path).is_empty());
        assert_eq!(std::fs::read(&legacy).unwrap(), original);

        let invalid_json = b"[corrupt data";
        std::fs::write(&file_path, invalid_json).unwrap();
        assert!(save_custom_rule_to(&file_path, Some(&legacy), ok_in).is_err());
        assert!(delete_custom_rule_from(&file_path, Some(&legacy), "custom-test-01").is_err());
        assert_eq!(std::fs::read(&file_path).unwrap(), invalid_json);

        let _ = std::fs::remove_dir_all(&base);
    }
}
