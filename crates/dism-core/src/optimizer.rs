use crate::catalog::{
    Action, LocalizedText, OptimizationGroup, OptimizationItem, OptimizationScope,
};
use crate::evaluator::{
    eval_condition, expand_env_vars, parse_hex_bytes, parse_hex_u32, parse_hex_u64, parse_reg_path,
    reg_value_to_u32, reg_value_to_u64, wow64_sam_list,
};
use crate::snapshot::{
    read_tree_recursive, split_parent_leaf, write_tree_recursive, RegistrySnapshot, SnapshotManager,
};
use crate::sysinfo::SystemInfo;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG,
};
use winreg::enums::{RegType, KEY_ALL_ACCESS, KEY_READ};
use winreg::{RegKey, RegValue};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScopeName {
    Current,
    Default,
    System,
}

impl ScopeName {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScopeName::Current => "current",
            ScopeName::Default => "default",
            ScopeName::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApplyValue {
    Bool(bool),
    Index(usize),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeCheckBoxStatus {
    pub enabled: bool,
    pub true_warning: Option<LocalizedText>,
    pub false_warning: Option<LocalizedText>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropdownOptionStatus {
    pub index: usize,
    pub name: LocalizedText,
    pub warning: Option<LocalizedText>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeComboStatus {
    pub selected_index: Option<usize>,
    pub options: Vec<DropdownOptionStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ScopeStatus {
    #[serde(rename = "checkbox")]
    CheckBox(ScopeCheckBoxStatus),
    #[serde(rename = "combo")]
    Combo(ScopeComboStatus),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationItemStatus {
    pub id: String,
    pub name: LocalizedText,
    pub item_type: String,
    pub source: String,
    pub current: Option<ScopeStatus>,
    pub default: Option<ScopeStatus>,
    pub system: Option<ScopeStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationGroupStatus {
    pub id: String,
    pub name: LocalizedText,
    pub items: Vec<OptimizationItemStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyOutcome {
    pub item_id: String,
    pub scope: ScopeName,
    pub snapshot: Option<RegistrySnapshot>,
    pub restart_hint: Option<String>,
    pub warnings: Vec<String>,
}

pub fn evaluate_all_groups(
    groups: &[OptimizationGroup],
    sys: &SystemInfo,
) -> Vec<OptimizationGroupStatus> {
    let mut out = Vec::new();
    for g in groups {
        if !eval_condition(&g.applicable, sys) {
            continue;
        }
        let mut items = Vec::new();
        for it in &g.items {
            if !eval_condition(&it.applicable, sys) {
                continue;
            }
            items.push(OptimizationItemStatus {
                id: it.id.clone(),
                name: it.name.clone(),
                item_type: it.item_type.clone(),
                source: it.source.clone(),
                current: it.current.as_ref().map(|s| eval_scope(s, sys)),
                default: it.default.as_ref().map(|s| eval_scope(s, sys)),
                system: it.system.as_ref().map(|s| eval_scope(s, sys)),
            });
        }
        if !items.is_empty() {
            out.push(OptimizationGroupStatus {
                id: g.id.clone(),
                name: g.name.clone(),
                items,
            });
        }
    }
    out
}

fn eval_scope(scope: &OptimizationScope, sys: &SystemInfo) -> ScopeStatus {
    match scope {
        OptimizationScope::CheckBox {
            state,
            true_warning,
            false_warning,
            ..
        } => ScopeStatus::CheckBox(ScopeCheckBoxStatus {
            enabled: eval_condition(state, sys),
            true_warning: true_warning.clone(),
            false_warning: false_warning.clone(),
        }),
        OptimizationScope::Combo { dropdowns } => {
            let mut selected_index = None;
            let mut options = Vec::with_capacity(dropdowns.len());
            for (idx, dd) in dropdowns.iter().enumerate() {
                if selected_index.is_none() && eval_condition(&dd.applicable, sys) {
                    selected_index = Some(idx);
                }
                options.push(DropdownOptionStatus {
                    index: idx,
                    name: dd.name.clone(),
                    warning: dd.warning.clone(),
                });
            }
            ScopeStatus::Combo(ScopeComboStatus {
                selected_index,
                options,
            })
        }
    }
}

/// Applies a single optimization toggle or dropdown selection, automatically capturing a registry snapshot beforehand.
pub fn apply_optimization(
    groups: &[OptimizationGroup],
    item_id: &str,
    scope: ScopeName,
    value: ApplyValue,
    snapshot_mgr: &SnapshotManager,
) -> Result<ApplyOutcome> {
    let item = find_item(groups, item_id)
        .ok_or_else(|| anyhow!("Optimization item '{}' not found", item_id))?;

    let scope_def = match scope {
        ScopeName::Current => item.current.as_ref(),
        ScopeName::Default => item.default.as_ref(),
        ScopeName::System => item.system.as_ref(),
    }
    .ok_or_else(|| anyhow!("Item '{}' does not support scope {:?}", item_id, scope))?;

    let (actions, restart_hint, action_summary) = match (scope_def, &value) {
        (
            OptimizationScope::CheckBox {
                true_actions,
                true_restart,
                false_actions,
                false_restart,
                ..
            },
            ApplyValue::Bool(enable),
        ) => {
            if *enable {
                (true_actions.as_slice(), true_restart.clone(), "Enable".to_string())
            } else {
                (false_actions.as_slice(), false_restart.clone(), "Disable".to_string())
            }
        }
        (OptimizationScope::Combo { dropdowns }, ApplyValue::Index(idx)) => {
            let dd = dropdowns
                .get(*idx)
                .ok_or_else(|| anyhow!("Dropdown index {} out of bounds", idx))?;
            (
                dd.actions.as_slice(),
                dd.restart.clone(),
                format!("Select: {}", dd.name.zh),
            )
        }
        _ => return Err(anyhow!("Mismatched ApplyValue type for item '{}'", item_id)),
    };

    let snapshot = snapshot_mgr.capture_before_actions(
        item_id,
        &item.name,
        scope.as_str(),
        &action_summary,
        actions,
    )?;

    let mut warnings = Vec::new();
    for action in actions {
        if let Err(e) = execute_action(action) {
            warnings.push(e.to_string());
        }
    }

    Ok(ApplyOutcome {
        item_id: item_id.to_string(),
        scope,
        snapshot,
        restart_hint,
        warnings,
    })
}

fn find_item<'a>(groups: &'a [OptimizationGroup], id: &str) -> Option<&'a OptimizationItem> {
    groups.iter().flat_map(|g| g.items.iter()).find(|i| i.id == id)
}

pub fn execute_action(action: &Action) -> Result<()> {
    match action {
        Action::RegWrite {
            key,
            value,
            reg_type,
            data,
            operator,
            wow64,
            fun,
        } => exec_reg_write(
            key,
            value.as_deref(),
            reg_type.as_deref(),
            data.as_deref(),
            operator,
            wow64.as_deref(),
            *fun,
        ),
        Action::RegDelete {
            key,
            value,
            skip_error,
            wow64,
            ..
        } => exec_reg_delete(key, value.as_deref(), *skip_error, wow64.as_deref()),
        Action::RegMove {
            key,
            new_key,
            value,
            new_value,
            skip_error,
            wow64,
        } => exec_reg_move(
            key,
            new_key,
            value.as_deref(),
            new_value.as_deref(),
            *skip_error,
            wow64.as_deref(),
        ),
        Action::SetServiceStart { name, start_type } => {
            let svc_key = format!(r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\{}", name);
            let hex = format!("{:X}", start_type);
            exec_reg_write(
                &svc_key,
                Some("Start"),
                Some("REG_DWORD"),
                Some(&hex),
                "=",
                None,
                false,
            )
        }
        Action::ExplorerNotify {
            notify_type,
            msg,
            w_param,
            l_param,
            cmd,
            ..
        } => exec_explorer_notify(
            notify_type,
            msg.as_deref(),
            w_param.as_deref(),
            l_param.as_deref(),
            cmd.as_deref(),
        ),
        Action::FileCreateByZIP { path, .. } => exec_create_blank_ico(path),
        Action::General { .. } | Action::Custom { .. } => Ok(()),
    }
}

fn exec_reg_write(
    key: &str,
    value: Option<&str>,
    reg_type: Option<&str>,
    data: Option<&str>,
    operator: &str,
    wow64: Option<&str>,
    fun: bool,
) -> Result<()> {
    if fun {
        return Err(anyhow!(
            "Interactive function expression '{}' is not supported in unattended apply",
            data.unwrap_or("")
        ));
    }
    let (hive, sub) = parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {}", key))?;
    let root = RegKey::predef(hive);

    for sam in wow64_sam_list(wow64) {
        let (k, _) = root
            .create_subkey_with_flags(&sub, KEY_ALL_ACCESS | sam)
            .with_context(|| format!("Failed to open/create registry key {}", key))?;

        let Some(vname) = value else {
            // Key-only creation
            continue;
        };
        let raw_data = data.unwrap_or("");
        let rtype = reg_type.unwrap_or("REG_SZ").trim().to_ascii_uppercase();

        match rtype.as_str() {
            "REG_DWORD" => {
                let operand = parse_hex_u32(raw_data)
                    .ok_or_else(|| anyhow!("Invalid hex REG_DWORD data '{}'", raw_data))?;
                let existing = k
                    .get_raw_value(vname)
                    .ok()
                    .and_then(|rv| reg_value_to_u32(&rv))
                    .unwrap_or(0);
                let new_val = match operator.trim() {
                    "|" => existing | operand,
                    "&" => existing & operand,
                    "^" => existing ^ operand,
                    _ => operand,
                };
                let rv = RegValue {
                    vtype: RegType::REG_DWORD,
                    bytes: new_val.to_le_bytes().to_vec(),
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_QWORD" => {
                let operand = parse_hex_u64(raw_data)
                    .ok_or_else(|| anyhow!("Invalid hex REG_QWORD data '{}'", raw_data))?;
                let existing = k
                    .get_raw_value(vname)
                    .ok()
                    .and_then(|rv| reg_value_to_u64(&rv))
                    .unwrap_or(0);
                let new_val = match operator.trim() {
                    "|" => existing | operand,
                    "&" => existing & operand,
                    "^" => existing ^ operand,
                    _ => operand,
                };
                let rv = RegValue {
                    vtype: RegType::REG_QWORD,
                    bytes: new_val.to_le_bytes().to_vec(),
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_BINARY" => {
                let bytes = parse_hex_bytes(raw_data)
                    .ok_or_else(|| anyhow!("Invalid REG_BINARY data '{}'", raw_data))?;
                let rv = RegValue {
                    vtype: RegType::REG_BINARY,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_EXPAND_SZ" => {
                let bytes = encode_utf16_sz(raw_data);
                let rv = RegValue {
                    vtype: RegType::REG_EXPAND_SZ,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
            _ => {
                let expanded = expand_env_vars(raw_data, None);
                let bytes = encode_utf16_sz(&expanded);
                let rv = RegValue {
                    vtype: RegType::REG_SZ,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
        }
    }
    Ok(())
}

fn encode_utf16_sz(s: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((s.len() + 1) * 2);
    for w in OsStr::new(s).encode_wide().chain(Some(0)) {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn exec_reg_delete(
    key: &str,
    value: Option<&str>,
    skip_error: Option<i32>,
    wow64: Option<&str>,
) -> Result<()> {
    let (hive, sub) = parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {}", key))?;
    let root = RegKey::predef(hive);

    for sam in wow64_sam_list(wow64) {
        match value {
            Some(vname) => match root.open_subkey_with_flags(&sub, KEY_ALL_ACCESS | sam) {
                Ok(k) => {
                    if let Err(e) = k.delete_value(vname) {
                        if e.kind() != std::io::ErrorKind::NotFound && skip_error != Some(2) {
                            return Err(e.into());
                        }
                    }
                }
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound && skip_error != Some(2) {
                        return Err(e.into());
                    }
                }
            },
            None => {
                if let Some((parent_sub, leaf)) = split_parent_leaf(&sub) {
                    if let Ok(pk) = root.open_subkey_with_flags(&parent_sub, KEY_ALL_ACCESS | sam) {
                        if let Err(e) = pk.delete_subkey_all(&leaf) {
                            if e.kind() != std::io::ErrorKind::NotFound && skip_error != Some(2) {
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn exec_reg_move(
    key: &str,
    new_key: &str,
    value: Option<&str>,
    new_value: Option<&str>,
    skip_error: Option<i32>,
    wow64: Option<&str>,
) -> Result<()> {
    let (src_hive, src_sub) =
        parse_reg_path(key).ok_or_else(|| anyhow!("Invalid source key: {}", key))?;
    let (dst_hive, dst_sub) =
        parse_reg_path(new_key).ok_or_else(|| anyhow!("Invalid dest key: {}", new_key))?;

    for sam in wow64_sam_list(wow64) {
        let src_root = RegKey::predef(src_hive);
        let dst_root = RegKey::predef(dst_hive);
        match value {
            Some(vname) => {
                let dst_vname = new_value.unwrap_or(vname);
                match src_root.open_subkey_with_flags(&src_sub, KEY_ALL_ACCESS | sam) {
                    Ok(sk) => match sk.get_raw_value(vname) {
                        Ok(rv) => {
                            let (dk, _) = dst_root.create_subkey_with_flags(&dst_sub, KEY_ALL_ACCESS | sam)?;
                            dk.set_raw_value(dst_vname, &rv)?;
                            let _ = sk.delete_value(vname);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound || skip_error == Some(2) => {}
                        Err(e) => return Err(e.into()),
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound || skip_error == Some(2) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            None => {
                match src_root.open_subkey_with_flags(&src_sub, KEY_READ | sam) {
                    Ok(sk) => {
                        let tree = read_tree_recursive(&sk);
                        drop(sk);
                        let (dk, _) = dst_root.create_subkey_with_flags(&dst_sub, KEY_ALL_ACCESS | sam)?;
                        write_tree_recursive(&dk, &tree)?;
                        if let Some((parent_sub, leaf)) = split_parent_leaf(&src_sub) {
                            if let Ok(pk) = src_root.open_subkey_with_flags(&parent_sub, KEY_ALL_ACCESS | sam) {
                                let _ = pk.delete_subkey_all(&leaf);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound || skip_error == Some(2) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    Ok(())
}

fn exec_explorer_notify(
    notify_type: &str,
    msg: Option<&str>,
    w_param: Option<&str>,
    l_param: Option<&str>,
    cmd: Option<&str>,
) -> Result<()> {
    match notify_type.trim() {
        "AssocChanged" => {
            unsafe {
                SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null());
            }
            Ok(())
        }
        "Custom" => {
            let msg_num = msg
                .and_then(parse_hex_u32)
                .ok_or_else(|| anyhow!("Invalid ExplorerNotify msg"))?;
            let wp = w_param
                .and_then(|s| s.strip_prefix('#').and_then(parse_hex_u32))
                .unwrap_or(0) as usize;
            let lp_wide: Option<Vec<u16>> = l_param.and_then(|s| {
                if s.starts_with('#') || s.eq_ignore_ascii_case("NULL") {
                    None
                } else {
                    Some(OsStr::new(s).encode_wide().chain(Some(0)).collect())
                }
            });
            let lp_val: isize = match (&lp_wide, l_param) {
                (Some(buf), _) => buf.as_ptr() as isize,
                (None, Some(s)) if s.starts_with('#') => parse_hex_u32(s).unwrap_or(0) as isize,
                _ => 0,
            };
            let mut result: usize = 0;
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    msg_num,
                    wp,
                    lp_val,
                    SMTO_ABORTIFHUNG,
                    300,
                    &mut result,
                );
            }
            Ok(())
        }
        "Cmd" => {
            if let Some(c) = cmd {
                let expanded = expand_env_vars(c, None);
                let _ = std::process::Command::new("cmd")
                    .args(["/C", &expanded])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Generates a minimal valid 16x16 transparent ICO file at target path (replaces Blank.ico extraction).
fn exec_create_blank_ico(path_tmpl: &str) -> Result<()> {
    let expanded = expand_env_vars(path_tmpl, None);
    let target = Path::new(&expanded);
    if target.exists() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Minimal 16x16 32-bpp transparent ICO (ICONDIR + ICONDIRENTRY + BITMAPINFOHEADER + XOR/AND masks)
    let mut ico = Vec::with_capacity(22 + 40 + 16 * 16 * 4 + 16 * 4);
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // ICONDIR: 1 icon
    ico.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0]); // 16x16, 32bpp
    let img_size: u32 = 40 + 16 * 16 * 4 + 16 * 4;
    ico.extend_from_slice(&img_size.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // offset
    // BITMAPINFOHEADER
    ico.extend_from_slice(&40u32.to_le_bytes());
    ico.extend_from_slice(&16i32.to_le_bytes());
    ico.extend_from_slice(&32i32.to_le_bytes()); // 2x height
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&[0u8; 24]);
    ico.extend_from_slice(&vec![0u8; 16 * 16 * 4]); // transparent RGBA
    ico.extend_from_slice(&vec![0xFFu8; 16 * 4]); // AND mask = all transparent
    std::fs::write(target, ico)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Condition;
    use winreg::enums::HKEY_CURRENT_USER;

    #[test]
    fn test_optimizer_and_snapshot_rollback_in_isolated_key() {
        let test_key = r"HKEY_CURRENT_USER\Software\DismNext_UnitTest";
        let sub_key = r"Software\DismNext_UnitTest";
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let _ = hkcu.delete_subkey_all(sub_key);

        // Seed initial state value=1 and bitmask=0x10
        {
            let (k, _) = hkcu.create_subkey(sub_key).unwrap();
            k.set_value("FlagVal", &1u32).unwrap();
            k.set_value("MaskVal", &0x10u32).unwrap();
        }

        let snap_dir = std::env::temp_dir().join(format!("dismnext_snap_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&snap_dir);
        let snap_mgr = SnapshotManager::new(snap_dir.clone());

        let item = OptimizationItem {
            id: "opt-test".to_string(),
            name: LocalizedText {
                zh: "沙箱测试项".into(),
                en: "Sandbox Test Item".into(),
            },
            item_type: "CheckBox".to_string(),
            source: "test".to_string(),
            applicable: Condition::True,
            current: Some(OptimizationScope::CheckBox {
                state: Condition::And {
                    children: vec![
                        Condition::RegExist {
                            key: test_key.to_string(),
                            value: Some("FlagVal".to_string()),
                            reg_type: Some("REG_DWORD".to_string()),
                            data: Some("2".to_string()),
                            compare: None,
                            wow64: None,
                        },
                        Condition::RegExist {
                            key: test_key.to_string(),
                            value: Some("MaskVal".to_string()),
                            reg_type: Some("REG_DWORD".to_string()),
                            data: Some("100000".to_string()),
                            compare: Some("&".to_string()),
                            wow64: None,
                        },
                    ],
                },
                true_actions: vec![
                    Action::RegWrite {
                        key: test_key.to_string(),
                        value: Some("FlagVal".to_string()),
                        reg_type: Some("REG_DWORD".to_string()),
                        data: Some("2".to_string()),
                        operator: "=".to_string(),
                        wow64: None,
                        fun: false,
                    },
                    Action::RegWrite {
                        key: test_key.to_string(),
                        value: Some("MaskVal".to_string()),
                        reg_type: Some("REG_DWORD".to_string()),
                        data: Some("100000".to_string()),
                        operator: "|".to_string(),
                        wow64: None,
                        fun: false,
                    },
                    Action::RegWrite {
                        key: test_key.to_string(),
                        value: Some("NewVal".to_string()),
                        reg_type: Some("REG_SZ".to_string()),
                        data: Some("hello".to_string()),
                        operator: "=".to_string(),
                        wow64: None,
                        fun: false,
                    },
                ],
                true_warning: None,
                true_restart: None,
                false_actions: Vec::new(),
                false_warning: None,
                false_restart: None,
            }),
            default: None,
            system: None,
        };

        let groups = vec![OptimizationGroup {
            id: "grp-test".to_string(),
            name: LocalizedText {
                zh: "测试".into(),
                en: "Test".into(),
            },
            applicable: Condition::True,
            items: vec![item],
        }];

        let sys = SystemInfo::collect();
        let before = evaluate_all_groups(&groups, &sys);
        match &before[0].items[0].current {
            Some(ScopeStatus::CheckBox(cb)) => assert!(!cb.enabled),
            _ => panic!("Expected checkbox"),
        }

        // Apply enable -> creates snapshot then mutates registry
        let outcome = apply_optimization(
            &groups,
            "opt-test",
            ScopeName::Current,
            ApplyValue::Bool(true),
            &snap_mgr,
        )
        .unwrap();
        let snap = outcome.snapshot.expect("Snapshot should be created");

        let after = evaluate_all_groups(&groups, &sys);
        match &after[0].items[0].current {
            Some(ScopeStatus::CheckBox(cb)) => assert!(cb.enabled),
            _ => panic!("Expected checkbox"),
        }

        // Restore snapshot -> values revert to 1, 0x10, and NewVal is removed
        snap_mgr.restore(&snap.id).unwrap();
        let k = hkcu.open_subkey(sub_key).unwrap();
        assert_eq!(k.get_value::<u32, _>("FlagVal").unwrap(), 1);
        assert_eq!(k.get_value::<u32, _>("MaskVal").unwrap(), 0x10);
        assert!(k.get_raw_value("NewVal").is_err());

        // Cleanup sandbox key & temp dir
        drop(k);
        let _ = hkcu.delete_subkey_all(sub_key);
        let _ = std::fs::remove_dir_all(&snap_dir);
    }
}
