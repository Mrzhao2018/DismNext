pub mod appx;
pub mod catalog;
pub mod cleaner;
pub mod diskmap;
pub mod drivers;
pub mod evaluator;
pub mod features;
pub mod optimizer;
pub mod snapshot;
pub mod startup;
pub mod sysinfo;
pub mod systasks;

pub use appx::{list_appx_packages, remove_appx_package, AppxCategory, AppxPackageInfo};
pub use catalog::{Catalog, CleanupRule, OptimizationGroup};
pub use cleaner::{
    execute_cleanup, is_deferred_system_task_proc, scan_all, scan_custom_proc_name, scan_rule,
    CleanupExecutionReport, CleanupScanResult, FileStatus, PreviewFile,
};
pub use diskmap::{
    analyze_path, get_presets, recycle_path, DiskAnalysisReport, DiskPreset, DiskTreeNode,
};
pub use drivers::{
    default_backup_dir, delete_driver, export_drivers, list_drivers, DriverBackupReport,
    DriverPackageInfo,
};
pub use features::{
    list_optional_features, set_feature_enabled, FeatureCategory, FeatureToggleOutcome,
    WindowsFeatureInfo,
};
pub use optimizer::{
    apply_optimization, evaluate_all_groups, ApplyOutcome, ApplyValue, OptimizationGroupStatus,
    OptimizationItemStatus, ScopeName, ScopeStatus,
};
pub use snapshot::{snapshot_summary, RegistrySnapshot, SnapshotManager, SnapshotSummary};
pub use startup::{list_startup_items, set_startup_enabled, StartupItem, StartupScope};
pub use sysinfo::SystemInfo;
pub use systasks::{
    get_system_task_status, query_compact_os, run_system_task, SystemTaskExecOutcome,
    SystemTaskStatus,
};
