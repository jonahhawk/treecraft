//! Arena-based file tree model with size aggregation (no I/O).
//!
//! All nodes live in one `Vec<Node>` and all names in one byte arena (see ADR 0002). Directory
//! totals are kept up to date as listings are inserted, so a partially scanned tree always shows
//! correct totals for what has been seen so far.

mod format;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub use format::{format_count, format_size};

/// Index of a node in a [`Tree`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    const NONE: u32 = u32::MAX;

    /// The raw index, for use as a key in side tables.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Per-node flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeFlags(u16);

impl NodeFlags {
    /// The node is a directory.
    pub const DIR: Self = Self(1 << 0);
    /// A symlink, junction or other link that is not followed.
    pub const LINK: Self = Self(1 << 1);
    /// A cloud placeholder whose data may not be stored locally.
    pub const CLOUD: Self = Self(1 << 2);
    /// The directory could not be read completely.
    pub const ERROR: Self = Self(1 << 3);
    /// The node was removed from the tree and must not be shown.
    pub const REMOVED: Self = Self(1 << 4);
    /// A sparse file.
    pub const SPARSE: Self = Self(1 << 5);
    /// A file-system compressed file.
    pub const COMPRESSED: Self = Self(1 << 6);
    /// An extra name of a hard-linked file; its size is counted under another name.
    pub const HARDLINK_DUP: Self = Self(1 << 7);
    /// Hidden or system file.
    pub const HIDDEN: Self = Self(1 << 8);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOr for NodeFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// One entry produced by a scanner, before it is inserted into the tree.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    /// Logical size in bytes. Zero for directories (their size is the sum of their contents).
    pub size: u64,
    /// Space allocated on disk in bytes.
    pub allocated: u64,
    /// Last modification time, seconds since the Unix epoch (0 = unknown).
    pub mtime: i64,
    pub flags: NodeFlags,
}

impl Entry {
    /// Whether a scanner should descend into this entry.
    pub fn is_traversable_dir(&self) -> bool {
        self.flags.contains(NodeFlags::DIR) && !self.flags.contains(NodeFlags::LINK)
    }
}

/// A node in the tree. For directories, `size`, `allocated`, `files` and `dirs` are subtree totals.
#[derive(Debug, Clone)]
#[repr(C)]
pub struct Node {
    parent: u32,
    first_child: u32,
    next_sibling: u32,
    name_off: u32,
    name_len: u16,
    flags: NodeFlags,
    files: u32,
    dirs: u32,
    size: u64,
    allocated: u64,
    mtime: i64,
}

impl Node {
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn allocated(&self) -> u64 {
        self.allocated
    }
    /// Files in the subtree (1 for a file node).
    pub fn files(&self) -> u32 {
        self.files
    }
    /// Directories in the subtree, not counting this node.
    pub fn dirs(&self) -> u32 {
        self.dirs
    }
    pub fn mtime(&self) -> i64 {
        self.mtime
    }
    pub fn flags(&self) -> NodeFlags {
        self.flags
    }
    pub fn is_dir(&self) -> bool {
        self.flags.contains(NodeFlags::DIR)
    }
    pub fn has_children(&self) -> bool {
        self.first_child != NodeId::NONE
    }
}

/// Column to sort children by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Allocated,
    Files,
    Dirs,
    Modified,
}

/// The scanned file tree.
#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<Node>,
    names: Vec<u8>,
    removed: u32,
    generation: u64,
}

