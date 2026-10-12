//! Platform scanners and volume information.
//!
//! On Windows this provides [`Win32Lister`], which reads a whole directory per system call and
//! never opens individual files (so cloud placeholders are never downloaded), and an MFT
//! scanner that reads a whole NTFS volume's file table at once when running as administrator.
//! On other platforms only the portable lister is available, and [`lister`] falls back to it.

#[cfg(windows)]
mod mft;
#[cfg(windows)]
mod win32;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tc_scan::{DirLister, PortableLister, ScanHandle, ScanOptions, SharedTree, start_scan};

#[cfg(windows)]
pub use win32::Win32Lister;

/// Which scanner to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScannerKind {
    /// The fastest scanner available for the root: MFT when elevated on local NTFS, otherwise
    /// Win32 on Windows and portable elsewhere.
    #[default]
    Auto,
    /// Read the NTFS Master File Table (Windows, administrator only).
    Mft,
    /// The Win32 lister (Windows only).
    Win32,
    /// The `std::fs` lister.
    Portable,
}

impl std::str::FromStr for ScannerKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "mft" => Ok(Self::Mft),
            "win32" => Ok(Self::Win32),
            "portable" => Ok(Self::Portable),
            _ => Err(format!(
                "unknown scanner '{s}' (expected auto, mft, win32 or portable)"
            )),
        }
    }
}

/// Creates a per-directory lister. `Auto` and `Mft` give the Win32 lister on Windows (the MFT
/// scanner is not a lister; see [`start`]), and everything falls back to portable elsewhere.
pub fn lister(kind: ScannerKind) -> Arc<dyn DirLister> {
    match kind {
        #[cfg(windows)]
        ScannerKind::Auto | ScannerKind::Mft | ScannerKind::Win32 => Arc::new(Win32Lister::new()),
        _ => Arc::new(PortableLister::new()),
    }
}

/// Whether this process runs with administrator rights. Always false off Windows.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        mft::is_elevated()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Why the MFT scanner can't scan `root`, or `None` if it can.
pub fn mft_unavailable_reason(root: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        mft::unavailable_reason(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Some("the MFT scanner is only available on Windows".into())
    }
}

/// A scan started with [`start`].
#[derive(Debug)]
pub struct Started {
    pub handle: ScanHandle,
    /// The scanner that runs: `"mft"`, `"win32"` or `"portable"`. If the MFT scanner fails,
    /// the scan continues with the Win32 lister and the error list says why.
    pub scanner: &'static str,
}

/// Starts scanning the root of `tree` with the chosen scanner on a background thread.
/// Fails only if the MFT scanner was asked for explicitly and can't be used.
pub fn start(tree: SharedTree, kind: ScannerKind, opts: ScanOptions) -> io::Result<Started> {
    let root = {
        let t = tree.read();
        t.path(t.root())
    };
    let use_mft = match kind {
        ScannerKind::Mft => match mft_unavailable_reason(&root) {
            Some(reason) => return Err(io::Error::new(io::ErrorKind::Unsupported, reason)),
            None => true,
        },
        ScannerKind::Auto => mft_unavailable_reason(&root).is_none(),
        _ => false,
    };
    if !use_mft {
        let lister = lister(kind);
        let scanner = lister.name();
        return Ok(Started {
            handle: start_scan(tree, lister, opts),
            scanner,
        });
    }
    Ok(Started {
        handle: start_mft(tree, opts, root),
        scanner: "mft",
    })
}

#[cfg(windows)]
fn start_mft(tree: SharedTree, opts: ScanOptions, root: PathBuf) -> ScanHandle {
    tc_scan::start_with(tree, move |tree, progress| {
        if let Err(e) = mft::scan(tree, progress) {
            progress.record_error(
                &root,
                &io::Error::new(
                    e.kind(),
                    format!("MFT scan failed ({e}); used the Win32 scanner instead"),
                ),
            );
            tc_scan::scan_into(tree, &Win32Lister::new(), &opts, progress);
        }
    })
}

#[cfg(not(windows))]
fn start_mft(tree: SharedTree, opts: ScanOptions, _root: PathBuf) -> ScanHandle {
    start_scan(tree, lister(ScannerKind::Portable), opts)
}

/// A mounted volume that can be scanned.
#[derive(Debug, Clone)]
pub struct Drive {
    /// Root path, e.g. `C:\` or `/`.
    pub root: PathBuf,
    /// Volume label, if any.
    pub label: String,
    /// Total and free bytes, when known.
    pub total: Option<u64>,
    pub free: Option<u64>,
}

/// Lists the volumes available for scanning.
pub fn drives() -> Vec<Drive> {
    #[cfg(windows)]
    {
        win32::drives()
    }
    #[cfg(not(windows))]
    {
        vec![Drive {
            root: PathBuf::from("/"),
            label: String::new(),
            total: None,
            free: None,
        }]
    }
}
