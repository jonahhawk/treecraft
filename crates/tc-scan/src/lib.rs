//! Scanner trait and the portable parallel directory walker.
//!
//! A scan is split into two parts:
//! - a [`DirLister`] reads one directory and describes its entries. Platforms provide faster
//!   listers (see `tc-scan-win`); [`PortableLister`] works everywhere via `std::fs`.
//! - the traversal driver in this crate walks the tree in parallel on a rayon pool, inserting each
//!   listing into a shared [`Tree`] as soon as it is read, so viewers see progressive results.

mod portable;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tc_model::{Entry, NodeId, Tree};

pub use portable::{PortableLister, unix_secs, windows_attribute_flags};

/// Reads the entries of a single directory.
pub trait DirLister: Send + Sync {
    /// Short name for display, e.g. `"portable"`.
    fn name(&self) -> &'static str;

    /// Called once before a scan of `root` starts, so the lister can inspect the volume and
    /// reset per-scan state.
    fn begin(&self, _root: &Path) {}

    /// Appends the entries of `dir` to `out`. On error, entries read before the error may
    /// already be in `out`; they are kept.
    fn list(&self, dir: &Path, out: &mut Vec<Entry>) -> io::Result<()>;
}

/// A tree shared between a running scan and its viewers.
#[derive(Debug, Clone)]
pub struct SharedTree(Arc<RwLock<Tree>>);

impl SharedTree {
    pub fn new(tree: Tree) -> Self {
        Self(Arc::new(RwLock::new(tree)))
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Tree> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, Tree> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Scan settings.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Worker threads. Directory listing is I/O bound, so more threads than cores helps.
    pub threads: usize,
    /// Run worker threads at below-normal CPU priority, so a UI in the same process stays
    /// responsive. This keeps GUI frames under 16.7 ms while scanning, but makes a scan on its
    /// own about 1.6x slower, so it is off by default and the GUI turns it on.
    /// (Windows only for now; I/O priority is unchanged.)
    pub low_priority: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            threads: (cores * 2).clamp(4, 32),
            low_priority: false,
        }
    }
}

/// A directory that could not be read.
#[derive(Debug, Clone)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

/// Live counters of a scan, safe to read from any thread.
#[derive(Debug, Default)]
pub struct ScanProgress {
    entries: AtomicU64,
    dirs: AtomicU64,
    errors: AtomicU64,
    cancelled: AtomicBool,
    finished: AtomicBool,
    error_list: Mutex<Vec<ScanError>>,
}

/// Errors beyond this many are counted but not kept.
const MAX_KEPT_ERRORS: usize = 10_000;

impl ScanProgress {
    /// Entries (files and directories) inserted so far.
    pub fn entries(&self) -> u64 {
        self.entries.load(Ordering::Relaxed)
    }
    /// Directories listed so far.
    pub fn dirs(&self) -> u64 {
        self.dirs.load(Ordering::Relaxed)
    }
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
    /// The first errors encountered (up to 10,000).
    pub fn error_list(&self) -> Vec<ScanError> {
        self.error_list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Requests the scan to stop. Directories already queued are skipped.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn record_error(&self, path: &Path, err: &io::Error) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        let mut list = self.error_list.lock().unwrap_or_else(|e| e.into_inner());
        if list.len() < MAX_KEPT_ERRORS {
            list.push(ScanError {
                path: path.to_path_buf(),
                message: err.to_string(),
            });
        }
    }
}

/// Summary of a finished scan.
#[derive(Debug, Clone)]
pub struct ScanStats {
    pub entries: u64,
    pub dirs: u64,
    pub errors: u64,
    pub elapsed: Duration,
    pub cancelled: bool,
}

/// A scan running on a background thread.
#[derive(Debug)]
pub struct ScanHandle {
    progress: Arc<ScanProgress>,
    thread: Option<JoinHandle<ScanStats>>,
}

impl ScanHandle {
    pub fn progress(&self) -> &Arc<ScanProgress> {
        &self.progress
    }

    pub fn cancel(&self) {
        self.progress.cancel();
    }

    pub fn is_finished(&self) -> bool {
        self.progress.is_finished()
    }

