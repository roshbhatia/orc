use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{config, state};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyMode {
    #[default]
    Supervised,
    ApprovalGated,
    Autonomous,
}

impl AutonomyMode {
    pub fn next(self) -> Self {
        match self {
            Self::Supervised => Self::ApprovalGated,
            Self::ApprovalGated => Self::Autonomous,
            Self::Autonomous => Self::Supervised,
        }
    }
}

impl std::fmt::Display for AutonomyMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}",
            serde_json::to_value(self)
                .expect("mode serializes")
                .as_str()
                .expect("mode is a string")
        )
    }
}

impl std::str::FromStr for AutonomyMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(&format!("\"{value}\""))
            .map_err(|_| format!("unknown autonomy mode: {value}"))
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkspacePreferences {
    pub version: String,
    pub autonomy: AutonomyMode,
    pub view: String,
    pub inspector_tab: String,
    pub inspector_dock: String,
    pub inspector_percent: u16,
    pub selected_item: Option<String>,
}

impl Default for WorkspacePreferences {
    fn default() -> Self {
        Self {
            version: "orc.preferences/v1".into(),
            autonomy: AutonomyMode::Supervised,
            view: "tree".into(),
            inspector_tab: "summary".into(),
            inspector_dock: "bottom".into(),
            inspector_percent: 28,
            selected_item: None,
        }
    }
}

pub fn path(scope: &Path) -> PathBuf {
    config::state_home()
        .join("orc/workspaces")
        .join(state::scope_key(scope))
        .join("preferences.json")
}

pub fn read(scope: &Path) -> Result<WorkspacePreferences> {
    let target = path(scope);
    match fs::read_to_string(&target) {
        Ok(source) => serde_json::from_str(&source)
            .with_context(|| format!("parse workspace preferences {}", target.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(WorkspacePreferences::default()),
        Err(error) => Err(error).with_context(|| format!("read {}", target.display())),
    }
}

pub fn write(scope: &Path, preferences: &WorkspacePreferences) -> Result<()> {
    let target = path(scope);
    let parent = target.parent().context("preferences path has no parent")?;
    fs::create_dir_all(parent).context("create preferences directory")?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    fs::write(&temporary, serde_json::to_vec_pretty(preferences)?)
        .context("write temporary workspace preferences")?;
    fs::rename(&temporary, &target).context("commit workspace preferences")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomy_modes_cycle_in_risk_order() {
        assert_eq!(AutonomyMode::Supervised.next(), AutonomyMode::ApprovalGated);
        assert_eq!(AutonomyMode::ApprovalGated.next(), AutonomyMode::Autonomous);
        assert_eq!(AutonomyMode::Autonomous.next(), AutonomyMode::Supervised);
    }

    #[test]
    fn preferences_are_separate_from_orchestration_state() {
        let scope = Path::new("/tmp/orc-preferences");
        assert_ne!(path(scope), state::path(scope));
        assert!(path(scope).ends_with("preferences.json"));
    }
}
