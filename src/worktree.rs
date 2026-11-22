use crate::config::Config;
use crate::paths::{check_path_length, expand_tilde, sanitize_slug};
use crate::state::{RepoStatus, WorkRepo, WorkState, WorkStatus};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct WorktreeInspection {
    pub alias: String,
    pub path: PathBuf,
    pub dirty_paths: Vec<String>,
    pub ignored_paths: Vec<String>,
    pub unpushed_commits: usize,
    pub has_submodules: bool,
}

pub fn run_git(repo_dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to invoke git in {}: {e}", repo_dir.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo_dir.display(),
            detail
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Preflight all target repos and create `<work_root>/<slug>/<repo>` worktrees atomically.
/// On mid-setup failure, rolls back every worktree and branch created so far.
pub fn setup_workspace(
    cfg: &Config,
    raw_slug: &str,
    title: Option<String>,
    raw_aliases: &[String],
) -> Result<WorkState, String> {
    let slug = sanitize_slug(raw_slug)?;
    if raw_aliases.is_empty() {
        return Err("At least one repo alias is required (--repos api,web)".to_string());
    }

    // Deduplicate aliases preserving order
    let mut seen = HashSet::new();
    let mut aliases = Vec::new();
    for a in raw_aliases {
        let t = a.trim();
        if !t.is_empty() && seen.insert(t.to_string()) {
            aliases.push(t.to_string());
        }
    }
    if aliases.is_empty() {
        return Err("No valid repo aliases provided".to_string());
    }

    let work_root = expand_tilde(&cfg.work_root)?;
    let work_dir = work_root.join(&slug);
    check_path_length(&work_dir)?;

    if work_dir.exists() {
        return Err(format!(
            "Work folder already exists: {}. Choose another slug or resume the existing work.",
            work_dir.display()
        ));
    }

    let branch_name = format!("{}{}", cfg.branch_prefix, slug);

    // Phase 1: Preflight EVERY repo before mutating disk or git
    struct PlannedRepo {
        alias: String,
        source_path: PathBuf,
        worktree_path: PathBuf,
        base_ref: String,
        base_sha: String,
    }

    let mut planned = Vec::with_capacity(aliases.len());
    for alias in &aliases {
        let repo_cfg = cfg.repos.get(alias).ok_or_else(|| {
            format!("Unknown repo alias '{alias}'. Add it to config.repos or run `devbench config check`.")
        })?;
        let source_path = expand_tilde(&repo_cfg.path)?;
        if !source_path.is_dir() || !source_path.join(".git").exists() {
            return Err(format!(
                "Repo '{alias}' is missing or not a Git repo: {}",
                source_path.display()
            ));
        }

        let worktree_path = work_dir.join(alias);
        check_path_length(&worktree_path)?;

        // Verify base_ref exists and record its SHA
        let rev_arg = format!("{}^{{commit}}", repo_cfg.base_ref);
        let base_sha = run_git(&source_path, &["rev-parse", "--verify", &rev_arg]).map_err(|_| {
            format!(
                "Repo '{alias}': base_ref '{}' not found in {}",
                repo_cfg.base_ref,
                source_path.display()
            )
        })?;

        // Check branch does not already exist (avoids colliding with existing or checked-out branch)
        let ref_check = format!("refs/heads/{branch_name}");
        if run_git(&source_path, &["show-ref", "--verify", "--quiet", &ref_check]).is_ok() {
            return Err(format!(
                "Repo '{alias}': branch '{branch_name}' already exists in {}. Use a distinct work slug.",
                source_path.display()
            ));
        }

        planned.push(PlannedRepo {
            alias: alias.clone(),
            source_path,
            worktree_path,
            base_ref: repo_cfg.base_ref.clone(),
            base_sha,
        });
    }

    // Phase 2: Create central folder + notes dir + worktrees, rolling back on any error
    fs::create_dir_all(work_dir.join("notes"))
        .map_err(|e| format!("Failed to create {}: {e}", work_dir.display()))?;

    let mut created: Vec<WorkRepo> = Vec::new();
    for item in planned {
        let wt_str = item.worktree_path.to_string_lossy().to_string();
        let res = run_git(
            &item.source_path,
            &[
                "worktree",
                "add",
                "-b",
                &branch_name,
                &wt_str,
                &item.base_sha,
            ],
        );

        match res {
            Ok(_) => {
                created.push(WorkRepo {
                    alias: item.alias,
                    source_path: item.source_path.to_string_lossy().to_string(),
                    worktree_path: wt_str,
                    branch: branch_name.clone(),
                    base_ref: item.base_ref,
                    base_sha: item.base_sha,
                    status: RepoStatus::Ready,
                    final_head_sha: None,
                });
            }
            Err(err) => {
                // Roll back previously created worktrees and branches cleanly
                for done in created.iter().rev() {
                    let src = Path::new(&done.source_path);
                    let _ = run_git(src, &["worktree", "remove", "--force", &done.worktree_path]);
                    let _ = run_git(src, &["worktree", "prune"]);
                    let _ = run_git(src, &["branch", "-D", &done.branch]);
                }
                let _ = fs::remove_dir_all(&work_dir);
                return Err(format!(
                    "Failed while creating worktree for '{}' (rolled back {} earlier repo(s)): {}",
                    item.alias,
                    created.len(),
                    err
                ));
            }
        }
    }

    let mut state = WorkState::new(
        slug,
        title.unwrap_or_default(),
        work_dir.to_string_lossy().to_string(),
    );
    state.repos = created;
    state.status = WorkStatus::Active;
    Ok(state)
}

/// Inspect a worktree for dirty/untracked files, gitignored files, unpushed commits, and submodules.
pub fn inspect_worktree(repo: &WorkRepo) -> Result<WorktreeInspection, String> {
    let wt = PathBuf::from(&repo.worktree_path);
    if !wt.is_dir() {
        return Ok(WorktreeInspection {
            alias: repo.alias.clone(),
            path: wt,
            dirty_paths: Vec::new(),
            ignored_paths: Vec::new(),
            unpushed_commits: 0,
            has_submodules: false,
        });
    }

    let status_out = run_git(&wt, &["status", "--porcelain"])?;
    let dirty_paths: Vec<String> = status_out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();

    let ignored_out = run_git(&wt, &["status", "--ignored", "--porcelain"]).unwrap_or_default();
    let ignored_paths: Vec<String> = ignored_out
        .lines()
        .filter(|l| l.starts_with("!! "))
        .map(|l| l.trim_start_matches("!! ").to_string())
        .collect();

    // Count commits not pushed to upstream (if an upstream exists; otherwise check against base_sha)
    let unpushed_commits = match run_git(&wt, &["rev-list", "--count", "@{u}..HEAD"]) {
        Ok(n) => n.parse::<usize>().unwrap_or(0),
        Err(_) => {
            let range = format!("{}..HEAD", repo.base_sha);
            run_git(&wt, &["rev-list", "--count", &range])
                .ok()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
        }
    };

    let has_submodules = wt.join(".gitmodules").is_file();

    Ok(WorktreeInspection {
        alias: repo.alias.clone(),
        path: wt,
        dirty_paths,
        ignored_paths,
        unpushed_commits,
        has_submodules,
    })
}

/// Commit all dirty/untracked changes as a WIP snapshot commit on the task branch.
pub fn snapshot_wip(repo: &WorkRepo, slug: &str) -> Result<String, String> {
    let wt = Path::new(&repo.worktree_path);
    run_git(wt, &["add", "-A"])?;
    let msg = format!("wip(devbench): archive snapshot for {slug}");
    run_git(wt, &["commit", "-m", &msg])?;
    run_git(wt, &["rev-parse", "HEAD"])
}

/// Read current HEAD SHA and a short log summary for archive notes before detaching.
pub fn collect_git_summary(repo: &WorkRepo) -> (Option<String>, String) {
    let wt = Path::new(&repo.worktree_path);
    if !wt.is_dir() {
        return (repo.final_head_sha.clone(), "(worktree already absent)\n".to_string());
    }
    let head_sha = run_git(wt, &["rev-parse", "HEAD"]).ok();
    let range = format!("{}..HEAD", repo.base_sha);
    let log_out = run_git(wt, &["log", "--oneline", &range]).unwrap_or_default();
    let diff_stat = run_git(wt, &["diff", "--stat", &repo.base_sha, "HEAD"]).unwrap_or_default();
    let summary = format!(
        "## {} ({})\n- Base SHA: `{}`\n- Final HEAD: `{}`\n\n### Commits\n```\n{}\n```\n\n### Diff Stat\n```\n{}\n```\n",
        repo.alias,
        repo.branch,
        repo.base_sha,
        head_sha.as_deref().unwrap_or("unknown"),
        if log_out.is_empty() { "(no new commits)" } else { &log_out },
        if diff_stat.is_empty() { "(no diff)" } else { &diff_stat },
    );
    (head_sha, summary)
}

/// Cleanly detach a git worktree (NO `--force`) and prune worktree admin metadata.
pub fn detach_worktree(repo: &WorkRepo) -> Result<(), String> {
    let src = Path::new(&repo.source_path);
    let wt = Path::new(&repo.worktree_path);
    if wt.exists() {
        run_git(src, &["worktree", "remove", &repo.worktree_path])?;
    }
    run_git(src, &["worktree", "prune"])?;
    Ok(())
}
