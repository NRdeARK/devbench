use crate::paths::{devbench_dir, expand_tilde};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
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
pub struct ConfigCheckReport {
    pub config_path: String,
    pub source: String,
    pub ok: bool,
    pub schema_version: u32,
    pub expected_schema_version: u32,
    pub missing_template_keys: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Resolve config path using D4 external-first lookup:
/// 1. `$DEVBENCH_CONFIG`
/// 2. `~/.devbench/config.json`
/// 3. Optional gitignored repo-local `./config.json` (dev-mode fallback only if present)
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

    // Optional dev-mode fallback when developing inside a local clone
    if let Ok(cwd) = env::current_dir() {
        let local = cwd.join("config.json");
        if local.is_file() && cwd.join("templates").join("config.example.json").is_file() {
            return Ok((local, "dev-fallback:./config.json"));
        }
    }

    Ok((external, "external:~/.devbench/config.json"))
}

/// Initialize user config from embedded template without ever overwriting an existing file.
pub fn init_config() -> Result<(PathBuf, bool, ConfigCheckReport), String> {
    let (path, source) = resolve_config_path()?;
    if path.is_file() {
        let report = check_config(&path, source);
        return Ok((path, false, report));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir {}: {e}", parent.display()))?;
    }
    fs::write(&path, CONFIG_TEMPLATE)
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

    let report = check_config(&path, source);
    Ok((path, true, report))
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
    if cfg.work_root.trim().is_empty() {
        return Err(format!("{}: 'work_root' cannot be empty", path.display()));
    }
    if cfg.archive_root.trim().is_empty() {
        return Err(format!("{}: 'archive_root' cannot be empty", path.display()));
    }
    Ok((path, cfg))
}

/// Validate config syntax, schema_version, template key diff, repo paths, and default agent.
pub fn check_config(path: &Path, source: &str) -> ConfigCheckReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut missing_template_keys = Vec::new();
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
                errors,
                warnings,
            };
        }
    };

    if let Ok(template_val) = serde_json::from_str::<Value>(CONFIG_TEMPLATE) {
        collect_missing_keys(&template_val, &user_val, "", &mut missing_template_keys);
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
                errors,
                warnings,
            };
        }
    };

    schema_version = cfg.schema_version;
    if cfg.schema_version != CURRENT_SCHEMA_VERSION {
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

    for (alias, repo) in &cfg.repos {
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
        errors,
        warnings,
    }
}

fn collect_missing_keys(template: &Value, user: &Value, prefix: &str, out: &mut Vec<String>) {
    let (Some(t_obj), Some(u_obj)) = (template.as_object(), user.as_object()) else {
        return;
    };
    for (k, t_val) in t_obj {
        let key_path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        // Do not diff user-chosen map entries inside repos or agents
        if prefix == "repos" || prefix == "agents" {
            continue;
        }
        match u_obj.get(k) {
            None => out.push(key_path),
            Some(u_val) => {
                if k != "repos" && k != "agents" {
                    collect_missing_keys(t_val, u_val, &key_path, out);
                }
            }
        }
    }
}
