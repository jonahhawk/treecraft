//! Windows-specific scanners: Win32 directory enumeration, MFT reader, USN journal.
//!
//! Everything here is compiled only on Windows; on other platforms the crate is empty.

#![cfg(windows)]
