//! Application state and top-level layout.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use tc_model::{NodeId, SortKey, format_count, format_size};
use tc_scan::{ScanHandle, ScanOptions, ScanProgress, ScanStats, SharedTree, new_tree, start_scan};
use tc_scan_win::{Drive, ScannerKind};

use crate::platform::show_in_file_manager;
use crate::tree_view::TreeView;
use crate::treemap_view::TreemapView;

/// Something the user asked for while the UI was being drawn, applied after drawing.
#[derive(Debug)]
pub enum Action {
    /// Select a node from the table; the treemap shows it (or its folder).
    Select(NodeId),
    /// Select a node from the treemap and reveal it in the table, keeping the treemap view.
    Reveal(NodeId),
    /// Show a folder in the treemap and reveal it in the table.
    Navigate(NodeId),
    Toggle(NodeId),
    Sort(SortKey),
    ShowInFileManager(PathBuf),
    AskDelete(NodeId),
}

/// One scanned (or scanning) root.
#[derive(Debug)]
struct Session {
    tree: SharedTree,
    handle: Option<ScanHandle>,
    progress: Arc<ScanProgress>,
    scanner: &'static str,
    started: Instant,
    stats: Option<ScanStats>,
}

impl Session {
    fn is_scanning(&self) -> bool {
        self.handle.is_some()
    }
}

#[derive(Debug)]
struct PendingDelete {
    id: NodeId,
    path: PathBuf,
    result: Receiver<Result<(), String>>,
}

#[derive(Debug)]
pub struct App {
    drives: Vec<Drive>,
    path_input: String,
    scanner: ScannerKind,
    allocated: bool,
    session: Option<Session>,
    tree_view: TreeView,
    treemap: TreemapView,
    selected: Option<NodeId>,
    treemap_root: Option<NodeId>,
    confirm_delete: Option<NodeId>,
    deleting: Option<PendingDelete>,
    message: Option<(String, bool)>,
    show_errors: bool,
}

impl App {
    pub fn new(initial_path: Option<PathBuf>) -> Self {
        let drives = tc_scan_win::drives();
        let path_input = initial_path
            .as_ref()
            .map(|p| p.display().to_string())
            .or_else(|| drives.first().map(|d| d.root.display().to_string()))
            .unwrap_or_default();
        let mut app = Self {
            drives,
            path_input,
            scanner: ScannerKind::Auto,
            allocated: false,
            session: None,
            tree_view: TreeView::new(),
            treemap: TreemapView::new(),
            selected: None,
            treemap_root: None,
            confirm_delete: None,
            deleting: None,
            message: None,
            show_errors: false,
        };
        if initial_path.is_some() {
            app.start_scan();
        }
        app
    }

    fn start_scan(&mut self) {
        self.stop_scan();
        let root = PathBuf::from(self.path_input.trim());
        let tree = match new_tree(&root) {
            Ok(t) => t,
            Err(e) => {
                self.message = Some((format!("Cannot scan {}: {e}", root.display()), true));
                return;
            }
        };
        let root_id = tree.root();
        let shared = SharedTree::new(tree);
        let lister = tc_scan_win::lister(self.scanner);
        let scanner = lister.name();
        // Below-normal priority keeps frames smooth while the scan saturates the CPU.
        let opts = ScanOptions {
            low_priority: true,
            ..ScanOptions::default()
        };
        let handle = start_scan(shared.clone(), lister, opts);
        self.session = Some(Session {
            tree: shared,
            progress: handle.progress().clone(),
            handle: Some(handle),
            scanner,
            started: Instant::now(),
            stats: None,
        });
        self.tree_view.reset(root_id);
        self.treemap.reset();
        self.selected = Some(root_id);
        self.treemap_root = Some(root_id);
        self.message = None;
        self.show_errors = false;
    }

    fn stop_scan(&mut self) {
        if let Some(s) = &self.session {
            if let Some(h) = &s.handle {
                h.cancel();
            }
        }
        self.poll_scan();
    }

    /// Collects the result of a finished scan.
    fn poll_scan(&mut self) {
        let Some(s) = &mut self.session else { return };
        if s.handle
            .as_ref()
            .is_some_and(|h| h.is_finished() || h.progress().is_cancelled())
        {
            let stats = s.handle.take().expect("checked above").join();
            s.stats = Some(stats);
            self.tree_view.mark_dirty();
        }
    }

