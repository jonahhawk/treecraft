//! Compares opening every directory by full path with opening it relative to its parent's
//! already-open handle (single-threaded, opens only).
//!
//! Usage: `cargo run --release -p tc-scan-win --example rel_open -- <root>`

#[cfg(not(windows))]
fn main() {
    eprintln!("rel_open measures Win32 directory handles and only runs on Windows");
}

#[cfg(windows)]
fn main() {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr::{null, null_mut};
    use std::time::Instant;

    use tc_model::{NodeFlags, NodeId, Tree};
    use tc_scan::{ScanOptions, scan};
    use windows_sys::Wdk::Foundation::{NtClose, OBJECT_ATTRIBUTES};
    use windows_sys::Wdk::Storage::FileSystem::NtOpenFile;
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    const FILE_DIRECTORY_FILE: u32 = 0x1;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_OPEN_FOR_BACKUP_INTENT: u32 = 0x4000;
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    let root = PathBuf::from(std::env::args().nth(1).expect("usage: rel_open <root>"));
    let (tree, _) = scan(
        &root,
        &tc_scan_win::Win32Lister::new(),
        &ScanOptions::default(),
    )
    .expect("scan failed");

    fn is_walkable(tree: &Tree, id: NodeId) -> bool {
        let n = tree.node(id);
        n.is_dir() && !n.flags().contains(NodeFlags::LINK)
    }

    let mut paths = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        if is_walkable(&tree, id) {
            let mut wide: Vec<u16> = r"\\?\".encode_utf16().collect();
            wide.extend(tree.path(id).as_os_str().encode_wide());
            wide.push(0);
            paths.push(wide);
            stack.extend(tree.children(id));
        }
    }
    println!("{} directories", paths.len());

    let full = |paths: &[Vec<u16>]| {
        let t = Instant::now();
        let mut failed = 0;
        for p in paths {
            // SAFETY: p is a NUL-terminated UTF-16 path; the handle is closed right away.
            unsafe {
                let h = CreateFileW(
                    p.as_ptr(),
                    FILE_LIST_DIRECTORY | SYNCHRONIZE,
                    share,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    null_mut(),
                );
                if h == INVALID_HANDLE_VALUE {
                    failed += 1;
                } else {
                    CloseHandle(h);
                }
            }
        }
        (t.elapsed(), failed)
    };

    fn walk(tree: &Tree, id: NodeId, parent: HANDLE, share: u32, failed: &mut usize) {
        for c in tree.children(id) {
            if !is_walkable(tree, c) {
                continue;
            }
            let name: Vec<u16> = tree.name(c).encode_wide().collect();
            let us = UNICODE_STRING {
                Length: (name.len() * 2) as u16,
                MaximumLength: (name.len() * 2) as u16,
                Buffer: name.as_ptr().cast_mut(),
            };
            let oa = OBJECT_ATTRIBUTES {
                Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
                RootDirectory: parent,
                ObjectName: &us,
                Attributes: 0,
                SecurityDescriptor: null(),
                SecurityQualityOfService: null(),
            };
            let mut h: HANDLE = null_mut();
            // SAFETY: all pointers refer to live locals; the handle is closed after the walk.
            unsafe {
                let mut iosb: IO_STATUS_BLOCK = std::mem::zeroed();
                let status = NtOpenFile(
                    &mut h,
                    FILE_LIST_DIRECTORY | SYNCHRONIZE,
                    &oa,
                    &mut iosb,
                    share,
                    FILE_DIRECTORY_FILE
                        | FILE_SYNCHRONOUS_IO_NONALERT
                        | FILE_OPEN_FOR_BACKUP_INTENT,
                );
                if status < 0 {
                    *failed += 1;
                    continue;
                }
                walk(tree, c, h, share, failed);
                NtClose(h);
            }
        }
    }

    let relative = || {
        let t = Instant::now();
        let mut failed = 0;
        // SAFETY: paths[0] is the root, NUL-terminated; the handle is closed after the walk.
        unsafe {
            let h = CreateFileW(
                paths[0].as_ptr(),
                FILE_LIST_DIRECTORY | SYNCHRONIZE,
                share,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            );
            assert_ne!(h, INVALID_HANDLE_VALUE);
            walk(&tree, tree.root(), h, share, &mut failed);
            CloseHandle(h);
        }
        (t.elapsed(), failed)
    };

    for round in 0..3 {
        let (f, ff) = full(&paths);
        let (r, rf) = relative();
        println!(
            "round {round}: full path {:.2}s ({ff} failed)   relative {:.2}s ({rf} failed)",
            f.as_secs_f64(),
            r.as_secs_f64()
        );
    }
}
