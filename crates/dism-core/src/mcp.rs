use crate::catalog::Catalog;
use crate::cleaner::{execute_cleanup, scan_all, scan_rule_preview};
use crate::diskmap::{analyze_path, find_duplicate_files, find_orphaned_app_dirs};
use crate::drivers::{default_backup_dir, export_drivers, list_drivers};
use crate::features::{list_optional_features, FeatureCategory};
use crate::lockcheck::find_locking_processes;
use crate::optimizer::{evaluate_all_groups, get_optimization_presets};
use crate::report::generate_health_report;
use crate::rescue::{create_restore_point, list_restore_points, run_rescue_action};
use crate::snapshot::{snapshot_summary, SnapshotManager, SnapshotSummary};
use crate::startup::{list_scheduled_tasks, list_startup_items, list_third_party_services};
use crate::sysinfo::SystemInfo;
use crate::systasks::get_system_task_status;
use crate::uninstaller::{list_win32_apps, scan_app_leftovers};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const SERVER_NAME: &str = "dismnext-mcp";
const SERVER_VERSION: &str = "2.0.0";
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

enum FramingMode {
    Newline,
    ContentLength,
}

/// Starts the Model Context Protocol (MCP) JSON-RPC 2.0 server over stdio.
/// Supports both newline-delimited JSON-RPC messages and `Content-Length` framed payloads.
pub fn run_mcp_stdio(catalog: &Catalog) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    loop {
        let Some((payload, framing)) = read_next_message(&mut reader)? else {
            break;
        };

        let response = match serde_json::from_str::<Value>(&payload) {
            Ok(req) => handle_mcp_request(&req, catalog),
            Err(err) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {
                    "code": -32700,
                    "message": format!("Parse error: {}", err)
                }
            })),
        };

        if let Some(resp) = response {
            write_message(&mut writer, &resp, framing)?;
        }
    }

    Ok(())
}

fn read_next_message<R: BufRead>(reader: &mut R) -> Result<Option<(String, FramingMode)>> {
    let mut first_line = String::new();
    loop {
        first_line.clear();
        let n = reader.read_line(&mut first_line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = first_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = strip_ascii_prefix_ci(trimmed, "content-length:") {
            let mut content_len: usize = rest
                .trim()
                .parse()
                .map_err(|_| anyhow!("Invalid Content-Length header: {}", trimmed))?;

            let mut header_line = String::new();
            loop {
                header_line.clear();
                let hn = reader.read_line(&mut header_line)?;
                if hn == 0 {
                    return Err(anyhow!("Unexpected EOF while reading MCP headers"));
                }
                let ht = header_line.trim();
                if ht.is_empty() {
                    break;
                }
                if let Some(more_len) = strip_ascii_prefix_ci(ht, "content-length:") {
                    content_len = more_len
                        .trim()
                        .parse()
                        .map_err(|_| anyhow!("Invalid Content-Length header: {}", ht))?;
                }
            }

            let mut body = vec![0u8; content_len];
            reader.read_exact(&mut body)?;
            let text = String::from_utf8(body)?;
            return Ok(Some((text, FramingMode::ContentLength)));
        }

        return Ok(Some((trimmed.to_string(), FramingMode::Newline)));
    }
}

fn strip_ascii_prefix_ci<'a>(s: &'a str, prefix_lower: &str) -> Option<&'a str> {
    if s.len() < prefix_lower.len() {
        return None;
    }
    let head = s.get(..prefix_lower.len())?;
    if head.eq_ignore_ascii_case(prefix_lower) {
        Some(&s[prefix_lower.len()..])
    } else {
        None
    }
}

fn write_message<W: Write>(writer: &mut W, value: &Value, framing: FramingMode) -> Result<()> {
    let body = serde_json::to_string(value)?;
    match framing {
        FramingMode::Newline => {
            writer.write_all(body.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        FramingMode::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        }
    }
    writer.flush()?;
    Ok(())
}

/// Processes a single parsed JSON-RPC 2.0 MCP message and returns an optional JSON-RPC response.
/// Notifications (messages without an `id` or explicit notification methods) return `None`.
pub fn handle_mcp_request(req: &Value, catalog: &Catalog) -> Option<Value> {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id_val = req.get("id");

    // JSON-RPC notifications do not receive a response.
    if id_val.is_none()
        || method.starts_with("notifications/")
        || method == "initialized"
    {
        return None;
    }

    let id = id_val.cloned().unwrap_or(Value::Null);
    let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "initialize" => {
            let client_proto = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(DEFAULT_PROTOCOL_VERSION);
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": client_proto,
                    "capabilities": {
                        "tools": {
                            "listChanged": false
                        }
                    },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "version": SERVER_VERSION
                    }
                }
            }))
        }
        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        })),
        "resources/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resources": []
            }
        })),
        "prompts/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "prompts": []
            }
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": tool_definitions()
            }
        })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            match dispatch_tool_call(name, &args, catalog) {
                Ok(payload) => {
                    let text = serde_json::to_string_pretty(&payload)
                        .unwrap_or_else(|_| payload.to_string());
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": text
                                }
                            ],
                            "isError": false
                        }
                    }))
                }
                Err(err) => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": format!("Tool '{}' error: {}", name, err)
                            }
                        ],
                        "isError": true
                    }
                })),
            }
        }
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {}", method)
            }
        })),
    }
}

pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "dismnext_system_overview",
            "description": "Get Windows environment info (OS version, architecture, admin privilege, C: disk free/total space), catalog counts, CompactOS state, outdated driver duplicates, and orphaned Windows Installer file summary.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_scan_cleanup",
            "description": "Scan Windows system cleanup & space-recycle rules (read-only). Omit rule_id to scan all applicable rules across the system; provide rule_id to run deep file-level diagnostics with Windows Restart Manager lock detection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "rule_id": {
                        "type": "string",
                        "description": "Optional cleanup rule ID for detailed file preview and lock inspection."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_execute_cleanup",
            "description": "Execute cleanup for specified space-recycle rule IDs. Defaults to dry_run=true for safety; set dry_run=false only when ready to delete or recycle matched files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "rule_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of cleanup rule IDs to evaluate or clean."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true (default), simulate cleanup without deleting files."
                    }
                },
                "required": ["rule_ids"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_analyze_disk",
            "description": "Parallel multi-threaded disk space analyzer returning directory tree breakdown, top file extensions, and top largest files for a target path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to analyze (defaults to %LOCALAPPDATA%)."
                    },
                    "max_depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 8,
                        "description": "Maximum tree recursion depth (default 3)."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_find_duplicates",
            "description": "Three-stage parallel duplicate file detector (size grouping, partial fingerprint, full SHA-256 verification) returning duplicate groups and potential wasted bytes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Root directory path to scan for duplicate files."
                    },
                    "min_size_bytes": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Minimum file size threshold in bytes (default 1048576 = 1 MiB)."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_find_orphan_dirs",
            "description": "Scan %LOCALAPPDATA%, %APPDATA%, and %ProgramData% to detect leftover data folders belonging to uninstalled software.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_inspect_file_locks",
            "description": "Inspect which running Windows processes (PID, application name, critical-process status) are currently locking a file using native Restart Manager (rstrtmgr.dll).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Full file path to inspect for open locks."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_list_drivers",
            "description": "List third-party oem*.inf DriverStore packages with provider, version, size, and strict hardware-descriptor-verified outdated duplicate detection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "outdated_only": {
                        "type": "boolean",
                        "description": "If true, return only superseded duplicate driver packages."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_backup_drivers",
            "description": "Back up third-party DriverStore packages to a destination folder alongside driver_backup_manifest.json and a one-click Restore-Drivers.cmd script.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_dir": {
                        "type": "string",
                        "description": "Destination folder path for driver backups (defaults to C:\\DismNext_DriverBackup)."
                    },
                    "published_names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of oem*.inf published names to export; exports all third-party drivers if omitted."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_list_startup",
            "description": "Inspect Windows startup governance across Run registry/Startup folders, third-party background services, and non-Windows scheduled tasks.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "enum": ["all", "run", "services", "tasks"],
                        "description": "Startup governance category filter (default 'all')."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_list_features",
            "description": "List Windows Optional Features (WSL, Hyper-V, NetFx3, Sandbox, Recall, SMB1, etc.) and their live enable/disable status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "enum": ["virtualization", "developer", "security_ai", "network_smb", "print_media", "iis_web"],
                        "description": "Optional feature category filter."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_list_optimizations",
            "description": "Evaluate live Windows system optimization items across registry scopes and optionally list built-in curated optimization preset packs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "search": {
                        "type": "string",
                        "description": "Optional keyword filter matching item/group names or IDs."
                    },
                    "include_presets": {
                        "type": "boolean",
                        "description": "Include curated preset pack definitions (default true)."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_list_snapshots",
            "description": "List saved DismNext registry snapshots available for safe rollback before/after optimization changes.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_system_rescue",
            "description": "Run Windows System Rescue & Health First-Aid actions or query/create System Restore Points.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "list_restore_points",
                            "create_restore_point",
                            "rebuild_icon_cache",
                            "flush_dns_network",
                            "reset_windows_update",
                            "clear_print_spooler",
                            "dism_check_health",
                            "dism_restore_health",
                            "sfc_scannow"
                        ],
                        "description": "Rescue operation ID or restore-point action."
                    },
                    "description": {
                        "type": "string",
                        "description": "Description text when action='create_restore_point'."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_list_installed_apps",
            "description": "List installed classic desktop software (Win32/MSI) from HKLM/HKCU Uninstall keys, optionally filtering by keyword or scanning leftover folders for a specific app ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "search": {
                        "type": "string",
                        "description": "Optional keyword filter matching display name or publisher."
                    },
                    "scan_leftovers_for_id": {
                        "type": "string",
                        "description": "Optional app ID (from list output) to scan candidate AppData/ProgramData/install leftovers."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_export_health_report",
            "description": "Generate a comprehensive Windows health audit report (score 0-100, disk usage, reclaimable space, drivers, startup items, and installed apps) in Markdown or structured JSON.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "json"],
                        "description": "Output format ('markdown' or 'json'; defaults to 'json')."
                    }
                },
                "additionalProperties": false
            }
        }),
    ]
}

