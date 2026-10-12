//! Whole-volume scanner that reads the NTFS Master File Table directly.
//!
//! Instead of opening every directory, this opens the volume once, reads `$MFT` in large
//! sequential chunks, parses every FILE record with `tc-ntfs`, and rebuilds the tree from the
//! parent references in `$FILE_NAME`. It needs administrator rights (the volume can only be
//! opened for reading when elevated) and works on local NTFS volumes only.
//!
//! All fatal errors happen before anything is inserted into the tree, so a caller can fall back
//! to the Win32 lister on the same, still empty, tree.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;

use rayon::prelude::*;
use tc_model::{Entry, NodeFlags};
use tc_ntfs::{
    Body, FileName, FileRecord, FileRef, FileSummary, Run, apply_fixups, attr, decode_runlist,
    parse_attribute_list,
};
use tc_scan::{ScanProgress, SharedTree, windows_attribute_flags};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW,
    GetFileInformationByHandle, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::win32::{NAME_SURROGATE_BIT, file_system_name, filetime_to_unix, to_wide};

/// Bytes per read. A multiple of every cluster and record size NTFS allows.
const CHUNK: usize = 8 * 1024 * 1024;
/// Records 0-15 are NTFS metadata files (`$MFT`, `$LogFile`, `$Extend`, ...). Directory
/// listings don't show them, so neither does this scanner.
const FIRST_USER_RECORD: u64 = 16;
/// Directories inserted per write-lock hold, so a GUI can draw between batches.
const DIRS_PER_BATCH: usize = 20_000;
/// Reads in flight at once; NVMe drives need several to reach full speed.
const READERS: usize = 4;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;

/// Whether this process runs with administrator rights (an elevated token).
pub fn is_elevated() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle; `token` receives a handle we close.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `elevation` is a writable TOKEN_ELEVATION of the size passed; `token` is valid.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    // SAFETY: `token` came from OpenProcessToken and is closed once.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Why the MFT scanner can't scan `root`, or `None` if it can.
pub fn unavailable_reason(root: &Path) -> Option<String> {
    if !is_elevated() {
        return Some("reading the MFT needs administrator rights".into());
    }
    if !file_system_name(root).is_some_and(|fs| fs.eq_ignore_ascii_case("NTFS")) {
        return Some("the MFT scanner only reads NTFS volumes".into());
    }
    let Some(volume) = volume_path(root) else {
        return Some("cannot find the volume of this folder".into());
    };
    // SAFETY: `volume` is NUL-terminated.
    let kind = unsafe { GetDriveTypeW(volume.as_ptr()) };
    if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
        return Some("the MFT scanner only reads local drives".into());
    }
    None
}

/// The volume mount point containing `path` (e.g. `C:\`), NUL-terminated.
fn volume_path(path: &Path) -> Option<Vec<u16>> {
    let wide = to_wide(path);
    let mut volume = vec![0u16; 1024];
    // SAFETY: `wide` is NUL-terminated and `volume` is writable for its length.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if ok == 0 {
        return None;
    }
    let n = volume.iter().position(|&c| c == 0)?;
    volume.truncate(n + 1);
    Some(volume)
}

/// Opens the volume containing `root` for raw reading.
fn open_volume(root: &Path) -> io::Result<File> {
    let mount = volume_path(root).ok_or_else(|| io::Error::other("cannot find the volume"))?;
    let mut name = [0u16; 64];
    // SAFETY: `mount` is NUL-terminated (with a trailing backslash, as required) and `name` is
    // writable for its length.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // `\\?\Volume{GUID}\` names the root directory; without the backslash it is the device.
    let mut n = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    if n > 0 && name[n - 1] == u16::from(b'\\') {
        n -= 1;
    }
    let device = PathBuf::from(OsString::from_wide(&name[..n]));
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(device)
}

