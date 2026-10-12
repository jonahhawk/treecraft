//! Compares directory listers on the same root.
//!
//! Usage: `cargo run --release -p tc-scan-win --example scan_bench -- <root> [rounds] [threads]`
//!
//! Rounds alternate between listers so that file-system caching affects them equally. The first
//! round warms the cache and is reported separately.

use std::path::PathBuf;

use tc_scan::{DirLister, PortableLister, ScanOptions, scan};

/// Creates a fresh lister, so per-scan state (like hard-link sets) starts empty each round.
type MakeLister = Box<dyn Fn() -> Box<dyn DirLister>>;

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
    let mut listers: Vec<(&str, MakeLister)> =
        vec![("portable", Box::new(|| Box::new(PortableLister::new())))];
    #[cfg(windows)]
    {
        use tc_scan_win::Win32Lister;
        listers.push(("win32-extd", Box::new(|| Box::new(Win32Lister::new()))));
        listers.push((
            "win32-both",
            Box::new(|| Box::new(Win32Lister::with_both_dir_info())),
        ));
    }

    println!(
        "root={} threads={} rounds={rounds}",
        root.display(),
        opts.threads
    );
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); listers.len()];
    for round in 0..=rounds {
        for (i, (name, make)) in listers.iter().enumerate() {
            let lister = make();
            let (tree, stats) = scan(&root, &*lister, &opts).expect("scan failed");
            let secs = stats.elapsed.as_secs_f64();
            let root_node = tree.node(tree.root());
            if round == 0 {
                println!(
                    "warm-up {name:<11} {secs:7.3}s entries={} files={} size={} errors={}",
                    stats.entries,
                    root_node.files(),
                    root_node.size(),
                    stats.errors
                );
            } else {
                times[i].push(secs);
            }
        }
    }
    for ((name, _), t) in listers.iter().zip(&mut times) {
        t.sort_by(f64::total_cmp);
        let median = t[t.len() / 2];
        println!(
            "{name:<11} median {median:7.3}s  min {:7.3}s  max {:7.3}s",
            t[0],
            t[t.len() - 1]
        );
    }
}
