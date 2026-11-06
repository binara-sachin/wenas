//! Directory sessions: the list plus the file views opened from it.
//!
//! Opened files keep their state (edits, undo history) when you go back to the
//! list, so a whole directory can be worked through and saved together.

use super::app::{App, BinaryInfo, ModalText, Outcome, Place, ViewConfig};
use super::chrome::Roots;
use super::dir::{DirAction, DirView, draw_dir};
use super::render;
use super::term::Ui;
use crate::diff::Whitespace;
use crate::dircmp::Status;
use crate::document::{Document, ReadOnlyReason};
use crate::editor::EditorRequest;
use crate::gitenv::mark_readonly;
use crate::input::{self, read_input};
use crate::ops::{DiffPolicy, Session, Side};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

pub struct ShellOptions {
    pub theme: Theme,
    pub view: ViewConfig,
    pub ws: Whitespace,
    pub readonly_left: bool,
    pub readonly_right: bool,
    /// Ask before loading a file larger than this (spec §9).
    pub large_file_bytes: u64,
    /// How diffs run (inline when small, on a worker when large).
    pub diff: DiffPolicy,
}

pub struct Shell {
    pub dir: DirView,
    opts: ShellOptions,
    pub open: BTreeMap<usize, App>,
    pub current: Option<usize>,
    pub quit_modal: bool,
    /// An entry waiting for "open it anyway?" because a side is large.
    pub large_pending: Option<usize>,
    approved_large: std::collections::HashSet<usize>,
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
            large_pending: None,
            approved_large: Default::default(),
            saving_and_quitting: false,
            outcome: None,
            size,
        }
    }

    /// Change the colours, for files opened from now on and the list.
    pub fn set_theme(&mut self, theme: Theme) {
        self.opts.theme = theme;
    }

    fn rel_label(&self, idx: usize) -> String {
        self.dir.entries[idx].rel.display().to_string()
    }

    fn load_side(
        &self,
        base: &Path,
        rel: &Path,
        exists: bool,
        forced: Option<ReadOnlyReason>,
    ) -> Result<Document, String> {
        let path = base.join(rel);
        let mut doc = if exists {
            Document::from_loaded(
                read_input(&path).map_err(|e| format!("{e:#}"))?,
                Some(&path),
            )
        } else {
            // Only on the other side: an empty pane, created on save.
            Document::empty_at(&path)
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
                    utf16: has_bom(&l) || has_bom(&r),
                };
                App::binary_screen(info, self.opts.theme.clone(), size)
            }
            status => {
                let left_exists = *status != Status::RightOnly;
                let right_exists = *status != Status::LeftOnly;
                let mut l = self.load_side(
                    &self.dir.left,
                    &rel,
                    left_exists,
                    self.opts.readonly_left.then_some(ReadOnlyReason::FlagLeft),
                )?;
                let mut r = self.load_side(
                    &self.dir.right,
                    &rel,
                    right_exists,
                    self.opts
                        .readonly_right
                        .then_some(ReadOnlyReason::FlagRight),
                )?;
                let sniff = |d: &Document| {
                    let head: Vec<u8> = (0..d.len().min(256))
                        .flat_map(|i| d.raw_line(i).to_vec())
                        .take(input::BINARY_SNIFF_LEN)
                        .collect();
                    input::looks_binary(&head)
                };
                if sniff(&l) || sniff(&r) {
                    let info = BinaryInfo::from_bytes(
                        rel.display().to_string(),
                        &l.to_bytes(),
                        &r.to_bytes(),
                    );
                    App::binary_screen(info, self.opts.theme.clone(), size)
                } else {
                    let label = rel.display().to_string();
                    l.label = Some(label.clone());
                    r.label = Some(label);
                    App::new(
                        Session::start(l, r, self.opts.ws, self.opts.diff),
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

    /// The largest side of an entry that would be loaded into memory.
    fn load_size(&self, idx: usize) -> Result<u64, String> {
        let e = &self.dir.entries[idx];
        if matches!(
            e.status,
            Status::Binary { .. }
                | Status::Mismatch(_)
                | Status::Error(_)
                | Status::SymlinkDiffers { .. }
        ) {
            return Ok(0); // never loaded
        }
        let mut biggest = 0;
        for base in [&self.dir.left, &self.dir.right] {
            let p = base.join(&e.rel);
            match input::check_size(&p, 0, input::MAX_FILE_BYTES) {
                Ok(input::SizeCheck::Large(n)) => biggest = biggest.max(n),
                Ok(input::SizeCheck::Ok) => {
                    biggest = biggest.max(std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0));
                }
                Err(e) => return Err(format!("{e:#}")),
            }
        }
        Ok(biggest)
    }

    fn open_entry(&mut self, idx: usize) {
        if !self.open.contains_key(&idx) && !self.approved_large.contains(&idx) {
            match self.load_size(idx) {
                Ok(n) if n > self.opts.large_file_bytes => {
                    self.large_pending = Some(idx);
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    self.dir.message = Some(e);
                    self.dir.message_is_error = true;
                    return;
                }
            }
        }
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
        self.update_place();
    }

    /// Tell the open file where it sits in the list, for its title bar. The
    /// list can grow while the scan runs, so this is redone on every draw.
    fn update_place(&mut self) {
        let Some(idx) = self.current else { return };
        let (index, total) = self.dir.file_position(idx).unwrap_or((0, 0));
        let roots = Roots::new(
            &self.dir.left.display().to_string(),
            &self.dir.right.display().to_string(),
        );
        let rel = self.rel_label(idx);
        if let Some(app) = self.open.get_mut(&idx) {
            app.place = Some(Place {
                roots,
                rel,
                index,
                total: total.max(index + 1),
            });
        }
    }

    /// `]f` / `[f` in a file: open the next or previous changed file in the
    /// list, without going back to it first.
    fn step_file(&mut self, idx: usize, forward: bool) {
        let Some(next) = self.dir.change_after(idx, forward) else {
            if let Some(app) = self.open.get_mut(&idx) {
                app.message = Some(if forward {
                    "this is the last changed file".into()
                } else {
                    "this is the first changed file".into()
                });
            }
            return;
        };
        self.close_current();
        self.dir.select_entry(next);
        self.open_entry(next);
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
            Outcome::Back | Outcome::Identical => {
                self.saving_and_quitting = false;
                // A cancelled first diff leaves nothing worth keeping: reopening
                // the entry starts over.
                if self.open.get(&idx).is_some_and(|a| a.diff_cancelled) {
                    self.open.remove(&idx);
                }
                self.close_current();
            }
            Outcome::NextFile | Outcome::PrevFile => {
                self.saving_and_quitting = false;
                self.step_file(idx, o == Outcome::NextFile);
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
        if self.current.is_some() {
            self.update_place();
        }
        if let Some(app) = self.current.and_then(|i| self.open.get_mut(&i)) {
            render::draw(f, app);
            return;
        }
        self.sync_unsaved();
        draw_dir(f.buffer_mut(), &mut self.dir, &self.opts.theme, area);
        if let Some(idx) = self.large_pending {
            let size = self.load_size(idx).unwrap_or(0);
            let text = ModalText {
                title: format!("{} is {}", self.rel_label(idx), input::human_size(size)),
                body: vec!["Loading it may be slow and use a lot of memory.".into()],
                hints: vec![("y", "Open anyway"), ("Esc", "Cancel")],
            };
            render::draw_modal_panel(f.buffer_mut(), &self.opts.theme, area, &text);
        } else if self.quit_modal {
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
        if let Some(idx) = self.large_pending {
            match key.code {
                KeyCode::Char('y') => {
                    self.large_pending = None;
                    self.approved_large.insert(idx);
                    self.open_entry(idx);
                }
                KeyCode::Esc | KeyCode::Char('n') => self.large_pending = None,
                _ => {}
            }
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

    fn on_scroll(&mut self, lines: isize, columns: isize) -> bool {
        if self.large_pending.is_some() || self.quit_modal {
            return false;
        }
        match self.current.and_then(|i| self.open.get_mut(&i)) {
            Some(app) => app.on_scroll(lines, columns),
            None => self.dir.on_scroll(lines),
        }
    }

    fn on_mouse(&mut self, event: MouseEvent) -> bool {
        if self.large_pending.is_some() || self.quit_modal {
            return false;
        }
        if let Some(idx) = self.current {
            let was_saving = self.saving_and_quitting;
            let acted = self.open.get_mut(&idx).is_some_and(|a| a.on_mouse(event));
            // A click can go back to the list (the back control).
            self.handle_app_outcome(idx, was_saving);
            return acted;
        }
        match self.dir.on_mouse(event, Instant::now()) {
            Some(DirAction::Open(i)) => {
                self.open_entry(i);
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn on_paste(&mut self, text: &str) -> bool {
        if self.large_pending.is_some() || self.quit_modal {
            return false;
        }
        match self.current.and_then(|i| self.open.get_mut(&i)) {
            Some(app) => app.on_paste(text),
            None => self.dir.on_paste(text),
        }
    }

    fn tick(&mut self) {
        self.dir.poll();
        if let Some(app) = self.current.and_then(|i| self.open.get_mut(&i)) {
            app.tick();
        }
    }

    fn polling(&self) -> bool {
        match self.current.and_then(|i| self.open.get(&i)) {
            Some(app) => app.session.is_busy(),
            None => self.dir.animating(),
        }
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

/// Does the file start with a UTF-16 byte-order mark?
fn has_bom(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && input::has_utf16_bom(&head)
}
