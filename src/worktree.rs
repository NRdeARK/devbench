use crate::config::Config;
use crate::paths::{
    check_path_length, expand_tilde, normalize_for_compare, sanitize_slug, validate_repo_alias,
};
use crate::state::{work_state_path, RepoStatus, WorkRepo, WorkState, WorkStatus};
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

/// Preflight all target repos and create `<work_root>/<slug>/<repo>` worktrees cleanly.
/// Uses exclusive `create_dir` (no TOCTOU race) and rolls back created worktrees & branches on error.
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

    // Prevent overwriting an existing active or archived central state record
    let state_path = work_state_path(&slug)?;
    if state_path.exists() {
        return Err(format!(
            "Work '{slug}' already exists in central state ({}). Choose a distinct slug.",
            state_path.display()
        ));
    }

    // Deduplicate aliases preserving order
    let mut seen_aliases = HashSet::new();
    let mut aliases = Vec::new();
    for a in raw_aliases {
        let t = a.trim();
        if !t.is_empty() && seen_aliases.insert(t.to_string()) {
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

    let mut seen_canonical_repos = HashSet::new();
    let mut planned = Vec::with_capacity(aliases.len());
    for alias in &aliases {
        validate_repo_alias(alias)?;
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

        // Guard against two different aliases pointing to the same physical repository
        let canon = fs::canonicalize(&source_path).unwrap_or_else(|_| source_path.clone());
        let canon_key = normalize_for_compare(&canon);
        if !seen_canonical_repos.insert(canon_key) {
            return Err(format!(
                "Repo alias '{alias}' points to the same Git repository as another alias in this work: {}",
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

        // Check branch does not already exist
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

    // Phase 2: Exclusively create central folder (eliminates TOCTOU race between two concurrent `new` calls)
    fs::create_dir_all(&work_root)
        .map_err(|e| format!("Failed to create work_root {}: {e}", work_root.display()))?;
    fs::create_dir(&work_dir).map_err(|e| {
        format!(
            "Failed to exclusively create work folder {} (already exists or permission denied): {e}",
            work_dir.display()
        )
    })?;

    if let Err(e) = fs::create_dir_all(work_dir.join("notes")) {
        let _ = fs::remove_dir_all(&work_dir);
        return Err(format!("Failed to create notes dir: {e}"));
    }

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
                let mut rollback_errors = Vec::new();
                // Clean up any partial branch/worktree left by the failing repo itself
                let _ = run_git(&item.source_path, &["worktree", "remove", "--force", &wt_str]);
                let _ = run_git(&item.source_path, &["worktree", "prune"]);
                let _ = run_git(&item.source_path, &["branch", "-D", &branch_name]);

                // Roll back previously created worktrees and branches cleanly
                for done in created.iter().rev() {
                    let src = Path::new(&done.source_path);
                    if let Err(re) = run_git(src, &["worktree", "remove", "--force", &done.worktree_path]) {
                        rollback_errors.push(format!("{}: {re}", done.alias));
                    }
                    let _ = run_git(src, &["worktree", "prune"]);
                    if let Err(be) = run_git(src, &["branch", "-D", &done.branch]) {
                        rollback_errors.push(format!("{} branch cleanup: {be}", done.alias));
                    }
                }
                if let Err(rm_err) = fs::remove_dir_all(&work_dir) {
                    rollback_errors.push(format!("remove {}: {rm_err}", work_dir.display()));
                }

                let rb_note = if rollback_errors.is_empty() {
                    format!("rolled back {} earlier repo(s) cleanly", created.len())
                } else {
                    format!("rollback had warnings: {}", rollback_errors.join("; "))
                };
                return Err(format!(
                    "Failed while creating worktree for '{}' ({rb_note}): {err}",
                    item.alias
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
/// Falls back to querying the preserved branch in `source_path` if the worktree was already detached
/// during an earlier interrupted archive run.
pub fn collect_git_summary(repo: &WorkRepo) -> (Option<String>, String) {
    let wt = Path::new(&repo.worktree_path);
    let src = Path::new(&repo.source_path);
    let (query_dir, target_ref) = if wt.is_dir() {
        (wt, "HEAD".to_string())
    } else if src.is_dir() {
        (src, repo.branch.clone())
    } else {
        return (
            repo.final_head_sha.clone(),
            format!("## {} ({})\n- Final HEAD: `{}`\n\n(repo unavailable)\n",
                repo.alias,
                repo.branch,
                repo.final_head_sha.as_deref().unwrap_or("unknown")),
        );
    };

    let head_sha = run_git(query_dir, &["rev-parse", &target_ref])
        .ok()
        .or_else(|| repo.final_head_sha.clone());
    let range = format!("{}..{}", repo.base_sha, target_ref);
    let log_out = run_git(query_dir, &["log", "--oneline", &range]).unwrap_or_default();
    let diff_stat =
        run_git(query_dir, &["diff", "--stat", &repo.base_sha, &target_ref]).unwrap_or_default();
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
