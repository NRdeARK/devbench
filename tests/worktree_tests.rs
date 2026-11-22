use devbench::archive::archive_work;
use devbench::config::{AgentConfig, ArchiveConfig, CaptureMode, Config, HerdrConfig, RepoConfig};
use devbench::state::{save_work, WorkStatus};
use devbench::worktree::{run_git, setup_workspace};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn init_sample_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    run_git(dir, &["init", "-b", "main"]).unwrap();
    run_git(dir, &["config", "user.name", "Test User"]).unwrap();
    run_git(dir, &["config", "user.email", "test@example.com"]).unwrap();
    fs::write(dir.join("README.md"), "# sample\n").unwrap();
    run_git(dir, &["add", "README.md"]).unwrap();
    run_git(dir, &["commit", "-m", "initial"]).unwrap();
}

fn test_config(root: &Path, repos: BTreeMap<String, RepoConfig>) -> Config {
    let mut agents = BTreeMap::new();
    agents.insert(
        "claude".to_string(),
        AgentConfig {
            argv: vec!["claude".to_string()],
            resume_argv: Some(vec![
                "claude".to_string(),
                "--resume".to_string(),
                "{session_id}".to_string(),
            ]),
            capture: CaptureMode::Preassign,
        },
    );
    Config {
        schema_version: 1,
        work_root: root.join("work").display().to_string(),
        archive_root: root.join("archive").display().to_string(),
        branch_prefix: "devbench/".to_string(),
        default_agent: "claude".to_string(),
        repos,
        agents,
        herdr: HerdrConfig::default(),
        archive: ArchiveConfig::default(),
    }
}

#[test]
fn multi_repo_setup_dirty_block_wip_snapshot_and_clean_archive() {
    let tmp = TempDir::new().unwrap();
    let repo_a = tmp.path().join("repos").join("api");
    let repo_b = tmp.path().join("repos").join("web");
    init_sample_repo(&repo_a);
    init_sample_repo(&repo_b);

    let mut repos = BTreeMap::new();
    repos.insert(
        "api".to_string(),
        RepoConfig {
            path: repo_a.display().to_string(),
            base_ref: "main".to_string(),
        },
    );
    repos.insert(
        "web".to_string(),
        RepoConfig {
            path: repo_b.display().to_string(),
            base_ref: "main".to_string(),
        },
    );
    let cfg = test_config(tmp.path(), repos);

    // Point central state dir to isolated temp dir
    std::env::set_var("DEVBENCH_STATE_DIR", tmp.path().join("state"));

    // Preflight fails cleanly on unknown alias without creating anything
    let bad = setup_workspace(
        &cfg,
        "fail-preflight",
        None,
        &["api".to_string(), "missing".to_string()],
    );
    assert!(bad.is_err());
    assert!(!tmp.path().join("work").join("fail-preflight").exists());

    // Valid multi-repo setup (with duplicate alias deduplication)
    let mut state = setup_workspace(
        &cfg,
        "auth-refresh",
        Some("Auth refresh".to_string()),
        &["api".to_string(), "web".to_string(), "api".to_string()],
    )
    .unwrap();
    assert_eq!(state.repos.len(), 2);
    assert_eq!(state.status, WorkStatus::Active);
    save_work(&mut state).unwrap();

    // Dirty worktree blocks archive cleanly
    let api_wt = tmp.path().join("work").join("auth-refresh").join("api");
    fs::write(api_wt.join("draft.txt"), "uncommitted\n").unwrap();
    let blocked = archive_work(&cfg, "auth-refresh", false, true);
    assert!(blocked.is_err());
    assert!(api_wt.exists());

    // --snapshot-wip commits WIP on task branch, detaches clean worktrees, preserves branches, and archives
    let report = archive_work(&cfg, "auth-refresh", true, true).unwrap();
    assert!(!tmp.path().join("work").join("auth-refresh").exists());
    assert!(Path::new(&report.archive_path).is_dir());
    assert!(Path::new(&report.snapshot_path).is_file());
    assert!(Path::new(&report.archive_path).join("git_summary.md").is_file());

    // Source repos remain clean and still have the preserved task branches
    let branches_a = run_git(&repo_a, &["branch", "--list", "devbench/auth-refresh"]).unwrap();
    assert!(branches_a.contains("devbench/auth-refresh"));
    let wt_list_a = run_git(&repo_a, &["worktree", "list"]).unwrap();
    assert!(!wt_list_a.contains("auth-refresh"));
}
