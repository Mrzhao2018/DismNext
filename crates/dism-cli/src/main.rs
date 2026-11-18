use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use dism_core::cleaner::{execute_cleanup, scan_all, scan_rule, FileStatus};
use dism_core::diskmap::analyze_path;
use dism_core::evaluator::eval_condition;
use dism_core::optimizer::{apply_optimization, evaluate_all_groups, ApplyValue, ScopeName, ScopeStatus};
use dism_core::{Catalog, SnapshotManager, SystemInfo};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dism-cli", about = "DismNext headless system cleanup & optimization CLI")]
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
        #[arg(long)]
        json: bool,
    },
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
    let snap_mgr = SnapshotManager::new(SnapshotManager::default_dir());

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
            let results = scan_all(&catalog.cleanup_rules, &sys);
            if json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                let mut total_ready = 0u64;
                let mut total_locked = 0u64;
                let mut total_protected = 0u64;
                println!(
                    "{:<10} {:<4} {:>12} {:>8} {:>8}  {}",
                    "ID", "Lv", "Cleanable", "Locked", "Prot", "Name"
                );
                println!("{}", "-".repeat(86));
                for r in &results {
                    total_ready += r.cleanable_bytes;
                    total_locked += r.skipped_locked_bytes;
                    total_protected += r.skipped_protected_bytes;
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
                    results.len(),
                    fmt_bytes(total_ready),
                    fmt_bytes(total_locked),
                    fmt_bytes(total_protected)
                );
            }
        }
        Commands::Preview { rule_id, json } => {
            let rule = catalog
                .cleanup_rules
                .iter()
                .find(|r| r.id == rule_id)
                .ok_or_else(|| anyhow!("Cleanup rule '{}' not found", rule_id))?;
            let res = scan_rule(rule, &sys);
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
                    "{} complete: {} files, {} freed, {} skipped",
                    if dry_run { "Dry-Run" } else { "Cleanup" },
                    report.total_deleted_files,
                    fmt_bytes(report.total_deleted_bytes),
                    report.total_skipped_files
                );
                for o in &report.outcomes {
                    println!(
                        "  {} ({}) -> {} ({} files, {} locked, {} protected, {} failed)",
                        o.id,
                        o.name.zh,
                        fmt_bytes(o.deleted_bytes),
                        o.deleted_files,
                        o.skipped_locked_files,
                        o.skipped_protected_files,
                        o.failed_files
                    );
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
                    _ => return Err(anyhow!("Invalid scope '{}': expected current|default|system", scope)),
                };
                let val_enum = if value.eq_ignore_ascii_case("true") {
                    ApplyValue::Bool(true)
                } else if value.eq_ignore_ascii_case("false") {
                    ApplyValue::Bool(false)
                } else {
                    let idx: usize = value
                        .parse()
                        .map_err(|_| anyhow!("Invalid value '{}': expected true|false|index", value))?;
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
                println!("Restored snapshot {} ({})", restored.id, restored.item_name.zh);
            }
        },
        Commands::Diskmap { path, depth, json } => {
            let target = match path {
                Some(p) => p,
                None => PathBuf::from(
                    std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\".to_string()),
                ),
            };
            let report = analyze_path(&target, depth)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "=== Disk Analysis: {} ({} ms) ===",
                    report.root_path, report.elapsed_ms
                );
                println!(
                    "Total: {} | Files: {} | Dirs: {}",
                    fmt_bytes(report.total_bytes),
                    report.total_files,
                    report.total_dirs
                );
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
                println!("\nTop Largest Files:");
                for lf in report.largest_files.iter().take(10) {
                    println!("  {:>10}  {}", fmt_bytes(lf.size_bytes), lf.path);
                }
            }
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

fn fmt_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let f = b as f64;
    if f >= GB {
        format!("{:.2} GB", f / GB)
    } else if f >= MB {
        format!("{:.2} MB", f / MB)
    } else if f >= KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{} B", b)
    }
}
