use crate::catalog::Condition;
use crate::sysinfo::SystemInfo;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};
use winreg::enums::{
    RegType, HKEY_CLASSES_ROOT, HKEY_CURRENT_CONFIG, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    HKEY_USERS, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
};
use winreg::{RegKey, RegValue, HKEY};

pub fn parse_reg_path(full_key: &str) -> Option<(HKEY, String)> {
    let trimmed = full_key.trim();
    let (hive_str, rest) = match trimmed.find('\\') {
        Some(idx) => (&trimmed[..idx], &trimmed[idx + 1..]),
        None => (trimmed, ""),
    };
    let hive = match hive_str.to_ascii_uppercase().as_str() {
        "HKEY_LOCAL_MACHINE" | "HKLM" => HKEY_LOCAL_MACHINE,
        "HKEY_CURRENT_USER" | "HKCU" => HKEY_CURRENT_USER,
        "HKEY_CLASSES_ROOT" | "HKCR" => HKEY_CLASSES_ROOT,
        "HKEY_USERS" | "HKU" => HKEY_USERS,
        "HKEY_CURRENT_CONFIG" | "HKCC" => HKEY_CURRENT_CONFIG,
        _ => return None,
    };

    let mut sub = rest.trim_start_matches('\\').to_string();
    // Map HKEY_USERS\DEFAULT\... (catalog XML convention) -> HKEY_USERS\.DEFAULT\...
    if hive == HKEY_USERS {
        if sub.eq_ignore_ascii_case("DEFAULT") {
            sub = ".DEFAULT".to_string();
        } else if sub.len() > 8 && sub[..8].eq_ignore_ascii_case("DEFAULT\\") {
            sub = format!(".DEFAULT\\{}", &sub[8..]);
        }
    }
    Some((hive, sub))
}

pub fn wow64_sam_list(wow64: Option<&str>) -> Vec<u32> {
    match wow64.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("true") => vec![KEY_WOW64_32KEY],
        Some("all") => vec![KEY_WOW64_64KEY, KEY_WOW64_32KEY],
        _ => vec![KEY_WOW64_64KEY],
    }
}

pub fn parse_hex_u32(s: &str) -> Option<u32> {
    let t = s
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    u32::from_str_radix(t, 16).ok()
}

pub fn parse_hex_u64(s: &str) -> Option<u64> {
    let t = s
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    u64::from_str_radix(t, 16).ok()
}

pub fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .collect();
    if !cleaned.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for i in (0..cleaned.len()).step_by(2) {
        let b = u8::from_str_radix(&cleaned[i..i + 2], 16).ok()?;
        out.push(b);
    }
    Some(out)
}

pub fn reg_value_to_string(val: &RegValue) -> String {
    if val.bytes.len() < 2 {
        return String::new();
    }
    let u16s: Vec<u16> = val
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&w| w != 0)
        .collect();
    String::from_utf16_lossy(&u16s)
}

pub fn reg_value_to_u32(val: &RegValue) -> Option<u32> {
    if val.bytes.len() >= 4 {
        Some(u32::from_le_bytes([
            val.bytes[0],
            val.bytes[1],
            val.bytes[2],
            val.bytes[3],
        ]))
    } else {
        None
    }
}

pub fn reg_value_to_u64(val: &RegValue) -> Option<u64> {
    if val.bytes.len() >= 8 {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&val.bytes[..8]);
        Some(u64::from_le_bytes(arr))
    } else {
        None
    }
}

