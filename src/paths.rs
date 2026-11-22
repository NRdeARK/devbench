use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Windows reserved DOS device names (case-insensitive, with or without extension).
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Safe path budget below Windows MAX_PATH (260) leaving headroom for nested files inside worktrees.
pub const MAX_SAFE_WORK_DIR_LEN: usize = 200;

/// Resolve the user's home directory across Windows and Unix.
pub fn home_dir() -> Result<PathBuf, String> {
    if let Ok(custom) = env::var("DEVBENCH_HOME") {
        if !custom.trim().is_empty() {
            return Ok(PathBuf::from(custom));
        }
    }
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
    Err("Could not determine home directory (HOME / USERPROFILE not set)".to_string())
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
/// trailing dots/spaces, path traversal, and case-insensitive uniqueness).
pub fn sanitize_slug(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Work slug cannot be empty".to_string());
    }

    let mut out = String::with_capacity(trimmed.len());
    let mut prev_dash = false;
    for ch in trimmed.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if (c == '-' || c == '_' || c == ' ' || c == '.') && !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') || out.ends_with('.') {
        out.pop();
    }

    if out.is_empty() {
        return Err(format!(
            "Work slug '{raw}' contains no valid ASCII alphanumeric characters"
        ));
    }
    if out.len() > 64 {
        return Err(format!(
            "Work slug '{out}' is too long ({} chars; max 64)",
            out.len()
        ));
    }

    let base = out.split('.').next().unwrap_or(&out);
    if WINDOWS_RESERVED.contains(&base) {
        return Err(format!(
            "Work slug '{out}' is a reserved Windows device name; try 'work-{out}' instead"
        ));
    }

    Ok(out)
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

/// Move a directory safely, falling back to recursive copy + remove across volumes/drives.
pub fn move_dir_safe(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create parent dir {}: {e}", parent.display()))?;
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Cross-device link (EXDEV=18 on Unix, ERROR_NOT_SAME_DEVICE=17 on Windows) or similar
            if should_fallback_copy(&err) {
                copy_dir_recursive(src, dst)?;
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
    matches!(err.raw_os_error(), Some(17) | Some(18))
        || err.kind() == io::ErrorKind::CrossesDevices
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

/// Normalize a path string for case-insensitive comparison on Windows (and exact on Unix).
pub fn normalize_for_compare(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let trimmed = s.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}
