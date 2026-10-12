//! Win32 directory lister using `GetFileInformationByHandleEx` directory classes.

use std::collections::HashSet;
use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tc_model::{Entry, NodeFlags};
use tc_scan::{DirLister, windows_attribute_flags};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_ID_EXTD_DIR_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, GetDiskFreeSpaceExW,
    GetFileInformationByHandleEx, GetLogicalDrives, GetVolumeInformationW, GetVolumePathNameW,
    OPEN_EXISTING,
};

use crate::Drive;

/// Lists directories with one `GetFileInformationByHandleEx` call per 64 KiB of entries.
///
/// Sizes, allocation sizes, attributes, times, reparse tags and file IDs all come from the
/// directory listing, so files are never opened (cloud placeholders are never downloaded).
///
/// Hard links: every name of a hard-linked file has the same file ID, so on NTFS the lister
/// remembers the IDs it has seen during a scan and counts each file once. Other names are
/// flagged [`NodeFlags::HARDLINK_DUP`] with size 0. On other file systems, where IDs may not be
/// unique or stable (FAT, network shares, ReFS's 128-bit IDs), no deduplication is done.
#[derive(Debug)]
pub struct Win32Lister {
    /// Whether to deduplicate hard links by file ID (set by `begin` for NTFS volumes).
    dedup: AtomicBool,
    /// File IDs seen in this scan, sharded to reduce lock contention between threads.
    seen: [Mutex<HashSet<u64>>; SHARDS],
    /// Which directory information class works on this volume (see `Format`).
    format: AtomicU8,
}

const SHARDS: usize = 16;
const BUF_BYTES: usize = 64 * 1024;
/// Reparse tags with this bit point somewhere else (symlinks, junctions); they are not followed.
const NAME_SURROGATE_BIT: u32 = 0x2000_0000;
/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET: i64 = 11_644_473_600;

/// Directory information classes, best first. `FileIdExtdDirectoryInfo` skips 8.3 short names
/// and reports the reparse tag directly; `FileIdBothDirectoryInfo` is supported more widely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Format {
    Extd = 0,
    Both = 1,
}

impl Default for Win32Lister {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32Lister {
    pub fn new() -> Self {
        Self {
            dedup: AtomicBool::new(false),
            seen: std::array::from_fn(|_| Mutex::new(HashSet::new())),
            format: AtomicU8::new(Format::Extd as u8),
        }
    }

    /// Forces the older `FileIdBothDirectoryInfo` class (for benchmarks and tests).
    pub fn with_both_dir_info() -> Self {
        let l = Self::new();
        l.format.store(Format::Both as u8, Ordering::Relaxed);
        l
    }

    /// Whether hard links are being deduplicated in the current scan.
    pub fn dedups_hard_links(&self) -> bool {
        self.dedup.load(Ordering::Relaxed)
    }

    fn format(&self) -> Format {
        if self.format.load(Ordering::Relaxed) == Format::Both as u8 {
            Format::Both
        } else {
            Format::Extd
        }
    }

    /// Returns true if this is the first name seen for `file_id` in the current scan.
    fn first_sighting(&self, file_id: u64) -> bool {
        let shard = &self.seen[(file_id % SHARDS as u64) as usize];
        shard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file_id)
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful CreateFileW and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// An entry decoded from a listing, before hard-link handling.
struct Raw {
    entry: Entry,
    file_id: u64,
}

impl DirLister for Win32Lister {
    fn name(&self) -> &'static str {
        "win32"
    }

