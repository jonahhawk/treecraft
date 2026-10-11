# Feature specification

Priorities: **P0** = MVP, **P1** = v1.0, **P2** = later. Each feature lists acceptance criteria
that tests should check. Sizes are in bytes unless stated.

## 1. Scanning

### 1.1 Recursive scan (P0)
Scan a drive root, folder or UNC path and build a tree of every directory and file below it.
- Directory size = sum of all file sizes in its subtree.
- Files, folders and errors are counted per subtree.
- Results appear progressively while the scan runs; the user can cancel at any time and keep the
  partial result.
- *Accept:* totals for a `gen-tree` tree match its manifest exactly.

### 1.2 Logical vs allocated size (P0)
Track both the logical file size and the space allocated on disk.
- Allocated size comes from the file system where available. Otherwise it is the logical size
  rounded up to the cluster size.
- The UI and reports can switch between the two.
- *Accept:* NTFS-compressed and sparse files show allocated < logical. Tiny files show
  allocated = 0 when they're stored inside the MFT record (MFT scanner only).

### 1.3 Special entries (P0/P1)
- **Hard links:** a file with several names counts once toward totals, under the first path
  seen. Other names are flagged and count 0.
- **Symlinks and junctions:** not followed by default; shown with their own flag and size 0. When
  following is enabled, loops are detected by (volume serial, file ID).
- **Alternate data streams (P1):** counted toward the file's size; listed in file details.
- **Cloud placeholders:** reported with their logical size and allocated size as stored locally.
  Scanning must never trigger a download (no file handles are opened on files).
- **Long paths:** paths over 260 characters work.
- **Access denied:** the entry is recorded with an error flag; the scan continues.
- *Accept:* each case has a test on the Windows edge-case volume (PLAN.md §4).

### 1.4 Fast NTFS scan (P1)
When running elevated on a local NTFS volume, read the MFT directly instead of walking
directories. Falls back to normal scanning on any error.
- *Accept:* produces the same tree as the normal scanner on the test volumes.

### 1.5 Refresh (P1) and live updates (P2)
Rescan a single subtree and update totals up to the root. P2: follow the USN journal to apply
changes without rescanning.

## 2. Tree view (P0)
- Columns: Name, Size, Allocated, % of parent (with bar), Files, Folders, Last modified.
  P1 adds Last accessed, Owner, Attributes, and Change (when comparing snapshots).
- Sort by any column; the default is size descending.
- Size units: automatic, bytes, KB, MB, GB, TB (1 KB = 1024 bytes, labelled KiB etc. if the user
  picks SI-correct labels in settings).
- Expanding a node with 100k children stays responsive.
- P1: filter by name pattern, size range, date range.

## 3. Visualisations
- **Treemap (P0):** squarified layout of the selected folder's subtree, area proportional to size.
  Click to drill down, a breadcrumb to go up, hover shows path and size.
- **Charts (P1):** pie or bar chart of the selected folder's children, with small items grouped
  into "Other".
- **Breakdowns (P1):** by extension (size, count), by file age (buckets by last modified), by
  owner, and a list of the N largest files.

## 4. Search (P1)
- Search the scanned tree by name (glob or regex), size, dates, extension, owner and attributes.
- Results show as a flat list with the same columns and actions as the tree.
- **Duplicates:** files with identical content, found by size, then a partial hash, then a full
  hash. Hard links to the same file are not duplicates. Cloud placeholders are skipped unless
  enabled.
- P2: built-in presets for old, temporary and large files.

## 5. Snapshots and history (P1)
- Save a scan to a `.tcs` file and reopen it without the original drive.
- Compare two snapshots of the same root: per-folder size and count change, plus the folders that
  grew the most.
- P2: trend charts across many snapshots.

## 6. Export (P1)
CSV, JSON, XLSX and a single-file HTML report, all respecting the current filter, depth and
columns. Copy the selection to the clipboard as text.

## 7. File actions (P1)
Open in Explorer, delete (to the Recycle Bin by default), move, NTFS-compress and show properties.
Destructive actions always confirm, showing the item count and total size.
P2: an "Analyze with Treecraft" entry in the Explorer context menu.

## 8. Command line (P0 basic, P1 full)
```
treecraft scan <root> --out <file.tcs> [--scanner auto|mft|win32|portable]
treecraft report <file.tcs> --format csv|json|xlsx|html [--top N] [--depth N] -o <out>
treecraft diff <old.tcs> <new.tcs> --format csv|json
treecraft dupes <root> [--min-size SIZE] --format json
```
Exit code 0 on success, 1 on error, 2 when a `--fail-if-larger` threshold is exceeded.

## 9. Platforms
Windows 10/11 x64 is first-class. ARM64 Windows, Linux and macOS use the portable scanner and get
every feature that doesn't depend on Windows APIs.

## Known limitations (MVP)

- The Win32 scanner reads sizes from directory listings, which don't include link counts, so a
  hard-linked file is counted under each of its names. The portable scanner deduplicates hard
  links on Linux and macOS. The MFT scanner (M5) will fix this on Windows.
- Space used by directory indexes and NTFS metadata is not counted.
- Deleting is disabled while a scan is running, because removing a subtree mid-scan would let
  the scanner add totals back for it.
