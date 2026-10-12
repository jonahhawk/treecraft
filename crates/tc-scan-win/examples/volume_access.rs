//! Probes which whole-volume metadata queries the current user may run on a volume, i.e.
//! whether a scanner could read the MFT or enumerate the USN data without opening every
//! directory.
//!
//! Usage: `cargo run --release -p tc-scan-win --example volume_access -- C`

#[cfg(not(windows))]
fn main() {
    eprintln!("volume_access probes NTFS volume handles and only runs on Windows");
}

#[cfg(windows)]
fn main() {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, ReadFile,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_VOLUME_DATA, MFT_ENUM_DATA_V0,
    };

    let letter = std::env::args().nth(1).unwrap_or_else(|| "C".into());
    let path: Vec<u16> = format!(r"\\.\{letter}:")
        .encode_utf16()
        .chain([0])
        .collect();
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    for (label, access) in [
        ("no access rights", 0),
        ("FILE_READ_ATTRIBUTES", FILE_READ_ATTRIBUTES),
        ("GENERIC_READ", GENERIC_READ),
    ] {
        // SAFETY: path is NUL-terminated; buffers outlive the calls; the handle is closed.
        unsafe {
            let h = CreateFileW(
                path.as_ptr(),
                access,
                share,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                println!("{label}: open failed, error {}", GetLastError());
                continue;
            }
            let mut buf = vec![0u64; 8192];
            let mut got = 0u32;
            let ok = DeviceIoControl(
                h,
                FSCTL_GET_NTFS_VOLUME_DATA,
                null(),
                0,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut got,
                null_mut(),
            );
            let volume_data = if ok != 0 {
                "ok".to_string()
            } else {
                format!("error {}", GetLastError())
            };
            let input = MFT_ENUM_DATA_V0 {
                StartFileReferenceNumber: 0,
                LowUsn: 0,
                HighUsn: i64::MAX,
            };
            let ok = DeviceIoControl(
                h,
                FSCTL_ENUM_USN_DATA,
                (&raw const input).cast(),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut got,
                null_mut(),
            );
            let enum_usn = if ok != 0 {
                format!("ok, {got} bytes")
            } else {
                format!("error {}", GetLastError())
            };
            let mut sector = vec![0u8; 4096];
            let ok = ReadFile(h, sector.as_mut_ptr(), 4096, &mut got, null_mut());
            let raw_read = if ok != 0 {
                format!("ok, {got} bytes")
            } else {
                format!("error {}", GetLastError())
            };
            println!(
                "{label}: NTFS volume data {volume_data}; ENUM_USN_DATA {enum_usn}; raw read {raw_read}"
            );
            CloseHandle(h);
        }
    }
}
