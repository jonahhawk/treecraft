//! Compares scanners on the same root.
//!
//! Usage: `cargo run --release -p tc-scan-win --example scan_bench -- <root> [rounds] [threads]`
//!
//! Rounds alternate between scanners so that file-system caching affects them equally. The first
//! round warms the cache and is reported separately. The MFT scanner is included when running
//! as administrator.

use std::path::{Path, PathBuf};

use tc_scan::{DirLister, PortableLister, ScanOptions, scan};

/// What one scan produced.
struct Outcome {
    secs: f64,
    entries: u64,
    files: u32,
    size: u64,
    errors: u64,
}

/// Runs one fresh scan, so per-scan state (like hard-link sets) starts empty each round.
type Scanner = Box<dyn Fn(&Path, &ScanOptions) -> Outcome>;

fn with_lister(make: fn() -> Box<dyn DirLister>) -> Scanner {
    Box::new(move |root, opts| {
        let (tree, stats) = scan(root, &*make(), opts).expect("scan failed");
        let n = tree.node(tree.root());
        Outcome {
            secs: stats.elapsed.as_secs_f64(),
            entries: stats.entries,
            files: n.files(),
            size: n.size(),
            errors: stats.errors,
        }
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .expect("usage: scan_bench <root> [rounds] [threads]"),
    );
    let rounds: usize = args.next().map_or(5, |s| s.parse().expect("rounds"));
    let mut opts = ScanOptions::default();
    if let Some(t) = args.next() {
        opts.threads = t.parse().expect("threads");
    }

    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut scanners: Vec<(&str, Scanner)> =
        vec![("portable", with_lister(|| Box::new(PortableLister::new())))];
    #[cfg(windows)]
    {
        use tc_scan::{SharedTree, new_tree};
        use tc_scan_win::{ScannerKind, Win32Lister, mft_unavailable_reason, start};

        scanners.push(("win32-extd", with_lister(|| Box::new(Win32Lister::new()))));
        scanners.push((
            "win32-both",
            with_lister(|| Box::new(Win32Lister::with_both_dir_info())),
        ));
        match mft_unavailable_reason(&root) {
            None => scanners.push((
                "mft",
                Box::new(|root, opts| {
                    let tree = SharedTree::new(new_tree(root).expect("bad root"));
                    let started = start(tree.clone(), ScannerKind::Mft, opts.clone())
                        .expect("MFT scanner unavailable");
                    let stats = started.handle.join();
                    let t = tree.read();
                    let n = t.node(t.root());
                    Outcome {
                        secs: stats.elapsed.as_secs_f64(),
                        entries: stats.entries,
                        files: n.files(),
                        size: n.size(),
                        errors: stats.errors,
                    }
                }),
            )),
            Some(reason) => println!("mft scanner skipped: {reason}"),
        }
    }

    println!(
        "root={} threads={} rounds={rounds}",
        root.display(),
        opts.threads
    );
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); scanners.len()];
    for round in 0..=rounds {
        for (i, (name, run)) in scanners.iter().enumerate() {
            let o = run(&root, &opts);
            if round == 0 {
                println!(
                    "warm-up {name:<11} {:7.3}s entries={} files={} size={} errors={}",
                    o.secs, o.entries, o.files, o.size, o.errors
                );
            } else {
                times[i].push(o.secs);
            }
        }
    }
    for ((name, _), t) in scanners.iter().zip(&mut times) {
        t.sort_by(f64::total_cmp);
        let median = t[t.len() / 2];
        println!(
            "{name:<11} median {median:7.3}s  min {:7.3}s  max {:7.3}s",
            t[0],
            t[t.len() - 1]
        );
    }
}
