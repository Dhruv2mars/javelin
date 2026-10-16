use std::{
    collections::HashMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(error: E) -> Self {
        Error(error.to_string())
    }
}

#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => { return Err($crate::Error(format!($($arg)*))) };
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| Error(format!("read {}: {e}", path.display())))
}

pub fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).map_err(|e| Error(format!("write {}: {e}", path.display())))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_hex(&read(path)?))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Run a command and return its stdout; stderr goes to the caller's stderr.
pub fn capture(command: &mut Command) -> Result<String> {
    let output = command
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| Error(format!("run {:?}: {e}", command.get_program())))?;
    if !output.status.success() {
        bail!("{:?} exited with {}", command.get_program(), output.status);
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn rustc_verbose() -> Result<String> {
    capture(Command::new("rustc").arg("-vV"))
}

pub fn host_triple() -> Result<String> {
    rustc_verbose()?
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| Error("rustc -vV has no host line".into()))
}

/// `--key value` / `--key=value` arguments with every key declared up front.
pub struct Args(HashMap<String, String>);

impl Args {
    pub fn parse(args: &[String], allowed: &[&str]) -> Result<Self> {
        let mut map = HashMap::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let Some(rest) = arg.strip_prefix("--") else {
                bail!("unexpected argument {arg:?}");
            };
            let (key, value) = match rest.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (
                    rest.to_string(),
                    iter.next()
                        .ok_or_else(|| Error(format!("--{rest} needs a value")))?
                        .clone(),
                ),
            };
            if !allowed.contains(&key.as_str()) {
                bail!("unknown option --{key}; allowed: {}", allowed.join(", "));
            }
            if map.insert(key.clone(), value).is_some() {
                bail!("--{key} given twice");
            }
        }
        Ok(Self(map))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn path(&self, key: &str) -> Result<PathBuf> {
        self.get(key)
            .map(PathBuf::from)
            .ok_or_else(|| Error(format!("--{key} is required")))
    }

    pub fn source_date_epoch(&self) -> Result<u64> {
        let value = match self.get("source-date-epoch") {
            Some(v) => v.to_string(),
            None => std::env::var("SOURCE_DATE_EPOCH").map_err(|_| {
                Error("--source-date-epoch or SOURCE_DATE_EPOCH is required".into())
            })?,
        };
        Ok(value.parse()?)
    }
}

pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path)
}

/// All regular files below `dir`, as `/`-joined relative paths in sorted order.
pub fn list_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(base, &entry.path(), out)?;
            } else if kind.is_file() {
                let rel = entry.path().strip_prefix(base)?.to_path_buf();
                out.push(
                    rel.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            } else {
                bail!("unsupported file type at {}", entry.path().display());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// Strip the Windows verbatim `\\?\` prefix that `canonicalize` adds; rustc and cargo
/// path remapping and many tools compare against the plain form.
pub fn simplify(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy().into_owned();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest),
        _ => path,
    }
}