fn dispatch_tool_call(name: &str, args: &Value, catalog: &Catalog) -> Result<Value> {
    let sys = SystemInfo::collect();

    match name {
        "dismnext_system_overview" => {
            let task_status = get_system_task_status();
            let opt_items: usize = catalog
                .optimization_groups
                .iter()
                .map(|g| g.items.len())
                .sum();
            Ok(json!({
                "system": sys,
                "catalog": {
                    "base_version": catalog.base_version,
                    "cleanup_rules": catalog.cleanup_rules.len(),
                    "optimization_groups": catalog.optimization_groups.len(),
                    "optimization_items": opt_items
                },
                "system_tasks": task_status
            }))
        }
        "dismnext_scan_cleanup" => {
            if let Some(rule_id) = args.get("rule_id").and_then(|v| v.as_str()) {
                let rule = catalog
                    .cleanup_rules
                    .iter()
                    .find(|r| r.id == rule_id)
                    .ok_or_else(|| anyhow!("Cleanup rule '{}' not found", rule_id))?;
                let res = scan_rule_preview(rule, &sys);
                Ok(serde_json::to_value(res)?)
            } else {
                let results = scan_all(&catalog.cleanup_rules, &sys);
                let total_cleanable_bytes: u64 = results.iter().map(|r| r.cleanable_bytes).sum();
                let total_locked_bytes: u64 = results.iter().map(|r| r.skipped_locked_bytes).sum();
                let total_protected_bytes: u64 =
                    results.iter().map(|r| r.skipped_protected_bytes).sum();
                Ok(json!({
                    "applicable_rule_count": results.len(),
                    "total_cleanable_bytes": total_cleanable_bytes,
                    "total_skipped_locked_bytes": total_locked_bytes,
                    "total_skipped_protected_bytes": total_protected_bytes,
                    "rules": results
                }))
            }
        }
        "dismnext_execute_cleanup" => {
            let arr = args
                .get("rule_ids")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("Missing required array argument 'rule_ids'"))?;
            let rule_ids: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if rule_ids.is_empty() {
                return Err(anyhow!("'rule_ids' must contain at least one rule ID"));
            }
            let dry_run = args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(true);
            let report = execute_cleanup(&catalog.cleanup_rules, &rule_ids, &sys, dry_run);
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_analyze_disk" => {
            let target = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(
                        std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\".to_string()),
                    )
                });
            let max_depth = args
                .get("max_depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(3)
                .clamp(1, 8) as usize;
            let report = analyze_path(&target, max_depth)?;
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_find_duplicates" => {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow!("Missing required string argument 'path'"))?;
            let min_size_bytes = args
                .get("min_size_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(1024 * 1024);
            let report = find_duplicate_files(Path::new(path_str), min_size_bytes)?;
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_find_orphan_dirs" => {
            let orphans = find_orphaned_app_dirs();
            let total_bytes: u64 = orphans.iter().map(|o| o.size_bytes).sum();
            Ok(json!({
                "orphan_count": orphans.len(),
                "total_bytes": total_bytes,
                "items": orphans
            }))
        }
        "dismnext_inspect_file_locks" => {
            let path_str = args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow!("Missing required string argument 'path'"))?;
            let procs = find_locking_processes(Path::new(path_str));
            Ok(json!({
                "path": path_str,
                "locking_process_count": procs.len(),
                "processes": procs
            }))
        }
        "dismnext_list_drivers" => {
            let outdated_only = args
                .get("outdated_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut drivers = list_drivers();
            if outdated_only {
                drivers.retain(|d| d.is_outdated_duplicate);
            }
            let total_bytes: u64 = drivers.iter().map(|d| d.size_bytes).sum();
            let outdated_count = drivers.iter().filter(|d| d.is_outdated_duplicate).count();
            Ok(json!({
                "count": drivers.len(),
                "outdated_count": outdated_count,
                "total_bytes": total_bytes,
                "drivers": drivers
            }))
        }
        "dismnext_backup_drivers" => {
            let target_dir = args
                .get("target_dir")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(default_backup_dir);
            let filter_names: Option<Vec<String>> = args
                .get("published_names")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                });
            let slice_ref = filter_names.as_deref().filter(|s| !s.is_empty());
            let report = export_drivers(Path::new(&target_dir), slice_ref)?;
            Ok(serde_json::to_value(report)?)
        }
        "dismnext_list_startup" => {
            let cat = args
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("all")
                .to_ascii_lowercase();
            let mut out = serde_json::Map::new();
            out.insert("category".to_string(), json!(cat));
            match cat.as_str() {
                "run" => {
                    out.insert("run_items".to_string(), serde_json::to_value(list_startup_items())?);
                }
                "services" => {
                    out.insert(
                        "services".to_string(),
                        serde_json::to_value(list_third_party_services())?,
                    );
                }
                "tasks" => {
                    out.insert(
                        "scheduled_tasks".to_string(),
                        serde_json::to_value(list_scheduled_tasks())?,
                    );
                }
                _ => {
                    out.insert("run_items".to_string(), serde_json::to_value(list_startup_items())?);
                    out.insert(
                        "services".to_string(),
                        serde_json::to_value(list_third_party_services())?,
                    );
                    out.insert(
                        "scheduled_tasks".to_string(),
                        serde_json::to_value(list_scheduled_tasks())?,
                    );
                }
            }
            Ok(Value::Object(out))
        }
        "dismnext_list_features" => {
            let mut features = list_optional_features();
            if let Some(cat) = args.get("category").and_then(|v| v.as_str()) {
                let target = cat.to_ascii_lowercase();
                features.retain(|f| match f.category {
                    FeatureCategory::Virtualization => target == "virtualization",
                    FeatureCategory::Developer => target == "developer",
                    FeatureCategory::SecurityAi => target == "security_ai" || target == "security",
                    FeatureCategory::NetworkSmb => target == "network_smb" || target == "network",
                    FeatureCategory::PrintMedia => target == "print_media" || target == "print",
                    FeatureCategory::IisWeb => target == "iis_web" || target == "iis",
                });
            }
            let enabled_count = features.iter().filter(|f| f.enabled).count();
            Ok(json!({
                "count": features.len(),
                "enabled_count": enabled_count,
                "features": features
            }))
        }
        "dismnext_list_optimizations" => {
            let mut groups = evaluate_all_groups(&catalog.optimization_groups, &sys);
            if let Some(search) = args
                .get("search")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
            {
                groups.retain_mut(|g| {
                    let group_match = g.name.zh.to_ascii_lowercase().contains(&search)
                        || g.name.en.to_ascii_lowercase().contains(&search)
                        || g.id.to_ascii_lowercase().contains(&search);
                    if !group_match {
                        g.items.retain(|it| {
                            it.name.zh.to_ascii_lowercase().contains(&search)
                                || it.name.en.to_ascii_lowercase().contains(&search)
                                || it.id.to_ascii_lowercase().contains(&search)
                        });
                    }
                    !g.items.is_empty()
                });
            }
            let include_presets = args
                .get("include_presets")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let item_count: usize = groups.iter().map(|g| g.items.len()).sum();
            let mut payload = json!({
                "group_count": groups.len(),
                "item_count": item_count,
                "groups": groups
            });
            if include_presets {
                payload["presets"] = serde_json::to_value(get_optimization_presets())?;
            }
            Ok(payload)
        }
        "dismnext_list_snapshots" => {
            let dir = resolve_snapshot_dir();
            let mgr = SnapshotManager::new(dir.clone());
            let list = mgr.list()?;
            let summaries: Vec<SnapshotSummary> = list.iter().map(snapshot_summary).collect();
            Ok(json!({
                "snapshot_dir": dir.to_string_lossy(),
                "count": summaries.len(),
                "snapshots": summaries
            }))
        }
        "dismnext_system_rescue" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing required string argument 'action'"))?;
            match action {
                "list_restore_points" => {
                    let points = list_restore_points();
                    Ok(json!({
                        "count": points.len(),
                        "restore_points": points
                    }))
                }
                "create_restore_point" => {
                    let desc = args
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("DismNext MCP Restore Point");
                    let message = create_restore_point(desc)?;
                    Ok(json!({
                        "success": true,
                        "message": message
                    }))
                }
                other => {
                    let outcome = run_rescue_action(other)?;
                    Ok(serde_json::to_value(outcome)?)
                }
            }
        }
        "dismnext_list_installed_apps" => {
            let mut apps = list_win32_apps();
            if let Some(target_id) = args
                .get("scan_leftovers_for_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let app = apps
                    .into_iter()
                    .find(|a| a.id == target_id)
                    .ok_or_else(|| anyhow!("Installed application '{}' not found", target_id))?;
                let leftovers =
                    scan_app_leftovers(&app.display_name, &app.publisher, &app.install_location);
                return Ok(json!({
                    "app": app,
                    "leftover_count": leftovers.len(),
                    "leftovers": leftovers
                }));
            }
            if let Some(search) = args
                .get("search")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
            {
                apps.retain(|a| {
                    a.display_name.to_ascii_lowercase().contains(&search)
                        || a.publisher.to_ascii_lowercase().contains(&search)
                });
            }
            Ok(json!({
                "count": apps.len(),
                "apps": apps
            }))
        }
        "dismnext_export_health_report" => {
            let fmt = args
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("json")
                .to_ascii_lowercase();
            let report = generate_health_report(catalog, None);
            if fmt == "markdown" {
                Ok(json!({
                    "health_score": report.health_score,
                    "generated_at": report.generated_at,
                    "markdown": report.markdown
                }))
            } else {
                Ok(serde_json::to_value(report)?)
            }
        }
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

