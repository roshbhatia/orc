use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(test)]
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
#[serde(default)]
pub struct Config {
    pub cache: CacheConfig,
    pub daemon: DaemonConfig,
    pub lifecycle: LifecycleConfig,
    pub providers: ProviderConfig,
    pub workflows: WorkflowConfig,
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DaemonConfig {
    pub autostart: bool,
    pub scan_interval_ms: u64,
    pub idle_shutdown_seconds: u64,
    pub termination_retry_seconds: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            autostart: true,
            scan_interval_ms: 5_000,
            idle_shutdown_seconds: 60,
            termination_retry_seconds: 60,
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LifecycleConfig {
    pub runtime_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            runtime_timeout_seconds: 28_800,
            idle_timeout_seconds: 1_800,
        }
    }
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
    pub animation_file: Option<PathBuf>,
    pub reduced_motion: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            refresh_ms: 5_000,
            activity_refresh_ms: 10_000,
            inspector_percent: 28,
            animation_file: None,
            reduced_motion: false,
        }
    }
}

pub fn config_home() -> PathBuf {
    config_home_from(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

fn config_home_from(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| path.join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"))
}

pub fn data_home() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"))
}

#[cfg(not(test))]
fn data_dirs() -> Vec<PathBuf> {
    env::var_os("XDG_DATA_DIRS")
        .map(|value| env::split_paths(&value).collect())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        })
}

pub fn state_home() -> PathBuf {
    #[cfg(test)]
    {
        static TEST_STATE_HOME: OnceLock<PathBuf> = OnceLock::new();
        TEST_STATE_HOME
            .get_or_init(|| env::temp_dir().join(format!("orc-tests-{}", std::process::id())))
            .clone()
    }
    #[cfg(not(test))]
    {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from(".local/state"))
    }
}

pub fn path() -> PathBuf {
    expand_home(
        env::var_os("ORC_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| config_home().join("orc/config.yaml")),
    )
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
    if let Ok(value) = env::var("ORC_DAEMON_AUTOSTART") {
        config.daemon.autostart = parse_bool(&value, "daemon autostart")?;
    }
    if let Ok(value) = env::var("ORC_DAEMON_SCAN_INTERVAL_MS") {
        config.daemon.scan_interval_ms = value.parse().context("parse daemon scan interval")?;
    }
    if let Ok(value) = env::var("ORC_DAEMON_IDLE_SHUTDOWN_SECONDS") {
        config.daemon.idle_shutdown_seconds = value
            .parse()
            .context("parse daemon idle shutdown timeout")?;
    }
    if let Ok(value) = env::var("ORC_DAEMON_TERMINATION_RETRY_SECONDS") {
        config.daemon.termination_retry_seconds = value
            .parse()
            .context("parse daemon termination retry interval")?;
    }
    if let Ok(value) = env::var("ORC_LIFECYCLE_RUNTIME_TIMEOUT_SECONDS") {
        config.lifecycle.runtime_timeout_seconds =
            value.parse().context("parse lifecycle runtime timeout")?;
    }
    if let Ok(value) = env::var("ORC_LIFECYCLE_IDLE_TIMEOUT_SECONDS") {
        config.lifecycle.idle_timeout_seconds =
            value.parse().context("parse lifecycle idle timeout")?;
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
    if let Some(value) = env::var_os("ORC_UI_ANIMATION_FILE") {
        config.ui.animation_file = Some(value.into());
    }
    if let Ok(value) = env::var("ORC_UI_REDUCED_MOTION") {
        config.ui.reduced_motion = parse_bool(&value, "UI reduced motion")?;
    }
    config.providers.directory = expand_home(config.providers.directory);
    config.workflows.repository = expand_home(config.workflows.repository);
    config.ui.animation_file = config.ui.animation_file.map(expand_home);
    if config.providers.timeout_ms == 0 {
        bail!("providers.timeoutMs must be positive");
    }
    if !(100..=3_600_000).contains(&config.daemon.scan_interval_ms) {
        bail!("daemon.scanIntervalMs must be between 100 and 3600000");
    }
    if config.workflows.max_depth == 0 {
        bail!("workflows.maxDepth must be positive");
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

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return path;
    };
    if path == Path::new("~") {
        return home;
    }
    path.strip_prefix("~/")
        .map(|suffix| home.join(suffix))
        .unwrap_or(path)
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

    pub fn provider_directories(&self) -> Vec<PathBuf> {
        #[cfg(test)]
        {
            vec![self.providers.directory.clone()]
        }
        #[cfg(not(test))]
        {
            provider_search_directories(&self.providers.directory, &data_home(), data_dirs().iter())
        }
    }
}

pub fn schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schema_for!(Config)).expect("config schema serializes");
    for pointer in [
        "/$defs/ProviderConfig/properties/directory/default",
        "/properties/providers/default/directory",
    ] {
        *schema
            .pointer_mut(pointer)
            .expect("provider path default exists") =
            serde_json::Value::String("~/.config/orc/providers".into());
    }
    for pointer in [
        "/$defs/WorkflowConfig/properties/repository/default",
        "/properties/workflows/default/repository",
    ] {
        *schema
            .pointer_mut(pointer)
            .expect("workflow path default exists") =
            serde_json::Value::String("~/.local/share/orc/workflows".into());
    }
    schema
}