impl Tree {
    /// Creates a tree containing only the root directory. The root's name is the full root path.
    pub fn new(root: &Path) -> Self {
        let mut tree = Tree {
            nodes: Vec::new(),
            names: Vec::new(),
            removed: 0,
            generation: 0,
        };
        let name_off = tree.push_name(root.as_os_str());
        tree.nodes.push(Node {
            parent: NodeId::NONE,
            first_child: NodeId::NONE,
            next_sibling: NodeId::NONE,
            name_off,
            name_len: tree.names.len() as u16,
            flags: NodeFlags::DIR,
            files: 0,
            dirs: 0,
            size: 0,
            allocated: 0,
            mtime: 0,
        });
        tree
    }

    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Number of nodes ever inserted, including removed ones.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Increases on every change, so views can tell when to refresh.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.nodes[id.index()].parent;
        (p != NodeId::NONE).then_some(NodeId(p))
    }

    /// The node's own name (the full root path for the root).
    pub fn name(&self, id: NodeId) -> &OsStr {
        let n = &self.nodes[id.index()];
        let bytes = &self.names[n.name_off as usize..n.name_off as usize + n.name_len as usize];
        // SAFETY: every name slice was produced by `OsStr::as_encoded_bytes` in this process
        // (see `push_name`), so it is valid encoded OsStr data for this platform.
        unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
    }

    /// The full path of a node.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            parts.push(c);
            cur = self.parent(c);
        }
        let mut path = PathBuf::new();
        for &p in parts.iter().rev() {
            path.push(self.name(p));
        }
        path
    }

    /// Depth below the root (the root is 0).
    pub fn depth(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = id;
        while let Some(p) = self.parent(cur) {
            d += 1;
            cur = p;
        }
        d
    }

    /// Iterates over the direct children of a node, in no particular order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            tree: self,
            next: self.nodes[id.index()].first_child,
        }
    }

    /// Direct children sorted by `key`. Names sort ascending, everything else descending,
    /// unless `reverse` is set.
    pub fn sorted_children(&self, id: NodeId, key: SortKey, reverse: bool) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.children(id).collect();
        match key {
            SortKey::Name => {
                v.sort_by_cached_key(|&c| self.name(c).to_string_lossy().to_lowercase())
            }
            SortKey::Size => v.sort_by_key(|&c| std::cmp::Reverse(self.node(c).size)),
            SortKey::Allocated => v.sort_by_key(|&c| std::cmp::Reverse(self.node(c).allocated)),
            SortKey::Files => v.sort_by_key(|&c| std::cmp::Reverse(self.node(c).files)),
            SortKey::Dirs => v.sort_by_key(|&c| std::cmp::Reverse(self.node(c).dirs)),
            SortKey::Modified => v.sort_by_key(|&c| std::cmp::Reverse(self.node(c).mtime)),
        }
        if reverse {
            v.reverse();
        }
        v
    }

    /// Sets the root's modification time.
    pub fn set_mtime(&mut self, id: NodeId, mtime: i64) {
        self.nodes[id.index()].mtime = mtime;
        self.generation += 1;
    }

    /// Inserts the listing of directory `parent` and adds its totals to every ancestor.
    ///
    /// For each entry that [`Entry::is_traversable_dir`], the new node's id is pushed to
    /// `subdirs`, in the same order as the entries.
    pub fn insert_listing(&mut self, parent: NodeId, entries: &[Entry], subdirs: &mut Vec<NodeId>) {
        let (mut size, mut allocated, mut files, mut dirs) = (0u64, 0u64, 0u32, 0u32);
        for e in entries {
            let id = NodeId(self.nodes.len() as u32);
            let is_dir = e.flags.contains(NodeFlags::DIR);
            let name_off = self.push_name(&e.name);
            let p = &mut self.nodes[parent.index()];
            let next_sibling = p.first_child;
            p.first_child = id.0;
            self.nodes.push(Node {
                parent: parent.0,
                first_child: NodeId::NONE,
                next_sibling,
                name_off,
                name_len: (self.names.len() - name_off as usize) as u16,
                flags: e.flags,
                files: u32::from(!is_dir),
                dirs: 0,
                size: e.size,
                allocated: e.allocated,
                mtime: e.mtime,
            });
            size += e.size;
            allocated += e.allocated;
            if is_dir {
                dirs += 1;
            } else {
                files += 1;
            }
            if e.is_traversable_dir() {
                subdirs.push(id);
            }
        }
        self.add_to_ancestors(
            Some(parent),
            size as i128,
            allocated as i128,
            files as i64,
            dirs as i64,
        );
        self.generation += 1;
    }

    /// Flags a directory as not completely readable.
    pub fn mark_error(&mut self, id: NodeId) {
        self.nodes[id.index()].flags.insert(NodeFlags::ERROR);
        self.generation += 1;
    }

    /// Detaches a node (and its subtree) from the tree and subtracts its totals from its
    /// ancestors. The root cannot be removed.
    pub fn remove(&mut self, id: NodeId) {
        let Some(parent) = self.parent(id) else {
            return;
        };
        if self.nodes[id.index()].flags.contains(NodeFlags::REMOVED) {
            return;
        }
        // Unlink from the parent's child list.
        let next = self.nodes[id.index()].next_sibling;
        if self.nodes[parent.index()].first_child == id.0 {
            self.nodes[parent.index()].first_child = next;
        } else {
            let mut cur = self.nodes[parent.index()].first_child;
            while cur != NodeId::NONE {
                if self.nodes[cur as usize].next_sibling == id.0 {
                    self.nodes[cur as usize].next_sibling = next;
                    break;
                }
                cur = self.nodes[cur as usize].next_sibling;
            }
        }
        let n = &mut self.nodes[id.index()];
        n.flags.insert(NodeFlags::REMOVED);
        n.next_sibling = NodeId::NONE;
        let (size, allocated, files) = (n.size, n.allocated, n.files);
        let dirs = n.dirs + u32::from(n.flags.contains(NodeFlags::DIR));
        self.add_to_ancestors(
            Some(parent),
            -(size as i128),
            -(allocated as i128),
            -(files as i64),
            -(dirs as i64),
        );
        self.removed += 1;
        self.generation += 1;
    }

    fn add_to_ancestors(
        &mut self,
        mut cur: Option<NodeId>,
        size: i128,
        allocated: i128,
        files: i64,
        dirs: i64,
    ) {
        if size == 0 && allocated == 0 && files == 0 && dirs == 0 {
            return;
        }
        while let Some(id) = cur {
            let n = &mut self.nodes[id.index()];
            n.size = (n.size as i128 + size) as u64;
            n.allocated = (n.allocated as i128 + allocated) as u64;
            n.files = (n.files as i64 + files) as u32;
            n.dirs = (n.dirs as i64 + dirs) as u32;
            cur = self.parent(id);
        }
    }

    fn push_name(&mut self, name: &OsStr) -> u32 {
        let off = self.names.len() as u32;
        let bytes = name.as_encoded_bytes();
        // Names are at most 255 UTF-16 units (765 bytes encoded); the root path may be longer
        // but is stored first, so cap it to keep the u16 length valid.
        let bytes = &bytes[..bytes.len().min(u16::MAX as usize)];
        self.names.extend_from_slice(bytes);
        off
    }
}

