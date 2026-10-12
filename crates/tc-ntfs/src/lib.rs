//! Pure parser for NTFS on-disk structures (no OS calls, fuzzable).
//!
//! Everything here works on byte slices read by someone else (see `tc-scan-win`'s MFT
//! scanner), so it can be unit-tested with hand-built records and fuzzed. Every offset and
//! length read from disk is bounds-checked; malformed input gives an [`Error`], never a panic.
//!
//! Layouts follow the public NTFS documentation (see `spec/SOURCES.md`).

#![forbid(unsafe_code)]

use std::fmt;

/// Attribute type codes used by the scanner.
pub mod attr {
    pub const STANDARD_INFORMATION: u32 = 0x10;
    pub const ATTRIBUTE_LIST: u32 = 0x20;
    pub const FILE_NAME: u32 = 0x30;
    pub const DATA: u32 = 0x80;
    pub const BITMAP: u32 = 0xB0;
    pub const REPARSE_POINT: u32 = 0xC0;
    pub const END: u32 = 0xFFFF_FFFF;
}

/// `$FILE_NAME` namespaces. DOS-only names are 8.3 aliases of another name on the same record.
pub mod namespace {
    pub const POSIX: u8 = 0;
    pub const WIN32: u8 = 1;
    pub const DOS: u8 = 2;
    pub const WIN32_AND_DOS: u8 = 3;
}

/// Record number of the root directory.
pub const ROOT_RECORD: u64 = 5;

/// Fixups protect every 512-byte stride of a record, whatever the sector size.
const FIXUP_STRIDE: usize = 512;

const RECORD_IN_USE: u16 = 0x0001;
const RECORD_IS_DIRECTORY: u16 = 0x0002;

const ATTR_COMPRESSED: u16 = 0x0001;
const ATTR_SPARSE: u16 = 0x8000;

/// Why a structure could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A read would go past the end of the buffer.
    Truncated,
    /// The record or boot sector does not start with the expected signature.
    BadSignature,
    /// A sector of the record was not fully written (update sequence mismatch).
    TornRecord,
    /// A header field is out of range.
    Invalid(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("structure is truncated"),
            Error::BadSignature => f.write_str("bad signature"),
            Error::TornRecord => f.write_str("torn record (update sequence mismatch)"),
            Error::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn bytes<const N: usize>(b: &[u8], off: usize) -> Result<[u8; N]> {
    let end = off.checked_add(N).ok_or(Error::Truncated)?;
    let s = b.get(off..end).ok_or(Error::Truncated)?;
    Ok(s.try_into().expect("slice has length N"))
}

fn u8_at(b: &[u8], off: usize) -> Result<u8> {
    b.get(off).copied().ok_or(Error::Truncated)
}

fn u16_at(b: &[u8], off: usize) -> Result<u16> {
    bytes::<2>(b, off).map(u16::from_le_bytes)
}

fn u32_at(b: &[u8], off: usize) -> Result<u32> {
    bytes::<4>(b, off).map(u32::from_le_bytes)
}

fn u64_at(b: &[u8], off: usize) -> Result<u64> {
    bytes::<8>(b, off).map(u64::from_le_bytes)
}

fn i64_at(b: &[u8], off: usize) -> Result<i64> {
    bytes::<8>(b, off).map(i64::from_le_bytes)
}

fn slice(b: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    let end = off.checked_add(len).ok_or(Error::Truncated)?;
    b.get(off..end).ok_or(Error::Truncated)
}

/// A file reference: a record number (low 48 bits) and its sequence number (high 16 bits).
/// The sequence number changes each time a record is reused, so a stale reference to a
/// deleted file can be told apart from the record's new owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FileRef(pub u64);

impl FileRef {
    pub fn record(self) -> u64 {
        self.0 & 0x0000_FFFF_FFFF_FFFF
    }
    pub fn sequence(self) -> u16 {
        (self.0 >> 48) as u16
    }
}

/// Volume geometry from the NTFS boot sector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootSector {
    pub bytes_per_sector: u32,
    pub cluster_size: u32,
    /// First cluster of `$MFT`.
    pub mft_cluster: u64,
    /// Size of one FILE record in bytes (usually 1024 or 4096).
    pub record_size: u32,
}

