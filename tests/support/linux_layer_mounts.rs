//! Real-mount Linux Layer runtime proof, compiled into the library test binary on
//! Linux only. Every test is ignored by default and refuses to run outside a
//! disposable GitHub-hosted Linux job with an installed root-owned helper.
//! Boundary hooks exist only in this test build; independent processes use pipe
//! barriers, never sleeps.
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::{
    PathDiff, PublicationReceipt, PublishAttribution, PublishOutcome, VersionNumber, WorldVersion,
};

struct Env {
    helper: PathBuf,
    parent: PathBuf,
    evidence: PathBuf,
    inventory: Vec<String>,
}

fn env() -> Env {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    assert_eq!(
        var("GITHUB_ACTIONS"),
        "true",
        "mount tests need disposable CI"
    );
    assert_eq!(var("RUNNER_ENVIRONMENT"), "github-hosted");
    assert_eq!(var("JAVELIN_LINUX_LAYER_MOUNT_TESTS"), "1");
    // SAFETY: getuid has no preconditions.
    assert_ne!(
        unsafe { libc::getuid() },
        0,
        "tools must run as an ordinary user"
    );
    Env {
        helper: var("JAVELIN_LINUX_LAYER_HELPER").into(),
        parent: var("JAVELIN_LINUX_LAYER_PARENT").into(),
        evidence: var("JAVELIN_LINUX_LAYER_EVIDENCE").into(),
        inventory: var("JAVELIN_LINUX_LAYER_INVENTORY")
            .split(' ')
            .map(String::from)
            .collect(),
    }
}

fn helper(env: &Env) -> LayerMountHelper {
    LayerMountHelper::sudo(&env.helper).unwrap()
}

