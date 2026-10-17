//! Deliberately literal, root-relative discovery rules; no external VCS semantics.
use super::*;

const LIMIT: u64 = 64 * 1024;
const MAX_RULES: usize = 256;
const CONFIG: &str = ".javelinignore";

pub(super) struct Rules(Vec<String>);

impl Rules {
    pub(super) fn load(root: &Path) -> Result<Self> {
        let mut file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(CONFIG))
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self(Vec::new())),
            Err(e) => return Err(Error::InvalidInput(format!("cannot read {CONFIG}: {e}"))),
        };
        if !file.metadata()?.is_file() {
            return Err(Error::InvalidInput(format!(
                "{CONFIG} must be a regular file"
            )));
        }
        let mut bytes = Vec::new();
        (&mut file).take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::InvalidInput(format!("{CONFIG} exceeds 64 KiB")));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidInput(format!("{CONFIG} must be UTF-8")))?;
        Self::parse(text)
    }

    fn parse(text: &str) -> Result<Self> {
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

    pub(super) fn ignored(&self, path: &str) -> bool {
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
        let rules =
            Rules::parse("# private outputs\nbuild/\npkg/cache\nsecret.txt\n.javelinignore\n")
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
            assert!(Rules::parse(text).is_err(), "{text}");
        }
        assert!(Rules::parse(&"a\n".repeat(257)).is_err());
    }

    #[test]
    fn rule_file_is_bounded_regular_utf8_and_never_follows_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(CONFIG);
        assert!(!Rules::load(root.path()).unwrap().ignored("a"));
        fs::write(&path, vec![b'a'; LIMIT as usize + 1]).unwrap();
        assert!(Rules::load(root.path()).is_err());
        fs::write(&path, [0xff]).unwrap();
        assert!(Rules::load(root.path()).is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing-outside", &path).unwrap();
        assert!(Rules::load(root.path()).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(Rules::load(root.path()).is_err());
    }
}
