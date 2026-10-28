//! Files as fixed-size pages in a provider's key-value rows. A Durable Object keeps each page
//! as one row of its SQLite storage (`tools/node-cloudflare/src/worker.js`); tests keep them in
//! memory (`MemPages`). The SQLite databases on top use the same page size, so every page
//! SQLite writes is one whole row, and no write reads a row back first.

use std::{cell::RefCell, collections::BTreeMap};

/// The page size of every file, and of the SQLite databases stored in them: the largest
/// SQLite allows, so a write touches as few rows as it can. Rows written per day is the
/// tightest Free-plan limit (`tools/node-cloudflare/README.md`).
pub const PAGE: usize = 64 << 10;

/// A storage failure, as the provider reported it.
pub type Result<T> = std::result::Result<T, String>;

/// A provider's durable rows for named files. Every call is synchronous; a Durable Object
/// makes all of one request's calls durable together, or none of them.
pub trait Pages {
    /// The file's size in bytes, or `None` when it does not exist.
    fn size(&self, file: &str) -> Result<Option<u64>>;
    /// Records the file's size; creates it when absent. Pages past the end are dropped.
    fn set_size(&self, file: &str, size: u64) -> Result<()>;
    /// Page `index` of the file, or `None` when it was never written.
    fn get(&self, file: &str, index: u64) -> Result<Option<Vec<u8>>>;
    /// Replaces page `index` of the file. At most `PAGE` bytes.
    fn put(&self, file: &str, index: u64, bytes: &[u8]) -> Result<()>;
    /// Deletes the file and its pages.
    fn remove(&self, file: &str) -> Result<()>;
}

/// Reads `buf.len()` bytes at `offset`. Bytes past the end, or in a page never written, read
/// as zero. Returns false when the read ran past the end, which SQLite treats as a short read.
pub fn read(pages: &dyn Pages, file: &str, size: u64, buf: &mut [u8], offset: u64) -> Result<bool> {
    buf.fill(0);
    let end = offset + buf.len() as u64;
    let mut at = offset;
    while at < end.min(size) {
        let index = at / PAGE as u64;
        let within = (at % PAGE as u64) as usize;
        let take = (PAGE - within).min((end.min(size) - at) as usize);
        // A page shorter than `within` was written only before it: the rest reads as zero.
        if let Some(page) = pages.get(file, index)?
            && page.len() > within
        {
            let available = (page.len() - within).min(take);
            let into = (at - offset) as usize;
            buf[into..into + available].copy_from_slice(&page[within..within + available]);
        }
        at += take as u64;
    }
    Ok(end <= size)
}

/// Writes `bytes` at `offset` and returns the new size. A whole page is written as it is;
/// a partial one is merged with what the page held. The size is written only when it grows.
pub fn write(pages: &dyn Pages, file: &str, size: u64, bytes: &[u8], offset: u64) -> Result<u64> {
    let end = offset + bytes.len() as u64;
    let mut at = offset;
    while at < end {
        let index = at / PAGE as u64;
        let within = (at % PAGE as u64) as usize;
        let take = (PAGE - within).min((end - at) as usize);
        let from = (at - offset) as usize;
        if within == 0 && take == PAGE {
            pages.put(file, index, &bytes[from..from + take])?;
        } else {
            let mut page = pages.get(file, index)?.unwrap_or_default();
            if page.len() < within + take {
                page.resize(within + take, 0);
            }
            page[within..within + take].copy_from_slice(&bytes[from..from + take]);
            pages.put(file, index, &page)?;
        }
        at += take as u64;
    }
    if end > size {
        pages.set_size(file, end)?;
    }
    Ok(size.max(end))
}

/// Shortens or extends the file to `size`. A shortened last page keeps only its live bytes,
/// so extending the file again reads zeros there.
pub fn truncate(pages: &dyn Pages, file: &str, size: u64) -> Result<()> {
    let within = (size % PAGE as u64) as usize;
    let index = size / PAGE as u64;
    pages.set_size(file, size)?;
    if within != 0
        && let Some(mut page) = pages.get(file, index)?
        && page.len() > within
    {
        page.truncate(within);
        pages.put(file, index, &page)?;
    }
    Ok(())
}

/// The whole file, or `None` when it does not exist.
pub fn read_all(pages: &dyn Pages, file: &str) -> Result<Option<Vec<u8>>> {
    let Some(size) = pages.size(file)? else {
        return Ok(None);
    };
    let mut bytes = vec![0; usize::try_from(size).map_err(|e| e.to_string())?];
    read(pages, file, size, &mut bytes, 0)?;
    Ok(Some(bytes))
}

