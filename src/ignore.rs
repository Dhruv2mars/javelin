//! Pure workspace discovery rules, independent of the Layer backend.
use crate::{Error, RepoPath, Result};

pub(crate) const LIMIT: usize = 64 * 1024;
pub(crate) const CONFIG: &str = ".javelinignore";
const MAX_RULES: usize = 256;

/// Bounded, literal root-relative ignore rules for untracked discovery.
///
/// ```
/// let rules = javelin::IgnoreRules::parse("build/\n")?;
/// assert!(rules.ignored("build/output.o"));
/// assert!(!rules.ignored("src/build.rs"));
/// # Ok::<(), javelin::Error>(())
/// ```
#[derive(Debug, Default)]
pub struct IgnoreRules(Vec<String>);

impl IgnoreRules {
    /// Parses literal root-relative rules without reading the filesystem.
    /// Rejects unsupported syntax, more than 256 rules, or more than 64 KiB.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > LIMIT {
            return Err(Error::InvalidInput(format!("{CONFIG} exceeds 64 KiB")));
        }
        let mut rules = Vec::new();
        for (line, value) in text.lines().enumerate() {
            if value.is_empty() || value.starts_with('#') {
                continue;
            }
            let value = value.strip_suffix('/').unwrap_or(value);
            if value.trim() != value
                || value.contains(['*', '?', '[', ']', '!', '\\'])
                || RepoPath::parse(value).is_err()
                || rules.len() == MAX_RULES
            {
                return Err(Error::InvalidInput(format!(
                    "invalid {CONFIG} line {}: expected a literal root-relative path (maximum 256 rules)",
                    line + 1
                )));
            }
            rules.push(value.to_string());
        }
        Ok(Self(rules))
    }

    /// Returns whether a normalized, slash-separated path matches a rule.
    ///
    /// This only decides discovery: callers must still visit tracked paths and
    /// their ancestors even when ignored. `.javelinignore` always remains visible.
    pub fn ignored(&self, path: &str) -> bool {
        path != CONFIG
            && self.0.iter().any(|r| {
                path == r
                    || path
                        .strip_prefix(r)
                        .is_some_and(|suffix| suffix.starts_with('/'))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_paths_match_only_rooted_components() {
        let rules = IgnoreRules::parse(
            "# private outputs\nbuild/\npkg/cache\nsecret.txt\n.javelinignore\n",
        )
        .unwrap();
        for path in ["build", "build/a/b", "pkg/cache/x", "secret.txt"] {
            assert!(rules.ignored(path), "{path}");
        }
        for path in ["builder", "src/build/a", "pkg/caches", ".javelinignore"] {
            assert!(!rules.ignored(path), "{path}");
        }
    }

    #[test]
    fn unsupported_or_ambiguous_rules_are_visible_errors() {
        for text in [
            "!build", "*.o", "/build", "../out", "a//b", " build", "build ", "a\\b",
        ] {
            assert!(IgnoreRules::parse(text).is_err(), "{text}");
        }
        assert!(IgnoreRules::parse(&"a\n".repeat(257)).is_err());
    }

    #[test]
    fn empty_crlf_and_size_limits_are_platform_independent() {
        assert!(!IgnoreRules::default().ignored("build/output"));
        let rules = IgnoreRules::parse("# comment\r\nbuild/\r\n\r\n").unwrap();
        assert!(rules.ignored("build/output"));
        assert!(!rules.ignored("src/build/output"));
        assert!(IgnoreRules::parse(&"#".repeat(LIMIT)).is_ok());
        assert!(IgnoreRules::parse(&"#".repeat(LIMIT + 1)).is_err());
    }
}
