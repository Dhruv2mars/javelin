//! Deterministic fixture content from a fixed seed. Nothing here hashes core objects;
//! object IDs always come from the public `put_blob`/`put_manifest` results.
const WORDS: [&str; 32] = [
    "alpha",
    "bravo",
    "charlie",
    "delta",
    "echo",
    "foxtrot",
    "golf",
    "hotel",
    "india",
    "juliet",
    "kilo",
    "lima",
    "mike",
    "november",
    "oscar",
    "papa",
    "quebec",
    "romeo",
    "sierra",
    "tango",
    "uniform",
    "victor",
    "whiskey",
    "xray",
    "yankee",
    "zulu",
    "layer",
    "world",
    "object",
    "manifest",
    "publish",
    "checkpoint",
];

pub fn mix(seed: u64, a: u64, b: u64) -> u64 {
    let mut rng = Rng(seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.rotate_left(32));
    rng.next();
    rng.next()
}

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Class {
    Text,
    Source,
    Binary,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Source => "source",
            Self::Binary => "binary",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Fixture {
    pub seed: u64,
    pub binary_max: usize,
}

impl Fixture {
    pub fn new(seed: u64, binary_max: usize) -> Self {
        Self { seed, binary_max }
    }

    pub fn class(&self, i: usize) -> Class {
        match mix(self.seed, i as u64, 1) % 100 {
            0..=54 => Class::Text,
            55..=94 => Class::Source,
            _ => Class::Binary,
        }
    }

    /// Sizes: 70% 64 B..4 KiB, 25% 4..64 KiB, 5% up to `binary_max` (binary only).
    pub fn size(&self, i: usize) -> usize {
        let r = mix(self.seed, i as u64, 2);
        let roll = r % 100;
        let span = |lo: usize, hi: usize| lo + (mix(self.seed, i as u64, 3) as usize) % (hi - lo);
        match self.class(i) {
            Class::Binary => span(65_536.min(self.binary_max - 1), self.binary_max),
            _ if roll < 74 => span(64, 4096),
            _ => span(4096, 65_536),
        }
    }

    pub fn path(&self, i: usize) -> String {
        let ext = match self.class(i) {
            Class::Text => "txt",
            Class::Source => "rs",
            Class::Binary => "bin",
        };
        format!("d{:02}/s{}/f{i}.{ext}", i % 16, i / 16)
    }

    /// File content at a version. Every tenth file at version 0 duplicates its
    /// predecessor's bytes so identical-content sharing is exercised on purpose.
    pub fn content(&self, i: usize, version: u64) -> Vec<u8> {
        let source = if version == 0 && i % 10 == 9 {
            i - 1
        } else {
            i
        };
        let size = self.size(source);
        let mut rng = Rng(mix(self.seed, source as u64, 1000 + version));
        let mut out = Vec::with_capacity(size + 32);
        match self.class(source) {
            Class::Binary => {
                while out.len() < size {
                    out.extend_from_slice(&rng.next().to_le_bytes());
                }
            }
            Class::Text => {
                while out.len() < size {
                    out.extend_from_slice(WORDS[(rng.next() % 32) as usize].as_bytes());
                    out.push(if rng.next().is_multiple_of(12) {
                        b'\n'
                    } else {
                        b' '
                    });
                }
            }
            Class::Source => {
                while out.len() < size {
                    let n = rng.next() % 10_000;
                    let line = format!(
                        "pub fn {}_{n}(x: u64) -> u64 {{ x.wrapping_add({}) }}\n",
                        WORDS[(rng.next() % 32) as usize],
                        rng.next() % 1000
                    );
                    out.extend_from_slice(line.as_bytes());
                }
            }
        }
        out.truncate(size);
        out
    }
}

/// Deterministic incompressible bytes for large single blobs and contention payloads.
pub fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        out.extend_from_slice(&rng.next().to_le_bytes());
    }
    out.truncate(len);
    out
}