impl BootSector {
    pub fn parse(b: &[u8]) -> Result<Self> {
        if slice(b, 3, 8)? != b"NTFS    " {
            return Err(Error::BadSignature);
        }
        let bytes_per_sector = u32::from(u16_at(b, 0x0B)?);
        if !(256..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
            return Err(Error::Invalid("bytes per sector"));
        }
        // Values above 0x80 encode 2^(256 - n) sectors per cluster (large clusters).
        let spc = u8_at(b, 0x0D)?;
        let sectors_per_cluster = if spc > 0x80 {
            1u32.checked_shl(256 - u32::from(spc))
                .ok_or(Error::Invalid("sectors per cluster"))?
        } else {
            u32::from(spc)
        };
        if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
            return Err(Error::Invalid("sectors per cluster"));
        }
        let cluster_size = bytes_per_sector
            .checked_mul(sectors_per_cluster)
            .filter(|&c| c <= 2 * 1024 * 1024)
            .ok_or(Error::Invalid("cluster size"))?;
        let record_size = record_size(u8_at(b, 0x40)? as i8, cluster_size)?;
        Ok(Self {
            bytes_per_sector,
            cluster_size,
            mft_cluster: u64_at(b, 0x30)?,
            record_size,
        })
    }
}

/// Decodes "clusters per record": positive is a cluster count, negative n means 2^-n bytes.
fn record_size(raw: i8, cluster_size: u32) -> Result<u32> {
    let size = if raw < 0 {
        1u32.checked_shl(u32::from(raw.unsigned_abs()))
    } else {
        cluster_size.checked_mul(raw as u32)
    };
    size.filter(|s| (512..=65536).contains(s) && s.is_power_of_two())
        .ok_or(Error::Invalid("file record size"))
}

/// Checks and removes the update sequence ("fixups") of a multi-sector structure in place.
/// The last two bytes of every 512-byte stride were replaced by the update sequence number
/// when written; this verifies them and puts the original bytes back.
pub fn apply_fixups(record: &mut [u8]) -> Result<()> {
    let usa_offset = usize::from(u16_at(record, 0x04)?);
    let usa_count = usize::from(u16_at(record, 0x06)?);
    if usa_count < 2 {
        return Err(Error::Invalid("update sequence size"));
    }
    let strides = usa_count - 1;
    if strides * FIXUP_STRIDE > record.len() {
        return Err(Error::Invalid("update sequence size"));
    }
    let usn = bytes::<2>(record, usa_offset)?;
    for i in 0..strides {
        let fix = bytes::<2>(record, usa_offset + 2 + 2 * i)?;
        let end = (i + 1) * FIXUP_STRIDE;
        if record[end - 2..end] != usn {
            return Err(Error::TornRecord);
        }
        record[end - 2..end].copy_from_slice(&fix);
    }
    Ok(())
}

/// A FILE record (one entry of `$MFT`) after fixups have been applied.
#[derive(Debug, Clone, Copy)]
pub struct FileRecord<'a> {
    data: &'a [u8],
    pub sequence: u16,
    pub link_count: u16,
    flags: u16,
    /// For an extension record, the base record it belongs to; zero for a base record.
    pub base: FileRef,
    first_attribute: usize,
}

impl<'a> FileRecord<'a> {
    /// Parses a record header. Call [`apply_fixups`] first.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if slice(data, 0, 4)? != b"FILE" {
            return Err(Error::BadSignature);
        }
        let used = u32_at(data, 0x18)? as usize;
        if used > data.len() {
            return Err(Error::Invalid("record used size"));
        }
        let data = &data[..used];
        let first_attribute = usize::from(u16_at(data, 0x14)?);
        if first_attribute >= data.len() {
            return Err(Error::Invalid("first attribute offset"));
        }
        Ok(Self {
            data,
            sequence: u16_at(data, 0x10)?,
            link_count: u16_at(data, 0x12)?,
            flags: u16_at(data, 0x16)?,
            base: FileRef(u64_at(data, 0x20)?),
            first_attribute,
        })
    }

    pub fn in_use(&self) -> bool {
        self.flags & RECORD_IN_USE != 0
    }

    pub fn is_directory(&self) -> bool {
        self.flags & RECORD_IS_DIRECTORY != 0
    }

    pub fn is_extension(&self) -> bool {
        self.base.0 != 0
    }

    pub fn attributes(&self) -> Attributes<'a> {
        Attributes {
            data: self.data,
            offset: self.first_attribute,
            done: false,
        }
    }
}

/// Iterator over the attributes of a record. Stops after the first error.
#[derive(Debug, Clone)]
pub struct Attributes<'a> {
    data: &'a [u8],
    offset: usize,
    done: bool,
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Result<Attribute<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = self.read();
        match &result {
            Ok(None) | Err(_) => self.done = true,
            Ok(Some(_)) => {}
        }
        result.transpose()
    }
}

