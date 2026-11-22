use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Windows reserved DOS device names (case-insensitive, with or without extension).
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Safe path budget below Windows MAX_PATH (260) leaving headroom for nested files inside worktrees.
pub const MAX_SAFE_WORK_DIR_LEN: usize = 200;

/// Resolve the user's home directory across Windows and Unix.
/// On Windows, `USERPROFILE` takes precedence over `HOME` so PowerShell and Git Bash agree.
pub fn home_dir() -> Result<PathBuf, String> {
    if let Ok(custom) = env::var("DEVBENCH_HOME") {
        if !custom.trim().is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
    #[cfg(windows)]
    {
        if let Ok(profile) = env::var("USERPROFILE") {
            if !profile.trim().is_empty() {
                return Ok(PathBuf::from(profile));
            }
        }
        if let Ok(home) = env::var("HOME") {
            if !home.trim().is_empty() {
                return Ok(PathBuf::from(home));
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = env::var("HOME") {
            if !home.trim().is_empty() {
                return Ok(PathBuf::from(home));
            }
        }
        if let Ok(profile) = env::var("USERPROFILE") {
            if !profile.trim().is_empty() {
                return Ok(PathBuf::from(profile));
            }
        }
    }
    Err("Could not determine home directory (USERPROFILE / HOME not set)".to_string())
}

/// Default devbench root directory (~/.devbench).
pub fn devbench_dir() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".devbench"))
}

/// Expand leading `~` or `~/` / `~\` against the user's home directory.
pub fn expand_tilde(raw: &str) -> Result<PathBuf, String> {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return Ok(home_dir()?.join(rest));
    }
    Ok(PathBuf::from(raw))
}

/// Validate and sanitize a work slug for cross-platform safety (Windows reserved names,
/// trailing dots/spaces, path separators, and case-insensitive uniqueness).
pub fn sanitize_slug(raw: &str) -> Result<String, String> {
    sanitize_identifier("Work slug", raw)
}

/// Validate and sanitize a named identifier (slug, domain, or repo directory segment).
pub fn sanitize_identifier(kind: &str, raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{kind} cannot be empty"));
    }

    // Check Windows reserved device names BEFORE dot-to-hyphen conversion so `CON.txt` is caught too.
    let raw_stem = trimmed
        .split('.')
        .next()
        .unwrap_or(trimmed)
        .trim()
        .to_ascii_lowercase();
    if WINDOWS_RESERVED.contains(&raw_stem.as_str()) {
        return Err(format!(
            "{kind} '{trimmed}' uses a reserved Windows device name; try 'work-{raw_stem}' instead"
        ));
    }

    let mut out = String::with_capacity(trimmed.len());
    let mut prev_dash = false;
    for ch in trimmed.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if matches!(c, '-' | '_' | ' ' | '.' | '/' | '\\') && !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        return Err(format!(
            "{kind} '{raw}' contains no valid ASCII alphanumeric characters"
        ));
    }
    if out.len() > 64 {
        return Err(format!(
            "{kind} '{out}' is too long ({} chars; max 64)",
            out.len()
        ));
    }
    if WINDOWS_RESERVED.contains(&out.as_str()) {
        return Err(format!(
            "{kind} '{out}' is a reserved Windows device name; try 'work-{out}' instead"
        ));
    }

    Ok(out)
}

/// Validate that a configured repo alias is safe to join as a single child directory name.
pub fn validate_repo_alias(alias: &str) -> Result<(), String> {
    if alias.trim().is_empty() {
        return Err("Repo alias cannot be empty".to_string());
    }
    if alias != alias.trim()
        || alias.contains('/')
        || alias.contains('\\')
        || alias.contains("..")
        || alias.contains(':')
    {
        return Err(format!(
            "Unsafe repo alias '{alias}': must be a single relative directory name"
        ));
    }
    let stem = alias.split('.').next().unwrap_or(alias).to_ascii_lowercase();
    if WINDOWS_RESERVED.contains(&stem.as_str()) {
        return Err(format!(
            "Unsafe repo alias '{alias}': reserved Windows device name"
        ));
    }
    Ok(())
}

