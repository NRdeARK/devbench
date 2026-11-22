use crate::paths::{devbench_dir, expand_tilde, validate_repo_alias};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const CONFIG_TEMPLATE: &str = include_str!("../templates/config.example.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub work_root: String,
    pub archive_root: String,
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
    #[serde(default = "default_agent")]
    pub default_agent: String,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoConfig>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentConfig>,
    #[serde(default)]
    pub herdr: HerdrConfig,
    #[serde(default)]
    pub archive: ArchiveConfig,
}

fn default_branch_prefix() -> String {
    "devbench/".to_string()
}

fn default_agent() -> String {
    "claude".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub path: String,
    #[serde(default = "default_base_ref")]
    pub base_ref: String,
}

fn default_base_ref() -> String {
    "origin/main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Preassign,
    Scan,
    Manual,
    Unavailable,
}

impl Default for CaptureMode {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub argv: Vec<String>,
    #[serde(default)]
    pub resume_argv: Option<Vec<String>>,
    #[serde(default)]
    pub capture: CaptureMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HerdrConfig {
    #[serde(default = "default_name_prefix")]
    pub name_prefix: String,
    #[serde(default)]
    pub focus_on_create: bool,
    #[serde(default = "default_worker_layout")]
    pub worker_layout: String,
    #[serde(default)]
    pub hc_bin: Option<String>,
    #[serde(default)]
    pub app_bin: Option<String>,
}

fn default_name_prefix() -> String {
    "db".to_string()
}

fn default_worker_layout() -> String {
    "tab".to_string()
}

impl Default for HerdrConfig {
    fn default() -> Self {
        Self {
            name_prefix: default_name_prefix(),
            focus_on_create: false,
            worker_layout: default_worker_layout(),
            hc_bin: None,
            app_bin: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConfig {
    #[serde(default = "default_on_dirty")]
    pub on_dirty: String,
    #[serde(default = "default_true")]
    pub warn_unpushed: bool,
    #[serde(default)]
    pub snapshot_wip: bool,
}

fn default_on_dirty() -> String {
    "block".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            on_dirty: default_on_dirty(),
            warn_unpushed: true,
            snapshot_wip: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingKeyHint {
    pub key: String,
    pub example: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigCheckReport {
    pub config_path: String,
    pub source: String,
    pub ok: bool,
    pub schema_version: u32,
    pub expected_schema_version: u32,
    pub missing_template_keys: Vec<String>,
    pub missing_key_hints: Vec<MissingKeyHint>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Resolve config path using D4 external-first lookup:
/// 1. `$DEVBENCH_CONFIG` (never falls back if explicitly set)
/// 2. `~/.devbench/config.json`
/// 3. Optional gitignored repo-local `./config.json` (dev-mode fallback only when external absent)
pub fn resolve_config_path() -> Result<(PathBuf, &'static str), String> {
    if let Ok(env_path) = env::var("DEVBENCH_CONFIG") {
        let trimmed = env_path.trim();
        if !trimmed.is_empty() {
            return Ok((expand_tilde(trimmed)?, "env:DEVBENCH_CONFIG"));
        }
    }

    let external = devbench_dir()?.join("config.json");
    if external.is_file() {
        return Ok((external, "external:~/.devbench/config.json"));
    }

    if let Ok(cwd) = env::current_dir() {
        let local = cwd.join("config.json");
        if local.is_file() && cwd.join("templates").join("config.example.json").is_file() {
            return Ok((local, "dev-fallback:./config.json"));
        }
    }

    Ok((external, "external:~/.devbench/config.json"))
}

/// Atomically initialize user config from embedded template (`create_new`), never overwriting an existing file.
pub fn init_config() -> Result<(PathBuf, bool, ConfigCheckReport), String> {
    let (path, source) = resolve_config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir {}: {e}", parent.display()))?;
    }

    let created = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            f.write_all(CONFIG_TEMPLATE.as_bytes())
                .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
            f.sync_all()
                .map_err(|e| format!("Failed to sync {}: {e}", path.display()))?;
            true
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(format!("Failed to create {}: {e}", path.display())),
    };

    let report = check_config(&path, source);
    Ok((path, created, report))
}

/// Load and parse user config strictly.
pub fn load_config() -> Result<(PathBuf, Config), String> {
    let (path, _) = resolve_config_path()?;
    if !path.is_file() {
        return Err(format!(
            "User config not found at {}. Run `devbench init` first.",
            path.display()
        ));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let cfg: Config = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "Invalid JSON in {} (line {}, col {}): {}",
            path.display(),
            e.line(),
            e.column(),
            e
        )
    })?;
    if cfg.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "{}: unsupported schema_version {} (this devbench supports up to {})",
            path.display(),
            cfg.schema_version,
            CURRENT_SCHEMA_VERSION
        ));
    }
    if cfg.work_root.trim().is_empty() {
        return Err(format!("{}: 'work_root' cannot be empty", path.display()));
    }
    if cfg.archive_root.trim().is_empty() {
        return Err(format!("{}: 'archive_root' cannot be empty", path.display()));
    }
    Ok((path, cfg))
}

