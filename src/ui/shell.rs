//! Directory sessions: the list plus the file views opened from it.
//!
//! Opened files keep their state (edits, undo history) when you go back to the
//! list, so a whole directory can be worked through and saved together.

use super::app::{App, BinaryInfo, EditorRequest, ModalText, Outcome, ViewConfig};
use super::dir::{DirAction, DirView, draw_dir};
use super::render;
use super::term::Ui;
use crate::diff::Whitespace;
use crate::dircmp::Status;
use crate::document::Document;
use crate::gitenv::mark_readonly;
use crate::input::{self, Loaded, Source, read_input};
use crate::ops::{Session, Side};
use crate::save::document_from_loaded;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::BTreeMap;
use std::path::Path;

pub struct ShellOptions {
    pub theme: Theme,
    pub view: ViewConfig,
    pub ws: Whitespace,
    pub readonly_left: bool,
    pub readonly_right: bool,
}

pub struct Shell {
    pub dir: DirView,
    opts: ShellOptions,
    pub open: BTreeMap<usize, App>,
    pub current: Option<usize>,
    pub quit_modal: bool,
    /// "Save both and quit" is in progress.
    saving_and_quitting: bool,
    pub outcome: Option<Outcome>,
    size: (u16, u16),
}

impl Shell {
    pub fn new(dir: DirView, opts: ShellOptions, size: (u16, u16)) -> Shell {
        Shell {
            dir,
            opts,
            open: BTreeMap::new(),
            current: None,
            quit_modal: false,
            saving_and_quitting: false,
            outcome: None,
            size,
        }
    }

    fn rel_label(&self, idx: usize) -> String {
        self.dir.entries[idx].rel.display().to_string()
    }

    fn load_side(
        &self,
        base: &Path,
        rel: &Path,
        exists: bool,
        forced: Option<&'static str>,
    ) -> Result<Document, String> {
        let path = base.join(rel);
        let mut doc = if exists {
            let loaded = read_input(&path).map_err(|e| format!("{e:#}"))?;
            document_from_loaded(loaded, Some(&path))
        } else {
            // Only on the other side: an empty pane, created on save.
            let mut d = document_from_loaded(
                Loaded {
                    bytes: vec![],
                    source: Source::File,
                },
                Some(&path),
            );
            d.disk = None;
            d
        };
        mark_readonly(&mut doc, &path, forced);
        Ok(doc)
    }

    /// Build the file view (or binary screen) for an entry.
    fn build_app(&self, idx: usize) -> Result<App, String> {
        let entry = &self.dir.entries[idx];
        let rel = entry.rel.clone();
        let size = self.size;
        let mut app = match &entry.status {
            Status::Mismatch(m) => return Err(format!("can't compare: {m}")),
            Status::SymlinkDiffers { .. } => {
                return Err(format!("symlinks differ: {}", entry.status.detail()));
            }
            Status::Error(e) => return Err(e.clone()),
            Status::Binary { first_diff } => {
                let (l, r) = (self.dir.left.join(&rel), self.dir.right.join(&rel));
                let sz = |p: &Path| std::fs::metadata(p).map(|m| m.len() as usize).unwrap_or(0);
                let info = BinaryInfo {
                    name: rel.display().to_string(),
                    left_size: sz(&l),
                    right_size: sz(&r),
                    first_difference: Some(*first_diff as usize),
                    utf16: false,
                };
                App::binary_screen(info, self.opts.theme.clone(), size)
            }
            status => {
                let left_exists = *status != Status::RightOnly;
                let right_exists = *status != Status::LeftOnly;
                let flag = |on: bool, name: &'static str| on.then_some(name);
                let mut l = self.load_side(
                    &self.dir.left,
                    &rel,
                    left_exists,
                    flag(self.opts.readonly_left, "marked --readonly-left"),
                )?;
                let mut r = self.load_side(
                    &self.dir.right,
                    &rel,
                    right_exists,
                    flag(self.opts.readonly_right, "marked --readonly-right"),
                )?;
                let sniff = |d: &Document| {
                    let head: Vec<u8> = (0..d.len().min(256))
                        .flat_map(|i| d.raw_line(i).to_vec())
                        .take(input::BINARY_SNIFF_LEN)
                        .collect();
                    input::is_binary(&head) || input::has_utf16_bom(&head)
                };
                if sniff(&l) || sniff(&r) {
                    let info = BinaryInfo {
                        name: rel.display().to_string(),
                        left_size: l.to_bytes().len(),
                        right_size: r.to_bytes().len(),
                        first_difference: Some(
                            input::first_difference(&l.to_bytes(), &r.to_bytes()).unwrap_or(0),
                        ),
                        utf16: false,
                    };
                    App::binary_screen(info, self.opts.theme.clone(), size)
                } else {
                    let label = rel.display().to_string();
                    l.label = Some(label.clone());
                    r.label = Some(label);
                    App::new(
                        Session::new(l, r, self.opts.ws),
                        self.opts.theme.clone(),
                        self.opts.view.clone(),
                        size,
                    )
                }
            }
        };
        app.in_directory = true;
        Ok(app)
    }

    fn open_entry(&mut self, idx: usize) {
        if !self.open.contains_key(&idx) {
            match self.build_app(idx) {
                Ok(app) => {
                    self.open.insert(idx, app);
                }
                Err(e) => {
                    self.dir.message = Some(e);
                    self.dir.message_is_error = true;
                    return;
                }
            }
        }
        self.current = Some(idx);
    }

