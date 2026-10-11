//! Platform scanners and volume information.
//!
//! On Windows this provides [`Win32Lister`], which reads a whole directory per system call and
//! never opens individual files (so cloud placeholders are never downloaded). On other platforms
//! only the portable lister is available, and [`lister`] falls back to it.

#[cfg(windows)]
mod win32;

use std::path::PathBuf;
use std::sync::Arc;

use tc_scan::{DirLister, PortableLister};

#[cfg(windows)]
pub use win32::Win32Lister;

/// Which directory lister to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScannerKind {
    /// The fastest lister available on this platform.
    #[default]
    Auto,
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
            "win32" => Ok(Self::Win32),
            "portable" => Ok(Self::Portable),
            _ => Err(format!(
                "unknown scanner '{s}' (expected auto, win32 or portable)"
            )),
        }
    }
}

/// Creates the requested lister. `Win32` falls back to portable on non-Windows platforms.
pub fn lister(kind: ScannerKind) -> Arc<dyn DirLister> {
    match kind {
        #[cfg(windows)]
        ScannerKind::Auto | ScannerKind::Win32 => Arc::new(Win32Lister::new()),
        _ => Arc::new(PortableLister::new()),
    }
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
