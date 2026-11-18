pub mod appx;
pub mod catalog;
pub mod cleaner;
pub mod diskmap;
pub mod evaluator;
pub mod optimizer;
pub mod snapshot;
pub mod startup;
pub mod sysinfo;

pub use appx::{list_appx_packages, remove_appx_package, AppxCategory, AppxPackageInfo};
pub use catalog::{Catalog, CleanupRule, OptimizationGroup};
pub use cleaner::{
    execute_cleanup, scan_all, scan_rule, CleanupExecutionReport, CleanupScanResult, FileStatus,
    PreviewFile,
};
pub use diskmap::{
    analyze_path, get_presets, recycle_path, DiskAnalysisReport, DiskPreset, DiskTreeNode,
};
pub use optimizer::{
    apply_optimization, evaluate_all_groups, ApplyOutcome, ApplyValue, OptimizationGroupStatus,
    OptimizationItemStatus, ScopeName, ScopeStatus,
};
pub use snapshot::{snapshot_summary, RegistrySnapshot, SnapshotManager, SnapshotSummary};
pub use startup::{list_startup_items, set_startup_enabled, StartupItem, StartupScope};
pub use sysinfo::SystemInfo;