impl<'a> Attributes<'a> {
    fn read(&mut self) -> Result<Option<Attribute<'a>>> {
        let type_code = u32_at(self.data, self.offset)?;
        if type_code == attr::END {
            return Ok(None);
        }
        let length = u32_at(self.data, self.offset + 4)? as usize;
        if length < 0x18 {
            return Err(Error::Invalid("attribute length"));
        }
        let a = slice(self.data, self.offset, length)?;
        self.offset += length;
        Attribute::parse(type_code, a).map(Some)
    }
}

/// One attribute of a record.
#[derive(Debug, Clone, Copy)]
pub struct Attribute<'a> {
    pub type_code: u32,
    /// The attribute name as raw UTF-16LE bytes; empty for the unnamed (main) stream.
    pub name: &'a [u8],
    pub flags: u16,
    pub body: Body<'a>,
}

/// Where an attribute's value lives.
#[derive(Debug, Clone, Copy)]
pub enum Body<'a> {
    /// The value is stored in the record itself.
    Resident(&'a [u8]),
    /// The value is stored in clusters described by a runlist.
    NonResident(NonResident<'a>),
}

#[derive(Debug, Clone, Copy)]
pub struct NonResident<'a> {
    /// First virtual cluster this extent covers. Sizes are only valid when it is 0.
    pub start_vcn: u64,
    pub last_vcn: u64,
    pub allocated_size: u64,
    pub data_size: u64,
    pub initialized_size: u64,
    /// Clusters actually allocated, present on compressed and sparse attributes.
    pub total_allocated: Option<u64>,
    /// Encoded runlist; see [`decode_runlist`].
    pub runlist: &'a [u8],
}

impl<'a> Attribute<'a> {
    fn parse(type_code: u32, a: &'a [u8]) -> Result<Self> {
        let non_resident = u8_at(a, 0x08)? != 0;
        let name_len = usize::from(u8_at(a, 0x09)?) * 2;
        let name_offset = usize::from(u16_at(a, 0x0A)?);
        let name = if name_len == 0 {
            &[][..]
        } else {
            slice(a, name_offset, name_len)?
        };
        let flags = u16_at(a, 0x0C)?;
        let body = if non_resident {
            let runlist_offset = usize::from(u16_at(a, 0x20)?);
            let compressed_or_sparse = flags & (ATTR_COMPRESSED | ATTR_SPARSE) != 0;
            Body::NonResident(NonResident {
                start_vcn: u64_at(a, 0x10)?,
                last_vcn: u64_at(a, 0x18)?,
                allocated_size: u64_at(a, 0x28)?,
                data_size: u64_at(a, 0x30)?,
                initialized_size: u64_at(a, 0x38)?,
                total_allocated: if compressed_or_sparse && runlist_offset >= 0x48 {
                    Some(u64_at(a, 0x40)?)
                } else {
                    None
                },
                runlist: a.get(runlist_offset..).ok_or(Error::Truncated)?,
            })
        } else {
            let len = u32_at(a, 0x10)? as usize;
            let off = usize::from(u16_at(a, 0x14)?);
            Body::Resident(slice(a, off, len)?)
        };
        Ok(Self {
            type_code,
            name,
            flags,
            body,
        })
    }

    pub fn is_named(&self) -> bool {
        !self.name.is_empty()
    }
}

/// `$STANDARD_INFORMATION`: the times and attributes shown by the file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardInformation {
    /// Last write time in FILETIME units (100 ns since 1601).
    pub modified: i64,
    /// Win32 file attribute bits (`FILE_ATTRIBUTE_*`).
    pub attributes: u32,
}

impl StandardInformation {
    pub fn parse(v: &[u8]) -> Result<Self> {
        Ok(Self {
            modified: i64_at(v, 0x08)?,
            attributes: u32_at(v, 0x20)?,
        })
    }
}

/// `$FILE_NAME`: one name of the file and the directory it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    pub parent: FileRef,
    pub namespace: u8,
    /// UTF-16 code units; may contain unpaired surrogates.
    pub name: Vec<u16>,
}

impl FileName {
    pub fn parse(v: &[u8]) -> Result<Self> {
        let len = usize::from(u8_at(v, 0x40)?);
        let raw = slice(v, 0x42, len * 2)?;
        Ok(Self {
            parent: FileRef(u64_at(v, 0x00)?),
            namespace: u8_at(v, 0x41)?,
            name: raw
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect(),
        })
    }
}

/// One entry of an `$ATTRIBUTE_LIST`: which record holds (part of) an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributeListEntry {
    pub type_code: u32,
    pub start_vcn: u64,
    pub record: FileRef,
}

