# Treecraft — Clean-Room Disk Space Analyzer in Rust

Goal: a Windows-first disk space analyzer with roughly the capabilities of TreeSize Pro
(JAM Software), built from scratch in Rust under a clean-room process. It runs on Linux and macOS
too, but some features are Windows-only.

**Progress (2026-10-11):** M0 is done, and so is the MVP: M1, M2, M3 and the treemap part of M4.
Measured on this machine (8 cores, SSD, warm cache) on a 897k-entry profile folder:

- Scan: 3.2–3.3 s with either scanner at normal priority (about 275k entries/s); throughput
  levels off at 8 threads. Benchmarks: `tc-scan-win/examples/scan_bench.rs` and `open_cost.rs`.
- Hard links count once on NTFS (Win32 scanner, by file ID): the folder totals 124.7 GB
  instead of 128.8 GB with the portable scanner, matching `fsutil hardlink list`.
- Windows edge-case suite (§4): `tc-scan-win/tests/edge_cases.rs`, 12 tests, all passing. The
  symlink case needs the symlink privilege and skips itself without it.
- M3: `tc-gui/examples/frame_bench.rs` expands 1,001,000 synthetic rows, runs a real scan into
  the same tree and renders at 60 Hz with a scroll jump every frame. With the GUI's
  below-normal scan priority: median 5.6 ms, p99 15.7 ms, 0.6% of frames over 16.7 ms during
  the scan, none after. At normal priority 66% of frames miss the budget because the scan
  saturates every core, so the GUI scans at below-normal priority and the CLI at normal
  (`--low-priority` opts in). Below-normal makes a scan on its own about 1.6× slower.

**Deviation from M2's exit criterion (≥3× faster than the portable walker): not met, and not
reachable by any scanner that opens each directory.** On Windows, `std::fs::read_dir` already
reads directory listings without opening the files in them, so both scanners pay the same
per-directory open. Single-threaded, opening the 96,728 directories alone takes 7.80 s of the
13.96 s full scan, so even a listing that cost nothing would be at most 1.8× faster. The 3×
goal moves to M5: the MFT scanner reads the file table instead of opening directories (it needs
administrator rights). The Win32 scanner is still the default because it reports real
allocated sizes and counts hard links once. Next up: M5, or M6/M7 if features matter more than
speed.

---

## 0. Clean-room ground rules (do this first)

"Clean room" means we copy behaviour, never expression. To keep that defensible:

1. **Allowed sources:** public marketing pages, public user manuals and help pages, published
   reviews, screenshots, and watching the shipped product from the outside (what it shows, how
   fast, which columns it has). Microsoft's own docs on NTFS, Win32 and USN, plus the public
   NTFS format write-ups (e.g. the Linux-NTFS project docs).
2. **Forbidden:** decompiling or disassembling TreeSize binaries, reading its config or report
   formats byte by byte to copy them, reusing its icons, images, UI text, help text, or report
   templates, and using its name or trade dress. Its EULA probably forbids reverse engineering
   anyway.
3. **Two roles, where possible:** a *spec author* who may use the product and writes
   `spec/*.md` in our own words, and *implementers* who build only from `spec/` and public
   platform docs. If only one person is doing the work, keep the spec and code phases apart in
   time and in git history.
4. **Provenance log:** `spec/SOURCES.md` lists every external source used, with URL and date.
   Commit early and often so the history shows independent development.
5. **Branding:** our name is "Treecraft", with our own icons, colours and wording. Don't market
   it as "a TreeSize clone". Say "disk space analyzer" instead.
6. **Dependencies:** only crates with OSI licenses (MIT, Apache-2.0, BSD, MPL). Enforce this with
   `cargo-deny`.

---

## 1. Feature inventory (target spec, our own words)

Priority: **P0** = MVP, **P1** = v1.0, **P2** = later.

