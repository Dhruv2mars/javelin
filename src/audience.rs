//! Who may read what. A repository's policy is the file `.javelinpolicy` in its World,
//! so it is versioned and published like code. Each line maps a path pattern to an
//! audience; the last matching line wins, and a path no line matches is public:
//!
//! ```text
//! # pattern        audience
//! .env*            maintainers
//! config/prod/**   maintainers
//! internal/**      members
//! security/**      group:security
//! ```
//!
//! Patterns are anchored at the repository root and matched per path component: `*`
//! matches any run of characters inside one component, `?` one character, and a `**`
//! component any number of components, including none. So `config/prod/**` matches the
//! directory `config/prod` itself, which is then hidden as one entry.
//!
//! An entry is hidden where its audience is not public and differs from its parent
//! directory's (the parent of a top-level entry is public). Everything below a hidden
//! entry is inside it, so a hidden directory hides the names it holds. Inside it, a
//! more specific rule makes a nested hidden entry, which needs its own audience to open.
//! The policy file itself is always public.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

/// Where the policy lives in the World.
pub const POLICY_PATH: &str = ".javelinpolicy";
/// Largest policy file, in bytes.
pub const MAX_POLICY_BYTES: usize = 64 << 10;
/// Most rules in one policy.
pub const MAX_RULES: usize = 1024;

/// Who may read an entry. Hidden entries carry any audience but `Public`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Audience {
    Public,
    /// Everyone with a role in the repository.
    Members,
    /// Members with the Maintain role.
    Maintainers,
    /// A named group of members, written `group:<name>`. Maintainers read it too.
    Group(String),
}

impl Audience {
    /// Longest encoded audience, in bytes.
    pub const MAX: usize = 64;

    pub fn parse(value: &str) -> Result<Self> {
        let bad = || {
            Error::InvalidInput(format!(
                "audience {value:?} is not public, members, maintainers or group:<name>"
            ))
        };
        match value {
            "public" => Ok(Self::Public),
            "members" => Ok(Self::Members),
            "maintainers" => Ok(Self::Maintainers),
            _ => {
                let name = value.strip_prefix("group:").ok_or_else(bad)?;
                if name.is_empty()
                    || value.len() > Self::MAX
                    || !name.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b)
                    })
                {
                    return Err(bad());
                }
                Ok(Self::Group(name.into()))
            }
        }
    }

    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Public => "public".into(),
            Self::Members => "members".into(),
            Self::Maintainers => "maintainers".into(),
            Self::Group(name) => format!("group:{name}").into(),
        }
    }
}

impl Audience {
    /// The narrower of two audiences: readable only by readers of both, rounded down to
    /// `maintainers` when two different groups meet (maintainers read every audience).
    pub fn meet(self, other: &Audience) -> Audience {
        match (self, other) {
            (Audience::Public, other) => other.clone(),
            (narrower, Audience::Public) => narrower,
            (Audience::Maintainers, _) | (_, Audience::Maintainers) => Audience::Maintainers,
            (Audience::Members, other) => other.clone(),
            (narrower, Audience::Members) => narrower,
            (Audience::Group(a), Audience::Group(b)) if a == *b => Audience::Group(a),
            (Audience::Group(_), Audience::Group(_)) => Audience::Maintainers,
        }
    }
}

impl fmt::Display for Audience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl Serialize for Audience {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Audience {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The audiences one reader belongs to. A node derives it from the caller's role: no
/// role is an outsider, Read and Review are members, Maintain is a maintainer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reader {
    member: bool,
    maintainer: bool,
    groups: BTreeSet<String>,
    /// Who the caller is, when authenticated. Objects this principal uploaded are readable
    /// by it whatever their audience: it already holds their bytes.
    principal: Option<String>,
}

impl Reader {
    /// Reads public content only.
    pub fn outsider() -> Self {
        Self::default()
    }

    /// A member, in the named groups.
    pub fn member(groups: impl IntoIterator<Item = String>) -> Self {
        Self {
            member: true,
            maintainer: false,
            groups: groups.into_iter().collect(),
            principal: None,
        }
    }

    /// Reads every audience. Maintainers can change the policy, so nothing is hidden
    /// from them.
    pub fn maintainer() -> Self {
        Self {
            member: true,
            maintainer: true,
            groups: BTreeSet::new(),
            principal: None,
        }
    }

    /// The same reader, identified as `principal`.
    pub fn with_principal(self, principal: impl Into<String>) -> Self {
        Self {
            principal: Some(principal.into()),
            ..self
        }
    }

    pub fn principal(&self) -> Option<&str> {
        self.principal.as_deref()
    }

    pub fn is_member(&self) -> bool {
        self.member
    }

    pub fn reads_all(&self) -> bool {
        self.maintainer
    }

