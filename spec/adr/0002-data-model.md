# ADR 0002: In-memory tree data model

- Status: Accepted
- Date: 2026-10-11

## Context

A full scan of a large volume can hold 10 million or more entries. A tree of individually
allocated nodes (`Box`/`Rc` per node, a `String` per name) costs well over 150 bytes per entry and
fragments the heap. Scanners produce entries from several threads, and the GUI reads the tree
while a scan is running.

## Decision

- Store nodes in one `Vec<Node>` addressed by `NodeId(u32)`, linked with `parent`,
  `first_child` and `next_sibling` indices. Target: at most 48 bytes per node.
- Store all names in one byte arena, referenced by offset. Names are WTF-8 so Windows names with
  unpaired surrogates round-trip without loss.
- Keep rarely used fields (owner, access and creation times, file ID, stream list) in side tables
  indexed by `NodeId`, so the hot struct stays small.
- One builder thread owns the tree and applies batches sent by scanner threads, so the arena
  needs no locks.
- Directory totals are computed bottom-up after the scan, and updated along the parent chain
  during the scan and on subtree refresh.
- Child ordering for display is computed by the view, not stored.

## Consequences

- `u32` IDs cap a single tree at about 4 billion entries, which is acceptable.
- Removing nodes (after delete or refresh) leaves holes; a compaction pass is needed when holes
  pass a threshold.
- The GUI needs either a read lock or periodic immutable copies of the tree. That choice is made
  in M3.
