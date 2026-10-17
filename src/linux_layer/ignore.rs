//! Filesystem adapter for the shared pure ignore matcher.
use std::{fs, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

use crate::{
    Error, IgnoreRules, Result,
    ignore::{CONFIG, LIMIT},
};

pub(super) fn load(root: &Path) -> Result<IgnoreRules> {
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(CONFIG))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(IgnoreRules::default()),
        Err(e) => return Err(Error::InvalidInput(format!("cannot read {CONFIG}: {e}"))),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::InvalidInput(format!(
            "{CONFIG} must be a regular file"
        )));
    }
    let mut bytes = Vec::new();
    (&mut file).take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(Error::InvalidInput(format!("{CONFIG} exceeds 64 KiB")));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::InvalidInput(format!("{CONFIG} must be UTF-8")))?;
    IgnoreRules::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_file_is_bounded_regular_utf8_and_never_follows_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(CONFIG);
        assert!(!load(root.path()).unwrap().ignored("a"));
        fs::write(&path, vec![b'a'; LIMIT + 1]).unwrap();
        assert!(load(root.path()).is_err());
        fs::write(&path, [0xff]).unwrap();
        assert!(load(root.path()).is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing-outside", &path).unwrap();
        assert!(load(root.path()).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(load(root.path()).is_err());
    }
}
