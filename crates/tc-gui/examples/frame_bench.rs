//! Measures GUI frame time with 1M+ rows expanded while a real scan runs (PLAN.md M3).
//!
//! Usage: `cargo run --release -p tc-gui --example frame_bench -- <folder> [threads] [low|normal]`
//!
//! The tree gets 1,000 synthetic folders of 1,000 files each, all expanded, so the table has
//! over a million rows. A real scan of `<folder>` then runs on the same shared tree while frames
//! are produced at 60 Hz (continuous redraw, as during scrolling). Every frame scrolls to a
//! different part of the table. Frame time covers egui's UI pass and
//! tessellation, i.e. all CPU work for a frame; GPU upload of a few thousand vertices is not
//! included.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use tc_gui::app::Action;
use tc_gui::tree_view::TreeView;
use tc_gui::treemap_view::TreemapView;
use tc_model::{Entry, NodeFlags};
use tc_scan::{ScanOptions, SharedTree, new_tree, start_scan};

const SYNTHETIC_DIRS: usize = 1000;
const FILES_PER_DIR: usize = 1000;
const ROW_STRIDE: f32 = 23.0; // row height plus spacing, roughly
const FRAME_BUDGET: Duration = Duration::from_micros(16_667);

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: frame_bench <folder>"),
    );
    // Optional: thread count and "low" or "normal" priority for the scan.
    let mut opts = ScanOptions::default();
    if let Some(t) = std::env::args().nth(2) {
        opts.threads = t.parse().expect("threads");
    }
    if let Some(p) = std::env::args().nth(3) {
        opts.low_priority = p == "low";
    }
    println!(
        "scan threads={} low_priority={}",
        opts.threads, opts.low_priority
    );
    let lister = || tc_scan_win::lister(tc_scan_win::ScannerKind::Auto);

    // Baseline: the same scan with no GUI attached.
    let baseline = {
        let shared = SharedTree::new(new_tree(&root).unwrap());
        start_scan(shared, lister(), opts.clone()).join()
    };

    let mut tree = new_tree(&root).unwrap();
    let root_id = tree.root();
    let dirs: Vec<Entry> = (0..SYNTHETIC_DIRS)
        .map(|i| Entry {
            name: format!("synthetic-{i:04}").into(),
            size: 0,
            allocated: 0,
            mtime: 1_700_000_000,
            flags: NodeFlags::DIR,
        })
        .collect();
    let mut dir_ids = Vec::new();
    tree.insert_listing(root_id, &dirs, &mut dir_ids);
    for (d, &id) in dir_ids.iter().enumerate() {
        let files: Vec<Entry> = (0..FILES_PER_DIR)
            .map(|f| {
                let size = ((d * 7919 + f * 104_729) % 5_000_000) as u64;
                Entry {
                    name: format!("file-{f:04}.dat").into(),
                    size,
                    allocated: size.div_ceil(4096) * 4096,
                    mtime: 1_700_000_000 + f as i64,
                    flags: NodeFlags::empty(),
                }
            })
            .collect();
        tree.insert_listing(id, &files, &mut Vec::new());
    }

    let mut view = TreeView::new();
    view.reset(root_id);
    for &id in &dir_ids {
        view.expand(id);
    }
    let mut treemap = TreemapView::new();
    let shared = SharedTree::new(tree);
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));

    let frame = |i: usize, view: &mut TreeView, treemap: &mut TreemapView| -> Duration {
        let rows = view.row_count().max(1);
        // Alternate between jumping far away and small wheel-sized steps.
        let row = if i.is_multiple_of(2) {
            (i * 7919) % rows
        } else {
            ((i - 1) * 7919) % rows + 3
        };
        view.set_scroll_offset(row as f32 * ROW_STRIDE);
        let input = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(i as f64 / 60.0),
            ..Default::default()
        };
        let start = Instant::now();
        let output = ctx.run_ui(input, |ui| {
            let tree = shared.read();
            let mut actions: Vec<Action> = Vec::new();
            egui::Panel::right("treemap")
                .default_size(440.0)
                .show(ui, |ui| {
                    treemap.ui(ui, &tree, tree.root(), None, false, &mut actions);
                });
            egui::CentralPanel::default().show(ui, |ui| {
                view.ui(ui, &tree, None, false, &mut actions);
            });
        });
        let _primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        start.elapsed()
    };

    // Warm-up: font atlas, first layout of 1M rows.
    for i in 0..5 {
        frame(i, &mut view, &mut treemap);
    }

    let handle = start_scan(shared.clone(), lister(), opts.clone());
    let progress = Arc::clone(handle.progress());
    let mut during = Vec::new();
    let mut i = 5;
    while !progress.is_finished() {
        let t = frame(i, &mut view, &mut treemap);
        during.push(t);
        // Pace at 60 Hz like a vsynced window; a slow frame just starts the next one late.
        std::thread::sleep(FRAME_BUDGET.saturating_sub(t));
        i += 1;
    }
    let stats = handle.join();
    let mut after = Vec::new();
    for _ in 0..300 {
        after.push(frame(i, &mut view, &mut treemap));
        i += 1;
    }

    println!(
        "rows in table: {} (synthetic {} + scanned {} entries)",
        view.row_count(),
        SYNTHETIC_DIRS * (FILES_PER_DIR + 1),
        stats.entries
    );
    println!(
        "scan: {:.2}s with continuous redraw vs {:.2}s with no GUI ({} entries)",
        stats.elapsed.as_secs_f64(),
        baseline.elapsed.as_secs_f64(),
        baseline.entries
    );
    report("frames during scan", &mut during);
    report("frames after scan ", &mut after);
}

fn report(label: &str, times: &mut [Duration]) {
    if times.is_empty() {
        println!("{label}: no frames");
        return;
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let pct = |p: f64| ms(times[((times.len() - 1) as f64 * p) as usize]);
    let over = times.iter().filter(|t| ms(**t) > 1000.0 / 60.0).count();
    println!(
        "{label}: {} frames  median {:.2} ms  p95 {:.2} ms  p99 {:.2} ms  max {:.2} ms  over 16.7 ms: {} ({:.1}%)",
        times.len(),
        pct(0.5),
        pct(0.95),
        pct(0.99),
        ms(times[times.len() - 1]),
        over,
        over as f64 * 100.0 / times.len() as f64
    );
}