| Area | Feature | Pri |
|---|---|---|
| Scanning | Scan a drive, folder or UNC share recursively, multithreaded | P0 |
| | Live, progressive results while the scan runs; cancel/pause | P0 |
| | Track both logical size and allocated (on-disk) size; configurable cluster-size rounding | P0 |
| | Correct handling of hard links (count once), symlinks/junctions (don't follow by default, show as such), sparse/compressed files, alternate data streams | P0/P1 |
| | Cloud placeholders (OneDrive and similar): report without triggering hydration | P0 |
| | NTFS fast path: read the MFT directly when elevated | P1 |
| | Incremental refresh of a subtree; USN-journal-based live update | P1/P2 |
| | Long paths (>260), permission-denied entries reported, not fatal | P0 |
| Tree view | Expandable tree with Size, Allocated, % of parent (with bar), Files, Folders, Last modified, Last accessed, Owner, Attributes | P0 (Owner P1) |
| | Sort by any column; size units auto/KB/MB/GB; toggle size vs allocated | P0 |
| | Filter (include/exclude by pattern, size, date) | P1 |
| Visuals | Treemap (squarified, cushion shading optional), drill-down | P0 |
| | Pie/bar chart of children; top-N largest files; extension breakdown; file age histogram; per-owner usage | P1 |
| Search | File search over the scanned tree: name glob/regex, size, dates, extension, owner, attributes | P1 |
| | Duplicate finder (size, then partial hash, then full hash), with hard-link awareness | P1 |
| | "Old/temporary/large files" preset searches | P2 |
| History | Save a scan snapshot; reopen it offline; compare two snapshots (growth per folder) | P1 |
| | Trend history across many snapshots of the same root | P2 |
| Export | CSV, JSON, XLSX, standalone HTML report; copy-to-clipboard | P1 |
| Actions | Open in Explorer, delete (to Recycle Bin), move, compress (NTFS), properties | P1 |
| | Explorer context-menu entry "Analyze with Treecraft" | P2 |
| Automation | CLI: scan to snapshot/report, exit codes, thresholds | P0 (basic) / P1 |
| | Scheduled scans via Windows Task Scheduler wrapper | P2 |
| Platform | Windows 10/11 x64 and ARM64 first-class; Linux/macOS portable scanner and GUI | P0 Win / P1 others |

Out of scope for now: SharePoint/cloud-API scanning, server/agent mode, mobile.

---

## 2. Architecture

### Workspace layout

```
treecraft/
  Cargo.toml                 # [workspace]
  spec/                      # clean-room specs + SOURCES.md
  crates/
    tc-model/                # arena tree, string interner, aggregates (no I/O)
    tc-scan/                 # Scanner trait + portable walker
    tc-scan-win/             # Win32 enumerator, MFT reader, USN journal (cfg(windows))
    tc-ntfs/                 # pure NTFS on-disk structure parser (fuzzable, no OS calls)
    tc-analysis/             # extensions, age buckets, top-N, owners, duplicates, search
    tc-snapshot/             # binary snapshot format, diff
    tc-export/               # csv/json/xlsx/html
    tc-cli/                  # `treecraft` binary
    tc-gui/                  # `treecraft-gui` binary
  tools/
    gen-tree/                # synthetic tree generator for tests/benchmarks
  fuzz/                      # cargo-fuzz targets for tc-ntfs, tc-snapshot
```

### Data model (`tc-model`): the most important design decision

Scans can reach 10M+ entries, so a `Box`/`Rc`-per-node tree is ruled out. Use a flat arena:

```rust
pub struct NodeId(u32);

#[repr(C)]
pub struct Node {            // target ≤ 48 bytes
    parent: NodeId,
    first_child: NodeId,
    next_sibling: NodeId,
    name: NameRef,           // u32 offset into a single name arena (UTF-16→UTF-8 / WTF-8 for lossless Windows names)
    size: u64,               // logical; for dirs = subtree aggregate
    allocated: u64,          // on-disk; for dirs = subtree aggregate
    mtime: i64,
    files: u32,              // subtree file count (dirs)
    flags: NodeFlags,        // dir, symlink, junction, hardlink, sparse, compressed, cloud, error, …
}
pub struct Tree { nodes: Vec<Node>, names: NameArena, extra: SideTables /* owner, atime, ctime, ADS, file-id */ }
```

- Rarely-used columns (owner SID, atime, ctime, file ID for hard-link dedup) live in side tables
  indexed by `NodeId`, so the hot struct stays small.
- Names are stored as WTF-8 so unpaired-surrogate Windows names round-trip without loss.
- Aggregation is a bottom-up pass, plus incremental propagation up the parent chain when a
  subtree is rescanned.
- Sorting the children of the expanded node is done lazily by the view and never stored in the tree.
- A memory cap option drops per-file nodes below depth N, keeping only aggregates.
  Budget: about 60 B/entry, so 10M files ≈ 600 MB.

### Scanner abstraction (`tc-scan`)

```rust
pub trait Scanner: Send {
    fn scan(&self, root: &Path, opts: &ScanOptions, sink: &dyn ScanSink, cancel: &CancelToken) -> Result<ScanStats>;
}
pub trait ScanSink: Sync { fn on_dir(&self, batch: DirBatch); fn on_error(&self, path: &Path, err: ScanError); }
```

Three implementations, picked automatically and overridable by the user:

1. **Portable walker.** Work-stealing parallel directory traversal (rayon or a
   crossbeam-deque pool) using `std::fs` / `openat`-style calls on Unix. Batches of entries go
   to a single builder thread that owns the `Tree`, so there is no lock on the arena.
2. **Win32 fast enumerator** (`tc-scan-win`). Open each directory with `CreateFileW`
   (`FILE_FLAG_BACKUP_SEMANTICS`) and call `GetFileInformationByHandleEx(FileIdExtdDirectoryInfo)`
   with large buffers. One call per directory returns sizes, allocation size, attributes,
   timestamps, file ID and reparse tag, with no per-file open. This also means cloud placeholders
   never get hydrated. Uses `\\?\` paths throughout. Works without elevation and on network shares.
3. **MFT reader** (elevated, local NTFS only). Open `\\.\C:`, read the boot sector, locate `$MFT`,
   read its own data runs, then stream all FILE records in large sequential reads. Parse
   `$STANDARD_INFORMATION`, `$FILE_NAME` (pick the Win32 namespace), `$DATA` (size, allocated
   size, ADS) and `$ATTRIBUTE_LIST`. Rebuild parent links from `$FILE_NAME` parent refs. Hard
   links show up as multiple `$FILE_NAME` attributes on one record. The parsing lives in
   `tc-ntfs` as pure functions over byte slices so it can be fuzzed and tested on captured
   images. Fallback to option 2 on any failure.

Refresh: rescan a subtree with option 2. Live mode (P2) reads the USN journal
(`FSCTL_READ_USN_JOURNAL`) and applies deltas.

### Analysis (`tc-analysis`)

All analysis runs over the in-memory `Tree` on a background thread and returns view models.

- Extension stats, age histogram, owner stats, and top-N (bounded heap) are single linear passes.
- Search compiles filters into a predicate and runs a parallel scan over the arena.
- Duplicates: group by size, skip groups of 1 and files sharing a file ID (hard links), hash the
  first and last 64 KiB with xxh3, then hash survivors in full with BLAKE3. I/O is throttled and
  cancellable. Cloud placeholders are skipped unless the user opts in.

### Snapshots (`tc-snapshot`)

- Format: magic, version, header (root, scan time, scanner, options), then the zstd-compressed
  node array and name arena, written with `bytemuck`/explicit little-endian so loading is
  near-instant. The format is versioned and has a fuzz target.
- Diff: match nodes by path across two trees and produce per-node delta size/count. The GUI shows
  this as an extra "Change" column and as a "grew most" list.
- History (P2): a SQLite index (`rusqlite`) of per-folder aggregates per snapshot, used for trend charts.

### Export (`tc-export`)
CSV/JSON (`serde`), XLSX (`rust_xlsxwriter`), HTML (one self-contained file built from our own
template with an inline SVG treemap). Exports respect the current filter, depth and columns.

### GUI (`tc-gui`)

**Recommendation: `egui`/`eframe`** for v1.
- Immediate mode suits a view that changes constantly while a scan runs.
- Virtualised rows (`egui_extras::TableBuilder` + custom tree rows) handle millions of entries.
- Treemaps and charts draw straight to the painter, with no extra charting dependency.
- It is cross-platform and ships as one exe.
- Trade-off: it doesn't look native. If native Windows look becomes a requirement, the model,
  scan and analysis crates are UI-agnostic, so a Slint or WinUI front end can be swapped in later.
  Record this in `spec/adr/0001-gui-toolkit.md`.

Layout: a toolbar with the drive picker, path box, scan/stop and Size/Allocated toggle. Left is
the tree table. Right is tabbed: Treemap, Chart, Extensions, Age, Top files, Search, Duplicates,
Compare. The status bar shows scan progress (entries/s, errors, elapsed).

Threads: the UI thread renders only. The scan, analysis and I/O threads talk to it over
channels, and the UI reads a `Arc<RwLock<Tree>>` or, better, gets periodic immutable published
generations, so rendering never blocks on the scanner.

Windows integration (via the `windows` crate): Recycle Bin delete (`IFileOperation`), "open in
Explorer", a shell properties dialog, and an elevation relaunch button ("Restart as admin for
faster scan"). Explorer context menu via registry verbs (P2).

### CLI (`tc-cli`)

```
treecraft scan C:\ --out c.tcs [--mft|--win32|--portable] [--max-depth N]
treecraft report c.tcs --format xlsx|csv|json|html --top 100 --depth 3 -o report.xlsx
treecraft diff old.tcs new.tcs --format csv
treecraft dupes D:\Photos --min-size 1MB --format json
```
Uses `clap`. Exit codes are non-zero on threshold breach (`--fail-if-larger 500GB`) so it can
be used in monitoring scripts.

---

## 3. Milestones

Durations assume one experienced Rust developer.

| # | Milestone | Contents | Exit criteria |
|---|---|---|---|
| M0 | Setup & spec (1 wk) | Install the toolchain (`rustup`, MSVC target). Workspace, CI (GitHub Actions on Windows/Linux/macOS: fmt, clippy `-D warnings`, test, `cargo-deny`). Write `spec/` feature docs and ADRs. Build `gen-tree`. | CI green; spec reviewed |
| M1 | Model + portable scan + CLI (2–3 wk) | `tc-model` arena, aggregation, portable parallel walker, `treecraft scan` and text/JSON output | Totals match `du -sb` / PowerShell `Measure-Object` on generated trees; 1M-entry tree scanned with <80 B/entry RSS |
| M2 | Windows enumerator (2 wk) | `FileIdExtdDirectoryInfo` scanner, allocated size, hard-link dedup by file ID, reparse points, cloud placeholders untouched, long paths, error capture | Passes the Windows edge-case suite (§4); ~~≥3× faster than the portable walker on C:\~~ (at most 1.8× is possible while opening directories; moved to M5, see Progress) |
| M3 | GUI MVP (3–4 wk) | eframe app, live tree table, sorting, % bars, unit switching, drive picker, cancel, open-in-Explorer, delete to Recycle Bin | Smooth (≥60 fps) scrolling at 1M rows while a scan is running |
| M4 | Visualisations (2–3 wk) | Squarified treemap with drill-down and hover, pie/bar chart, extensions, age histogram, top-N | Treemap of 1M files lays out in <200 ms |
| M5 | MFT scanner (3–4 wk) | `tc-ntfs` parser + fuzzing, raw volume reader, elevation flow, automatic fallback | Results identical to the M2 scanner on test VHDs; C:\ full scan well under 10 s on SSD and ≥3× faster than the M2 scanner; 24 h fuzz run with no crashes |
| M6 | Snapshots, compare, export (2–3 wk) | `.tcs` format, open offline, diff view, CSV/JSON/XLSX/HTML export, `report`/`diff` CLI | Round-trip test: scan, save, load gives an identical tree; exports open cleanly in Excel/LibreOffice |
| M7 | Search & duplicates (2–3 wk) | Filter language/UI, duplicate finder, bulk actions on results | Duplicate finder matches a reference `fdupes` run on test data |
| M8 | Hardening & release (2 wk) | Perf pass, memory caps, settings persistence, crash reporting (local logs), MSI via `cargo-wix`, code signing, winget manifest, docs | v1.0 tagged |
| M9+ | P2 backlog | USN live updates, history/trends, owner stats, Explorer integration, scheduled scans, i18n, ARM64 builds | — |

Total to v1.0: roughly 20–26 weeks for one developer. M4 and M5 can run in parallel with two people.

---

## 4. Testing strategy

- **Unit and property tests** (`proptest`): aggregation invariants (parent = sum of children),
  incremental update equals full recompute, and snapshot round-trips.
- **Golden scans:** `gen-tree` builds deterministic trees (depth, fan-out, size distribution,
  unicode names, long paths). Every scanner must produce the same tree.
- **Windows edge-case suite:** a script creates a VHDX with `diskpart`/`New-VHD`, formats it NTFS,
  and fills it with hard links, junction loops, symlinks to outside the root, ADS, sparse files,
  NTFS-compressed files, files with denied ACLs, more than 260 chars of path, names with
  unpaired surrogates, 0-byte files, a fragmented `$MFT` (many files created and deleted), and
  `$ATTRIBUTE_LIST` overflow (a heavily fragmented file). It runs on a self-hosted or elevated CI runner.
- **Fuzzing:** `cargo-fuzz` targets for FILE record parsing, data-run decoding and snapshot loading.
- **Benchmarks:** `criterion` micro-benchmarks plus a `scan-bench` harness that records
  entries/sec and peak RSS per scanner, tracked over time.
- **GUI:** keep the logic in testable view-model structs. Smoke-test that the app launches,
  scans a generated tree and exits, in CI.

---

## 5. Key crates

`windows` (Win32/COM), `rayon`, `crossbeam`, `clap`, `serde`/`serde_json`, `csv`,
`rust_xlsxwriter`, `zstd`, `bytemuck`, `xxhash-rust`, `blake3`, `globset`, `regex`,
`eframe`/`egui`/`egui_extras`, `rusqlite` (P2), `tracing`, `anyhow`/`thiserror`, `proptest`,
`criterion`. Dev tools: `cargo-deny`, `cargo-fuzz`, `cargo-wix`.

---

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| MFT parsing bugs corrupt results | Parser is pure and fuzzed; differential tests against the Win32 scanner; automatic fallback on any inconsistency |
| MFT needs admin | Win32 enumerator is the default and still fast; one-click elevated relaunch |
| Accidental hydration of cloud files | Never open file handles during a scan; check `RECALL_ON_DATA_ACCESS`/`OFFLINE` attributes; duplicates skip placeholders by default |
| Memory on huge volumes | Compact arena, side tables, optional depth cap, snapshot-backed lazy loading later |
| Junction/symlink cycles and double counting | Don't follow reparse points by default; visited set by (volume serial, file ID) when following |
| egui doesn't feel native | UI isolated behind view models; ADR records the swap path |
| IP/legal challenge | Clean-room process (§0), provenance log, distinct branding, no copied assets or text |
| Destructive actions (delete/move) | Recycle Bin by default, confirmation listing totals, dry-run in CLI, never act on paths outside the scanned root |

---

## 7. Immediate next steps

1. Install Rust: `winget install Rustlang.Rustup`, then `rustup default stable-x86_64-pc-windows-msvc`.
   Also install the VS Build Tools C++ workload if it isn't already present.
2. `git init`, create the workspace skeleton and CI.
3. Write `spec/SOURCES.md`, `spec/features.md` (expanding §1) and ADR-0001 (GUI) and ADR-0002 (data model).
4. Start M1: `tc-model` arena with aggregation tests, then the portable walker.
