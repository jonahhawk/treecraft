//! Windows edge-case suite (PLAN.md §4, spec/features.md §1.3).
//!
//! Each test builds its case in a temporary folder on the system drive (NTFS) and scans it with
//! the Win32 scanner, and with the portable scanner where the two should agree. Cases that need
//! a privilege this account may not have (creating symlinks) print a message and pass.
//!
//! Cases that only matter to the MFT scanner (fragmented $MFT, $ATTRIBUTE_LIST overflow) belong
//! to milestone M5 and need an elevated test runner.

#![cfg(windows)]

use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::Command;

use tc_model::{NodeFlags, NodeId, Tree};
use tc_scan::{DirLister, PortableLister, ScanOptions, ScanStats, scan};
use tc_scan_win::Win32Lister;

fn win32_scan(root: &Path) -> (Tree, ScanStats) {
    scan(root, &Win32Lister::new(), &ScanOptions::default()).unwrap()
}

fn portable_scan(root: &Path) -> (Tree, ScanStats) {
    scan(root, &PortableLister::new(), &ScanOptions::default()).unwrap()
}

fn child(tree: &Tree, parent: NodeId, name: &str) -> NodeId {
    tree.children(parent)
        .find(|&c| tree.name(c) == name)
        .unwrap_or_else(|| panic!("no child {name}"))
}

fn write(path: &Path, len: u64) {
    fs::File::create(path).unwrap().set_len(len).unwrap();
}

fn mklink_junction(link: &Path, target: &Path) {
    // cmd would read "/b" in "a/b" as a switch, so pass backslash paths.
    let backslashes = |p: &Path| p.to_string_lossy().replace('/', "\\");
    let out = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(backslashes(link))
        .arg(backslashes(target))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink /J failed: {out:?}");
}

/// Sends a filesystem control code with an optional input buffer.
fn fsctl(file: &fs::File, code: u32, input: &[u8]) {
    let mut returned = 0u32;
    // SAFETY: the handle is valid for the lifetime of `file`; the input buffer is valid for
    // `input.len()` bytes; no output buffer or overlapped structure is used.
    let ok = unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            file.as_raw_handle(),
            code,
            input.as_ptr().cast(),
            input.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    assert!(
        ok != 0,
        "DeviceIoControl({code:#x}) failed: {}",
        std::io::Error::last_os_error()
    );
}

#[test]
fn hard_links_count_once() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("original.bin"), 50_000);
    fs::create_dir(dir.path().join("sub")).unwrap();
    fs::hard_link(
        dir.path().join("original.bin"),
        dir.path().join("link1.bin"),
    )
    .unwrap();
    fs::hard_link(
        dir.path().join("original.bin"),
        dir.path().join("sub/link2.bin"),
    )
    .unwrap();
    write(&dir.path().join("plain.bin"), 7);

    let lister = Win32Lister::new();
    let (tree, _) = scan(dir.path(), &lister, &ScanOptions::default()).unwrap();
    assert!(lister.dedups_hard_links(), "temp folder should be on NTFS");
    let root = tree.node(tree.root());
    assert_eq!(root.size(), 50_007, "the 3 names of one file count once");
    assert_eq!(root.files(), 4, "every name is still listed");
    let sub = child(&tree, tree.root(), "sub");
    let names = [
        child(&tree, tree.root(), "original.bin"),
        child(&tree, tree.root(), "link1.bin"),
        child(&tree, sub, "link2.bin"),
    ];
    let dups = names
        .iter()
        .filter(|&&n| tree.node(n).flags().contains(NodeFlags::HARDLINK_DUP))
        .count();
    assert_eq!(dups, 2);
    let counted: Vec<_> = names
        .iter()
        .filter(|&&n| tree.node(n).size() == 50_000)
        .collect();
    assert_eq!(counted.len(), 1);
}

#[test]
fn hard_link_state_resets_between_scans() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a"), 10);
    fs::hard_link(dir.path().join("a"), dir.path().join("b")).unwrap();
    let lister = Win32Lister::new();
    for _ in 0..2 {
        let (tree, _) = scan(dir.path(), &lister, &ScanOptions::default()).unwrap();
        assert_eq!(tree.node(tree.root()).size(), 10);
    }
}

