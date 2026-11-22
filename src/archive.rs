use crate::config::Config;
use crate::herdr::{current_hc_terminal_id, HerdrClient};
use crate::paths::{expand_tilde, move_dir_safe};
use crate::state::{
    load_work, mutate_work, write_frozen_snapshot, ArchivePhase, RepoStatus, SessionStatus,
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
/// 1. Self-archive guard (with `--allow-self-archive`, defers closing caller's own space/terminal to last)
/// 2. Inspect ALL worktrees BEFORE mutating anything (so a submodule blocker never causes partial WIP commits)
/// 3. Phase 1 (`herdr_closed`): close 1:1 Herdr space (`{id, confirmed: true}`) + kill/wait/rm recorded terminals
/// 4. Phase 2 (`worktrees_detached`): save each repo's `final_head_sha` + `Removed` immediately after detach
/// 5. Phase 3 (`snapshot_written`): write `git_summary.md` and frozen `state.snapshot.json` (fails if work_dir missing)
/// 6. Phase 4 (`moved`): staged cross-volume-safe move into collision-free `<archive_root>/<slug>`
pub fn archive_work(
    cfg: &Config,
    slug: &str,
    do_snapshot_wip: bool,
    allow_self_archive: bool,
) -> Result<ArchiveReport, String> {
    let state_initial = load_work(slug)?;
    if state_initial.status == WorkStatus::Archived
        && state_initial.archive_phase == Some(ArchivePhase::Moved)
    {
        return Err(format!(
            "Work '{slug}' is already archived at {}",
            state_initial.archive_path.as_deref().unwrap_or("(unknown)")
        ));
    }

    let caller_hc = current_hc_terminal_id();
    let invoked_from_self = caller_hc.as_deref().is_some_and(|cid| {
        state_initial
            .sessions
            .iter()
            .any(|s| s.hc_terminal_id.as_deref() == Some(cid))
            || state_initial.herdr_space_initial_terminal.as_deref() == Some(cid)
    });

    if invoked_from_self && !allow_self_archive {
        return Err(format!(
            "Refusing to close workspace while invoked from inside this work's own terminal ({}). \
             Run `devbench archive {slug}` from the Dispatcher outside this workspace, or pass --allow-self-archive.",
            caller_hc.as_deref().unwrap_or("unknown")
        ));
    }

    let effective_snapshot_wip = do_snapshot_wip || cfg.archive.snapshot_wip;
    let defer_self_close = invoked_from_self && allow_self_archive;
    let mut warnings = Vec::new();
    let phase_rank = archive_phase_rank(&state_initial.archive_phase);

    // Step 2: Inspect ALL repos BEFORE committing WIP or closing Herdr
    if phase_rank < 2 {
        let mut dirty_blockers = Vec::new();
        let mut repos_needing_wip = Vec::new();

        for repo in &state_initial.repos {
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
                if effective_snapshot_wip {
                    repos_needing_wip.push(repo.clone());
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
                "Archive blocked due to dirty/untracked worktrees or submodules (Herdr left open):\n- {}\nRe-run with --snapshot-wip to commit WIP onto task branches, or clean/commit manually.",
                dirty_blockers.join("\n- ")
            ));
        }

        // Only commit WIP snapshots after ALL blockers passed across ALL repos
        for repo in repos_needing_wip {
            let sha = snapshot_wip(&repo, slug)?;
            warnings.push(format!(
                "{}: committed WIP snapshot {} on branch {}",
                repo.alias, sha, repo.branch
            ));
        }
    }

    let (mut state, _) = mutate_work(slug, |s| {
        s.status = WorkStatus::Archiving;
        Ok((true, ()))
    })?;

    let herdr = HerdrClient::from_config(&cfg.herdr);

    // Phase 1: Close 1:1 Herdr workspace & kill recorded terminals FIRST (unless deferring self-archive)
    if phase_rank < 1 {
        if !defer_self_close {
            if let Some(space_id) = state.herdr_space_id.clone() {
                if let Err(e) = herdr.space_close(&space_id) {
                    warnings.push(format!(
                        "Herdr space '{space_id}' already closed or unreachable (tolerated): {e}"
                    ));
                }
            }
            if let Some(init_tid) = state.herdr_space_initial_terminal.clone() {
                let _ = herdr.hc_kill_and_rm(&init_tid);
            }
            let session_tids: Vec<String> = state
                .sessions
                .iter()
                .filter_map(|s| s.hc_terminal_id.clone())
                .collect();
            for tid in session_tids {
                if let Err(e) = herdr.hc_kill_and_rm(&tid) {
                    warnings.push(format!("hc cleanup for '{tid}' warned: {e}"));
                }
            }
            let (updated, _) = mutate_work(slug, |s| {
                for sess in &mut s.sessions {
                    sess.status = SessionStatus::Archived;
                }
                s.herdr_space_id = None;
                s.herdr_space_initial_terminal = None;
                s.archive_phase = Some(ArchivePhase::HerdrClosed);
                Ok((true, ()))
            })?;
            state = updated;
        } else {
            // Self-archive mode: kill only OTHER sessions now; keep caller's space/terminal until last
            let caller_id = caller_hc.as_deref();
            for sess in &state.sessions {
                if let Some(tid) = sess.hc_terminal_id.as_deref() {
                    if Some(tid) != caller_id {
                        let _ = herdr.hc_kill_and_rm(tid);
                    }
                }
            }
            let (updated, _) = mutate_work(slug, |s| {
                for sess in &mut s.sessions {
                    if sess.hc_terminal_id.as_deref() != caller_id {
                        sess.status = SessionStatus::Archived;
                    }
                }
                s.archive_phase = Some(ArchivePhase::HerdrClosed);
                Ok((true, ()))
            })?;
            state = updated;
        }
    }

    // Phase 2: Detach each clean worktree and checkpoint IMMEDIATELY per repo so partial failure retains SHAs
    let work_dir = PathBuf::from(&state.work_dir);
    if phase_rank < 2 {
        let repo_count = state.repos.len();
        for idx in 0..repo_count {
            let repo = state.repos[idx].clone();
            if repo.status == RepoStatus::Removed {
                continue;
            }
            let (head_sha, _) = collect_git_summary(&repo);
            detach_worktree(&repo).map_err(|e| {
                format!(
                    "Failed to detach worktree '{}' (rerun `devbench archive {slug}` after releasing any Windows lock): {e}",
                    repo.alias
                )
            })?;
            let (updated, _) = mutate_work(slug, |s| {
                if let Some(r) = s.repos.get_mut(idx) {
                    r.final_head_sha = head_sha.clone();
                    r.status = RepoStatus::Removed;
                }
                Ok((true, ()))
            })?;
            state = updated;
        }

        // Write complete git_summary.md covering ALL repos (using preserved branches in source repos)
        if !work_dir.is_dir() {
            return Err(format!(
                "Active work directory is missing before snapshot: {}",
                work_dir.display()
            ));
        }
        let mut summary_doc = format!("# Archive Git Summary — {slug}\n\n");
        for repo in &state.repos {
            let (_, md) = collect_git_summary(repo);
            summary_doc.push_str(&md);
            summary_doc.push('\n');
        }
        fs::write(work_dir.join("git_summary.md"), summary_doc)
            .map_err(|e| format!("Failed to write git_summary.md: {e}"))?;

        let (updated, _) = mutate_work(slug, |s| {
            s.archive_phase = Some(ArchivePhase::WorktreesDetached);
            Ok((true, ()))
        })?;
        state = updated;
    }

    // Compute collision-safe archive destination (reusing recorded archive_path if resuming Phase 4)
    let archive_root = expand_tilde(&cfg.archive_root)?;
    let target_dir = match &state.archive_path {
        Some(existing) if phase_rank >= 3 => PathBuf::from(existing),
        _ => collision_safe_archive_dir(&archive_root, slug),
    };

    // Phase 3: Write frozen snapshot into work folder
    if phase_rank < 3 {
        if !work_dir.is_dir() {
            return Err(format!(
                "Cannot write frozen snapshot: work directory missing at {}",
                work_dir.display()
            ));
        }
        let (updated, _) = mutate_work(slug, |s| {
            s.status = WorkStatus::Archived;
            s.archive_path = Some(target_dir.to_string_lossy().to_string());
            s.archive_phase = Some(ArchivePhase::SnapshotWritten);
            for sess in &mut s.sessions {
                sess.status = SessionStatus::Archived;
            }
            Ok((true, ()))
        })?;
        state = updated;
        write_frozen_snapshot(&work_dir, &state)?;
    }

    // Phase 4: Move folder to archive_root (safe across drives; never claim Moved if folder vanished)
    let final_dir = PathBuf::from(
        state
            .archive_path
            .clone()
            .unwrap_or_else(|| target_dir.to_string_lossy().to_string()),
    );
    if phase_rank < 4 {
        if work_dir.is_dir() {
            move_dir_safe(&work_dir, &final_dir)?;
        } else if !final_dir.is_dir() {
            return Err(format!(
                "Cannot complete archive move: neither source {} nor archive destination {} exists",
                work_dir.display(),
                final_dir.display()
            ));
        }
        let (updated, _) = mutate_work(slug, |s| {
            s.status = WorkStatus::Archived;
            s.archive_phase = Some(ArchivePhase::Moved);
            s.herdr_space_id = None;
            s.herdr_space_initial_terminal = None;
            Ok((true, ()))
        })?;
        state = updated;
    }

    // Deferred self-archive cleanup: close caller's own workspace LAST after state & archive are safely persisted
    if defer_self_close {
        if let Some(space_id) = state_initial.herdr_space_id.as_deref() {
            let _ = herdr.space_close(space_id);
        }
        if let Some(cid) = caller_hc.as_deref() {
            let _ = herdr.hc_kill_and_rm(cid);
        }
    }

    let kept_branches = state
        .repos
        .iter()
        .map(|r| {
            format!(
                "{}:{}@{}",
                r.display_label(),
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
    let candidate = archive_root.join(format!("{slug}-{ts}"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 2..=999 {
        let indexed = archive_root.join(format!("{slug}-{ts}-{n}"));
        if !indexed.exists() {
            return indexed;
        }
    }
    archive_root.join(format!("{slug}-{ts}-{}", uuid::Uuid::new_v4().simple()))
}