/// Iterator over a node's children.
#[derive(Debug)]
pub struct Children<'a> {
    tree: &'a Tree,
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        if self.next == NodeId::NONE {
            return None;
        }
        let id = NodeId(self.next);
        self.next = self.tree.nodes[id.index()].next_sibling;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> Entry {
        Entry {
            name: name.into(),
            size,
            allocated: size.div_ceil(4096) * 4096,
            mtime: 0,
            flags: NodeFlags::empty(),
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.into(),
            size: 0,
            allocated: 0,
            mtime: 0,
            flags: NodeFlags::DIR,
        }
    }

    /// root/{a.txt(10), b.bin(5000), sub/{c(100), deeper/{d(1)}}}
    fn sample() -> (Tree, NodeId, NodeId) {
        let mut t = Tree::new(Path::new("/r"));
        let mut subs = Vec::new();
        t.insert_listing(
            t.root(),
            &[file("a.txt", 10), file("b.bin", 5000), dir("sub")],
            &mut subs,
        );
        let sub = subs[0];
        subs.clear();
        t.insert_listing(sub, &[file("c", 100), dir("deeper")], &mut subs);
        let deeper = subs[0];
        t.insert_listing(deeper, &[file("d", 1)], &mut Vec::new());
        (t, sub, deeper)
    }

    #[test]
    fn node_stays_small() {
        assert!(
            std::mem::size_of::<Node>() <= 56,
            "Node is {} bytes",
            std::mem::size_of::<Node>()
        );
    }

    #[test]
    fn totals_aggregate_up() {
        let (t, sub, deeper) = sample();
        let root = t.node(t.root());
        assert_eq!(root.size(), 5111);
        assert_eq!(root.files(), 4);
        assert_eq!(root.dirs(), 2);
        assert_eq!(root.allocated(), 4096 * 5);
        assert_eq!(t.node(sub).size(), 101);
        assert_eq!(t.node(sub).dirs(), 1);
        assert_eq!(t.node(deeper).files(), 1);
    }

    #[test]
    fn paths_and_names() {
        let (t, _, deeper) = sample();
        assert_eq!(t.path(deeper), Path::new("/r").join("sub").join("deeper"));
        assert_eq!(t.name(deeper), "deeper");
        assert_eq!(t.depth(deeper), 2);
    }

    #[test]
    fn sorting() {
        let (t, _, _) = sample();
        let names = |ids: Vec<NodeId>| {
            ids.into_iter()
                .map(|c| t.name(c).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(t.sorted_children(t.root(), SortKey::Size, false)),
            ["b.bin", "sub", "a.txt"]
        );
        assert_eq!(
            names(t.sorted_children(t.root(), SortKey::Name, false)),
            ["a.txt", "b.bin", "sub"]
        );
        assert_eq!(
            names(t.sorted_children(t.root(), SortKey::Name, true)),
            ["sub", "b.bin", "a.txt"]
        );
    }

    #[test]
    fn remove_subtracts_totals() {
        let (mut t, sub, deeper) = sample();
        let g = t.generation();
        t.remove(deeper);
        assert!(t.generation() > g);
        assert_eq!(t.node(t.root()).size(), 5110);
        assert_eq!(t.node(t.root()).files(), 3);
        assert_eq!(t.node(t.root()).dirs(), 1);
        assert_eq!(t.node(sub).dirs(), 0);
        assert_eq!(t.children(sub).count(), 1);
        // Removing twice is a no-op; removing the root is ignored.
        t.remove(deeper);
        t.remove(t.root());
        assert_eq!(t.node(t.root()).size(), 5110);
        t.remove(sub);
        assert_eq!(t.node(t.root()).size(), 5010);
        assert_eq!(t.node(t.root()).dirs(), 0);
        assert_eq!(t.children(t.root()).count(), 2);
    }

    #[test]
    fn links_are_not_traversed() {
        let mut link = dir("j");
        link.flags.insert(NodeFlags::LINK);
        let mut t = Tree::new(Path::new("/r"));
        let mut subs = Vec::new();
        t.insert_listing(t.root(), &[link, dir("real")], &mut subs);
        assert_eq!(subs.len(), 1);
        assert_eq!(t.name(subs[0]), "real");
    }

    #[test]
    fn non_ascii_names_round_trip() {
        let mut t = Tree::new(Path::new("/r"));
        t.insert_listing(t.root(), &[file("日本語 😀.txt", 1)], &mut Vec::new());
        let c = t.children(t.root()).next().unwrap();
        assert_eq!(t.name(c), "日本語 😀.txt");
    }
}