/// Build an unambiguous, collision-resistant `hc run --name` (1–32 chars of `[a-z0-9-_]`).
/// Uses FNV-1a disambiguation when needed so `a-b`+`c` never collides with `a`+`b-c`,
/// and enforces `hc`'s 32-character limit even for 64-char slugs.
pub fn build_hc_name(prefix: &str, slug: &str, domain: Option<&str>) -> String {
    let clean_prefix: String = prefix
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(6)
        .collect();
    let pfx = if clean_prefix.is_empty() {
        "db".to_string()
    } else {
        clean_prefix
    };

    // Unambiguous role tag + short deterministic 6-hex hash of (slug, role)
    let (role_tag, hash_input) = match domain {
        None => ("o".to_string(), format!("{slug}\0owner")),
        Some(dom) => (format!("w_{dom}"), format!("{slug}\0worker\0{dom}")),
    };
    let hash = fnv1a_32_hex(&hash_input);

    // Format: <pfx>-<slug_part>-<role_part>-<6hex> <= 32 chars
    // Reserve: pfx + 1 + 1 + 1 + 6 = pfx.len() + 9
    let budget = 32usize.saturating_sub(pfx.len() + 9);
    let role_budget = role_tag.len().min(budget / 2).max(1);
    let slug_budget = budget.saturating_sub(role_budget).max(1);
    let slug_part: String = slug.chars().take(slug_budget).collect();
    let role_part: String = role_tag.chars().take(role_budget).collect();
    let slug_part = slug_part.trim_end_matches('-');
    let role_part = role_part.trim_end_matches(['-', '_']);

    format!("{pfx}-{slug_part}-{role_part}-{hash}")
}

fn fnv1a_32_hex(input: &str) -> String {
    let mut h: u32 = 0x811c9dc5;
    for b in input.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    format!("{:06x}", h & 0x00ff_ffff)
}

/// Guard against Windows MAX_PATH issues before creating a worktree folder.
pub fn check_path_length(path: &Path) -> Result<(), String> {
    let len = path.to_string_lossy().chars().count();
    if len > MAX_SAFE_WORK_DIR_LEN {
        return Err(format!(
            "Path is too long ({len} chars > {MAX_SAFE_WORK_DIR_LEN} safe limit): {}. Use a shorter work_root or slug.",
            path.display()
        ));
    }
    Ok(())
}

/// Move a directory safely, falling back to staged recursive copy + atomic rename + remove
/// across volumes/drives so interrupted copies never leave a half-populated `dst`.
pub fn move_dir_safe(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.is_dir() {
        return Err(format!(
            "Source directory does not exist: {}",
            src.display()
        ));
    }
    if dst.exists() {
        return Err(format!(
            "Destination archive directory already exists: {}",
            dst.display()
        ));
    }
    let parent = dst
        .parent()
        .ok_or_else(|| format!("Invalid destination path: {}", dst.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create parent dir {}: {e}", parent.display()))?;

    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) => {
            if should_fallback_copy(&err) {
                let stage = parent.join(format!(
                    ".devbench-stage-{}-{}",
                    std::process::id(),
                    Uuid::new_v4().simple()
                ));
                if let Err(copy_err) = copy_dir_recursive(src, &stage) {
                    let _ = fs::remove_dir_all(&stage);
                    return Err(copy_err);
                }
                if let Err(ren_err) = fs::rename(&stage, dst) {
                    let _ = fs::remove_dir_all(&stage);
                    return Err(format!(
                        "Failed to finalize staged archive {} -> {}: {ren_err}",
                        stage.display(),
                        dst.display()
                    ));
                }
                fs::remove_dir_all(src).map_err(|e| {
                    format!(
                        "Copied {} to {} but failed to remove source dir: {e}",
                        src.display(),
                        dst.display()
                    )
                })?;
                Ok(())
            } else {
                Err(format!(
                    "Failed to move {} to {}: {err}",
                    src.display(),
                    dst.display()
                ))
            }
        }
    }
}

fn should_fallback_copy(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE = 17
        matches!(err.raw_os_error(), Some(17))
    }
    #[cfg(unix)]
    {
        // EXDEV = 18 (note: 17 on Unix is EEXIST, NOT cross-device!)
        matches!(err.raw_os_error(), Some(18))
    }
    #[cfg(not(any(windows, unix)))]
    {
        false
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create {}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("Failed to read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("Failed to read dir entry: {e}"))?;
        let ty = entry
            .file_type()
            .map_err(|e| format!("Failed to stat {}: {e}", entry.path().display()))?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|e| {
                format!(
                    "Failed to copy {} -> {}: {e}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Normalize a path string for cross-platform comparison (strips Windows `\\?\` prefix).
pub fn normalize_for_compare(p: &Path) -> String {
    let raw = p.to_string_lossy();
    let stripped = raw
        .strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
        .unwrap_or(&raw);
    let slash = stripped.replace('\\', "/");
    let trimmed = slash.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}