    pub fn can_read(&self, audience: &Audience) -> bool {
        match audience {
            Audience::Public => true,
            Audience::Members => self.member,
            Audience::Maintainers => self.maintainer,
            Audience::Group(name) => self.maintainer || self.groups.contains(name),
        }
    }
}

/// A parsed `.javelinpolicy`. The empty policy makes everything public.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Policy {
    rules: Vec<(Vec<String>, Audience)>,
}

impl Policy {
    /// Parses the policy file. Each line is blank, a `#` comment, or a pattern and an
    /// audience separated by whitespace.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let bad = |line: usize, why: &str| {
            Error::InvalidInput(format!("{POLICY_PATH} line {line}: {why}"))
        };
        if bytes.len() > MAX_POLICY_BYTES {
            return Err(Error::InvalidInput(format!(
                "{POLICY_PATH} exceeds {MAX_POLICY_BYTES} bytes"
            )));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::InvalidInput(format!("{POLICY_PATH} is not UTF-8")))?;
        let mut rules = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [pattern, audience] = fields[..] else {
                return Err(bad(index + 1, "expected a pattern and an audience"));
            };
            let components: Vec<String> = pattern.split('/').map(str::to_owned).collect();
            if components.iter().any(|c| {
                c.is_empty()
                    || c == "."
                    || c == ".."
                    || c.contains(['\\', ':', '\0'])
                    || (c.contains("**") && c != "**")
            }) {
                return Err(bad(index + 1, "invalid path pattern"));
            }
            let audience = Audience::parse(audience).map_err(|e| bad(index + 1, &e.to_string()))?;
            rules.push((components, audience));
            if rules.len() > MAX_RULES {
                return Err(bad(index + 1, "too many rules"));
            }
        }
        Ok(Self { rules })
    }

    /// The audience of the file or directory at `path`: the last matching rule's, else
    /// public. Matching fails toward the narrower audience: a rule that matches `path` only
    /// when letter case is ignored, as on a case-folding volume, can narrow the result but
    /// never widen it. The policy file itself is always public.
    pub fn audience(&self, path: &str) -> Audience {
        if path == POLICY_PATH {
            return Audience::Public;
        }
        let components: Vec<&str> = path.split('/').collect();
        let folded: Vec<String> = components.iter().map(|c| fold(c)).collect();
        let folded: Vec<&str> = folded.iter().map(String::as_str).collect();
        let exact = self
            .rules
            .iter()
            .rev()
            .find(|(pattern, _)| matches_path(pattern, &components, false))
            .map_or(Audience::Public, |(_, audience)| audience.clone());
        self.rules
            .iter()
            .filter(|(_, audience)| *audience != Audience::Public)
            .filter(|(pattern, _)| {
                !matches_path(pattern, &components, false) && matches_path(pattern, &folded, true)
            })
            .fold(exact, |narrowest, (_, audience)| narrowest.meet(audience))
    }

    /// The hidden entries on the way to `path`, outermost first: the byte length of
    /// each hidden prefix (the whole path when the entry itself is hidden) and its
    /// audience. O(depth x rules).
    pub fn hidden_along(&self, path: &str) -> Vec<(usize, Audience)> {
        let mut out = Vec::new();
        let mut parent = Audience::Public;
        let ends = path
            .match_indices('/')
            .map(|(i, _)| i)
            .chain(std::iter::once(path.len()));
        for end in ends {
            let audience = self.audience(&path[..end]);
            if audience != Audience::Public && audience != parent {
                out.push((end, audience.clone()));
            }
            parent = audience;
        }
        out
    }

    /// True when nothing is hidden.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// Lower-cases a name for case-insensitive matching.
fn fold(name: &str) -> String {
    name.chars().flat_map(char::to_lowercase).collect()
}

