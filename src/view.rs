use crate::config::IgnorePolicy;
use crate::durability::sync_dir;
use crate::error::{Context, JavelinError, Result};
use crate::model::{Change, ChangeKind, EntryKind, Tree, TreeEntry, ViewMarker};
use crate::objects::{ObjectBatch, ObjectKind, ObjectStore};
use crate::paths::{is_reserved_path, safe_join, validate_relative};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

#[derive(Debug, Default)]
pub struct ScanResult {
    pub tree: Tree,
    pub ignored: Vec<(String, String)>,
    pub objects: Vec<(String, ObjectKind, u64)>,
}

pub fn scan_view(view: &Path, objects: &ObjectStore) -> Result<ScanResult> {
    let policy = IgnorePolicy::load(view)?;
    scan_view_with_policy(view, objects, &policy)
}

pub fn scan_view_with_policy(
    view: &Path,
    objects: &ObjectStore,
    policy: &IgnorePolicy,
) -> Result<ScanResult> {
    let mut batch = objects.batch();
    let scan = scan_view_into_batch(view, &mut batch, policy)?;
    batch.commit()?;
    Ok(scan)
}

fn scan_view_into_batch(
    view: &Path,
    objects: &mut ObjectBatch<'_>,
    policy: &IgnorePolicy,
) -> Result<ScanResult> {
    let mut entries = Vec::new();
    let mut ignored = Vec::new();
    let mut object_records = Vec::new();
    let walker = WalkDir::new(view)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            !is_reserved_path(&entry.file_name().to_string_lossy())
        });
    for item in walker {
        let item = item.jctx("SCAN_IO", format!("cannot scan {}", view.display()))?;
        if item.depth() == 0 {
            continue;
        }
        let relative = item
            .path()
            .strip_prefix(view)
            .map_err(|_| JavelinError::corruption("scanner escaped managed view"))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| JavelinError::unsupported("non-UTF-8 paths are unsupported"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_relative(&relative)?;
        let metadata =
            fs::symlink_metadata(item.path()).jctx("SCAN_IO", format!("cannot stat {relative}"))?;
        let is_directory = metadata.file_type().is_dir();
        if let Some((is_ignored, rule)) = policy.decision(&relative, is_directory) {
            if is_ignored {
                ignored.push((relative, rule.to_string()));
                continue;
            }
        }
        let kind = if metadata.file_type().is_symlink() {
            EntryKind::Symlink
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else if metadata.is_file() {
            EntryKind::File
        } else {
            return Err(JavelinError::unsupported(format!(
                "unsupported filesystem entry {relative}"
            )));
        };
        let object_id = match kind {
            EntryKind::File => {
                let id = objects.put_blob_file(item.path())?;
                object_records.push((id.clone(), ObjectKind::Blob, metadata.len()));
                Some(id)
            }
            EntryKind::Symlink => {
                let target = symlink_target_bytes(item.path())?;
                let id = objects.put_blob(&target)?;
                object_records.push((id.clone(), ObjectKind::Blob, target.len() as u64));
                Some(id)
            }
            EntryKind::Directory => None,
        };
        let executable = kind == EntryKind::File && is_executable(&metadata);
        entries.push(TreeEntry {
            path: relative,
            kind,
            object_id,
            executable,
        });
    }
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    detect_case_collisions(&entries)?;
    Ok(ScanResult {
        tree: Tree { entries },
        ignored,
        objects: object_records,
    })
}

pub fn view_stamp(view: &Path) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let walker = WalkDir::new(view)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            !is_reserved_path(&entry.file_name().to_string_lossy())
        });
    for item in walker {
        let item = item.jctx("SCAN_IO", format!("cannot inspect {}", view.display()))?;
        if item.depth() == 0 {
            continue;
        }
        let relative = item
            .path()
            .strip_prefix(view)
            .map_err(|_| JavelinError::corruption("stamp escaped Managed view"))?
            .to_str()
            .ok_or_else(|| JavelinError::unsupported("non-UTF-8 paths are unsupported"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        validate_relative(&relative)?;
        let metadata =
            fs::symlink_metadata(item.path()).jctx("SCAN_IO", format!("cannot stat {relative}"))?;
        hasher.update(relative.as_bytes());
        hasher.update(&[0]);
        hasher.update(&metadata.len().to_be_bytes());
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        hasher.update(&modified.as_secs().to_be_bytes());
        hasher.update(&modified.subsec_nanos().to_be_bytes());
        hasher.update(&[if metadata.file_type().is_symlink() {
            3
        } else if metadata.is_dir() {
            2
        } else {
            1
        }]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            hasher.update(&metadata.permissions().mode().to_be_bytes());
        }
        if metadata.file_type().is_symlink() {
            hasher.update(&symlink_target_bytes(item.path())?);
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn detect_case_collisions(entries: &[TreeEntry]) -> Result<()> {
    let mut folded = HashMap::<String, &str>::new();
    for entry in entries {
        let key = entry.path.to_lowercase();
        if let Some(previous) = folded.insert(key, &entry.path)
            && previous != entry.path
        {
            return Err(JavelinError::unsupported(format!(
                "case-fold collision between {previous:?} and {:?}",
                entry.path
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_target_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Ok(fs::read_link(path)
        .jctx("SCAN_IO", format!("cannot read symlink {}", path.display()))?
        .as_os_str()
        .as_bytes()
        .to_vec())
}

#[cfg(not(unix))]
fn symlink_target_bytes(path: &Path) -> Result<Vec<u8>> {
    Ok(fs::read_link(path)
        .jctx("SCAN_IO", format!("cannot read symlink {}", path.display()))?
        .to_string_lossy()
        .as_bytes()
        .to_vec())
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

pub fn materialize_tree(
    tree: &Tree,
    destination: &Path,
    objects: &ObjectStore,
    marker: Option<&ViewMarker>,
) -> Result<&'static str> {
    let parent = destination.parent().unwrap_or(destination);
    fs::create_dir_all(parent).jctx("VIEW_IO", "cannot create materialization parent")?;
    let snapshot = tempfile::Builder::new()
        .prefix(".javelin-materialize-")
        .tempdir_in(parent)
        .jctx("VIEW_IO", "cannot create materialization snapshot")?;
    populate_snapshot(tree, snapshot.path(), objects, false)?;
    materialize_snapshot(tree, snapshot.path(), destination, objects, marker)
}

pub fn materialize_tree_from_cache(
    tree: &Tree,
    root_id: &str,
    metadata: &Path,
    destination: &Path,
    objects: &ObjectStore,
    marker: Option<&ViewMarker>,
) -> Result<&'static str> {
    let cache = ensure_root_cache(tree, root_id, metadata, objects)?;
    materialize_snapshot(tree, &cache, destination, objects, marker)
}

fn materialize_snapshot(
    tree: &Tree,
    source: &Path,
    destination: &Path,
    objects: &ObjectStore,
    marker: Option<&ViewMarker>,
) -> Result<&'static str> {
    fs::create_dir_all(destination).jctx(
        "VIEW_IO",
        format!("cannot create view {}", destination.display()),
    )?;
    clear_view(destination)?;
    if let Some(marker) = marker {
        let marker_bytes = serde_json::to_vec(marker).map_err(|error| {
            JavelinError::corruption(format!("cannot encode view marker: {error}"))
        })?;
        atomic_write(&destination.join(".javelin-view"), &marker_bytes, false)?;
    }
    let mut backend = "copy";
    for entry in &tree.entries {
        let path = safe_join(destination, &entry.path)?;
        match entry.kind {
            EntryKind::Directory => {
                fs::create_dir_all(&path)
                    .jctx("VIEW_IO", format!("cannot create directory {}", entry.path))?;
            }
            EntryKind::File => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).jctx(
                        "VIEW_IO",
                        format!("cannot create parent for {}", entry.path),
                    )?;
                }
                let cached = safe_join(source, &entry.path)?;
                if clone_or_copy(&cached, &path)? {
                    backend = "copy_on_write";
                }
                set_writable_mode(&path, entry.executable)?;
            }
            EntryKind::Symlink => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).jctx(
                        "VIEW_IO",
                        format!("cannot create parent for {}", entry.path),
                    )?;
                }
                let object_id = entry.object_id.as_deref().ok_or_else(|| {
                    JavelinError::corruption(format!("symlink {} has no target", entry.path))
                })?;
                let target = objects.read_blob(object_id)?;
                create_symlink(
                    &target,
                    &path,
                    symlink_target_is_directory(tree, &entry.path, &target),
                )?;
            }
        }
    }
    Ok(backend)
}

pub fn ensure_root_cache(
    tree: &Tree,
    root_id: &str,
    metadata: &Path,
    objects: &ObjectStore,
) -> Result<PathBuf> {
    let materialized = metadata.join("materialized");
    fs::create_dir_all(&materialized).jctx("VIEW_IO", "cannot create root cache directory")?;
    let target = materialized.join(root_id);
    if target.join(".complete").is_file() {
        return Ok(target);
    }
    let temp = metadata
        .join("temp")
        .join(format!("materialized-{}", ulid::Ulid::new()));
    fs::create_dir_all(&temp).jctx("VIEW_IO", "cannot create temporary root cache")?;
    populate_snapshot(tree, &temp, objects, true)?;
    let complete = temp.join(".complete");
    File::create(&complete)
        .and_then(|file| file.sync_all())
        .jctx("VIEW_IO", "cannot complete root cache")?;
    sync_dir(&temp)?;
    match fs::rename(&temp, &target) {
        Ok(()) => sync_dir(&materialized)?,
        Err(_) if target.join(".complete").is_file() => {
            fs::remove_dir_all(&temp).jctx("VIEW_IO", "cannot remove raced root cache")?;
        }
        Err(error) => {
            return Err(JavelinError::new(7, "VIEW_IO", "cannot install root cache")
                .details(serde_json::json!({"cause": error.to_string()})));
        }
    }
    Ok(target)
}

fn populate_snapshot(
    tree: &Tree,
    destination: &Path,
    objects: &ObjectStore,
    protect_files: bool,
) -> Result<()> {
    for entry in &tree.entries {
        let path = safe_join(destination, &entry.path)?;
        match entry.kind {
            EntryKind::Directory => {
                fs::create_dir_all(&path)
                    .jctx("VIEW_IO", format!("cannot cache directory {}", entry.path))?;
            }
            EntryKind::File => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).jctx("VIEW_IO", "cannot create cache parent")?;
                }
                let object_id = entry.object_id.as_deref().ok_or_else(|| {
                    JavelinError::corruption(format!("file {} has no blob", entry.path))
                })?;
                objects.write_blob_to_file(object_id, &path)?;
                if protect_files {
                    set_cache_mode(&path, entry.executable)?;
                } else {
                    set_executable(&path, entry.executable)?;
                }
            }
            EntryKind::Symlink => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).jctx("VIEW_IO", "cannot create cache parent")?;
                }
                let object_id = entry.object_id.as_deref().ok_or_else(|| {
                    JavelinError::corruption(format!("symlink {} has no target", entry.path))
                })?;
                let link_target = objects.read_blob(object_id)?;
                create_symlink(
                    &link_target,
                    &path,
                    symlink_target_is_directory(tree, &entry.path, &link_target),
                )?;
            }
        }
    }
    Ok(())
}

