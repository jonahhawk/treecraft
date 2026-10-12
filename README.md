# Treecraft

A fast disk space analyzer for Windows (with Linux and macOS support), written in Rust.

Status: MVP. Scans folders and drives in parallel, shows an expandable tree with sizes and
percentages, a treemap, and lets you reveal or delete items. See [PLAN.md](PLAN.md) for the
roadmap and [spec/](spec/) for the feature specs.

## Using it

Desktop app:

```
cargo run --release -p tc-gui                    # pick a drive or type a folder
cargo run --release -p tc-gui -- D:\projects     # scan a folder on startup
```

- Click a folder to show it in the treemap; double-click (or click the arrow) to expand it.
- In the treemap, click to find an item in the tree, double-click to zoom into a folder, and use
  ⬆ Up to zoom out.
- Right-click for Show in file manager, Copy path and Delete to Recycle Bin.
- Switch between logical Size and Allocated (on-disk) size in the toolbar.

Command line:

```
treecraft scan C:\Users --depth 2 --top 10
treecraft scan D:\ --json --depth 3 > report.json
treecraft scan . --scanner portable --allocated --errors
treecraft scan C:\ --low-priority                # leave the CPU to other programs
```

## Layout

| Path | Purpose |
|---|---|
| `crates/tc-model` | Arena-based file tree and size aggregation |
| `crates/tc-scan` | Scanner trait and portable parallel walker |
| `crates/tc-scan-win` | Windows scanners (Win32 enumeration; MFT and USN later) and drive list |
| `crates/tc-ntfs` | Pure NTFS structure parser (milestone M5) |
| `crates/tc-analysis` | Treemap layout; later extensions, age, top-N, search, duplicates |
| `crates/tc-snapshot` | Snapshot format and diffing (milestone M6) |
| `crates/tc-export` | CSV / JSON / XLSX / HTML export (milestone M6) |
| `crates/tc-cli` | `treecraft` command-line tool |
| `crates/tc-gui` | `treecraft-gui` desktop app (egui) |
| `tools/gen-tree` | Deterministic test-tree generator |

## Building

```
cargo build --workspace --release
cargo test --workspace
```

Generate a test tree:

```
cargo run -p gen-tree -- scratch/tree1 --seed 1 --depth 4 --fanout 4
```

## Benchmarks

```
cargo run --release -p tc-scan-win --example scan_bench -- C:\Users   # portable vs Win32 scanner
cargo run --release -p tc-scan-win --example open_cost -- C:\Users    # cost of opening directories
cargo run --release -p tc-gui --example frame_bench -- C:\Users 16 low  # GUI frames, 1M rows, during a scan
```

Results on the development machine are in [PLAN.md](PLAN.md) under Progress.

## License

MIT. See [LICENSE](LICENSE).