    /// Back to the list, reflecting what happened to the file.
    fn close_current(&mut self) {
        let Some(idx) = self.current.take() else {
            return;
        };
        self.sync_unsaved();
        if let Some(app) = self.open.get(&idx) {
            let done = app.binary.is_none()
                && app.session.hunks.is_empty()
                && !app.session.is_dirty()
                && app.session.ws == Whitespace::Off;
            if done {
                self.dir.mark_identical(idx);
            }
        }
    }

    fn sync_unsaved(&mut self) {
        self.dir.unsaved = self
            .open
            .iter()
            .filter(|(_, a)| a.session.is_dirty())
            .map(|(&i, _)| i)
            .collect();
    }

    fn dirty_files(&self) -> Vec<usize> {
        self.open
            .iter()
            .filter(|(_, a)| a.session.is_dirty())
            .map(|(&i, _)| i)
            .collect()
    }

    fn ask_quit(&mut self) {
        self.sync_unsaved();
        if self.dirty_files().is_empty() {
            self.outcome = Some(Outcome::Quit);
        } else {
            self.quit_modal = true;
        }
    }

    /// Save each dirty file in turn; a conflict switches to that file's own question.
    fn continue_save_and_quit(&mut self) {
        self.saving_and_quitting = true;
        for idx in self.dirty_files() {
            let app = self.open.get_mut(&idx).unwrap();
            app.save_all(true);
            if app.modal.is_some() || app.message_is_error {
                self.current = Some(idx);
                return;
            }
            app.outcome = None;
        }
        self.sync_unsaved();
        self.saving_and_quitting = false;
        self.outcome = Some(Outcome::Quit);
    }

    pub fn quit_text(&self) -> ModalText {
        let files = self.dirty_files();
        let title = match files.len() {
            1 => "1 file has unsaved changes".to_string(),
            n => format!("{n} files have unsaved changes"),
        };
        let body = files
            .iter()
            .map(|&i| {
                let app = &self.open[&i];
                let n = app.session.edit_count(Side::Left) + app.session.edit_count(Side::Right);
                format!(
                    "{}   {n} {}",
                    self.rel_label(i),
                    if n == 1 { "edit" } else { "edits" }
                )
            })
            .collect();
        let save = if files.len() > 1 {
            "Save both and quit"
        } else {
            "Save and quit"
        };
        ModalText {
            title,
            body,
            hints: vec![
                ("w", save),
                ("Q", "Quit without saving"),
                ("Esc", "Keep working"),
            ],
        }
    }

    fn handle_app_outcome(&mut self, idx: usize, was_saving: bool) {
        let Some(o) = self.open.get_mut(&idx).and_then(|a| a.outcome.take()) else {
            self.saving_and_quitting = false;
            return;
        };
        match o {
            Outcome::Quit | Outcome::Abort => self.outcome = Some(o),
            Outcome::Back => {
                self.saving_and_quitting = false;
                self.close_current();
            }
            Outcome::QuitAsk if was_saving => self.continue_save_and_quit(),
            Outcome::QuitAsk => {
                self.saving_and_quitting = false;
                self.close_current();
                self.ask_quit();
            }
        }
    }

    fn on_quit_modal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('w') => {
                self.quit_modal = false;
                self.continue_save_and_quit();
            }
            KeyCode::Char('Q') => self.outcome = Some(Outcome::Quit),
            KeyCode::Esc => self.quit_modal = false,
            _ => {}
        }
    }
}

impl Ui for Shell {
    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.size = (area.width, area.height);
        if let Some(app) = self.current.and_then(|i| self.open.get_mut(&i)) {
            render::draw(f, app);
            return;
        }
        self.sync_unsaved();
        draw_dir(f.buffer_mut(), &mut self.dir, &self.opts.theme, area);
        if self.quit_modal {
            let text = self.quit_text();
            render::draw_modal_panel(f.buffer_mut(), &self.opts.theme, area, &text);
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.outcome = Some(Outcome::Abort);
            return;
        }
        if self.quit_modal {
            self.on_quit_modal_key(key);
            return;
        }
        if let Some(idx) = self.current {
            let was_saving = self.saving_and_quitting;
            if let Some(app) = self.open.get_mut(&idx) {
                app.on_key(key);
            }
            self.handle_app_outcome(idx, was_saving);
            return;
        }
        match self.dir.on_key(key) {
            DirAction::None => {}
            DirAction::Open(i) => self.open_entry(i),
            DirAction::Quit => self.ask_quit(),
            DirAction::Abort => self.outcome = Some(Outcome::Abort),
        }
    }

    fn tick(&mut self) {
        self.dir.poll();
    }

    fn polling(&self) -> bool {
        self.current.is_none() && self.dir.animating()
    }

    fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    fn editor_request(&mut self) -> Option<EditorRequest> {
        self.current
            .and_then(|i| self.open.get_mut(&i))
            .and_then(|a| a.editor_request.take())
    }

    fn finish_editor(&mut self, side: Side, result: Result<bool, String>) {
        if let Some(app) = self.current.and_then(|i| self.open.get_mut(&i)) {
            app.finish_editor(side, result);
        }
    }
}
