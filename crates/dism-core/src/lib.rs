pub mod catalog;
pub mod cleaner;
pub mod evaluator;
pub mod optimizer;
pub mod snapshot;
pub mod sysinfo;

pub use catalog::{Catalog, CleanupRule, OptimizationGroup};
pub use cleaner::{
    execute_cleanup, scan_all, scan_rule, CleanupExecutionReport, CleanupScanResult, FileStatus,
    PreviewFile,
};
pub use optimizer::{
    apply_optimization, evaluate_all_groups, ApplyOutcome, ApplyValue, OptimizationGroupStatus,
    OptimizationItemStatus, ScopeName, ScopeStatus,
};
pub use snapshot::{snapshot_summary, RegistrySnapshot, SnapshotManager, SnapshotSummary};
pub use sysinfo::SystemInfo;
