use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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
pub struct Catalog {
    pub schema_version: u32,
    pub base_version: String,
    pub cleanup_rules: Vec<CleanupRule>,
    pub optimization_groups: Vec<OptimizationGroup>,
}

impl Catalog {
    /// Loads catalog from an explicit path, `catalog/rules.json` in CWD/exe dir, or embedded JSON fallback.
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit_path {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("Failed to read catalog at {}", p.display()))?;
            return serde_json::from_str(&text)
                .with_context(|| format!("Failed to parse catalog JSON at {}", p.display()));
        }

        let candidates = [
            Path::new("catalog/rules.json").to_path_buf(),
            std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.join("catalog/rules.json")))
                .unwrap_or_default(),
        ];

        for c in candidates {
            if !c.as_os_str().is_empty() && c.exists() {
                let text = std::fs::read_to_string(&c)
                    .with_context(|| format!("Failed to read catalog at {}", c.display()))?;
                return serde_json::from_str(&text)
                    .with_context(|| format!("Failed to parse catalog JSON at {}", c.display()));
            }
        }

        serde_json::from_str(EMBEDDED_CATALOG_JSON)
            .context("Failed to parse embedded catalog/rules.json")
    }
}