/// Parses the value of an `$ATTRIBUTE_LIST`.
pub fn parse_attribute_list(v: &[u8]) -> Result<Vec<AttributeListEntry>> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < v.len() {
        let len = usize::from(u16_at(v, off + 4)?);
        if len < 0x1A {
            return Err(Error::Invalid("attribute list entry length"));
        }
        let e = slice(v, off, len)?;
        out.push(AttributeListEntry {
            type_code: u32_at(e, 0)?,
            start_vcn: u64_at(e, 0x08)?,
            record: FileRef(u64_at(e, 0x10)?),
        });
        off += len;
    }
    Ok(out)
}

/// A run of clusters: `lcn` is the first logical cluster, or `None` for a sparse (unallocated)
/// run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub lcn: Option<u64>,
    pub clusters: u64,
}

/// Decodes a non-resident attribute's runlist.
pub fn decode_runlist(mut b: &[u8]) -> Result<Vec<Run>> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    loop {
        let header = *b.first().ok_or(Error::Truncated)?;
        if header == 0 {
            return Ok(runs);
        }
        let len_size = usize::from(header & 0x0F);
        let off_size = usize::from(header >> 4);
        if len_size == 0 || len_size > 8 || off_size > 8 {
            return Err(Error::Invalid("runlist header"));
        }
        let field = slice(b, 1, len_size + off_size)?;
        let clusters = le_unsigned(&field[..len_size]);
        if clusters == 0 {
            return Err(Error::Invalid("run length"));
        }
        let run_lcn = if off_size == 0 {
            None
        } else {
            lcn = lcn
                .checked_add(le_signed(&field[len_size..]))
                .filter(|&l| l >= 0)
                .ok_or(Error::Invalid("run offset"))?;
            Some(lcn as u64)
        };
        runs.push(Run {
            lcn: run_lcn,
            clusters,
        });
        b = &b[1 + len_size + off_size..];
    }
}

fn le_unsigned(b: &[u8]) -> u64 {
    b.iter().rev().fold(0, |acc, &x| (acc << 8) | u64::from(x))
}

fn le_signed(b: &[u8]) -> i64 {
    let unsigned = le_unsigned(b);
    let bits = b.len() * 8;
    if bits == 0 || bits >= 64 {
        unsigned as i64
    } else {
        // Sign-extend from the top bit of the last byte.
        let shift = 64 - bits;
        ((unsigned << shift) as i64) >> shift
    }
}

/// What the scanner needs from one file: everything gathered from its base record and any
/// extension records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSummary {
    pub sequence: u16,
    pub is_directory: bool,
    /// Names other than DOS 8.3 aliases. More than one means hard links.
    pub names: Vec<FileName>,
    pub modified: i64,
    pub attributes: u32,
    /// Logical size of the unnamed data stream.
    pub size: u64,
    /// Bytes allocated on disk for the unnamed data stream. For resident data this is the value
    /// length rounded up to 8 bytes, which is what Windows reports for such files.
    pub allocated: u64,
    /// Logical size of all named data streams (alternate data streams).
    pub stream_size: u64,
    pub reparse_tag: Option<u32>,
    /// The record has an `$ATTRIBUTE_LIST`, so some attributes live in extension records.
    pub has_attribute_list: bool,
}

impl FileSummary {
    /// Collects the attributes of one record (base or extension) into `self`.
    pub fn add_record(&mut self, record: &FileRecord<'_>) -> Result<()> {
        if !record.is_extension() {
            self.sequence = record.sequence;
            self.is_directory = record.is_directory();
        }
        for a in record.attributes() {
            let a = a?;
            match (a.type_code, a.body) {
                (attr::STANDARD_INFORMATION, Body::Resident(v)) => {
                    let si = StandardInformation::parse(v)?;
                    self.modified = si.modified;
                    self.attributes = si.attributes;
                }
                (attr::FILE_NAME, Body::Resident(v)) => {
                    let name = FileName::parse(v)?;
                    if name.namespace != namespace::DOS {
                        self.names.push(name);
                    }
                }
                (attr::DATA, body) => {
                    let (size, allocated) = match body {
                        Body::Resident(v) => (v.len() as u64, (v.len() as u64).next_multiple_of(8)),
                        // Only the first extent of a multi-record attribute carries sizes.
                        Body::NonResident(n) if n.start_vcn == 0 => {
                            (n.data_size, n.total_allocated.unwrap_or(n.allocated_size))
                        }
                        Body::NonResident(_) => continue,
                    };
                    if a.is_named() {
                        self.stream_size = self.stream_size.saturating_add(size);
                    } else {
                        self.size = size;
                        self.allocated = allocated;
                    }
                }
                (attr::REPARSE_POINT, Body::Resident(v)) => {
                    self.reparse_tag = Some(u32_at(v, 0)?);
                }
                (attr::ATTRIBUTE_LIST, _) => self.has_attribute_list = true,
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
