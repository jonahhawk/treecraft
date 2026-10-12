//! Times opening a volume and its first raw reads, with and without buffering. Needs
//! administrator rights.
//!
//! Usage: `cargo run --release -p tc-scan-win --example volume_read_cost -- C`

#[cfg(not(windows))]
fn main() {
    eprintln!("volume_read_cost reads raw NTFS volumes and only runs on Windows");
}

#[cfg(windows)]
fn main() {
    use std::os::windows::fs::{FileExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::time::Instant;

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER};

    let letter = std::env::args().nth(1).unwrap_or_else(|| "C".into());
    let device = format!(r"\\.\{letter}:");
    for (label, flags) in [
        ("buffered", 0),
        ("no buffering", FILE_FLAG_NO_BUFFERING),
        ("buffered again", 0),
    ] {
        let t = Instant::now();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(flags)
            .open(&device)
            .expect("open the volume (run as administrator)");
        let open = t.elapsed();

        let t = Instant::now();
        // SAFETY: zero is a valid bit pattern for this plain C struct.
        let mut data: NTFS_VOLUME_DATA_BUFFER = unsafe { std::mem::zeroed() };
        let mut got = 0u32;
        // SAFETY: the handle is open; `data` is writable for the size passed.
        let ok = unsafe {
            DeviceIoControl(
                f.as_raw_handle(),
                FSCTL_GET_NTFS_VOLUME_DATA,
                std::ptr::null(),
                0,
                (&raw mut data).cast(),
                size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "FSCTL_GET_NTFS_VOLUME_DATA failed");
        let fsctl = t.elapsed();

        let mft = data.MftStartLcn as u64 * u64::from(data.BytesPerCluster);
        // A page-aligned buffer, as unbuffered reads require.
        let mut buf = vec![0u64; 64 * 1024 / 8];
        let bytes: &mut [u8] = bytemuck_cast(&mut buf);
        let mut reads = Vec::new();
        for pos in [mft, mft, mft + 1024 * 1024] {
            let t = Instant::now();
            f.seek_read(&mut bytes[..4096], pos).expect("read");
            reads.push(t.elapsed());
        }
        println!(
            "{label:15} open {open:?}  fsctl {fsctl:?}  reads {:?} {:?} {:?}",
            reads[0], reads[1], reads[2]
        );

        // Throughput over the start of the MFT (contiguous on most volumes), 8 MiB per read.
        const CHUNK: usize = 8 << 20;
        let total = (data.MftValidDataLength as u64).min(1 << 30);
        for threads in [1usize, 4] {
            let next = std::sync::atomic::AtomicU64::new(0);
            let t = Instant::now();
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        let mut buf = AlignedBuf::new(CHUNK);
                        loop {
                            let off =
                                next.fetch_add(CHUNK as u64, std::sync::atomic::Ordering::Relaxed);
                            if off >= total {
                                break;
                            }
                            f.seek_read(buf.as_mut(), mft + off).expect("read");
                        }
                    });
                }
            });
            let secs = t.elapsed().as_secs_f64();
            println!(
                "{label:15} {threads} thread(s): {:.0} MB in {secs:.3}s = {:.0} MB/s",
                total as f64 / 1e6,
                total as f64 / 1e6 / secs
            );
        }
    }
}

/// A page-aligned buffer, as unbuffered reads require.
#[cfg(windows)]
struct AlignedBuf(*mut u8, usize);

#[cfg(windows)]
impl AlignedBuf {
    fn layout(len: usize) -> std::alloc::Layout {
        std::alloc::Layout::from_size_align(len, 4096).expect("layout")
    }
    fn new(len: usize) -> Self {
        // SAFETY: the layout has a non-zero size.
        let p = unsafe { std::alloc::alloc_zeroed(Self::layout(len)) };
        assert!(!p.is_null(), "out of memory");
        Self(p, len)
    }
    fn as_mut(&mut self) -> &mut [u8] {
        // SAFETY: the allocation is `self.1` initialised bytes owned by `self`.
        unsafe { std::slice::from_raw_parts_mut(self.0, self.1) }
    }
}

#[cfg(windows)]
impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with the same layout.
        unsafe { std::alloc::dealloc(self.0, Self::layout(self.1)) }
    }
}

#[cfg(windows)]
fn bytemuck_cast(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: u8 has no alignment or validity requirements and the length covers the same bytes.
    unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast(), words.len() * 8) }
}