/// Expands environment variables (`%SystemRoot%`, `%0%`, `%Root%`, etc.) and `?Func(...)` expressions.
/// Returns `None` if an inline lookup (`?GetRegSz`, `?GetFileVersion`, `?GetIniData`) fails/is empty,
/// preventing dangerous fallback paths like `\..\` or `DwnlData\`.
pub fn expand_expression(input: &str, root_ctx: Option<&str>) -> Option<String> {
    let mut rest = input;
    let mut expanded = String::with_capacity(input.len());
    while let Some(start) = find_inline_func_start(rest) {
        let prefix = &rest[..start];
        expanded.push_str(&expand_env_vars(prefix, root_ctx));
        if prefix.to_ascii_lowercase().ends_with("%root%")
            && root_ctx.is_some()
            && !expanded.ends_with(['\\', '/'])
        {
            expanded.push('\\');
        }
        let (name, body, tail) = split_inline_call(&rest[start..])?;
        let args: Vec<_> = body
            .split(',')
            .map(|arg| expand_env_vars(arg.trim(), root_ctx))
            .collect();
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        let value = eval_inline_func(name, &args)?;
        if value.is_empty() {
            return None;
        }
        expanded.push_str(&value);
        rest = tail;
    }
    expanded.push_str(&expand_env_vars(rest, root_ctx));
    Some(expanded)
}

fn find_inline_func_start(s: &str) -> Option<usize> {
    [
        "?GetRegSz(",
        "?GetFileVersion(",
        "?GetIniData(",
        "?GetNewestChild(",
        "?GetLocaleName(",
        "?ColorDialog(",
    ]
    .iter()
    .filter_map(|prefix| s.find(prefix))
    .min()
}

fn split_inline_call(call: &str) -> Option<(&str, &str, &str)> {
    let call = call.strip_prefix('?')?;
    let (name, body) = call.split_once('(')?;
    let mut depth = 0;
    let close = body.char_indices().find_map(|(index, ch)| match ch {
        '(' => {
            depth += 1;
            None
        }
        ')' if depth == 0 => Some(index),
        ')' => {
            depth -= 1;
            None
        }
        _ => None,
    })?;
    Some((name, &body[..close], &body[close + 1..]))
}

#[cfg(test)]
pub(crate) fn has_balanced_inline_calls(mut value: &str) -> bool {
    while let Some(start) = find_inline_func_start(value) {
        let Some((_, _, tail)) = split_inline_call(&value[start..]) else {
            return false;
        };
        value = tail;
    }
    true
}

fn eval_inline_func(name: &str, args: &[&str]) -> Option<String> {
    match name {
        "GetRegSz" => {
            let key_path = args.first()?;
            let val_name = args.get(1).copied().unwrap_or("");
            let wow64 = args.get(2).copied();
            read_reg_sz(key_path, val_name, wow64)
        }
        "GetFileVersion" => {
            let path = args.first()?;
            read_file_version(Path::new(path))
        }
        "GetIniData" => {
            let path = args.first()?;
            let section = args.get(1)?;
            let key = args.get(2)?;
            read_ini_value(Path::new(path), section, key)
        }
        "GetNewestChild" => {
            let dir = args.first()?;
            newest_version_child(Path::new(dir))
        }
        "GetLocaleName" => Some("zh-Hans".to_string()),
        "ColorDialog" => None,
        _ => None,
    }
}

fn newest_version_child(dir: &Path) -> Option<String> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(Vec<u32>, String)> = None;
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let parts = parse_version_parts(&name);
        if parts.is_empty() {
            continue;
        }
        let is_newer = match &best {
            None => true,
            Some((best_parts, best_name)) => {
                (parts.as_slice(), &name) > (best_parts.as_slice(), best_name)
            }
        };
        if is_newer {
            best = Some((parts, name));
        }
    }
    best.map(|(_, name)| name)
}