/// Validate config syntax, schema_version, template key diff + hints, repo aliases/paths, and agents.
pub fn check_config(path: &Path, source: &str) -> ConfigCheckReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut missing_template_keys = Vec::new();
    let mut missing_key_hints = Vec::new();
    let mut schema_version = 0;

    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            errors.push(format!("Config file unreadable: {e}. Run `devbench init`."));
            return ConfigCheckReport {
                config_path: path.display().to_string(),
                source: source.to_string(),
                ok: false,
                schema_version,
                expected_schema_version: CURRENT_SCHEMA_VERSION,
                missing_template_keys,
                missing_key_hints,
                errors,
                warnings,
            };
        }
    };

    let user_val: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            errors.push(format!(
                "Malformed JSON at line {}, column {}: {}",
                e.line(),
                e.column(),
                e
            ));
            return ConfigCheckReport {
                config_path: path.display().to_string(),
                source: source.to_string(),
                ok: false,
                schema_version,
                expected_schema_version: CURRENT_SCHEMA_VERSION,
                missing_template_keys,
                missing_key_hints,
                errors,
                warnings,
            };
        }
    };

    if let Ok(template_val) = serde_json::from_str::<Value>(CONFIG_TEMPLATE) {
        collect_missing_keys(
            &template_val,
            &user_val,
            "",
            &mut missing_template_keys,
            &mut missing_key_hints,
        );
    }

    let cfg: Config = match serde_json::from_value(user_val) {
        Ok(c) => c,
        Err(e) => {
            errors.push(format!("Config schema error: {e}"));
            return ConfigCheckReport {
                config_path: path.display().to_string(),
                source: source.to_string(),
                ok: false,
                schema_version,
                expected_schema_version: CURRENT_SCHEMA_VERSION,
                missing_template_keys,
                missing_key_hints,
                errors,
                warnings,
            };
        }
    };

    schema_version = cfg.schema_version;
    if cfg.schema_version > CURRENT_SCHEMA_VERSION {
        errors.push(format!(
            "schema_version {} is newer than supported version {}; upgrade devbench",
            cfg.schema_version, CURRENT_SCHEMA_VERSION
        ));
    } else if cfg.schema_version != CURRENT_SCHEMA_VERSION {
        warnings.push(format!(
            "schema_version is {}, expected {} (compare against templates/config.example.json)",
            cfg.schema_version, CURRENT_SCHEMA_VERSION
        ));
    }
    if cfg.work_root.trim().is_empty() {
        errors.push("work_root is empty".to_string());
    }
    if cfg.archive_root.trim().is_empty() {
        errors.push("archive_root is empty".to_string());
    }
    if !cfg.agents.contains_key(&cfg.default_agent) {
        errors.push(format!(
            "default_agent '{}' is not defined in agents",
            cfg.default_agent
        ));
    }

    // Validate herdr.name_prefix against hc's `[a-z0-9-_]` rule
    if cfg.herdr.name_prefix.is_empty()
        || cfg.herdr.name_prefix.len() > 8
        || !cfg
            .herdr
            .name_prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        errors.push(format!(
            "herdr.name_prefix '{}' must be 1..8 lowercase chars of [a-z0-9-_]",
            cfg.herdr.name_prefix
        ));
    }

    for (alias, repo) in &cfg.repos {
        if let Err(e) = validate_repo_alias(alias) {
            errors.push(e);
            continue;
        }
        match expand_tilde(&repo.path) {
            Ok(p) => {
                if !p.exists() {
                    errors.push(format!(
                        "repo '{alias}' path does not exist: {}",
                        p.display()
                    ));
                } else if !p.join(".git").exists() {
                    errors.push(format!(
                        "repo '{alias}' is not a Git repository (.git missing): {}",
                        p.display()
                    ));
                }
            }
            Err(e) => errors.push(format!("repo '{alias}': {e}")),
        }
    }

    for (name, agent) in &cfg.agents {
        if agent.argv.is_empty() {
            errors.push(format!("agent '{name}' has an empty argv array"));
        }
        if let Some(resume) = &agent.resume_argv {
            if !resume.iter().any(|a| a.contains("{session_id}")) {
                warnings.push(format!(
                    "agent '{name}' resume_argv does not contain '{{session_id}}' placeholder"
                ));
            }
        }
        if name == "codex" && !agent.argv.iter().any(|a| a == "--no-daemon") {
            warnings.push(
                "agent 'codex' argv does not include '--no-daemon' (required by herdr-core)"
                    .to_string(),
            );
        }
    }

    ConfigCheckReport {
        config_path: path.display().to_string(),
        source: source.to_string(),
        ok: errors.is_empty(),
        schema_version,
        expected_schema_version: CURRENT_SCHEMA_VERSION,
        missing_template_keys,
        missing_key_hints,
        errors,
        warnings,
    }
}

fn collect_missing_keys(
    template: &Value,
    user: &Value,
    prefix: &str,
    out_keys: &mut Vec<String>,
    out_hints: &mut Vec<MissingKeyHint>,
) {
    let (Some(t_obj), Some(u_obj)) = (template.as_object(), user.as_object()) else {
        return;
    };
    for (k, t_val) in t_obj {
        let key_path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        if prefix == "repos" || prefix == "agents" {
            continue;
        }
        match u_obj.get(k) {
            None => {
                out_keys.push(key_path.clone());
                out_hints.push(MissingKeyHint {
                    key: key_path,
                    example: t_val.clone(),
                });
            }
            Some(u_val) => {
                if k != "repos" && k != "agents" {
                    collect_missing_keys(t_val, u_val, &key_path, out_keys, out_hints);
                }
            }
        }
    }
}
