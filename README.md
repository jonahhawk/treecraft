# Treecraft

A fast disk space analyzer for Windows (with Linux and macOS support), written in Rust.

Status: early development. See [PLAN.md](PLAN.md) for the roadmap and [spec/](spec/) for the
feature specs.

## Layout

| Path | Purpose |
|---|---|
| `crates/tc-model` | Arena-based file tree and size aggregation |
| `crates/tc-scan` | Scanner trait and portable parallel walker |
| `crates/tc-scan-win` | Windows scanners (Win32 enumeration, MFT, USN) |
| `crates/tc-ntfs` | Pure NTFS structure parser |
| `crates/tc-analysis` | Extensions, age, top-N, search, duplicates |
| `crates/tc-snapshot` | Snapshot format and diffing |
| `crates/tc-export` | CSV / JSON / XLSX / HTML export |
| `crates/tc-cli` | `treecraft` command-line tool |
| `crates/tc-gui` | `treecraft-gui` desktop app |
| `tools/gen-tree` | Deterministic test-tree generator |

## Building

```
cargo build --workspace
cargo test --workspace
```

Generate a test tree:

```
cargo run -p gen-tree -- scratch/tree1 --seed 1 --depth 4 --fanout 4
```

## License

MIT. See [LICENSE](LICENSE).