pub fn expand_env_vars(input: &str, root_ctx: Option<&str>) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '%') {
                let var: String = chars[i + 1..i + 1 + rel_end].iter().collect();
                if let Some(val) = resolve_var(&var, root_ctx) {
                    out.push_str(&val);
                    // Ensure trailing slash after %Root% when followed directly by a file name
                    if var.eq_ignore_ascii_case("Root") {
                        let next_ch = chars.get(i + 1 + rel_end + 1).copied();
                        if let Some(nc) = next_ch {
                            if nc != '\\'
                                && nc != '/'
                                && !val.ends_with('\\')
                                && !val.ends_with('/')
                            {
                                out.push('\\');
                            }
                        }
                    }
                    i += rel_end + 2;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn resolve_var(name: &str, root_ctx: Option<&str>) -> Option<String> {
    if name.eq_ignore_ascii_case("Root") {
        return root_ctx.map(|s| s.to_string());
    }
    if name == "0" {
        let mut dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
            .unwrap_or_else(|| ".".to_string());
        if !dir.ends_with('\\') {
            dir.push('\\');
        }
        return Some(dir);
    }

    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let user_prof = std::env::var("USERPROFILE").unwrap_or_default();

    let resolved = match name.to_ascii_lowercase().as_str() {
        "systemroot" | "windir" => Some(sys_root),
        "systemdrive" => Some(std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string())),
        "system" => Some(format!(r"{}\System32", sys_root)),
        "systemx86" => Some(format!(r"{}\SysWOW64", sys_root)),
        "programfiles" => std::env::var("ProgramW6432")
            .or_else(|_| std::env::var("ProgramFiles"))
            .ok(),
        "programfilesx86" => std::env::var("ProgramFiles(x86)")
            .or_else(|_| std::env::var("ProgramFiles"))
            .ok(),
        "programdata" => std::env::var("ProgramData").ok(),
        "appdata" => std::env::var("APPDATA").ok(),
        "localappdata" => std::env::var("LOCALAPPDATA").ok(),
        "userprofile" => Some(user_prof),
        "documents" => Some(format!(r"{}\Documents", user_prof)),
        "temp" | "tmp" => std::env::var("TEMP").or_else(|_| std::env::var("TMP")).ok(),
        _ => std::env::var(name).ok(),
    };
    resolved
}

pub fn read_reg_sz(full_key: &str, value_name: &str, wow64: Option<&str>) -> Option<String> {
    let (hive, sub) = parse_reg_path(full_key)?;
    let root = RegKey::predef(hive);
    for sam in wow64_sam_list(wow64) {
        if let Ok(k) = root.open_subkey_with_flags(&sub, KEY_READ | sam) {
            if let Ok(raw) = k.get_raw_value(value_name) {
                let s = reg_value_to_string(&raw).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

static FILE_VERSION_CACHE: OnceLock<RwLock<HashMap<String, Option<String>>>> = OnceLock::new();

pub fn read_file_version(path: &Path) -> Option<String> {
    let cache = FILE_VERSION_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    let key = path.to_string_lossy().to_ascii_lowercase();
    if let Ok(guard) = cache.read() {
        if let Some(hit) = guard.get(&key) {
            return hit.clone();
        }
    }
    let val = read_file_version_uncached(path);
    if let Ok(mut guard) = cache.write() {
        guard.insert(key, val.clone());
    }
    val
}

pub(crate) fn read_file_version_uncached(path: &Path) -> Option<String> {
    let buf = read_version_resource(path)?;
    let sub_block: Vec<u16> = OsStr::new("\\").encode_wide().chain(Some(0)).collect();
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len: u32 = 0;
    let queried = unsafe {
        VerQueryValueW(
            buf.as_ptr() as *const _,
            sub_block.as_ptr(),
            &mut ptr,
            &mut len,
        )
    };
    if queried == 0 || ptr.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }
    let info = unsafe { &*(ptr as *const VS_FIXEDFILEINFO) };
    let major = (info.dwFileVersionMS >> 16) & 0xffff;
    let minor = info.dwFileVersionMS & 0xffff;
    let build = (info.dwFileVersionLS >> 16) & 0xffff;
    let rev = info.dwFileVersionLS & 0xffff;
    Some(format!("{}.{}.{}.{}", major, minor, build, rev))
}

pub(crate) fn read_version_resource(path: &Path) -> Option<Vec<u8>> {
    if !path.exists() {
        return None;
    }
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut reserved = 0;
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), &mut reserved) };
    if size == 0 {
        return None;
    }
    let mut resource = vec![0; size as usize];
    let ok = unsafe { GetFileVersionInfoW(wide.as_ptr(), 0, size, resource.as_mut_ptr().cast()) };
    (ok != 0).then_some(resource)
}

pub fn read_ini_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };

    let target_sec = section.trim().to_ascii_lowercase();
    let target_key = key.trim().to_ascii_lowercase();
    let mut in_sec = false;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with(';') || l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') && l.ends_with(']') {
            let sec = l[1..l.len() - 1].trim().to_ascii_lowercase();
            in_sec = sec == target_sec;
            continue;
        }
        if in_sec {
            if let Some(eq) = l.find('=') {
                let k = l[..eq].trim().to_ascii_lowercase();
                if k == target_key {
                    return Some(l[eq + 1..].trim().to_string());
                }
            }
        }
    }
    None
}

