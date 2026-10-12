//! MFT scanner tests.
//!
//! Without administrator rights only the refusal paths can be checked. In an elevated test run
//! (start the terminal as administrator), the differential tests build edge cases in a temp
//! folder on the system drive and require the MFT scanner to produce the same tree as the Win32
//! scanner: same names, flags, sizes and totals.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use tc_model::{NodeFlags, NodeId, Tree};
use tc_scan::{ScanOptions, SharedTree, new_tree, scan};
use tc_scan_win::{ScannerKind, Win32Lister, is_elevated, start};

fn skip_unless_elevated(test: &str) -> bool {
    if is_elevated() {
        return false;
    }
    eprintln!("{test}: skipped, needs administrator rights");
    true
}

#[test]
fn mft_scanner_is_refused_without_elevation() {
    if is_elevated() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tree = SharedTree::new(new_tree(dir.path()).unwrap());
    let err = start(tree, ScannerKind::Mft, ScanOptions::default()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("administrator"), "{err}");
}

#[test]
fn auto_uses_the_win32_scanner_without_elevation() {
    if is_elevated() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tree = SharedTree::new(new_tree(dir.path()).unwrap());
    let started = start(tree, ScannerKind::Auto, ScanOptions::default()).unwrap();
    assert_eq!(started.scanner, "win32");
    started.handle.join();
}

fn mft_scan(root: &Path) -> SharedTree {
    let shared = SharedTree::new(new_tree(root).unwrap());
    let started = start(shared.clone(), ScannerKind::Mft, ScanOptions::default()).unwrap();
    assert_eq!(started.scanner, "mft");
    let progress = started.handle.progress().clone();
    started.handle.join();
    for e in progress.error_list() {
        assert!(
            !e.message.contains("used the Win32 scanner instead"),
            "MFT scan fell back: {}",
            e.message
        );
    }
    shared
}

fn children_by_name(t: &Tree, id: NodeId) -> BTreeMap<OsString, NodeId> {
    t.children(id)
        .map(|c| (t.name(c).to_os_string(), c))
        .collect()
}

/// Asserts that two scans of the same folder agree, below `a`/`b`.
fn assert_same(at: &Tree, a: NodeId, bt: &Tree, b: NodeId) {
    let path = at.path(a);
    let (ac, bc) = (children_by_name(at, a), children_by_name(bt, b));
    assert_eq!(
        ac.keys().collect::<Vec<_>>(),
        bc.keys().collect::<Vec<_>>(),
        "children of {}",
        path.display()
    );
    for (name, &x) in &ac {
        let y = bc[name];
        let (nx, ny) = (at.node(x), bt.node(y));
        let what = path.join(name);
        let kind = |f: NodeFlags| (f.contains(NodeFlags::DIR), f.contains(NodeFlags::LINK));
        assert_eq!(
            kind(nx.flags()),
            kind(ny.flags()),
            "kind of {}",
            what.display()
        );
        let dup = NodeFlags::HARDLINK_DUP;
        if !nx.is_dir() && !nx.flags().contains(dup) && !ny.flags().contains(dup) {
            assert_eq!(nx.size(), ny.size(), "size of {}", what.display());
            assert_eq!(
                nx.allocated(),
                ny.allocated(),
                "allocated size of {}",
                what.display()
            );
        }
        if nx.is_dir() && !nx.flags().contains(NodeFlags::LINK) {
            assert_same(at, x, bt, y);
        }
    }
    // Hard links may be counted under different names, but each file exactly once.
    let (na, nb) = (at.node(a), bt.node(b));
    assert_eq!(na.size(), nb.size(), "total size of {}", path.display());
    assert_eq!(
        na.allocated(),
        nb.allocated(),
        "total allocated of {}",
        path.display()
    );
    assert_eq!(na.files(), nb.files(), "file count of {}", path.display());
    assert_eq!(na.dirs(), nb.dirs(), "folder count of {}", path.display());
}

fn compare_with_win32(root: &Path) {
    let (win, _) = scan(root, &Win32Lister::new(), &ScanOptions::default()).unwrap();
    let mft = mft_scan(root);
    let mft = mft.read();
    assert_same(&win, win.root(), &mft, mft.root());
}

fn write(path: &Path, len: u64) {
    fs::File::create(path).unwrap().set_len(len).unwrap();
}

#[test]
fn mft_matches_win32_on_edge_cases() {
    if skip_unless_elevated("mft_matches_win32_on_edge_cases") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // Sizes from resident (tiny) to non-resident, plus empty files and folders.
    for (i, len) in [0u64, 1, 300, 700, 5_000, 100_000, 3_000_000]
        .iter()
        .enumerate()
    {
        write(&d.join(format!("file{i}.bin")), *len);
    }
    fs::create_dir_all(d.join("a/b/c/empty")).unwrap();
    write(&d.join("a/b/c/deep.txt"), 42);
    // Hard links, in one folder and across folders.
    write(&d.join("original.bin"), 50_000);
    fs::hard_link(d.join("original.bin"), d.join("link1.bin")).unwrap();
    fs::hard_link(d.join("original.bin"), d.join("a/link2.bin")).unwrap();
    // A junction (not followed) pointing back up. cmd would read "/loop" in "a/loop" as a
    // switch, so pass a backslash path.
    let out = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(d.join("a").join("loop"))
        .arg(d)
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink /J failed: {out:?}");
    // An alternate data stream (not counted by either scanner).
    let mut f = fs::File::create(d.join("with-stream.txt")).unwrap();
    f.write_all(b"main").unwrap();
    // Close it: the directory index entry, which the Win32 scanner reads, only gets the new
    // size when the handle closes, while the MFT record has it at once.
    drop(f);
    fs::write(d.join("with-stream.txt:extra"), vec![7u8; 10_000]).unwrap();
    // Unicode and long names, and a path longer than 260 characters.
    write(&d.join("ünïcødé 日本語.txt"), 10);
    let mut long = d.to_path_buf();
    for i in 0..6 {
        long.push(format!("{i}-{}", "x".repeat(50)));
    }
    fs::create_dir_all(&long).unwrap();
    write(&long.join("far.txt"), 123);

    compare_with_win32(d);
}

/// A file with hundreds of names has more `$FILE_NAME` attributes than fit in one record, so
/// NTFS moves them to extension records listed in an `$ATTRIBUTE_LIST`.
#[test]
fn mft_merges_extension_records() {
    if skip_unless_elevated("mft_merges_extension_records") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(&d.join("many-names.bin"), 77_777);
    fs::create_dir(d.join("links")).unwrap();
    for i in 0..300 {
        fs::hard_link(
            d.join("many-names.bin"),
            d.join(format!("links/a-rather-long-hard-link-name-{i:04}.bin")),
        )
        .unwrap();
    }
    compare_with_win32(d);
    let mft = mft_scan(d);
    let t = mft.read();
    assert_eq!(t.node(t.root()).size(), 77_777);
    assert_eq!(t.node(t.root()).files(), 301);
}

/// Many files created and deleted leave reused and free records behind.
#[test]
fn mft_skips_deleted_and_reused_records() {
    if skip_unless_elevated("mft_skips_deleted_and_reused_records") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    for round in 0..3 {
        for i in 0..2_000 {
            write(&d.join(format!("churn-{i}.tmp")), 2_000);
        }
        for i in 0..2_000 {
            if i % 3 != round {
                fs::remove_file(d.join(format!("churn-{i}.tmp"))).unwrap();
            }
        }
    }
    compare_with_win32(d);
}
