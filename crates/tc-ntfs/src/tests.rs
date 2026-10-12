use super::*;

const RECORD_SIZE: usize = 1024;
const USN: [u8; 2] = [0x34, 0x12];

fn align8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn resident(type_code: u32, name: &str, value: &[u8]) -> Vec<u8> {
    let name = utf16(name);
    let value_offset = align8(0x18 + name.len());
    let mut a = vec![0u8; align8(value_offset + value.len())];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    let len = a.len() as u32;
    a[4..8].copy_from_slice(&len.to_le_bytes());
    a[9] = (name.len() / 2) as u8;
    a[0x0A..0x0C].copy_from_slice(&0x18u16.to_le_bytes());
    a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
    a[0x14..0x16].copy_from_slice(&(value_offset as u16).to_le_bytes());
    a[0x18..0x18 + name.len()].copy_from_slice(&name);
    a[value_offset..value_offset + value.len()].copy_from_slice(value);
    a
}

struct NonRes<'a> {
    name: &'a str,
    flags: u16,
    start_vcn: u64,
    allocated: u64,
    size: u64,
    total_allocated: Option<u64>,
    runlist: &'a [u8],
}

fn non_resident(type_code: u32, n: &NonRes<'_>) -> Vec<u8> {
    let header = if n.total_allocated.is_some() {
        0x48
    } else {
        0x40
    };
    let name = utf16(n.name);
    let runlist_offset = align8(header + name.len());
    let mut a = vec![0u8; align8(runlist_offset + n.runlist.len())];
    a[0..4].copy_from_slice(&type_code.to_le_bytes());
    let len = a.len() as u32;
    a[4..8].copy_from_slice(&len.to_le_bytes());
    a[8] = 1;
    a[9] = (name.len() / 2) as u8;
    a[0x0A..0x0C].copy_from_slice(&(header as u16).to_le_bytes());
    a[0x0C..0x0E].copy_from_slice(&n.flags.to_le_bytes());
    a[0x10..0x18].copy_from_slice(&n.start_vcn.to_le_bytes());
    a[0x20..0x22].copy_from_slice(&(runlist_offset as u16).to_le_bytes());
    a[0x28..0x30].copy_from_slice(&n.allocated.to_le_bytes());
    a[0x30..0x38].copy_from_slice(&n.size.to_le_bytes());
    a[0x38..0x40].copy_from_slice(&n.size.to_le_bytes());
    if let Some(t) = n.total_allocated {
        a[0x40..0x48].copy_from_slice(&t.to_le_bytes());
    }
    a[header..header + name.len()].copy_from_slice(&name);
    a[runlist_offset..runlist_offset + n.runlist.len()].copy_from_slice(n.runlist);
    a
}

