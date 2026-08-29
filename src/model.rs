use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub target_kind: String,
    pub target_id: Option<String>,
    pub status: String,
    pub view_path: String,
    pub created_at: String,
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
