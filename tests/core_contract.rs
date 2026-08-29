use javelin::config::IgnorePolicy;
use javelin::model::{EntryKind, Tree, TreeEntry};
use javelin::objects::{ObjectStore, decode_tree, encode_tree};
use javelin::paths::validate_relative;

#[test]
fn tree_encoding_is_deterministic_and_round_trips() {
    let first = Tree {
        entries: vec![
            TreeEntry {
                path: "z.txt".into(),
                kind: EntryKind::File,
                object_id: Some("a".repeat(64)),
                executable: false,
            },
            TreeEntry {
                path: "a".into(),
                kind: EntryKind::Directory,
                object_id: None,
                executable: false,
            },
        ],
    };
    let second = Tree {
        entries: first.entries.iter().cloned().rev().collect(),
    };
    let encoded = encode_tree(&first).unwrap();
    assert_eq!(encoded, encode_tree(&second).unwrap());
    assert_eq!(
        decode_tree(&encoded).unwrap(),
        Tree {
            entries: vec![first.entries[1].clone(), first.entries[0].clone()],
        }
    );
}

#[test]
fn domain_separation_changes_blob_and_tree_identity() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join(".javelin");
    let objects = ObjectStore::new(&metadata).unwrap();
    let blob = objects.put_blob(b"\0\0\0\0").unwrap();
    let tree = objects.put_tree(&Tree::default()).unwrap();
    assert_ne!(blob, tree);
    assert_eq!(objects.read_blob(&blob).unwrap(), b"\0\0\0\0");
    assert_eq!(objects.read_tree(&tree).unwrap(), Tree::default());
}

#[test]
fn unsafe_paths_are_rejected() {
    for path in [
        "../outside",
        "/absolute",
        ".javelin/store.sqlite3",
        ".javelin-view",
    ] {
        assert!(validate_relative(path).is_err(), "accepted {path}");
    }
    for path in ["src/main.rs", ".env.example", "empty"] {
        assert!(validate_relative(path).is_ok(), "rejected {path}");
    }
}

#[test]
fn ignore_policy_supports_reinclusion_and_exact_secrets() {
    let policy =
        IgnorePolicy::parse(".env\n.env.local\nnode_modules/\n*.log\n!important.log\n").unwrap();
    assert!(policy.ignored(".env", false));
    assert!(!policy.ignored(".env.example", false));
    assert!(policy.ignored("node_modules/pkg/index.js", false));
    assert!(policy.ignored("debug.log", false));
    assert!(!policy.ignored("important.log", false));
}
