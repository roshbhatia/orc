use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rs_utils::animation::{AnimationConfig, Preferences, Sequence, Style};
use serde::Serialize;
use unicode_width::UnicodeWidthStr;

use crate::config::{self, Config};

const FALLBACK_SOURCE: &str = include_str!("../assets/animations.yaml");
const NAME: &str = "loading";

#[derive(Clone, Debug)]
pub struct Loaded {
    pub config: AnimationConfig,
    pub source: Source,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    File(PathBuf),
    Packaged,
}

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub source: String,
    pub warning: Option<String>,
    pub variant: &'static str,
    pub frame_index: usize,
    pub progress: f64,
    pub width: u16,
    pub height: u16,
    pub content: String,
}

#[derive(Clone, Copy, Debug)]
pub struct SampledFrame<'a> {
    pub content: &'a str,
    pub style: Style,
    pub variant: &'static str,
    pub frame_index: usize,
    pub progress: f64,
    pub width: u16,
    pub height: u16,
}

pub fn fallback() -> AnimationConfig {
    AnimationConfig::from_yaml(FALLBACK_SOURCE).expect("packaged animation must be valid")
}

pub fn default_path() -> PathBuf {
    config::config_home().join("orc/animations.yaml")
}

pub fn load(config: &Config, override_path: Option<&Path>) -> Result<Loaded> {
    if let Some(path) = override_path.or(config.ui.animation_file.as_deref()) {
        return load_explicit(path);
    }
    load_implicit(&default_path())
}

fn load_explicit(path: &Path) -> Result<Loaded> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("read animation configuration {}", path.display()))?;
    let config = AnimationConfig::from_yaml(&source)
        .with_context(|| format!("validate animation configuration {}", path.display()))?;
    require_loading(&config)?;
    Ok(Loaded {
        config,
        source: Source::File(path.to_path_buf()),
        warning: None,
    })
}

fn load_implicit(path: &Path) -> Result<Loaded> {
    if !path.exists() {
        return Ok(packaged(None));
    }
    match load_explicit(path) {
        Ok(loaded) => Ok(loaded),
        Err(error) => Ok(packaged(Some(format!(
            "ignored invalid {}: {error:#}",
            path.display()
        )))),
    }
}

fn packaged(warning: Option<String>) -> Loaded {
    Loaded {
        config: fallback(),
        source: Source::Packaged,
        warning,
    }
}

fn require_loading(config: &AnimationConfig) -> Result<()> {
    if config.animations.contains_key(NAME) {
        Ok(())
    } else {
        bail!("animations.{NAME} is required")
    }
}

pub fn select(
    config: &AnimationConfig,
    area_width: u16,
    area_height: u16,
    reduced_motion: bool,
) -> (&Sequence, &'static str) {
    let full = config
        .select(NAME, Preferences::default())
        .expect("validated Orc animation contains loading");
    let compact = area_width < full.dimensions.width
        || area_height < full.dimensions.height.saturating_add(2);
    let variant = if reduced_motion {
        "reduced_motion"
    } else if compact {
        "compact"
    } else {
        "full"
    };
    let sequence = config
        .select(
            NAME,
            Preferences {
                compact,
                reduced_motion,
            },
        )
        .expect("validated Orc animation contains loading");
    (sequence, variant)
}

pub fn sample(
    config: &AnimationConfig,
    elapsed_ms: u64,
    area_width: u16,
    area_height: u16,
    reduced_motion: bool,
) -> SampledFrame<'_> {
    let (sequence, variant) = select(config, area_width, area_height, reduced_motion);
    let sample = sequence.sample(elapsed_ms);
    let frame = &sequence.frames[sample.frame_index];
    SampledFrame {
        content: &frame.content,
        style: frame.style,
        variant,
        frame_index: sample.frame_index,
        progress: sample.progress,
        width: sequence.dimensions.width,
        height: sequence.dimensions.height,
    }
}

