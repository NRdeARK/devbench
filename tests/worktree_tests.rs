use devbench::archive::archive_work;
use devbench::config::{AgentConfig, ArchiveConfig, CaptureMode, Config, HerdrConfig, RepoConfig};
use devbench::state::{load_work, save_work, ArchivePhase, RepoStatus, WorkStatus};
use devbench::worktree::{add_track_worktree, run_git, setup_workspace};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn init_sample_repo(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    run_git(dir, &["init", "-b", "main"]).unwrap();
    run_git(dir, &["config", "user.name", "Test User"]).unwrap();
    run_git(dir, &["config", "user.email", "test@example.com"]).unwrap();
    run_git(dir, &["config", "commit.gpgsign", "false"]).unwrap();
    fs::write(dir.join(".gitignore"), ".env\n").unwrap();
    fs::write(dir.join("README.md"), "# sample\n").unwrap();
    run_git(dir, &["add", ".gitignore", "README.md"]).unwrap();
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
fn preflight_guards_dirty_block_submodule_no_partial_wip_and_archive_collision() {
    let tmp = TempDir::new().unwrap();
    std::env::set_var("DEVBENCH_STATE_DIR", tmp.path().join("state"));

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
    repos.insert(
        "api-dup".to_string(),
        RepoConfig {
            path: repo_a.display().to_string(),
            base_ref: "main".to_string(),
        },
    );
    repos.insert(
        "../evil".to_string(),
        RepoConfig {
            path: repo_a.display().to_string(),
            base_ref: "main".to_string(),
        },
    );
    let cfg = test_config(tmp.path(), repos);

    // 1. Unknown alias fails preflight with zero folder created
    assert!(setup_workspace(&cfg, "bad-unknown", None, &["api".into(), "nope".into()]).is_err());
    assert!(!tmp.path().join("work").join("bad-unknown").exists());

    // 2. Traversal alias rejected
    assert!(setup_workspace(&cfg, "bad-alias", None, &["../evil".into()]).is_err());

    // 3. Two aliases pointing to same Git repo rejected in preflight
    assert!(setup_workspace(&cfg, "bad-same", None, &["api".into(), "api-dup".into()]).is_err());
    assert!(!tmp.path().join("work").join("bad-same").exists());

    // 4. Dirty source repo still succeeds cleanly (branches from base_ref)
    fs::write(repo_a.join("local-dirty.txt"), "host edit\n").unwrap();
    let mut state = setup_workspace(
        &cfg,
        "auth-refresh",
        Some("Auth refresh".to_string()),
        &["api".into(), "web".into(), "api".into()],
    )
    .unwrap();
    assert_eq!(state.repos.len(), 2);
    assert_eq!(state.status, WorkStatus::Active);
    save_work(&mut state).unwrap();

    // 5. Re-running setup for same slug rejected (state exists)
    assert!(setup_workspace(&cfg, "auth-refresh", None, &["api".into()]).is_err());

    // 5b. Add parallel track worktree inside the same initiative work & reject duplicate track
    let track_repo = add_track_worktree(&cfg, "auth-refresh", "api", Some("phase-2"), None).unwrap();
    assert_eq!(track_repo.track.as_deref(), Some("phase-2"));
    assert_eq!(track_repo.branch, "devbench/auth-refresh--phase-2");
    assert!(Path::new(&track_repo.worktree_path).is_dir());
    assert!(add_track_worktree(&cfg, "auth-refresh", "api", Some("phase-2"), None).is_err());

    // 6. Submodule in repo B blocks BEFORE committing WIP in dirty repo A!
    let api_wt = tmp.path().join("work").join("auth-refresh").join("api");
    let web_wt = tmp.path().join("work").join("auth-refresh").join("web");
    fs::write(api_wt.join("draft.txt"), "uncommitted\n").unwrap();
    fs::write(api_wt.join(".env"), "SECRET=demo\n").unwrap();
    fs::write(web_wt.join(".gitmodules"), "[submodule \"x\"]\n").unwrap();

    let head_before = run_git(&api_wt, &["rev-parse", "HEAD"]).unwrap();
    let sub_blocked = archive_work(&cfg, "auth-refresh", true, true);
    assert!(sub_blocked.is_err());
    // Verify repo A did NOT receive a premature WIP commit!
    assert_eq!(run_git(&api_wt, &["rev-parse", "HEAD"]).unwrap(), head_before);

    // Clean up fake .gitmodules; dirty tree blocks when snapshot_wip = false
    fs::remove_file(web_wt.join(".gitmodules")).unwrap();
    assert!(archive_work(&cfg, "auth-refresh", false, true).is_err());

    // Pre-create archive collision folder to test timestamped archive suffix
    fs::create_dir_all(tmp.path().join("archive").join("auth-refresh")).unwrap();

    // Now archive with --snapshot-wip -> commits WIP, warns on .env ignored file, moves to timestamped folder
    let report = archive_work(&cfg, "auth-refresh", true, true).unwrap();
    assert!(!tmp.path().join("work").join("auth-refresh").exists());
    assert!(Path::new(&report.archive_path).is_dir());
    assert_ne!(
        Path::new(&report.archive_path),
        tmp.path().join("archive").join("auth-refresh")
    );
    assert!(report.warnings.iter().any(|w| w.contains(".env")));
    assert!(Path::new(&report.snapshot_path).is_file());
    let summary = fs::read_to_string(Path::new(&report.archive_path).join("git_summary.md")).unwrap();
    assert!(summary.contains("devbench/auth-refresh"));
    assert!(summary.contains("devbench/auth-refresh--phase-2"));

    // Source repos clean and preserved branches match HEAD
    let branches_a = run_git(&repo_a, &["branch", "--list", "devbench/auth-refresh*"]).unwrap();
    assert!(branches_a.contains("devbench/auth-refresh"));
    assert!(branches_a.contains("devbench/auth-refresh--phase-2"));
    assert!(!run_git(&repo_a, &["worktree", "list"]).unwrap().contains("auth-refresh"));

    // Rerun after Moved is rejected cleanly
    assert!(archive_work(&cfg, "auth-refresh", false, true).is_err());

    // Verify central state reflects Moved + Removed repos + final SHAs
    let final_state = load_work("auth-refresh").unwrap();
    assert_eq!(final_state.status, WorkStatus::Archived);
    assert_eq!(final_state.archive_phase, Some(ArchivePhase::Moved));
    assert!(final_state.repos.iter().all(|r| r.status == RepoStatus::Removed && r.final_head_sha.is_some()));
}
