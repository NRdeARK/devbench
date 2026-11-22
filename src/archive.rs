use crate::config::Config;
use crate::herdr::{current_hc_terminal_id, HerdrClient};
use crate::paths::{expand_tilde, move_dir_safe};
use crate::state::{
    load_work, save_work, write_frozen_snapshot, ArchivePhase, RepoStatus, SessionStatus,
    WorkStatus,
};
use crate::worktree::{collect_git_summary, detach_worktree, inspect_worktree, snapshot_wip};
use chrono::Utc;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveReport {
    pub slug: String,
    pub archive_path: String,
    pub kept_branches: Vec<String>,
    pub warnings: Vec<String>,
    pub snapshot_path: String,
}

/// Safely archive a completed work following D6 + UC-05:
/// 1. Self-archive guard
/// 2. Dirty/untracked/submodule guard (block unless `--snapshot-wip`), soft-warn unpushed & ignored
/// 3. Phase 1 (`herdr_closed`): close 1:1 Herdr space + kill/rm ONLY recorded hc terminals FIRST
/// 4. Phase 2 (`worktrees_detached`): capture git summary + final SHA, `git worktree remove` (no --force) + `prune`
/// 5. Phase 3 (`snapshot_written`): write frozen `state.snapshot.json` + `git_summary.md`
/// 6. Phase 4 (`moved`): move folder to `<archive_root>/<slug>` (timestamp suffix on collision)
pub fn archive_work(
    cfg: &Config,
    slug: &str,
    do_snapshot_wip: bool,
    allow_self_archive: bool,
) -> Result<ArchiveReport, String> {
    let mut state = load_work(slug)?;
    if state.status == WorkStatus::Archived && state.archive_phase == Some(ArchivePhase::Moved) {
        return Err(format!(
            "Work '{slug}' is already archived at {}",
            state.archive_path.as_deref().unwrap_or("(unknown)")
        ));
    }

    // Edge case: invoked from inside the work's own Herdr workspace
    if !allow_self_archive {
        if let Some(caller_hc) = current_hc_terminal_id() {
            let inside_self = state
                .sessions
                .iter()
                .any(|s| s.hc_terminal_id.as_deref() == Some(caller_hc.as_str()));
            if inside_self {
                return Err(format!(
                    "Refusing to close workspace while invoked from inside this work's own terminal ({caller_hc}). \
                     Run `devbench archive {slug}` from the Dispatcher outside this workspace, or pass --allow-self-archive."
                ));
            }
        }
    }

    let mut warnings = Vec::new();
    let phase_rank = archive_phase_rank(&state.archive_phase);

    // Step 2: Inspect dirty / untracked / unpushed / ignored before closing Herdr or mutating anything
    if phase_rank < 2 {
        let mut dirty_blockers = Vec::new();
        for repo in &state.repos {
            if repo.status == RepoStatus::Removed {
                continue;
            }
            let info = inspect_worktree(repo)?;
            if info.has_submodules {
                dirty_blockers.push(format!(
                    "{}: contains git submodules (.gitmodules) — inspect manually before archiving",
                    info.alias
                ));
            }
            if !info.dirty_paths.is_empty() {
                if do_snapshot_wip {
                    let sha = snapshot_wip(repo, slug)?;
                    warnings.push(format!(
                        "{}: committed WIP snapshot {} on branch {}",
                        repo.alias, sha, repo.branch
                    ));
                } else {
                    dirty_blockers.push(format!(
                        "{} ({}): {}",
                        info.alias,
                        info.path.display(),
                        info.dirty_paths.join(", ")
                    ));
                }
            }
            if cfg.archive.warn_unpushed && info.unpushed_commits > 0 {
                warnings.push(format!(
                    "{}: {} unpushed commit(s) remain safely on local branch '{}'",
                    info.alias, info.unpushed_commits, repo.branch
                ));
            }
            if !info.ignored_paths.is_empty() {
                warnings.push(format!(
                    "{}: {} gitignored file(s) will be deleted on worktree remove ({})",
                    info.alias,
                    info.ignored_paths.len(),
                    info.ignored_paths.join(", ")
                ));
            }
        }

        if !dirty_blockers.is_empty() {
            return Err(format!(
                "Archive blocked due to dirty/untracked worktrees (Herdr left open):\n- {}\nRe-run with --snapshot-wip to commit WIP onto task branches, or clean/commit manually.",
                dirty_blockers.join("\n- ")
            ));
        }
    }

    state.status = WorkStatus::Archiving;
    save_work(&mut state)?;

    // Phase 1: Close 1:1 Herdr workspace & kill ONLY recorded terminals FIRST (releases Windows file locks)
    if phase_rank < 1 {
        let herdr = HerdrClient::from_config(&cfg.herdr);
        if let Some(space_id) = state.herdr_space_id.clone() {
            if let Err(e) = herdr.space_close(&space_id) {
                warnings.push(format!(
                    "Herdr space '{space_id}' already closed or unreachable (tolerated): {e}"
                ));
            }
        }
        for s in &mut state.sessions {
            if let Some(tid) = s.hc_terminal_id.as_deref() {
                herdr.hc_kill_and_rm(tid);
            }
            s.status = SessionStatus::Archived;
        }
        state.herdr_space_id = None;
        state.archive_phase = Some(ArchivePhase::HerdrClosed);
        save_work(&mut state)?;
    }

    // Phase 2: Collect git summary + detach clean worktrees (NO --force) + prune
    let work_dir = PathBuf::from(&state.work_dir);
    if phase_rank < 2 {
        let mut summary_doc = format!("# Archive Git Summary — {slug}\n\n");
        for repo in &mut state.repos {
            if repo.status == RepoStatus::Removed {
                continue;
            }
            let (head_sha, md) = collect_git_summary(repo);
            summary_doc.push_str(&md);
            summary_doc.push('\n');
            repo.final_head_sha = head_sha;
            detach_worktree(repo).map_err(|e| {
                format!(
                    "Failed to detach worktree '{}' (state checkpointed at herdr_closed; rerun `devbench archive {slug}` after releasing any Windows lock): {e}",
                    repo.alias
                )
            })?;
            repo.status = RepoStatus::Removed;
        }
        if work_dir.is_dir() {
            let _ = fs::write(work_dir.join("git_summary.md"), summary_doc);
        }
        state.archive_phase = Some(ArchivePhase::WorktreesDetached);
        save_work(&mut state)?;
    }

    // Compute collision-safe archive destination
    let archive_root = expand_tilde(&cfg.archive_root)?;
    let target_dir = collision_safe_archive_dir(&archive_root, slug);

    // Phase 3: Write frozen snapshot into work folder
    if phase_rank < 3 {
        state.status = WorkStatus::Archived;
        state.archive_path = Some(target_dir.to_string_lossy().to_string());
        state.archive_phase = Some(ArchivePhase::SnapshotWritten);
        if work_dir.is_dir() {
            write_frozen_snapshot(&work_dir, &state)?;
        }
        save_work(&mut state)?;
    }

    // Phase 4: Move folder to archive_root (safe across drives)
    let final_dir = PathBuf::from(
        state
            .archive_path
            .clone()
            .unwrap_or_else(|| target_dir.to_string_lossy().to_string()),
    );
    if phase_rank < 4 {
        if work_dir.is_dir() {
            move_dir_safe(&work_dir, &final_dir)?;
        }
        state.status = WorkStatus::Archived;
        state.archive_phase = Some(ArchivePhase::Moved);
        save_work(&mut state)?;
    }

    let kept_branches = state
        .repos
        .iter()
        .map(|r| {
            format!(
                "{}:{}@{}",
                r.alias,
                r.branch,
                r.final_head_sha
                    .as_deref()
                    .map(|s| &s[..s.len().min(7)])
                    .unwrap_or("unknown")
            )
        })
        .collect();

    Ok(ArchiveReport {
        slug: state.slug,
        archive_path: final_dir.to_string_lossy().to_string(),
        kept_branches,
        warnings,
        snapshot_path: final_dir
            .join("state.snapshot.json")
            .to_string_lossy()
            .to_string(),
    })
}

fn archive_phase_rank(phase: &Option<ArchivePhase>) -> u8 {
    match phase {
        None => 0,
        Some(ArchivePhase::HerdrClosed) => 1,
        Some(ArchivePhase::WorktreesDetached) => 2,
        Some(ArchivePhase::SnapshotWritten) => 3,
        Some(ArchivePhase::Moved) => 4,
    }
}

fn collision_safe_archive_dir(archive_root: &Path, slug: &str) -> PathBuf {
    let base = archive_root.join(slug);
    if !base.exists() {
        return base;
    }
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    archive_root.join(format!("{slug}-{ts}"))
}