pub fn invalidate_root_cache(metadata: &Path, root_id: &str) -> Result<()> {
    if root_id.len() != 64 || !root_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(JavelinError::corruption(format!(
            "invalid root cache ID {root_id}"
        )));
    }
    let path = metadata.join("materialized").join(root_id);
    if path.exists() {
        fs::remove_dir_all(&path)
            .jctx("VIEW_IO", format!("cannot invalidate root cache {root_id}"))?;
    }
    Ok(())
}

fn clear_view(view: &Path) -> Result<()> {
    for item in fs::read_dir(view).jctx("VIEW_IO", format!("cannot list {}", view.display()))? {
        let item = item.jctx("VIEW_IO", "cannot read view entry")?;
        let path = item.path();
        let name = item.file_name();
        if name == ".javelin" {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).jctx("VIEW_IO", "cannot stat view entry")?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path)
                .jctx("VIEW_IO", format!("cannot remove {}", path.display()))?;
        } else {
            fs::remove_file(&path).jctx("VIEW_IO", format!("cannot remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| JavelinError::corruption("write path has no parent"))?;
    fs::create_dir_all(parent).jctx("VIEW_IO", "cannot create write parent")?;
    let temp = parent.join(format!(".javelin-write-{}.tmp", ulid::Ulid::new()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .jctx("VIEW_IO", format!("cannot create {}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .jctx("VIEW_IO", format!("cannot write {}", path.display()))?;
    set_executable(&temp, executable)?;
    fs::rename(&temp, path).jctx("VIEW_IO", format!("cannot install {}", path.display()))?;
    sync_dir(parent)?;
    Ok(())
}

#[cfg(unix)]
fn set_cache_mode(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o555 } else { 0o444 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).jctx(
        "VIEW_IO",
        format!("cannot protect cached file {}", path.display()),
    )
}

#[cfg(not(unix))]
fn set_cache_mode(path: &Path, _executable: bool) -> Result<()> {
    let mut permissions = fs::metadata(path)
        .jctx("VIEW_IO", "cannot stat cached file")?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions).jctx("VIEW_IO", "cannot protect cached file")
}

#[cfg(unix)]
fn set_writable_mode(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).jctx(
        "VIEW_IO",
        format!("cannot set view mode on {}", path.display()),
    )
}

#[cfg(not(unix))]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "this code only runs on platforms where readonly is a file attribute"
)]
fn set_writable_mode(path: &Path, _executable: bool) -> Result<()> {
    let mut permissions = fs::metadata(path)
        .jctx("VIEW_IO", "cannot stat materialized file")?
        .permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).jctx("VIEW_IO", "cannot make view file writable")
}

