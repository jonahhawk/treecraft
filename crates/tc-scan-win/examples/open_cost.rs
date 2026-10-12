//! Measures how scan time splits between opening directories and listing them.
//!
//! Usage: `cargo run --release -p tc-scan-win --example open_cost -- <root>`

#[cfg(not(windows))]
fn main() {
    eprintln!("open_cost measures Win32 directory handles and only runs on Windows");
}

#[cfg(windows)]
fn main() {
    use std::path::PathBuf;
    use std::time::Instant;

    use tc_scan::{DirLister, ScanOptions, scan};

    let root = PathBuf::from(std::env::args().nth(1).expect("usage: open_cost <root>"));
    let lister = tc_scan_win::Win32Lister::new();
    let (tree, _) = scan(&root, &lister, &ScanOptions::default()).expect("scan failed");
    let mut dirs = Vec::new();
    let mut stack = vec![tree.root()];
    while let Some(id) = stack.pop() {
        let n = tree.node(id);
        if n.is_dir() && !n.flags().contains(tc_model::NodeFlags::LINK) {
            dirs.push(tree.path(id));
            stack.extend(tree.children(id));
        }
    }
    println!("{} directories", dirs.len());

    // Single-threaded, so the numbers are per-call costs without contention.
    let t = Instant::now();
    for d in &dirs {
        let _ = std::fs::File::options()
            .read(true)
            .custom_flags_backup_semantics()
            .open(d);
    }
    let open = t.elapsed();
    let t = Instant::now();
    let mut out = Vec::new();
    for d in &dirs {
        out.clear();
        let _ = lister.list(d, &mut out);
    }
    let open_and_list = t.elapsed();
    println!("open only:     {:.2}s", open.as_secs_f64());
    println!("open and list: {:.2}s", open_and_list.as_secs_f64());
}

#[cfg(windows)]
trait BackupSemantics {
    fn custom_flags_backup_semantics(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl BackupSemantics for std::fs::OpenOptions {
    fn custom_flags_backup_semantics(&mut self) -> &mut Self {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS is required to open directories.
        self.custom_flags(0x0200_0000)
    }
}