/// Evaluates an `Applicable` condition tree against the live online Windows system.
pub fn eval_condition(cond: &Condition, sys: &SystemInfo) -> bool {
    match cond {
        Condition::True => true,
        Condition::False => false,
        Condition::And { children } => children.iter().all(|c| eval_condition(c, sys)),
        Condition::Or { children } => children.iter().any(|c| eval_condition(c, sys)),
        Condition::Not { children } => !children.iter().all(|c| eval_condition(c, sys)),
        Condition::ImageFlags { mask } => (1 & mask) == *mask,
        Condition::Arch { arch } => sys.arch_code == *arch,
        Condition::OSVersion { compare, version } => {
            compare_version_str(&sys.version_tuple(), version, compare)
        }
        Condition::FileExist { file_path, wow64 } => {
            let Some(expanded) = expand_expression(file_path, None) else {
                return false;
            };
            if path_or_wildcard_exists(&expanded, &sys.fixed_drives) {
                return true;
            }
            if matches!(wow64.as_deref(), Some("True") | Some("All")) {
                if let Some(alt) = wow64_path_variant(&expanded) {
                    if path_or_wildcard_exists(&alt, &sys.fixed_drives) {
                        return true;
                    }
                }
            }
            false
        }
        Condition::FileVersion {
            file_path,
            compare,
            version,
        } => {
            let Some(expanded) = expand_expression(file_path, None) else {
                return false;
            };
            let Some(actual_str) = read_file_version(Path::new(&expanded)) else {
                return false;
            };
            let actual = parse_version_parts(&actual_str);
            compare_version_parts(&actual, version, compare)
        }
        Condition::QueryServiceStart { name, start_type } => {
            let sub = format!(r"SYSTEM\CurrentControlSet\Services\{}", name);
            let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
            if let Ok(k) = hklm.open_subkey_with_flags(&sub, KEY_READ) {
                if let Ok(val) = k.get_value::<u32, _>("Start") {
                    return val == *start_type;
                }
            }
            false
        }
        Condition::RegExist {
            key,
            value,
            reg_type,
            data,
            compare,
            wow64,
        } => eval_reg_exist(
            key,
            value.as_deref(),
            reg_type.as_deref(),
            data.as_deref(),
            compare.as_deref(),
            wow64.as_deref(),
        ),
    }
}

fn parse_version_parts(s: &str) -> Vec<u32> {
    s.trim()
        .split('.')
        .filter_map(|p| p.trim().parse::<u32>().ok())
        .collect()
}

fn compare_version_str(actual_4: &[u32; 4], target_str: &str, op: &str) -> bool {
    compare_version_parts(actual_4, target_str, op)
}

fn compare_version_parts(actual: &[u32], target_str: &str, op: &str) -> bool {
    let target = parse_version_parts(target_str);
    if target.is_empty() {
        return false;
    }
    let n = target.len().min(actual.len());
    let lhs = &actual[..n];
    let rhs = &target[..n];
    match op.trim() {
        "==" | "=" => lhs == rhs,
        "!=" => lhs != rhs,
        "<" => lhs < rhs,
        "<=" => lhs <= rhs,
        ">" => lhs > rhs,
        ">=" => lhs >= rhs,
        _ => false,
    }
}

