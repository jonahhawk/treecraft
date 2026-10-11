use std::fs;
use std::path::Path;
use std::sync::Arc;

use tc_model::{NodeFlags, SortKey, Tree};
use tc_scan::{PortableLister, ScanOptions, SharedTree, new_tree, scan, start_scan};

/// Builds: a.txt(100), b.bin(2000), sub/c(30), sub/日本語 😀/d(4), sub/empty/
fn fixture(root: &Path) {
    fs::create_dir_all(root.join("sub/日本語 😀")).unwrap();
    fs::create_dir_all(root.join("sub/empty")).unwrap();
    for (p, size) in [
        ("a.txt", 100),
        ("b.bin", 2000),
        ("sub/c", 30),
        ("sub/日本語 😀/d", 4),
    ] {
        fs::File::create(root.join(p))
            .unwrap()
            .set_len(size)
            .unwrap();
    }
}

fn child(tree: &Tree, parent: tc_model::NodeId, name: &str) -> tc_model::NodeId {
    tree.children(parent)
        .find(|&c| tree.name(c) == name)
        .unwrap_or_else(|| panic!("no child {name}"))
}

#[test]
fn totals_match_fixture() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let (tree, stats) = scan(dir.path(), &PortableLister::new(), &ScanOptions::default()).unwrap();
    let root = tree.node(tree.root());
    assert_eq!(root.size(), 2134);
    assert_eq!(root.files(), 4);
    assert_eq!(root.dirs(), 3);
    assert_eq!(stats.entries, 7);
    assert_eq!(stats.dirs, 4);
    assert_eq!(stats.errors, 0);
    assert!(!stats.cancelled);

    let sub = child(&tree, tree.root(), "sub");
    assert_eq!(tree.node(sub).size(), 34);
    let uni = child(&tree, sub, "日本語 😀");
    assert_eq!(tree.node(uni).size(), 4);
    assert_eq!(
        tree.path(uni),
        std::path::absolute(dir.path())
            .unwrap()
            .join("sub")
            .join("日本語 😀")
    );
    let order: Vec<_> = tree
        .sorted_children(tree.root(), SortKey::Size, false)
        .into_iter()
        .map(|c| tree.name(c).to_owned())
        .collect();
    assert_eq!(order, ["b.bin", "a.txt", "sub"]);
}

#[test]
fn background_scan_matches_blocking_scan() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let shared = SharedTree::new(new_tree(dir.path()).unwrap());
    let handle = start_scan(
        shared.clone(),
        Arc::new(PortableLister::new()),
        ScanOptions { threads: 2 },
    );
    let stats = handle.join();
    assert_eq!(stats.entries, 7);
    assert_eq!(shared.read().node(shared.read().root()).size(), 2134);
}

#[test]
fn cancelled_scan_stops() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let shared = SharedTree::new(new_tree(dir.path()).unwrap());
    let handle = start_scan(
        shared.clone(),
        Arc::new(PortableLister::new()),
        ScanOptions::default(),
    );
    handle.cancel();
    let stats = handle.join();
    assert!(stats.cancelled);
    assert!(stats.entries <= 7);
}

#[test]
fn missing_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(new_tree(&dir.path().join("nope")).is_err());
    let file = dir.path().join("f");
    fs::write(&file, b"x").unwrap();
    assert!(new_tree(&file).is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_and_hard_links() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    std::os::unix::fs::symlink(dir.path().join("sub"), dir.path().join("link")).unwrap();
    fs::hard_link(dir.path().join("b.bin"), dir.path().join("b2.bin")).unwrap();
    let (tree, _) = scan(dir.path(), &PortableLister::new(), &ScanOptions::default()).unwrap();
    // The symlink is not followed and the second hard link adds nothing.
    assert_eq!(tree.node(tree.root()).size(), 2134);
    let link = child(&tree, tree.root(), "link");
    assert!(tree.node(link).flags().contains(NodeFlags::LINK));
    let dup = [
        child(&tree, tree.root(), "b.bin"),
        child(&tree, tree.root(), "b2.bin"),
    ]
    .into_iter()
    .filter(|&c| tree.node(c).flags().contains(NodeFlags::HARDLINK_DUP))
    .count();
    assert_eq!(dup, 1);
}

#[cfg(windows)]
#[test]
fn junctions_are_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(dir.path().join("junction"))
        .arg(dir.path().join("sub"))
        .output()
        .unwrap();
    assert!(status.status.success(), "mklink failed: {status:?}");
    let (tree, _) = scan(dir.path(), &PortableLister::new(), &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(tree.root()).size(), 2134);
    let j = child(&tree, tree.root(), "junction");
    assert!(tree.node(j).flags().contains(NodeFlags::LINK));
    assert!(!tree.node(j).has_children());
}