fn save(env: &Env, name: &str, value: &Value) {
    fs::write(
        env.evidence.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}

/// Root-side read-only inventory of the hidden backing tree (hashes, inodes, xattrs).
fn inventory(env: &Env, root: &Path, label: &str) -> Value {
    let output = Command::new(&env.inventory[0])
        .args(&env.inventory[1..])
        .arg(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    save(env, label, &value);
    value
}

fn sh(dir: &Path, script: &str, log: &mut Vec<Value>) -> String {
    let output = Command::new("timeout")
        .args(["20s", "sh", "-c", script])
        .current_dir(dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    log.push(json!({"cwd": dir, "script": script, "code": output.status.code(), "stdout": stdout, "stderr": stderr}));
    assert!(output.status.success(), "{script}: {stderr}");
    stdout
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Exact observable tree: kind, executable bit, content hash or symlink target.
fn tree(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(root.join(&dir)).unwrap() {
            let rel = dir.join(entry.unwrap().file_name());
            let full = root.join(&rel);
            let meta = fs::symlink_metadata(&full).unwrap();
            let key = rel.to_string_lossy().into_owned();
            if meta.is_dir() {
                out.insert(key, "dir".into());
                stack.push(rel);
            } else if meta.file_type().is_symlink() {
                out.insert(
                    key,
                    format!("link:{}", fs::read_link(&full).unwrap().display()),
                );
            } else {
                let exec = meta.mode() & 0o100 != 0;
                out.insert(
                    key,
                    format!("file:{exec}:{}", hex(&fs::read(&full).unwrap())),
                );
            }
        }
    }
    out
}

fn files(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.iter()
        .filter(|(_, v)| *v != "dir")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn fixture_tree(dir: &Path) {
    fs::create_dir_all(dir.join("dir/sub")).unwrap();
    fs::write(dir.join("README.txt"), "shared base text\n").unwrap();
    fs::write(
        dir.join("bytes.bin"),
        (0..8192u32).map(|i| (i % 256) as u8).collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(dir.join("delete.txt"), "delete only in A\n").unwrap();
    fs::write(dir.join("dir/nested.txt"), "nested\n").unwrap();
    fs::write(dir.join("dir/sub/deep.txt"), "deep\n").unwrap();
    fs::write(dir.join("value.h"), "#define VALUE 7\n").unwrap();
    fs::write(
        dir.join("main.c"),
        "#include <stdio.h>\n#include \"value.h\"\nint main(void) { printf(\"%d\\n\", VALUE); return 0; }\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("README.txt", dir.join("inside-link")).unwrap();
}

/// Imports an ordinary tree as World 1. Test setup only; not a product command.
fn repository_from(tree_root: &Path) -> (TempDir, Repository) {
    let dir = TempDir::new().unwrap();
    let mut repo = Repository::init(dir.path()).unwrap();
    let mut upper = HashSet::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(tree_root.join(&d)).unwrap() {
            let rel = d.join(entry.unwrap().file_name());
            if fs::symlink_metadata(tree_root.join(&rel)).unwrap().is_dir() {
                stack.push(rel);
            } else {
                upper.insert(rel.as_os_str().as_bytes().to_vec());
            }
        }
    }
    let snap = snapshot(tree_root, &upper, &Manifest::new([]).unwrap(), &mut |b| {
        repo.put_blob(b)
    })
    .unwrap();
    let manifest = repo
        .put_manifest(&Manifest::new(snap.entries).unwrap())
        .unwrap();
    repo.compare_and_advance(&op("world-1"), VersionNumber::new(0).unwrap(), manifest)
        .unwrap();
    (dir, repo)
}

fn op(s: &str) -> OperationId {
    OperationId::parse(s).unwrap()
}
fn id(s: &str) -> LayerId {
    LayerId::parse(s).unwrap()
}

fn runtime_root(env: &Env, name: &str) -> PathBuf {
    env.parent.join(format!("rt-{name}"))
}

fn active(repo: &Repository, layer: &LayerId) -> LayerHead {
    match repo.layer_state(layer).unwrap() {
        LayerState::Active { head } => head,
        other => panic!("expected active Layer, got {other:?}"),
    }
}

fn applied(update: &LayerUpdate) -> LayerHead {
    match update {
        LayerUpdate::Applied {
            state: LayerState::Active { head },
        } => head.clone(),
        other => panic!("expected applied capture, got {other:?}"),
    }
}

fn os_error(result: std::io::Result<()>) -> i32 {
    result
        .expect_err("write unexpectedly succeeded")
        .raw_os_error()
        .unwrap()
}

fn diff_json(repo: &Repository, diff: &[PathDiff]) -> Value {
    let describe = |entry: &Option<FileEntry>| match entry {
        None => Value::Null,
        Some(FileEntry::Regular {
            content,
            executable,
        }) => {
            json!({"kind": "regular", "executable": executable, "sha256": hex(&repo.read_blob(*content).unwrap())})
        }
        Some(FileEntry::Symlink { target }) => {
            json!({"kind": "symlink", "target": String::from_utf8_lossy(&repo.read_blob(*target).unwrap())})
        }
    };
    diff.iter()
        .map(|d| json!({"path": d.path.as_str(), "before": describe(&d.before), "after": describe(&d.after)}))
        .collect()
}

fn upper_entries(inventory: &Value, slot: &str) -> usize {
    inventory["uppers"][slot]["entries"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn mounted_views_capture_clone_and_discard() {
    let env = env();
    let mut log = Vec::new();
    let source = TempDir::new().unwrap();
    fixture_tree(source.path());
    let fixture = tree(source.path());
    let (_repo_dir, mut repo) = repository_from(source.path());
    let root = runtime_root(&env, "flow");
    let mut rt = LinuxLayerRuntime::init(&repo, &root, helper(&env)).unwrap();
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let (la, lb) = (id("A"), id("a"));
    repo.create_layer(&la, &base).unwrap();
    repo.create_layer(&lb, &base).unwrap();
    let a = rt.mount(&mut repo, &la).unwrap();
    let b = rt.mount(&mut repo, &lb).unwrap();
    assert_ne!(a, b, "case-distinct Layer IDs map to distinct views");
    assert_eq!(
        rt.mount(&mut repo, &la).unwrap(),
        a,
        "remount is idempotent"
    );
    let outside =
        LinuxLayerRuntime::init(&repo, "/var/lib/javelin-not-permitted/rt-x", helper(&env));
    assert!(
        matches!(outside, Err(Error::InvalidInput(_))),
        "unlisted parent accepted"
    );
    // The helper refuses to list a writable (unfrozen) upper even when invoked directly.
    let direct = Command::new("/usr/bin/sudo")
        .arg("-n")
        .arg(&env.helper)
        .args(["list", root.to_str().unwrap(), "view", &slot(&la)])
        .output()
        .unwrap();
    assert_eq!(direct.status.code(), Some(2), "unfrozen upper listed");
    for view in [&a, &b] {
        let c = std::ffi::CString::new(view.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid path and zeroed out-parameter.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::statfs(c.as_ptr(), &mut stat) }, 0);
        assert_eq!(stat.f_type as i64, 0x794c7630, "view is not OverlayFS");
        assert_eq!(tree(view), fixture);
    }
    let initial = inventory(&env, &root, "flow-inventory-initial");
    assert_eq!(
        upper_entries(&initial, &slot(&la)),
        0,
        "fresh upper must be empty"
    );
    assert_eq!(upper_entries(&initial, &slot(&lb)), 0);
    // Readonly base path: kernel refuses writes; hidden backing is unreachable.
    let base_view = rt
        .mount_checkpoint(&repo, active(&repo, &lb).checkpoint())
        .unwrap();
    assert_eq!(tree(&base_view), fixture);
    assert_eq!(
        os_error(fs::write(base_view.join("README.txt"), "x")),
        libc::EROFS
    );
    assert_eq!(
        os_error(fs::read_dir(root.join("hidden")).map(|_| ())),
        libc::EACCES
    );
    // Unmounted window: the root-owned 0555 mountpoint refuses bypass writes.
    rt.unmount(&lb).unwrap();
    assert_eq!(os_error(fs::write(b.join("bypass"), "x")), libc::EACCES);
    rt.mount(&mut repo, &lb).unwrap();
    assert_eq!(tree(&b), fixture);

    // Ordinary developer tools edit A.
    sh(
        &a,
        "printf 'atomic\\n' > .README.tmp && mv .README.tmp README.txt",
        &mut log,
    );
    sh(
        &a,
        "printf 'c' | dd of=bytes.bin bs=1 seek=4096 conv=notrunc status=none",
        &mut log,
    );
    sh(
        &a,
        "rm -r dir && rm delete.txt && chmod +x value.h && ln -s value.h alias.h",
        &mut log,
    );
    assert_eq!(
        sh(
            &a,
            "cc -Wall -Wextra -Werror main.c -o app && ./app",
            &mut log
        ),
        "7\n"
    );
    sh(&a, "mkdir empty-dir", &mut log);
    assert_eq!(
        sh(&a, "find . -type f | sort", &mut log),
        "./README.txt\n./app\n./bytes.bin\n./main.c\n./value.h\n"
    );
    assert_eq!(tree(&b), fixture, "sibling view changed");

    // A writer holding a file open makes capture refuse with no partial state.
    let mut writer = Command::new("sh")
        .args([
            "-c",
            "exec 3>>README.txt; echo ready; read x; printf 'more\\n' >&3",
        ])
        .current_dir(&a)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(writer.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let busy = rt.capture(&mut repo, &la, &op("a-busy"));
    assert!(matches!(busy, Err(Error::Busy(_))), "{busy:?}");
    let status = rt.status(&repo, &la).unwrap();
    assert!(!status.frozen && status.pending.is_none() && status.bound_revision == 0);
    assert_eq!(repo.layer_history(&la).unwrap().len(), 1);
    writer.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
    assert!(writer.wait().unwrap().success());
    assert_eq!(fs::read(a.join("README.txt")).unwrap(), b"atomic\nmore\n");

    let first = rt.capture(&mut repo, &la, &op("a-1")).unwrap();
    assert_eq!(
        first.unrepresented_directories,
        vec!["empty-dir".to_string()]
    );
    let c1 = applied(&first.update);
    // A caller that lost the reply retries the same operation and gets the same outcome.
    let retried = rt.capture(&mut repo, &la, &op("a-1")).unwrap();
    assert_eq!(applied(&retried.update), c1);
    assert_eq!(repo.layer_history(&la).unwrap().len(), 2);
    // An unreadable file fails capture without leaving the view frozen.
    sh(&a, "printf s > secret && chmod 000 secret", &mut log);
    assert!(matches!(
        rt.capture(&mut repo, &la, &op("a-unreadable")),
        Err(Error::Io(_))
    ));
    let status = rt.status(&repo, &la).unwrap();
    assert!(!status.frozen && status.pending.is_none());
    sh(&a, "chmod 644 secret && rm secret", &mut log);
    let diff = repo.layer_diff(&c1).unwrap();
    let described = diff_json(&repo, &diff);
    let paths: Vec<&str> = diff.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "README.txt",
            "alias.h",
            "app",
            "bytes.bin",
            "delete.txt",
            "dir/nested.txt",
            "dir/sub/deep.txt",
            "value.h"
        ]
    );
    let after = |p: &str| {
        diff.iter()
            .find(|d| d.path.as_str() == p)
            .unwrap()
            .after
            .clone()
    };
    let blob = |e: Option<FileEntry>| repo.read_blob(e.unwrap().content()).unwrap();
    assert_eq!(blob(after("README.txt")), b"atomic\nmore\n");
    let mut bytes: Vec<u8> = (0..8192u32).map(|i| (i % 256) as u8).collect();
    bytes[4096] = b'c';
    assert_eq!(blob(after("bytes.bin")), bytes);
    assert!(after("dir/nested.txt").is_none() && after("dir/sub/deep.txt").is_none());
    assert!(matches!(
        after("value.h"),
        Some(FileEntry::Regular {
            executable: true,
            ..
        })
    ));
    assert!(matches!(
        after("app"),
        Some(FileEntry::Regular {
            executable: true,
            ..
        })
    ));
    assert!(matches!(after("alias.h"), Some(FileEntry::Symlink { .. })));
    assert_eq!(blob(after("alias.h")), b"value.h");
    repo.check().unwrap();
    let at_c1 = tree(&a);
    let after_capture = inventory(&env, &root, "flow-inventory-after-capture");
    assert_eq!(
        after_capture["caches"]["b".to_string() + &base.manifest().to_string()],
        initial["caches"]["b".to_string() + &base.manifest().to_string()],
        "base cache changed"
    );

    // Later edits do not alter the frozen checkpoint.
    sh(&a, "printf 'later\\n' > README.txt && rm app", &mut log);
    let c2 = applied(&rt.capture(&mut repo, &la, &op("a-2")).unwrap().update);
    assert_eq!(c2.revision(), 2);

    // Clone the older checkpoint into a separate sparse view.
    let lc = id("clone.of.A");
    repo.clone_layer(&lc, c1.checkpoint()).unwrap();
    let c = rt.mount(&mut repo, &lc).unwrap();
    assert_eq!(files(&tree(&c)), files(&at_c1));
    assert!(
        !c.join("empty-dir").exists(),
        "empty directories are not represented"
    );
    let cloned = inventory(&env, &root, "flow-inventory-clone");
    assert_eq!(
        upper_entries(&cloned, &slot(&lc)),
        0,
        "clone upper must start empty"
    );
    let delta_key = format!("c{}", c1.checkpoint().object());
    let delta = cloned["caches"][&delta_key]["entries"].as_array().unwrap();
    assert!(
        delta.iter().all(|e| e["path"] != "main.c"),
        "delta cache copied untouched base file"
    );
    assert!(
        delta
            .iter()
            .any(|e| e["path"] == "dir" && e["type"] == "whiteout")
    );
    assert_eq!(
        sh(
            &c,
            "printf '#define VALUE 11\\n' > value.h && cc main.c -o app2 && ./app2",
            &mut log
        ),
        "11\n"
    );
    assert_eq!(
        sh(&a, "cc main.c -o app3 && ./app3 && rm app3", &mut log),
        "7\n"
    );
    assert_eq!(fs::read(a.join("README.txt")).unwrap(), b"later\n");

    // Exact immutable checkpoint view is read-only and matches the clone source.
    let ro = rt.mount_checkpoint(&repo, c1.checkpoint()).unwrap();
    assert_eq!(files(&tree(&ro)), files(&at_c1));
    assert_eq!(os_error(fs::write(ro.join("README.txt"), "x")), libc::EROFS);
    rt.unmount_checkpoint(&repo, c1.checkpoint()).unwrap();

    // Another caller advances A natively; binding capture is stale and keeps edits.
    let external = repo
        .capture_layer(
            &op("external"),
            &active(&repo, &la),
            &SparseChanges::new([]).unwrap(),
        )
        .unwrap();
    applied(&external);
    sh(&a, "printf 'kept\\n' > kept.txt", &mut log);
    let stale = rt.capture(&mut repo, &la, &op("a-3")).unwrap();
    assert!(matches!(stale.update, LayerUpdate::Stale { .. }));
    assert_eq!(fs::read(a.join("kept.txt")).unwrap(), b"kept\n");
    let status = rt.status(&repo, &la).unwrap();
    assert_eq!((status.bound_revision, status.frozen), (2, false));

    // Discard the clone: terminal native event, bounded cleanup, siblings intact.
    let discarded = rt.discard(&mut repo, &lc, &op("discard-clone")).unwrap();
    assert!(matches!(
        discarded,
        LayerUpdate::Applied {
            state: LayerState::Discarded { .. }
        }
    ));
    assert!(!c.exists(), "discarded view mountpoint remains");
    assert!(rt.status(&repo, &lc).is_err());
    assert_eq!(repo.layer_history(&lc).unwrap().len(), 2);
    assert_eq!(repo.history().unwrap().len(), 2, "World advanced");
    assert_eq!(tree(&b), fixture);
    assert_eq!(tree(&base_view), fixture);
    assert_eq!(fs::read(a.join("kept.txt")).unwrap(), b"kept\n");
    let final_inventory = inventory(&env, &root, "flow-inventory-final");
    assert!(
        final_inventory["uppers"].get(slot(&lc)).is_none(),
        "clone storage remains"
    );
    assert_eq!(
        final_inventory["caches"], cloned["caches"],
        "immutable caches changed"
    );
    let report = repo.check().unwrap();
    save(
        &env,
        "flow",
        &json!({
            "verdict": "VERIFIED",
            "views": {"A": a, "a": b, "clone": c, "base": base_view},
            "firstCaptureDiff": described,
            "unrepresentedDirectories": first.unrepresented_directories,
            "integrity": report,
            "commands": log,
        }),
    );
    rt.unmount_checkpoint(&repo, active(&repo, &lb).checkpoint())
        .unwrap();
    rt.unmount(&la).unwrap();
    rt.unmount(&lb).unwrap();
}

struct Actor {
    child: Child,
    lines: Receiver<String>,
}

impl Actor {
    fn spawn(
        env: &Env,
        repo: &Path,
        runtime: &Path,
        mode: &str,
        layer: &str,
        operation: &str,
        boundary: &str,
    ) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "linux_layer::mount_tests::runtime_actor",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("JAVELIN_TEST_ROOT", repo)
            .env("JAVELIN_TEST_RUNTIME", runtime)
            .env("JAVELIN_TEST_MODE", mode)
            .env("JAVELIN_TEST_LAYER", layer)
            .env("JAVELIN_TEST_OPERATION", operation)
            .env("JAVELIN_TEST_BOUNDARY", boundary)
            .env("JAVELIN_LINUX_LAYER_HELPER", &env.helper);
        Self::start(command)
    }
    fn start(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }
    /// Next line containing any prefix, or `None` once the child closes stdout.
    fn next_of(&mut self, prefixes: &[&str]) -> Option<(usize, String)> {
        loop {
            let line = match self.lines.recv_timeout(Duration::from_secs(60)) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(e) => panic!("child did not reach barrier: {e}"),
            };
            for (i, prefix) in prefixes.iter().enumerate() {
                if let Some(index) = line.find(prefix) {
                    return Some((i, line[index + prefix.len()..].trim().into()));
                }
            }
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        let _ = self.child.stdin.as_mut().unwrap().write_all(bytes);
    }
    fn marker(&mut self, prefix: &str) -> String {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .expect("actor did not reach barrier");
            // libtest may print `test <name> ... ` on the same line first.
            if let Some(index) = line.find(prefix) {
                return line[index + prefix.len()..].trim().into();
            }
        }
    }
    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
    fn release(mut self) -> String {
        self.child.stdin.as_mut().unwrap().write_all(b"x").unwrap();
        let result = self.marker("RESULT ");
        assert!(self.child.wait().unwrap().success());
        result
    }
}

#[test]
#[ignore = "child process entry point for runtime process-loss tests"]
fn runtime_actor() {
    let Ok(mode) = std::env::var("JAVELIN_TEST_MODE") else {
        return;
    };
    let var = |n: &str| std::env::var(n).unwrap();
    let mut repo = Repository::open(var("JAVELIN_TEST_ROOT")).unwrap();
    let helper = LayerMountHelper::sudo(var("JAVELIN_LINUX_LAYER_HELPER")).unwrap();
    let mut rt = LinuxLayerRuntime::open(&repo, var("JAVELIN_TEST_RUNTIME"), helper).unwrap();
    let layer = id(&var("JAVELIN_TEST_LAYER"));
    let operation = op(&var("JAVELIN_TEST_OPERATION"));
    let result = match mode.as_str() {
        "capture" => format!(
            "{:?}",
            rt.capture(&mut repo, &layer, &operation).map(|o| o.update)
        ),
        "discard" => format!("{:?}", rt.discard(&mut repo, &layer, &operation)),
        "publish" => format!(
            "{:?}",
            rt.publish(
                &mut repo,
                &layer,
                &operation,
                &PublishAttribution::Human,
                |_, _| Ok(()),
            )
        ),
        "rebind" => {
            let status = rt.status(&repo, &layer).unwrap();
            let expected =
                match &repo.layer_history(&layer).unwrap()[status.bound_revision as usize] {
                    LayerState::Active { head } => head.clone(),
                    _ => panic!("bound head must be active"),
                };
            let desired = active(&repo, &layer);
            format!("{:?}", rt.rebind(&mut repo, &expected, &desired))
        }
        "native_publish" => {
            let head = active(&repo, &layer);
            format!(
                "{:?}",
                repo.publish_layer(&operation, &head, &PublishAttribution::Human, |_, _| Ok(()))
            )
        }
        "mount" => format!("{:?}", rt.mount(&mut repo, &layer)),
        other => panic!("unknown actor mode {other}"),
    };
    println!("RESULT {result}");
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn process_loss_and_restart_converge() {
    let env = env();
    let mut log = Vec::new();
    let mut evidence = serde_json::Map::new();
    let source = TempDir::new().unwrap();
    fixture_tree(source.path());
    let (repo_dir, mut repo) = repository_from(source.path());
    let root = runtime_root(&env, "recovery");
    let mut rt = LinuxLayerRuntime::init(&repo, &root, helper(&env)).unwrap();
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let la = id("A");
    repo.create_layer(&la, &base).unwrap();
    let a = rt.mount(&mut repo, &la).unwrap();

    // Concurrent runtime callers: the holder of the runtime lock wins; others get busy.
    sh(&a, "printf 'one\\n' > one.txt", &mut log);
    let mut holder = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "lock-holder",
        "capture_frozen",
    );
    holder.marker("BOUNDARY capture_frozen");
    assert!(matches!(
        rt.capture(&mut repo, &la, &op("contender")),
        Err(Error::Busy(_))
    ));
    assert!(holder.release().starts_with("Ok(Applied"));
    assert_eq!(repo.layer_history(&la).unwrap().len(), 2);
    evidence.insert(
        "lock".into(),
        json!("independent capture held the runtime lock; contender got busy; holder applied"),
    );

    // Killed after freeze and pending record, before native capture.
    sh(&a, "printf 'two\\n' > two.txt", &mut log);
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "killed-frozen",
        "capture_frozen",
    );
    actor.marker("BOUNDARY capture_frozen");
    actor.kill();
    let status = rt.status(&repo, &la).unwrap();
    assert!(status.frozen && status.pending.as_deref() == Some("killed-frozen"));
    assert_eq!(os_error(fs::write(a.join("blocked"), "x")), libc::EROFS);
    let report = rt.recover(&mut repo).unwrap();
    assert_eq!(report.pending_resolved, 1);
    assert_eq!(repo.layer_history(&la).unwrap().len(), 3);
    assert!(!rt.status(&repo, &la).unwrap().frozen);
    evidence.insert(
        "killedBeforeNative".into(),
        json!({"recovery": report, "revisions": 3}),
    );

    // Killed after native commit, before the binding record update.
    sh(&a, "printf 'three\\n' > three.txt", &mut log);
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "killed-committed",
        "capture_committed",
    );
    actor.marker("BOUNDARY capture_committed");
    actor.kill();
    assert_eq!(
        repo.layer_history(&la).unwrap().len(),
        4,
        "native commit happened"
    );
    assert_eq!(rt.status(&repo, &la).unwrap().bound_revision, 2);
    let report = rt.recover(&mut repo).unwrap();
    assert_eq!(report.pending_resolved, 1);
    assert_eq!(
        repo.layer_history(&la).unwrap().len(),
        4,
        "retry appended twice"
    );
    assert_eq!(rt.status(&repo, &la).unwrap().bound_revision, 3);
    let head = active(&repo, &la);
    let names: Vec<_> = repo
        .layer_diff(&head)
        .unwrap()
        .into_iter()
        .map(|d| d.path.as_str().to_string())
        .collect();
    assert_eq!(names, ["one.txt", "three.txt", "two.txt"]);
    evidence.insert(
        "killedAfterNative".into(),
        json!({"recovery": report, "revisions": 4}),
    );

    // Restart: unmount with private edits, reopen a new runtime handle, edits survive.
    sh(&a, "printf 'private\\n' > private.txt", &mut log);
    rt.unmount(&la).unwrap();
    drop(rt);
    let mut rt = LinuxLayerRuntime::open(&repo, &root, helper(&env)).unwrap();
    rt.mount(&mut repo, &la).unwrap();
    assert_eq!(fs::read(a.join("private.txt")).unwrap(), b"private\n");

    // Killed while staging a checkpoint cache: incomplete cache is never selected.
    let clone = id("clone");
    repo.clone_layer(&clone, head.checkpoint()).unwrap();
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "mount",
        "clone",
        "unused",
        "cache_staged",
    );
    actor.marker("BOUNDARY cache_staged");
    actor.kill();
    let key = format!("c{}", head.checkpoint().object());
    let staged = inventory(&env, &root, "recovery-inventory-staged");
    assert!(
        staged["stages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == &json!(key))
    );
    assert!(
        staged["caches"].get(&key).is_none(),
        "incomplete cache selected"
    );
    let c = rt.mount(&mut repo, &clone).unwrap();
    assert_eq!(fs::read(c.join("three.txt")).unwrap(), b"three\n");
    assert!(!c.join("private.txt").exists());
    let rebuilt = inventory(&env, &root, "recovery-inventory-rebuilt");
    assert!(rebuilt["stages"].as_array().unwrap().is_empty());
    assert!(rebuilt["caches"].get(&key).is_some());

    // Killed during discard after unmount; recovery completes the same discard.
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "discard",
        "clone",
        "discard-clone",
        "discard_unmounted",
    );
    actor.marker("BOUNDARY discard_unmounted");
    actor.kill();
    assert!(mount_state(&c).unwrap().is_none());
    let report = rt.recover(&mut repo).unwrap();
    assert_eq!(report.pending_resolved, 1);
    assert!(matches!(
        repo.layer_state(&clone).unwrap(),
        LayerState::Discarded { .. }
    ));
    assert!(!c.exists());
    assert_eq!(fs::read(a.join("private.txt")).unwrap(), b"private\n");
    evidence.insert("killedDuringDiscard".into(), json!({"recovery": report}));

    // A shared writable mapping whose descriptor is closed: record whether the
    // read-only freeze excludes its writes on this kernel instead of assuming it.
    fs::write(a.join("m.bin"), vec![b'A'; 4096]).unwrap();
    let mut mapper_command = Command::new("timeout");
    mapper_command
        .args(["60s", "python3", "-I", "-c"])
        .arg("import mmap,sys\nf=open('m.bin','r+b')\nm=mmap.mmap(f.fileno(),0)\nf.close()\nprint('ready',flush=True)\nsys.stdin.readline()\nm[0:1]=b'B'\nm.flush()\nprint('wrote',flush=True)\nsys.stdin.readline()\n")
        .current_dir(&a);
    let mut mapper = Actor::start(mapper_command);
    mapper.marker("ready");
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "mmap-capture",
        "capture_frozen",
    );
    let mmap = match actor.next_of(&["BOUNDARY capture_frozen", "RESULT "]) {
        Some((0, _)) => {
            mapper.send(b"go\n");
            let wrote = mapper.next_of(&["wrote"]).is_some();
            let during = fs::read(a.join("m.bin")).unwrap()[0];
            let result = actor.release();
            let head = active(&repo, &la);
            let captured = repo
                .layer_diff(&head)
                .unwrap()
                .into_iter()
                .find(|d| d.path.as_str() == "m.bin")
                .and_then(|d| d.after)
                .map(|e| repo.read_blob(e.content()).unwrap()[0]);
            let bypass = wrote && during == b'B';
            json!({
                "verdict": if bypass { "NOT VERIFIED" } else { "VERIFIED" },
                "freezeAllowed": true, "mapperWroteWhileFrozen": wrote,
                "viewByteWhileFrozen": (during as char).to_string(),
                "capturedByte": captured.map(|b| (b as char).to_string()),
                "captureResult": result,
                "scope": "MAP_SHARED|PROT_WRITE mapping after close(2); kernel-specific",
            })
        }
        Some((_, result)) => {
            assert!(actor.child.wait().unwrap().success());
            json!({"verdict": "VERIFIED", "freezeAllowed": false, "captureResult": result})
        }
        None => panic!("capture actor exited without a result"),
    };
    mapper.send(b"done\n");
    let _ = mapper.child.wait();
    evidence.insert("sharedWritableMapping".into(), mmap);

    let final_inventory = inventory(&env, &root, "recovery-inventory-final");
    assert!(final_inventory["stages"].as_array().unwrap().is_empty());
    repo.check().unwrap();
    evidence.insert("verdict".into(), json!("VERIFIED"));
    evidence.insert("commands".into(), json!(log));
    save(&env, "recovery", &Value::Object(evidence));
    rt.unmount(&la).unwrap();
}