#[cfg(target_os = "macos")]
fn clone_or_copy(source: &Path, destination: &Path) -> Result<bool> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn clonefile(src: *const libc::c_char, dst: *const libc::c_char, flags: u32)
        -> libc::c_int;
    }
    let source_c = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| JavelinError::unsupported("NUL in materialization path"))?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| JavelinError::unsupported("NUL in materialization path"))?;
    let cloned = unsafe { clonefile(source_c.as_ptr(), destination_c.as_ptr(), 0) } == 0;
    if cloned {
        return Ok(true);
    }
    fs::copy(source, destination)
        .jctx("VIEW_IO", format!("cannot copy {}", destination.display()))?;
    Ok(false)
}

#[cfg(not(target_os = "macos"))]
fn clone_or_copy(source: &Path, destination: &Path) -> Result<bool> {
    fs::copy(source, destination)
        .jctx("VIEW_IO", format!("cannot copy {}", destination.display()))?;
    Ok(false)
}

fn symlink_target_is_directory(tree: &Tree, link_path: &str, target: &[u8]) -> bool {
    let Ok(target) = std::str::from_utf8(target) else {
        return false;
    };
    let mut resolved = Path::new(link_path)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>();
    for component in Path::new(target).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if resolved.pop().is_none() {
                    return false;
                }
            }
            std::path::Component::Normal(value) => {
                let Some(value) = value.to_str() else {
                    return false;
                };
                resolved.push(value.to_owned());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    let resolved = resolved.join("/");
    tree.entries
        .iter()
        .any(|entry| entry.path == resolved && entry.kind == EntryKind::Directory)
}

