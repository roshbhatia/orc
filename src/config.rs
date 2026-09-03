use std::{env, fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct Config {
    pub cache: CacheConfig,
    pub providers: ProviderConfig,
    pub workflows: WorkflowConfig,
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheConfig {
    pub provider_ttl_ms: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            provider_ttl_ms: 30_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderConfig {
    pub directory: PathBuf,
    pub timeout_ms: u64,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            directory: config_home().join("orc/providers"),
            timeout_ms: 15_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkflowConfig {
    pub repository: PathBuf,
    pub auto_commit: bool,
    pub max_depth: usize,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            repository: data_home().join("orc/workflows"),
            auto_commit: true,
            max_depth: 10,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiConfig {
    pub refresh_ms: u64,
    pub activity_refresh_ms: u64,
    pub inspector_percent: u16,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            refresh_ms: 5_000,
            activity_refresh_ms: 10_000,
            inspector_percent: 28,
        }
    }
}

pub fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

pub fn data_home() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
}

pub fn state_home() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from(".local/state"))
}

pub fn path() -> PathBuf {
    env::var_os("ORC_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| config_home().join("orc/config.yaml"))
}

pub fn load() -> Result<Config> {
    let mut config = if path().exists() {
        serde_yaml::from_str(&fs::read_to_string(path()).context("read Orc config")?)
            .context("parse Orc config")?
    } else {
        Config::default()
    };
    if let Some(value) =
        env::var_os("ORC_PROVIDERS_DIRECTORY").or_else(|| env::var_os("ORC_PROVIDER_DIR"))
    {
        config.providers.directory = value.into();
    }
    if let Ok(value) =
        env::var("ORC_PROVIDERS_TIMEOUT_MS").or_else(|_| env::var("ORC_PROVIDER_TIMEOUT_MS"))
    {
        config.providers.timeout_ms = value.parse().context("parse provider timeout")?;
    }
    if let Ok(value) = env::var("ORC_CACHE_PROVIDER_TTL_MS") {
        config.cache.provider_ttl_ms = value.parse().context("parse provider cache TTL")?;
    }
    if let Some(value) = env::var_os("ORC_WORKFLOWS_REPOSITORY") {
        config.workflows.repository = value.into();
    }
    if let Ok(value) = env::var("ORC_WORKFLOWS_AUTO_COMMIT") {
        config.workflows.auto_commit = parse_bool(&value, "workflow auto-commit")?;
    }
    if let Ok(value) = env::var("ORC_WORKFLOWS_MAX_DEPTH") {
        config.workflows.max_depth = value.parse().context("parse workflow max depth")?;
    }
    if let Ok(value) = env::var("ORC_UI_REFRESH_MS") {
        config.ui.refresh_ms = value.parse().context("parse UI refresh interval")?;
    }
    if let Ok(value) = env::var("ORC_UI_ACTIVITY_REFRESH_MS") {
        config.ui.activity_refresh_ms = value.parse().context("parse activity refresh interval")?;
    }
    if let Ok(value) = env::var("ORC_UI_INSPECTOR_PERCENT") {
        config.ui.inspector_percent = value.parse().context("parse UI inspector size")?;
    }
    if config.providers.timeout_ms == 0 {
        bail!("providers.timeoutMs must be positive");
    }
    if config.ui.refresh_ms < 50 {
        bail!("ui.refreshMs must be at least 50");
    }
    if config.ui.activity_refresh_ms < 1_000 {
        bail!("ui.activityRefreshMs must be at least 1000");
    }
    if !(20..=80).contains(&config.ui.inspector_percent) {
        bail!("ui.inspectorPercent must be between 20 and 80");
    }
    Ok(config)
}

fn parse_bool(value: &str, name: &str) -> Result<bool> {
    match value {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be true or false"),
    }
}

impl Config {
    pub fn provider_timeout(&self) -> Duration {
        Duration::from_millis(self.providers.timeout_ms)
    }
}

pub fn schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(Config)).expect("config schema serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let config = Config::default();
        assert!(config.providers.timeout_ms > 0);
        assert!(config.workflows.max_depth > 0);
    }
}