#[test]
#[ignore = "requires disposable hosted Linux and the pinned common probe"]
fn common_probe_against_runtime_views() {
    let env = env();
    let mut log = Vec::new();
    let probe = PathBuf::from(std::env::var("JAVELIN_COMMON_PROBE").unwrap());
    let bun = std::env::var("JAVELIN_BUN").unwrap();
    let source = TempDir::new().unwrap();
    let seeded = Command::new(&bun)
        .arg(&probe)
        .arg("--seed")
        .arg(source.path())
        .output()
        .unwrap();
    assert!(seeded.status.success());
    let (_repo_dir, mut repo) = repository_from(source.path());
    let root = runtime_root(&env, "probe");
    let mut rt = LinuxLayerRuntime::init(&repo, &root, helper(&env)).unwrap();
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let mut paths = serde_json::Map::new();
    for (key, name) in [
        ("layerA", "probe-A"),
        ("layerB", "probe-B"),
        ("clone", "probe-C"),
    ] {
        repo.create_layer(&id(name), &base).unwrap();
        paths.insert(key.into(), json!(rt.mount(&mut repo, &id(name)).unwrap()));
    }
    let base_view = rt
        .mount_checkpoint(&repo, active(&repo, &id("probe-B")).checkpoint())
        .unwrap();
    paths.insert("base".into(), json!(base_view));
    let paths_file = env.evidence.join("probe-paths.json");
    fs::write(&paths_file, serde_json::to_vec(&paths).unwrap()).unwrap();
    let out = env.evidence.join("common-probe.json");
    let status = Command::new("timeout")
        .args(["70s", &bun])
        .arg(&probe)
        .args(["--backend", "paths", "--paths"])
        .arg(&paths_file)
        .arg("--out")
        .arg(&out)
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(1),
        "probe must report its one INCONCLUSIVE caller-path clone check"
    );
    let report: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    assert!(report["fatal"].is_null());
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 11);
    for check in checks {
        let expected = if check["id"] == "layer-clone-snapshot-isolation" {
            "INCONCLUSIVE"
        } else {
            "VERIFIED"
        };
        assert_eq!(check["verdict"], expected, "{}", check["id"]);
    }

    // The probe's live edits enter native state through a frozen capture.
    let la = id("probe-A");
    let a = PathBuf::from(paths["layerA"].as_str().unwrap());
    let captured = applied(
        &rt.capture(&mut repo, &la, &op("probe-capture"))
            .unwrap()
            .update,
    );
    let diff = repo.layer_diff(&captured).unwrap();
    let names: Vec<_> = diff.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        names,
        [
            "README.txt",
            "bytes.bin",
            "created.txt",
            "delete.txt",
            "dir/nested.txt",
            "renamed-dir/nested.txt",
            "value.h"
        ]
    );
    let described = diff_json(&repo, &diff);

    // Separate backend clone proof from the exact captured checkpoint.
    let lc = id("probe-A-clone");
    repo.clone_layer(&lc, captured.checkpoint()).unwrap();
    let c = rt.mount(&mut repo, &lc).unwrap();
    assert_eq!(files(&tree(&c)), files(&tree(&a)));
    assert_eq!(
        sh(&c, "cc main.c -o app && ./app && rm app", &mut log),
        "9\n"
    );
    sh(&c, "printf '#define VALUE 11\\n' > value.h", &mut log);
    assert_eq!(
        sh(&c, "cc main.c -o app && ./app && rm app", &mut log),
        "11\n"
    );
    assert_eq!(
        sh(&a, "cc main.c -o app && ./app && rm app", &mut log),
        "9\n"
    );
    let inv = inventory(&env, &root, "probe-inventory-final");
    save(
        &env,
        "probe",
        &json!({
            "verdict": "VERIFIED",
            "commonProbe": "10 VERIFIED; caller-path clone INCONCLUSIVE by design",
            "captureDiff": described,
            "cloneUpperEntries": upper_entries(&inv, &slot(&lc)),
            "commands": log,
        }),
    );
    rt.unmount_checkpoint(&repo, active(&repo, &id("probe-B")).checkpoint())
        .unwrap();
    for name in ["probe-A", "probe-B", "probe-C", "probe-A-clone"] {
        rt.unmount(&id(name)).unwrap();
    }
}

struct Setup {
    _source: TempDir,
    repo_dir: TempDir,
    repo: Repository,
    rt: LinuxLayerRuntime,
    root: PathBuf,
    layer: LayerId,
    view: PathBuf,
}

fn setup(env: &Env, name: &str) -> Setup {
    let source = TempDir::new().unwrap();
    fixture_tree(source.path());
    let (repo_dir, mut repo) = repository_from(source.path());
    let root = runtime_root(env, name);
    let mut rt = LinuxLayerRuntime::init(&repo, &root, helper(env)).unwrap();
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let layer = id("A");
    repo.create_layer(&layer, &base).unwrap();
    let view = rt.mount(&mut repo, &layer).unwrap();
    Setup {
        _source: source,
        repo_dir,
        repo,
        rt,
        root,
        layer,
        view,
    }
}

fn published(receipt: &PublicationReceipt) -> &WorldVersion {
    match &receipt.outcome {
        PublishOutcome::Published { version } => version,
        other => panic!("expected successful publication, got {other:?}"),
    }
}

/// A separate ordinary process holds a writable descriptor across the call.
fn publication_writer(view: &Path) -> Actor {
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "exec 3>>README.txt; echo READY; head -c 1 >/dev/null; printf 'writer bytes\\n' >&3; exec 3>&-; echo RESULT written",
        ])
        .current_dir(view);
    let mut writer = Actor::start(command);
    writer.marker("READY");
    writer
}