    /// Waits for the scan to finish.
    pub fn join(mut self) -> ScanStats {
        self.thread
            .take()
            .expect("joined twice")
            .join()
            .expect("scan thread panicked")
    }
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.progress.cancel();
            let _ = t.join();
        }
    }
}

/// Creates an empty tree for `root`, after checking that `root` is a readable directory.
/// The root path is made absolute.
pub fn new_tree(root: &Path) -> io::Result<Tree> {
    let root = std::path::absolute(root)?;
    let md = std::fs::metadata(&root)?;
    if !md.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is not a directory", root.display()),
        ));
    }
    let mut tree = Tree::new(&root);
    tree.set_mtime(
        tree.root(),
        md.modified().map(portable::unix_secs).unwrap_or(0),
    );
    Ok(tree)
}

/// Starts scanning the root of `tree` on a background thread.
pub fn start_scan(tree: SharedTree, lister: Arc<dyn DirLister>, opts: ScanOptions) -> ScanHandle {
    let progress = Arc::new(ScanProgress::default());
    let p = progress.clone();
    let thread = std::thread::Builder::new()
        .name("tc-scan".into())
        .spawn(move || run(&tree, &*lister, &opts, &p))
        .expect("failed to spawn scan thread");
    ScanHandle {
        progress,
        thread: Some(thread),
    }
}

/// Scans `root` on the calling thread and returns the finished tree.
pub fn scan(
    root: &Path,
    lister: &dyn DirLister,
    opts: &ScanOptions,
) -> io::Result<(Tree, ScanStats)> {
    let tree = SharedTree::new(new_tree(root)?);
    let progress = ScanProgress::default();
    let stats = run(&tree, lister, opts, &progress);
    let tree = Arc::try_unwrap(tree.0)
        .expect("tree still shared")
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    Ok((tree, stats))
}

struct Ctx<'a> {
    tree: &'a SharedTree,
    lister: &'a dyn DirLister,
    progress: &'a ScanProgress,
}

fn run(
    tree: &SharedTree,
    lister: &dyn DirLister,
    opts: &ScanOptions,
    progress: &ScanProgress,
) -> ScanStats {
    let start = Instant::now();
    let (root_id, root_path) = {
        let t = tree.read();
        (t.root(), t.path(t.root()))
    };
    lister.begin(&root_path);
    let ctx = Ctx {
        tree,
        lister,
        progress,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .thread_name(|i| format!("tc-scan-{i}"))
        .start_handler({
            let low = opts.low_priority;
            move |_| {
                if low {
                    lower_thread_priority();
                }
            }
        })
        .build()
        .expect("failed to build scan thread pool");
    pool.scope(|s| visit(s, &ctx, root_path, root_id));
    progress.finished.store(true, Ordering::Release);
    ScanStats {
        entries: progress.entries(),
        dirs: progress.dirs(),
        errors: progress.errors(),
        elapsed: start.elapsed(),
        cancelled: progress.is_cancelled(),
    }
}

fn visit<'s>(s: &rayon::Scope<'s>, ctx: &'s Ctx<'s>, path: PathBuf, id: NodeId) {
    if ctx.progress.is_cancelled() {
        return;
    }
    let mut entries = Vec::new();
    let result = ctx.lister.list(&path, &mut entries);
    let mut subdirs = Vec::new();
    {
        let mut tree = ctx.tree.write();
        tree.insert_listing(id, &entries, &mut subdirs);
        if result.is_err() {
            tree.mark_error(id);
        }
    }
    if let Err(e) = result {
        ctx.progress.record_error(&path, &e);
    }
    ctx.progress
        .entries
        .fetch_add(entries.len() as u64, Ordering::Relaxed);
    ctx.progress.dirs.fetch_add(1, Ordering::Relaxed);

    let dirs = entries.into_iter().filter(Entry::is_traversable_dir);
    for (entry, sub_id) in dirs.zip(subdirs) {
        let sub_path = path.join(&entry.name);
        s.spawn(move |s| visit(s, ctx, sub_path, sub_id));
    }
}

#[cfg(windows)]
fn lower_thread_priority() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    };
    // SAFETY: GetCurrentThread returns a pseudo-handle for the calling thread that is always
    // valid; SetThreadPriority only changes this thread's scheduling priority.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

#[cfg(not(windows))]
fn lower_thread_priority() {}
