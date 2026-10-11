# ADR 0001: GUI toolkit

- Status: Accepted
- Date: 2026-10-11

## Context

The GUI has to show a tree table with millions of rows that changes many times a second while a
scan runs, plus a treemap and charts. It must ship as a single executable on Windows, and should
also run on Linux and macOS.

Options considered:

| Option | For | Against |
|---|---|---|
| egui / eframe | Immediate mode fits constantly changing data; easy virtualised rows; custom painting for treemaps; one exe; pure Rust | Not native-looking; accessibility weaker than native toolkits |
| Slint | Declarative, good performance, looks closer to native | Licence (GPL or royalty-free with attribution) needs care; less flexible custom painting |
| Tauri + web UI | Rich UI ecosystem | Two languages; IPC cost for large trees; WebView2 dependency |
| Native Win32 / WinUI via `windows` crate | Fully native look | Windows-only; much more code; WinUI from Rust is immature |
| iced | Pure Rust, Elm-style | Large virtualised tables are harder; API still churning |

## Decision

Use **egui/eframe** for v1.

## Consequences

- The GUI crate only depends on view-model types from the core crates. Scanning, analysis and
  storage don't know about egui, so another front end can replace it later.
- Treemap and charts are drawn with egui's painter; no charting dependency.
- If a native Windows look becomes a requirement, revisit with a WinUI or Slint front end.
