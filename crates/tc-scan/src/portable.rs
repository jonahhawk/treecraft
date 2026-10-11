//! Directory lister built on `std::fs`, available on every platform.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use tc_model::{Entry, NodeFlags};

use crate::DirLister;

/// Lists directories with `std::fs::read_dir`.
///
/// On Windows the entry metadata comes from the directory listing itself, so no file is opened.
/// On Unix each entry costs one `lstat`. Hard links are counted once on Unix; on Windows link
/// counts are not available from a listing, so hard links are counted under every name.
#[derive(Debug, Default)]
pub struct PortableLister {
    /// (device, inode) of multiply-linked files already seen.
    #[cfg_attr(not(unix), allow(dead_code))]
    seen_links: Mutex<HashSet<(u64, u64)>>,
}

impl PortableLister {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DirLister for PortableLister {
    fn name(&self) -> &'static str {
        "portable"
    }

    fn list(&self, dir: &Path, out: &mut Vec<Entry>) -> io::Result<()> {
        let mut first_err = None;
        for de in fs::read_dir(dir)? {
            let de = match de {
                Ok(de) => de,
                Err(e) => {
                    first_err.get_or_insert(e);
                    continue;
                }
            };
            match de.metadata() {
                Ok(md) => out.push(self.entry(de.file_name(), &md)),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

impl PortableLister {
    fn entry(&self, name: std::ffi::OsString, md: &fs::Metadata) -> Entry {
        let ft = md.file_type();
        let mut flags = NodeFlags::empty();
        if ft.is_dir() {
            flags.insert(NodeFlags::DIR);
        }
        if ft.is_symlink() {
            flags.insert(NodeFlags::LINK);
        }
        let size = if ft.is_file() { md.len() } else { 0 };
        let mut entry = Entry {
            name,
            size,
            allocated: size,
            mtime: md.modified().map(unix_secs).unwrap_or(0),
            flags,
        };
        self.platform_details(md, &mut entry);
        entry
    }

    #[cfg(unix)]
    fn platform_details(&self, md: &fs::Metadata, e: &mut Entry) {
        use std::os::unix::fs::MetadataExt;
        if md.file_type().is_file() {
            e.allocated = md.blocks() * 512;
            if e.allocated < e.size {
                e.flags.insert(NodeFlags::SPARSE);
            }
            if md.nlink() > 1 {
                let mut seen = self.seen_links.lock().unwrap_or_else(|e| e.into_inner());
                if !seen.insert((md.dev(), md.ino())) {
                    e.flags.insert(NodeFlags::HARDLINK_DUP);
                    e.size = 0;
                    e.allocated = 0;
                }
            }
        }
        if e.name.as_encoded_bytes().first() == Some(&b'.') {
            e.flags.insert(NodeFlags::HIDDEN);
        }
    }

    #[cfg(windows)]
    fn platform_details(&self, md: &fs::Metadata, e: &mut Entry) {
        use std::os::windows::fs::MetadataExt;
        const DIRECTORY: u32 = 0x10;
        let attrs = md.file_attributes();
        e.flags.insert(crate::windows_attribute_flags(attrs));
        // std reports directory symlinks and junctions as non-directories; they are still
        // folders, just not followed.
        if attrs & DIRECTORY != 0 {
            e.flags.insert(NodeFlags::DIR);
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn platform_details(&self, _md: &fs::Metadata, _e: &mut Entry) {}
}

/// Maps Windows `FILE_ATTRIBUTE_*` bits to node flags. Shared with the Win32 lister.
pub fn windows_attribute_flags(attrs: u32) -> NodeFlags {
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const SPARSE: u32 = 0x200;
    const COMPRESSED: u32 = 0x800;
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    let mut f = NodeFlags::empty();
    if attrs & (HIDDEN | SYSTEM) != 0 {
        f.insert(NodeFlags::HIDDEN);
    }
    if attrs & SPARSE != 0 {
        f.insert(NodeFlags::SPARSE);
    }
    if attrs & COMPRESSED != 0 {
        f.insert(NodeFlags::COMPRESSED);
    }
    if attrs & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0 {
        f.insert(NodeFlags::CLOUD);
    }
    f
}

/// Seconds since the Unix epoch (negative before 1970).
pub fn unix_secs(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}
