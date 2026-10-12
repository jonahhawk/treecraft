//! Squarified treemap layout.
//!
//! [`squarify`] lays out one level of weights in a rectangle, keeping cells as close to square
//! as possible (Bruls, Huizing and van Wijk's squarified algorithm). [`layout`] applies it
//! recursively to a [`Tree`].

use tc_model::{NodeId, Tree};

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// Shrinks the rectangle by `m` on every side, never below zero size.
    pub fn inset(&self, m: f32) -> Self {
        let mx = m.min(self.w / 2.0);
        let my = m.min(self.h / 2.0);
        Self {
            x: self.x + mx,
            y: self.y + my,
            w: self.w - 2.0 * mx,
            h: self.h - 2.0 * my,
        }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Lays out `weights` inside `rect`. Returns one rectangle per weight, in input order. Zero
/// weights get an empty rectangle. Areas are proportional to the weights.
pub fn squarify(weights: &[u64], rect: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::new(rect.x, rect.y, 0.0, 0.0); weights.len()];
    let total: u64 = weights.iter().sum();
    if total == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return out;
    }
    let scale = rect.area() as f64 / total as f64;
    let mut order: Vec<usize> = (0..weights.len()).filter(|&i| weights[i] > 0).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(weights[i]));
    let areas: Vec<f64> = order.iter().map(|&i| weights[i] as f64 * scale).collect();

    let mut free = rect;
    let mut start = 0;
    while start < order.len() {
        let side = f64::from(free.w.min(free.h));
        // Grow the row while the worst aspect ratio improves.
        let mut end = start + 1;
        let mut sum = areas[start];
        let mut worst_now = worst(&areas[start..end], sum, side);
        while end < order.len() {
            let next_sum = sum + areas[end];
            let w = worst(&areas[start..=end], next_sum, side);
            if w > worst_now {
                break;
            }
            worst_now = w;
            sum = next_sum;
            end += 1;
        }
        // The last row takes all remaining space, avoiding gaps from rounding.
        if end == order.len() {
            sum = f64::from(free.area());
        }
        place_row(
            &order[start..end],
            &areas[start..end],
            sum,
            &mut free,
            &mut out,
        );
        start = end;
    }
    out
}

/// Worst aspect ratio of a row with total area `sum` laid along a side of length `side`.
fn worst(row: &[f64], sum: f64, side: f64) -> f64 {
    let (min, max) = row
        .iter()
        .fold((f64::MAX, 0.0f64), |(lo, hi), &a| (lo.min(a), hi.max(a)));
    let s2 = sum * sum;
    let side2 = side * side;
    (side2 * max / s2).max(s2 / (side2 * min))
}

fn place_row(idx: &[usize], areas: &[f64], sum: f64, free: &mut Rect, out: &mut [Rect]) {
    let row_area_sum: f64 = areas.iter().sum();
    if free.w >= free.h {
        // Column along the left edge.
        let width = if free.h > 0.0 {
            (sum / f64::from(free.h)) as f32
        } else {
            0.0
        };
        let width = width.min(free.w);
        let mut y = free.y;
        for (k, (&i, &a)) in idx.iter().zip(areas).enumerate() {
            let h = if k + 1 == idx.len() {
                free.y + free.h - y
            } else {
                (a / row_area_sum) as f32 * free.h
            };
            out[i] = Rect::new(free.x, y, width, h);
            y += h;
        }
        free.x += width;
        free.w -= width;
    } else {
        // Row along the top edge.
        let height = if free.w > 0.0 {
            (sum / f64::from(free.w)) as f32
        } else {
            0.0
        };
        let height = height.min(free.h);
        let mut x = free.x;
        for (k, (&i, &a)) in idx.iter().zip(areas).enumerate() {
            let w = if k + 1 == idx.len() {
                free.x + free.w - x
            } else {
                (a / row_area_sum) as f32 * free.w
            };
            out[i] = Rect::new(x, free.y, w, height);
            x += w;
        }
        free.y += height;
        free.h -= height;
    }
}

