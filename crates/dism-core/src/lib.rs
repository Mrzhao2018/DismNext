pub mod appx;
pub mod catalog;
pub mod cleaner;
mod disk_allocation;
pub mod diskmap;
pub mod drivers;
pub mod duplicate_recycle;
pub mod evaluator;
pub mod features;
pub mod lockcheck;
pub mod mcp;
pub mod optimizer;
pub mod report;
pub mod rescue;
pub mod scan_progress;
pub mod snapshot;
pub mod startup;
pub mod sysinfo;
pub mod systasks;
pub mod uninstaller;

pub use appx::{list_appx_packages, remove_appx_package, AppxCategory, AppxPackageInfo};
pub use catalog::{Catalog, CleanupRule, CustomRuleInput, OptimizationGroup};
pub use cleaner::{
    execute_cleanup, is_deferred_system_task_proc, scan_all, scan_custom_proc_name, scan_rule,
    scan_rule_preview, CleanupExecutionReport, CleanupScanResult, FileStatus, PreviewFile,
};
pub use diskmap::{
    analyze_path, find_duplicate_files, find_orphaned_app_dirs, get_presets, recycle_path,
    DiskAnalysisReport, DiskPreset, DiskTreeNode, DuplicateFileEntry, DuplicateGroup,
    DuplicateScanReport, OrphanDirEntry,
};
pub use drivers::{
    default_backup_dir, delete_driver, export_drivers, list_drivers, DriverBackupReport,
    DriverPackageInfo,
};
pub use features::{
    list_optional_features, set_feature_enabled, FeatureCategory, FeatureToggleOutcome,
    WindowsFeatureInfo,
};
pub use lockcheck::{
    find_locking_processes, find_locking_processes_batch, terminate_locking_process,
    LockingProcessInfo,
};
pub use optimizer::{
    apply_optimization, apply_optimization_batch, evaluate_all_groups,
    export_optimization_profile, get_optimization_presets, ApplyOutcome, ApplyValue,
    BatchApplyOutcome, OptimizationGroupStatus, OptimizationItemStatus, OptimizationPreset,
    OptimizationProfile, OptimizationProfileEntry, ScopeName, ScopeStatus,
};
pub use report::{generate_health_report, HealthReportPayload, ReportRuleSummary};
pub use rescue::{
    create_restore_point, list_restore_points, run_rescue_action, RescueActionOutcome,
    RestorePointInfo,
};
pub use snapshot::{snapshot_summary, RegistrySnapshot, SnapshotManager, SnapshotSummary};
pub use startup::{
    list_scheduled_tasks, list_startup_items, list_third_party_services,
    set_scheduled_task_enabled, set_service_start_mode, set_startup_enabled, ScheduledTaskItem,
    StartupItem, StartupScope, ThirdPartyServiceItem,
};
pub use sysinfo::SystemInfo;
pub use systasks::{
    get_system_task_status, query_compact_os, run_system_task, SystemTaskExecOutcome,
    SystemTaskStatus,
};
pub use uninstaller::{
    invalidate_win32_apps_cache, launch_win32_uninstaller, list_win32_apps, scan_app_leftovers,
    AppLeftoverItem, LeftoverMatch, UninstallState, UninstallStatus, Win32AppInfo, Win32Uninstall,
};
