//! Identical RNG calls, paths and bytes to layer-bench::fixture; retain bytes in RAM.
use javelin::ObjectId;
use std::path::{Path, PathBuf};

pub struct Blob {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub id: ObjectId,
}
impl Blob {
    pub fn new(path: PathBuf, bytes: Vec<u8>) -> Self {
        let id = ObjectId::for_blob(&bytes);
        Self { path, bytes, id }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    /// Log-normal file size (median about 3 KB, long tail, capped at 2 MB).
    fn size(&mut self) -> usize {
        let z = (-2.0 * self.unit().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.unit()).cos();
        ((8.0 + 1.3 * z).exp() as usize).min(2_000_000)
    }
}

pub fn fixture(files: usize, seed: u64) -> Vec<Blob> {
    let root = Path::new("");
    let mut rng = Rng(seed);
    let mut dirs = vec![root.to_path_buf()];
    let mut buf = Vec::new();
    let mut blobs = Vec::with_capacity(files);
    for i in 0..files {
        if i % 20 == 0 {
            let recent = &dirs[dirs.len().saturating_sub(50)..];
            let parent = recent[rng.next() as usize % recent.len()].clone();
            let depth = parent.strip_prefix(root).unwrap().components().count();
            let dir = if depth < 4 {
                parent
            } else {
                root.to_path_buf()
            }
            .join(format!("d{}", i / 20));
            dirs.push(dir);
        }
        let size = rng.size();
        buf.clear();
        while buf.len() < size {
            buf.extend_from_slice(&rng.next().to_le_bytes());
        }
        buf.truncate(size);
        blobs.push(Blob::new(
            dirs.last().unwrap().join(format!("f{i}.txt")),
            buf.clone(),
        ));
    }
    blobs
}