pub fn inspect(
    loaded: &Loaded,
    elapsed_ms: u64,
    area_width: u16,
    area_height: u16,
    reduced_motion: bool,
) -> Inspection {
    let frame = sample(
        &loaded.config,
        elapsed_ms,
        area_width,
        area_height,
        reduced_motion,
    );
    Inspection {
        source: match &loaded.source {
            Source::File(path) => path.display().to_string(),
            Source::Packaged => "packaged".into(),
        },
        warning: loaded.warning.clone(),
        variant: frame.variant,
        frame_index: frame.frame_index,
        progress: frame.progress,
        width: frame.width,
        height: frame.height,
        content: padded_content(frame),
    }
}

fn padded_content(frame: SampledFrame<'_>) -> String {
    let mut output = String::new();
    let mut lines = frame.content.split('\n');
    for row in 0..usize::from(frame.height) {
        if row > 0 {
            output.push('\n');
        }
        let line = lines.next().unwrap_or_default();
        output.push_str(line);
        output.push_str(&" ".repeat(usize::from(frame.width) - line.width()));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn custom() -> &'static str {
        r#"version: terminal.animation/v1
animations:
  loading:
    full:
      dimensions: { width: 4, height: 2 }
      playback: loop
      easing: linear
      fps: 10
      frames:
        - { content: a, style: accent }
        - { content: bb, style: warning }
    compact:
      dimensions: { width: 1, height: 1 }
      playback: loop
      easing: linear
      frames:
        - { content: x, style: muted, duration_ms: 50 }
        - { content: y, style: muted, duration_ms: 50 }
    reduced_motion:
      dimensions: { width: 4, height: 1 }
      playback: once
      easing: linear
      frames:
        - { content: stop, style: muted, duration_ms: 1000 }
"#
    }

    #[test]
    fn default_lookup_is_under_the_xdg_config_home() {
        assert!(default_path().ends_with("orc/animations.yaml"));
    }

    #[test]
    fn missing_implicit_file_uses_the_packaged_animation() {
        let directory = TempDir::new().expect("temporary directory");
        let loaded = load_implicit(&directory.path().join("missing.yaml")).expect("fallback");
        assert_eq!(loaded.source, Source::Packaged);
        assert!(loaded.warning.is_none());
    }

    #[test]
    fn invalid_implicit_file_falls_back_but_invalid_override_fails() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("animations.yaml");
        fs::write(&path, "not: the contract\n").expect("fixture");
        let fallback = load_implicit(&path).expect("safe fallback");
        assert_eq!(fallback.source, Source::Packaged);
        assert!(fallback.warning.is_some());
        assert!(load_explicit(&path).is_err());
    }

    #[test]
    fn actual_area_selects_compact_and_reduced_motion_wins() {
        let config = AnimationConfig::from_yaml(custom()).expect("custom animation");
        assert_eq!(select(&config, 4, 4, false).1, "full");
        assert_eq!(select(&config, 3, 4, false).1, "compact");
        assert_eq!(select(&config, 4, 1, false).1, "compact");
        assert_eq!(select(&config, 1, 1, true).1, "reduced_motion");
    }

    #[test]
    fn inspection_reports_dimensions_and_samples_shared_timing() {
        let loaded = Loaded {
            config: AnimationConfig::from_yaml(custom()).expect("custom animation"),
            source: Source::Packaged,
            warning: None,
        };
        let inspection = inspect(&loaded, 100, 80, 24, false);
        assert_eq!(inspection.frame_index, 1);
        assert_eq!((inspection.width, inspection.height), (4, 2));
        assert_eq!(inspection.content, "bb  \n    ");
    }

    #[test]
    fn sampled_frame_borrows_the_selected_content_without_padding() {
        let config = AnimationConfig::from_yaml(custom()).expect("custom animation");
        let sampled = sample(&config, 100, 80, 24, false);

        assert_eq!(sampled.frame_index, 1);
        assert_eq!(sampled.content, "bb");
        assert_eq!((sampled.width, sampled.height), (4, 2));
    }

    #[test]
    fn packaged_frame_preserves_the_supplied_dimensions() {
        let loaded = packaged(None);
        let inspection = inspect(&loaded, 0, 80, 24, true);
        assert_eq!((inspection.width, inspection.height), (31, 15));
        assert_eq!(inspection.content.lines().count(), 15);
    }
}