/// The record number of directory `dir` (from its file ID).
fn record_of(dir: &Path) -> io::Result<FileRef> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?;
    // SAFETY: zero is a valid bit pattern for this plain C struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is open for the duration of the call; `info` is writable.
    if unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileRef(
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    cluster: u64,
    record: usize,
    mft_start: u64,
    mft_valid: u64,
}

fn geometry(volume: &File) -> io::Result<Geometry> {
    // SAFETY: zero is a valid bit pattern for this plain C struct.
    let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
    let mut got = 0u32;
    // SAFETY: the volume handle is open; `data` is writable for the size passed.
    let ok = unsafe {
        DeviceIoControl(
            volume.as_raw_handle(),
            FSCTL_GET_NTFS_VOLUME_DATA,
            std::ptr::null(),
            0,
            (&raw mut data).cast(),
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut got,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let g = Geometry {
        cluster: u64::from(data.BytesPerCluster),
        record: data.BytesPerFileRecordSegment as usize,
        mft_start: data.MftStartLcn as u64,
        mft_valid: data.MftValidDataLength as u64,
    };
    let sane = g.cluster.is_power_of_two()
        && g.record.is_power_of_two()
        && (512..=65536).contains(&g.record)
        && CHUNK.is_multiple_of(g.record)
        && (CHUNK as u64).is_multiple_of(g.cluster);
    if !sane {
        return Err(io::Error::other(format!("unexpected NTFS geometry {g:?}")));
    }
    Ok(g)
}

fn bad_data(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Fills `buf` from byte offset `pos` of the volume.
fn read_exact_at(volume: &File, mut buf: &mut [u8], mut pos: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match volume.seek_read(buf, pos) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                pos += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Reads one record at byte offset `pos` of the volume and applies fixups.
fn read_record_at(volume: &File, pos: u64, size: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; size];
    read_exact_at(volume, &mut buf, pos)?;
    apply_fixups(&mut buf).map_err(|e| bad_data(format!("$MFT record: {e}")))?;
    Ok(buf)
}

/// Finds where `record` lives on disk, given the MFT's runs.
fn record_offset(runs: &[Run], g: Geometry, record: u64) -> Option<u64> {
    let mut vcn_byte = record.checked_mul(g.record as u64)?;
    for run in runs {
        let len = run.clusters.checked_mul(g.cluster)?;
        if vcn_byte < len {
            return Some(run.lcn?.checked_mul(g.cluster)? + vcn_byte);
        }
        vcn_byte -= len;
    }
    None
}

/// Where `$MFT` lives on disk, and which of its records are in use.
struct Layout {
    /// Runs of the MFT's data, in order.
    runs: Vec<Run>,
    /// Bytes of MFT to read.
    size: u64,
    /// One bit per record, set if the record is in use. `None` if `$MFT`'s `$BITMAP` could not
    /// be read, in which case every record is read.
    bitmap: Option<Vec<u8>>,
}

/// The layout of `$MFT`, following its attribute list if the MFT is very fragmented.
fn mft_layout(volume: &File, g: Geometry) -> io::Result<Layout> {
    let rec0 = read_record_at(volume, g.mft_start * g.cluster, g.record)?;
    let rec0 = FileRecord::parse(&rec0).map_err(|e| bad_data(format!("$MFT record: {e}")))?;
    let mut extents: Vec<(u64, Vec<Run>)> = Vec::new();
    let mut size = None;
    let mut list = None;
    let mut bitmap = None;
    for a in rec0.attributes() {
        let a = a.map_err(|e| bad_data(format!("$MFT attribute: {e}")))?;
        match (a.type_code, a.body) {
            (attr::BITMAP, body) if !a.is_named() => bitmap = read_bitmap(volume, g, body),
            (attr::DATA, Body::NonResident(n)) if !a.is_named() => {
                if n.start_vcn == 0 {
                    size = Some(n.data_size);
                }
                let runs =
                    decode_runlist(n.runlist).map_err(|e| bad_data(format!("$MFT runs: {e}")))?;
                extents.push((n.start_vcn, runs));
            }
            (attr::ATTRIBUTE_LIST, Body::Resident(v)) => {
                list =
                    Some(parse_attribute_list(v).map_err(|e| bad_data(format!("$MFT list: {e}")))?);
            }
            (attr::ATTRIBUTE_LIST, Body::NonResident(_)) => {
                return Err(bad_data("$MFT has a non-resident attribute list"));
            }
            _ => {}
        }
    }
    // Further extents of $MFT's data live in extension records, which are themselves in the
    // part of the MFT already described.
    for entry in list.unwrap_or_default() {
        if entry.type_code != attr::DATA || entry.start_vcn == 0 || entry.record.record() == 0 {
            continue;
        }
        let mut known: Vec<&(u64, Vec<Run>)> = extents.iter().collect();
        known.sort_by_key(|e| e.0);
        let runs: Vec<Run> = known.iter().flat_map(|e| e.1.iter().copied()).collect();
        let pos = record_offset(&runs, g, entry.record.record())
            .ok_or_else(|| bad_data("$MFT extension record is outside the known runs"))?;
        let buf = read_record_at(volume, pos, g.record)?;
        let rec = FileRecord::parse(&buf).map_err(|e| bad_data(format!("$MFT extension: {e}")))?;
        for a in rec.attributes() {
            let a = a.map_err(|e| bad_data(format!("$MFT extension attribute: {e}")))?;
            if let (attr::DATA, Body::NonResident(n)) = (a.type_code, a.body)
                && !a.is_named()
                && n.start_vcn == entry.start_vcn
            {
                let runs =
                    decode_runlist(n.runlist).map_err(|e| bad_data(format!("$MFT runs: {e}")))?;
                extents.push((n.start_vcn, runs));
            }
        }
    }
    extents.sort_by_key(|e| e.0);
    let runs = extents.into_iter().flat_map(|e| e.1).collect();
    let size = size.ok_or_else(|| bad_data("$MFT has no data attribute"))?;
    Ok(Layout {
        runs,
        size: size.min(g.mft_valid),
        bitmap,
    })
}

/// Reads `$MFT`'s `$BITMAP`, or `None` if it can't be read.
fn read_bitmap(volume: &File, g: Geometry, body: Body<'_>) -> Option<Vec<u8>> {
    match body {
        Body::Resident(v) => Some(v.to_vec()),
        // A bitmap continued in an extension record would need the attribute list; then all
        // records are read instead.
        Body::NonResident(n) if n.start_vcn == 0 => {
            let runs = decode_runlist(n.runlist).ok()?;
            let size = n.data_size.min(n.initialized_size);
            let covered: u64 = runs
                .iter()
                .map(|r| r.clusters.saturating_mul(g.cluster))
                .sum();
            if size > covered || size > 1 << 30 {
                return None;
            }
            let mut bytes = vec![0u8; usize::try_from(size).ok()?];
            read_stream_at(volume, &segments(&runs, g, size), &mut bytes, 0).ok()?;
            Some(bytes)
        }
        Body::NonResident(_) => None,
    }
}

/// Whether any record in `from..to` is in use. Records past the end of the bitmap count as in
/// use, so a short bitmap never hides records.
fn any_in_use(bitmap: &[u8], from: usize, to: usize) -> bool {
    (from..to).any(|r| bitmap.get(r / 8).is_none_or(|b| b & (1 << (r % 8)) != 0))
}

/// Byte ranges `(volume offset, length)` covering the first `total` bytes of a stream, in order.
/// Sparse runs (which the MFT never has in practice) come back with offset `None`.
fn segments(runs: &[Run], g: Geometry, total: u64) -> Vec<(Option<u64>, u64)> {
    let mut out = Vec::new();
    let mut left = total;
    for run in runs {
        if left == 0 {
            break;
        }
        let len = (run.clusters.saturating_mul(g.cluster)).min(left);
        out.push((run.lcn.map(|l| l * g.cluster), len));
        left -= len;
    }
    out
}

/// Fills `buf` from byte offset `pos` of a stream laid out as `segs`. Sparse parts read as zeros.
fn read_stream_at(
    volume: &File,
    segs: &[(Option<u64>, u64)],
    buf: &mut [u8],
    pos: u64,
) -> io::Result<()> {
    let mut seg_start = 0u64;
    let mut done = 0usize;
    for &(offset, len) in segs {
        let seg_end = seg_start + len;
        let at = pos + done as u64;
        if done < buf.len() && at < seg_end {
            let skip = at - seg_start;
            let take = usize::try_from(seg_end - at).unwrap_or(usize::MAX);
            let take = take.min(buf.len() - done);
            let part = &mut buf[done..done + take];
            match offset {
                Some(off) => read_exact_at(volume, part, off + skip)?,
                None => part.fill(0),
            }
            done += take;
        }
        seg_start = seg_end;
    }
    if done < buf.len() {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(())
}

/// What one MFT slot holds after parsing.
enum Slot {
    Base(FileSummary),
    Extension(FileRef, Vec<u8>),
}

struct Parsed {
    files: Vec<Option<FileSummary>>,
    extensions: Vec<(FileRef, Vec<u8>)>,
    bad: u64,
}

/// Reads and parses the whole MFT.
fn read_mft(volume: &File, g: Geometry, progress: &ScanProgress) -> io::Result<Parsed> {
    let layout = mft_layout(volume, g)?;
    let total = layout.size - layout.size % g.record as u64;
    let records = (total / g.record as u64) as usize;
    let mut files: Vec<Option<FileSummary>> = Vec::new();
    files.resize_with(records, || None);
    let mut extensions = Vec::new();
    let bad = AtomicU64::new(0);
    let segs = segments(&layout.runs, g, total);
    let bitmap = layout.bitmap.as_deref();
    let per_chunk = CHUNK / g.record;

    // An MFT that grew and then emptied can be mostly free records; skip windows without any.
    let windows: Vec<usize> = (0..records)
        .step_by(per_chunk)
        .filter(|&first| {
            bitmap.is_none_or(|b| any_in_use(b, first, (first + per_chunk).min(records)))
        })
        .collect();
    let next = AtomicUsize::new(0);

    std::thread::scope(|scope| -> io::Result<()> {
        // Reader threads keep several reads in flight while earlier chunks are parsed.
        let (tx, rx) = sync_channel::<io::Result<(usize, Vec<u8>)>>(READERS + 1);
        for _ in 0..READERS {
            let tx = tx.clone();
            let (windows, next, segs) = (&windows, &next, &segs);
            scope.spawn(move || {
                while let Some(&first) = windows.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if progress.is_cancelled() {
                        return;
                    }
                    let end = (first + per_chunk).min(records);
                    let mut chunk = vec![0u8; (end - first) * g.record];
                    let pos = (first * g.record) as u64;
                    let msg =
                        read_stream_at(volume, segs, &mut chunk, pos).map(|()| (first, chunk));
                    let failed = msg.is_err();
                    if tx.send(msg).is_err() || failed {
                        return;
                    }
                }
            });
        }
        drop(tx);

        for msg in rx {
            let (first, mut chunk) = msg?;
            let slots: Vec<(usize, Slot)> = chunk
                .par_chunks_mut(g.record)
                .enumerate()
                .filter_map(|(i, rec)| {
                    let n = first + i;
                    match parse_slot(rec) {
                        Ok(slot) => slot.map(|s| (n, s)),
                        Err(_) => {
                            bad.fetch_add(1, Ordering::Relaxed);
                            None
                        }
                    }
                })
                .collect();
            for (n, slot) in slots {
                match slot {
                    Slot::Base(s) => {
                        if let Some(f) = files.get_mut(n) {
                            *f = Some(s);
                        }
                    }
                    Slot::Extension(base, bytes) => extensions.push((base, bytes)),
                }
            }
        }
        Ok(())
    })?;

    Ok(Parsed {
        files,
        extensions,
        bad: bad.into_inner(),
    })
}

/// Parses one record. `Ok(None)` is a free slot.
fn parse_slot(rec: &mut [u8]) -> tc_ntfs::Result<Option<Slot>> {
    // Never-used slots are all zeros.
    if rec[..4] != *b"FILE" {
        return Ok(None);
    }
    apply_fixups(rec)?;
    let r = FileRecord::parse(rec)?;
    if !r.in_use() {
        return Ok(None);
    }
    if r.is_extension() {
        return Ok(Some(Slot::Extension(r.base, rec.to_vec())));
    }
    let mut s = FileSummary::default();
    s.add_record(&r)?;
    Ok(Some(Slot::Base(s)))
}

/// Folds extension records into their base records.
fn merge_extensions(parsed: &mut Parsed) {
    for (base, bytes) in std::mem::take(&mut parsed.extensions) {
        let Some(Some(s)) = parsed.files.get_mut(base.record() as usize) else {
            continue;
        };
        if s.sequence != base.sequence() {
            continue; // the base record was reused; this extension is stale
        }
        let ok = FileRecord::parse(&bytes).and_then(|r| s.add_record(&r));
        if ok.is_err() {
            parsed.bad += 1;
        }
    }
}

/// Children of each directory as `(child record, name index)`, in compressed-row form.
struct Children {
    start: Vec<u32>,
    items: Vec<(u32, u16)>,
}

impl Children {
    fn build(files: &[Option<FileSummary>]) -> Self {
        let valid_parent = |child: usize, p: FileRef| -> Option<usize> {
            let pi = p.record() as usize;
            let parent = files.get(pi)?.as_ref()?;
            (pi != child && parent.is_directory && parent.sequence == p.sequence()).then_some(pi)
        };
        let mut count = vec![0u32; files.len() + 1];
        for (i, f) in files.iter().enumerate().skip(FIRST_USER_RECORD as usize) {
            for n in f.iter().flat_map(|f| &f.names) {
                if let Some(p) = valid_parent(i, n.parent) {
                    count[p + 1] += 1;
                }
            }
        }
        for i in 1..count.len() {
            count[i] += count[i - 1];
        }
        let mut next = count.clone();
        let mut items = vec![(0u32, 0u16); count[files.len()] as usize];
        for (i, f) in files.iter().enumerate().skip(FIRST_USER_RECORD as usize) {
            for (k, n) in f.iter().flat_map(|f| f.names.iter().enumerate()) {
                if let Some(p) = valid_parent(i, n.parent) {
                    items[next[p] as usize] = (i as u32, k as u16);
                    next[p] += 1;
                }
            }
        }
        Self {
            start: count,
            items,
        }
    }

    fn of(&self, dir: usize) -> &[(u32, u16)] {
        &self.items[self.start[dir] as usize..self.start[dir + 1] as usize]
    }
}

/// The entry for one name of `s`, counting its size. Later names of a hard-linked file go
/// through [`count_once`].
fn entry(s: &FileSummary, name: &FileName) -> Entry {
    let mut flags = windows_attribute_flags(s.attributes);
    if s.is_directory {
        flags.insert(NodeFlags::DIR);
    }
    // A non-resident reparse point has no tag here; treat it as a link so it is not descended.
    if s.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && s.reparse_tag.is_none_or(|t| t & NAME_SURROGATE_BIT != 0)
    {
        flags.insert(NodeFlags::LINK);
    }
    let (size, allocated) = if s.is_directory || flags.contains(NodeFlags::LINK) {
        (0, 0)
    } else {
        (s.size, s.allocated)
    };
    Entry {
        name: OsString::from_wide(&name.name),
        size,
        allocated,
        mtime: filetime_to_unix(s.modified),
        flags,
    }
}

/// Counts a file's size under the first of its names that is inserted; later names become
/// zero-sized duplicates.
fn count_once(e: &mut Entry, counted: &mut bool) {
    if e.flags.contains(NodeFlags::DIR) || !std::mem::replace(counted, true) {
        return;
    }
    if !e.flags.contains(NodeFlags::LINK) {
        e.flags.insert(NodeFlags::HARDLINK_DUP);
        e.size = 0;
        e.allocated = 0;
    }
}

/// Inserts everything below `root` into the tree, breadth first, in batches. Each batch's
/// entries are built in parallel; only the insertion holds the tree's write lock.
fn build_tree(
    tree: &SharedTree,
    progress: &ScanProgress,
    files: &[Option<FileSummary>],
    root: usize,
) {
    let children = Children::build(files);
    let mut counted = vec![false; files.len()];
    let root_id = tree.read().root();
    let mut queue = std::collections::VecDeque::from([(root, root_id)]);
    let mut subdirs = Vec::new();
    while !queue.is_empty() {
        if progress.is_cancelled() {
            return;
        }
        let batch: Vec<_> = queue.drain(..queue.len().min(DIRS_PER_BATCH)).collect();
        let listings: Vec<(Vec<u32>, Vec<Entry>)> = batch
            .par_iter()
            .map(|&(dir, _)| {
                children
                    .of(dir)
                    .iter()
                    .filter_map(|&(child, k)| {
                        let s = files[child as usize].as_ref()?;
                        Some((child, entry(s, &s.names[usize::from(k)])))
                    })
                    .unzip()
            })
            .collect();
        let mut t = tree.write();
        for (&(_, id), (records, mut entries)) in batch.iter().zip(listings) {
            for (&r, e) in records.iter().zip(&mut entries) {
                count_once(e, &mut counted[r as usize]);
            }
            subdirs.clear();
            t.insert_listing(id, &entries, &mut subdirs);
            progress.add(entries.len() as u64, 1);
            let traversable = records
                .iter()
                .zip(&entries)
                .filter(|(_, e)| e.is_traversable_dir())
                .map(|(&r, _)| r as usize);
            queue.extend(traversable.zip(subdirs.iter().copied()));
        }
    }
}

/// Scans the root of `tree` from the MFT. On error nothing has been inserted yet.
pub fn scan(tree: &SharedTree, progress: &ScanProgress) -> io::Result<()> {
    let root_path = {
        let t = tree.read();
        t.path(t.root())
    };
    let volume = open_volume(&root_path)?;
    let g = geometry(&volume)?;
    let root = record_of(&root_path)?;
    let mut parsed = read_mft(&volume, g, progress)?;
    if progress.is_cancelled() {
        return Ok(());
    }
    merge_extensions(&mut parsed);
    let root_ok = parsed
        .files
        .get(root.record() as usize)
        .and_then(Option::as_ref)
        .is_some_and(|s| s.is_directory && s.sequence == root.sequence());
    if !root_ok {
        return Err(bad_data("the scan root was not found in the MFT"));
    }
    // A few torn or garbled records are normal on a live volume; many mean something is wrong.
    if parsed.bad > 1000 && parsed.bad * 100 > parsed.files.len() as u64 {
        return Err(bad_data(format!("{} unreadable MFT records", parsed.bad)));
    }
    build_tree(tree, progress, &parsed.files, root.record() as usize);
    if parsed.bad > 0 {
        progress.record_error(
            &root_path,
            &bad_data(format!(
                "{} MFT records could not be read (changed during the scan?)",
                parsed.bad
            )),
        );
    }
    // Millions of small allocations take a noticeable time to free; don't make the scan wait.
    std::thread::spawn(move || drop(parsed));
    Ok(())
}