#[test]
fn junction_loops_terminate() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("a/b")).unwrap();
    write(&dir.path().join("a/b/f"), 123);
    // a/b/up -> a: following it would loop forever.
    mklink_junction(&dir.path().join("a/b/up"), &dir.path().join("a"));
    for (tree, stats) in [win32_scan(dir.path()), portable_scan(dir.path())] {
        assert_eq!(tree.node(tree.root()).size(), 123);
        assert_eq!(stats.errors, 0);
        let b = child(&tree, child(&tree, tree.root(), "a"), "b");
        let up = child(&tree, b, "up");
        assert!(tree.node(up).flags().contains(NodeFlags::LINK));
        assert!(tree.node(up).is_dir());
        assert!(!tree.node(up).has_children());
    }
}

#[test]
fn symlinks_outside_root_are_not_followed() {
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("big"), 1_000_000);
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("small"), 5);
    let dir_link = std::os::windows::fs::symlink_dir(outside.path(), dir.path().join("out_dir"));
    let file_link =
        std::os::windows::fs::symlink_file(outside.path().join("big"), dir.path().join("out_file"));
    if let Err(e) = dir_link.and(file_link) {
        // Needs Developer Mode or SeCreateSymbolicLinkPrivilege; junctions cover the same path.
        eprintln!("skipping symlink case: {e}");
        return;
    }
    for (tree, _) in [win32_scan(dir.path()), portable_scan(dir.path())] {
        assert_eq!(tree.node(tree.root()).size(), 5);
        for name in ["out_dir", "out_file"] {
            let n = child(&tree, tree.root(), name);
            assert!(tree.node(n).flags().contains(NodeFlags::LINK), "{name}");
            assert_eq!(tree.node(n).size(), 0, "{name}");
        }
    }
}

#[test]
fn alternate_data_streams_do_not_break_listing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f.txt"), vec![b'x'; 100]).unwrap();
    // Writing "name:stream" creates an alternate data stream on NTFS.
    fs::write(dir.path().join("f.txt:extra"), vec![b'y'; 10_000]).unwrap();
    let (tree, stats) = win32_scan(dir.path());
    assert_eq!(stats.errors, 0);
    assert_eq!(
        tree.node(tree.root()).files(),
        1,
        "streams are not separate files"
    );
    // Directory listings report the main stream only. Counting ADS is P1 (spec §1.3).
    assert_eq!(tree.node(tree.root()).size(), 100);
}

#[test]
fn sparse_files_report_small_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse.bin");
    {
        let f = fs::File::create(&path).unwrap();
        fsctl(&f, windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE, &[]);
        f.set_len(256 * 1024 * 1024).unwrap();
    }
    let (tree, _) = win32_scan(dir.path());
    let n = tree.node(child(&tree, tree.root(), "sparse.bin"));
    assert_eq!(n.size(), 256 * 1024 * 1024);
    assert!(n.allocated() < 1024 * 1024, "allocated {}", n.allocated());
    assert!(n.flags().contains(NodeFlags::SPARSE));
}

#[test]
fn compressed_files_report_small_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zeros.bin");
    {
        // FSCTL_SET_COMPRESSION needs both read and write access.
        let f = fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let format = windows_sys::Win32::Storage::FileSystem::COMPRESSION_FORMAT_DEFAULT;
        fsctl(
            &f,
            windows_sys::Win32::System::Ioctl::FSCTL_SET_COMPRESSION,
            &format.to_le_bytes(),
        );
        std::io::Write::write_all(&mut &f, &vec![0u8; 4 * 1024 * 1024]).unwrap();
        f.sync_all().unwrap();
    }
    let (tree, _) = win32_scan(dir.path());
    let n = tree.node(child(&tree, tree.root(), "zeros.bin"));
    assert_eq!(n.size(), 4 * 1024 * 1024);
    assert!(
        n.allocated() < n.size(),
        "allocated {} of {}",
        n.allocated(),
        n.size()
    );
    assert!(n.flags().contains(NodeFlags::COMPRESSED));
}