fn std_info(modified: i64, attributes: u32) -> Vec<u8> {
    let mut v = vec![0u8; 0x48];
    v[0x08..0x10].copy_from_slice(&modified.to_le_bytes());
    v[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
    resident(attr::STANDARD_INFORMATION, "", &v)
}

fn file_name(parent: u64, ns: u8, name: &str) -> Vec<u8> {
    let n = utf16(name);
    let mut v = vec![0u8; 0x42 + n.len()];
    v[0..8].copy_from_slice(&parent.to_le_bytes());
    v[0x40] = (n.len() / 2) as u8;
    v[0x41] = ns;
    v[0x42..].copy_from_slice(&n);
    resident(attr::FILE_NAME, "", &v)
}

/// Builds a FILE record and protects it with fixups, as NTFS writes it.
fn record(sequence: u16, flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![0u8; RECORD_SIZE];
    r[0..4].copy_from_slice(b"FILE");
    let strides = RECORD_SIZE / FIXUP_STRIDE;
    r[0x04..0x06].copy_from_slice(&0x30u16.to_le_bytes());
    r[0x06..0x08].copy_from_slice(&((strides + 1) as u16).to_le_bytes());
    r[0x10..0x12].copy_from_slice(&sequence.to_le_bytes());
    r[0x12..0x14].copy_from_slice(&1u16.to_le_bytes());
    let first = align8(0x30 + 2 * (strides + 1));
    r[0x14..0x16].copy_from_slice(&(first as u16).to_le_bytes());
    r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
    r[0x1C..0x20].copy_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
    r[0x20..0x28].copy_from_slice(&base.to_le_bytes());
    let mut off = first;
    for a in attrs {
        r[off..off + a.len()].copy_from_slice(a);
        off += a.len();
    }
    r[off..off + 4].copy_from_slice(&attr::END.to_le_bytes());
    let used = (off + 8) as u32;
    r[0x18..0x1C].copy_from_slice(&used.to_le_bytes());
    // Protect: move the last two bytes of each stride into the update sequence array.
    r[0x30..0x32].copy_from_slice(&USN);
    for i in 0..strides {
        let end = (i + 1) * FIXUP_STRIDE;
        let saved = [r[end - 2], r[end - 1]];
        r[0x32 + 2 * i..0x34 + 2 * i].copy_from_slice(&saved);
        r[end - 2..end].copy_from_slice(&USN);
    }
    r
}

fn summarize(records: &[Vec<u8>]) -> Result<FileSummary> {
    let mut s = FileSummary::default();
    for r in records {
        let mut r = r.clone();
        apply_fixups(&mut r)?;
        s.add_record(&FileRecord::parse(&r)?)?;
    }
    Ok(s)
}

#[test]
fn file_refs_split_into_record_and_sequence() {
    let r = FileRef(0x0003_0000_0000_0010);
    assert_eq!(r.record(), 16);
    assert_eq!(r.sequence(), 3);
}

#[test]
fn fixups_restore_bytes_and_detect_torn_records() {
    // Put recognizable bytes where the fixups go, inside a resident data value.
    let r = record(
        1,
        RECORD_IN_USE,
        0,
        &[resident(attr::DATA, "", &[0xAB; 700])],
    );
    let mut ok = r.clone();
    apply_fixups(&mut ok).unwrap();
    assert_eq!(&ok[510..512], &[0xAB, 0xAB]);

    let mut torn = r.clone();
    torn[1022] = 0; // second stride's check bytes no longer match
    assert_eq!(apply_fixups(&mut torn), Err(Error::TornRecord));

    let mut bad_count = r;
    bad_count[0x06..0x08].copy_from_slice(&9u16.to_le_bytes()); // 8 strides > 1024 bytes
    assert!(apply_fixups(&mut bad_count).is_err());
}

#[test]
fn boot_sector_geometry() {
    let mut b = vec![0u8; 512];
    b[3..11].copy_from_slice(b"NTFS    ");
    b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
    b[0x0D] = 8;
    b[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
    b[0x40] = (-10i8) as u8; // 2^10 = 1024-byte records
    let bs = BootSector::parse(&b).unwrap();
    assert_eq!(bs.cluster_size, 4096);
    assert_eq!(bs.record_size, 1024);
    assert_eq!(bs.mft_cluster, 786_432);

    b[0x0D] = 0xF4; // 2^(256-244) = 4096 sectors per cluster = 2 MiB clusters
    b[0x40] = 1; // one cluster per record: too big
    assert_eq!(
        BootSector::parse(&b),
        Err(Error::Invalid("file record size"))
    );

    b[3] = b'X';
    assert_eq!(BootSector::parse(&b), Err(Error::BadSignature));
}

#[test]
fn file_with_long_short_and_hard_link_names() {
    let r = record(
        7,
        RECORD_IN_USE,
        0,
        &[
            std_info(133_000_000_000_000_000, 0x20),
            file_name(0x0001_0000_0000_0005, namespace::WIN32, "Long name.txt"),
            file_name(0x0001_0000_0000_0005, namespace::DOS, "LONGNA~1.TXT"),
            file_name(0x0002_0000_0000_0040, namespace::POSIX, "link.txt"),
            resident(attr::DATA, "", b"hello"),
        ],
    );
    let s = summarize(&[r]).unwrap();
    assert_eq!(s.sequence, 7);
    assert!(!s.is_directory);
    assert_eq!(s.modified, 133_000_000_000_000_000);
    assert_eq!(s.attributes, 0x20);
    let names: Vec<String> = s
        .names
        .iter()
        .map(|n| String::from_utf16_lossy(&n.name))
        .collect();
    assert_eq!(names, ["Long name.txt", "link.txt"]);
    assert_eq!(s.names[1].parent.record(), 0x40);
    assert_eq!(s.names[1].parent.sequence(), 2);
    assert_eq!((s.size, s.allocated), (5, 8));
}

#[test]
fn non_resident_compressed_data_uses_total_allocated() {
    let r = record(
        1,
        RECORD_IN_USE,
        0,
        &[non_resident(
            attr::DATA,
            &NonRes {
                name: "",
                flags: ATTR_COMPRESSED,
                start_vcn: 0,
                allocated: 1 << 20,
                size: 1_000_000,
                total_allocated: Some(65_536),
                runlist: &[0x11, 0x10, 0x20, 0x00],
            },
        )],
    );
    let s = summarize(&[r]).unwrap();
    assert_eq!((s.size, s.allocated), (1_000_000, 65_536));
}

#[test]
fn named_streams_are_counted_separately() {
    let r = record(
        1,
        RECORD_IN_USE,
        0,
        &[
            resident(attr::DATA, "", b"main"),
            resident(attr::DATA, "Zone.Identifier", b"[ZoneTransfer]"),
        ],
    );
    let s = summarize(&[r]).unwrap();
    assert_eq!(s.size, 4);
    assert_eq!(s.stream_size, 14);
}

#[test]
fn extension_records_contribute_their_attributes() {
    let base = record(
        3,
        RECORD_IN_USE | RECORD_IS_DIRECTORY,
        0,
        &[
            std_info(1, 0x10),
            resident(attr::ATTRIBUTE_LIST, "", &[0u8; 0x20]),
            file_name(5, namespace::WIN32_AND_DOS, "big"),
        ],
    );
    let first_extent = NonRes {
        name: "",
        flags: 0,
        start_vcn: 0,
        allocated: 8192,
        size: 8000,
        total_allocated: None,
        runlist: &[0x11, 0x01, 0x10, 0x00],
    };
    let later_extent = NonRes {
        start_vcn: 1,
        allocated: 0,
        size: 0,
        ..first_extent
    };
    let ext = record(
        9,
        RECORD_IN_USE,
        0x0003_0000_0000_0020,
        &[
            non_resident(attr::DATA, &first_extent),
            non_resident(attr::DATA, &later_extent),
        ],
    );
    let s = summarize(&[base, ext]).unwrap();
    assert!(s.has_attribute_list);
    assert!(s.is_directory);
    assert_eq!(
        s.sequence, 3,
        "extension records must not overwrite the base sequence"
    );
    assert_eq!((s.size, s.allocated), (8000, 8192));
}

#[test]
fn reparse_tags_are_read() {
    let tag = 0xA000_0003u32.to_le_bytes(); // mount point / junction
    let mut v = tag.to_vec();
    v.extend([0u8; 4]);
    let r = record(
        1,
        RECORD_IN_USE | RECORD_IS_DIRECTORY,
        0,
        &[resident(attr::REPARSE_POINT, "", &v)],
    );
    assert_eq!(summarize(&[r]).unwrap().reparse_tag, Some(0xA000_0003));
}

#[test]
fn runlists_with_negative_and_sparse_runs() {
    let runs = decode_runlist(&[
        0x21, 0x18, 0x34, 0x56, // 0x18 clusters at LCN 0x5634
        0x11, 0x10, 0xF0, // 0x10 clusters at LCN 0x5634 - 0x10
        0x01, 0x05, // 5 sparse clusters
        0x11, 0x02, 0x20, // 2 clusters at previous LCN + 0x20
        0x00,
    ])
    .unwrap();
    assert_eq!(
        runs,
        [
            Run {
                lcn: Some(0x5634),
                clusters: 0x18
            },
            Run {
                lcn: Some(0x5624),
                clusters: 0x10
            },
            Run {
                lcn: None,
                clusters: 5
            },
            Run {
                lcn: Some(0x5644),
                clusters: 2
            },
        ]
    );
}

#[test]
fn bad_runlists_are_rejected() {
    assert_eq!(decode_runlist(&[]), Err(Error::Truncated));
    assert_eq!(decode_runlist(&[0x21, 0x18]), Err(Error::Truncated));
    assert!(
        decode_runlist(&[0x11, 0x00, 0x10, 0x00]).is_err(),
        "zero-length run"
    );
    assert!(
        decode_runlist(&[0x11, 0x01, 0xF0, 0x00]).is_err(),
        "negative LCN"
    );
    assert!(
        decode_runlist(&[0x09, 0x01, 0x00]).is_err(),
        "9-byte length field"
    );
}

#[test]
fn attribute_lists_parse() {
    let mut e = vec![0u8; 0x20];
    e[0..4].copy_from_slice(&attr::DATA.to_le_bytes());
    e[4..6].copy_from_slice(&0x20u16.to_le_bytes());
    e[0x08..0x10].copy_from_slice(&4u64.to_le_bytes());
    e[0x10..0x18].copy_from_slice(&0x0001_0000_0000_0099u64.to_le_bytes());
    let list = parse_attribute_list(&[e.clone(), e].concat()).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].type_code, attr::DATA);
    assert_eq!(list[0].start_vcn, 4);
    assert_eq!(list[0].record.record(), 0x99);
    assert!(parse_attribute_list(&[0u8; 8]).is_err());
}

/// Truncated and corrupted records must give errors, never panics.
#[test]
fn malformed_records_never_panic() {
    let good = record(
        1,
        RECORD_IN_USE,
        0,
        &[
            std_info(1, 0x20),
            file_name(5, namespace::WIN32, "name"),
            resident(attr::DATA, "s", b"abc"),
            non_resident(
                attr::DATA,
                &NonRes {
                    name: "",
                    flags: ATTR_SPARSE,
                    start_vcn: 0,
                    allocated: 4096,
                    size: 4096,
                    total_allocated: Some(0),
                    runlist: &[0x01, 0x01, 0x00],
                },
            ),
        ],
    );
    let parse = |r: &[u8]| {
        let mut r = r.to_vec();
        let _ = apply_fixups(&mut r);
        if let Ok(rec) = FileRecord::parse(&r) {
            let _ = FileSummary::default().add_record(&rec);
        }
    };
    for len in 0..good.len() {
        parse(&good[..len]);
    }
    // Deterministic pseudo-random single and multi-byte corruption.
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..20_000 {
        let mut r = good.clone();
        for _ in 0..1 + (seed % 4) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let i = (seed as usize) % r.len();
            r[i] = (seed >> 32) as u8;
        }
        parse(&r);
    }
}