/// Matches path components against pattern components, where `**` matches any run of
/// components. With `folded`, `path` is already lower-cased and each pattern component is
/// compared lower-cased. Greedy with one backtrack point, so O(pattern x path).
fn matches_path(pattern: &[String], path: &[&str], folded: bool) -> bool {
    let (mut p, mut s) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while s < path.len() {
        if p < pattern.len() && pattern[p] == "**" {
            star = Some((p, s));
            p += 1;
        } else if p < pattern.len()
            && if folded {
                matches_name(&fold(&pattern[p]), path[s])
            } else {
                matches_name(&pattern[p], path[s])
            }
        {
            p += 1;
            s += 1;
        } else if let Some((sp, ss)) = star {
            p = sp + 1;
            s = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| c == "**")
}

/// Matches one name against a glob. In a pattern, `*` and `?` are always wildcards, so a
/// name that holds them literally is matched by them too: a pattern never fails to match
/// because of a special character in a name. O(pattern x name).
fn matches_name(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut s) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while s < name.len() {
        // The wildcard comes first: a literal `*` in the name must not consume it.
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, s));
            p += 1;
        } else if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[s]) {
            p += 1;
            s += 1;
        } else if let Some((sp, ss)) = star {
            p = sp + 1;
            s = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str) -> Policy {
        Policy::parse(text.as_bytes()).unwrap()
    }

    #[test]
    fn patterns_match_components() {
        let p = policy(
            "# comment\n.env*  maintainers\nconfig/prod/** maintainers\n**/*.pem group:ops\n",
        );
        assert_eq!(p.audience(".env"), Audience::Maintainers);
        assert_eq!(p.audience(".env.local"), Audience::Maintainers);
        assert_eq!(p.audience("src/.env"), Audience::Public);
        assert_eq!(p.audience("config/prod"), Audience::Maintainers);
        assert_eq!(p.audience("config/prod/db.yml"), Audience::Maintainers);
        assert_eq!(p.audience("config"), Audience::Public);
        assert_eq!(p.audience("config/production"), Audience::Public);
        assert_eq!(p.audience("a/b/c.pem"), Audience::Group("ops".into()));
        assert_eq!(p.audience("c.pem"), Audience::Group("ops".into()));
    }

    #[test]
    fn the_last_rule_wins_and_the_policy_stays_public() {
        let p = policy("** members\nsrc/** public\n");
        assert_eq!(p.audience("README"), Audience::Members);
        assert_eq!(p.audience("src/main.rs"), Audience::Public);
        assert_eq!(p.audience(POLICY_PATH), Audience::Public);
    }

    #[test]
    fn hidden_entries_start_where_the_audience_narrows() {
        let p = policy("internal/** members\ninternal/keys/** maintainers\n");
        assert_eq!(
            p.hidden_along("internal/keys/a"),
            vec![
                ("internal".len(), Audience::Members),
                ("internal/keys".len(), Audience::Maintainers)
            ]
        );
        assert_eq!(
            p.hidden_along("internal/notes"),
            vec![("internal".len(), Audience::Members)]
        );
        assert!(p.hidden_along("src/x").is_empty());
    }

    #[test]
    fn malformed_policies_are_refused() {
        for text in [
            "onlyone\n",
            "a b c\n",
            "a/**b members\n",
            "a//b members\n",
            "a everyone\n",
            "a group:\n",
            "a group:UPPER\n",
        ] {
            assert!(Policy::parse(text.as_bytes()).is_err(), "{text:?}");
        }
    }

    #[test]
    fn special_characters_in_names_never_escape_a_rule() {
        let p = policy(
            ".env* maintainers\n?secret members\na?b/** maintainers\nkeys/[x] maintainers\n",
        );
        for name in [
            ".env*prod",
            ".env*",
            ".env**",
            ".env?",
            ".env ",
            ".env prod",
            ".env[1]",
            ".env#x",
            ".env\\x",
            ".envé",
            ".env\u{1f512}",
        ] {
            assert_eq!(p.audience(name), Audience::Maintainers, "{name:?}");
        }
        assert_eq!(p.audience("*secret"), Audience::Members);
        assert_eq!(p.audience("?secret"), Audience::Members);
        assert_eq!(p.audience(" secret"), Audience::Members);
        assert_eq!(p.audience("a*b/x"), Audience::Maintainers);
        assert_eq!(p.audience("a?b/x"), Audience::Maintainers);
        assert_eq!(p.audience("keys/[x]"), Audience::Maintainers);
        // Wildcards still need their surroundings to match.
        assert_eq!(p.audience("env*prod"), Audience::Public);
        assert_eq!(p.audience("x.env"), Audience::Public);
        assert_eq!(p.audience("secret"), Audience::Public);
    }

    #[test]
    fn letter_case_can_only_narrow_an_audience() {
        let p = policy("** members\nsrc/** public\n.env* maintainers\n");
        // A name that matches only when case is ignored is still hidden.
        assert_eq!(p.audience(".ENV"), Audience::Maintainers);
        assert_eq!(p.audience(".Env.local"), Audience::Maintainers);
        // A public rule never widens a path through case folding.
        assert_eq!(p.audience("SRC/a"), Audience::Members);
        assert_eq!(p.audience("src/a"), Audience::Public);
        let groups = policy("a/** group:x\nA/** group:y\n");
        assert_eq!(groups.audience("a/f"), Audience::Maintainers);
    }

    #[test]
    fn readers_open_their_audiences() {
        let ops = Audience::Group("ops".into());
        assert!(!Reader::outsider().can_read(&Audience::Members));
        assert!(Reader::member([]).can_read(&Audience::Members));
        assert!(!Reader::member([]).can_read(&Audience::Maintainers));
        assert!(!Reader::member([]).can_read(&ops));
        assert!(Reader::member(["ops".into()]).can_read(&ops));
        assert!(Reader::maintainer().can_read(&ops));
    }
}
