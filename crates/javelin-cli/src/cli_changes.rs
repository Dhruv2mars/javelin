//! What each path a request revision changes did to its base, for `javelin request show`:
//! added, modified or deleted. The base's tree is read from the root down to each path, from
//! chunks this clone holds, else from the node's `Get`, which serves this token only what it
//! may read, as a fetch does. Nothing new crosses the wire.

use std::cell::RefCell;

use javelin::{
    Error, ObjectId, RepoPath, Repository, SparseChanges, VersionNumber,
    tree::{Node, Trees},
};
use javelin_protocol::{Kind, Message, limits::MAX_TREE};

use crate::{
    CliError,
    cli_remote::{Link, call, unexpected},
};

/// Whether each change in `changes` adds, modifies or deletes its path over remote World
/// `base`, in path order. The base's tree is read from the root down to each path: chunks this
/// clone holds from its own Worlds, the rest from the node, which serves this token only what
/// it may read, as a fetch does. A path under a hidden entry is `changed`: whether the base had
/// it is hidden too.
pub(crate) fn change_kinds(
    link: &RefCell<Link>,
    repo: &Repository,
    base: VersionNumber,
    changes: &SparseChanges,
) -> Result<Vec<&'static str>, CliError> {
    let root = match call(
        link,
        &Message::Resolve {
            version: Some(base),
        },
    )
    .map_err(CliError::Remote)?
    {
        Message::Resolved { number, tree, .. } if number == base => tree,
        _ => return Err(CliError::Remote(unexpected())),
    };
    let trees = Trees::new(NodeTrees { repo, link });
    let mut kinds = Vec::new();
    for (path, entry) in changes.entries() {
        let mut hidden = false;
        let mut prefix = String::new();
        for name in path.as_str().split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            if prefix.len() == path.as_str().len() {
                break;
            }
            let ancestor = RepoPath::parse(&prefix)?;
            if let Some(Node::File(file)) = trees.lookup(root, &ancestor)? {
                hidden = file.is_hidden();
                break;
            }
        }
        let before = trees.lookup(root, path)?.is_some();
        kinds.push(match (hidden, before, entry.is_some()) {
            (true, _, true) => "changed",
            (_, _, false) => "deleted",
            (false, true, true) => "modified",
            (false, false, true) => "added",
        });
    }
    Ok(kinds)
}

/// Tree chunks from this clone's store, else from the node, each checked against its ID.
struct NodeTrees<'a> {
    repo: &'a Repository,
    link: &'a RefCell<Link>,
}

impl javelin::tree::Store for NodeTrees<'_> {
    fn load(&self, id: ObjectId) -> javelin::Result<Vec<u8>> {
        if self.repo.has_tree(id)? {
            return self.repo.tree_bytes_bounded(id, MAX_TREE);
        }
        match call(self.link, &Message::Get(vec![(Kind::Tree, id)])) {
            Ok(Message::Objects(mut objects))
                if objects.len() == 1
                    && objects[0].0 == Kind::Tree
                    && objects[0].1 == id
                    && Kind::Tree.id(&objects[0].2) == id =>
            {
                Ok(objects.remove(0).2)
            }
            Ok(_) => Err(Error::MissingObject(id)),
            Err(error) => Err(Error::InvalidInput(format!(
                "reading tree {id} from the node: {}: {}",
                error.code, error.detail
            ))),
        }
    }
    fn save(&self, _: ObjectId, _: &[u8]) -> javelin::Result<()> {
        Ok(())
    }
}