/// Replaces the whole file with `bytes`.
pub fn write_all(pages: &dyn Pages, file: &str, bytes: &[u8]) -> Result<()> {
    truncate(pages, file, 0)?;
    write(pages, file, 0, bytes, 0)?;
    Ok(())
}

/// One file in memory: its size and its pages by index.
type MemFile = (u64, BTreeMap<u64, Vec<u8>>);

/// Pages in memory: the store the tests run the node on.
#[derive(Default)]
pub struct MemPages {
    files: RefCell<BTreeMap<String, MemFile>>,
}

impl Pages for MemPages {
    fn size(&self, file: &str) -> Result<Option<u64>> {
        Ok(self.files.borrow().get(file).map(|(size, _)| *size))
    }

    fn set_size(&self, file: &str, size: u64) -> Result<()> {
        let mut files = self.files.borrow_mut();
        let entry = files.entry(file.into()).or_default();
        entry.0 = size;
        entry.1.retain(|index, _| index * (PAGE as u64) < size);
        Ok(())
    }

    fn get(&self, file: &str, index: u64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .files
            .borrow()
            .get(file)
            .and_then(|(_, pages)| pages.get(&index).cloned()))
    }

    fn put(&self, file: &str, index: u64, bytes: &[u8]) -> Result<()> {
        if bytes.len() > PAGE {
            return Err(format!("page of {} bytes exceeds {PAGE}", bytes.len()));
        }
        let mut files = self.files.borrow_mut();
        files
            .entry(file.into())
            .or_default()
            .1
            .insert(index, bytes.to_vec());
        Ok(())
    }

    fn remove(&self, file: &str) -> Result<()> {
        self.files.borrow_mut().remove(file);
        Ok(())
    }
}

impl MemPages {
    /// How many pages the store holds in all files.
    pub fn page_count(&self) -> usize {
        self.files.borrow().values().map(|(_, p)| p.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_across_pages() {
        let pages = MemPages::default();
        let bytes: Vec<u8> = (0..PAGE * 2 + 100).map(|i| (i % 251) as u8).collect();
        let size = write(&pages, "f", 0, &bytes, 10).unwrap();
        assert_eq!(size, bytes.len() as u64 + 10);
        let mut back = vec![0; bytes.len()];
        assert!(read(&pages, "f", size, &mut back, 10).unwrap());
        assert_eq!(back, bytes);
        let mut head = [7; 10];
        assert!(read(&pages, "f", size, &mut head, 0).unwrap());
        assert_eq!(head, [0; 10]);
    }

    #[test]
    fn a_read_past_the_end_is_short_and_zero_filled() {
        let pages = MemPages::default();
        let size = write(&pages, "f", 0, b"abc", 0).unwrap();
        let mut buf = [9; 6];
        assert!(!read(&pages, "f", size, &mut buf, 0).unwrap());
        assert_eq!(&buf, b"abc\0\0\0");
    }

    #[test]
    fn a_gap_past_a_short_page_reads_as_zero() {
        let pages = MemPages::default();
        write(&pages, "f", 0, b"abc", 0).unwrap();
        let size = write(&pages, "f", 3, b"x", PAGE as u64).unwrap();
        let mut buf = [9; 4];
        assert!(read(&pages, "f", size, &mut buf, 10).unwrap());
        assert_eq!(buf, [0; 4]);
    }

    #[test]
    fn truncate_drops_and_zeroes_what_it_cut() {
        let pages = MemPages::default();
        let bytes = vec![1; PAGE + 10];
        write(&pages, "f", 0, &bytes, 0).unwrap();
        truncate(&pages, "f", 5).unwrap();
        assert_eq!(pages.page_count(), 1);
        let size = write(&pages, "f", 5, b"x", 20).unwrap();
        let mut buf = [9; 21];
        assert!(read(&pages, "f", size, &mut buf, 0).unwrap());
        assert_eq!(&buf[..5], &[1; 5]);
        assert_eq!(&buf[5..20], &[0; 15]);
        assert_eq!(buf[20], b'x');
    }

    #[test]
    fn whole_files_round_trip() {
        let pages = MemPages::default();
        assert_eq!(read_all(&pages, "doc").unwrap(), None);
        write_all(&pages, "doc", b"long content").unwrap();
        write_all(&pages, "doc", b"short").unwrap();
        assert_eq!(read_all(&pages, "doc").unwrap().unwrap(), b"short");
    }
}