    fn poll_delete(&mut self) {
        let Some(d) = &self.deleting else { return };
        let outcome = match d.result.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("delete thread stopped unexpectedly".into()),
        };
        let d = self.deleting.take().expect("checked above");
        match outcome {
            Ok(()) => {
                if let Some(s) = &self.session {
                    let mut tree = s.tree.write();
                    let parent = tree.parent(d.id);
                    tree.remove(d.id);
                    if self.selected == Some(d.id) {
                        self.selected = parent;
                    }
                    if self
                        .treemap_root
                        .is_some_and(|r| r == d.id || is_descendant(&tree, r, d.id))
                    {
                        self.treemap_root = parent;
                    }
                }
                self.tree_view.mark_dirty();
                self.message = Some((
                    format!("Moved {} to the Recycle Bin", display_name(&d.path)),
                    false,
                ));
            }
            Err(e) => {
                self.message = Some((format!("Could not delete {}: {e}", d.path.display()), true))
            }
        }
    }

    fn apply(&mut self, actions: Vec<Action>) {
        let Some(session) = &self.session else { return };
        let tree = session.tree.clone();
        for action in actions {
            match action {
                Action::Select(id) => {
                    let t = tree.read();
                    self.selected = Some(id);
                    self.treemap_root = Some(if t.node(id).is_dir() {
                        id
                    } else {
                        t.parent(id).unwrap_or(id)
                    });
                }
                Action::Reveal(id) => {
                    self.selected = Some(id);
                    self.tree_view.reveal(&tree.read(), id);
                }
                Action::Navigate(id) => {
                    self.selected = Some(id);
                    self.treemap_root = Some(id);
                    self.tree_view.reveal(&tree.read(), id);
                }
                Action::Toggle(id) => self.tree_view.toggle(id),
                Action::Sort(key) => self.tree_view.set_sort(key),
                Action::ShowInFileManager(path) => {
                    if let Err(e) = show_in_file_manager(&path) {
                        self.message = Some((format!("Could not open file manager: {e}"), true));
                    }
                }
                Action::AskDelete(id) => self.confirm_delete = Some(id),
            }
        }
    }

    fn start_delete(&mut self, id: NodeId) {
        let Some(s) = &self.session else { return };
        let path = s.tree.read().path(id);
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(trash::delete(&p).map_err(|e| e.to_string()));
        });
        self.deleting = Some(PendingDelete {
            id,
            path,
            result: rx,
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let scanning = self.session.as_ref().is_some_and(Session::is_scanning);
            let mut picked = None;
            egui::ComboBox::from_id_salt("drive")
                .selected_text("Drives")
                .width(70.0)
                .show_ui(ui, |ui| {
                    for d in &self.drives {
                        if ui.selectable_label(false, drive_label(d)).clicked() {
                            picked = Some(d.root.display().to_string());
                        }
                    }
                });
            if let Some(p) = picked {
                self.path_input = p;
                self.start_scan();
            }
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .hint_text("Folder to scan")
                    .desired_width((ui.available_width() - 330.0).max(160.0)),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if scanning {
                if ui.button("⏹ Stop").clicked() {
                    self.stop_scan();
                }
            } else if ui.button("▶ Scan").clicked() || enter {
                self.start_scan();
            }
            egui::ComboBox::from_id_salt("scanner")
                .selected_text(match self.scanner {
                    ScannerKind::Auto => "Auto",
                    ScannerKind::Win32 => "Win32",
                    ScannerKind::Portable => "Portable",
                })
                .width(80.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.scanner, ScannerKind::Auto, "Auto");
                    if cfg!(windows) {
                        ui.selectable_value(&mut self.scanner, ScannerKind::Win32, "Win32");
                    }
                    ui.selectable_value(&mut self.scanner, ScannerKind::Portable, "Portable");
                })
                .response
                .on_hover_text("Scanner used for the next scan");
            ui.separator();
            if ui
                .selectable_label(!self.allocated, "Size")
                .on_hover_text("Show logical file sizes")
                .clicked()
            {
                self.allocated = false;
            }
            if ui
                .selectable_label(self.allocated, "Allocated")
                .on_hover_text("Show space used on disk")
                .clicked()
            {
                self.allocated = true;
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let total = self.session.as_ref().map(|s| {
            let t = s.tree.read();
            let root = t.node(t.root());
            format!(
                "Total: {} · {} files · {} folders",
                format_size(if self.allocated {
                    root.allocated()
                } else {
                    root.size()
                }),
                format_count(root.files().into()),
                format_count(root.dirs().into())
            )
        });
        let mut open_errors = false;
        egui::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| {
                if let Some((msg, is_err)) = &self.message {
                    let text = RichText::new(msg);
                    ui.label(if *is_err {
                        text.color(ui.visuals().error_fg_color)
                    } else {
                        text
                    });
                    ui.separator();
                }
                if let Some(d) = &self.deleting {
                    ui.label(format!("Deleting {}…", d.path.display()));
                    ui.separator();
                }
                let Some(s) = &self.session else {
                    ui.label("Pick a drive or type a folder, then press Scan.");
                    return;
                };
                let p = &s.progress;
                if p.errors() > 0 {
                    let label = format!("⚠ {} unreadable folders", format_count(p.errors()));
                    if ui.link(label).clicked() {
                        open_errors = true;
                    }
                    ui.separator();
                }
                if s.is_scanning() {
                    // No spinner: it repaints every frame, which would hold the tree lock and
                    // slow the scan down. The view refreshes a few times a second instead.
                    ui.label(format!(
                        "Scanning… {} entries · {} folders · {:.1} s",
                        format_count(p.entries()),
                        format_count(p.dirs()),
                        s.started.elapsed().as_secs_f32()
                    ));
                } else if let Some(st) = &s.stats {
                    let verb = if st.cancelled {
                        "Stopped after"
                    } else {
                        "Scanned"
                    };
                    ui.label(format!(
                        "{verb} {} entries in {:.2} s ({} scanner)",
                        format_count(st.entries),
                        st.elapsed.as_secs_f32(),
                        s.scanner
                    ));
                }
            },
            |ui| {
                if let Some(total) = total {
                    ui.label(total);
                }
            },
        );
        if open_errors {
            self.show_errors = true;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.confirm_delete {
            let Some(s) = &self.session else { return };
            let (path, size, files, is_dir) = {
                let t = s.tree.read();
                let n = t.node(id);
                (t.path(id), n.size(), n.files(), n.is_dir())
            };
            let scanning = s.is_scanning();
            let busy = self.deleting.is_some();
            let mut decision = None;
            let modal = egui::Modal::new(egui::Id::new("confirm-delete")).show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.heading("Move to Recycle Bin?");
                ui.add_space(6.0);
                ui.label(path.display().to_string());
                ui.label(if is_dir {
                    format!(
                        "{} in {} files",
                        format_size(size),
                        format_count(files.into())
                    )
                } else {
                    format_size(size)
                });
                if scanning {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Wait for the scan to finish before deleting.",
                    );
                }
                if busy {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Another delete is still running.",
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!scanning && !busy, egui::Button::new("Move to Recycle Bin"))
                        .clicked()
                    {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            if modal.should_close() && decision.is_none() {
                decision = Some(false);
            }
            match decision {
                Some(true) => {
                    self.confirm_delete = None;
                    self.start_delete(id);
                }
                Some(false) => self.confirm_delete = None,
                None => {}
            }
        }

        if self.show_errors {
            if let Some(s) = &self.session {
                let errors = s.progress.error_list();
                egui::Window::new("Unreadable folders")
                    .open(&mut self.show_errors)
                    .default_size([640.0, 360.0])
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().show_rows(
                            ui,
                            18.0,
                            errors.len(),
                            |ui, range| {
                                for e in &errors[range] {
                                    // One line per error keeps row heights fixed for show_rows.
                                    let text = format!("{}: {}", e.path.display(), e.message);
                                    ui.add(egui::Label::new(&text).truncate())
                                        .on_hover_text(text);
                                }
                            },
                        );
                    });
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_scan();
        self.poll_delete();
        let ctx = ui.ctx().clone();

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        let mut actions = Vec::new();
        if let Some(session) = &self.session {
            let tree = session.tree.read();
            let root = self.treemap_root.unwrap_or(tree.root());
            egui::Panel::right("treemap")
                .resizable(true)
                .default_size(440.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let parent = tree.parent(root);
                        if ui
                            .add_enabled(parent.is_some(), egui::Button::new("⬆ Up"))
                            .clicked()
                        {
                            actions.push(Action::Navigate(
                                parent.expect("button enabled only with a parent"),
                            ));
                        }
                        ui.add(egui::Label::new(tree.path(root).display().to_string()).truncate());
                    });
                    self.treemap
                        .ui(ui, &tree, root, self.selected, self.allocated, &mut actions);
                });
            egui::CentralPanel::default().show(ui, |ui| {
                self.tree_view
                    .ui(ui, &tree, self.selected, self.allocated, &mut actions);
            });
            if session.is_scanning() || self.tree_view.is_stale(&tree) {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("Pick a drive or type a folder, then press Scan.").size(16.0),
                    );
                });
            });
        }
        if self.deleting.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        self.apply(actions);
        self.dialogs(&ctx);
    }
}

fn is_descendant(tree: &tc_model::Tree, id: NodeId, ancestor: NodeId) -> bool {
    let mut cur = tree.parent(id);
    while let Some(p) = cur {
        if p == ancestor {
            return true;
        }
        cur = tree.parent(p);
    }
    false
}

fn drive_label(d: &Drive) -> String {
    let mut s = d.root.display().to_string();
    if !d.label.is_empty() {
        s.push_str(&format!("  {}", d.label));
    }
    if let (Some(total), Some(free)) = (d.total, d.free) {
        s.push_str(&format!(
            "  ({} free of {})",
            format_size(free),
            format_size(total)
        ));
    }
    s
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
