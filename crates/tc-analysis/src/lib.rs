//! Analyses over a scanned tree: extensions, age, top-N, owners, search, duplicates.

pub mod treemap;

use std::ffi::OsStr;

/// Lower-case file extension without the dot, if the name has one.
pub fn extension(name: &OsStr) -> Option<String> {
    let name = name.to_string_lossy();
    let dot = name.rfind('.')?;
    // ".bashrc" has no extension; "file." has an empty one, treated as none.
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    Some(name[dot + 1..].to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(extension(OsStr::new("a.TXT")).as_deref(), Some("txt"));
        assert_eq!(
            extension(OsStr::new("archive.tar.gz")).as_deref(),
            Some("gz")
        );
        assert_eq!(extension(OsStr::new(".bashrc")), None);
        assert_eq!(extension(OsStr::new("Makefile")), None);
        assert_eq!(extension(OsStr::new("file.")), None);
    }
}