fn resolve_snapshot_dir() -> PathBuf {
    let cwd_dir = SnapshotManager::default_dir();
    if cwd_dir.exists() {
        return cwd_dir;
    }
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let candidate = ancestor.join("data").join("snapshots");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    cwd_dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_catalog() -> Catalog {
        Catalog::load(None).expect("embedded catalog should load")
    }

    #[test]
    fn mcp_initialize_and_notifications() {
        let catalog = test_catalog();
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "clientInfo": { "name": "unit-test", "version": "1.0" }
            }
        });
        let resp = handle_mcp_request(&init_req, &catalog).expect("initialize returns response");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(resp["result"]["serverInfo"]["name"], "dismnext-mcp");

        let notif = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        assert!(handle_mcp_request(&notif, &catalog).is_none());
    }

    #[test]
    fn mcp_tools_list_contains_all_tools_with_valid_schema() {
        let catalog = test_catalog();
        let req = json!({
            "jsonrpc": "2.0",
            "id": "list-1",
            "method": "tools/list",
            "params": {}
        });
        let resp = handle_mcp_request(&req, &catalog).expect("tools/list returns response");
        let tools = resp["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 16);
        for t in tools {
            assert!(t["name"].as_str().unwrap().starts_with("dismnext_"));
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn mcp_tools_call_overview_and_snapshots() {
        let catalog = test_catalog();

        let overview_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "dismnext_system_overview",
                "arguments": {}
            }
        });
        let resp = handle_mcp_request(&overview_req, &catalog).expect("overview response");
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("content text");
        let parsed: Value = serde_json::from_str(text).expect("valid JSON tool payload");
        assert!(parsed["catalog"]["cleanup_rules"].as_u64().unwrap() >= 93);
        assert!(parsed["system"]["system_disk"]["total_bytes"].as_u64().unwrap() > 0);

        let snap_req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "dismnext_list_snapshots",
                "arguments": {}
            }
        });
        let snap_resp = handle_mcp_request(&snap_req, &catalog).expect("snapshots response");
        assert_eq!(snap_resp["result"]["isError"], false);

        let unknown_req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "dismnext_nonexistent_tool",
                "arguments": {}
            }
        });
        let err_resp = handle_mcp_request(&unknown_req, &catalog).expect("error response");
        assert_eq!(err_resp["result"]["isError"], true);
    }
}
