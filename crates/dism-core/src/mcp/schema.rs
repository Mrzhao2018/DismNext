use serde_json::{json, Value};

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
            "description": "Simulate cleanup for the selected rules. To modify files, review with dismnext_review_cleanup and obtain explicit user confirmation before dismnext_confirm_cleanup.",
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
                        "enum": [true],
                        "description": "The legacy dry_run field must be true when supplied."
                    }
                },
                "required": ["rule_ids"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_review_cleanup",
            "description": "Capture targets for a cleanup review and return a five-minute plan ID with the expected effects. Present the report to the user and request explicit confirmation.",
            "inputSchema": {
                "type": "object",
                "properties": { "rule_ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 } },
                "required": ["rule_ids"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_confirm_cleanup",
            "description": "Apply exactly the reviewed targets using a single-use plan ID. Call only after the user explicitly confirms the corresponding cleanup report.",
            "inputSchema": {
                "type": "object",
                "properties": { "plan_id": { "type": "string" } },
                "required": ["plan_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_discard_cleanup_review",
            "description": "Discard a pending cleanup plan without making changes.",
            "inputSchema": {
                "type": "object",
                "properties": { "plan_id": { "type": "string" } },
                "required": ["plan_id"],
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
                    },
                    "measure_allocation": {
                        "type": "boolean",
                        "description": "Also measure allocated file data on NTFS and count hard links once (default false)."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "dismnext_find_duplicates",
            "description": "Three-stage parallel duplicate file detector (size grouping, hard-link deduplication, partial fingerprint, full BLAKE3 verification) returning duplicate groups and potential wasted bytes.",
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