fn wow64_path_variant(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    if let (Ok(pf64), Ok(pf86)) = (
        std::env::var("ProgramW6432"),
        std::env::var("ProgramFiles(x86)"),
    ) {
        let pf64_l = pf64.to_ascii_lowercase();
        if lower.starts_with(&pf64_l) {
            return Some(format!("{}{}", pf86, &path[pf64.len()..]));
        }
    }
    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let sys32 = format!(r"{}\system32", sys_root).to_ascii_lowercase();
    if lower.starts_with(&sys32) {
        return Some(format!(r"{}\SysWOW64{}", sys_root, &path[sys32.len()..]));
    }
    None
}

fn path_or_wildcard_exists(path: &str, fixed_drives: &[String]) -> bool {
    if !path.contains('*') && !path.contains('?') {
        return Path::new(path).exists();
    }
    !resolve_wildcard_roots(path, fixed_drives).is_empty()
}

/// Expands a `RootPath` pattern (supporting leading `*\` for fixed drives and intermediate `*`/`?` segments).
pub fn resolve_wildcard_roots(pattern: &str, fixed_drives: &[String]) -> Vec<PathBuf> {
    resolve_wildcard_roots_until(pattern, fixed_drives, &|| false)
}

pub(crate) fn resolve_wildcard_roots_until(
    pattern: &str,
    fixed_drives: &[String],
    cancelled: &impl Fn() -> bool,
) -> Vec<PathBuf> {
    if cancelled() {
        return Vec::new();
    }
    let norm = pattern.replace('/', "\\");
    let trimmed = norm.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // Leading `*\...` means iterate every fixed drive root.
    if let Some(rest) = trimmed.strip_prefix(r"*\") {
        let mut results = Vec::new();
        for drv in fixed_drives {
            let candidate = format!("{}{}", drv, rest);
            results.extend(resolve_wildcard_roots_until(
                &candidate,
                fixed_drives,
                cancelled,
            ));
        }
        return results;
    }

    if !trimmed.contains('*') && !trimmed.contains('?') {
        let p = PathBuf::from(trimmed);
        return if p.exists() { vec![p] } else { Vec::new() };
    }

    // Split into prefix (no wildcards) + remaining segments
    let parts: Vec<&str> = trimmed.split('\\').collect();
    let mut base_parts = Vec::new();
    let mut idx = 0;
    while idx < parts.len() {
        if parts[idx].contains('*') || parts[idx].contains('?') {
            break;
        }
        base_parts.push(parts[idx]);
        idx += 1;
    }

    let mut current: Vec<PathBuf> = if base_parts.is_empty() {
        Vec::new()
    } else {
        let mut base = base_parts.join("\\");
        // Drive root like "C:" needs trailing backslash
        if base.len() == 2 && base.ends_with(':') {
            base.push('\\');
        }
        let pb = PathBuf::from(base);
        if pb.exists() {
            vec![pb]
        } else {
            Vec::new()
        }
    };

    for seg in &parts[idx..] {
        if seg.is_empty() {
            continue;
        }
        let mut next = Vec::new();
        if *seg == "." {
            continue;
        }
        if *seg == ".." {
            for p in current {
                if let Some(parent) = p.parent() {
                    next.push(parent.to_path_buf());
                }
            }
            current = next;
            continue;
        }
        let has_wild = seg.contains('*') || seg.contains('?');
        for dir in current {
            if cancelled() {
                return Vec::new();
            }
            if !has_wild {
                let candidate = dir.join(seg);
                if candidate.exists() {
                    next.push(candidate);
                }
            } else if let Ok(rd) = std::fs::read_dir(&dir) {
                for entry in rd.flatten() {
                    if cancelled() {
                        return Vec::new();
                    }
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if wildcard_match_ci(seg, &name_str) {
                        next.push(entry.path());
                    }
                }
            }
        }
        current = next;
    }

    current
}

