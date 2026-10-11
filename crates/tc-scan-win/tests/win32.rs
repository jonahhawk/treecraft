#![cfg(windows)]

use std::fs;
use std::path::Path;

use tc_model::{NodeFlags, NodeId, Tree};
use tc_scan::{PortableLister, ScanOptions, scan};
use tc_scan_win::Win32Lister;

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("sub/日本語 😀")).unwrap();
    fs::create_dir_all(root.join("sub/empty")).unwrap();
    for (p, size) in [
        ("a.txt", 100),
        ("b.bin", 2000),
        ("sub/c", 30),
        ("sub/日本語 😀/d", 4),
        ("big", 5_000_000),
    ] {
        fs::File::create(root.join(p))
            .unwrap()
            .set_len(size)
            .unwrap();
    }
}

fn child(tree: &Tree, parent: NodeId, name: &str) -> NodeId {
    tree.children(parent)
        .find(|&c| tree.name(c) == name)
        .unwrap_or_else(|| panic!("no child {name}"))
}

/// Sorted (relative path, size, is_dir) for every node, to compare scanners.
fn listing(tree: &Tree) -> Vec<(String, u64, bool)> {
    let root = tree.path(tree.root());
    let mut out = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let rel = tree
            .path(id)
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        out.push((rel, tree.node(id).size(), tree.node(id).is_dir()));
        stack.extend(tree.children(id));
    }
    out.sort();
    out
}

#[test]
fn matches_portable_scanner() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let (win, wstats) = scan(dir.path(), &Win32Lister::new(), &ScanOptions::default()).unwrap();
    let (port, _) = scan(dir.path(), &PortableLister::new(), &ScanOptions::default()).unwrap();
    assert_eq!(listing(&win), listing(&port));
    assert_eq!(win.node(win.root()).size(), 5_002_134);
    assert_eq!(wstats.errors, 0);
    // Allocated size is real on-disk usage: at least the logical size for a plain file.
    let big = child(&win, win.root(), "big");
    assert!(win.node(big).allocated() >= 5_000_000);
    // Modification times are filled in.
    assert!(win.node(big).mtime() > 1_600_000_000);
}

#[test]
fn junctions_are_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(dir.path().join("junction"))
        .arg(dir.path().join("sub"))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink failed: {out:?}");
    let (tree, _) = scan(dir.path(), &Win32Lister::new(), &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(tree.root()).size(), 5_002_134);
    let j = child(&tree, tree.root(), "junction");
    assert!(tree.node(j).flags().contains(NodeFlags::LINK));
    assert!(!tree.node(j).has_children());
}

#[test]
fn long_paths() {
    let dir = tempfile::tempdir().unwrap();
    // Build a path well over 260 characters through the verbatim prefix.
    let mut deep = std::path::absolute(dir.path()).unwrap();
    for i in 0..12 {
        deep.push(format!("{i:02}_{}", "x".repeat(30)));
    }
    let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", deep.display()));
    fs::create_dir_all(&verbatim).unwrap();
    fs::File::create(verbatim.join("deep.bin"))
        .unwrap()
        .set_len(777)
        .unwrap();
    assert!(deep.as_os_str().len() > 260);

    let (tree, stats) = scan(dir.path(), &Win32Lister::new(), &ScanOptions::default()).unwrap();
    assert_eq!(stats.errors, 0);
    assert_eq!(tree.node(tree.root()).size(), 777);
    assert_eq!(tree.node(tree.root()).dirs(), 12);
}

#[test]
fn unreadable_directory_is_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let lister = Win32Lister::new();
    let mut out = Vec::new();
    let err = tc_scan::DirLister::list(&lister, &dir.path().join("missing"), &mut out);
    assert!(err.is_err());
    assert!(out.is_empty());
}
