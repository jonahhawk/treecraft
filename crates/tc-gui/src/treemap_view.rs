//! Treemap panel: paints a squarified treemap of the selected folder.

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Sense, Stroke, StrokeKind, ecolor::Hsva};
use tc_analysis::treemap::{Cell, LayoutOptions, Rect, layout};
use tc_model::{NodeId, Tree, format_size};

use crate::app::Action;

const REBUILD_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub struct TreemapView {
    cells: Vec<Cell>,
    /// Fill colour and optional label per cell, computed with the layout so painting a frame
    /// is cheap and holds the tree lock only briefly.
    looks: Vec<(Color32, Option<String>)>,
    /// (tree generation, root, size in px, allocated, dark mode) of the current layout.
    key: Option<(u64, NodeId, [i32; 2], bool, bool)>,
    built_at: Instant,
}

impl Default for TreemapView {
    fn default() -> Self {
        Self::new()
    }
}

impl TreemapView {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            looks: Vec::new(),
            key: None,
            built_at: Instant::now(),
        }
    }

    pub fn reset(&mut self) {
        self.cells.clear();
        self.looks.clear();
        self.key = None;
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        tree: &Tree,
        root: NodeId,
        selected: Option<NodeId>,
        allocated: bool,
        actions: &mut Vec<Action>,
    ) {
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let size = [rect.width() as i32, rect.height() as i32];
        let dark = ui.visuals().dark_mode;
        let key = (tree.generation(), root, size, allocated, dark);
        let same_view = self
            .key
            .is_some_and(|k| (k.1, k.2, k.3, k.4) == (root, size, allocated, dark));
        // While a scan is changing the tree, relayout at most every REBUILD_INTERVAL.
        if self.key != Some(key) && (!same_view || self.built_at.elapsed() >= REBUILD_INTERVAL) {
            let opts = LayoutOptions {
                allocated,
                ..Default::default()
            };
            let area = Rect::new(rect.left(), rect.top(), rect.width(), rect.height());
            self.cells = layout(tree, root, area, &opts);
            self.looks = self
                .cells
                .iter()
                .map(|c| {
                    let label = (c.rect.w > 50.0 && c.rect.h > 14.0)
                        .then(|| tree.name(c.id).to_string_lossy().into_owned());
                    (cell_color(tree, c, dark), label)
                })
                .collect();
            self.key = Some(key);
            self.built_at = Instant::now();
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let font = egui::FontId::proportional(11.0);
        let text_color = if dark {
            Color32::from_gray(235)
        } else {
            Color32::from_gray(20)
        };
        for (cell, (fill, label)) in self.cells.iter().zip(&self.looks) {
            let r = to_egui(cell.rect);
            painter.rect_filled(r, 0.0, *fill);
            if r.width() > 3.0 && r.height() > 3.0 {
                painter.rect_stroke(
                    r,
                    0.0,
                    Stroke::new(1.0, darken(*fill, 0.6)),
                    StrokeKind::Inside,
                );
            }
            if let Some(label) = label {
                painter.with_clip_rect(r.shrink(2.0)).text(
                    r.left_top() + egui::vec2(3.0, 1.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    font.clone(),
                    text_color,
                );
            }
        }
        if let Some(sel) = selected.and_then(|s| self.cells.iter().find(|c| c.id == s)) {
            painter.rect_stroke(
                to_egui(sel.rect),
                0.0,
                Stroke::new(2.0, Color32::YELLOW),
                StrokeKind::Inside,
            );
        }

        let pointer = response.hover_pos();
        let hovered = pointer.and_then(|p| self.hit(p));
        if let Some(cell) = hovered {
            painter.rect_stroke(
                to_egui(cell.rect),
                0.0,
                Stroke::new(2.0, Color32::WHITE),
                StrokeKind::Inside,
            );
            let root_size = value(tree, root, allocated);
            let size = value(tree, cell.id, allocated);
            let pct = if root_size == 0 {
                0.0
            } else {
                size as f64 * 100.0 / root_size as f64
            };
            let path = tree.path(cell.id);
            response.clone().on_hover_ui_at_pointer(|ui| {
                ui.label(path.display().to_string());
                ui.label(format!("{}  ({pct:.1} % of view)", format_size(size)));
            });
        }
        if self.cells.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Nothing to show",
                font,
                ui.visuals().weak_text_color(),
            );
        }

        if let Some(cell) = hovered {
            if response.double_clicked() {
                // Drill into the folder, or into the folder containing the file.
                let target = if cell.is_dir {
                    cell.id
                } else {
                    tree.parent(cell.id).unwrap_or(cell.id)
                };
                actions.push(Action::Navigate(target));
            } else if response.clicked() {
                actions.push(Action::Reveal(cell.id));
            }
            let id = cell.id;
            response.context_menu(|ui| crate::tree_view::context_menu(ui, tree, id, actions));
        }
    }

    /// The deepest cell under `p`.
    fn hit(&self, p: Pos2) -> Option<Cell> {
        self.cells
            .iter()
            .rev()
            .find(|c| c.rect.contains(p.x, p.y))
            .copied()
    }
}

fn value(tree: &Tree, id: NodeId, allocated: bool) -> u64 {
    if allocated {
        tree.node(id).allocated()
    } else {
        tree.node(id).size()
    }
}

fn darken(c: Color32, f: f32) -> Color32 {
    let s = |v: u8| (f32::from(v) * f) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

fn to_egui(r: Rect) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h))
}

/// Folders are grey, darker with depth; files are coloured by extension.
fn cell_color(tree: &Tree, cell: &Cell, dark: bool) -> Color32 {
    if cell.is_dir {
        let base = if dark { 70u8 } else { 200 };
        let shade = (cell.depth.min(8) * 6) as u8;
        return Color32::from_gray(if dark {
            base.saturating_add(shade)
        } else {
            base.saturating_sub(shade)
        });
    }
    let hue = match tc_analysis::extension(tree.name(cell.id)) {
        Some(ext) => {
            // FNV-1a hash of the extension picks a stable hue.
            let h = ext.bytes().fold(0x811c_9dc5u32, |h, b| {
                (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
            });
            (h % 360) as f32 / 360.0
        }
        None => {
            return if dark {
                Color32::from_gray(110)
            } else {
                Color32::from_gray(170)
            };
        }
    };
    let (s, v) = if dark { (0.55, 0.72) } else { (0.45, 0.92) };
    Hsva::new(hue, s, v, 1.0).into()
}