    fn begin(&self, root: &Path) {
        for shard in &self.seen {
            shard.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
        let ntfs = file_system_name(root).is_some_and(|fs| fs.eq_ignore_ascii_case("NTFS"));
        self.dedup.store(ntfs, Ordering::Relaxed);
    }

    fn list(&self, dir: &Path, out: &mut Vec<Entry>) -> io::Result<()> {
        let wide = to_wide(&verbatim(dir));
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call; all other
        // arguments are plain flags or null.
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let h = Handle(h);

        let mut raw = Vec::new();
        let result = match self.read_listing(&h, self.format(), &mut raw) {
            Err(e) if self.format() == Format::Extd && raw.is_empty() && is_unsupported(&e) => {
                // This volume does not support the extended class; use the older one from now on.
                self.format.store(Format::Both as u8, Ordering::Relaxed);
                self.read_listing(&h, Format::Both, &mut raw)
            }
            r => r,
        };

        let dedup = self.dedups_hard_links();
        out.reserve(raw.len());
        for Raw { mut entry, file_id } in raw {
            let is_file =
                !entry.flags.contains(NodeFlags::DIR) && !entry.flags.contains(NodeFlags::LINK);
            if dedup && is_file && file_id != 0 && !self.first_sighting(file_id) {
                entry.flags.insert(NodeFlags::HARDLINK_DUP);
                entry.size = 0;
                entry.allocated = 0;
            }
            out.push(entry);
        }
        result
    }
}

impl Win32Lister {
    fn read_listing(&self, h: &Handle, format: Format, out: &mut Vec<Raw>) -> io::Result<()> {
        let (restart, next): (FILE_INFO_BY_HANDLE_CLASS, FILE_INFO_BY_HANDLE_CLASS) = match format {
            Format::Extd => (FileIdExtdDirectoryRestartInfo, FileIdExtdDirectoryInfo),
            Format::Both => (FileIdBothDirectoryRestartInfo, FileIdBothDirectoryInfo),
        };
        // u64 storage keeps the buffer 8-byte aligned, as the info structs require.
        let mut buf = vec![0u64; BUF_BYTES / 8];
        let mut class = restart;
        loop {
            // SAFETY: `buf` is a writable buffer of BUF_BYTES bytes owned by this function.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    h.0,
                    class,
                    buf.as_mut_ptr().cast::<c_void>(),
                    BUF_BYTES as u32,
                )
            };
            if ok == 0 {
                // SAFETY: trivially safe; reads the calling thread's last-error value.
                let code = unsafe { GetLastError() };
                if code == ERROR_NO_MORE_FILES {
                    return Ok(());
                }
                return Err(io::Error::from_raw_os_error(code as i32));
            }
            class = next;
            match format {
                Format::Extd => parse_buffer::<FILE_ID_EXTD_DIR_INFO>(&buf, out),
                Format::Both => parse_buffer::<FILE_ID_BOTH_DIR_INFO>(&buf, out),
            }
        }
    }
}

fn is_unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED)
    )
}

/// The fields we need from either directory information struct.
trait DirInfo: Copy {
    const NAME_OFFSET: usize;
    fn next_entry_offset(&self) -> u32;
    fn name_bytes(&self) -> usize;
    fn attributes(&self) -> u32;
    fn reparse_tag(&self) -> u32;
    fn end_of_file(&self) -> i64;
    fn allocation_size(&self) -> i64;
    fn last_write_time(&self) -> i64;
    fn file_id(&self) -> u64;
}

impl DirInfo for FILE_ID_BOTH_DIR_INFO {
    const NAME_OFFSET: usize = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    fn next_entry_offset(&self) -> u32 {
        self.NextEntryOffset
    }
    fn name_bytes(&self) -> usize {
        self.FileNameLength as usize
    }
    fn attributes(&self) -> u32 {
        self.FileAttributes
    }
    fn reparse_tag(&self) -> u32 {
        // For reparse points, EaSize holds the reparse tag.
        self.EaSize
    }
    fn end_of_file(&self) -> i64 {
        self.EndOfFile
    }
    fn allocation_size(&self) -> i64 {
        self.AllocationSize
    }
    fn last_write_time(&self) -> i64 {
        self.LastWriteTime
    }
    fn file_id(&self) -> u64 {
        self.FileId as u64
    }
}

impl DirInfo for FILE_ID_EXTD_DIR_INFO {
    const NAME_OFFSET: usize = std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    fn next_entry_offset(&self) -> u32 {
        self.NextEntryOffset
    }
    fn name_bytes(&self) -> usize {
        self.FileNameLength as usize
    }
    fn attributes(&self) -> u32 {
        self.FileAttributes
    }
    fn reparse_tag(&self) -> u32 {
        self.ReparsePointTag
    }
    fn end_of_file(&self) -> i64 {
        self.EndOfFile
    }
    fn allocation_size(&self) -> i64 {
        self.AllocationSize
    }
    fn last_write_time(&self) -> i64 {
        self.LastWriteTime
    }
    fn file_id(&self) -> u64 {
        // NTFS IDs fit in the low 64 bits; dedup is only enabled on NTFS.
        let mut low = [0u8; 8];
        low.copy_from_slice(&self.FileId.Identifier[..8]);
        u64::from_le_bytes(low)
    }
}

/// Decodes the chain of directory information records in `buf`.
fn parse_buffer<T: DirInfo>(buf: &[u64], out: &mut Vec<Raw>) {
    let base = buf.as_ptr().cast::<u8>();
    let len = buf.len() * 8;
    let mut offset = 0usize;
    loop {
        if offset + T::NAME_OFFSET > len {
            return;
        }
        // SAFETY: the record header lies within `buf` (checked above). The system writes
        // 8-byte-aligned records, but read_unaligned avoids relying on that.
        let info = unsafe { std::ptr::read_unaligned(base.add(offset).cast::<T>()) };
        let name_units = info.name_bytes() / 2;
        if offset + T::NAME_OFFSET + name_units * 2 > len {
            return;
        }
        let mut name = vec![0u16; name_units];
        // SAFETY: the name lies within `buf` (checked above); copying bytes avoids alignment
        // assumptions.
        unsafe {
            std::ptr::copy_nonoverlapping(
                base.add(offset + T::NAME_OFFSET),
                name.as_mut_ptr().cast::<u8>(),
                name_units * 2,
            );
        }
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            out.push(Raw {
                entry: entry(&info, OsString::from_wide(&name)),
                file_id: info.file_id(),
            });
        }
        if info.next_entry_offset() == 0 {
            return;
        }
        offset += info.next_entry_offset() as usize;
    }
}