pub fn wildcard_match_ci(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn eval_reg_exist(
    key: &str,
    value: Option<&str>,
    reg_type: Option<&str>,
    data: Option<&str>,
    compare: Option<&str>,
    wow64: Option<&str>,
) -> bool {
    let Some((hive, sub)) = parse_reg_path(key) else {
        return false;
    };
    let root = RegKey::predef(hive);
    for sam in wow64_sam_list(wow64) {
        let Ok(k) = root.open_subkey_with_flags(&sub, KEY_READ | sam) else {
            continue;
        };
        let Some(val_name) = value else {
            // Key-only existence check
            return true;
        };
        let Ok(raw) = k.get_raw_value(val_name) else {
            continue;
        };

        if let Some(expected_type) = reg_type {
            if !reg_type_matches(&raw.vtype, expected_type) {
                continue;
            }
        }

        let Some(expected_data) = data else {
            return true;
        };

        if match_reg_data(&raw, expected_data, compare) {
            return true;
        }
    }
    false
}

fn reg_type_matches(actual: &RegType, expected: &str) -> bool {
    match expected.trim().to_ascii_uppercase().as_str() {
        "REG_SZ" => matches!(actual, RegType::REG_SZ | RegType::REG_EXPAND_SZ),
        "REG_EXPAND_SZ" => matches!(actual, RegType::REG_EXPAND_SZ | RegType::REG_SZ),
        "REG_DWORD" => matches!(actual, RegType::REG_DWORD),
        "REG_QWORD" => matches!(actual, RegType::REG_QWORD),
        "REG_BINARY" => matches!(actual, RegType::REG_BINARY),
        "REG_MULTI_SZ" => matches!(actual, RegType::REG_MULTI_SZ),
        _ => false,
    }
}

fn match_reg_data(raw: &RegValue, expected: &str, compare: Option<&str>) -> bool {
    match raw.vtype {
        RegType::REG_DWORD => {
            let Some(actual_v) = reg_value_to_u32(raw) else {
                return false;
            };
            let Some(exp_v) = parse_hex_u32(expected) else {
                return false;
            };
            compare_u64(actual_v as u64, exp_v as u64, compare)
        }
        RegType::REG_QWORD => {
            let Some(actual_v) = reg_value_to_u64(raw) else {
                return false;
            };
            let Some(exp_v) = parse_hex_u64(expected) else {
                return false;
            };
            compare_u64(actual_v, exp_v, compare)
        }
        RegType::REG_BINARY => {
            let Some(exp_bytes) = parse_hex_bytes(expected) else {
                return false;
            };
            match compare.unwrap_or("==").trim() {
                "==" | "=" => raw.bytes == exp_bytes,
                "!=" => raw.bytes != exp_bytes,
                _ => false,
            }
        }
        RegType::REG_SZ | RegType::REG_EXPAND_SZ => {
            let actual_s = reg_value_to_string(raw);
            compare_str(&actual_s, expected, compare)
        }
        _ => false,
    }
}

fn compare_u64(actual: u64, expected: u64, compare: Option<&str>) -> bool {
    match compare.unwrap_or("==").trim() {
        "==" | "=" => actual == expected,
        "!=" => actual != expected,
        ">=" => actual >= expected,
        "<=" => actual <= expected,
        ">" => actual > expected,
        "<" => actual < expected,
        "&" => (actual & expected) == expected,
        _ => false,
    }
}

fn compare_str(actual: &str, expected: &str, compare: Option<&str>) -> bool {
    let mut op = compare.unwrap_or("==").trim();
    let mut prefix_n: Option<usize> = None;
    let mut case_sensitive = false;

    if let Some(rest) = op.strip_prefix('N') {
        prefix_n = Some(expected.chars().count());
        op = rest;
    }
    if let Some(rest) = op.strip_prefix('I') {
        case_sensitive = true;
        op = rest;
    }

    let a_cmp: String = match prefix_n {
        Some(n) => actual.chars().take(n).collect(),
        None => actual.to_string(),
    };
    let e_cmp = expected;

    let ord = if case_sensitive {
        a_cmp.as_str().cmp(e_cmp)
    } else {
        a_cmp.to_lowercase().cmp(&e_cmp.to_lowercase())
    };

    match op {
        "==" | "=" | "" => ord.is_eq(),
        "!=" => ord.is_ne(),
        ">=" => ord.is_ge(),
        "<=" => ord.is_le(),
        ">" => ord.is_gt(),
        "<" => ord.is_lt(),
        "&" => {
            if case_sensitive {
                a_cmp.contains(e_cmp)
            } else {
                a_cmp.to_lowercase().contains(&e_cmp.to_lowercase())
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_dword_and_default_hive() {
        assert_eq!(parse_hex_u32("A"), Some(10));
        assert_eq!(parse_hex_u32("100000"), Some(0x10_0000));
        assert_eq!(parse_hex_u32("FFEFFFFF"), Some(0xFFEFFFFF));
        assert_eq!(
            parse_reg_path(r"HKEY_USERS\DEFAULT\Software\Test"),
            Some((HKEY_USERS, r".DEFAULT\Software\Test".to_string()))
        );
    }

    #[test]
    fn test_version_compare() {
        let win11 = [10, 0, 26100, 1000];
        assert!(compare_version_str(&win11, "10.0", "=="));
        assert!(compare_version_str(&win11, "10.0.22000", ">="));
        assert!(!compare_version_str(&win11, "10.0", "<"));
        assert!(!compare_version_str(&win11, "6.3", "<="));
    }

    #[test]
    fn test_inline_get_file_version_and_wildcard() {
        let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
        let notepad = format!(r"{}\System32\notepad.exe", sys_root);
        if Path::new(&notepad).exists() {
            let ver = expand_expression("?GetFileVersion(%System%\\notepad.exe)", None);
            assert!(ver.is_some());
            assert!(ver.unwrap().contains('.'));
        }
        assert!(wildcard_match_ci("*.dmp", "Crash123.DMP"));
        assert!(!wildcard_match_ci("*.dmp", "Crash123.log"));
    }

    #[test]
    fn inline_lookup_accepts_parentheses_and_commas_in_paths() {
        let root = std::env::temp_dir().join(format!(
            "dismnext-inline-{} (x86), test)",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("4.2")).unwrap();
        std::fs::write(root.join("config.ini"), "[Common]\nVersion=4.2\n").unwrap();
        let context = root.to_str().unwrap();
        assert_eq!(
            expand_expression("?GetNewestChild(%Root%)", Some(context)).as_deref(),
            Some("4.2")
        );
        assert_eq!(
            expand_expression(
                "?GetIniData(%Root%config.ini,Common,Version)",
                Some(context)
            )
            .as_deref(),
            Some("4.2")
        );
        std::fs::remove_dir(root.join("4.2")).unwrap();
        std::fs::remove_file(root.join("config.ini")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn test_compare_str_n_prefix_and_case_insensitive() {
        assert!(compare_str("11.0.19041.1", "11.", Some("N==")));
        assert!(!compare_str("11.0.19041.1", "8.", Some("N==")));
        assert!(compare_str("Off", "off", None));
    }

    #[test]
    fn test_get_newest_child_picks_highest_semantic_version() {
        let base =
            std::env::temp_dir().join(format!("dismnext_newest_child_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for ver in ["2.1.90", "2.1.286", "2.1.299", "not-a-version"] {
            std::fs::create_dir_all(base.join(ver)).unwrap();
        }
        let expr = format!("?GetNewestChild({})", base.display());
        assert_eq!(expand_expression(&expr, None), Some("2.1.299".to_string()));
        let _ = std::fs::remove_dir_all(&base);
    }
}
