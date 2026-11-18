use crate::catalog::{Action, LocalizedText};
use crate::evaluator::{parse_reg_path, wow64_sam_list};
use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use winreg::enums::{RegType, KEY_ALL_ACCESS, KEY_READ};
use winreg::{RegKey, RegValue};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedRegValue {
    pub vtype: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedRegTree {
    pub values: BTreeMap<String, SavedRegValue>,
    pub subkeys: BTreeMap<String, SavedRegTree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SnapshotRecord {
    ValueState {
        key: String,
        value_name: String,
        sam: u32,
        previous: Option<SavedRegValue>,
    },
    KeyTreeState {
        key: String,
        sam: u32,
        previous: Option<SavedRegTree>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySnapshot {
    pub id: String,
    pub created_at: String,
    pub item_id: String,
    pub item_name: LocalizedText,
    pub scope: String,
    pub action_summary: String,
    pub restored: bool,
    pub records: Vec<SnapshotRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub id: String,
    pub created_at: String,
    pub item_id: String,
    pub item_name: LocalizedText,
    pub scope: String,
    pub action_summary: String,
    pub restored: bool,
    pub record_count: usize,
}

pub struct SnapshotManager {
    dir: PathBuf,
}

impl SnapshotManager {
    pub fn default_dir() -> PathBuf {
        PathBuf::from("data").join("snapshots")
    }

    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Captures pre-mutation registry state for a list of `Action`s before executing them.
    pub fn capture_before_actions(
        &self,
        item_id: &str,
        item_name: &LocalizedText,
        scope: &str,
        action_summary: &str,
        actions: &[Action],
    ) -> Result<Option<RegistrySnapshot>> {
        let mut records = Vec::new();
        for act in actions {
            match act {
                Action::RegWrite {
                    key,
                    value,
                    wow64,
                    ..
                } => {
                    for sam in wow64_sam_list(wow64.as_deref()) {
                        match value {
                            Some(vname) => {
                                records.push(capture_value_record(key, vname, sam));
                            }
                            None => {
                                records.push(capture_tree_record(key, sam));
                            }
                        }
                    }
                }
                Action::RegDelete {
                    key,
                    value,
                    wow64,
                    ..
                } => {
                    for sam in wow64_sam_list(wow64.as_deref()) {
                        match value {
                            Some(vname) => {
                                records.push(capture_value_record(key, vname, sam));
                            }
                            None => {
                                records.push(capture_tree_record(key, sam));
                            }
                        }
                    }
                }
                Action::RegMove {
                    key,
                    new_key,
                    value,
                    new_value,
                    wow64,
                    ..
                } => {
                    for sam in wow64_sam_list(wow64.as_deref()) {
                        match value {
                            Some(vname) => {
                                let dst_vname = new_value.as_deref().unwrap_or(vname);
                                records.push(capture_value_record(key, vname, sam));
                                records.push(capture_value_record(new_key, dst_vname, sam));
                            }
                            None => {
                                records.push(capture_tree_record(key, sam));
                                records.push(capture_tree_record(new_key, sam));
                            }
                        }
                    }
                }
                Action::SetServiceStart { name, .. } => {
                    let svc_key = format!(r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\{}", name);
                    for sam in wow64_sam_list(None) {
                        records.push(capture_value_record(&svc_key, "Start", sam));
                    }
                }
                _ => {}
            }
        }

        if records.is_empty() {
            return Ok(None);
        }

        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create snapshot directory {}", self.dir.display()))?;

        let now = Local::now();
        let id = format!(
            "snap_{}_{}",
            now.format("%Y%m%d_%H%M%S_%3f"),
            item_id.replace('-', "")
        );
        let snap = RegistrySnapshot {
            id: id.clone(),
            created_at: now.to_rfc3339(),
            item_id: item_id.to_string(),
            item_name: item_name.clone(),
            scope: scope.to_string(),
            action_summary: action_summary.to_string(),
            restored: false,
            records,
        };

        let path = self.snapshot_path(&id);
        let json = serde_json::to_string_pretty(&snap)?;
        std::fs::write(&path, json)?;
        Ok(Some(snap))
    }

    pub fn list(&self) -> Result<Vec<RegistrySnapshot>> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(snap) = serde_json::from_str::<RegistrySnapshot>(&text) {
                        items.push(snap);
                    }
                }
            }
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items)
    }

    pub fn load(&self, id: &str) -> Result<RegistrySnapshot> {
        let path = self.snapshot_path(id);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("Snapshot {} not found", id))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn restore(&self, id: &str) -> Result<RegistrySnapshot> {
        let mut snap = self.load(id)?;
        // Apply records in reverse order to accurately undo sequences like RegMove
        for rec in snap.records.iter().rev() {
            restore_record(rec)?;
        }
        snap.restored = true;
        let path = self.snapshot_path(id);
        std::fs::write(&path, serde_json::to_string_pretty(&snap)?)?;
        Ok(snap)
    }

    fn snapshot_path(&self, id: &str) -> PathBuf {
        let safe_id: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        self.dir.join(format!("{}.json", safe_id))
    }
}

fn capture_value_record(key: &str, value_name: &str, sam: u32) -> SnapshotRecord {
    let prev = read_saved_value(key, value_name, sam);
    SnapshotRecord::ValueState {
        key: key.to_string(),
        value_name: value_name.to_string(),
        sam,
        previous: prev,
    }
}

fn capture_tree_record(key: &str, sam: u32) -> SnapshotRecord {
    let prev = read_saved_tree(key, sam);
    SnapshotRecord::KeyTreeState {
        key: key.to_string(),
        sam,
        previous: prev,
    }
}

fn read_saved_value(key: &str, value_name: &str, sam: u32) -> Option<SavedRegValue> {
    let (hive, sub) = parse_reg_path(key)?;
    let root = RegKey::predef(hive);
    let k = root.open_subkey_with_flags(&sub, KEY_READ | sam).ok()?;
    let raw = k.get_raw_value(value_name).ok()?;
    Some(SavedRegValue {
        vtype: reg_type_to_str(&raw.vtype).to_string(),
        bytes: raw.bytes,
    })
}

pub fn read_saved_tree(key: &str, sam: u32) -> Option<SavedRegTree> {
    let (hive, sub) = parse_reg_path(key)?;
    let root = RegKey::predef(hive);
    let k = root.open_subkey_with_flags(&sub, KEY_READ | sam).ok()?;
    Some(read_tree_recursive(&k))
}

pub fn read_tree_recursive(k: &RegKey) -> SavedRegTree {
    let mut values = BTreeMap::new();
    for (name, val) in k.enum_values().flatten() {
        values.insert(
            name,
            SavedRegValue {
                vtype: reg_type_to_str(&val.vtype).to_string(),
                bytes: val.bytes,
            },
        );
    }
    let mut subkeys = BTreeMap::new();
    for sub_name in k.enum_keys().flatten() {
        if let Ok(sub_k) = k.open_subkey_with_flags(&sub_name, KEY_READ) {
            subkeys.insert(sub_name, read_tree_recursive(&sub_k));
        }
    }
    SavedRegTree { values, subkeys }
}

pub fn write_tree_recursive(k: &RegKey, tree: &SavedRegTree) -> Result<()> {
    for (name, val) in &tree.values {
        let rv = RegValue {
            vtype: str_to_reg_type(&val.vtype),
            bytes: val.bytes.clone(),
        };
        k.set_raw_value(name, &rv)?;
    }
    for (sub_name, sub_tree) in &tree.subkeys {
        let (sub_k, _) = k.create_subkey(sub_name)?;
        write_tree_recursive(&sub_k, sub_tree)?;
    }
    Ok(())
}

fn restore_record(rec: &SnapshotRecord) -> Result<()> {
    match rec {
        SnapshotRecord::ValueState {
            key,
            value_name,
            sam,
            previous,
        } => {
            let Some((hive, sub)) = parse_reg_path(key) else {
                return Ok(());
            };
            let root = RegKey::predef(hive);
            match previous {
                Some(saved) => {
                    let (k, _) = root
                        .create_subkey_with_flags(&sub, KEY_ALL_ACCESS | *sam)
                        .with_context(|| format!("Failed to open/create key {}", key))?;
                    let rv = RegValue {
                        vtype: str_to_reg_type(&saved.vtype),
                        bytes: saved.bytes.clone(),
                    };
                    k.set_raw_value(value_name, &rv)?;
                }
                None => {
                    if let Ok(k) = root.open_subkey_with_flags(&sub, KEY_ALL_ACCESS | *sam) {
                        let _ = k.delete_value(value_name);
                    }
                }
            }
        }
        SnapshotRecord::KeyTreeState { key, sam, previous } => {
            let Some((hive, sub)) = parse_reg_path(key) else {
                return Ok(());
            };
            let root = RegKey::predef(hive);
            // Delete existing key tree first to avoid leftover subkeys/values
            if let Some((parent_sub, leaf)) = split_parent_leaf(&sub) {
                if let Ok(parent_k) = root.open_subkey_with_flags(&parent_sub, KEY_ALL_ACCESS | *sam) {
                    let _ = parent_k.delete_subkey_all(&leaf);
                }
            } else {
                let _ = root.delete_subkey_all(&sub);
            }
            if let Some(tree) = previous {
                let (k, _) = root.create_subkey_with_flags(&sub, KEY_ALL_ACCESS | *sam)?;
                write_tree_recursive(&k, tree)?;
            }
        }
    }
    Ok(())
}

pub fn split_parent_leaf(sub: &str) -> Option<(String, String)> {
    let trimmed = sub.trim_end_matches('\\');
    let idx = trimmed.rfind('\\')?;
    Some((trimmed[..idx].to_string(), trimmed[idx + 1..].to_string()))
}

pub fn reg_type_to_str(t: &RegType) -> &'static str {
    match t {
        RegType::REG_NONE => "REG_NONE",
        RegType::REG_SZ => "REG_SZ",
        RegType::REG_EXPAND_SZ => "REG_EXPAND_SZ",
        RegType::REG_BINARY => "REG_BINARY",
        RegType::REG_DWORD => "REG_DWORD",
        RegType::REG_DWORD_BIG_ENDIAN => "REG_DWORD_BIG_ENDIAN",
        RegType::REG_LINK => "REG_LINK",
        RegType::REG_MULTI_SZ => "REG_MULTI_SZ",
        RegType::REG_QWORD => "REG_QWORD",
        _ => "REG_BINARY",
    }
}

pub fn str_to_reg_type(s: &str) -> RegType {
    match s {
        "REG_NONE" => RegType::REG_NONE,
        "REG_SZ" => RegType::REG_SZ,
        "REG_EXPAND_SZ" => RegType::REG_EXPAND_SZ,
        "REG_DWORD" => RegType::REG_DWORD,
        "REG_DWORD_BIG_ENDIAN" => RegType::REG_DWORD_BIG_ENDIAN,
        "REG_LINK" => RegType::REG_LINK,
        "REG_MULTI_SZ" => RegType::REG_MULTI_SZ,
        "REG_QWORD" => RegType::REG_QWORD,
        _ => RegType::REG_BINARY,
    }
}

pub fn snapshot_summary(s: &RegistrySnapshot) -> SnapshotSummary {
    SnapshotSummary {
        id: s.id.clone(),
        created_at: s.created_at.clone(),
        item_id: s.item_id.clone(),
        item_name: s.item_name.clone(),
        scope: s.scope.clone(),
        action_summary: s.action_summary.clone(),
        restored: s.restored,
        record_count: s.records.len(),
    }
}

#[allow(dead_code)]
fn _keep_path(_: &Path) {}
