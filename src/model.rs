use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub kind: EntryKind,
    pub object_id: Option<String>,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    pub fn map(&self) -> BTreeMap<String, TreeEntry> {
        self.entries
            .iter()
            .cloned()
            .map(|entry| (entry.path.clone(), entry))
            .collect()
    }

    pub fn from_map(map: BTreeMap<String, TreeEntry>) -> Self {
        Self {
            entries: map.into_values().collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewMarker {
    pub format: u8,
    pub project: String,
    pub layer_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub origin_ref: String,
    pub synchronized_ref: String,
    pub head_checkpoint: String,
    pub target_kind: TargetKind,
    pub target_id: Option<String>,
    pub status: LayerStatus,
    pub view_path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    World,
    Layer,
}

impl TargetKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Layer => "layer",
        }
    }
}

impl fmt::Display for TargetKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TargetKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "world" => Ok(Self::World),
            "layer" => Ok(Self::Layer),
            _ => Err(format!("invalid Layer target kind {value:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayerStatus {
    Active,
    Conflicted,
    Publishing,
    Discarded,
}

impl LayerStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Conflicted => "conflicted",
            Self::Publishing => "publishing",
            Self::Discarded => "discarded",
        }
    }
}

impl fmt::Display for LayerStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for LayerStatus {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "active" => Ok(Self::Active),
            "conflicted" => Ok(Self::Conflicted),
            "publishing" => Ok(Self::Publishing),
            "discarded" => Ok(Self::Discarded),
            _ => Err(format!("invalid Layer status {value:?}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub layer_id: String,
    pub sequence: i64,
    pub previous_checkpoint: Option<String>,
    pub root_tree: String,
    pub synchronized_ref: String,
    pub reason: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldVersion {
    pub id: String,
    pub sequence: i64,
    pub parent_version: Option<String>,
    pub root_tree: String,
    pub accepted_contribution: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Add,
    Modify,
    Delete,
    Type,
    Mode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub change: ChangeKind,
    pub old: Option<TreeEntry>,
    pub new: Option<TreeEntry>,
}
