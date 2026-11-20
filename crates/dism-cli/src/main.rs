use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use dism_core::appx::{list_appx_packages, remove_appx_package, AppxCategory};
use dism_core::cleaner::{
    execute_cleanup, scan_all, scan_all_with_summary, scan_rule_preview, FileStatus,
};
use dism_core::diskmap::{analyze_path_with_options, DiskAllocation};
use dism_core::drivers::{delete_driver, export_drivers, list_drivers};
use dism_core::evaluator::eval_condition;
use dism_core::features::{list_optional_features, set_feature_enabled, FeatureCategory};
use dism_core::fmt_bytes;
use dism_core::mcp::run_mcp_stdio;
use dism_core::offline_wim::{inspect_wim, WimMounts};
use dism_core::optimizer::{
    apply_optimization, evaluate_all_groups, ApplyValue, ScopeName, ScopeStatus,
};
use dism_core::startup::{list_startup_items, set_startup_enabled};
use dism_core::{Catalog, SnapshotManager, SystemInfo};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "dism-cli",
    version,
    about = "DismNext headless system cleanup & optimization CLI"
)]
struct Cli {
    #[arg(long)]
    catalog: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display Windows environment, disk usage, admin status, and catalog stats
    Info {
        #[arg(long)]
        json: bool,
    },
    /// Scan all applicable space-recycle rules (read-only)
    Scan {
        #[arg(long)]
        json: bool,
    },
    /// Show Dry-Run file-level diagnostics for a specific rule ID
    Preview {
        rule_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Execute cleanup for one or more rule IDs (defaults to dry-run unless --execute is passed)
    Clean {
        #[arg(required = true)]
        rule_ids: Vec<String>,
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        json: bool,
    },
    /// Optimization queries and mutations
    Opt {
        #[command(subcommand)]
        action: OptCommands,
    },
    /// Registry snapshot history and rollback
    Snapshot {
        #[command(subcommand)]
        action: SnapshotCommands,
    },
    /// Parallel disk space analyzer (treemap & extension breakdown)
    Diskmap {
        path: Option<PathBuf>,
        #[arg(long, default_value_t = 4)]
        depth: usize,
        /// Also measure allocated file data on NTFS, counting hard links once
        #[arg(long)]
        allocated: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect a WIM file and the current Windows WIM mounts
    Wim {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Startup items manager
    Startup {
        #[command(subcommand)]
        action: StartupCommands,
    },
    /// Modern Windows AppX/MSIX package manager
    Appx {
        #[command(subcommand)]
        action: AppxCommands,
    },
    /// Third-party DriverStore package manager & backup utility
    Drivers {
        #[command(subcommand)]
        action: DriversCommands,
    },
    /// Windows Optional Features (WSL, Hyper-V, NetFx3, Sandbox, SMB1, etc.)
    Features {
        #[command(subcommand)]
        action: FeaturesCommands,
    },
    /// Run the Model Context Protocol (MCP) server over stdio (JSON-RPC 2.0)
    Mcp,
}

#[derive(Subcommand)]
enum DriversCommands {
    /// List third-party oem*.inf driver packages
    List {
        #[arg(long)]
        outdated_only: bool,
        #[arg(long)]
        json: bool,
    },
    /// Export backed-up driver packages to a folder
    Export {
        target_dir: PathBuf,
        #[arg(long)]
        names: Vec<String>,
    },
    /// Safely delete a non-active oem*.inf driver package
    Delete { published_name: String },
}

#[derive(Subcommand)]
enum FeaturesCommands {
    /// List Windows Optional Features
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Enable or disable a Windows Optional Feature via DISM (Admin required)
    Toggle {
        name: String,
        #[arg(long)]
        enable: bool,
    },
}

#[derive(Subcommand)]
enum StartupCommands {
    /// List startup entries across Run keys and Startup folders
    List {
        #[arg(long)]
        json: bool,
    },
    /// Enable or disable a startup entry via StartupApproved
    Toggle {
        id: String,
        #[arg(long)]
        enable: bool,
    },
}

#[derive(Subcommand)]
enum AppxCommands {
    /// List installed AppX/MSIX packages
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Uninstall a non-protected AppX/MSIX package
    Remove { package_full_name: String },
}

#[derive(Subcommand)]
enum OptCommands {
    /// List applicable optimization items and live registry states
    List {
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Apply an optimization state with automatic registry snapshot
    Set {
        item_id: String,
        #[arg(long)]
        scope: String,
        #[arg(long)]
        value: String,
    },
}

#[derive(Subcommand)]
enum SnapshotCommands {
    /// List saved registry snapshots
    List {
        #[arg(long)]
        json: bool,
    },
    /// Restore registry values from a saved snapshot ID
    Restore { snapshot_id: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let catalog = Catalog::load(cli.catalog.as_deref())?;
    let sys = SystemInfo::collect();
    let snap_mgr = SnapshotManager::user_default();

    match cli.command {
        Commands::Info { json } => {
            if json {
                let payload = serde_json::json!({
                    "system": sys,
                    "catalog": {
                        "base_version": catalog.base_version,
                        "cleanup_rules": catalog.cleanup_rules.len(),
                        "optimization_groups": catalog.optimization_groups.len(),
                        "optimization_items": catalog.optimization_groups.iter().map(|g| g.items.len()).sum::<usize>(),
                    }
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("=== DismNext System Info ===");
                println!(
                    "OS       : {} {} ({})",
                    sys.product_name, sys.display_version, sys.version_string
                );
                println!("Arch     : {} (code {})", sys.arch, sys.arch_code);
                println!("Admin    : {}", if sys.is_admin { "Yes" } else { "No" });
                println!(
                    "Drive {} : {} free / {} total ({} used)",
                    sys.system_disk.root,
                    fmt_bytes(sys.system_disk.free_bytes),
                    fmt_bytes(sys.system_disk.total_bytes),
                    fmt_bytes(sys.system_disk.used_bytes),
                );
                println!(
                    "Catalog  : {} ({} cleanup rules, {} groups / {} optimization items)",
                    catalog.base_version,
                    catalog.cleanup_rules.len(),
                    catalog.optimization_groups.len(),
                    catalog
                        .optimization_groups
                        .iter()
                        .map(|g| g.items.len())
                        .sum::<usize>()
                );
            }
        }
        Commands::Scan { json } => {
            if json {
                let results = scan_all(&catalog.cleanup_rules, &sys);
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                let scan = scan_all_with_summary(&catalog.cleanup_rules, &sys);
                println!(
                    "{:<10} {:<4} {:>12} {:>8} {:>8}  Name",
                    "ID", "Lv", "Cleanable", "Locked", "Prot"
                );
                println!("{}", "-".repeat(86));
                for r in &scan.items {
                    let mark = if r.is_system_task { " [SYS]" } else { "" };
                    println!(
                        "{:<10} L{:<3} {:>12} {:>8} {:>8}  {}{}",
                        r.id,
                        r.level,
                        fmt_bytes(r.cleanable_bytes),
                        r.skipped_locked_count,
                        r.skipped_protected_count,
                        r.name.zh,
                        mark
                    );
                }
                println!("{}", "-".repeat(86));
                println!(
                    "Applicable rules: {} | Total cleanable: {} | Skipped locked: {} | Skipped protected: {}",
                    scan.items.len(),
                    fmt_bytes(scan.summary.cleanable_bytes),
                    fmt_bytes(scan.summary.skipped_locked_bytes),
                    fmt_bytes(scan.summary.skipped_protected_bytes)
                );
            }
        }
        Commands::Preview { rule_id, json } => {
            let rule = catalog
                .cleanup_rules
                .iter()
                .find(|r| r.id == rule_id)
                .ok_or_else(|| anyhow!("Cleanup rule '{}' not found", rule_id))?;
            let res = scan_rule_preview(rule, &sys);
            if json {
                println!("{}", serde_json::to_string_pretty(&res)?);
            } else {
                println!("{} - {} (L{})", res.id, res.name.zh, res.level);
                if let Some(hint) = &res.system_task_hint {
                    println!("System Task: {}", hint.zh);
                }
                println!(
                    "Cleanable: {} ({} files) | Locked: {} ({}) | Protected: {} ({})",
                    fmt_bytes(res.cleanable_bytes),
                    res.cleanable_count,
                    fmt_bytes(res.skipped_locked_bytes),
                    res.skipped_locked_count,
                    fmt_bytes(res.skipped_protected_bytes),
                    res.skipped_protected_count
                );
                for f in &res.preview_files {
                    let st = match f.status {
                        FileStatus::Ready => "READY",
                        FileStatus::Locked => "LOCKED",
                        FileStatus::Protected => "PROTECTED",
                    };
                    println!(
                        "  [{:<9}] {:>10}  {}{}",
                        st,
                        fmt_bytes(f.size_bytes),
                        f.path,
                        f.reason
                            .as_ref()
                            .map(|r| format!(" ({})", r))
                            .unwrap_or_default()
                    );
                }
            }
        }
        Commands::Clean {
            rule_ids,
            execute,
            json,
        } => {
            let dry_run = !execute;
            let report = execute_cleanup(&catalog.cleanup_rules, &rule_ids, &sys, dry_run);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{} complete: {} targets ({} total size), {} skipped",
                    if dry_run { "Dry-Run" } else { "Cleanup" },
                    report.total_deleted_files,
                    fmt_bytes(report.total_deleted_bytes),
                    report.total_skipped_files
                );
                for o in &report.outcomes {
                    println!(
                        "  {} ({}): permanent deletion {} ({}), Recycle Bin {} ({}), driver removal {} ({}), {} locked, {} protected, {} failed",
                        o.id,
                        o.name.zh,
                        o.actions.permanent_delete.count,
                        fmt_bytes(o.actions.permanent_delete.bytes),
                        o.actions.recycle.count,
                        fmt_bytes(o.actions.recycle.bytes),
                        o.actions.remove_driver.count,
                        fmt_bytes(o.actions.remove_driver.bytes),
                        o.skipped_locked_files,
                        o.skipped_protected_files,
                        o.failed_files
                    );
                    for error in &o.errors {
                        println!("    {error}");
                    }
                }
            }
        }
        Commands::Opt { action } => match action {
            OptCommands::List { search, json } => {
                let groups = evaluate_all_groups(&catalog.optimization_groups, &sys);
                let kw = search.as_deref().map(|s| s.to_lowercase());
                if json {
                    println!("{}", serde_json::to_string_pretty(&groups)?);
                } else {
                    let mut count = 0usize;
                    for g in &groups {
                        let matched: Vec<_> = g
                            .items
                            .iter()
                            .filter(|it| match &kw {
                                None => true,
                                Some(q) => {
                                    it.name.zh.to_lowercase().contains(q)
                                        || it.name.en.to_lowercase().contains(q)
                                        || g.name.zh.to_lowercase().contains(q)
                                }
                            })
                            .collect();
                        if matched.is_empty() {
                            continue;
                        }
                        println!("\n[{}] {}", g.id, g.name.zh);
                        for it in matched {
                            count += 1;
                            println!(
                                "  {:<8} {:<8} C:{} D:{} S:{}  {}",
                                it.id,
                                it.item_type,
                                fmt_scope(&it.current),
                                fmt_scope(&it.default),
                                fmt_scope(&it.system),
                                it.name.zh
                            );
                        }
                    }
                    println!("\nTotal matching applicable optimization items: {}", count);
                }
            }
            OptCommands::Set {
                item_id,
                scope,
                value,
            } => {
                let scope_enum = match scope.to_ascii_lowercase().as_str() {
                    "current" => ScopeName::Current,
                    "default" => ScopeName::Default,
                    "system" => ScopeName::System,
                    _ => {
                        return Err(anyhow!(
                            "Invalid scope '{}': expected current|default|system",
                            scope
                        ))
                    }
                };
                let val_enum = if value.eq_ignore_ascii_case("true") {
                    ApplyValue::Bool(true)
                } else if value.eq_ignore_ascii_case("false") {
                    ApplyValue::Bool(false)
                } else {
                    let idx: usize = value.parse().map_err(|_| {
                        anyhow!("Invalid value '{}': expected true|false|index", value)
                    })?;
                    ApplyValue::Index(idx)
                };
                let outcome = apply_optimization(
                    &catalog.optimization_groups,
                    &item_id,
                    scope_enum,
                    val_enum,
                    &snap_mgr,
                )?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            }
        },
        Commands::Snapshot { action } => match action {
            SnapshotCommands::List { json } => {
                let list = snap_mgr.list()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&list)?);
                } else if list.is_empty() {
                    println!("No registry snapshots recorded yet.");
                } else {
                    for s in list {
                        println!(
                            "{} | {} | {} ({}) | {} records | restored={}",
                            s.id,
                            s.created_at,
                            s.item_name.zh,
                            s.scope,
                            s.records.len(),
                            s.restored
                        );
                    }
                }
            }
            SnapshotCommands::Restore { snapshot_id } => {
                let restored = snap_mgr.restore(&snapshot_id)?;
                println!(
                    "Restored snapshot {} ({})",
                    restored.id, restored.item_name.zh
                );
            }
        },
        Commands::Diskmap {
            path,
            depth,
            allocated,
            json,
        } => {
            let target = match path {
                Some(p) => p,
                None => PathBuf::from(
                    std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\".to_string()),
                ),
            };
            let report = analyze_path_with_options(&target, depth, allocated, None)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "=== Disk Analysis: {} ({} ms) ===",
                    report.root_path, report.elapsed_ms
                );
                println!(
                    "Logical size: {} | Files: {} | Dirs: {}",
                    fmt_bytes(report.total_bytes),
                    report.total_files,
                    report.total_dirs
                );
                if let Some(allocation) = &report.allocation {
                    match allocation {
                        DiskAllocation::Measured {
                            allocated_bytes,
                            hard_link_aliases,
                            unmeasured_files,
                            unreadable_entries,
                        } => println!(
                            "Measured NTFS file data: {} | Hard-link aliases excluded: {} | Unmeasured files: {} | Unreadable entries: {}",
                            fmt_bytes(*allocated_bytes),
                            hard_link_aliases,
                            unmeasured_files,
                            unreadable_entries
                        ),
                        DiskAllocation::UnsupportedFileSystem => {
                            println!("Allocation measurement is supported on NTFS volumes.")
                        }
                        DiskAllocation::Unavailable => {
                            println!("Could not query the target volume for allocation measurement.")
                        }
                    }
                }
                println!("\nTop Subdirectories / Entries:");
                for ch in report.tree.children.iter().take(15) {
                    let pct = if report.total_bytes > 0 {
                        (ch.size_bytes as f64 / report.total_bytes as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!(
                        "  {:>10} ({:5.1}%)  {} {}",
                        fmt_bytes(ch.size_bytes),
                        pct,
                        if ch.is_dir { "[DIR]" } else { "     " },
                        ch.name
                    );
                }
                println!("\nTop Extensions:");
                for ext in report.extensions.iter().take(10) {
                    println!(
                        "  {:<12} {:>10} ({} files)",
                        ext.ext,
                        fmt_bytes(ext.size_bytes),
                        ext.file_count
                    );
                }
                println!("\nLargest Files by Logical Size:");
                for lf in report.largest_files.iter().take(10) {
                    let allocation = lf
                        .allocated_bytes
                        .map(|bytes| format!(" (allocated {})", fmt_bytes(bytes)))
                        .unwrap_or_default();
                    println!(
                        "  {:>10}  {}{}",
                        fmt_bytes(lf.size_bytes),
                        lf.path,
                        allocation
                    );
                }
            }
        }
        Commands::Wim { path, json } => {
            let report = inspect_wim(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{} ({} image indices)", report.path, report.images.len());
                for image in &report.images {
                    let name = image.name.as_deref().unwrap_or("Unnamed image");
                    let edition = image.edition.as_deref().unwrap_or("-");
                    let architecture = image.architecture.as_deref().unwrap_or("-");
                    let version = image.version.as_deref().unwrap_or("-");
                    println!(
                        "  #{} {name} | {edition} | {architecture} | {version}",
                        image.index
                    );
                    if let Some(description) = &image.description {
                        println!("    {description}");
                    }
                }
                match report.mounts {
                    WimMounts::Available { entries } if entries.is_empty() => {
                        println!("No WIM mounts reported by Windows.");
                    }
                    WimMounts::Available { entries } => {
                        println!("Windows WIM mounts");
                        for mount in entries {
                            let access = if mount.read_write {
                                "read/write"
                            } else {
                                "read-only"
                            };
                            println!(
                                "  {} #{} -> {} | {:?} | {access}",
                                mount.image_path, mount.index, mount.mount_path, mount.state
                            );
                        }
                    }
                    WimMounts::AccessDenied => {
                        println!("Windows denied access to the mount list. Run as administrator to check it.");
                    }
                    WimMounts::Unavailable { error } => {
                        println!("Could not query the Windows WIM mounts\n  {error}");
                    }
                }
            }
        }
        Commands::Startup { action } => match action {
            StartupCommands::List { json } => {
                let items = list_startup_items();
                if json {
                    println!("{}", serde_json::to_string_pretty(&items)?);
                } else {
                    println!("{:<6} {:<20} {:<28} Command", "State", "Location", "Name");
                    println!("{}", "-".repeat(90));
                    for it in &items {
                        println!(
                            "{:<6} {:<20} {:<28} {}",
                            if it.enabled { "ON" } else { "OFF" },
                            it.location,
                            it.name,
                            it.command
                        );
                    }
                    println!("\nTotal startup items: {}", items.len());
                }
            }
            StartupCommands::Toggle { id, enable } => {
                let item = set_startup_enabled(&id, enable)?;
                println!("{}", serde_json::to_string_pretty(&item)?);
            }
        },
        Commands::Appx { action } => match action {
            AppxCommands::List { category, json } => {
                let mut pkgs = list_appx_packages();
                if let Some(ref cat) = category {
                    let target = cat.to_ascii_lowercase();
                    pkgs.retain(|p| match p.category {
                        AppxCategory::Bloatware => target == "bloatware",
                        AppxCategory::UserApp => target == "user_app" || target == "user",
                        AppxCategory::Framework => target == "framework",
                        AppxCategory::SystemCore => target == "system_core" || target == "system",
                    });
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&pkgs)?);
                } else {
                    for p in &pkgs {
                        println!(
                            "[{:?}{}] {:<30} {:<16} {}",
                            p.category,
                            if p.protected { "/LOCK" } else { "" },
                            p.display_name.zh,
                            p.version,
                            p.package_full_name
                        );
                    }
                    println!("\nTotal matching packages: {}", pkgs.len());
                }
            }
            AppxCommands::Remove { package_full_name } => {
                remove_appx_package(&package_full_name)?;
                println!("Removed AppX package: {}", package_full_name);
            }
        },
        Commands::Drivers { action } => match action {
            DriversCommands::List {
                outdated_only,
                json,
            } => {
                let mut drivers = list_drivers();
                if outdated_only {
                    drivers.retain(|d| d.is_outdated_duplicate);
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&drivers)?);
                } else {
                    println!(
                        "{:<10} {:<24} {:<16} {:<14} {:>10}  Status",
                        "OEM INF", "Original Name", "Provider", "Version", "Size"
                    );
                    println!("{}", "-".repeat(94));
                    let total_bytes: u64 = drivers.iter().map(|d| d.size_bytes).sum();
                    let outdated_count = drivers.iter().filter(|d| d.is_outdated_duplicate).count();
                    for d in &drivers {
                        let status = if let Some(ref newer) = d.newer_version {
                            format!("OUTDATED -> {}", newer)
                        } else {
                            d.class_name.clone()
                        };
                        println!(
                            "{:<10} {:<24} {:<16} {:<14} {:>10}  {}",
                            d.published_name,
                            d.original_name,
                            d.provider.chars().take(15).collect::<String>(),
                            d.version,
                            fmt_bytes(d.size_bytes),
                            status
                        );
                    }
                    println!(
                        "\nTotal: {} packages ({}) | Outdated duplicates: {}",
                        drivers.len(),
                        fmt_bytes(total_bytes),
                        outdated_count
                    );
                }
            }
            DriversCommands::Export { target_dir, names } => {
                let filter = if names.is_empty() {
                    None
                } else {
                    Some(names.as_slice())
                };
                let report = export_drivers(&target_dir, filter)?;
                println!(
                    "Backed up {} drivers ({}) to {}",
                    report.exported_count,
                    fmt_bytes(report.total_bytes),
                    report.target_dir
                );
                println!("Manifest: {}", report.manifest_path);
            }
            DriversCommands::Delete { published_name } => {
                let out = delete_driver(&published_name)?;
                println!("{}", out);
            }
        },
        Commands::Features { action } => match action {
            FeaturesCommands::List { category, json } => {
                let mut features = list_optional_features();
                if let Some(ref cat) = category {
                    let target = cat.to_ascii_lowercase();
                    features.retain(|f| match f.category {
                        FeatureCategory::Virtualization => target == "virtualization",
                        FeatureCategory::Developer => target == "developer",
                        FeatureCategory::SecurityAi => {
                            target == "security_ai" || target == "security"
                        }
                        FeatureCategory::NetworkSmb => {
                            target == "network_smb" || target == "network"
                        }
                        FeatureCategory::PrintMedia => target == "print_media" || target == "print",
                        FeatureCategory::IisWeb => target == "iis_web" || target == "iis",
                    });
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&features)?);
                } else {
                    println!("{:<6} {:<40} Display Name", "State", "Feature Name");
                    println!("{}", "-".repeat(90));
                    for f in &features {
                        println!(
                            "{:<6} {:<40} {}",
                            if f.enabled { "ON" } else { "OFF" },
                            f.name,
                            f.display_name.zh
                        );
                    }
                    println!("\nTotal matching features: {}", features.len());
                }
            }
            FeaturesCommands::Toggle { name, enable } => {
                let res = set_feature_enabled(&name, enable)?;
                println!("{}", serde_json::to_string_pretty(&res)?);
            }
        },
        Commands::Mcp => {
            run_mcp_stdio(&catalog)?;
        }
    }

    let _ = eval_condition;
    Ok(())
}

fn fmt_scope(s: &Option<ScopeStatus>) -> String {
    match s {
        None => "-".to_string(),
        Some(ScopeStatus::CheckBox(cb)) => if cb.enabled { "ON" } else { "OFF" }.to_string(),
        Some(ScopeStatus::Combo(c)) => c
            .selected_index
            .map(|i| format!("#{}", i))
            .unwrap_or_else(|| "?".to_string()),
    }
}
