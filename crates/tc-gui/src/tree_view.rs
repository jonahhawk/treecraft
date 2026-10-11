//! The expandable tree table.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use tc_model::{NodeFlags, NodeId, SortKey, Tree, format_count, format_size};

use crate::app::Action;
use crate::platform::format_time;

/// How often the visible rows are rebuilt while the tree is changing.
const REBUILD_INTERVAL: Duration = Duration::from_millis(300);
const ROW_HEIGHT: f32 = 20.0;
const INDENT: f32 = 16.0;

#[derive(Debug)]
pub struct TreeView {
    expanded: HashSet<NodeId>,
    pub sort: SortKey,
    pub reverse: bool,
    rows: Vec<(NodeId, u16)>,
    built_generation: Option<u64>,
    built_at: Instant,
    dirty: bool,
    scroll_to: Option<NodeId>,
}

impl TreeView {
    pub fn new() -> Self {
        Self {
            expanded: HashSet::new(),
            sort: SortKey::Size,
            reverse: false,
            rows: Vec::new(),
            built_generation: None,
            built_at: Instant::now(),
            dirty: true,
            scroll_to: None,
        }
    }

    /// Forgets all state for a new tree; the root starts expanded.
    pub fn reset(&mut self, root: NodeId) {
        self.expanded.clear();
        self.expanded.insert(root);
        self.rows.clear();
        self.built_generation = None;
        self.dirty = true;
        self.scroll_to = None;
    }

    pub fn toggle(&mut self, id: NodeId) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.dirty = true;
    }

    /// Expands every ancestor of `id` and scrolls it into view.
    pub fn reveal(&mut self, tree: &Tree, id: NodeId) {
        let mut cur = tree.parent(id);
        while let Some(p) = cur {
            self.expanded.insert(p);
            cur = tree.parent(p);
        }
        self.scroll_to = Some(id);
        self.dirty = true;
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort == key {
            self.reverse = !self.reverse;
        } else {
            self.sort = key;
            self.reverse = false;
        }
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn rebuild_if_needed(&mut self, tree: &Tree) {
        let changed = self.built_generation != Some(tree.generation());
        let due = self.built_generation.is_none() || self.built_at.elapsed() >= REBUILD_INTERVAL;
        if !(self.dirty || changed && due) {
            return;
        }
        self.rows.clear();
        let mut stack = vec![(tree.root(), 0u16)];
        while let Some((id, depth)) = stack.pop() {
            self.rows.push((id, depth));
            if self.expanded.contains(&id) {
                let children = tree.sorted_children(id, self.sort, self.reverse);
                stack.extend(children.into_iter().rev().map(|c| (c, depth + 1)));
            }
        }
        self.built_generation = Some(tree.generation());
        self.built_at = Instant::now();
        self.dirty = false;
    }

    /// Whether rows are waiting for a rebuild that is throttled.
    pub fn is_stale(&self, tree: &Tree) -> bool {
        self.built_generation != Some(tree.generation())
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        tree: &Tree,
        selected: Option<NodeId>,
        allocated: bool,
        actions: &mut Vec<Action>,
    ) {
        self.rebuild_if_needed(tree);

        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(260.0).at_least(120.0).clip(true))
            .column(Column::initial(76.0).at_least(60.0))
            .column(Column::initial(76.0).at_least(60.0))
            .column(Column::initial(100.0).at_least(60.0))
            .column(Column::initial(64.0).at_least(40.0))
            .column(Column::initial(56.0).at_least(40.0))
            .column(Column::remainder().at_least(110.0));
        if let Some(target) = self.scroll_to.take() {
            if let Some(i) = self.rows.iter().position(|&(id, _)| id == target) {
                table = table.scroll_to_row(i, Some(Align::Center));
            }
        }

        let sort = self.sort;
        let reverse = self.reverse;
        let header = |ui: &mut egui::Ui, label: &str, key: SortKey, actions: &mut Vec<Action>| {
            let arrow = if sort == key {
                if reverse { " ⏶" } else { " ⏷" }
            } else {
                ""
            };
            let text = RichText::new(format!("{label}{arrow}")).strong();
            if ui
                .add(egui::Label::new(text).sense(Sense::click()))
                .on_hover_text("Sort")
                .clicked()
            {
                actions.push(Action::Sort(key));
            }
        };
        let size_key = if allocated {
            SortKey::Allocated
        } else {
            SortKey::Size
        };

        table
            .header(22.0, |mut h| {
                h.col(|ui| header(ui, "Name", SortKey::Name, actions));
                h.col(|ui| header(ui, "Size", SortKey::Size, actions));
                h.col(|ui| header(ui, "Allocated", SortKey::Allocated, actions));
                h.col(|ui| header(ui, "% of parent", size_key, actions));
                h.col(|ui| header(ui, "Files", SortKey::Files, actions));
                h.col(|ui| header(ui, "Folders", SortKey::Dirs, actions));
                h.col(|ui| header(ui, "Modified", SortKey::Modified, actions));
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, self.rows.len(), |mut row| {
                    let (id, depth) = self.rows[row.index()];
                    let node = tree.node(id);
                    row.set_selected(selected == Some(id));
                    let is_root = id == tree.root();
                    let value = |id: NodeId| {
                        if allocated {
                            tree.node(id).allocated()
                        } else {
                            tree.node(id).size()
                        }
                    };

                    row.col(|ui| {
                        ui.add_space(f32::from(depth) * INDENT);
                        if node.is_dir() && node.has_children() {
                            let icon = if self.expanded.contains(&id) {
                                "⏷"
                            } else {
                                "⏵"
                            };
                            if ui
                                .add(egui::Label::new(icon).sense(Sense::click()))
                                .clicked()
                            {
                                actions.push(Action::Toggle(id));
                            }
                        } else {
                            ui.add_space(INDENT - ui.spacing().item_spacing.x);
                        }
                        let flags = node.flags();
                        icon(ui, node.is_dir(), flags.contains(NodeFlags::LINK));
                        let name = if is_root {
                            tree.path(id).display().to_string()
                        } else {
                            tree.name(id).to_string_lossy().into_owned()
                        };
                        let mut text = RichText::new(name);
                        if flags.contains(NodeFlags::ERROR) {
                            text = text.color(Color32::from_rgb(220, 90, 70));
                        } else if flags.contains(NodeFlags::LINK)
                            || flags.contains(NodeFlags::HIDDEN)
                        {
                            text = text.weak();
                        }
                        let label = ui.add(egui::Label::new(text).selectable(false).truncate());
                        if flags.contains(NodeFlags::ERROR) {
                            label.on_hover_text("Some contents could not be read");
                        } else if flags.contains(NodeFlags::LINK) {
                            label.on_hover_text("Link or junction (not followed)");
                        } else if flags.contains(NodeFlags::CLOUD) {
                            label.on_hover_text("Cloud file (may not be stored locally)");
                        }
                    });
                    row.col(|ui| right(ui, format_size(node.size())));
                    row.col(|ui| right(ui, format_size(node.allocated())));
                    row.col(|ui| {
                        let parent_value = tree.parent(id).map_or(value(id), value);
                        let frac = if parent_value == 0 {
                            0.0
                        } else {
                            value(id) as f32 / parent_value as f32
                        };
                        percent_bar(ui, frac);
                    });
                    row.col(|ui| {
                        right(
                            ui,
                            if node.is_dir() {
                                format_count(node.files().into())
                            } else {
                                String::new()
                            },
                        )
                    });
                    row.col(|ui| {
                        right(
                            ui,
                            if node.is_dir() {
                                format_count(node.dirs().into())
                            } else {
                                String::new()
                            },
                        )
                    });
                    row.col(|ui| {
                        ui.label(format_time(node.mtime()));
                    });

                    let resp = row.response();
                    if resp.clicked() || resp.secondary_clicked() {
                        actions.push(Action::Select(id));
                    }
                    if resp.double_clicked() && node.is_dir() {
                        actions.push(Action::Toggle(id));
                    }
                    resp.context_menu(|ui| context_menu(ui, tree, id, actions));
                });
            });
    }
}

