use crate::error::{Context, JavelinError, Result};
use crate::paths::is_reserved_path;
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

pub const PURGED_PROVENANCE_PAYLOAD: &str = "{\"purged\":true}";

pub const DEFAULT_CONFIG: &str = r#"format = 1

[checkpoint]
debounce_ms = 250

[retention]
discarded_days = 7
raw_trace_days = 30
"#;

pub const DEFAULT_IGNORE: &str = r#"# Javelin tracking policy
.git/
.hg/
.svn/
node_modules/
target/
dist/
build/
.next/
.turbo/
.cache/
.DS_Store
.idea/
.vscode/
.env
.env.local
.env.*.local
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub format: u32,
    #[serde(default)]
    pub checkpoint: CheckpointConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub verification: VerificationConfig,
    #[serde(default)]
    pub provenance: ProvenanceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            debounce_ms: default_debounce(),
        }
    }
}

fn default_debounce() -> u64 {
    250
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    #[serde(default = "default_discarded_days")]
    pub discarded_days: u64,
    #[serde(default = "default_trace_days")]
    pub raw_trace_days: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            discarded_days: default_discarded_days(),
            raw_trace_days: default_trace_days(),
        }
    }
}

fn default_discarded_days() -> u64 {
    7
}

fn default_trace_days() -> u64 {
    30
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationConfig {
    #[serde(default, rename = "rule")]
    pub rules: Vec<WorldRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldRule {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_required() -> bool {
    true
}

fn default_timeout() -> u64 {
    600
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProvenanceConfig {
    #[serde(default)]
    pub redact: Vec<String>,
}

impl Config {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("javelin.toml");
        let text = fs::read_to_string(&path)
            .jctx("CONFIG_IO", format!("cannot read {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)
            .map_err(|error| JavelinError::policy(format!("invalid javelin.toml: {error}")))?;
        if config.format != 1 {
            return Err(JavelinError::policy(format!(
                "unsupported javelin.toml format {}",
                config.format
            )));
        }
        for rule in &config.verification.rules {
            if rule.name.trim().is_empty() || rule.command.is_empty() {
                return Err(JavelinError::policy(
                    "verification rules require a name and argv command",
                ));
            }
            if rule.timeout_seconds == 0 {
                return Err(JavelinError::policy(
                    "verification rule timeout_seconds must be greater than zero",
                ));
            }
        }
        retention_duration(config.retention.discarded_days, "retention.discarded_days")?;
        retention_duration(config.retention.raw_trace_days, "retention.raw_trace_days")?;
        Ok(config)
    }
}

pub fn retention_duration(days: u64, field: &str) -> Result<chrono::Duration> {
    let days =
        i64::try_from(days).map_err(|_| JavelinError::policy(format!("{field} is too large")))?;
    chrono::Duration::try_days(days)
        .ok_or_else(|| JavelinError::policy(format!("{field} is too large")))
}

#[derive(Debug)]
struct IgnoreRule {
    source: String,
    include: bool,
    directory: bool,
    matcher: GlobMatcher,
}

#[derive(Debug)]
pub struct IgnorePolicy {
    rules: Vec<IgnoreRule>,
}

impl IgnorePolicy {
    pub fn load(view: &Path) -> Result<Self> {
        let path = view.join(".javelinignore");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => DEFAULT_IGNORE.to_string(),
            Err(error) => {
                return Err(JavelinError::new(
                    7,
                    "CONFIG_IO",
                    format!("cannot read {}", path.display()),
                )
                .details(serde_json::json!({"cause": error.to_string()})));
            }
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut rules = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let include = line.starts_with('!');
            let pattern = line
                .strip_prefix('!')
                .unwrap_or(line)
                .trim_start_matches('/');
            let directory = pattern.ends_with('/');
            let pattern = pattern.trim_end_matches('/');
            let glob = if pattern.contains('/') {
                format!("{pattern}{}", if directory { "/**" } else { "" })
            } else if directory {
                format!("**/{pattern}/**")
            } else {
                format!("**/{pattern}")
            };
            let matcher = GlobBuilder::new(&glob)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .map_err(|error| {
                    JavelinError::policy(format!("invalid .javelinignore rule {line:?}: {error}"))
                })?
                .compile_matcher();
            rules.push(IgnoreRule {
                source: line.to_string(),
                include,
                directory,
                matcher,
            });
        }
        Ok(Self { rules })
    }

    pub fn decision(&self, path: &str, is_directory: bool) -> Option<(bool, &str)> {
        if is_reserved_path(path) {
            return Some((true, "Javelin internal/VCS isolation"));
        }
        let mut result = None;
        for rule in &self.rules {
            if rule.matcher.is_match(path)
                || (is_directory && rule.directory && rule.matcher.is_match(format!("{path}/x")))
            {
                result = Some((!rule.include, rule.source.as_str()));
            }
        }
        result
    }

    pub fn ignored(&self, path: &str, is_directory: bool) -> bool {
        self.decision(path, is_directory)
            .is_some_and(|(ignored, _)| ignored)
    }
}
