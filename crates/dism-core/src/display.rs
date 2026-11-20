/// Formats logical bytes with 1024-based units, using the CLI and report display convention.
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    for (threshold, unit, precision) in [(TB, "TB", 2), (GB, "GB", 2), (MB, "MB", 2), (KB, "KB", 1)]
    {
        if bytes >= threshold {
            return format!("{:.*} {unit}", precision, bytes as f64 / threshold as f64);
        }
    }
    format!("{bytes} B")
}
