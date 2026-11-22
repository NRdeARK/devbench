use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// RAII exclusive advisory lock for a single work slug (`<works_dir>/<slug>.lock`).
pub struct WorkLock {
    file: File,
    _path: PathBuf,
}

impl WorkLock {
    pub fn acquire(works_dir: &Path, slug: &str) -> Result<Self, String> {
        fs::create_dir_all(works_dir)
            .map_err(|e| format!("Failed to create state dir {}: {e}", works_dir.display()))?;
        let lock_path = works_dir.join(format!("{slug}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("Failed to open lock {}: {e}", lock_path.display()))?;
        file.lock_exclusive()
            .map_err(|e| format!("Failed to lock {}: {e}", lock_path.display()))?;
        Ok(Self {
            file,
            _path: lock_path,
        })
    }
}

impl Drop for WorkLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Write `bytes` atomically via sibling temporary file + fsync + atomic rename.
/// Rust's `std::fs::rename` uses `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` on Windows and
/// `rename(2)` on Unix, so readers never observe a missing or partially written file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid target path: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create dir {}: {e}", parent.display()))?;

    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("state"),
        std::process::id(),
        Uuid::new_v4().simple()
    );
    let tmp_path = parent.join(tmp_name);

    let write_res = (|| -> Result<(), String> {
        let mut f = File::create(&tmp_path)
            .map_err(|e| format!("Failed to create temp file {}: {e}", tmp_path.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("Failed to write temp file {}: {e}", tmp_path.display()))?;
        f.sync_all()
            .map_err(|e| format!("Failed to sync temp file {}: {e}", tmp_path.display()))?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!(
            "Failed to atomically rename {} -> {}: {err}",
            tmp_path.display(),
            path.display()
        ));
    }

    Ok(())
}