/// Restores access to a folder when dropped, so the temp dir can be deleted.
struct Unlock(std::path::PathBuf, String);

impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = Command::new("icacls")
            .arg(&self.0)
            .args(["/remove:d", &self.1])
            .output();
    }
}

#[test]
fn access_denied_folders_are_reported_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    write(&locked.join("hidden"), 999);
    write(&dir.path().join("visible"), 11);
    let user = std::env::var("USERNAME").unwrap();
    let out = Command::new("icacls")
        .arg(&locked)
        .args(["/deny", &format!("{user}:(RD)")])
        .output()
        .unwrap();
    assert!(out.status.success(), "icacls failed: {out:?}");
    let _unlock = Unlock(locked.clone(), user);
    // With the backup privilege enabled (as elevated Git Bash/Cygwin shells do), directory
    // opens ignore the ACL and nothing is denied.
    if fs::read_dir(&locked).is_ok() {
        eprintln!(
            "access_denied_folders_are_reported_and_skipped: skipped, the deny ACE is bypassed"
        );
        return;
    }

    for (tree, stats) in [win32_scan(dir.path()), portable_scan(dir.path())] {
        assert_eq!(stats.errors, 1);
        assert_eq!(tree.node(tree.root()).size(), 11);
        let l = child(&tree, tree.root(), "locked");
        assert!(tree.node(l).flags().contains(NodeFlags::ERROR));
    }
}

#[test]
fn unpaired_surrogate_names_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    // "a\u{D800}b": a lone high surrogate, legal in NTFS names but not valid Unicode.
    let name = OsString::from_wide(&[0x61, 0xD800, 0x62]);
    write(&dir.path().join(&name), 42);
    for (tree, _) in [win32_scan(dir.path()), portable_scan(dir.path())] {
        let id = tree.children(tree.root()).next().unwrap();
        assert_eq!(tree.name(id), name.as_os_str());
        assert_eq!(tree.node(id).size(), 42);
        // The rebuilt path opens the same file.
        assert_eq!(fs::metadata(tree.path(id)).unwrap().len(), 42);
    }
}

#[test]
fn zero_byte_files_and_empty_dirs() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("empty.txt"), 0);
    fs::create_dir(dir.path().join("nothing")).unwrap();
    for (tree, _) in [win32_scan(dir.path()), portable_scan(dir.path())] {
        let root = tree.node(tree.root());
        assert_eq!((root.size(), root.files(), root.dirs()), (0, 1, 1));
        assert_eq!(
            tree.node(child(&tree, tree.root(), "empty.txt"))
                .allocated(),
            0
        );
    }
}

#[test]
fn files_locked_by_other_processes_are_still_listed() {
    // Listing reads the directory, never the files, so an exclusively opened file is no
    // obstacle. This is also why cloud placeholders are not downloaded by a scan.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.db");
    fs::write(&path, vec![1u8; 4096]).unwrap();
    let _exclusive = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::File::options()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap()
    };
    let lister = Win32Lister::new();
    let mut out = Vec::new();
    lister.list(dir.path(), &mut out).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].size, 4096);
}

#[test]
fn both_directory_info_formats_agree() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("x/y")).unwrap();
    for (i, p) in ["a", "x/b", "x/y/c"].iter().enumerate() {
        write(&dir.path().join(p), 1000 * (i as u64 + 1));
    }
    mklink_junction(&dir.path().join("j"), &dir.path().join("x"));
    let (extd, _) = win32_scan(dir.path());
    let (both, _) = scan(
        dir.path(),
        &Win32Lister::with_both_dir_info(),
        &ScanOptions::default(),
    )
    .unwrap();
    for t in [&extd, &both] {
        assert_eq!(t.node(t.root()).size(), 6000);
        assert!(
            t.node(child(t, t.root(), "j"))
                .flags()
                .contains(NodeFlags::LINK)
        );
    }
}