/// A deliberately disjoint prepared fixture, committed through the public native
/// Stack storage API. No adapter dependency or merge algorithm lives in this test.
fn stacked(s: &mut Setup) -> (LayerHead, LayerHead, LayerHead) {
    fs::write(s.view.join("owned.txt"), b"target checkpoint bytes\n").unwrap();
    let old = applied(
        &s.rt
            .capture(&mut s.repo, &s.layer, &op("rebind-old"))
            .unwrap()
            .update,
    );
    let source = id("source");
    let base = s.repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    s.repo.create_layer(&source, &base).unwrap();
    let blob = s.repo.put_blob(b"#define VALUE 42\n").unwrap();
    let source_changes = SparseChanges::new([
        (
            RepoPath::parse("value.h").unwrap(),
            Some(FileEntry::Regular {
                content: blob,
                executable: false,
            }),
        ),
        (RepoPath::parse("delete.txt").unwrap(), None),
    ])
    .unwrap();
    let source_head = applied(
        &s.repo
            .capture_layer(
                &op("rebind-source"),
                &active(&s.repo, &source),
                &source_changes,
            )
            .unwrap(),
    );
    let mut entries = s
        .repo
        .read_checkpoint(old.checkpoint())
        .unwrap()
        .changes()
        .entries()
        .clone();
    entries.extend(source_changes.entries().clone());
    let desired = applied(
        &s.repo
            .commit_stack(
                &op("rebind-stack"),
                &old,
                source_head.checkpoint(),
                &SparseChanges::new(entries).unwrap(),
            )
            .unwrap(),
    );
    assert_eq!(
        s.repo
            .read_checkpoint(desired.checkpoint())
            .unwrap()
            .stack_sources(),
        &[source_head.checkpoint()]
    );
    (old, desired, source_head)
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn stack_rebind_tools_checkpoint_and_publish() {
    let env = env();
    let mut s = setup(&env, "rebind-flow");
    let (old, desired, source) = stacked(&mut s);
    let world = s.repo.history().unwrap();
    let sibling = s.rt.mount(&mut s.repo, source.layer()).unwrap();
    let sibling_tree = tree(&sibling);
    let sibling_history = s.repo.layer_history(source.layer()).unwrap();
    let old_checkpoint = s.repo.read_checkpoint(old.checkpoint()).unwrap();
    assert_eq!(
        fs::read(s.view.join("value.h")).unwrap(),
        b"#define VALUE 7\n"
    );
    s.rt.rebind(&mut s.repo, &old, &desired).unwrap();
    let status = s.rt.status(&s.repo, &s.layer).unwrap();
    assert!(status.mounted && !status.frozen && status.pending.is_none());
    assert_eq!(status.bound_revision, desired.revision());
    assert_eq!(
        fs::read(s.view.join("value.h")).unwrap(),
        b"#define VALUE 42\n"
    );
    assert!(!s.view.join("delete.txt").exists());
    assert_eq!(
        fs::read(s.view.join("owned.txt")).unwrap(),
        b"target checkpoint bytes\n"
    );
    assert_eq!(s.repo.history().unwrap(), world);
    assert_eq!(
        s.repo.layer_history(source.layer()).unwrap(),
        sibling_history
    );
    assert_eq!(tree(&sibling), sibling_tree);
    assert_eq!(
        s.repo.read_checkpoint(old.checkpoint()).unwrap().manifest(),
        old_checkpoint.manifest()
    );
    let inv = inventory(&env, &s.root, "rebind-flow-inventory");
    assert!(inv["stages"].as_array().unwrap().is_empty());
    let mut log = Vec::new();
    assert_eq!(
        sh(
            &s.view,
            "cc main.c -o app && ./app && rm app && printf 'tool edit\\n' > tool.txt",
            &mut log
        ),
        "42\n"
    );
    // Exact completed retry preserves later private work.
    let edited = tree(&s.view);
    s.rt.rebind(&mut s.repo, &old, &desired).unwrap();
    assert_eq!(tree(&s.view), edited);
    let captured = applied(
        &s.rt
            .capture(&mut s.repo, &s.layer, &op("rebind-tool-capture"))
            .unwrap()
            .update,
    );
    assert!(captured.revision() > desired.revision());
    let receipt =
        s.rt.publish(
            &mut s.repo,
            &s.layer,
            &op("rebind-publish"),
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(published(&receipt).number.get(), 2);
    let manifest = s.repo.read_manifest(published(&receipt).manifest).unwrap();
    assert_eq!(
        s.repo
            .read_blob(manifest.entries()[&RepoPath::parse("tool.txt").unwrap()].content())
            .unwrap(),
        b"tool edit\n"
    );
    assert!(s.rt.rebind(&mut s.repo, &old, &desired).is_err());
    assert!(s.rt.status(&s.repo, &s.layer).unwrap().frozen);
    s.repo.check().unwrap();
    save(
        &env,
        "rebind-flow",
        &json!({"verdict":"VERIFIED", "old":old, "desired":desired, "capture":captured, "publication":receipt, "commands":log}),
    );
    s.rt.unmount(&s.layer).unwrap();
    s.rt.unmount(source.layer()).unwrap();
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn rebind_refuses_edits_writers_mappings_and_wrong_heads() {
    let env = env();
    let mut s = setup(&env, "rebind-refusals");
    let (old, desired, source) = stacked(&mut s);
    let before = tree(&s.view);
    let history = s.repo.layer_history(&s.layer).unwrap();
    let foreign_dir = TempDir::new().unwrap();
    let mut foreign = Repository::init(foreign_dir.path()).unwrap();
    let base = foreign.layer_base(VersionNumber::new(0).unwrap()).unwrap();
    let foreign_head = foreign.create_layer(&s.layer, &base).unwrap();
    assert!(s.rt.rebind(&mut s.repo, &foreign_head, &desired).is_err());
    assert!(s.rt.rebind(&mut s.repo, &old, &foreign_head).is_err());
    assert!(
        s.rt.rebind(&mut foreign, &foreign_head, &foreign_head)
            .is_err()
    );
    assert!(s.rt.rebind(&mut s.repo, &source, &desired).is_err());
    assert!(s.rt.rebind(&mut s.repo, &old, &source).is_err());
    assert!(s.rt.rebind(&mut s.repo, &desired, &desired).is_err());
    assert!(s.rt.rebind(&mut s.repo, &old, &old).is_err());
    let writer = publication_writer(&s.view);
    let result = s.rt.rebind(&mut s.repo, &old, &desired);
    assert!(matches!(result, Err(Error::Busy(_))), "{result:?}");
    assert_eq!(writer.release(), "written");
    assert_eq!(
        fs::read(s.view.join("README.txt")).unwrap(),
        b"shared base text\nwriter bytes\n"
    );
    let edits = tree(&s.view);
    for unmounted in [false, true] {
        if unmounted {
            s.rt.unmount(&s.layer).unwrap();
        }
        let result = s.rt.rebind(&mut s.repo, &old, &desired);
        assert!(matches!(result, Err(Error::InvalidInput(_))), "{result:?}");
        assert_eq!(s.rt.status(&s.repo, &s.layer).unwrap().mounted, !unmounted);
        s.rt.mount(&mut s.repo, &s.layer).unwrap();
        assert_eq!(tree(&s.view), edits);
    }
    fs::write(s.view.join("README.txt"), b"shared base text\n").unwrap();
    assert_eq!(tree(&s.view), before);
    let mut command = Command::new("python3");
    command.args(["-I", "-c", "import mmap,sys\nf=open('README.txt','r+b')\nm=mmap.mmap(f.fileno(),0)\nf.close()\nprint('ready',flush=True)\nsys.stdin.readline()\nm[0:1]=b'S'\nm.flush()\nprint('RESULT written',flush=True)"]).current_dir(&s.view);
    let mut mapper = Actor::start(command);
    mapper.marker("ready");
    let result = s.rt.rebind(&mut s.repo, &old, &desired);
    assert!(matches!(result, Err(Error::Busy(_))), "{result:?}");
    assert_eq!(mapper.release(), "written");
    assert_eq!(
        fs::read(s.view.join("README.txt")).unwrap(),
        b"Shared base text\n"
    );
    fs::write(s.view.join("README.txt"), b"shared base text\n").unwrap();
    assert_eq!(
        s.rt.status(&s.repo, &s.layer).unwrap().bound_revision,
        old.revision()
    );
    assert_eq!(tree(&s.view), before);
    assert_eq!(s.repo.layer_history(&s.layer).unwrap(), history);
    s.rt.unmount(&s.layer).unwrap();
    s.rt.rebind(&mut s.repo, &old, &desired).unwrap();
    assert!(!s.rt.status(&s.repo, &s.layer).unwrap().mounted);
    assert!(s.rt.recover(&mut s.repo).unwrap().failures.is_empty());
    assert!(!s.rt.status(&s.repo, &s.layer).unwrap().mounted);
    s.rt.mount(&mut s.repo, &s.layer).unwrap();
    assert_eq!(
        fs::read(s.view.join("value.h")).unwrap(),
        b"#define VALUE 42\n"
    );
    s.rt.unmount(&s.layer).unwrap();
    let current = active(&s.repo, &s.layer);
    s.repo
        .discard_layer(&op("rebind-discard"), &current)
        .unwrap();
    assert!(s.rt.rebind(&mut s.repo, &old, &desired).is_err());
    s.rt.recover(&mut s.repo).unwrap();
    save(
        &env,
        "rebind-refusals",
        &json!({"verdict":"VERIFIED", "writer":"busy", "mapping":"busy", "uncaptured":"retained", "unmounted":"preserved", "integrity":s.repo.check().unwrap()}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn rebind_process_loss_preserves_old_or_selects_exact_new_checkpoint() {
    let env = env();
    let mut cases = Vec::new();
    for mounted in [false, true] {
        for (index, boundary) in [
            "cache_staged",
            "rebind_frozen",
            "rebind_validated",
            "rebind_prepared",
            "rebind_removed",
            "rebind_selected",
            "rebind_complete",
        ]
        .into_iter()
        .enumerate()
        {
            let mut s = setup(&env, &format!("rebind-kill-{}-{index}", u8::from(mounted)));
            let (old, desired, source) = stacked(&mut s);
            let before = tree(&s.view);
            let world = s.repo.history().unwrap();
            let source_history = s.repo.layer_history(source.layer()).unwrap();
            if !mounted {
                s.rt.unmount(&s.layer).unwrap();
            }
            let mut actor = Actor::spawn(
                &env,
                s.repo_dir.path(),
                &s.root,
                "rebind",
                s.layer.as_str(),
                "rebind",
                boundary,
            );
            actor.marker(&format!("BOUNDARY {boundary}"));
            let busy = s.rt.recover(&mut s.repo);
            assert!(
                matches!(busy, Err(Error::Busy(_))),
                "runtime lock was not held: {busy:?}"
            );
            actor.kill();
            s.rt = LinuxLayerRuntime::open(&s.repo, &s.root, helper(&env)).unwrap();
            let recovered = s.rt.recover(&mut s.repo).unwrap();
            assert!(recovered.failures.is_empty(), "{recovered:?}");
            let selected = index >= 3;
            let status = s.rt.status(&s.repo, &s.layer).unwrap();
            assert!(status.pending.is_none());
            assert_eq!(status.mounted, mounted, "mount state at {boundary}");
            assert_eq!(
                status.bound_revision,
                if selected {
                    desired.revision()
                } else {
                    old.revision()
                }
            );
            s.rt.mount(&mut s.repo, &s.layer).unwrap();
            if selected {
                assert_eq!(
                    fs::read(s.view.join("value.h")).unwrap(),
                    b"#define VALUE 42\n"
                );
                assert!(!s.view.join("delete.txt").exists());
                // New bytes after recovery must survive both recover and exact retry.
                fs::write(s.view.join("after-recovery.txt"), b"retained\n").unwrap();
            } else {
                assert_eq!(tree(&s.view), before);
            }
            s.rt.rebind(&mut s.repo, &old, &desired).unwrap();
            let expected_tree = tree(&s.view);
            assert!(s.rt.recover(&mut s.repo).unwrap().failures.is_empty());
            assert_eq!(tree(&s.view), expected_tree);
            assert_eq!(s.repo.history().unwrap(), world);
            assert_eq!(active(&s.repo, &s.layer), desired);
            assert_eq!(
                s.repo.layer_history(source.layer()).unwrap(),
                source_history
            );
            let old_view = s.rt.mount_checkpoint(&s.repo, old.checkpoint()).unwrap();
            assert_eq!(tree(&old_view), before);
            s.rt.unmount_checkpoint(&s.repo, old.checkpoint()).unwrap();
            let inv = inventory(
                &env,
                &s.root,
                &format!("rebind-kill-{}-{index}-inventory", u8::from(mounted)),
            );
            assert!(inv["stages"].as_array().unwrap().is_empty());
            cases.push(json!({"boundary":boundary,"originalMounted":mounted,"selectedNew":selected,"recovery":recovered,"integrity":s.repo.check().unwrap()}));
            s.rt.unmount(&s.layer).unwrap();
        }
    }
    save(
        &env,
        "rebind-recovery",
        &json!({"verdict":"VERIFIED","cases":cases}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn rebind_refuses_head_race_and_helper_epoch_change() {
    let env = env();
    let mut cases = Vec::new();
    for native in [false, true] {
        let mut s = setup(
            &env,
            if native {
                "rebind-head-race"
            } else {
                "rebind-epoch-race"
            },
        );
        let (old, desired, _) = stacked(&mut s);
        let before = tree(&s.view);
        let mut actor = Actor::spawn(
            &env,
            s.repo_dir.path(),
            &s.root,
            "rebind",
            s.layer.as_str(),
            "rebind",
            "rebind_validated",
        );
        actor.marker("BOUNDARY rebind_validated");
        if native {
            let changes = s
                .repo
                .read_checkpoint(desired.checkpoint())
                .unwrap()
                .changes()
                .clone();
            applied(
                &s.repo
                    .capture_layer(&op("rebind-raced-head"), &desired, &changes)
                    .unwrap(),
            );
        } else {
            assert!(
                direct(
                    &env,
                    &["thaw", s.root.to_str().unwrap(), "view", &slot(&s.layer)]
                )
                .status
                .success()
            );
            fs::write(s.view.join("raced.txt"), b"keep race bytes\n").unwrap();
        }
        let result = actor.release();
        assert!(
            result.starts_with(if native {
                "Err(InvalidInput"
            } else {
                "Err(Busy"
            }),
            "{result}"
        );
        assert_eq!(
            s.rt.status(&s.repo, &s.layer).unwrap().bound_revision,
            old.revision()
        );
        assert!(!s.rt.status(&s.repo, &s.layer).unwrap().frozen);
        if native {
            assert_eq!(tree(&s.view), before);
        } else {
            assert_eq!(
                fs::read(s.view.join("raced.txt")).unwrap(),
                b"keep race bytes\n"
            );
        }
        cases.push(json!({"nativeHeadRace":native,"result":result}));
        s.rt.unmount(&s.layer).unwrap();
    }
    // After the durable intent, a later native advance does not retarget recovery.
    let mut s = setup(&env, "rebind-later-head");
    let (old, desired, _) = stacked(&mut s);
    let mut actor = Actor::spawn(
        &env,
        s.repo_dir.path(),
        &s.root,
        "rebind",
        s.layer.as_str(),
        "rebind",
        "rebind_prepared",
    );
    actor.marker("BOUNDARY rebind_prepared");
    let changes = s
        .repo
        .read_checkpoint(desired.checkpoint())
        .unwrap()
        .changes()
        .clone();
    let later = applied(
        &s.repo
            .capture_layer(&op("rebind-later-head"), &desired, &changes)
            .unwrap(),
    );
    actor.kill();
    assert!(s.rt.recover(&mut s.repo).unwrap().failures.is_empty());
    assert_eq!(
        s.rt.status(&s.repo, &s.layer).unwrap().bound_revision,
        desired.revision()
    );
    assert_eq!(active(&s.repo, &s.layer), later);
    assert!(s.rt.rebind(&mut s.repo, &old, &desired).is_err());
    assert!(matches!(
        s.rt.capture(&mut s.repo, &s.layer, &op("rebind-stale-capture"))
            .unwrap()
            .update,
        LayerUpdate::Stale { .. }
    ));
    s.rt.unmount(&s.layer).unwrap();
    save(
        &env,
        "rebind-races",
        &json!({"verdict":"VERIFIED", "refusals":cases, "selected":desired, "laterNative":later}),
    );
}

/// Check actual filesystem refusals, including namespace changes and truncation.
fn publication_read_only(view: &Path) -> Value {
    let before = tree(view);
    let errors = [
        (
            "write",
            os_error(
                fs::OpenOptions::new()
                    .write(true)
                    .open(view.join("README.txt"))
                    .and_then(|mut file| file.write_all(b"overwrite")),
            ),
        ),
        (
            "create",
            os_error(fs::write(view.join("publication-forbidden.txt"), b"new")),
        ),
        (
            "truncate",
            os_error(
                fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(view.join("README.txt"))
                    .map(|_| ()),
            ),
        ),
        (
            "rename",
            os_error(fs::rename(
                view.join("README.txt"),
                view.join("publication-renamed.txt"),
            )),
        ),
        ("unlink", os_error(fs::remove_file(view.join("README.txt")))),
    ];
    for (operation, errno) in errors {
        assert_eq!(errno, libc::EROFS, "published view allowed {operation}");
    }
    assert_eq!(tree(view), before, "read-only attempts changed bytes");
    json!(errors.into_iter().collect::<BTreeMap<_, _>>())
}

fn publication_settled(rt: &LinuxLayerRuntime, repo: &Repository, layer: &LayerId) {
    let status = rt.status(repo, layer).unwrap();
    assert!(status.mounted && status.frozen && status.pending.is_none());
    assert!(matches!(status.current, LayerState::Published { .. }));
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn publication_refuses_busy_writer_and_requires_exact_checkpoint() {
    let env = env();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        _source,
        repo_dir: _repo_dir,
    } = setup(&env, "publication-refusals");
    let history = repo.history().unwrap();
    let head = active(&repo, &layer);
    let writer = publication_writer(&view);
    let busy_op = op("publish-busy");
    let busy = rt.publish(
        &mut repo,
        &layer,
        &busy_op,
        &PublishAttribution::Human,
        |_, _| panic!("busy writer reached native guard"),
    );
    assert!(matches!(busy, Err(Error::Busy(_))), "{busy:?}");
    let status = rt.status(&repo, &layer).unwrap();
    assert!(!status.frozen && status.pending.is_none());
    assert!(repo.publication(&busy_op).unwrap().is_none());
    assert_eq!(active(&repo, &layer), head);
    assert_eq!(writer.release(), "written");
    assert_eq!(
        fs::read(view.join("README.txt")).unwrap(),
        b"shared base text\nwriter bytes\n"
    );
    let head = applied(
        &rt.capture(&mut repo, &layer, &op("writer-checkpoint"))
            .unwrap()
            .update,
    );
    let checkpoint_bytes = tree(&view);

    // Changes to content, additions, deletions and modes must all be checkpointed.
    let mut refusals = Vec::new();
    for (index, edit) in [
        "printf 'uncaptured\\n' > README.txt",
        "printf 'uncaptured\\n' > added.txt",
        "rm delete.txt",
        "chmod +x value.h",
        "mkdir uncaptured-empty",
        "ln -s README.txt uncaptured-link",
    ]
    .iter()
    .enumerate()
    {
        let mut log = Vec::new();
        sh(&view, edit, &mut log);
        let bytes = tree(&view);
        let operation = op(&format!("publish-uncaptured-{index}"));
        let result = rt.publish(
            &mut repo,
            &layer,
            &operation,
            &PublishAttribution::Human,
            |_, _| panic!("uncaptured view reached native guard"),
        );
        assert!(
            matches!(&result, Err(Error::InvalidInput(message)) if message.to_lowercase().contains("checkpoint")),
            "{result:?}"
        );
        assert!(repo.publication(&operation).unwrap().is_none());
        let status = rt.status(&repo, &layer).unwrap();
        assert!(!status.frozen && status.pending.is_none());
        assert_eq!(tree(&view), bytes, "uncaptured edits were overwritten");
        refusals.push(
            json!({"operation": operation, "result": format!("{result:?}"), "commands": log}),
        );
        match index {
            0 => fs::write(view.join("README.txt"), "shared base text\nwriter bytes\n").unwrap(),
            1 => fs::remove_file(view.join("added.txt")).unwrap(),
            2 => fs::write(view.join("delete.txt"), "delete only in A\n").unwrap(),
            3 => fs::set_permissions(view.join("value.h"), fs::Permissions::from_mode(0o644))
                .unwrap(),
            4 => fs::remove_dir(view.join("uncaptured-empty")).unwrap(),
            5 => fs::remove_file(view.join("uncaptured-link")).unwrap(),
            _ => unreachable!(),
        }
        assert_eq!(tree(&view), checkpoint_bytes);
    }
    fs::write(view.join("still-writable.txt"), "kept\n").unwrap();
    assert_eq!(active(&repo, &layer), head);
    assert_eq!(repo.history().unwrap(), history);
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 2);
    let captured = applied(
        &rt.capture(&mut repo, &layer, &op("checkpoint-before-publish"))
            .unwrap()
            .update,
    );
    let bytes = tree(&view);
    let refused_op = op("publish-guard-refused");
    let refused = rt.publish(
        &mut repo,
        &layer,
        &refused_op,
        &PublishAttribution::Human,
        |_, _| Err(Error::InvalidInput("optional guard refused".into())),
    );
    assert!(matches!(refused, Err(Error::InvalidInput(_))));
    assert!(repo.publication(&refused_op).unwrap().is_none());
    let status = rt.status(&repo, &layer).unwrap();
    assert!(!status.frozen && status.pending.is_none());
    assert_eq!(tree(&view), bytes);
    // The optional guard must not bypass the existing helper epoch boundary.
    let epoch_op = op("publish-guard-thaw");
    let raced = rt.publish(
        &mut repo,
        &layer,
        &epoch_op,
        &PublishAttribution::Human,
        |_, _| {
            assert!(
                direct(
                    &env,
                    &["thaw", root.to_str().unwrap(), "view", &slot(&layer)]
                )
                .status
                .success()
            );
            fs::write(view.join("guard-race.txt"), b"preserved\n").unwrap();
            assert!(
                direct(
                    &env,
                    &["freeze", root.to_str().unwrap(), "view", &slot(&layer)]
                )
                .status
                .success()
            );
            Ok(())
        },
    );
    assert!(matches!(raced, Err(Error::Busy(_))));
    assert!(repo.publication(&epoch_op).unwrap().is_none());
    let status = rt.status(&repo, &layer).unwrap();
    assert!(!status.frozen && status.pending.is_none());
    assert_eq!(
        fs::read(view.join("guard-race.txt")).unwrap(),
        b"preserved\n"
    );
    fs::remove_file(view.join("guard-race.txt")).unwrap();
    let receipt = rt
        .publish(
            &mut repo,
            &layer,
            &busy_op,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    assert_eq!(receipt.expected, captured);
    assert_eq!(published(&receipt).number.get(), 2);
    assert_eq!(repo.history().unwrap().len(), history.len() + 1);
    publication_settled(&rt, &repo, &layer);
    save(
        &env,
        "publication-refusals",
        &json!({"verdict": "VERIFIED", "busy": format!("{busy:?}"),
            "uncaptured": refusals, "receipt": receipt, "tree": bytes,
            "readOnly": publication_read_only(&view), "integrity": repo.check().unwrap()}),
    );
    inventory(&env, &root, "publication-refusals-inventory");
    rt.unmount(&layer).unwrap();
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn published_view_replays_receipt_and_keeps_private_checkpoint_clone() {
    let env = env();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        _source,
        repo_dir,
    } = setup(&env, "publication");
    let initial = active(&repo, &layer);
    fs::write(view.join("README.txt"), "published bytes\n").unwrap();
    let captured = applied(
        &rt.capture(&mut repo, &layer, &op("published-capture"))
            .unwrap()
            .update,
    );
    let stale_discard = repo
        .discard_layer(&op("historical-stale-discard"), &initial)
        .unwrap();
    assert!(matches!(stale_discard, LayerUpdate::Stale { .. }));
    let bytes = tree(&view);
    let before = inventory(&env, &root, "publication-inventory-before");
    assert!(upper_entries(&before, &slot(&layer)) > 0);
    let attribution = PublishAttribution::Agent {
        context: Some(repo.put_blob(b"supplied publication context").unwrap()),
        allow_missing_trace: false,
    };
    let operation = op("publish-mounted");
    let manifest = repo.read_checkpoint(captured.checkpoint()).unwrap();
    let mut guard_calls = 0;
    let receipt = rt
        .publish(
            &mut repo,
            &layer,
            &operation,
            &attribution,
            |world, candidate| {
                guard_calls += 1;
                assert_eq!(world.number.get(), 1);
                assert_eq!(candidate, manifest.manifest());
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(guard_calls, 1);
    assert_eq!(receipt.expected, captured);
    assert_eq!(receipt.attribution, attribution);
    let version = published(&receipt).clone();
    assert_eq!(version.number.get(), 2);
    assert_eq!(
        repo.read_manifest(version.manifest).unwrap(),
        *manifest.manifest()
    );
    assert_eq!(repo.publication(&operation).unwrap(), Some(receipt.clone()));
    publication_settled(&rt, &repo, &layer);
    let errors = publication_read_only(&view);

    // Later World advancement cannot change the original receipt or rerun policy.
    repo.compare_and_advance(&op("after-publication"), version.number, version.manifest)
        .unwrap();
    drop(rt);
    drop(repo);
    let mut repo = Repository::open(repo_dir.path()).unwrap();
    let mut rt = LinuxLayerRuntime::open(&repo, &root, helper(&env)).unwrap();
    assert_eq!(rt.mount(&mut repo, &layer).unwrap(), view);
    assert_eq!(
        rt.publish(&mut repo, &layer, &operation, &attribution, |_, _| {
            panic!("durable publication replay reran guard")
        })
        .unwrap(),
        receipt
    );
    let mismatched = rt.publish(
        &mut repo,
        &layer,
        &operation,
        &PublishAttribution::Human,
        |_, _| panic!("attribution mismatch reached guard"),
    );
    assert!(matches!(mismatched, Err(Error::OperationIdReused(_))));

    // Terminal refusals reserve no new attempt and must never thaw Published views.
    for capture in [true, false] {
        let operation = op(if capture {
            "capture-published"
        } else {
            "discard-published"
        });
        let result = if capture {
            rt.capture(&mut repo, &layer, &operation).map(|_| ())
        } else {
            rt.discard(&mut repo, &layer, &operation).map(|_| ())
        };
        assert!(
            matches!(result, Err(Error::LayerPublished { .. })),
            "{result:?}"
        );
        assert!(repo.layer_operation(&operation).unwrap().is_none());
        publication_settled(&rt, &repo, &layer);
        publication_read_only(&view);
    }
    let replayed_capture = rt
        .capture(&mut repo, &layer, &op("published-capture"))
        .unwrap();
    assert!(replayed_capture.replayed);
    assert_eq!(applied(&replayed_capture.update), captured);
    assert_eq!(
        rt.discard(&mut repo, &layer, &op("historical-stale-discard"))
            .unwrap(),
        stale_discard
    );
    publication_settled(&rt, &repo, &layer);
    let recovery = rt.recover(&mut repo).unwrap();
    assert!(recovery.failures.is_empty() && recovery.pending_resolved == 0);
    publication_settled(&rt, &repo, &layer);
    let after = inventory(&env, &root, "publication-inventory-after");
    assert_eq!(
        before["uppers"][slot(&layer)],
        after["uppers"][slot(&layer)]
    );
    assert_eq!(tree(&view), bytes);

    let clone = id("published-clone");
    repo.clone_layer(&clone, captured.checkpoint()).unwrap();
    let cloned = rt.mount(&mut repo, &clone).unwrap();
    assert_eq!(tree(&cloned), bytes);
    let clone_inventory = inventory(&env, &root, "publication-inventory-clone");
    assert_eq!(upper_entries(&clone_inventory, &slot(&clone)), 0);
    fs::write(cloned.join("README.txt"), "private clone bytes\n").unwrap();
    fs::write(cloned.join("clone-only.txt"), "private\n").unwrap();
    assert_eq!(tree(&view), bytes);
    publication_read_only(&view);
    let clone_head = applied(
        &rt.capture(&mut repo, &clone, &op("capture-published-clone"))
            .unwrap()
            .update,
    );
    assert_ne!(clone_head.checkpoint(), captured.checkpoint());
    assert_eq!(repo.history().unwrap().len(), 4);
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 3);
    save(
        &env,
        "publication",
        &json!({"verdict": "VERIFIED", "receipt": receipt, "readOnly": errors,
            "recovery": recovery, "publishedTree": bytes, "cloneTree": tree(&cloned),
            "history": repo.history().unwrap(), "integrity": repo.check().unwrap()}),
    );
    rt.unmount(&clone).unwrap();
    rt.unmount(&layer).unwrap();
}

/// Invokes the root helper directly, as any process of the same user could.
fn direct(env: &Env, args: &[&str]) -> std::process::Output {
    Command::new("/usr/bin/sudo")
        .arg("-n")
        .arg(&env.helper)
        .args(args)
        .output()
        .unwrap()
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn stale_view_with_unreadable_entry_thaws_and_keeps_edits() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        _source,
        repo_dir: _repo_dir,
        ..
    } = setup(&env, "stale");
    sh(&view, "printf 'one\\n' > one.txt", &mut log);
    applied(&rt.capture(&mut repo, &layer, &op("s-1")).unwrap().update);
    // Another caller advances the Layer natively; the view's bound head is now stale.
    let external = repo
        .capture_layer(
            &op("s-external"),
            &active(&repo, &layer),
            &SparseChanges::new([]).unwrap(),
        )
        .unwrap();
    applied(&external);
    sh(
        &view,
        "printf 'kept\\n' > kept.txt && printf s > secret && chmod 000 secret",
        &mut log,
    );
    let failed = rt.capture(&mut repo, &layer, &op("s-unreadable"));
    let status = rt.status(&repo, &layer).unwrap();
    save(
        &env,
        "stale-unreadable",
        &json!({
            "captureResult": format!("{failed:?}"), "frozen": status.frozen,
            "pending": status.pending, "boundRevision": status.bound_revision,
            "nativeRevisions": repo.layer_history(&layer).unwrap().len(),
        }),
    );
    assert!(matches!(failed, Err(Error::Io(_))), "{failed:?}");
    assert!(
        !status.frozen && status.pending.is_none(),
        "view left frozen behind a pending retry"
    );
    assert_eq!(fs::read(view.join("kept.txt")).unwrap(), b"kept\n");
    sh(&view, "chmod 644 secret", &mut log);
    let stale = rt.capture(&mut repo, &layer, &op("s-2")).unwrap();
    assert!(matches!(stale.update, LayerUpdate::Stale { .. }));
    let report = rt.recover(&mut repo).unwrap();
    assert!(report.failures.is_empty() && report.pending_resolved == 0);
    // Documented limit: the runtime cannot discard a view bound to a stale head.
    let refused = rt.discard(&mut repo, &layer, &op("s-discard")).unwrap();
    assert!(matches!(refused, LayerUpdate::Stale { .. }));
    assert_eq!(fs::read(view.join("kept.txt")).unwrap(), b"kept\n");
    // Escape: discard natively at the current head; recovery removes the view.
    repo.discard_layer(&op("s-native-discard"), &active(&repo, &layer))
        .unwrap();
    let report = rt.recover(&mut repo).unwrap();
    assert_eq!(report.views_removed, 1);
    assert!(!view.exists());
    inventory(&env, &root, "stale-inventory-final");
    save(
        &env,
        "stale",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn thaw_between_listing_and_reading_is_refused() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        repo_dir,
        _source,
        ..
    } = setup(&env, "race");
    let slot = slot(&layer);
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "race-capture",
        "capture_listed",
    );
    actor.marker("BOUNDARY capture_listed");
    // Same-user direct helper calls change a lower-only file during the capture.
    assert!(
        direct(&env, &["thaw", root.to_str().unwrap(), "view", &slot])
            .status
            .success()
    );
    sh(&view, "printf '/* edited */\\n' >> main.c", &mut log);
    assert!(
        direct(&env, &["freeze", root.to_str().unwrap(), "view", &slot])
            .status
            .success()
    );
    let result = actor.release();
    let main_in_head = repo
        .layer_diff(&active(&repo, &layer))
        .unwrap()
        .iter()
        .any(|d| d.path.as_str() == "main.c");
    save(
        &env,
        "race-result",
        &json!({"captureResult": result, "mainCInCapturedHead": main_in_head,
        "revisions": repo.layer_history(&layer).unwrap().len()}),
    );
    assert!(
        result.starts_with("Err(Busy"),
        "incoherent capture accepted: {result}"
    );
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 1);
    let status = rt.status(&repo, &layer).unwrap();
    assert!(!status.frozen && status.pending.is_none());
    let captured = applied(
        &rt.capture(&mut repo, &layer, &op("race-retry"))
            .unwrap()
            .update,
    );
    assert!(
        repo.layer_diff(&captured)
            .unwrap()
            .iter()
            .any(|d| d.path.as_str() == "main.c")
    );
    save(
        &env,
        "race",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn completed_captures_replay_exactly_and_thaw() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        _source,
        repo_dir: _repo_dir,
        ..
    } = setup(&env, "replay");
    sh(&view, "printf 'one\\n' > one.txt", &mut log);
    let first = applied(&rt.capture(&mut repo, &layer, &op("r-1")).unwrap().update);
    sh(&view, "printf 'two\\n' > two.txt", &mut log);
    let second = applied(&rt.capture(&mut repo, &layer, &op("r-2")).unwrap().update);
    // A prior thaw failure leaves the view frozen without a pending operation.
    let slot = slot(&layer);
    assert!(
        direct(&env, &["freeze", root.to_str().unwrap(), "view", &slot])
            .status
            .success()
    );
    let replay_second = rt.capture(&mut repo, &layer, &op("r-2"));
    let frozen_after = rt.status(&repo, &layer).unwrap().frozen;
    let replay_first = rt.capture(&mut repo, &layer, &op("r-1"));
    save(
        &env,
        "replay-result",
        &json!({"replaySecond": format!("{replay_second:?}"),
        "frozenAfterReplay": frozen_after, "replayFirst": format!("{replay_first:?}")}),
    );
    assert_eq!(applied(&replay_second.unwrap().update), second);
    assert!(!frozen_after, "replay left the view frozen");
    assert_eq!(
        applied(&replay_first.unwrap().update),
        first,
        "older operation not replayed"
    );
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 3);
    sh(&view, "printf 'three\\n' > three.txt", &mut log);
    save(
        &env,
        "replay",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn init_never_deletes_a_runtime_named_like_staging() {
    let env = env();
    let source = TempDir::new().unwrap();
    fixture_tree(source.path());
    let (_dir, repo) = repository_from(source.path());
    let victim = env.parent.join(".rt-y.javelin-init");
    let created = LinuxLayerRuntime::init(&repo, &victim, helper(&env)).is_ok();
    let other = LinuxLayerRuntime::init(&repo, env.parent.join("rt-y"), helper(&env));
    let survived = victim.join("owner").exists();
    save(
        &env,
        "init-names-result",
        &json!({"stagingLikeNameAccepted": created,
        "victimSurvived": survived, "otherInit": format!("{:?}", other.map(|_| ()))}),
    );
    assert!(
        !created || survived,
        "init deleted an existing runtime named like its staging"
    );
    assert!(!created, "staging-like runtime name accepted");
    assert!(matches!(
        LinuxLayerRuntime::init(&repo, env.parent.join("rt.z"), helper(&env)),
        Err(Error::InvalidInput(_))
    ));
    save(&env, "init-names", &json!({"verdict": "VERIFIED"}));
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn replaying_a_foreign_operation_never_rebinds_the_view() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        _source,
        repo_dir: _repo_dir,
        ..
    } = setup(&env, "foreign");
    sh(&view, "printf 'one\\n' > one.txt", &mut log);
    applied(&rt.capture(&mut repo, &layer, &op("f-1")).unwrap().update);
    // Another caller captures natively; its head adds x.txt that this view lacks.
    let x = FileEntry::Regular {
        content: repo.put_blob(b"x\n").unwrap(),
        executable: false,
    };
    let changes = SparseChanges::new([(RepoPath::parse("x.txt").unwrap(), Some(x))]).unwrap();
    applied(
        &repo
            .capture_layer(&op("f-ext"), &active(&repo, &layer), &changes)
            .unwrap(),
    );
    let replay = rt.capture(&mut repo, &layer, &op("f-ext"));
    let bound = rt.status(&repo, &layer).unwrap().bound_revision;
    sh(&view, "printf 'two\\n' > two.txt", &mut log);
    let next = rt.capture(&mut repo, &layer, &op("f-2"));
    let head_has_x = repo
        .layer_diff(&active(&repo, &layer))
        .unwrap()
        .iter()
        .any(|d| d.path.as_str() == "x.txt");
    save(
        &env,
        "foreign-result",
        &json!({"replay": format!("{replay:?}"), "boundAfterReplay": bound,
        "nextCapture": format!("{next:?}"), "headHasX": head_has_x}),
    );
    assert!(replay.unwrap().replayed);
    assert_eq!(bound, 1, "replay of a foreign operation rebound the view");
    assert!(
        matches!(next.unwrap().update, LayerUpdate::Stale { .. }),
        "foreign work overwritten"
    );
    assert!(head_has_x);
    assert_eq!(fs::read(view.join("two.txt")).unwrap(), b"two\n");
    save(
        &env,
        "foreign",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn reused_pending_capture_id_thaws_the_view() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        repo_dir,
        _source,
    } = setup(&env, "reused");
    let other = id("other");
    let base = repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let other_head = repo.create_layer(&other, &base).unwrap();
    sh(&view, "printf 'edit\\n' > edit.txt", &mut log);
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "shared-id",
        "capture_frozen",
    );
    actor.marker("BOUNDARY capture_frozen");
    actor.kill();
    // The pending ID is then used natively for a different Layer.
    applied(
        &repo
            .capture_layer(
                &op("shared-id"),
                &other_head,
                &SparseChanges::new([]).unwrap(),
            )
            .unwrap(),
    );
    let report = rt.recover(&mut repo).unwrap();
    let status = rt.status(&repo, &layer).unwrap();
    save(
        &env,
        "reused-result",
        &json!({"recovery": report, "frozen": status.frozen,
        "pending": status.pending}),
    );
    assert!(
        !status.frozen && status.pending.is_none(),
        "reused pending ID left the view frozen"
    );
    assert_eq!(report.failures.len(), 1);
    assert_eq!(fs::read(view.join("edit.txt")).unwrap(), b"edit\n");
    let captured = applied(
        &rt.capture(&mut repo, &layer, &op("after-reuse"))
            .unwrap()
            .update,
    );
    assert!(
        repo.layer_diff(&captured)
            .unwrap()
            .iter()
            .any(|d| d.path.as_str() == "edit.txt")
    );
    save(
        &env,
        "reused",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn pending_capture_never_adopts_a_foreign_same_id_commit() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        repo_dir,
        _source,
    } = setup(&env, "sameid");
    sh(&view, "printf 'mine\\n' > mine.txt", &mut log);
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "same-id",
        "capture_frozen",
    );
    actor.marker("BOUNDARY capture_frozen");
    actor.kill();
    // Another caller commits different content on the same Layer under the same ID.
    let x = FileEntry::Regular {
        content: repo.put_blob(b"x\n").unwrap(),
        executable: false,
    };
    let changes = SparseChanges::new([(RepoPath::parse("x.txt").unwrap(), Some(x))]).unwrap();
    applied(
        &repo
            .capture_layer(&op("same-id"), &active(&repo, &layer), &changes)
            .unwrap(),
    );
    let report = rt.recover(&mut repo).unwrap();
    let status = rt.status(&repo, &layer).unwrap();
    save(
        &env,
        "sameid-result",
        &json!({"recovery": report, "frozen": status.frozen,
        "pending": status.pending, "boundRevision": status.bound_revision}),
    );
    assert_eq!(
        status.bound_revision, 0,
        "foreign same-ID commit adopted as this view's capture"
    );
    assert!(!status.frozen && status.pending.is_none());
    assert_eq!(report.failures.len(), 1);
    let next = rt.capture(&mut repo, &layer, &op("after-same-id")).unwrap();
    assert!(
        matches!(next.update, LayerUpdate::Stale { .. }),
        "foreign work overwritten"
    );
    assert_eq!(fs::read(view.join("mine.txt")).unwrap(), b"mine\n");
    save(
        &env,
        "sameid",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn recover_thaws_a_frozen_view_without_pending_work() {
    let env = env();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        _source,
        repo_dir: _repo_dir,
    } = setup(&env, "orphanfreeze");
    // Models a crash between the freeze and the pending-record write.
    let slot = slot(&layer);
    assert!(
        direct(&env, &["freeze", root.to_str().unwrap(), "view", &slot])
            .status
            .success()
    );
    let report = rt.recover(&mut repo).unwrap();
    let frozen = rt.status(&repo, &layer).unwrap().frozen;
    save(
        &env,
        "orphanfreeze-result",
        &json!({"recovery": report, "frozenAfterRecover": frozen}),
    );
    assert!(!frozen, "recover left a frozen view without pending work");
    fs::write(view.join("writable.txt"), "ok").unwrap();
    save(&env, "orphanfreeze", &json!({"verdict": "VERIFIED"}));
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn committed_capture_survives_a_transient_replay_failure() {
    let env = env();
    let mut log = Vec::new();
    let Setup {
        mut repo,
        mut rt,
        layer,
        view,
        root,
        repo_dir,
        _source,
    } = setup(&env, "transient");
    sh(&view, "printf 'mine\\n' > mine.txt", &mut log);
    let mut actor = Actor::spawn(
        &env,
        repo_dir.path(),
        &root,
        "capture",
        "A",
        "committed",
        "capture_committed",
    );
    actor.marker("BOUNDARY capture_committed");
    actor.kill();
    assert_eq!(
        repo.layer_history(&layer).unwrap().len(),
        2,
        "native commit happened"
    );
    // Another writer holds the database, so replay verification fails transiently.
    let blocker =
        rusqlite::Connection::open(repo_dir.path().join(".javelin/repository.sqlite3")).unwrap();
    // EXCLUSIVE blocks readers too, so even the native ownership lookup fails.
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let first = rt.recover(&mut repo).unwrap();
    blocker.execute_batch("COMMIT").unwrap();
    let status = rt.status(&repo, &layer).unwrap();
    let second = rt.recover(&mut repo).unwrap();
    let after = rt.status(&repo, &layer).unwrap();
    save(
        &env,
        "transient-result",
        &json!({"firstRecovery": first, "frozenAfterFirst": status.frozen,
        "pendingAfterFirst": status.pending, "secondRecovery": second,
        "boundAfterSecond": after.bound_revision, "frozenAfterSecond": after.frozen}),
    );
    assert_eq!(first.failures.len(), 1, "blocked lookup did not fail");
    assert!(
        status.frozen && status.pending.as_deref() == Some("committed"),
        "own commit abandoned"
    );
    assert_eq!(second.pending_resolved, 1);
    assert_eq!(
        after.bound_revision, 1,
        "own commit not adopted after the transient failure"
    );
    assert!(!after.frozen && after.pending.is_none());
    assert_eq!(repo.layer_history(&layer).unwrap().len(), 2);
    save(
        &env,
        "transient",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

fn kill_after_native_commit(env: &Env, s: &Setup, operation: &str) {
    let mut actor = Actor::spawn(
        env,
        s.repo_dir.path(),
        &s.root,
        "capture",
        "A",
        operation,
        "capture_committed",
    );
    actor.marker("BOUNDARY capture_committed");
    actor.kill();
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn helper_misconfiguration_never_abandons_an_own_commit() {
    let env = env();
    let mut log = Vec::new();
    let mut s = setup(&env, "misconfig");
    sh(&s.view, "printf 'mine\\n' > mine.txt", &mut log);
    kill_after_native_commit(&env, &s, "misconfig");
    // The user's own state directory mode makes every helper verb refuse (exit 2).
    let state = s.root.join("state");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
    // Resolving the pending capture through mount meets the helper refusal.
    let first = format!("{:?}", s.rt.mount(&mut s.repo, &s.layer));
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let second = s.rt.recover(&mut s.repo).unwrap();
    let status = s.rt.status(&s.repo, &s.layer).unwrap();
    save(
        &env,
        "misconfig-result",
        &json!({"firstRecovery": first, "secondRecovery": second,
        "bound": status.bound_revision, "frozen": status.frozen, "pending": status.pending}),
    );
    assert_eq!(
        status.bound_revision, 1,
        "own commit abandoned after a helper refusal"
    );
    assert!(!status.frozen && status.pending.is_none());
    save(
        &env,
        "misconfig",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn direct_thaw_after_an_own_commit_still_adopts_it() {
    let env = env();
    let mut log = Vec::new();
    let mut s = setup(&env, "thawedcommit");
    sh(&s.view, "printf 'mine\\n' > mine.txt", &mut log);
    kill_after_native_commit(&env, &s, "thawed-commit");
    let slot = slot(&s.layer);
    assert!(
        direct(&env, &["thaw", s.root.to_str().unwrap(), "view", &slot])
            .status
            .success()
    );
    sh(&s.view, "printf 'later\\n' > later.txt", &mut log);
    let report = s.rt.recover(&mut s.repo).unwrap();
    let status = s.rt.status(&s.repo, &s.layer).unwrap();
    save(
        &env,
        "thawedcommit-result",
        &json!({"recovery": report, "bound": status.bound_revision,
        "frozen": status.frozen, "pending": status.pending}),
    );
    assert_eq!(
        status.bound_revision, 1,
        "own commit treated as foreign after a direct thaw"
    );
    assert!(!status.frozen && status.pending.is_none());
    assert_eq!(fs::read(s.view.join("later.txt")).unwrap(), b"later\n");
    save(
        &env,
        "thawedcommit",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn foreign_same_id_commit_with_unlistable_upper_thaws() {
    let env = env();
    let mut log = Vec::new();
    let mut s = setup(&env, "unlistable");
    // More upper entries than the helper lists (a documented scale limit).
    fs::create_dir(s.view.join("many")).unwrap();
    for i in 0..200_001 {
        fs::File::create(s.view.join("many").join(i.to_string())).unwrap();
    }
    log.push(json!({"created": "200001 empty files under many/"}));
    let mut actor = Actor::spawn(
        &env,
        s.repo_dir.path(),
        &s.root,
        "capture",
        "A",
        "unlistable",
        "capture_frozen",
    );
    actor.marker("BOUNDARY capture_frozen");
    actor.kill();
    let x = FileEntry::Regular {
        content: s.repo.put_blob(b"x\n").unwrap(),
        executable: false,
    };
    let changes = SparseChanges::new([(RepoPath::parse("x.txt").unwrap(), Some(x))]).unwrap();
    let head = active(&s.repo, &s.layer);
    applied(
        &s.repo
            .capture_layer(&op("unlistable"), &head, &changes)
            .unwrap(),
    );
    let report = s.rt.recover(&mut s.repo).unwrap();
    let status = s.rt.status(&s.repo, &s.layer).unwrap();
    save(
        &env,
        "unlistable-result",
        &json!({"recovery": report, "bound": status.bound_revision,
        "frozen": status.frozen, "pending": status.pending}),
    );
    assert!(
        !status.frozen && status.pending.is_none(),
        "view frozen behind a foreign same-ID commit"
    );
    assert_eq!(status.bound_revision, 0);
    save(
        &env,
        "unlistable",
        &json!({"verdict": "VERIFIED", "commands": log}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn publication_process_loss_recovers_native_receipts_without_advancing_twice() {
    let env = env();
    let mut cases = Vec::new();
    for (index, boundary) in [
        "workspace_publish_frozen",
        "workspace_publish_prepared",
        "publish_before_commit",
        "publish_after_commit",
        "workspace_publish_committed",
    ]
    .iter()
    .enumerate()
    {
        let mut s = setup(&env, &format!("publication-kill-{index}"));
        fs::write(s.view.join("private.txt"), b"recoverable bytes\n").unwrap();
        let head = applied(
            &s.rt
                .capture(&mut s.repo, &s.layer, &op("capture-for-publish"))
                .unwrap()
                .update,
        );
        let bytes = tree(&s.view);
        let operation = op("killed-publication");
        let mut actor = Actor::spawn(
            &env,
            s.repo_dir.path(),
            &s.root,
            "publish",
            "A",
            operation.as_str(),
            boundary,
        );
        actor.marker(&format!("BOUNDARY {boundary}"));
        // The freeze precedes both transaction acceptance and the runtime reply.
        publication_read_only(&s.view);
        actor.kill();
        let committed = index >= 3;
        let original = s.repo.publication(&operation).unwrap();
        assert_eq!(original.is_some(), committed);
        let report = s.rt.recover(&mut s.repo).unwrap();
        assert!(report.failures.is_empty(), "{report:?}");
        assert_eq!(report.pending_resolved, 1);
        assert_eq!(tree(&s.view), bytes);
        let status = s.rt.status(&s.repo, &s.layer).unwrap();
        assert!(status.pending.is_none());
        assert_eq!(status.frozen, committed);
        assert_eq!(
            s.repo.history().unwrap().len(),
            if committed { 3 } else { 2 }
        );
        assert_eq!(s.repo.publication(&operation).unwrap(), original);
        let mut guard_calls = 0;
        let receipt =
            s.rt.publish(
                &mut s.repo,
                &s.layer,
                &operation,
                &PublishAttribution::Human,
                |_, _| {
                    guard_calls += 1;
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(guard_calls, if committed { 0 } else { 1 });
        if let Some(original) = original {
            assert_eq!(receipt, original);
        }
        assert_eq!(receipt.expected, head);
        assert_eq!(published(&receipt).number.get(), 2);
        publication_settled(&s.rt, &s.repo, &s.layer);
        let again = s.rt.recover(&mut s.repo).unwrap();
        assert!(again.failures.is_empty() && again.pending_resolved == 0);
        assert_eq!(s.repo.history().unwrap().len(), 3);
        assert_eq!(s.repo.layer_history(&s.layer).unwrap().len(), 3);
        cases.push(
            json!({"boundary": boundary, "committedBeforeRecovery": committed,
            "recovery": report, "receipt": receipt, "guardCallsOnRetry": guard_calls,
            "integrity": s.repo.check().unwrap()}),
        );
        s.rt.unmount(&s.layer).unwrap();
    }
    save(
        &env,
        "publication-recovery",
        &json!({"verdict": "VERIFIED", "cases": cases}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn external_native_publication_is_enforced_when_runtime_observes_it() {
    let env = env();
    let mut cases = Vec::new();
    for observe in ["mount", "open", "status", "recover"] {
        let mut s = setup(&env, &format!("publication-external-{observe}"));
        let head = active(&s.repo, &s.layer);
        let writer = publication_writer(&s.view);
        // This independent process deliberately bypasses the workspace freeze.
        // There is no continuous reconciler for an already-mounted Active view.
        let mut native = Actor::spawn(
            &env,
            s.repo_dir.path(),
            &s.root,
            "native_publish",
            "A",
            "external-publish",
            "",
        );
        assert!(native.marker("RESULT ").starts_with("Ok("));
        assert!(native.child.wait().unwrap().success());
        let receipt = s
            .repo
            .publication(&op("external-publish"))
            .unwrap()
            .unwrap();
        assert_eq!(receipt.expected, head);
        published(&receipt);
        assert_eq!(mount_state(&s.view).unwrap(), Some(false));
        fs::write(
            s.view.join("after-native.txt"),
            b"uncaptured after publication\n",
        )
        .unwrap();
        let blocked = match observe {
            "mount" => matches!(s.rt.mount(&mut s.repo, &s.layer), Err(Error::Busy(_))),
            "open" => matches!(
                LinuxLayerRuntime::open(&s.repo, &s.root, helper(&env)),
                Err(Error::Busy(_))
            ),
            "status" => matches!(s.rt.status(&s.repo, &s.layer), Err(Error::Busy(_))),
            "recover" => s.rt.recover(&mut s.repo).unwrap().failures.len() == 1,
            _ => unreachable!(),
        };
        assert!(blocked, "{observe} did not report a held writer");
        assert_eq!(writer.release(), "written");
        let bytes = tree(&s.view);
        match observe {
            "mount" => {
                s.rt.mount(&mut s.repo, &s.layer).unwrap();
            }
            "open" => {
                s.rt = LinuxLayerRuntime::open(&s.repo, &s.root, helper(&env)).unwrap();
            }
            "status" => {
                assert!(s.rt.status(&s.repo, &s.layer).unwrap().frozen);
            }
            "recover" => {
                assert!(s.rt.recover(&mut s.repo).unwrap().failures.is_empty());
            }
            _ => unreachable!(),
        }
        publication_settled(&s.rt, &s.repo, &s.layer);
        assert_eq!(tree(&s.view), bytes);
        publication_read_only(&s.view);
        let accepted = s.repo.read_checkpoint(head.checkpoint()).unwrap();
        assert!(
            !accepted
                .manifest()
                .entries()
                .contains_key(&RepoPath::parse("after-native.txt").unwrap())
        );
        assert_eq!(
            fs::read(s.view.join("after-native.txt")).unwrap(),
            b"uncaptured after publication\n"
        );
        cases.push(json!({"observer": observe, "busyWriterReported": blocked,
            "privateTreeRetained": bytes, "receipt": receipt}));
        s.rt.unmount(&s.layer).unwrap();
    }
    // A first mount after native publication also creates a read-only view.
    let mut s = setup(&env, "publication-fresh-mount");
    let layer = id("never-mounted");
    let base = s.repo.layer_base(VersionNumber::new(1).unwrap()).unwrap();
    let head = s.repo.create_layer(&layer, &base).unwrap();
    s.repo
        .publish_layer(
            &op("never-mounted-publish"),
            &head,
            &PublishAttribution::Human,
            |_, _| Ok(()),
        )
        .unwrap();
    let view = s.rt.mount(&mut s.repo, &layer).unwrap();
    publication_settled(&s.rt, &s.repo, &layer);
    publication_read_only(&view);
    s.rt.unmount(&layer).unwrap();
    s.rt.unmount(&s.layer).unwrap();
    save(
        &env,
        "publication-external",
        &json!({"verdict": "VERIFIED", "cases": cases,
        "continuousReconciler": false}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn terminal_publication_resolves_pending_capture_and_discard() {
    let env = env();
    let mut cases = Vec::new();
    for (mode, boundary) in [
        ("capture", "capture_frozen"),
        ("discard", "discard_unmounted"),
    ] {
        let mut s = setup(&env, &format!("publication-pending-{mode}"));
        fs::write(s.view.join("private.txt"), b"uncaptured private bytes\n").unwrap();
        let bytes = tree(&s.view);
        let head = active(&s.repo, &s.layer);
        let mut actor = Actor::spawn(
            &env,
            s.repo_dir.path(),
            &s.root,
            mode,
            "A",
            "interrupted-edit",
            boundary,
        );
        actor.marker(&format!("BOUNDARY {boundary}"));
        actor.kill();
        let receipt = s
            .repo
            .publish_layer(
                &op("external-terminal"),
                &head,
                &PublishAttribution::Human,
                |_, _| Ok(()),
            )
            .unwrap();
        let recovery = s.rt.recover(&mut s.repo).unwrap();
        assert!(recovery.failures.is_empty(), "{recovery:?}");
        assert_eq!(recovery.pending_resolved, 1);
        assert!(
            s.repo
                .layer_operation(&op("interrupted-edit"))
                .unwrap()
                .is_none()
        );
        publication_settled(&s.rt, &s.repo, &s.layer);
        assert_eq!(tree(&s.view), bytes);
        publication_read_only(&s.view);
        cases.push(json!({"pending": mode, "recovery": recovery,
            "receipt": receipt, "privateTreeRetained": bytes}));
        s.rt.unmount(&s.layer).unwrap();
    }
    save(
        &env,
        "publication-terminal",
        &json!({"verdict": "VERIFIED", "cases": cases}),
    );
}

#[test]
#[ignore = "requires disposable hosted Linux; run by linux-layer-runtime workflow"]
fn published_observation_preserves_explicitly_unmounted_views() {
    let env = env();
    let mut s = setup(&env, "publication-unmounted");
    fs::write(s.view.join("private.txt"), b"published private upper\n").unwrap();
    s.rt.capture(&mut s.repo, &s.layer, &op("unmounted-capture"))
        .unwrap();
    s.rt.publish(
        &mut s.repo,
        &s.layer,
        &op("unmounted-publish"),
        &PublishAttribution::Human,
        |_, _| Ok(()),
    )
    .unwrap();
    let bytes = tree(&s.view);
    let mut cases = Vec::new();
    for observer in ["status", "open", "recover"] {
        publication_settled(&s.rt, &s.repo, &s.layer);
        s.rt.unmount(&s.layer).unwrap();
        assert_eq!(mount_state(&s.view).unwrap(), None);
        match observer {
            "status" => {
                let status = s.rt.status(&s.repo, &s.layer).unwrap();
                assert!(
                    !status.mounted && !status.frozen && status.pending.is_none(),
                    "status recreated an explicitly unmounted Published view"
                );
                assert!(matches!(status.current, LayerState::Published { .. }));
            }
            "open" => {
                s.rt = LinuxLayerRuntime::open(&s.repo, &s.root, helper(&env)).unwrap();
            }
            "recover" => {
                let report = s.rt.recover(&mut s.repo).unwrap();
                assert!(report.failures.is_empty());
                assert_eq!(
                    report.views_published, 0,
                    "unmounted view counted as frozen"
                );
            }
            _ => unreachable!(),
        }
        assert_eq!(
            mount_state(&s.view).unwrap(),
            None,
            "{observer} recreated an explicitly unmounted Published view"
        );
        let status = s.rt.status(&s.repo, &s.layer).unwrap();
        assert!(!status.mounted && !status.frozen && status.pending.is_none());
        assert_eq!(s.rt.mount(&mut s.repo, &s.layer).unwrap(), s.view);
        assert_eq!(tree(&s.view), bytes, "explicit remount lost private bytes");
        publication_read_only(&s.view);

        // Observation must still freeze a mount made writable through the helper.
        assert!(
            direct(
                &env,
                &["thaw", s.root.to_str().unwrap(), "view", &slot(&s.layer)]
            )
            .status
            .success()
        );
        assert_eq!(mount_state(&s.view).unwrap(), Some(false));
        match observer {
            "status" => {
                assert!(s.rt.status(&s.repo, &s.layer).unwrap().frozen);
            }
            "open" => {
                s.rt = LinuxLayerRuntime::open(&s.repo, &s.root, helper(&env)).unwrap();
            }
            "recover" => {
                assert!(s.rt.recover(&mut s.repo).unwrap().failures.is_empty());
            }
            _ => unreachable!(),
        }
        assert_eq!(mount_state(&s.view).unwrap(), Some(true));
        publication_read_only(&s.view);
        cases.push(json!({"observer": observer, "absentMountPreserved": true,
            "existingMountFrozen": true}));
    }
    // Damaged unrelated metadata must not prevent partial recovery from opening,
    // or prevent a healthy Published mount from being frozen on that open.
    let broken = s.root.join("state/broken.json");
    fs::write(&broken, b"{}").unwrap();
    assert!(
        direct(
            &env,
            &["thaw", s.root.to_str().unwrap(), "view", &slot(&s.layer)]
        )
        .status
        .success()
    );
    s.rt = LinuxLayerRuntime::open(&s.repo, &s.root, helper(&env)).unwrap();
    assert_eq!(mount_state(&s.view).unwrap(), Some(true));
    let partial = s.rt.recover(&mut s.repo).unwrap();
    assert_eq!(partial.failures.len(), 1);
    assert!(partial.failures[0].contains("invalid view record"));
    publication_read_only(&s.view);
    fs::remove_file(broken).unwrap();
    s.rt.unmount(&s.layer).unwrap();

    // A lost publication reply is pending work, so recovery may restore its view.
    let mut pending = setup(&env, "publication-unmounted-pending");
    let mut actor = Actor::spawn(
        &env,
        pending.repo_dir.path(),
        &pending.root,
        "publish",
        "A",
        "unmounted-pending",
        "workspace_publish_committed",
    );
    actor.marker("BOUNDARY workspace_publish_committed");
    actor.kill();
    let receipt = pending
        .repo
        .publication(&op("unmounted-pending"))
        .unwrap()
        .unwrap();
    pending.rt.unmount(&pending.layer).unwrap();
    let status = pending.rt.status(&pending.repo, &pending.layer).unwrap();
    assert!(!status.mounted && status.pending.as_deref() == Some("unmounted-pending"));
    pending.rt = LinuxLayerRuntime::open(&pending.repo, &pending.root, helper(&env)).unwrap();
    assert_eq!(mount_state(&pending.view).unwrap(), None);
    let recovery = pending.rt.recover(&mut pending.repo).unwrap();
    assert!(recovery.failures.is_empty());
    assert_eq!(recovery.pending_resolved, 1);
    publication_settled(&pending.rt, &pending.repo, &pending.layer);
    publication_read_only(&pending.view);
    assert_eq!(
        pending.repo.publication(&op("unmounted-pending")).unwrap(),
        Some(receipt)
    );
    assert_eq!(pending.repo.history().unwrap().len(), 3);
    pending.rt.unmount(&pending.layer).unwrap();
    save(
        &env,
        "publication-unmounted",
        &json!({"verdict": "VERIFIED", "cases": cases,
            "pendingRecovery": recovery, "pendingReceiptRetained": true,
            "partialRecovery": partial}),
    );
}
