//! Bounded root mount helper for Javelin Linux Layer views.
//!
//! Invoked as `sudo -n <helper> <verb> <runtime-root> <tokens...>`. It accepts fixed
//! verbs and hex tokens beneath one runtime root whose ancestors are root-owned and
//! not group/other writable. It never opens the repository, reads file contents,
//! accepts caller paths beyond the root, or uses lazy or forced unmounts.
//! Exit codes: 0 success, 2 refused input or state, 3 busy, 1 other failure.

use std::process::ExitCode;

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("javelin-linux-layer-helper supports Linux only");
    ExitCode::from(2)
}

#[cfg(unix)]
fn main() -> ExitCode {
    let args = match std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(_) => return helper::exit(helper::Failure::Invalid("non-UTF-8 argument".into())),
    };
    match helper::run(&args) {
        Ok(output) => {
            if let Some(output) = output {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        Err(failure) => helper::exit(failure),
    }
}

#[cfg(unix)]
mod helper {
    use std::{
        fs, io,
        os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, chown},
        path::{Path, PathBuf},
        process::ExitCode,
    };

    use serde_json::{Value, json};

    pub enum Failure {
        Invalid(String),
        Busy(String),
        Io(String),
    }

    impl From<io::Error> for Failure {
        fn from(e: io::Error) -> Self {
            Failure::Io(e.to_string())
        }
    }

    type R<T> = Result<T, Failure>;

    pub fn exit(failure: Failure) -> ExitCode {
        let (code, message) = match failure {
            Failure::Invalid(m) => (2, m),
            Failure::Busy(m) => (3, m),
            Failure::Io(m) => (1, m),
        };
        eprintln!("javelin-linux-layer-helper: {message}");
        ExitCode::from(code)
    }

    fn invalid<T>(message: impl Into<String>) -> R<T> {
        Err(Failure::Invalid(message.into()))
    }

    const OVERLAY_OPTIONS: &str = "index=off,metacopy=off,redirect_dir=nofollow";
    const MAX_LIST: usize = 200_000;
    const OWNER: &str = "javelin-linux-layer-runtime v1";
    /// Root-owned list of directories that may contain runtimes, one path per line.
    const PARENTS: &str = "/etc/javelin/linux-layer-parents";

    #[derive(Clone, Copy)]
    pub struct User {
        uid: u32,
        gid: u32,
    }

    fn invoking_user() -> R<User> {
        // SAFETY: getuid/geteuid have no preconditions.
        if unsafe { libc::getuid() != 0 || libc::geteuid() != 0 } {
            return invalid("must run as root through sudo");
        }
        let id = |name: &str| -> R<u32> {
            match std::env::var(name).ok().and_then(|v| v.parse::<u32>().ok()) {
                Some(id) if id != 0 => Ok(id),
                _ => invalid(format!("{name} must name a non-root invoking user")),
            }
        };
        Ok(User {
            uid: id("SUDO_UID")?,
            gid: id("SUDO_GID")?,
        })
    }

    pub fn run(args: &[String]) -> R<Option<String>> {
        if !cfg!(target_os = "linux") {
            return invalid("Linux only");
        }
        let [verb, root, tokens @ ..] = args else {
            return invalid("usage: <verb> <runtime-root> <tokens...>");
        };
        let user = invoking_user()?;
        let root = root_syntax(root)?;
        allowed_parent(&root)?;
        if verb == "init" && tokens.is_empty() {
            init(&root, user)?;
            return Ok(None);
        }
        let rt = Runtime::open(root, user)?;
        // Serializes every helper verb on this runtime, including direct sudo
        // invocations that bypass the library lock, so a view cannot be thawed
        // while its frozen upper is being listed.
        let _lock = rt.lock()?;
        // Every verb that can change mount or storage state advances one runtime
        // epoch, even if it later fails. A caller that sees the same epoch before
        // and after its reads knows no thaw, remount or unmount happened between.
        let epoch = rt.epoch()?;
        if !matches!(verb.as_str(), "list" | "status" | "epoch") {
            rt.set_epoch(epoch + 1)?;
        }
        let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
        let none = |r: R<()>| r.map(|()| None);
        match (verb.as_str(), tokens.as_slice()) {
            ("epoch", []) => Ok(Some(json!({ "epoch": epoch }).to_string())),
            ("stage", [key]) => rt.stage(key, None).map(Some),
            ("stage", [key, lower]) => rt.stage(key, Some(lower)).map(Some),
            ("freeze", ["stage", key]) => none(rt.remount(&rt.stage_mount(cache_key(key)?), true)),
            ("freeze", ["view", slot]) => none(rt.remount(&rt.view_mount(slot_token(slot)?), true)),
            ("thaw", ["view", slot]) => none(rt.remount(&rt.view_mount(slot_token(slot)?), false)),
            ("list", ["stage", key]) => {
                let key = cache_key(key)?;
                rt.list(
                    &rt.stage_mount(key),
                    &rt.stage_dir(key).join("upper"),
                    epoch,
                )
                .map(Some)
            }
            ("list", ["view", slot]) => {
                let slot = slot_token(slot)?;
                rt.list(
                    &rt.view_mount(slot),
                    &rt.view_dir(slot).join("upper"),
                    epoch,
                )
                .map(Some)
            }
            ("seal", [key, listed]) => {
                if listed.parse::<u64>().ok() != Some(epoch) {
                    return Err(Failure::Busy(
                        "stage changed after its verified listing".into(),
                    ));
                }
                none(rt.seal(cache_key(key)?))
            }
            ("abort", [key]) => none(rt.abort(cache_key(key)?)),
            ("mount-view", [slot, mode @ ("rw" | "ro"), lowers @ ..]) => rt
                .mount_view(slot_token(slot)?, *mode == "ro", lowers)
                .map(Some),
            ("unmount-view", [slot]) => none(rt.unmount(&rt.view_mount(slot_token(slot)?))),
            ("remove-view", [slot]) => none(rt.remove_view(slot_token(slot)?)),
            ("mount-ro", keys) if !keys.is_empty() => rt.mount_ro(keys).map(Some),
            ("unmount-ro", [key]) => none(rt.unmount(&rt.ro_mount(cache_key(key)?))),
            ("status", []) => rt.status().map(Some),
            _ => invalid("unknown verb or arguments"),
        }
    }

    /// Absolute, normalized and limited to a portable byte set; overlay options use
    /// `,` and `:` as separators, so those characters can never reach mount data.
    pub fn root_syntax(root: &str) -> R<PathBuf> {
        let ok = root.len() <= 200
            && root.starts_with('/')
            && root.len() > 1
            && root
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
            && root[1..]
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != "..");
        if !ok {
            return invalid("runtime root must be a normalized absolute [A-Za-z0-9_./-] path");
        }
        // Runtime names never contain dots, so they can never collide with the
        // `.<name>.javelin-init` staging directories that `init` may remove.
        let name = root.rsplit('/').next().unwrap_or_default();
        if !name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return invalid("runtime name must be [A-Za-z0-9][A-Za-z0-9_-]*");
        }
        Ok(PathBuf::from(root))
    }

    fn hex64(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    pub fn cache_key(value: &str) -> R<&str> {
        match value.split_at_checked(1) {
            Some(("b" | "c", rest)) if hex64(rest) => Ok(value),
            _ => invalid("cache key must be b|c followed by 64 lowercase hex digits"),
        }
    }

    pub fn slot_token(value: &str) -> R<&str> {
        if hex64(value) {
            Ok(value)
        } else {
            invalid("view slot must be 64 lowercase hex digits")
        }
    }

    /// Keys are either `[b<manifest>]` or `[c<checkpoint>, b<manifest>]`.
    pub fn lower_keys<'a>(keys: &[&'a str]) -> R<Vec<&'a str>> {
        let keys = keys.iter().map(|k| cache_key(k)).collect::<R<Vec<_>>>()?;
        match keys.as_slice() {
            [b] if b.starts_with('b') => Ok(keys),
            [c, b] if c.starts_with('c') && b.starts_with('b') => Ok(keys),
            _ => invalid("lower keys must be [b...] or [c..., b...]"),
        }
    }

    /// Runtimes may only be created or used beneath an administrator-listed parent.
    fn allowed_parent(root: &Path) -> R<()> {
        let config = Path::new(PARENTS);
        trusted_ancestors(config.parent().unwrap())?;
        let meta = fs::symlink_metadata(config)?;
        if !meta.file_type().is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return invalid(format!("{PARENTS} must be a root-owned regular file"));
        }
        let parent = root.parent().unwrap_or(root);
        if fs::read_to_string(config)?
            .lines()
            .any(|line| !line.is_empty() && Path::new(line) == parent)
        {
            Ok(())
        } else {
            invalid(format!(
                "{} is not a permitted runtime parent",
                parent.display()
            ))
        }
    }

    fn trusted_directory(path: &Path) -> R<()> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.file_type().is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return invalid(format!(
                "{} must be a root-owned directory not writable by group or others",
                path.display()
            ));
        }
        Ok(())
    }

    fn trusted_ancestors(path: &Path) -> R<()> {
        for ancestor in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            trusted_directory(ancestor)?;
        }
        if fs::canonicalize(path)? != path {
            return invalid("runtime path must be canonical");
        }
        Ok(())
    }

    fn make_dir(path: &Path, mode: u32, owner: Option<User>) -> R<()> {
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        if let Some(user) = owner {
            chown(path, Some(user.uid), Some(user.gid))?;
        }
        Ok(())
    }

    fn sync_dir(path: &Path) -> R<()> {
        fs::File::open(path)?.sync_all()?;
        Ok(())
    }

    /// (relative path, mode, owned by invoking user)
    const LAYOUT: [(&str, u32, bool); 10] = [
        ("hidden", 0o700, false),
        ("hidden/empty", 0o755, false),
        ("hidden/cache", 0o700, false),
        ("hidden/stage", 0o700, false),
        ("hidden/views", 0o700, false),
        ("mnt", 0o755, false),
        ("mnt/stage", 0o755, false),
        ("mnt/ro", 0o755, false),
        ("views", 0o755, false),
        ("state", 0o700, true),
    ];

    fn owner_text(root: &Path, user: User) -> String {
        format!(
            "{OWNER}\nroot={}\nuid={}\ngid={}\n",
            root.display(),
            user.uid,
            user.gid
        )
    }

    fn init(root: &Path, user: User) -> R<()> {
        let parent = root.parent().ok_or(Failure::Invalid("no parent".into()))?;
        trusted_ancestors(parent)?;
        if fs::symlink_metadata(root).is_ok() {
            Runtime::open(root.to_path_buf(), user)?;
            return Ok(());
        }
        let name = root.file_name().unwrap().to_string_lossy();
        let staging = parent.join(format!(".{name}.javelin-init"));
        if fs::symlink_metadata(&staging).is_ok() {
            fs::remove_dir_all(&staging)?;
        }
        make_dir(&staging, 0o755, None)?;
        for (relative, mode, owned) in LAYOUT {
            make_dir(&staging.join(relative), mode, owned.then_some(user))?;
        }
        let owner = staging.join("owner");
        fs::write(&owner, owner_text(root, user))?;
        fs::set_permissions(&owner, fs::Permissions::from_mode(0o644))?;
        fs::File::open(&owner)?.sync_all()?;
        sync_dir(&staging)?;
        fs::rename(&staging, root)?;
        sync_dir(parent)?;
        Runtime::open(root.to_path_buf(), user).map(|_| ())
    }

    pub struct MountRow {
        pub target: PathBuf,
        pub fstype: String,
        pub readonly: bool,
        pub super_options: String,
    }

    fn unescape(field: &str) -> String {
        let bytes = field.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\'
                && i + 3 < bytes.len()
                && bytes[i + 1..i + 4]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                let digits = std::str::from_utf8(&bytes[i + 1..i + 4]).unwrap();
                out.push(u8::from_str_radix(digits, 8).unwrap_or(b'?'));
                i += 4;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    pub fn parse_mountinfo(text: &str) -> Vec<MountRow> {
        text.lines()
            .filter_map(|line| {
                let (left, right) = line.split_once(" - ")?;
                let left: Vec<&str> = left.split(' ').collect();
                let right: Vec<&str> = right.split(' ').collect();
                Some(MountRow {
                    target: PathBuf::from(unescape(left.get(4)?)),
                    readonly: left.get(5)?.split(',').any(|o| o == "ro"),
                    fstype: right.first()?.to_string(),
                    super_options: right.get(2).map(|s| unescape(s)).unwrap_or_default(),
                })
            })
            .collect()
    }

    fn mounts() -> R<Vec<MountRow>> {
        Ok(parse_mountinfo(&fs::read_to_string(
            "/proc/self/mountinfo",
        )?))
    }

    fn mounted(target: &Path) -> R<Option<MountRow>> {
        Ok(mounts()?.into_iter().rev().find(|m| m.target == target))
    }

    fn any_mount_under(path: &Path) -> R<bool> {
        Ok(mounts()?.iter().any(|m| m.target.starts_with(path)))
    }

    pub fn overlay_data(lowers: &[PathBuf], upper: Option<(&Path, &Path)>) -> String {
        let lower = lowers
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        match upper {
            Some((upper, work)) => format!(
                "lowerdir={lower},upperdir={},workdir={},{OVERLAY_OPTIONS}",
                upper.display(),
                work.display()
            ),
            None => format!("lowerdir={lower},{OVERLAY_OPTIONS}"),
        }
    }

    #[cfg(target_os = "linux")]
    fn cstring(path: &Path) -> R<std::ffi::CString> {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| Failure::Invalid("NUL in path".into()))
    }

    #[cfg(target_os = "linux")]
    fn errno(what: &str) -> Failure {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EBUSY) {
            Failure::Busy(format!("{what}: {error}"))
        } else {
            Failure::Io(format!("{what}: {error}"))
        }
    }

    #[cfg(target_os = "linux")]
    fn mount_overlay(target: &Path, data: &str, readonly: bool) -> R<()> {
        let target = cstring(target)?;
        let data = std::ffi::CString::new(data).unwrap();
        let mut flags = libc::MS_NOSUID | libc::MS_NODEV;
        if readonly {
            flags |= libc::MS_RDONLY;
        }
        // SAFETY: all pointers are valid NUL-terminated strings for the call.
        let result = unsafe {
            libc::mount(
                c"overlay".as_ptr(),
                target.as_ptr(),
                c"overlay".as_ptr(),
                flags,
                data.as_ptr().cast(),
            )
        };
        if result != 0 {
            return Err(errno("mount overlay"));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn remount_superblock(target: &Path, readonly: bool) -> R<()> {
        let target = cstring(target)?;
        let mut flags = libc::MS_REMOUNT | libc::MS_NOSUID | libc::MS_NODEV;
        if readonly {
            flags |= libc::MS_RDONLY;
        }
        // SAFETY: valid target string; null source, type and data are permitted for remount.
        let result = unsafe {
            libc::mount(
                std::ptr::null(),
                target.as_ptr(),
                std::ptr::null(),
                flags,
                std::ptr::null(),
            )
        };
        if result != 0 {
            return Err(errno("remount"));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn unmount_exact(target: &Path) -> R<()> {
        let target = cstring(target)?;
        // SAFETY: valid target string. No MNT_DETACH or MNT_FORCE.
        if unsafe { libc::umount2(target.as_ptr(), libc::UMOUNT_NOFOLLOW) } != 0 {
            return Err(errno("unmount"));
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn mount_overlay(_: &Path, _: &str, _: bool) -> R<()> {
        invalid("Linux only")
    }
    #[cfg(not(target_os = "linux"))]
    fn remount_superblock(_: &Path, _: bool) -> R<()> {
        invalid("Linux only")
    }
    #[cfg(not(target_os = "linux"))]
    fn unmount_exact(_: &Path) -> R<()> {
        invalid("Linux only")
    }

    pub struct Runtime {
        root: PathBuf,
        user: User,
    }

    impl Runtime {
        fn open(root: PathBuf, user: User) -> R<Self> {
            trusted_ancestors(&root)?;
            if root.metadata()?.mode() & 0o7777 != 0o755 {
                return invalid("runtime root must have mode 0755");
            }
            let owner = root.join("owner");
            let meta = fs::symlink_metadata(&owner)?;
            if !meta.file_type().is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                return invalid("runtime owner marker must be a root-owned regular file");
            }
            if fs::read_to_string(&owner)? != owner_text(&root, user) {
                return invalid("runtime root belongs to another path or user");
            }
            for (relative, mode, owned) in LAYOUT {
                let meta = fs::symlink_metadata(root.join(relative))?;
                let uid = if owned { user.uid } else { 0 };
                if !meta.file_type().is_dir() || meta.uid() != uid || meta.mode() & 0o7777 != mode {
                    return invalid(format!("runtime layout entry {relative} is not as created"));
                }
            }
            Ok(Self { root, user })
        }

        fn lock(&self) -> R<fs::File> {
            use std::os::unix::fs::OpenOptionsExt;
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(self.root.join("hidden/.lock"))?;
            file.lock()?;
            Ok(file)
        }

        fn epoch(&self) -> R<u64> {
            match fs::read_to_string(self.root.join("hidden/epoch")) {
                Ok(text) => text
                    .trim()
                    .parse()
                    .map_err(|_| Failure::Io("corrupt runtime epoch".into())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
                Err(e) => Err(e.into()),
            }
        }

        fn set_epoch(&self, value: u64) -> R<()> {
            let temporary = self.root.join("hidden/epoch.tmp");
            fs::write(&temporary, value.to_string())?;
            fs::File::open(&temporary)?.sync_all()?;
            fs::rename(&temporary, self.root.join("hidden/epoch"))?;
            Ok(())
        }

        fn stage_dir(&self, key: &str) -> PathBuf {
            self.root.join("hidden/stage").join(key)
        }
        fn stage_mount(&self, key: &str) -> PathBuf {
            self.root.join("mnt/stage").join(key)
        }
        fn cache(&self, key: &str) -> PathBuf {
            self.root.join("hidden/cache").join(key)
        }
        fn view_dir(&self, slot: &str) -> PathBuf {
            self.root.join("hidden/views").join(slot)
        }
        fn view_mount(&self, slot: &str) -> PathBuf {
            self.root.join("views").join(slot)
        }
        fn ro_mount(&self, key: &str) -> PathBuf {
            self.root.join("mnt/ro").join(key)
        }

        /// Unmounted mountpoints are root-owned 0555, so ordinary writers cannot
        /// create files beneath a missing view.
        fn mountpoint(&self, path: &Path) -> R<()> {
            match fs::symlink_metadata(path) {
                Ok(meta) => {
                    if !meta.file_type().is_dir() || meta.uid() != 0 {
                        return invalid("mountpoint must be a root-owned directory");
                    }
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => make_dir(path, 0o555, None),
                Err(e) => Err(e.into()),
            }
        }

        fn ensure_dir(&self, path: &Path, mode: u32, owned: bool) -> R<()> {
            match fs::symlink_metadata(path) {
                Ok(meta) => {
                    let uid = if owned { self.user.uid } else { 0 };
                    if !meta.file_type().is_dir() || meta.uid() != uid {
                        return invalid(format!("{} has unexpected type or owner", path.display()));
                    }
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    make_dir(path, mode, owned.then_some(self.user))
                }
                Err(e) => Err(e.into()),
            }
        }

        fn overlay_at(&self, target: &Path) -> R<MountRow> {
            match mounted(target)? {
                Some(row) if row.fstype == "overlay" => Ok(row),
                _ => invalid(format!("no overlay mounted at {}", target.display())),
            }
        }

        fn stage(&self, key: &str, lower: Option<&str>) -> R<String> {
            let key = cache_key(key)?;
            let lower = match (key.starts_with('b'), lower) {
                (true, None) => self.root.join("hidden/empty"),
                (false, Some(lower)) if lower_keys(&[key, lower]).is_ok() => {
                    let path = self.cache(lower);
                    if !fs::symlink_metadata(&path)?.file_type().is_dir() {
                        return invalid("lower cache is missing");
                    }
                    path
                }
                _ => return invalid("base keys take no lower; checkpoint keys take a base"),
            };
            if fs::symlink_metadata(self.cache(key)).is_ok() {
                return Ok(json!({"cached": true}).to_string());
            }
            self.abort(key)?;
            let dir = self.stage_dir(key);
            make_dir(&dir, 0o700, None)?;
            make_dir(&dir.join("upper"), 0o700, Some(self.user))?;
            make_dir(&dir.join("work"), 0o700, None)?;
            let target = self.stage_mount(key);
            self.mountpoint(&target)?;
            mount_overlay(
                &target,
                &overlay_data(&[lower], Some((&dir.join("upper"), &dir.join("work")))),
                false,
            )?;
            Ok(json!({"cached": false}).to_string())
        }

        fn remount(&self, target: &Path, readonly: bool) -> R<()> {
            let row = self.overlay_at(target)?;
            if row.readonly == readonly {
                return Ok(());
            }
            remount_superblock(target, readonly)
        }

        fn unmount(&self, target: &Path) -> R<()> {
            if mounted(target)?.is_some() {
                self.overlay_at(target)?;
                unmount_exact(target)?;
            }
            if mounted(target)?.is_some() {
                return Err(Failure::Busy("mount remains after unmount".into()));
            }
            Ok(())
        }

        /// Lists the upper only while its overlay superblock is frozen read-only.
        fn list(&self, target: &Path, upper: &Path, epoch: u64) -> R<String> {
            let row = self.overlay_at(target)?;
            if !row.readonly || !row.super_options.split(',').any(|o| o == "ro") {
                return invalid("upper listing requires a frozen read-only overlay");
            }
            let mut entries = Vec::new();
            list_tree(upper, Path::new(""), &mut entries)?;
            if !self.overlay_at(target)?.readonly {
                return invalid("overlay thawed during upper listing");
            }
            Ok(json!({ "epoch": epoch, "entries": entries }).to_string())
        }

        fn seal(&self, key: &str) -> R<()> {
            let target = self.stage_mount(key);
            let row = self.overlay_at(&target)?;
            if !row.readonly {
                return invalid("seal requires a frozen, verified stage");
            }
            self.unmount(&target)?;
            let dir = self.stage_dir(key);
            if fs::symlink_metadata(self.cache(key)).is_err() {
                fs::rename(dir.join("upper"), self.cache(key))?;
                sync_dir(&self.root.join("hidden/cache"))?;
            }
            fs::remove_dir_all(&dir)?;
            sync_dir(&self.root.join("hidden/stage"))
        }

        fn abort(&self, key: &str) -> R<()> {
            self.unmount(&self.stage_mount(key))?;
            let dir = self.stage_dir(key);
            if any_mount_under(&dir)? {
                return invalid("mount remains beneath stage");
            }
            if fs::symlink_metadata(&dir).is_ok() {
                fs::remove_dir_all(&dir)?;
            }
            Ok(())
        }

        fn mount_view(&self, slot: &str, readonly: bool, keys: &[&str]) -> R<String> {
            let keys = lower_keys(keys)?;
            let mut lowers = Vec::new();
            for key in &keys {
                let path = self.cache(key);
                if !fs::symlink_metadata(&path)?.file_type().is_dir() {
                    return invalid("lower cache is missing");
                }
                lowers.push(path);
            }
            let dir = self.view_dir(slot);
            let (upper, work) = (dir.join("upper"), dir.join("work"));
            let target = self.view_mount(slot);
            let data = overlay_data(&lowers, Some((&upper, &work)));
            if let Some(row) = mounted(&target)? {
                let expected: Vec<&str> = data.split(',').take(2).collect();
                let actual: Vec<&str> = row.super_options.split(',').collect();
                if row.fstype != "overlay" || !expected.iter().all(|e| actual.contains(e)) {
                    return invalid("view is mounted with different layers");
                }
                return Ok(json!({"already": true, "readonly": row.readonly}).to_string());
            }
            self.ensure_dir(&dir, 0o700, false)?;
            self.ensure_dir(&upper, 0o700, true)?;
            self.ensure_dir(&work, 0o700, false)?;
            self.mountpoint(&target)?;
            mount_overlay(&target, &data, readonly)?;
            Ok(json!({"already": false, "readonly": readonly}).to_string())
        }

        fn remove_view(&self, slot: &str) -> R<()> {
            let target = self.view_mount(slot);
            let dir = self.view_dir(slot);
            if mounted(&target)?.is_some() || any_mount_under(&dir)? {
                return invalid("view is still mounted");
            }
            if fs::symlink_metadata(&dir).is_ok() {
                fs::remove_dir_all(&dir)?;
            }
            if fs::symlink_metadata(&target).is_ok() {
                fs::remove_dir(&target)?;
            }
            sync_dir(&self.root.join("hidden/views"))
        }

        /// Read-only checkpoint view. The empty bottom lower satisfies the kernel's
        /// two-lower minimum for mounts without an upper.
        fn mount_ro(&self, keys: &[&str]) -> R<String> {
            let keys = lower_keys(keys)?;
            let target = self.ro_mount(keys[0]);
            if mounted(&target)?.is_some() {
                self.overlay_at(&target)?;
                return Ok(json!({"already": true}).to_string());
            }
            let mut lowers = Vec::new();
            for key in &keys {
                let path = self.cache(key);
                if !fs::symlink_metadata(&path)?.file_type().is_dir() {
                    return invalid("lower cache is missing");
                }
                lowers.push(path);
            }
            lowers.push(self.root.join("hidden/empty"));
            self.mountpoint(&target)?;
            mount_overlay(&target, &overlay_data(&lowers, None), true)?;
            Ok(json!({"already": false}).to_string())
        }

        fn status(&self) -> R<String> {
            let names = |relative: &str| -> R<Vec<String>> {
                let mut names = fs::read_dir(self.root.join(relative))?
                    .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
                    .collect::<R<Vec<_>>>()?;
                names.sort();
                Ok(names)
            };
            let mounts: Vec<Value> = mounts()?
                .into_iter()
                .filter(|m| m.target.starts_with(&self.root))
                .map(|m| json!({"target": m.target, "fstype": m.fstype, "readonly": m.readonly}))
                .collect();
            Ok(json!({
                "caches": names("hidden/cache")?,
                "stages": names("hidden/stage")?,
                "views": names("hidden/views")?,
                "mounts": mounts,
            })
            .to_string())
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Metadata-only walk; never follows symlinks and never reads file contents.
    fn list_tree(root: &Path, relative: &Path, out: &mut Vec<Value>) -> R<()> {
        use std::os::unix::ffi::OsStrExt;
        let mut children = fs::read_dir(root.join(relative))?
            .map(|e| Ok(e?.file_name()))
            .collect::<R<Vec<_>>>()?;
        children.sort();
        for name in children {
            let path = relative.join(&name);
            let meta = fs::symlink_metadata(root.join(&path))?;
            let kind = meta.file_type();
            let kind = if kind.is_file() {
                "file"
            } else if kind.is_symlink() {
                "symlink"
            } else if kind.is_dir() {
                "directory"
            } else if kind.is_char_device() && meta.rdev() == 0 {
                "whiteout"
            } else {
                "other"
            };
            if out.len() >= MAX_LIST {
                return Err(Failure::Io("upper listing exceeds entry limit".into()));
            }
            out.push(json!({"path": hex(path.as_os_str().as_bytes()), "kind": kind}));
            if kind == "directory" {
                list_tree(root, &path, out)?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn tokens_and_roots_are_strict() {
            let h = "a".repeat(64);
            assert!(cache_key(&format!("b{h}")).is_ok());
            assert!(cache_key(&format!("c{h}")).is_ok());
            for bad in [
                h.clone(),
                format!("d{h}"),
                format!("b{}", "A".repeat(64)),
                "b".into(),
            ] {
                assert!(cache_key(&bad).is_err());
            }
            assert!(slot_token(&h).is_ok());
            assert!(slot_token("../x").is_err());
            assert!(lower_keys(&[&format!("b{h}")]).is_ok());
            assert!(lower_keys(&[&format!("c{h}"), &format!("b{h}")]).is_ok());
            assert!(lower_keys(&[&format!("b{h}"), &format!("c{h}")]).is_err());
            assert!(lower_keys(&[&format!("c{h}")]).is_err());
            assert!(root_syntax("/var/lib/javelin-ci/rt-1").is_ok());
            assert!(root_syntax("/var/lib/javelin.ci/rt_1").is_ok());
            for bad in [
                "/p/.rt-y.javelin-init",
                "/p/rt.1",
                "/p/-rt",
                "/p/_rt",
                "",
                "/",
                "rel/x",
                "/a//b",
                "/a/./b",
                "/a/../b",
                "/a/b/",
                "/a,b",
                "/a:b",
                "/a b",
            ] {
                assert!(root_syntax(bad).is_err(), "{bad}");
            }
        }

        #[test]
        fn mountinfo_rows_decode_targets_and_flags() {
            let text = "36 35 0:31 / /r/views/x\\040y ro,nosuid,nodev,relatime shared:1 - overlay overlay ro,lowerdir=/r/a:/r/b,upperdir=/r/u,workdir=/r/w\n\
                        37 35 8:1 / /other rw - ext4 /dev/sda1 rw\n";
            let rows = parse_mountinfo(text);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].target, PathBuf::from("/r/views/x y"));
            assert!(rows[0].readonly);
            assert_eq!(rows[0].fstype, "overlay");
            assert!(rows[0].super_options.contains("lowerdir=/r/a:/r/b"));
            assert!(!rows[1].readonly);
        }

        #[test]
        fn overlay_data_keeps_verified_options() {
            let data = overlay_data(
                &[PathBuf::from("/r/c"), PathBuf::from("/r/b")],
                Some((Path::new("/r/u"), Path::new("/r/w"))),
            );
            assert_eq!(
                data,
                "lowerdir=/r/c:/r/b,upperdir=/r/u,workdir=/r/w,index=off,metacopy=off,redirect_dir=nofollow"
            );
            assert_eq!(
                overlay_data(&[PathBuf::from("/r/b"), PathBuf::from("/r/e")], None),
                "lowerdir=/r/b:/r/e,index=off,metacopy=off,redirect_dir=nofollow"
            );
        }
    }
}
