//! World numbers as a user sees them. In an attached repository the node's World is the
//! World, so every command shows the node's numbers and IDs, and clones of one repository
//! agree on them. The local store numbers its Worlds from its attach, one per attach or sync,
//! and names the remote version each holds in the World's own operation
//! (`attach-<number>-<id>`, `sync-<number>-<id>`), written in the same transaction as the
//! advance. That record is the map, so it is as crash-safe as the World itself
//! ([`Repository::remote_world`]). A repository with no `attach.json` shows its own numbers.

use javelin::{Error, RemoteHead, Repository, Result, VersionNumber, WorldVersion};
use serde_json::{Value, json};

/// How one repository numbers its Worlds for display.
pub(crate) struct Worlds<'a> {
    repo: &'a Repository,
    attached: bool,
}

impl<'a> Worlds<'a> {
    pub(crate) fn of(repo: &'a Repository) -> Result<Self> {
        Ok(Self {
            repo,
            attached: repo.attachment()?.is_some(),
        })
    }

    /// The node's World that local World `local` holds; `None` when this repository is not
    /// attached or the World was made here and never reached the node.
    pub(crate) fn remote(&self, local: VersionNumber) -> Result<Option<RemoteHead>> {
        if !self.attached {
            return Ok(None);
        }
        self.repo.remote_world(local)
    }

    /// The number to show for local World `local`: the node's when it holds one.
    pub(crate) fn number(&self, local: VersionNumber) -> Result<Value> {
        Ok(match (self.attached, self.remote(local)?) {
            (_, Some(remote)) => json!(remote.number),
            (false, None) => json!(local.get()),
            (true, None) => Value::Null,
        })
    }

    /// `World N` for local World `local`, in the node's numbers when attached.
    pub(crate) fn label(&self, local: VersionNumber) -> Result<String> {
        Ok(match (self.attached, self.remote(local)?) {
            (_, Some(remote)) => format!("World {}", remote.number),
            (false, None) => format!("World {local}"),
            (true, None) => format!("local World {local} (not on the node)"),
        })
    }

    /// A World as JSON: the node's number and ID when attached, with the local number under
    /// `local`. A World made here that never reached the node has `number: null`.
    pub(crate) fn version(&self, version: &WorldVersion) -> Result<Value> {
        if !self.attached {
            return Ok(json!(version));
        }
        let remote = self.remote(version.number)?;
        Ok(json!({
            "number": remote.as_ref().map(|remote| remote.number),
            "id": remote.as_ref().map_or(version.id, |remote| remote.id),
            "tree": version.tree,
            "operation": version.operation,
            "local": version.number,
        }))
    }

    /// The local World for a `--world N` the user typed, which is the node's number when
    /// attached.
    pub(crate) fn local(&self, number: u64) -> Result<VersionNumber> {
        if !self.attached {
            return VersionNumber::new(number);
        }
        self.repo.local_world(number)?.ok_or_else(|| {
            Error::InvalidInput(format!(
                "this clone does not hold World {number}: it holds the Worlds attach and sync \
                 brought here; run `javelin sync` for the node's head"
            ))
        })
    }
}