/// One cell of a laid-out tree.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub id: NodeId,
    pub rect: Rect,
    /// Depth below the layout root (direct children are 1).
    pub depth: u32,
    pub is_dir: bool,
}

/// Options for [`layout`].
#[derive(Debug, Clone, Copy)]
pub struct LayoutOptions {
    /// Use allocated size instead of logical size.
    pub allocated: bool,
    /// Directories smaller than this (in either dimension) are not subdivided.
    pub min_side: f32,
    /// Space between a directory's border and its contents.
    pub padding: f32,
    /// Stop after this many cells.
    pub max_cells: usize,
    /// Stop after examining this many children in total. Bounds the cost of a layout when
    /// many folders hold thousands of items too small to draw.
    pub max_visits: usize,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            allocated: false,
            min_side: 6.0,
            padding: 2.0,
            max_cells: 30_000,
            max_visits: 300_000,
        }
    }
}

/// Children whose share of their parent is below this many square pixels are not drawn; they
/// are laid out together as one undrawn remainder, so they still take up their space.
const MIN_CELL_AREA: f64 = 1.0;

/// Lays out the subtree under `root` in `rect`. Parents come before their children in the
/// result, so painting in order draws children on top and hit-testing in reverse finds the
/// deepest cell first.
pub fn layout(tree: &Tree, root: NodeId, rect: Rect, opts: &LayoutOptions) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut visits = 0usize;
    let weight = |c: NodeId| {
        if opts.allocated {
            tree.node(c).allocated()
        } else {
            tree.node(c).size()
        }
    };
    // Breadth-first, so when a limit is reached every level is filled before going deeper.
    let mut queue = std::collections::VecDeque::from([(root, rect, 0u32)]);
    while let Some((id, r, depth)) = queue.pop_front() {
        if cells.len() >= opts.max_cells || visits >= opts.max_visits {
            break;
        }
        let inner = if depth == 0 { r } else { r.inset(opts.padding) };
        let total: u64 = weight(id);
        if total == 0 || inner.area() <= 0.0 {
            continue;
        }
        // Keep only children big enough to see; the rest become one undrawn remainder.
        let min_weight = (MIN_CELL_AREA * total as f64 / f64::from(inner.area())) as u64;
        let mut shown: Vec<NodeId> = Vec::new();
        let mut weights: Vec<u64> = Vec::new();
        let mut rest = 0u64;
        for c in tree.children(id) {
            visits += 1;
            let w = weight(c);
            if w >= min_weight.max(1) {
                shown.push(c);
                weights.push(w);
            } else {
                rest += w;
            }
        }
        weights.push(rest);
        let rects = squarify(&weights, inner);
        for (&c, cr) in shown.iter().zip(rects) {
            if cr.w < 1.0 || cr.h < 1.0 {
                continue;
            }
            let is_dir = tree.node(c).is_dir();
            cells.push(Cell {
                id: c,
                rect: cr,
                depth: depth + 1,
                is_dir,
            });
            if is_dir
                && cr.w >= opts.min_side
                && cr.h >= opts.min_side
                && tree.node(c).has_children()
            {
                queue.push_back((c, cr, depth + 1));
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tc_model::{Entry, NodeFlags};

    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        a.x + 0.001 < b.x + b.w
            && b.x + 0.001 < a.x + a.w
            && a.y + 0.001 < b.y + b.h
            && b.y + 0.001 < a.y + a.h
    }

    #[test]
    fn areas_are_proportional_and_fill_the_rect() {
        let weights = [6, 6, 4, 3, 2, 2, 1];
        let rect = Rect::new(0.0, 0.0, 6.0, 4.0);
        let cells = squarify(&weights, rect);
        let total: f32 = cells.iter().map(Rect::area).sum();
        assert!(approx(total, 24.0), "total area {total}");
        for (w, c) in weights.iter().zip(&cells) {
            assert!(
                approx(c.area(), *w as f32),
                "weight {w} got area {}",
                c.area()
            );
            assert!(c.x >= -0.001 && c.y >= -0.001 && c.x + c.w <= 6.001 && c.y + c.h <= 4.001);
        }
        for i in 0..cells.len() {
            for j in i + 1..cells.len() {
                assert!(
                    !overlaps(&cells[i], &cells[j]),
                    "{i} and {j} overlap: {:?} {:?}",
                    cells[i],
                    cells[j]
                );
            }
        }
    }

    #[test]
    fn cells_are_reasonably_square() {
        let weights: Vec<u64> = (1..=50).collect();
        let cells = squarify(&weights, Rect::new(0.0, 0.0, 800.0, 600.0));
        let worst = cells
            .iter()
            .map(|c| (c.w / c.h).max(c.h / c.w))
            .fold(0.0f32, f32::max);
        assert!(worst < 4.0, "worst aspect ratio {worst}");
    }

    #[test]
    fn zero_weights_and_empty_input() {
        assert!(squarify(&[], Rect::new(0.0, 0.0, 10.0, 10.0)).is_empty());
        let cells = squarify(&[0, 5, 0], Rect::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(cells[0].area(), 0.0);
        assert!(approx(cells[1].area(), 100.0));
        assert!(
            squarify(&[0, 0], Rect::new(0.0, 0.0, 10.0, 10.0))
                .iter()
                .all(|c| c.area() == 0.0)
        );
    }

    #[test]
    fn sub_pixel_children_keep_their_space_but_are_not_drawn() {
        let mut t = Tree::new(Path::new("/r"));
        let mut entries = vec![Entry {
            name: "big".into(),
            size: 1_000_000,
            allocated: 0,
            mtime: 0,
            flags: NodeFlags::empty(),
        }];
        entries.extend((0..10_000).map(|i| Entry {
            name: format!("tiny{i}").into(),
            size: 100,
            allocated: 0,
            mtime: 0,
            flags: NodeFlags::empty(),
        }));
        t.insert_listing(t.root(), &entries, &mut Vec::new());
        let cells = layout(
            &t,
            t.root(),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            &LayoutOptions::default(),
        );
        // Each tiny file would get 0.05 px², so only "big" is drawn, with its true share (50%).
        assert_eq!(cells.len(), 1);
        assert!(
            approx(cells[0].rect.area(), 5_000.0),
            "area {}",
            cells[0].rect.area()
        );
    }

    #[test]
    fn tree_layout_nests_children_inside_parents() {
        let mut t = Tree::new(Path::new("/r"));
        let f = |n: &str, s| Entry {
            name: n.into(),
            size: s,
            allocated: s,
            mtime: 0,
            flags: NodeFlags::empty(),
        };
        let d = |n: &str| Entry {
            name: n.into(),
            size: 0,
            allocated: 0,
            mtime: 0,
            flags: NodeFlags::DIR,
        };
        let mut subs = Vec::new();
        t.insert_listing(t.root(), &[f("a", 300), d("sub")], &mut subs);
        t.insert_listing(subs[0], &[f("b", 200), f("c", 100)], &mut Vec::new());

        let cells = layout(
            &t,
            t.root(),
            Rect::new(0.0, 0.0, 300.0, 200.0),
            &LayoutOptions::default(),
        );
        assert_eq!(cells.len(), 4);
        let sub = cells.iter().find(|c| c.is_dir).unwrap();
        assert!(approx(sub.rect.area(), 30_000.0));
        for c in cells.iter().filter(|c| c.depth == 2) {
            let r = c.rect;
            assert!(sub.rect.contains(r.x, r.y) && r.x + r.w <= sub.rect.x + sub.rect.w + 0.01);
        }
        // Parents precede children.
        let sub_pos = cells.iter().position(|c| c.is_dir).unwrap();
        assert!(
            cells
                .iter()
                .enumerate()
                .filter(|(_, c)| c.depth == 2)
                .all(|(i, _)| i > sub_pos)
        );
    }
}
