//! Scans a folder with the Win32 and the MFT scanner and lists where they disagree: entries
//! only one of them found, and files whose sizes differ, largest difference first. Needs
//! administrator rights.
//!
//! Usage: `cargo run --release -p tc-scan-win --example mft_diff -- <root> [limit]`

#[cfg(not(windows))]
fn main() {
    eprintln!("mft_diff compares Windows scanners and only runs on Windows");
}

#[cfg(windows)]
fn main() {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use tc_model::{NodeFlags, NodeId, Tree};
    use tc_scan::{ScanOptions, SharedTree, new_tree, scan};
    use tc_scan_win::{ScannerKind, Win32Lister, start};

    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: mft_diff <root> [limit]"));
    let limit: usize = args.next().map_or(30, |s| s.parse().expect("limit"));

    let (win, _) = scan(&root, &Win32Lister::new(), &ScanOptions::default()).expect("scan");
    let shared = SharedTree::new(new_tree(&root).expect("bad root"));
    let started = start(shared.clone(), ScannerKind::Mft, ScanOptions::default())
        .expect("MFT scanner unavailable (run as administrator)");
    for e in started.handle.progress().clone().error_list() {
        println!("mft error: {}", e.message);
    }
    started.handle.join();
    let mft = shared.read();

    let children = |t: &Tree, id: NodeId| -> BTreeMap<OsString, NodeId> {
        t.children(id)
            .map(|c| (t.name(c).to_os_string(), c))
            .collect()
    };
    let counted = |t: &Tree, id: NodeId| {
        let n = t.node(id);
        if n.flags().contains(NodeFlags::HARDLINK_DUP) {
            0
        } else {
            n.size()
        }
    };

    let mut only_win = Vec::new();
    let mut only_mft = Vec::new();
    let mut differ = Vec::new();
    let mut stack = vec![(win.root(), mft.root())];
    while let Some((a, b)) = stack.pop() {
        let (ac, bc) = (children(&win, a), children(&mft, b));
        for (name, &x) in &ac {
            let Some(&y) = bc.get(name) else {
                only_win.push((win.node(x).size(), win.path(x)));
                continue;
            };
            let (nx, ny) = (win.node(x), mft.node(y));
            if nx.is_dir() && ny.is_dir() {
                stack.push((x, y));
            } else if counted(&win, x) != counted(&mft, y) {
                let (sx, sy) = (counted(&win, x), counted(&mft, y));
                differ.push((sx.abs_diff(sy), sx, sy, win.path(x)));
            }
        }
        for (name, &y) in &bc {
            if !ac.contains_key(name) {
                only_mft.push((mft.node(y).size(), mft.path(y)));
            }
        }
    }

    let (wr, mr) = (win.node(win.root()), mft.node(mft.root()));
    println!(
        "win32: {} bytes in {} files; mft: {} bytes in {} files; difference {}",
        wr.size(),
        wr.files(),
        mr.size(),
        mr.files(),
        mr.size() as i128 - wr.size() as i128
    );
    // A hard-linked file counted under a different name by each scanner shows up as one name
    // with (X, 0) and another with (0, X). Pair those off; what remains are real differences.
    let mut moved: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    let mut paired = vec![false; differ.len()];
    for (i, &(_, w, m, _)) in differ.iter().enumerate() {
        if w == 0 || m == 0 {
            let size = w.max(m);
            let other = moved.get_mut(&size).and_then(|v| {
                let j = v.iter().position(|&j| (differ[j].1 == 0) != (w == 0))?;
                Some(v.swap_remove(j))
            });
            match other {
                Some(j) => {
                    paired[i] = true;
                    paired[j] = true;
                }
                None => moved.entry(size).or_default().push(i),
            }
        }
    }
    let links = paired.iter().filter(|&&p| p).count() / 2;
    println!("{links} hard-linked files are counted under a different name (not a difference)");
    let mut differ: Vec<_> = differ
        .into_iter()
        .zip(paired)
        .filter(|(_, p)| !p)
        .map(|(d, _)| d)
        .collect();
    differ.sort_by_key(|d| std::cmp::Reverse(d.0));
    let total: u64 = differ.iter().map(|d| d.0).sum();
    println!(
        "{} files differ in size ({total} bytes in all):",
        differ.len()
    );
    for (_, w, m, path) in differ.iter().take(limit) {
        println!("  win32 {w:>14}  mft {m:>14}  {}", path.display());
    }
    for (label, list) in [
        ("only in win32", &mut only_win),
        ("only in mft", &mut only_mft),
    ] {
        list.sort_by_key(|e| std::cmp::Reverse(e.0));
        println!("{} entries {label}:", list.len());
        for (size, path) in list.iter().take(limit) {
            println!("  {size:>14}  {}", path.display());
        }
    }
}