#[cfg(unix)]
fn create_symlink(target: &[u8], path: &Path, _directory: bool) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(OsStr::from_bytes(target), path).jctx(
        "VIEW_IO",
        format!("cannot create symlink {}", path.display()),
    )
}

#[cfg(windows)]
fn create_symlink(target: &[u8], path: &Path, directory: bool) -> Result<()> {
    let target = String::from_utf8(target.to_vec())
        .map_err(|_| JavelinError::unsupported("non-UTF-8 symlink target on Windows"))?;
    let result = if directory {
        std::os::windows::fs::symlink_dir(target, path)
    } else {
        std::os::windows::fs::symlink_file(target, path)
    };
    result.jctx(
        "VIEW_IO",
        format!("cannot create symlink {}", path.display()),
    )
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .jctx("VIEW_IO", format!("cannot stat {}", path.display()))?
        .permissions();
    let mut mode = permissions.mode();
    if executable {
        mode |= 0o111;
    } else {
        mode &= !0o111;
    }
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
        .jctx("VIEW_IO", format!("cannot set mode on {}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> Result<()> {
    Ok(())
}

pub fn diff_trees(old: &Tree, new: &Tree) -> Vec<Change> {
    let old = old.map();
    let new = new.map();
    let mut paths = old.keys().chain(new.keys()).cloned().collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter_map(|path| {
            let before = old.get(&path).cloned();
            let after = new.get(&path).cloned();
            if before == after {
                return None;
            }
            let change = match (&before, &after) {
                (None, Some(_)) => ChangeKind::Add,
                (Some(_), None) => ChangeKind::Delete,
                (Some(left), Some(right)) if left.kind != right.kind => ChangeKind::Type,
                (Some(left), Some(right))
                    if left.object_id == right.object_id && left.executable != right.executable =>
                {
                    ChangeKind::Mode
                }
                _ => ChangeKind::Modify,
            };
            Some(Change {
                path,
                change,
                old: before,
                new: after,
            })
        })
        .collect()
}

pub fn apply_entries(base: &Tree, updates: BTreeMap<String, Option<TreeEntry>>) -> Tree {
    let mut map = base.map();
    for (path, entry) in updates {
        if let Some(entry) = entry {
            map.insert(path, entry);
        } else {
            map.remove(&path);
        }
    }
    Tree::from_map(map)
}

pub fn write_marker(path: &Path, marker: &ViewMarker) -> Result<()> {
    let bytes = serde_json::to_vec(marker)
        .map_err(|error| JavelinError::corruption(format!("cannot encode view marker: {error}")))?;
    atomic_write(&path.join(".javelin-view"), &bytes, false)
}

pub fn managed_cache_path(metadata: &Path, root_id: &str) -> PathBuf {
    metadata.join("materialized").join(root_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_kind_follows_portable_tree_target() {
        let tree = Tree {
            entries: vec![TreeEntry {
                path: "assets".into(),
                kind: EntryKind::Directory,
                object_id: None,
                executable: false,
            }],
        };
        assert!(symlink_target_is_directory(
            &tree,
            "links/assets",
            b"../assets"
        ));
        assert!(!symlink_target_is_directory(
            &tree,
            "links/file",
            b"../missing"
        ));
    }
}
