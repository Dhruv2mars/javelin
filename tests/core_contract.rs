use javelin::config::{Config, IgnorePolicy};
use javelin::model::{EntryKind, Tree, TreeEntry};
use javelin::objects::ObjectKind;
use javelin::objects::{ObjectStore, decode_tree, encode_tree};
use javelin::paths::validate_relative;
use javelin::store::{NewLayer, Store};
#[cfg(unix)]
use javelin::view::materialize_tree;
use std::fs;

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
fn duplicate_objects_do_not_need_the_temp_directory() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join(".javelin");
    let objects = ObjectStore::new(&metadata).unwrap();
    let first = objects.put_blob(b"same bytes").unwrap();
    fs::remove_dir(metadata.join("temp")).unwrap();

    let duplicate = objects.put_blob(b"same bytes").unwrap();

    assert_eq!(duplicate, first);
}

#[test]
fn object_batch_installs_every_blob_at_commit() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join(".javelin");
    let objects = ObjectStore::new(&metadata).unwrap();
    let mut batch = objects.batch();
    let first = batch.put_blob(b"first").unwrap();
    let second = batch.put_blob(b"second").unwrap();
    let duplicate = batch.put_blob(b"first").unwrap();
    assert_eq!(duplicate, first);
    assert!(objects.read_blob(&first).is_err());

    batch.commit().unwrap();

    assert_eq!(objects.read_blob(&first).unwrap(), b"first");
    assert_eq!(objects.read_blob(&second).unwrap(), b"second");
    assert!(
        fs::read_dir(metadata.join("temp"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn unsafe_paths_are_rejected() {
    for path in [
        "../outside",
        "/absolute",
        ".javelin/store.sqlite3",
        ".javelin-view",
        ".git/config",
        ".hg/store",
        ".svn/wc.db",
        ".JAVELIN/store.sqlite3",
        ".Git/config",
    ] {
        assert!(validate_relative(path).is_err(), "accepted {path}");
    }
    for path in ["src/main.rs", ".env.example", "empty"] {
        assert!(validate_relative(path).is_ok(), "rejected {path}");
    }
}

#[test]
fn config_rejects_zero_timeouts_and_missing_ignore_uses_defaults() {
    let invalid = r#"format = 1

[[verification.rule]]
name = "gate"
command = ["true"]
timeout_seconds = 0
"#;
    assert!(Config::parse(invalid).is_err());

    let temp = tempfile::tempdir().unwrap();
    let policy = IgnorePolicy::load(temp.path()).unwrap();
    assert!(policy.ignored(".env", false));
    fs::create_dir(temp.path().join(".javelinignore")).unwrap();
    assert!(IgnorePolicy::load(temp.path()).is_err());
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

#[test]
fn publish_idempotency_key_keeps_its_original_layer_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("world");
    fs::create_dir_all(&root).unwrap();
    let mut store = Store::create(&root).unwrap();
    let tree = Tree::default();
    let root_tree = store.objects.put_tree(&tree).unwrap();
    store
        .register_object(
            &root_tree,
            ObjectKind::Tree,
            encode_tree(&tree).unwrap().len() as u64,
        )
        .unwrap();
    let (world, _) = store.initialize_world(&root_tree).unwrap();
    let first_view = store.metadata.join("views/first");
    let second_view = store.metadata.join("views/second");
    let first = store
        .create_layer(NewLayer {
            name: "first",
            origin_ref: &world.id,
            synchronized_ref: &world.id,
            root_tree: &root_tree,
            target_kind: "world",
            target_id: None,
            view_path: &first_view,
        })
        .unwrap();
    let second = store
        .create_layer(NewLayer {
            name: "second",
            origin_ref: &world.id,
            synchronized_ref: &world.id,
            root_tree: &root_tree,
            target_kind: "world",
            target_id: None,
            view_path: &second_view,
        })
        .unwrap();
    let first_head = store.layer_head(&first).unwrap();
    let second_head = store.layer_head(&second).unwrap();
    store
        .accept_publish(
            &first,
            &first_head,
            &root_tree,
            Some("shared-key"),
            &[],
            &serde_json::json!({}),
        )
        .unwrap();

    let error = store
        .accept_publish(
            &second,
            &second_head,
            &root_tree,
            Some("shared-key"),
            &[],
            &serde_json::json!({}),
        )
        .unwrap_err();

    assert_eq!(error.exit_code, 6);
    assert!(error.message.contains("different Private Layer"));
}

#[cfg(unix)]
#[test]
fn crafted_symlink_parent_cannot_escape_materialization() {
    let temp = tempfile::tempdir().unwrap();
    let metadata = temp.path().join(".javelin");
    let objects = ObjectStore::new(&metadata).unwrap();
    let link = objects.put_blob(b"../outside").unwrap();
    let payload = objects.put_blob(b"must stay contained").unwrap();
    let tree = Tree {
        entries: vec![
            TreeEntry {
                path: "escape".into(),
                kind: EntryKind::Symlink,
                object_id: Some(link),
                executable: false,
            },
            TreeEntry {
                path: "escape/written.txt".into(),
                kind: EntryKind::File,
                object_id: Some(payload),
                executable: false,
            },
        ],
    };
    let destination = temp.path().join("view");
    assert!(materialize_tree(&tree, &destination, &objects, None).is_err());
    assert!(!temp.path().join("outside/written.txt").exists());
    if let Ok(metadata) = fs::symlink_metadata(destination.join("escape")) {
        assert!(metadata.file_type().is_symlink());
    }
}