/// A small folder or file icon, drawn rather than taken from a font.
fn icon(ui: &mut egui::Ui, is_dir: bool, is_link: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
    let p = ui.painter_at(rect);
    let r = rect.shrink2(egui::vec2(0.5, 1.5));
    if is_dir {
        let color = if is_link {
            Color32::from_rgb(120, 160, 210)
        } else {
            Color32::from_rgb(230, 184, 80)
        };
        let tab = egui::Rect::from_min_size(r.min, egui::vec2(r.width() * 0.45, 3.0));
        p.rect_filled(tab, 1.0, color);
        p.rect_filled(
            egui::Rect::from_min_max(r.min + egui::vec2(0.0, 2.0), r.max),
            1.5,
            color,
        );
    } else {
        let page =
            egui::Rect::from_center_size(r.center(), egui::vec2(r.width() * 0.75, r.height()));
        let color = if is_link {
            Color32::from_rgb(120, 160, 210)
        } else {
            ui.visuals().weak_text_color()
        };
        p.rect_stroke(
            page,
            1.0,
            egui::Stroke::new(1.2, color),
            egui::StrokeKind::Inside,
        );
    }
}

fn right(ui: &mut egui::Ui, text: String) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(text);
    });
}

fn percent_bar(ui: &mut egui::Ui, frac: f32) {
    let frac = frac.clamp(0.0, 1.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT - 4.0),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 2.0, visuals.extreme_bg_color);
    let mut fill = rect;
    fill.set_width(rect.width() * frac);
    painter.rect_filled(fill, 2.0, visuals.selection.bg_fill.gamma_multiply(0.8));
    painter.text(
        rect.right_center() - egui::vec2(4.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        format!("{:.1} %", frac * 100.0),
        egui::FontId::proportional(12.0),
        visuals.text_color(),
    );
}

/// Menu shared by the tree table and the treemap.
pub fn context_menu(ui: &mut egui::Ui, tree: &Tree, id: NodeId, actions: &mut Vec<Action>) {
    let path = tree.path(id);
    if ui.button("Show in file manager").clicked() {
        actions.push(Action::ShowInFileManager(path.clone()));
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(path.display().to_string());
        ui.close();
    }
    if tree.node(id).is_dir() && ui.button("Show in treemap").clicked() {
        actions.push(Action::Select(id));
        ui.close();
    }
    ui.separator();
    let can_delete = id != tree.root();
    if ui
        .add_enabled(can_delete, egui::Button::new("Delete to Recycle Bin…"))
        .clicked()
    {
        actions.push(Action::AskDelete(id));
        ui.close();
    }
}