fn provider_search_directories<'a>(
    configured: &Path,
    user_data: &Path,
    system_data: impl IntoIterator<Item = &'a PathBuf>,
) -> Vec<PathBuf> {
    let mut directories = vec![configured.to_path_buf(), user_data.join("orc/providers")];
    directories.extend(
        system_data
            .into_iter()
            .map(|directory| directory.join("orc/providers")),
    );
    let mut seen = std::collections::BTreeSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    directories
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let config = Config::default();
        assert!(config.providers.timeout_ms > 0);
        assert!(config.daemon.autostart);
        assert!(config.lifecycle.runtime_timeout_seconds > 0);
        assert!(config.workflows.max_depth > 0);
    }

    #[test]
    fn provider_search_prefers_user_configuration_then_xdg_data() {
        let system = [
            PathBuf::from("/first/share"),
            PathBuf::from("/second/share"),
        ];

        assert_eq!(
            provider_search_directories(
                Path::new("/config/orc/providers"),
                Path::new("/user/share"),
                system.iter(),
            ),
            vec![
                PathBuf::from("/config/orc/providers"),
                PathBuf::from("/user/share/orc/providers"),
                PathBuf::from("/first/share/orc/providers"),
                PathBuf::from("/second/share/orc/providers"),
            ]
        );
    }

    #[test]
    fn config_home_rejects_an_empty_xdg_path() {
        assert_eq!(
            config_home_from(Some(OsString::new()), Some(OsString::from("/home/tester"))),
            PathBuf::from("/home/tester/.config")
        );
    }

    #[test]
    fn config_home_rejects_a_relative_xdg_path() {
        assert_eq!(
            config_home_from(
                Some(OsString::from("relative")),
                Some(OsString::from("/home/tester"))
            ),
            PathBuf::from("/home/tester/.config")
        );
    }

    #[test]
    fn config_home_accepts_an_absolute_xdg_path() {
        assert_eq!(
            config_home_from(
                Some(OsString::from("/custom/config")),
                Some(OsString::from("/home/tester"))
            ),
            PathBuf::from("/custom/config")
        );
    }

    #[test]
    fn generated_schema_uses_portable_default_paths() {
        let schema = schema();

        assert_eq!(
            schema.pointer("/$defs/ProviderConfig/properties/directory/default"),
            Some(&serde_json::Value::String("~/.config/orc/providers".into()))
        );
        assert_eq!(
            schema.pointer("/$defs/WorkflowConfig/properties/repository/default"),
            Some(&serde_json::Value::String(
                "~/.local/share/orc/workflows".into()
            ))
        );
    }

    #[test]
    fn home_relative_paths_expand() {
        let home = env::var_os("HOME").map(PathBuf::from).expect("HOME");

        assert_eq!(
            expand_home(PathBuf::from("~/providers")),
            home.join("providers")
        );
    }
}
