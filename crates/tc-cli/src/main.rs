//! `treecraft` command-line interface.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use tc_model::{NodeFlags, NodeId, SortKey, Tree, format_count, format_size};
use tc_scan::{ScanOptions, ScanStats, SharedTree, new_tree};
use tc_scan_win::ScannerKind;

#[derive(Debug, Parser)]
#[command(name = "treecraft", version, about = "Disk space analyzer")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scan a directory tree and print the largest items.
    Scan(ScanArgs),
}

#[derive(Debug, clap::Args)]
struct ScanArgs {
    /// Root directory to scan.
    root: PathBuf,
    /// Scanner: auto, mft (Windows, administrator), win32 or portable. Auto uses the MFT when
    /// running as administrator on a local NTFS drive.
    #[arg(long, default_value = "auto")]
    scanner: ScannerKind,
    /// How many levels below the root to print.
    #[arg(long, short, default_value_t = 1)]
    depth: usize,
    /// Show at most this many children per directory (0 = all).
    #[arg(long, short, default_value_t = 20)]
    top: usize,
    /// Sort order.
    #[arg(long, value_enum, default_value_t = Sort::Size)]
    sort: Sort,
    /// Show allocated (on-disk) size instead of logical size.
    #[arg(long)]
    allocated: bool,
    /// Print JSON instead of text.
    #[arg(long)]
    json: bool,
    /// Worker threads (default: twice the CPU count, 4 to 32).
    #[arg(long)]
    threads: Option<usize>,
    /// Run scan threads at below-normal priority (slower, but leaves the CPU to other programs).
    #[arg(long)]
    low_priority: bool,
    /// List directories that could not be read.
    #[arg(long)]
    errors: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Sort {
    Size,
    Allocated,
    Name,
    Files,
    Modified,
}

impl From<Sort> for SortKey {
    fn from(s: Sort) -> Self {
        match s {
            Sort::Size => SortKey::Size,
            Sort::Allocated => SortKey::Allocated,
            Sort::Name => SortKey::Name,
            Sort::Files => SortKey::Files,
            Sort::Modified => SortKey::Modified,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan(args) => run_scan(&args),
    }
}

fn run_scan(args: &ScanArgs) -> anyhow::Result<()> {
    let tree =
        new_tree(&args.root).with_context(|| format!("cannot scan {}", args.root.display()))?;
    let shared = SharedTree::new(tree);
    let mut opts = ScanOptions::default();
    if let Some(t) = args.threads {
        opts.threads = t;
    }
    opts.low_priority = args.low_priority;
    let started = tc_scan_win::start(shared.clone(), args.scanner, opts)
        .with_context(|| format!("cannot use the {:?} scanner", args.scanner))?;
    let lister_name = started.scanner;
    let handle = started.handle;

    let show_progress = std::io::stderr().is_terminal();
    let progress = handle.progress().clone();
    while !handle.is_finished() {
        std::thread::sleep(Duration::from_millis(100));
        if show_progress {
            eprint!(
                "\rScanning... {} entries, {} errors",
                format_count(progress.entries()),
                progress.errors()
            );
            let _ = std::io::stderr().flush();
        }
    }
    if show_progress {
        eprint!("\r\x1b[2K");
    }
    let stats = handle.join();
    let errors = progress.error_list();
    let tree = shared.read();

    let sort = SortKey::from(if args.allocated && matches!(args.sort, Sort::Size) {
        Sort::Allocated
    } else {
        args.sort
    });
    if args.json {
        let mut doc = node_json(&tree, tree.root(), args.depth, args.top, sort);
        doc["scan"] = json!({
            "scanner": lister_name,
            "entries": stats.entries,
            "errors": stats.errors,
            "seconds": stats.elapsed.as_secs_f64(),
        });
        if args.errors {
            doc["scan"]["error_list"] = errors
                .iter()
                .map(|e| json!({"path": e.path, "message": e.message}))
                .collect::<Value>();
        }
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        print_text(&tree, args, sort, &stats, lister_name);
        if args.errors {
            for e in &errors {
                println!("error: {}: {}", e.path.display(), e.message);
            }
        }
    }
    Ok(())
}

fn size_of(tree: &Tree, id: NodeId, allocated: bool) -> u64 {
    let n = tree.node(id);
    if allocated { n.allocated() } else { n.size() }
}

fn print_text(tree: &Tree, args: &ScanArgs, sort: SortKey, stats: &ScanStats, lister: &str) {
    let root = tree.root();
    let n = tree.node(root);
    println!(
        "{}  {}  ({} files, {} folders)",
        tree.path(root).display(),
        format_size(size_of(tree, root, args.allocated)),
        format_count(n.files().into()),
        format_count(n.dirs().into()),
    );
    print_children(tree, root, 1, args, sort);
    println!(
        "\nScanned {} entries in {:.2}s with the {lister} scanner{}{}.",
        format_count(stats.entries),
        stats.elapsed.as_secs_f64(),
        if stats.errors > 0 {
            format!(", {} unreadable directories", stats.errors)
        } else {
            String::new()
        },
        if stats.errors > 0 && !args.errors {
            " (use --errors to list them)"
        } else {
            ""
        },
    );
}

fn print_children(tree: &Tree, id: NodeId, level: usize, args: &ScanArgs, sort: SortKey) {
    if level > args.depth {
        return;
    }
    let parent_size = size_of(tree, id, args.allocated);
    let children = tree.sorted_children(id, sort, false);
    let shown = if args.top == 0 {
        children.len()
    } else {
        children.len().min(args.top)
    };
    for &c in &children[..shown] {
        let size = size_of(tree, c, args.allocated);
        let pct = if parent_size == 0 {
            0.0
        } else {
            size as f64 * 100.0 / parent_size as f64
        };
        let node = tree.node(c);
        let suffix = if node.flags().contains(NodeFlags::LINK) {
            " -> (link, not followed)"
        } else if node.is_dir() {
            std::path::MAIN_SEPARATOR_STR
        } else {
            ""
        };
        println!(
            "{indent}{pct:5.1}%  {size:>10}  {name}{suffix}",
            indent = "  ".repeat(level),
            size = format_size(size),
            name = tree.name(c).to_string_lossy(),
        );
        if node.is_dir() {
            print_children(tree, c, level + 1, args, sort);
        }
    }
    if children.len() > shown {
        let rest: u64 = children[shown..]
            .iter()
            .map(|&c| size_of(tree, c, args.allocated))
            .sum();
        println!(
            "{}         {:>10}  ({} more items)",
            "  ".repeat(level),
            format_size(rest),
            children.len() - shown
        );
    }
}

fn node_json(tree: &Tree, id: NodeId, depth: usize, top: usize, sort: SortKey) -> Value {
    let n = tree.node(id);
    let name = if id == tree.root() {
        tree.path(id)
    } else {
        PathBuf::from(tree.name(id))
    };
    let mut v = json!({
        "name": name.to_string_lossy(),
        "dir": n.is_dir(),
        "size": n.size(),
        "allocated": n.allocated(),
        "files": n.files(),
        "dirs": n.dirs(),
        "mtime": n.mtime(),
    });
    let flags = n.flags();
    for (flag, key) in [
        (NodeFlags::LINK, "link"),
        (NodeFlags::CLOUD, "cloud"),
        (NodeFlags::ERROR, "error"),
        (NodeFlags::HARDLINK_DUP, "hardlink_dup"),
    ] {
        if flags.contains(flag) {
            v[key] = json!(true);
        }
    }
    if n.is_dir() && depth > 0 {
        let children = tree.sorted_children(id, sort, false);
        let take = if top == 0 { children.len() } else { top };
        v["children"] = children
            .iter()
            .take(take)
            .map(|&c| node_json(tree, c, depth - 1, top, sort))
            .collect();
    }
    v
}
