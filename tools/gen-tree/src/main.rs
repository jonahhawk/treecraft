//! Generates a deterministic directory tree for scanner tests and benchmarks.
//!
//! The same `--seed` and shape options always produce the same names and sizes, so scanners can
//! be checked against the totals written to `<out>/.gen-tree-manifest.txt`. That manifest file is
//! not counted in the totals.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Directory to create the tree in. Must not exist yet, or be empty.
    out: PathBuf,
    /// Random seed.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Number of directory levels below the root.
    #[arg(long, default_value_t = 4)]
    depth: u32,
    /// Subdirectories per directory.
    #[arg(long, default_value_t = 4)]
    fanout: u32,
    /// Maximum files per directory (the actual count is random in 0..=max).
    #[arg(long, default_value_t = 20)]
    max_files: u32,
    /// Largest file size in bytes. Sizes are log-uniform between 0 and this.
    #[arg(long, default_value_t = 1 << 20)]
    max_size: u64,
    /// Mix non-ASCII characters into some names.
    #[arg(long)]
    unicode: bool,
}

/// SplitMix64: tiny, deterministic, good enough for test data.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    /// Log-uniform size in `0..=max`, so small files dominate like on real disks.
    fn size(&mut self, max: u64) -> u64 {
        if max == 0 {
            return 0;
        }
        let bits = 64 - max.leading_zeros() as u64;
        let b = self.below(bits + 1);
        if b == 0 {
            return 0;
        }
        // Pick uniformly within the bucket [2^(b-1), 2^b), then clamp to max.
        let low = 1u64 << (b - 1);
        (low + self.below(low)).min(max)
    }
}

#[derive(Debug, Default)]
struct Totals {
    files: u64,
    dirs: u64,
    bytes: u64,
}

const EXTENSIONS: &[&str] = &[
    "txt", "log", "jpg", "png", "mp4", "dll", "exe", "json", "rs", "zip",
];
const UNICODE_PARTS: &[&str] = &["café", "日本語", "Ωmega", "naïve", "😀"];

fn name(rng: &mut Rng, unicode: bool, i: u32) -> String {
    if unicode && rng.below(4) == 0 {
        let part = UNICODE_PARTS[rng.below(UNICODE_PARTS.len() as u64) as usize];
        format!("{part}_{i}")
    } else {
        format!("n{i}_{:x}", rng.below(1 << 16))
    }
}

fn generate(
    dir: &Path,
    level: u32,
    args: &Args,
    rng: &mut Rng,
    totals: &mut Totals,
) -> anyhow::Result<()> {
    let files = rng.below(u64::from(args.max_files) + 1) as u32;
    for i in 0..files {
        let ext = EXTENSIONS[rng.below(EXTENSIONS.len() as u64) as usize];
        let path = dir.join(format!("{}.{ext}", name(rng, args.unicode, i)));
        let size = rng.size(args.max_size);
        let f = fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        f.set_len(size)
            .with_context(|| format!("sizing {}", path.display()))?;
        totals.files += 1;
        totals.bytes += size;
    }
    if level < args.depth {
        for i in 0..args.fanout {
            let sub = dir.join(name(rng, args.unicode, i));
            fs::create_dir(&sub).with_context(|| format!("creating {}", sub.display()))?;
            totals.dirs += 1;
            generate(&sub, level + 1, args, rng, totals)?;
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.out.exists() {
        if fs::read_dir(&args.out)?.next().is_some() {
            bail!(
                "{} exists and is not empty; refusing to write into it",
                args.out.display()
            );
        }
    } else {
        fs::create_dir_all(&args.out)?;
    }

    let mut rng = Rng(args.seed);
    let mut totals = Totals::default();
    generate(&args.out, 0, &args, &mut rng, &mut totals)?;

    let manifest = format!(
        "seed={}\nfiles={}\ndirs={}\nbytes={}\n",
        args.seed, totals.files, totals.dirs, totals.bytes
    );
    fs::write(args.out.join(".gen-tree-manifest.txt"), &manifest)?;
    print!("{manifest}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let a: Vec<u64> = {
            let mut r = Rng(42);
            (0..8).map(|_| r.next()).collect()
        };
        let b: Vec<u64> = {
            let mut r = Rng(42);
            (0..8).map(|_| r.next()).collect()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn sizes_stay_in_range() {
        let mut r = Rng(7);
        for max in [0, 1, 2, 100, 1 << 20, u64::MAX] {
            for _ in 0..1000 {
                assert!(r.size(max) <= max);
            }
        }
    }
}
