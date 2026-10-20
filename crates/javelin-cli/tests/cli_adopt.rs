use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_javelin"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn json(cwd: &Path, args: &[&str], code: i32) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = invoke(cwd, &args);
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let bytes = if code == 0 {
        &output.stdout
    } else {
        &output.stderr
    };
    serde_json::from_slice(bytes).unwrap()
}

fn adopt(project: &Path, operation: &str) -> Value {
    json(
        project.parent().unwrap(),
        &["adopt", project.to_str().unwrap(), "--operation", operation],
        0,
    )
}

/// Materializes World `number` into a fresh workspace and returns its directory.
fn materialize(project: &Path, name: &str, number: &str) -> PathBuf {
    json(
        project,
        &["workspace", "create", name, "--world", number],
        0,
    );
    let located = json(project, &["workspace", "path", name], 0);
    PathBuf::from(located["workspace"]["root"].as_str().unwrap())
}

fn project() -> (TempDir, PathBuf) {
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("proj");
    fs::create_dir_all(root.join("src/deep")).unwrap();
    fs::create_dir_all(root.join("build")).unwrap();
    fs::write(root.join("src/deep/a.txt"), "alpha\n").unwrap();
    fs::write(root.join("readme.md"), "# proj\n").unwrap();
    fs::write(root.join("build/out.bin"), "generated\n").unwrap();
    fs::write(root.join(".javelinignore"), "build/\n").unwrap();
    fs::write(root.join("run.sh"), "#!/bin/sh\necho run\n").unwrap();
    fs::set_permissions(
        root.join("run.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("src/deep/a.txt", root.join("link")).unwrap();
    (parent, root)
}

/// Every file under `.javelin`, read as bytes, to prove a value never reached the store.
/// Bases are sealed to mode 000 by design, so the scan opens them first.
fn stored_bytes(root: &Path) -> Vec<u8> {
    for base in fs::read_dir(root.join(".javelin/bases"))
        .into_iter()
        .flatten()
    {
        let path = base.unwrap().path();
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    let mut all = Vec::new();
    let mut pending = vec![root.join(".javelin")];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                all.extend(fs::read(path).unwrap());
            }
        }
    }
    all
}

#[test]
fn adopt_publishes_nested_files_symlinks_executables_and_honors_ignores() {
    let (_parent, root) = project();
    let adopted = adopt(&root, "first");
    assert_eq!(adopted["files"], 5, "{adopted}");
    assert_eq!(adopted["receipt"]["outcome"]["outcome"], "published");
    assert_eq!(
        json(&root, &["history"], 0)["versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let world = materialize(&root, "view", "1");
    assert_eq!(fs::read(world.join("src/deep/a.txt")).unwrap(), b"alpha\n");
    assert_eq!(fs::read(world.join("readme.md")).unwrap(), b"# proj\n");
    assert_eq!(fs::read(world.join(".javelinignore")).unwrap(), b"build/\n");
    let mode = fs::metadata(world.join("run.sh")).unwrap().permissions();
    assert_ne!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o111, 0);
    let link = fs::symlink_metadata(world.join("link")).unwrap();
    assert!(link.file_type().is_symlink());
    assert_eq!(
        fs::read_link(world.join("link")).unwrap(),
        Path::new("src/deep/a.txt")
    );
    assert!(!world.join("build").exists(), "ignored path was adopted");
}

#[test]
fn a_git_directory_is_never_read_or_imported() {
    const MARKER: &[u8] = b"GIT-MARKER-7f3a9c";
    let (_parent, root) = project();
    let git = root.join(".git");
    fs::create_dir_all(git.join("objects")).unwrap();
    fs::write(
        git.join("HEAD"),
        [b"ref: refs/heads/main\n".as_slice(), MARKER].concat(),
    )
    .unwrap();
    fs::write(git.join("objects/pack.idx"), MARKER).unwrap();
    // Unreadable: adoption must succeed without ever opening anything inside `.git`.
    fs::set_permissions(&git, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();

    let adopted = adopt(&root, "first");
    fs::set_permissions(&git, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    assert_eq!(adopted["files"], 5, "{adopted}");

    let world = materialize(&root, "view", "1");
    assert!(
        !world.join(".git").exists(),
        ".git was imported into the World"
    );
    let stored = stored_bytes(&root);
    assert!(
        !stored.windows(MARKER.len()).any(|window| window == MARKER),
        ".git content reached the repository store"
    );
}

#[test]
fn edits_to_the_original_directory_reach_a_world_only_through_a_layer() {
    let (_parent, root) = project();
    adopt(&root, "first");
    fs::write(root.join("src/deep/a.txt"), "edited outside\n").unwrap();
    fs::write(root.join("sneaked.txt"), "not a layer\n").unwrap();

    assert_eq!(
        json(&root, &["history"], 0)["versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let check = json(&root, &["check"], 0);
    assert_eq!(check["ok"], true, "{check}");
    let world = materialize(&root, "view", "1");
    assert_eq!(fs::read(world.join("src/deep/a.txt")).unwrap(), b"alpha\n");
    assert!(!world.join("sneaked.txt").exists());

    fs::write(world.join("src/deep/a.txt"), "edited in a layer\n").unwrap();
    let published = json(
        &root,
        &["workspace", "publish", "view", "--operation", "layer-edit"],
        0,
    );
    assert_eq!(published["ok"], true, "{published}");
    let next = materialize(&root, "after", "2");
    assert_eq!(
        fs::read(next.join("src/deep/a.txt")).unwrap(),
        b"edited in a layer\n"
    );
    assert!(!next.join("sneaked.txt").exists());
    assert_eq!(
        json(&root, &["history"], 0)["versions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn an_existing_repository_is_refused_without_change() {
    let (_parent, root) = project();
    json(&root, &["init"], 0);
    let before = stored_bytes(&root);
    let refused = invoke(
        root.parent().unwrap(),
        &["adopt", root.to_str().unwrap(), "--operation", "first"],
    );
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("already exists"));
    assert_eq!(stored_bytes(&root), before);
}

#[test]
fn a_completed_adoption_replays_for_its_operation_and_refuses_another() {
    let (_parent, root) = project();
    let first = adopt(&root, "first");
    let replayed = adopt(&root, "first");
    assert_eq!(replayed["receipt"], first["receipt"]);
    let other = invoke(
        root.parent().unwrap(),
        &["adopt", root.to_str().unwrap(), "--operation", "other"],
    );
    assert_eq!(other.status.code(), Some(2), "{other:?}");
    assert!(String::from_utf8_lossy(&other.stderr).contains("already adopted by operation first"));
}

#[test]
fn adopt_requires_an_operation_id() {
    let (_parent, root) = project();
    let missing = invoke(root.parent().unwrap(), &["adopt", root.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    assert!(!root.join(".javelin").exists());
}
