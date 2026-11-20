use crate::catalog::{Action, LocalizedText};
use crate::evaluator::{parse_reg_path, wow64_sam_list};
use anyhow::{anyhow, Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use winreg::enums::{
    RegType, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_ALL_ACCESS, KEY_READ,
};
use winreg::{RegKey, RegValue};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRegValue {
    pub vtype: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    MissingKey {
        key: String,
        sam: u32,
    },
    ShellRefresh(ShellRefresh),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refresh", rename_all = "snake_case")]
pub enum ShellRefresh {
    Associations,
    TraySettings,
    TrayCommand,
    StartMenu,
}

impl ShellRefresh {
    pub(crate) fn from_action(action: &Action) -> Option<Self> {
        let Action::ExplorerNotify {
            notify_type,
            msg,
            w_param,
            l_param,
            cmd,
            ..
        } = action
        else {
            return None;
        };
        match notify_type.trim() {
            "AssocChanged" => Some(Self::Associations),
            "Custom"
                if msg.as_deref() == Some("1A")
                    && w_param.is_none()
                    && l_param.as_deref() == Some("TraySettings") =>
            {
                Some(Self::TraySettings)
            }
            "Custom"
                if msg.as_deref() == Some("111")
                    && w_param.as_deref() == Some("#A220")
                    && l_param.is_none() =>
            {
                Some(Self::TrayCommand)
            }
            "Cmd"
                if cmd
                    .as_deref()
                    .is_some_and(crate::optimizer::is_start_menu_refresh) =>
            {
                Some(Self::StartMenu)
            }
            _ => None,
        }
    }
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
    legacy_dirs: Vec<PathBuf>,
    mutation: Mutex<()>,
}

impl SnapshotManager {
    pub fn default_dir() -> PathBuf {
        crate::storage::data_directories().primary.join("snapshots")
    }

    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            legacy_dirs: Vec::new(),
            mutation: Mutex::new(()),
        }
    }

    pub fn user_default() -> Self {
        let mut manager = Self::new(Self::default_dir());
        for dir in &crate::storage::data_directories().legacy {
            manager.add_legacy_directory(dir.join("snapshots"));
        }
        manager
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn add_legacy_directory(&mut self, dir: PathBuf) {
        if self.dir != dir && !self.legacy_dirs.contains(&dir) {
            self.legacy_dirs.push(dir);
        }
    }

    fn directories(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.dir.as_path()).chain(
            self.legacy_dirs
                .iter()
                .map(PathBuf::as_path)
                .filter(|dir| dir.is_dir()),
        )
    }

    pub(crate) fn lock_mutation(&self) -> Result<MutexGuard<'_, ()>> {
        self.mutation
            .lock()
            .map_err(|_| anyhow!("Registry operation lock is poisoned"))
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
        let mut refresh = Vec::new();
        for act in actions {
            match act {
                Action::RegWrite {
                    key, value, wow64, ..
                } => {
                    for sam in wow64_sam_list(wow64.as_deref()) {
                        capture_missing_keys(&mut records, key, sam)?;
                        if let Some(value) = value {
                            records.push(capture_value_record(key, value, sam)?);
                        }
                    }
                }
                Action::RegDelete {
                    key, value, wow64, ..
                } => {
                    for sam in wow64_sam_list(wow64.as_deref()) {
                        match value {
                            Some(vname) => {
                                records.push(capture_value_record(key, vname, sam)?);
                            }
                            None => {
                                records.push(capture_tree_record(key, sam)?);
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
                        capture_missing_keys(&mut records, new_key, sam)?;
                        match value {
                            Some(vname) => {
                                let dst_vname = new_value.as_deref().unwrap_or(vname);
                                records.push(capture_value_record(key, vname, sam)?);
                                records.push(capture_value_record(new_key, dst_vname, sam)?);
                            }
                            None => {
                                records.push(capture_tree_record(key, sam)?);
                                records.push(capture_tree_record(new_key, sam)?);
                            }
                        }
                    }
                }
                Action::SetServiceStart { name, .. } => {
                    let svc_key = format!(
                        r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\{}",
                        name
                    );
                    for sam in wow64_sam_list(None) {
                        capture_missing_keys(&mut records, &svc_key, sam)?;
                        records.push(capture_value_record(&svc_key, "Start", sam)?);
                    }
                }
                Action::ExplorerNotify { .. } => {
                    if let Some(notification) = ShellRefresh::from_action(act) {
                        if !refresh.contains(&notification) {
                            refresh.push(notification);
                        }
                    }
                }
                _ => {}
            }
        }

        if records.is_empty() {
            return Ok(None);
        }
        records.extend(refresh.into_iter().map(SnapshotRecord::ShellRefresh));

        std::fs::create_dir_all(&self.dir).with_context(|| {
            format!("Failed to create snapshot directory {}", self.dir.display())
        })?;

        let now = Local::now();
        let id = format!(
            "snap_{}_{}_{}",
            now.format("%Y%m%d_%H%M%S_%9f"),
            unique_suffix(),
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
        write_snapshot(&path, &snap)?;
        Ok(Some(snap))
    }

    pub fn list(&self) -> Result<Vec<RegistrySnapshot>> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for dir in self.directories() {
            let Some(entries) = missing_is_none(std::fs::read_dir(dir))? else {
                continue;
            };
            for entry in entries {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        if let Ok(snapshot) = serde_json::from_str::<RegistrySnapshot>(&text) {
                            if seen.insert(snapshot.id.clone()) {
                                items.push(snapshot);
                            }
                        }
                    }
                }
            }
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(items)
    }

    pub fn load(&self, id: &str) -> Result<RegistrySnapshot> {
        let path = self.snapshot_path(id);
        let name = path
            .file_name()
            .expect("snapshot path always has a filename");
        for dir in self.directories() {
            if let Some(text) = missing_is_none(std::fs::read_to_string(dir.join(name)))? {
                return Ok(serde_json::from_str(&text)?);
            }
        }
        Err(anyhow!("Snapshot {} not found", id))
    }

    pub fn restore(&self, id: &str) -> Result<RegistrySnapshot> {
        let _mutation = self.lock_mutation()?;
        let mut snap = self.load(id)?;
        if snapshot_requires_admin(&snap) && !crate::sysinfo::SystemInfo::collect().is_admin {
            return Err(anyhow!(
                "该快照包含系统级注册表项 (HKLM/HKU)，回滚需要管理员权限。请先点击顶部“一键提权为管理员”后再执行回滚。 / Restoring system-level registry snapshots requires Administrator privileges."
            ));
        }
        std::fs::create_dir_all(&self.dir).with_context(|| {
            format!("Failed to create snapshot directory {}", self.dir.display())
        })?;
        self.restore_records(&snap)?;
        for record in &snap.records {
            if let SnapshotRecord::ShellRefresh(notification) = record {
                crate::optimizer::restore_shell_refresh(notification)?;
            }
        }
        snap.restored = true;
        let path = self.snapshot_path(id);
        write_snapshot(&path, &snap)?;
        Ok(snap)
    }

    pub(crate) fn restore_records(&self, snap: &RegistrySnapshot) -> Result<()> {
        for record in snap
            .records
            .iter()
            .rev()
            .filter(|record| !matches!(record, SnapshotRecord::ShellRefresh(_)))
        {
            if record_changed(record)? {
                restore_record(record)
                    .with_context(|| format!("Failed to restore snapshot {}", snap.id))?;
            }
        }
        Ok(())
    }

    pub(crate) fn has_changes(&self, snap: &RegistrySnapshot) -> Result<bool> {
        for record in &snap.records {
            if record_changed(record)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn discard(&self, snap: &RegistrySnapshot) -> Result<()> {
        missing_is_none(std::fs::remove_file(self.snapshot_path(&snap.id)))?;
        Ok(())
    }

    fn snapshot_path(&self, id: &str) -> PathBuf {
        let safe_id: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        self.dir.join(format!("{}.json", safe_id))
    }
}

fn unique_suffix() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}_{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_snapshot(path: &Path, snap: &RegistrySnapshot) -> Result<()> {
    let json = serde_json::to_vec_pretty(snap)?;
    crate::storage::write_atomic(path, &json)
        .with_context(|| format!("Failed to persist snapshot {}", path.display()))
}

fn missing_is_none<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn capture_value_record(key: &str, value_name: &str, sam: u32) -> Result<SnapshotRecord> {
    let prev = read_saved_value(key, value_name, sam)
        .with_context(|| format!("Failed to back up registry value {key}\\{value_name}"))?;
    Ok(SnapshotRecord::ValueState {
        key: key.to_string(),
        value_name: value_name.to_string(),
        sam,
        previous: prev,
    })
}

fn capture_missing_keys(records: &mut Vec<SnapshotRecord>, key: &str, sam: u32) -> Result<()> {
    let mut missing = Vec::new();
    let mut candidate = key.trim().trim_end_matches('\\');
    while let Some((parent, _)) = candidate.rsplit_once('\\') {
        let (hive, sub) = parse_reg_path(candidate)
            .ok_or_else(|| anyhow!("Invalid registry key: {candidate}"))?;
        if missing_is_none(RegKey::predef(hive).open_subkey_with_flags(sub, KEY_READ | sam))?
            .is_some()
        {
            break;
        }
        missing.push(SnapshotRecord::MissingKey {
            key: candidate.to_string(),
            sam,
        });
        candidate = parent;
    }
    records.extend(missing.into_iter().rev());
    Ok(())
}

fn record_changed(record: &SnapshotRecord) -> Result<bool> {
    Ok(match record {
        SnapshotRecord::ValueState {
            key,
            value_name,
            sam,
            previous,
        } => read_saved_value(key, value_name, *sam)? != *previous,
        SnapshotRecord::KeyTreeState { key, sam, previous } => {
            read_saved_tree(key, *sam)? != *previous
        }
        SnapshotRecord::MissingKey { key, sam } => {
            let (hive, sub) =
                parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {key}"))?;
            missing_is_none(RegKey::predef(hive).open_subkey_with_flags(sub, KEY_READ | *sam))?
                .is_some()
        }
        SnapshotRecord::ShellRefresh(_) => false,
    })
}

fn capture_tree_record(key: &str, sam: u32) -> Result<SnapshotRecord> {
    let prev = read_saved_tree(key, sam)
        .with_context(|| format!("Failed to back up registry tree {key}"))?;
    Ok(SnapshotRecord::KeyTreeState {
        key: key.to_string(),
        sam,
        previous: prev,
    })
}

fn read_saved_value(key: &str, value_name: &str, sam: u32) -> Result<Option<SavedRegValue>> {
    let (hive, sub) = parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {key}"))?;
    let root = RegKey::predef(hive);
    let Some(k) = missing_is_none(root.open_subkey_with_flags(&sub, KEY_READ | sam))? else {
        return Ok(None);
    };
    Ok(
        missing_is_none(k.get_raw_value(value_name))?.map(|raw| SavedRegValue {
            vtype: reg_type_to_str(&raw.vtype).to_string(),
            bytes: raw.bytes,
        }),
    )
}

pub fn read_saved_tree(key: &str, sam: u32) -> Result<Option<SavedRegTree>> {
    let (hive, sub) = parse_reg_path(key)
        .filter(|(_, sub)| !sub.is_empty())
        .ok_or_else(|| anyhow!("Invalid registry tree key: {key}"))?;
    let root = RegKey::predef(hive);
    missing_is_none(root.open_subkey_with_flags(&sub, KEY_READ | sam))?
        .map(|k| read_tree_recursive(&k, sam))
        .transpose()
}

pub fn read_tree_recursive(k: &RegKey, sam: u32) -> Result<SavedRegTree> {
    let mut values = BTreeMap::new();
    for entry in k.enum_values() {
        let (name, val) = entry?;
        values.insert(
            name,
            SavedRegValue {
                vtype: reg_type_to_str(&val.vtype).to_string(),
                bytes: val.bytes,
            },
        );
    }
    let mut subkeys = BTreeMap::new();
    for entry in k.enum_keys() {
        let sub_name = entry?;
        let sub_k = k
            .open_subkey_with_flags(&sub_name, KEY_READ | sam)
            .with_context(|| format!("Failed to read registry subkey {sub_name}"))?;
        subkeys.insert(sub_name, read_tree_recursive(&sub_k, sam)?);
    }
    Ok(SavedRegTree { values, subkeys })
}

pub fn write_tree_recursive(k: &RegKey, tree: &SavedRegTree, sam: u32) -> Result<()> {
    for (name, val) in &tree.values {
        let rv = RegValue {
            vtype: str_to_reg_type(&val.vtype),
            bytes: val.bytes.clone(),
        };
        k.set_raw_value(name, &rv)?;
    }
    for (sub_name, sub_tree) in &tree.subkeys {
        let (sub_k, _) = k.create_subkey_with_flags(sub_name, KEY_ALL_ACCESS | sam)?;
        write_tree_recursive(&sub_k, sub_tree, sam)?;
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
            let (hive, sub) =
                parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {key}"))?;
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
                    if let Some(k) =
                        missing_is_none(root.open_subkey_with_flags(&sub, KEY_ALL_ACCESS | *sam))?
                    {
                        missing_is_none(k.delete_value(value_name))?;
                    }
                }
            }
        }
        SnapshotRecord::KeyTreeState { key, sam, previous } => {
            let (hive, sub) = parse_reg_path(key)
                .filter(|(_, sub)| !sub.is_empty())
                .ok_or_else(|| anyhow!("Invalid registry tree key: {key}"))?;
            let root = RegKey::predef(hive);
            // Delete existing key tree first to avoid leftover subkeys/values
            if let Some((parent_sub, leaf)) = split_parent_leaf(&sub) {
                if let Some(parent_k) = missing_is_none(
                    root.open_subkey_with_flags(&parent_sub, KEY_ALL_ACCESS | *sam),
                )? {
                    missing_is_none(parent_k.delete_subkey_all(&leaf))?;
                }
            } else {
                if let Some(view) =
                    missing_is_none(root.open_subkey_with_flags("", KEY_ALL_ACCESS | *sam))?
                {
                    missing_is_none(view.delete_subkey_all(&sub))?;
                }
            }
            if let Some(tree) = previous {
                let (k, _) = root.create_subkey_with_flags(&sub, KEY_ALL_ACCESS | *sam)?;
                write_tree_recursive(&k, tree, *sam)?;
            }
        }
        SnapshotRecord::MissingKey { key, sam } => {
            let (hive, sub) = parse_reg_path(key)
                .filter(|(_, sub)| !sub.is_empty())
                .ok_or_else(|| anyhow!("Invalid registry key: {key}"))?;
            let root = RegKey::predef(hive);
            if let Some(open) = missing_is_none(root.open_subkey_with_flags(&sub, KEY_READ | *sam))?
            {
                let info = open.query_info()?;
                if info.values == 0 && info.sub_keys == 0 {
                    missing_is_none(root.delete_subkey_with_flags(&sub, *sam))?;
                }
            }
        }
        SnapshotRecord::ShellRefresh(_) => {}
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
        RegType::REG_RESOURCE_LIST => "REG_RESOURCE_LIST",
        RegType::REG_FULL_RESOURCE_DESCRIPTOR => "REG_FULL_RESOURCE_DESCRIPTOR",
        RegType::REG_RESOURCE_REQUIREMENTS_LIST => "REG_RESOURCE_REQUIREMENTS_LIST",
        RegType::REG_QWORD => "REG_QWORD",
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
        "REG_RESOURCE_LIST" => RegType::REG_RESOURCE_LIST,
        "REG_FULL_RESOURCE_DESCRIPTOR" => RegType::REG_FULL_RESOURCE_DESCRIPTOR,
        "REG_RESOURCE_REQUIREMENTS_LIST" => RegType::REG_RESOURCE_REQUIREMENTS_LIST,
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

/// Returns true if any snapshot record targets machine-wide or privileged registry hives (`HKLM`/`HKCR`/`HKU`).
pub fn snapshot_requires_admin(snap: &RegistrySnapshot) -> bool {
    snap.records.iter().any(|rec| {
        let key = match rec {
            SnapshotRecord::ValueState { key, .. } => key,
            SnapshotRecord::KeyTreeState { key, .. } | SnapshotRecord::MissingKey { key, .. } => {
                key
            }
            SnapshotRecord::ShellRefresh(_) => return false,
        };
        matches!(
            parse_reg_path(key),
            Some((h, _)) if h == HKEY_LOCAL_MACHINE || h == HKEY_CLASSES_ROOT || h == HKEY_USERS
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_capture_distinguishes_missing_from_unreadable_registry_state() {
        let path = format!(r"HKCU\Software\DismNext_Missing_{}", unique_suffix());
        assert!(matches!(
            capture_value_record(&path, "Missing", 0).unwrap(),
            SnapshotRecord::ValueState { previous: None, .. }
        ));
        assert!(read_saved_tree(&path, 0).unwrap().is_none());
        assert!(capture_value_record(r"InvalidHive\Software", "Value", 0).is_err());
        assert!(read_saved_tree("HKCU", 0).is_err());
        assert_eq!(
            missing_is_none::<()>(Err(io::ErrorKind::PermissionDenied.into()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn registry_tree_snapshot_round_trips_nested_keys() {
        let name = format!(r"Software\DismNext_Tree_{}", unique_suffix());
        let path = format!(r"HKCU\{name}");
        let root = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let (key, _) = root.create_subkey(&name).unwrap();
        let (nested, _) = key.create_subkey("nested").unwrap();
        nested.set_value("Value", &42u32).unwrap();
        drop(nested);
        drop(key);

        let record = capture_tree_record(&path, 0).unwrap();
        root.delete_subkey_all(&name).unwrap();
        restore_record(&record).unwrap();
        let nested = root.open_subkey(format!(r"{name}\nested")).unwrap();
        let restored: u32 = nested.get_value("Value").unwrap();
        drop(nested);
        root.delete_subkey_all(&name).unwrap();
        assert_eq!(restored, 42);
    }

    #[test]
    fn failed_restore_does_not_mark_snapshot_restored() {
        let dir = std::env::temp_dir().join(format!("dismnext_snapshot_{}", unique_suffix()));
        std::fs::create_dir(&dir).unwrap();
        let manager = SnapshotManager::new(dir.clone());
        let snap = RegistrySnapshot {
            id: "invalid-key".into(),
            created_at: Local::now().to_rfc3339(),
            item_id: "test".into(),
            item_name: LocalizedText {
                zh: "测试".into(),
                en: "Test".into(),
            },
            scope: "current".into(),
            action_summary: "test".into(),
            restored: false,
            records: vec![SnapshotRecord::ValueState {
                key: r"InvalidHive\Software".into(),
                value_name: "Value".into(),
                sam: 0,
                previous: None,
            }],
        };
        write_snapshot(&manager.snapshot_path(&snap.id), &snap).unwrap();
        let restore = manager.restore(&snap.id);
        let saved = manager.load(&snap.id).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(restore.is_err());
        assert!(!saved.restored);
    }

    #[test]
    fn restoring_a_legacy_snapshot_preserves_it_and_prefers_the_updated_copy() {
        let dir = std::env::temp_dir().join(format!("dismnext_snapshot_{}", unique_suffix()));
        let mut manager = SnapshotManager::new(dir.join("current"));
        let legacy = dir.join("legacy");
        std::fs::create_dir_all(&legacy).unwrap();
        manager.add_legacy_directory(legacy.clone());
        let snapshot = RegistrySnapshot {
            id: "legacy".into(),
            created_at: Local::now().to_rfc3339(),
            item_id: "test".into(),
            item_name: LocalizedText {
                en: "Test".into(),
                zh: "测试".into(),
            },
            scope: "current".into(),
            action_summary: "test".into(),
            restored: false,
            records: Vec::new(),
        };
        write_snapshot(&legacy.join("legacy.json"), &snapshot).unwrap();
        assert!(!manager.list().unwrap()[0].restored);
        assert!(manager.restore(&snapshot.id).unwrap().restored);
        let snapshots = manager.list().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0].restored);
        assert!(
            !SnapshotManager::new(legacy)
                .load(&snapshot.id)
                .unwrap()
                .restored
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_snapshot_requires_admin_detection() {
        let make_snap = |key: &str| RegistrySnapshot {
            id: "t1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            item_id: "opt-1".into(),
            item_name: LocalizedText {
                zh: "测试".into(),
                en: "Test".into(),
            },
            scope: "current".into(),
            action_summary: "Enable".into(),
            restored: false,
            records: vec![SnapshotRecord::ValueState {
                key: key.into(),
                value_name: "Val".into(),
                sam: 0,
                previous: None,
            }],
        };

        assert!(!snapshot_requires_admin(&make_snap(
            r"HKEY_CURRENT_USER\Software\Test"
        )));
        assert!(snapshot_requires_admin(&make_snap(
            r"HKEY_LOCAL_MACHINE\SOFTWARE\Test"
        )));
        assert!(snapshot_requires_admin(&make_snap(
            r"HKEY_USERS\DEFAULT\Control Panel"
        )));
    }
}
