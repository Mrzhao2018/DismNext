use crate::catalog::{
    Action, LocalizedText, OptimizationGroup, OptimizationItem, OptimizationScope,
};
use crate::evaluator::eval_condition;
use crate::snapshot::{RegistrySnapshot, ShellRefresh, SnapshotManager};
use crate::sysinfo::SystemInfo;
use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod actions;
use actions::{
    exec_create_blank_ico, exec_explorer_notify, exec_reg_delete, exec_reg_move, exec_reg_write,
};
pub(crate) use actions::{is_start_menu_refresh, restore_shell_refresh};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationProfileEntry {
    pub item_id: String,
    pub scope: ScopeName,
    pub value: ApplyValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationPreset {
    pub id: String,
    pub name: LocalizedText,
    pub description: LocalizedText,
    pub entries: Vec<OptimizationProfileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationProfile {
    pub name: String,
    pub created_at: String,
    pub entries: Vec<OptimizationProfileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchApplyOutcome {
    pub applied_count: usize,
    pub snapshots_created: usize,
    pub warnings: Vec<String>,
}

/// Returns built-in curated optimization preset packs for one-click safe batch tuning.
pub fn get_optimization_presets() -> Vec<OptimizationPreset> {
    let bool_entries = |ids: &[&str], enabled: bool| -> Vec<OptimizationProfileEntry> {
        ids.iter()
            .map(|id| OptimizationProfileEntry {
                item_id: (*id).to_string(),
                scope: ScopeName::Current,
                value: ApplyValue::Bool(enabled),
            })
            .collect()
    };

    let privacy_ids = &[
        "opt-023", "opt-028", "opt-108", "opt-164", "opt-165", "opt-166", "opt-169", "opt-170",
        "opt-174", "opt-175",
    ];
    let dev_ids = &[
        "opt-011", "opt-036", "opt-037", "opt-038", "opt-059", "opt-168", "opt-172", "opt-173",
    ];
    let mut all_ids = Vec::with_capacity(privacy_ids.len() + dev_ids.len());
    all_ids.extend_from_slice(privacy_ids);
    all_ids.extend_from_slice(dev_ids);

    vec![
        OptimizationPreset {
            id: "privacy_clean".to_string(),
            name: LocalizedText {
                zh: "纯净隐私与去广告方案".to_string(),
                en: "Clean Privacy & No-Ads Pack".to_string(),
            },
            description: LocalizedText {
                zh: "关闭开始菜单推荐、锁屏提示、Bing 搜索联想、OneDrive 横幅、Recall 与 Copilot 广告入口。".to_string(),
                en: "Disables Start suggestions, Bing search suggestions, OneDrive banners, Recall, and Copilot buttons.".to_string(),
            },
            entries: bool_entries(privacy_ids, true),
        },
        OptimizationPreset {
            id: "dev_power".to_string(),
            name: LocalizedText {
                zh: "开发者与极客效率方案".to_string(),
                en: "Developer & Power User Pack".to_string(),
            },
            description: LocalizedText {
                zh: "显示文件扩展名/隐藏文件、展开经典右键菜单、启用任务栏右键结束任务、关闭文件夹自动猜测并默认打开此电脑。".to_string(),
                en: "Shows file extensions & hidden items, expands classic context menu, enables taskbar End Task, and opens This PC.".to_string(),
            },
            entries: bool_entries(dev_ids, true),
        },
        OptimizationPreset {
            id: "windows_default".to_string(),
            name: LocalizedText {
                zh: "恢复上述预设项为系统默认".to_string(),
                en: "Reset Preset Tweaks to Default".to_string(),
            },
            description: LocalizedText {
                zh: "将隐私方案与效率方案中的所有开关统一还原为默认关闭状态，并自动生成快照。".to_string(),
                en: "Resets all items included in the Privacy and Developer packs back to default off state.".to_string(),
            },
            entries: bool_entries(&all_ids, false),
        },
    ]
}

/// Applies a batch with one pre-change snapshot and rolls back registry changes on failure.
pub fn apply_optimization_batch(
    groups: &[OptimizationGroup],
    entries: &[OptimizationProfileEntry],
    snapshot_mgr: &SnapshotManager,
) -> Result<BatchApplyOutcome> {
    let mut actions = Vec::new();
    for entry in entries {
        let (_, item_actions, _, _) =
            resolve_apply(groups, &entry.item_id, entry.scope, &entry.value)
                .with_context(|| format!("Invalid batch entry {}", entry.item_id))?;
        anyhow::ensure!(
            item_actions.iter().all(is_batch_action),
            "Batch entry {} includes an action that cannot be rolled back",
            entry.item_id
        );
        actions.extend_from_slice(item_actions);
    }

    let _mutation = snapshot_mgr.lock_mutation()?;
    let snapshot = snapshot_mgr.capture_before_actions(
        "batch",
        &LocalizedText {
            zh: "批量系统优化".into(),
            en: "Optimization batch".into(),
        },
        "batch",
        &format!("{} optimization settings", entries.len()),
        &actions,
    )?;
    let (snapshot, warnings) = apply_captured_actions(&actions, snapshot_mgr, snapshot)?;
    Ok(BatchApplyOutcome {
        applied_count: entries.len(),
        snapshots_created: usize::from(snapshot.is_some()),
        warnings,
    })
}

fn is_batch_action(action: &Action) -> bool {
    matches!(
        action,
        Action::RegWrite { fun: false, .. }
            | Action::RegMove { .. }
            | Action::RegDelete { .. }
            | Action::SetServiceStart { .. }
    ) || ShellRefresh::from_action(action).is_some()
}

/// Exports the active non-default optimization settings into a portable JSON profile.
pub fn export_optimization_profile(
    groups: &[OptimizationGroup],
    sys: &SystemInfo,
) -> OptimizationProfile {
    let evaluated = evaluate_all_groups(groups, sys);
    let mut entries = Vec::new();

    for grp in evaluated {
        for it in grp.items {
            for (scope, st_opt) in [
                (ScopeName::Current, &it.current),
                (ScopeName::System, &it.system),
            ] {
                let Some(st) = st_opt else {
                    continue;
                };
                match st {
                    ScopeStatus::CheckBox(cb) if cb.enabled => {
                        entries.push(OptimizationProfileEntry {
                            item_id: it.id.clone(),
                            scope,
                            value: ApplyValue::Bool(true),
                        });
                    }
                    ScopeStatus::Combo(c) => {
                        if let Some(idx) = c.selected_index {
                            entries.push(OptimizationProfileEntry {
                                item_id: it.id.clone(),
                                scope,
                                value: ApplyValue::Index(idx),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    entries.retain(|entry| {
        resolve_apply(groups, &entry.item_id, entry.scope, &entry.value)
            .is_ok_and(|(_, actions, _, _)| actions.iter().all(is_batch_action))
    });
    OptimizationProfile {
        name: "DismNext Custom Optimization Profile".to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        entries,
    }
}

pub fn evaluate_all_groups(
    groups: &[OptimizationGroup],
    sys: &SystemInfo,
) -> Vec<OptimizationGroupStatus> {
    groups
        .par_iter()
        .filter(|g| eval_condition(&g.applicable, sys))
        .filter_map(|g| {
            let items: Vec<OptimizationItemStatus> = g
                .items
                .par_iter()
                .filter(|it| !item_requires_interaction(it) && eval_condition(&it.applicable, sys))
                .map(|it| OptimizationItemStatus {
                    id: it.id.clone(),
                    name: it.name.clone(),
                    item_type: it.item_type.clone(),
                    source: it.source.clone(),
                    current: it.current.as_ref().map(|s| eval_scope(s, sys)),
                    default: it.default.as_ref().map(|s| eval_scope(s, sys)),
                    system: it.system.as_ref().map(|s| eval_scope(s, sys)),
                })
                .collect();
            if items.is_empty() {
                None
            } else {
                Some(OptimizationGroupStatus {
                    id: g.id.clone(),
                    name: g.name.clone(),
                    items,
                })
            }
        })
        .collect()
}

fn eval_scope(scope: &OptimizationScope, sys: &SystemInfo) -> ScopeStatus {
    match scope {
        OptimizationScope::CheckBox(scope) => ScopeStatus::CheckBox(ScopeCheckBoxStatus {
            enabled: eval_condition(&scope.state, sys),
            true_warning: scope.true_warning.clone(),
            false_warning: scope.false_warning.clone(),
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
    let (item, actions, restart_hint, action_summary) =
        resolve_apply(groups, item_id, scope, &value)?;
    let _mutation = snapshot_mgr.lock_mutation()?;
    let snapshot = snapshot_mgr.capture_before_actions(
        item_id,
        &item.name,
        scope.as_str(),
        &action_summary,
        actions,
    )?;
    let (snapshot, warnings) = apply_captured_actions(actions, snapshot_mgr, snapshot)?;
    Ok(ApplyOutcome {
        item_id: item_id.to_string(),
        scope,
        snapshot,
        restart_hint,
        warnings,
    })
}

fn interactive_action(action: &Action) -> bool {
    matches!(action, Action::RegWrite { fun: true, .. })
}

fn item_requires_interaction(item: &OptimizationItem) -> bool {
    [
        item.current.as_ref(),
        item.default.as_ref(),
        item.system.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|scope| match scope {
        OptimizationScope::CheckBox(scope) => scope
            .true_actions
            .iter()
            .chain(&scope.false_actions)
            .any(interactive_action),
        OptimizationScope::Combo { dropdowns } => dropdowns
            .iter()
            .flat_map(|option| &option.actions)
            .any(interactive_action),
    })
}

type ResolvedApply<'a> = (&'a OptimizationItem, &'a [Action], Option<String>, String);

fn resolve_apply<'a>(
    groups: &'a [OptimizationGroup],
    item_id: &str,
    scope: ScopeName,
    value: &ApplyValue,
) -> Result<ResolvedApply<'a>> {
    let item = find_item(groups, item_id)
        .ok_or_else(|| anyhow!("Optimization item '{}' not found", item_id))?;

    let scope_def = match scope {
        ScopeName::Current => item.current.as_ref(),
        ScopeName::Default => item.default.as_ref(),
        ScopeName::System => item.system.as_ref(),
    }
    .ok_or_else(|| anyhow!("Item '{}' does not support scope {:?}", item_id, scope))?;

    let (actions, restart_hint, action_summary) = match (scope_def, value) {
        (OptimizationScope::CheckBox(scope), ApplyValue::Bool(enable)) => {
            if *enable {
                (
                    scope.true_actions.as_slice(),
                    scope.true_restart.clone(),
                    "Enable".to_string(),
                )
            } else {
                (
                    scope.false_actions.as_slice(),
                    scope.false_restart.clone(),
                    "Disable".to_string(),
                )
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

    anyhow::ensure!(
        !actions.iter().any(interactive_action),
        "Item '{item_id}' requires an interactive Windows dialog"
    );
    Ok((item, actions, restart_hint, action_summary))
}

fn apply_captured_actions(
    actions: &[Action],
    manager: &SnapshotManager,
    snapshot: Option<RegistrySnapshot>,
) -> Result<(Option<RegistrySnapshot>, Vec<String>)> {
    let is_notification = |action: &Action| matches!(action, Action::ExplorerNotify { .. });
    for action in actions.iter().filter(|action| !is_notification(action)) {
        if let Err(error) = execute_action(action) {
            if let Some(snapshot) = &snapshot {
                let rollback = (|| -> Result<()> {
                    if manager.has_changes(snapshot)? {
                        manager.restore_records(snapshot)?;
                    }
                    manager.discard(snapshot)
                })();
                if let Err(rollback_error) = rollback {
                    return Err(anyhow!(
                        "{error:#}. Rollback incomplete ({rollback_error:#}). Snapshot {} was kept",
                        snapshot.id
                    ));
                }
            }
            return Err(error);
        }
    }

    let snapshot = if let Some(snapshot) = snapshot {
        if manager.has_changes(&snapshot)? {
            Some(snapshot)
        } else {
            manager.discard(&snapshot)?;
            None
        }
    } else {
        None
    };

    let mut warnings = Vec::new();
    let mut sent = Vec::new();
    for action in actions.iter().filter(|action| is_notification(action)) {
        if let Some(notification) = ShellRefresh::from_action(action) {
            if sent.contains(&notification) {
                continue;
            }
            sent.push(notification);
        }
        if let Err(error) = execute_action(action) {
            if snapshot.is_none() {
                return Err(error);
            }
            warnings.push(error.to_string());
        }
    }
    Ok((snapshot, warnings))
}

fn find_item<'a>(groups: &'a [OptimizationGroup], id: &str) -> Option<&'a OptimizationItem> {
    groups
        .iter()
        .flat_map(|g| g.items.iter())
        .find(|i| i.id == id)
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
            let svc_key = format!(
                r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Services\{}",
                name
            );
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
        Action::General { .. } | Action::VersionedDirectory { .. } | Action::Custom { .. } => {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Condition;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

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

        let snap_dir =
            std::env::temp_dir().join(format!("dismnext_snap_test_{}", std::process::id()));
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
            current: Some(OptimizationScope::CheckBox(Box::new(
                crate::catalog::OptimizationCheckBox {
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
                },
            ))),
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

        let presets = get_optimization_presets();
        assert_eq!(presets.len(), 3);
        assert!(presets.iter().all(|p| !p.entries.is_empty()));
    }

    #[test]
    fn batch_creates_one_snapshot_and_rolls_back_failed_writes() {
        let name = format!("DismNext_BatchTest_{}", std::process::id());
        let sub = format!(r"Software\{name}");
        let branch = format!(r"{sub}\Created\Nested");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (root, _) = hkcu.create_subkey(&sub).unwrap();
        root.set_value("Flag", &10u32).unwrap();
        let dir = std::env::temp_dir().join(&name);
        let manager = SnapshotManager::new(dir.clone());
        let label = LocalizedText {
            zh: "测试".into(),
            en: "Test".into(),
        };
        let write = |sub: &str, value: &str, data: &str| Action::RegWrite {
            key: format!(r"HKEY_CURRENT_USER\{sub}"),
            value: Some(value.into()),
            reg_type: Some("REG_DWORD".into()),
            data: Some(data.into()),
            operator: "=".into(),
            wow64: None,
            fun: false,
        };
        let item = |id: &str, on: Action, off: Action| OptimizationItem {
            id: id.into(),
            name: label.clone(),
            item_type: "CheckBox".into(),
            source: "test".into(),
            applicable: Condition::True,
            current: Some(OptimizationScope::CheckBox(Box::new(
                crate::catalog::OptimizationCheckBox {
                    state: Condition::True,
                    true_actions: vec![on],
                    false_actions: vec![off],
                    true_warning: None,
                    false_warning: None,
                    true_restart: None,
                    false_restart: None,
                },
            ))),
            default: None,
            system: None,
        };
        let groups = [OptimizationGroup {
            id: "test-group".into(),
            name: label.clone(),
            applicable: Condition::True,
            items: vec![
                item(
                    "test-flag",
                    write(&sub, "Flag", "20"),
                    write(&sub, "Flag", "0"),
                ),
                item(
                    "test-nested",
                    write(&branch, "New", "2"),
                    write(&branch, "New", "invalid"),
                ),
            ],
        }];
        let entry = |id: &str, enabled| OptimizationProfileEntry {
            item_id: id.into(),
            scope: ScopeName::Current,
            value: ApplyValue::Bool(enabled),
        };
        let read_flag = || root.get_value::<u32, _>("Flag").unwrap();
        let success = [entry("test-flag", true), entry("test-nested", true)];
        let outcome = apply_optimization_batch(&groups, &success, &manager).unwrap();
        assert_eq!(outcome.applied_count, 2);
        assert_eq!(outcome.snapshots_created, 1);
        assert_eq!(read_flag(), 0x20);
        assert_eq!(
            hkcu.open_subkey(&branch)
                .unwrap()
                .get_value::<u32, _>("New")
                .unwrap(),
            2
        );
        let snapshots = manager.list().unwrap();
        assert_eq!(snapshots.len(), 1);
        manager.restore(&snapshots[0].id).unwrap();
        assert_eq!(read_flag(), 10);
        assert!(hkcu.open_subkey(format!(r"{sub}\Created")).is_err());

        let failure = [entry("test-flag", true), entry("test-nested", false)];
        assert!(apply_optimization_batch(&groups, &failure, &manager).is_err());
        assert_eq!(read_flag(), 10);
        assert!(hkcu.open_subkey(format!(r"{sub}\Created")).is_err());
        assert_eq!(manager.list().unwrap().len(), 1);
        drop(root);
        hkcu.delete_subkey_all(&sub).unwrap();
        std::fs::remove_file(dir.join(format!("{}.json", snapshots[0].id))).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn built_in_presets_only_use_batch_actions() {
        let catalog = crate::catalog::parse_embedded_catalog().unwrap();
        for preset in get_optimization_presets() {
            for entry in preset.entries {
                let (_, actions, _, _) = resolve_apply(
                    &catalog.optimization_groups,
                    &entry.item_id,
                    entry.scope,
                    &entry.value,
                )
                .unwrap();
                assert!(actions.iter().all(is_batch_action), "{}", entry.item_id);
            }
        }
    }

    #[test]
    fn catalog_dialog_item_is_hidden_and_cannot_open_through_apply() {
        let catalog = crate::catalog::parse_embedded_catalog().unwrap();
        let item = find_item(&catalog.optimization_groups, "opt-015").unwrap();
        assert!(item_requires_interaction(item));
        let mut always_applicable = item.clone();
        always_applicable.applicable = crate::catalog::Condition::True;
        let groups = [OptimizationGroup {
            id: "interaction-test".into(),
            name: item.name.clone(),
            applicable: crate::catalog::Condition::True,
            items: vec![always_applicable],
        }];
        assert!(evaluate_all_groups(&groups, &SystemInfo::collect()).is_empty());
        assert!(resolve_apply(
            &groups,
            &item.id,
            ScopeName::Current,
            &ApplyValue::Bool(true)
        )
        .is_err());
    }
}
