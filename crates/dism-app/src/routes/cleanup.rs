use super::{parse_query_param, read_json_body, unknown_endpoint};
use crate::scan_jobs::ScanLane;
use crate::state::AppState;
use anyhow::Result;
use dism_core::cleaner::{
    execute_cleanup, is_deferred_system_task_proc, scan_all_with_summary, scan_custom_proc_name,
    scan_rule_preview, CleanupLease, CleanupReviewUnavailable, CleanupScanResult, ReviewedCleanup,
};
use dism_core::evaluator::eval_condition;
use dism_core::report::{generate_localized_health_report, ReportLanguage};
use dism_core::{Catalog, CustomRuleInput, SystemInfo};
use serde::Deserialize;
use std::{collections::HashMap, time::Instant};
use tiny_http::{Method, Request};

#[derive(Deserialize)]
struct ExecuteReq {
    rule_ids: Vec<String>,
    dry_run: bool,
}

#[derive(Deserialize)]
struct CleanupReviewReq {
    rule_ids: Vec<String>,
}

#[derive(Deserialize)]
struct CleanupPlanReq {
    plan_id: String,
}

#[derive(Deserialize)]
struct DeleteCustomRuleReq {
    rule_id: String,
}

pub(super) fn handle(
    req: &mut Request,
    path: &str,
    query: &str,
    state: &AppState,
) -> Result<serde_json::Value> {
    let sys = SystemInfo::collect();
    match (req.method(), path) {
        (&Method::Get, "/api/cleanup/rules") => {
            let cat = state.catalog.read().unwrap();
            let cache = state.scan_cache.read().unwrap();
            if let Some(scan) = cache.fresh_full_scan(Instant::now()) {
                let list: Vec<_> = scan
                    .items
                    .iter()
                    .map(|rule| serde_json::json!({ "scanned": true, "rule": rule }))
                    .collect();
                return Ok(serde_json::json!({ "items": list, "summary": scan.summary }));
            }
            let list = cleanup_rules_stub_list(&cat, &cache.rules, &sys);
            Ok(serde_json::json!({ "items": list, "summary": null }))
        }
        (&Method::Post, "/api/cleanup/scan") => {
            let started = Instant::now();
            let results = state.scan_jobs.run_inline(ScanLane::CleanupRules, || {
                let rules = state.catalog.read().unwrap().cleanup_rules.clone();
                Ok(scan_all_with_summary(&rules, &sys))
            })?;
            Ok(state.publish_cleanup_scan(results, started))
        }
        (&Method::Get, "/api/cleanup/preview") => {
            let started = Instant::now();
            let id = parse_query_param(query, "id")
                .ok_or_else(|| anyhow::anyhow!("Missing ?id= parameter"))?;
            let rule = state
                .catalog
                .read()
                .unwrap()
                .cleanup_rules
                .iter()
                .find(|r| r.id == id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Rule '{}' not found", id))?;
            let res = scan_rule_preview(&rule, &sys);
            let mut cache = state.scan_cache.write().unwrap();
            if cache.can_store(started) {
                cache.rules.insert(res.id.clone(), res.clone());
            }
            Ok(serde_json::to_value(res)?)
        }
        (&Method::Post, "/api/cleanup/execute") => {
            let payload: ExecuteReq = read_json_body(req)?;
            anyhow::ensure!(payload.dry_run, "Desktop cleanup requires a reviewed plan");
            let rules = state.catalog.read().unwrap().cleanup_rules.clone();
            let report = state.scan_jobs.run_inline(ScanLane::CleanupRules, || {
                Ok(execute_cleanup(&rules, &payload.rule_ids, &sys, true))
            })?;
            Ok(serde_json::json!({
                "report": report,
                "system": sys,
            }))
        }
        (&Method::Post, "/api/cleanup/review") => {
            let payload: CleanupReviewReq = read_json_body(req)?;
            let _lease = CleanupLease::try_acquire()?;
            let rules = state.catalog.read().unwrap().cleanup_rules.clone();
            let (plan, report) = ReviewedCleanup::prepare(&rules, &payload.rule_ids, &sys);
            let mut operation = state.try_cleanup()?;
            let plan_id = (!report.outcomes.is_empty())
                .then(|| operation.reviews.store(plan, Instant::now()));
            Ok(serde_json::json!({ "plan_id": plan_id, "report": report }))
        }
        (&Method::Post, "/api/cleanup/confirm") => {
            let payload: CleanupPlanReq = read_json_body(req)?;
            let lease = CleanupLease::try_acquire()?;
            let plan = {
                let mut operation = state.try_cleanup()?;
                let plan = operation
                    .reviews
                    .take(&payload.plan_id, Instant::now())
                    .ok_or(CleanupReviewUnavailable)?;
                state.invalidate_cleanup(&mut operation);
                plan
            };
            let report = plan.confirm(&lease);
            Ok(serde_json::json!({ "report": report, "system": SystemInfo::collect() }))
        }
        (&Method::Post, "/api/cleanup/discard-review") => {
            let payload: CleanupPlanReq = read_json_body(req)?;
            let mut operation = state.lock_cleanup();
            operation.reviews.take(&payload.plan_id, Instant::now());
            Ok(serde_json::json!({ "ok": true }))
        }
        (&Method::Post, "/api/cleanup/custom/test") => {
            let payload: CustomRuleInput = read_json_body(req)?;
            let rule = Catalog::build_custom_rule(&payload)?;
            let res = scan_rule_preview(&rule, &sys);
            Ok(serde_json::to_value(res)?)
        }
        (&Method::Post, "/api/cleanup/custom/save") => {
            let payload: CustomRuleInput = read_json_body(req)?;
            let mut operation = state.try_cleanup()?;
            if let Some(id) = payload.id.as_deref().map(str::trim) {
                anyhow::ensure!(
                    !state
                        .catalog
                        .read()
                        .unwrap()
                        .cleanup_rules
                        .iter()
                        .any(|rule| rule.id == id && rule.source != "custom"),
                    "Cannot replace a catalog cleanup rule"
                );
            }
            let saved = Catalog::save_custom_rule(payload)?;
            state.invalidate_cleanup(&mut operation);
            let scanned = scan_rule_preview(&saved, &sys);
            let mut cat = state.catalog.write().unwrap();
            if let Some(pos) = cat.cleanup_rules.iter().position(|r| r.id == saved.id) {
                cat.cleanup_rules[pos] = saved.clone();
            } else {
                cat.cleanup_rules.push(saved.clone());
            }
            let mut cache = state.scan_cache.write().unwrap();
            cache.rules.insert(scanned.id.clone(), scanned.clone());
            let items = cleanup_rules_stub_list(&cat, &cache.rules, &sys);
            Ok(serde_json::json!({
                "saved": scanned,
                "items": items,
            }))
        }
        (&Method::Post, "/api/cleanup/custom/delete") => {
            let payload: DeleteCustomRuleReq = read_json_body(req)?;
            let mut operation = state.try_cleanup()?;
            Catalog::delete_custom_rule(&payload.rule_id)?;
            state.invalidate_cleanup(&mut operation);
            let mut cat = state.catalog.write().unwrap();
            cat.cleanup_rules
                .retain(|r| r.id != payload.rule_id || r.source != "custom");
            let mut cache = state.scan_cache.write().unwrap();
            cache.rules.remove(&payload.rule_id);
            let items = cleanup_rules_stub_list(&cat, &cache.rules, &sys);
            Ok(serde_json::json!({ "items": items }))
        }
        (&Method::Get, "/api/report/export") => {
            let language = match parse_query_param(query, "lang").as_deref() {
                None | Some("zh") => ReportLanguage::Chinese,
                Some("en") => ReportLanguage::English,
                Some(_) => anyhow::bail!("Report language must be zh or en"),
            };
            let (cat, scans) = {
                let cat = state.catalog.read().unwrap().clone();
                let cached = state
                    .scan_cache
                    .read()
                    .unwrap()
                    .fresh_full_scan(Instant::now());
                let scans = if let Some(scans) = cached {
                    scans
                } else {
                    let started = Instant::now();
                    let results = state.scan_jobs.run_inline(ScanLane::CleanupRules, || {
                        Ok(scan_all_with_summary(&cat.cleanup_rules, &sys))
                    })?;
                    state
                        .scan_cache
                        .write()
                        .unwrap()
                        .store_full_scan(results, started)
                };
                (cat, scans)
            };
            let report = generate_localized_health_report(&cat, Some(&scans), language);
            Ok(serde_json::to_value(report)?)
        }
        _ => unknown_endpoint(path),
    }
}

pub(crate) fn cleanup_rules_stub_list(
    catalog: &Catalog,
    cache: &HashMap<String, CleanupScanResult>,
    sys: &SystemInfo,
) -> Vec<serde_json::Value> {
    let mut list = Vec::new();
    for rule in &catalog.cleanup_rules {
        if let Some(cached) = cache.get(&rule.id) {
            list.push(serde_json::json!({
                "scanned": true,
                "rule": cached,
            }));
        } else {
            let active_scan = rule
                .scans
                .iter()
                .find(|s| eval_condition(&s.applicable, sys));
            if let Some(scan) = active_scan {
                let system_task_proc = scan_custom_proc_name(scan);
                let is_sys_task = system_task_proc
                    .as_deref()
                    .map(is_deferred_system_task_proc)
                    .unwrap_or(false);
                let stub = CleanupScanResult {
                    id: rule.id.clone(),
                    name: scan
                        .name_override
                        .clone()
                        .unwrap_or_else(|| rule.name.clone()),
                    group: rule.group.clone(),
                    description: scan
                        .description_override
                        .clone()
                        .unwrap_or_else(|| rule.description.clone()),
                    warning: scan
                        .warning_override
                        .clone()
                        .or_else(|| rule.warning.clone()),
                    level: rule.level,
                    source: rule.source.clone(),
                    applicable: true,
                    is_system_task: is_sys_task,
                    system_task_proc,
                    system_task_hint: None,
                    cleanable_bytes: 0,
                    cleanable_count: 0,
                    skipped_locked_bytes: 0,
                    skipped_locked_count: 0,
                    skipped_protected_bytes: 0,
                    skipped_protected_count: 0,
                    preview_files: Vec::new(),
                };
                list.push(serde_json::json!({
                    "scanned": false,
                    "rule": stub,
                }));
            }
        }
    }
    list
}