fn entry<T: DirInfo>(info: &T, name: OsString) -> Entry {
    let attrs = info.attributes();
    let mut flags = windows_attribute_flags(attrs);
    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_dir {
        flags.insert(NodeFlags::DIR);
    }
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && info.reparse_tag() & NAME_SURROGATE_BIT != 0 {
        flags.insert(NodeFlags::LINK);
    }
    let (size, allocated) = if is_dir || flags.contains(NodeFlags::LINK) {
        (0, 0)
    } else {
        (
            info.end_of_file().max(0) as u64,
            info.allocation_size().max(0) as u64,
        )
    };
    Entry {
        name,
        size,
        allocated,
        mtime: filetime_to_unix(info.last_write_time()),
        flags,
    }
}

/// The file system name (e.g. "NTFS") of the volume containing `path`.
fn file_system_name(path: &Path) -> Option<String> {
    let wide = to_wide(path);
    let mut volume = [0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `volume` is a writable buffer of the given length.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0 {
        return None;
    }
    let mut fs = [0u16; 64];
    // SAFETY: `volume` is NUL-terminated by GetVolumePathNameW; `fs` is a writable buffer of the
    // given length; other outputs are null, which the API allows.
    let ok = unsafe {
        GetVolumeInformationW(
            volume.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let n = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Some(String::from_utf16_lossy(&fs[..n]))
}

fn filetime_to_unix(ft: i64) -> i64 {
    if ft <= 0 {
        0
    } else {
        ft / 10_000_000 - FILETIME_UNIX_OFFSET
    }
}

/// Converts an absolute path to the `\\?\` form, which lifts the 260-character limit.
fn verbatim(path: &Path) -> PathBuf {
    let s = path.as_os_str().encode_wide().collect::<Vec<u16>>();
    let starts =
        |p: &str| s.len() >= p.len() && s.iter().zip(p.encode_utf16()).all(|(a, b)| *a == b);
    if starts(r"\\?\") || starts(r"\\.\") {
        return path.to_path_buf();
    }
    let mut out: Vec<u16> = if starts(r"\\") {
        let mut v: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        v.extend_from_slice(&s[2..]);
        v
    } else {
        let mut v: Vec<u16> = r"\\?\".encode_utf16().collect();
        v.extend_from_slice(&s);
        v
    };
    // `\\?\` paths are not normalised by Windows, so forward slashes must be converted here.
    for c in &mut out {
        if *c == b'/' as u16 {
            *c = b'\\' as u16;
        }
    }
    PathBuf::from(OsString::from_wide(&out))
}

fn to_wide(s: &Path) -> Vec<u16> {
    s.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Lists drive letters with their label and free space.
pub(crate) fn drives() -> Vec<Drive> {
    // SAFETY: trivially safe; returns a bitmask of drive letters.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let root = format!("{}:\\", (b'A' + i) as char);
            let wide = to_wide(Path::new(&root));
            let (mut free, mut total) = (0u64, 0u64);
            // SAFETY: `wide` is NUL-terminated; the out pointers are valid u64s.
            let have_space = unsafe {
                GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
            } != 0;
            let mut label = [0u16; 261];
            // SAFETY: `wide` is NUL-terminated; `label` is a writable buffer of the given length;
            // other outputs are null, which the API allows.
            let have_label = unsafe {
                GetVolumeInformationW(
                    wide.as_ptr(),
                    label.as_mut_ptr(),
                    label.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                )
            } != 0;
            let label = if have_label {
                let n = label.iter().position(|&c| c == 0).unwrap_or(label.len());
                OsString::from_wide(&label[..n])
                    .to_string_lossy()
                    .into_owned()
            } else {
                String::new()
            };
            Drive {
                root: PathBuf::from(root),
                label,
                total: have_space.then_some(total),
                free: have_space.then_some(free),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_paths() {
        assert_eq!(verbatim(Path::new(r"C:\a\b")), Path::new(r"\\?\C:\a\b"));
        assert_eq!(verbatim(Path::new(r"C:/a/b")), Path::new(r"\\?\C:\a\b"));
        assert_eq!(
            verbatim(Path::new(r"\\server\share\x")),
            Path::new(r"\\?\UNC\server\share\x")
        );
        assert_eq!(verbatim(Path::new(r"\\?\D:\x")), Path::new(r"\\?\D:\x"));
    }

    #[test]
    fn filetime_conversion() {
        // 2000-01-01T00:00:00Z
        assert_eq!(filetime_to_unix(125_911_584_000_000_000), 946_684_800);
        assert_eq!(filetime_to_unix(0), 0);
    }

    #[test]
    fn lists_system_drive() {
        let d = drives();
        assert!(d.iter().any(|d| d.total.is_some()), "{d:?}");
    }
}
