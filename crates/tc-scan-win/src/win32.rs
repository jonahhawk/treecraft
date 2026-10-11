//! Win32 directory lister using `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`.

use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use tc_model::{Entry, NodeFlags};
use tc_scan::{DirLister, windows_attribute_flags};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    GetDiskFreeSpaceExW, GetFileInformationByHandleEx, GetLogicalDrives, GetVolumeInformationW,
    OPEN_EXISTING,
};

use crate::Drive;

/// Lists directories with one `GetFileInformationByHandleEx` call per 64 KiB of entries.
///
/// Sizes, allocation sizes, attributes, times and reparse tags all come from the directory
/// listing, so files are never opened. Link counts are not part of the listing, so hard links are
/// counted under every name (the MFT scanner fixes this).
#[derive(Debug, Default)]
pub struct Win32Lister;

impl Win32Lister {
    pub fn new() -> Self {
        Self
    }
}

const BUF_BYTES: usize = 64 * 1024;
/// Reparse tags with this bit point somewhere else (symlinks, junctions); they are not followed.
const NAME_SURROGATE_BIT: u32 = 0x2000_0000;
/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_OFFSET: i64 = 11_644_473_600;

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful CreateFileW and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

impl DirLister for Win32Lister {
    fn name(&self) -> &'static str {
        "win32"
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

        // u64 storage keeps the buffer 8-byte aligned, as the info structs require.
        let mut buf = vec![0u64; BUF_BYTES / 8];
        let mut class = FileIdBothDirectoryRestartInfo;
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
            class = FileIdBothDirectoryInfo;
            parse_buffer(&buf, out);
        }
    }
}

/// Decodes the chain of `FILE_ID_BOTH_DIR_INFO` records in `buf`.
fn parse_buffer(buf: &[u64], out: &mut Vec<Entry>) {
    let base = buf.as_ptr().cast::<u8>();
    let len = buf.len() * 8;
    let name_field = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let mut offset = 0usize;
    loop {
        if offset + name_field > len {
            return;
        }
        // SAFETY: the record header lies within `buf` (checked above). The system writes
        // 8-byte-aligned records, but read_unaligned avoids relying on that.
        let info =
            unsafe { std::ptr::read_unaligned(base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>()) };
        let name_units = info.FileNameLength as usize / 2;
        if offset + name_field + name_units * 2 > len {
            return;
        }
        let mut name = vec![0u16; name_units];
        // SAFETY: the name lies within `buf` (checked above); copying bytes avoids alignment
        // assumptions.
        unsafe {
            std::ptr::copy_nonoverlapping(
                base.add(offset + name_field),
                name.as_mut_ptr().cast::<u8>(),
                name_units * 2,
            );
        }
        if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
            out.push(entry(&info, OsString::from_wide(&name)));
        }
        if info.NextEntryOffset == 0 {
            return;
        }
        offset += info.NextEntryOffset as usize;
    }
}

fn entry(info: &FILE_ID_BOTH_DIR_INFO, name: OsString) -> Entry {
    let attrs = info.FileAttributes;
    let mut flags = windows_attribute_flags(attrs);
    let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
    if is_dir {
        flags.insert(NodeFlags::DIR);
    }
    // For reparse points, EaSize holds the reparse tag.
    if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && info.EaSize & NAME_SURROGATE_BIT != 0 {
        flags.insert(NodeFlags::LINK);
    }
    let (size, allocated) = if is_dir || flags.contains(NodeFlags::LINK) {
        (0, 0)
    } else {
        (
            info.EndOfFile.max(0) as u64,
            info.AllocationSize.max(0) as u64,
        )
    };
    Entry {
        name,
        size,
        allocated,
        mtime: filetime_to_unix(info.LastWriteTime),
        flags,
    }
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
