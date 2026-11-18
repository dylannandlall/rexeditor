//! Terminal user interface (`rexeditor --tui`).
//!
//! A keyboard- and mouse-driven front end over the same `rex-core` engine as the GUI.
//! It only needs a terminal, so it works over SSH and on server installs without any
//! graphical stack. Build a TUI-only binary with:
//!
//! ```text
//! cargo build --release -p rex-gui --no-default-features --features tui
//! ```

mod view;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Position, Rect};
use rex_core::search::{self, SearchMessage, SearchWorker};
use rex_core::{
    CopyFormat, Document, EditKind, Endianness, Field, FieldColor, SearchMatch, Selection,
    cycle_field_background,
};

use crate::cli::{LaunchOptions, apply_offset_with_bounds_notice};

/// Which column keyboard edits go to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Column {
    #[default]
    Hex,
    Ascii,
}

impl Column {
    fn toggle(self) -> Self {
        match self {
            Self::Hex => Self::Ascii,
            Self::Ascii => Self::Hex,
        }
    }
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub current: usize,
    pub running: bool,
    pub scanned: usize,
    pub total: usize,
    worker: Option<SearchWorker>,
}

pub struct Tab {
    pub doc: Document,
    pub search: SearchState,
    pub column: Column,
}

impl Tab {
    fn new(doc: Document) -> Self {
        Self {
            doc,
            search: SearchState::default(),
            column: Column::Hex,
        }
    }

    fn start_search(&mut self, query: String) {
        if let Some(worker) = self.search.worker.take() {
            worker.cancel();
        }
        self.search = SearchState {
            query: query.clone(),
            total: self.doc.buffer.len(),
            ..SearchState::default()
        };
        match search::spawn(self.doc.buffer.clone(), query.clone()) {
            Ok(worker) => {
                self.search.worker = Some(worker);
                self.search.running = true;
                self.doc.status = format!("Searching for '{query}'...");
            }
            Err(err) => self.doc.status = err,
        }
    }

    /// Drains search results; returns true if anything changed.
    fn poll_search(&mut self) -> bool {
        let Some(worker) = &self.search.worker else {
            return false;
        };
        let mut changed = false;
        let mut finished = false;
        let messages: Vec<SearchMessage> = worker.receiver.try_iter().collect();
        for msg in messages {
            changed = true;
            match msg {
                SearchMessage::Batch(batch) => {
                    let first = self.search.matches.is_empty() && !batch.is_empty();
                    self.search.matches.extend(batch);
                    if first {
                        self.jump_to_match(0);
                    }
                }
                SearchMessage::Progress(n) => self.search.scanned = n,
                SearchMessage::Done => finished = true,
                SearchMessage::Error(err) => {
                    self.doc.status = err;
                    finished = true;
                }
            }
        }
        if finished {
            self.search.worker = None;
            self.search.running = false;
            if self.search.matches.is_empty() {
                self.doc.status = format!("No matches for '{}'", self.search.query);
            } else {
                self.doc.status = format!(
                    "{} match(es) for '{}' (n / N to step)",
                    self.search.matches.len(),
                    self.search.query
                );
            }
        }
        changed
    }

    fn jump_to_match(&mut self, index: usize) {
        if let Some(m) = self.search.matches.get(index).copied() {
            self.search.current = index;
            self.doc.selection = Some(Selection::from_range(m.start, m.end));
            self.doc.additional_selections.clear();
            self.doc.ensure_visible(m.start);
            self.doc.status = format!(
                "Match {}/{} at 0x{:X}..0x{:X}",
                index + 1,
                self.search.matches.len(),
                m.start,
                m.end
            );
        }
    }

    fn step_match(&mut self, forward: bool) {
        let n = self.search.matches.len();
        if n == 0 {
            self.doc.status = "No search results (press / to search)".into();
            return;
        }
        let next = if forward {
            (self.search.current + 1) % n
        } else {
            self.search.current.checked_sub(1).unwrap_or(n - 1)
        };
        self.jump_to_match(next);
    }

    fn select_field(&mut self, index: usize) {
        if let Some(f) = self.doc.fields.get(index) {
            let (start, end) = (f.start, f.end);
            self.doc.selected_field = index;
            self.doc.select_offset(start, false);
            if let Some(sel) = &mut self.doc.selection {
                sel.cursor = end;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Jump,
    Search,
    Open,
    SaveAs,
    AddField,
    RenameField(usize),
}

impl PromptKind {
    fn label(self) -> &'static str {
        match self {
            Self::Jump => "Jump to offset",
            Self::Search => "Search",
            Self::Open => "Open file",
            Self::SaveAs => "Save as",
            Self::AddField => "New field name",
            Self::RenameField(_) => "Rename field",
        }
    }
}

/// A single-line text input.
#[derive(Default)]
pub struct LineInput {
    pub text: String,
    /// Cursor position in chars.
    pub cursor: usize,
}

impl LineInput {
    fn with_text(text: String) -> Self {
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    fn byte_index(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn insert_str(&mut self, s: &str) {
        for ch in s.chars().filter(|c| !c.is_control()) {
            let at = self.byte_index(self.cursor);
            self.text.insert(at, ch);
            self.cursor += 1;
        }
    }

    /// Returns true when the key was consumed.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let len = self.text.chars().count();
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut buf = [0u8; 4];
                self.insert_str(c.encode_utf8(&mut buf));
            }
            KeyCode::Char('u') => {
                let at = self.byte_index(self.cursor);
                self.text.drain(..at);
                self.cursor = 0;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_index(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte_index(self.cursor);
                self.text.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: LineInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Hex,
    Fields,
}

/// Screen geometry from the last frame, used for mouse hit-testing.
#[derive(Clone, Copy, Debug, Default)]
pub struct HexGeometry {
    /// Inner area of the hex block (no borders).
    pub area: Rect,
    pub off_w: u16,
    pub bpr: usize,
}

impl HexGeometry {
    pub fn hex_x(off_w: u16, i: usize) -> u16 {
        off_w + 2 + (i * 3 + i / 8) as u16
    }

    pub fn ascii_x(off_w: u16, bpr: usize, i: usize) -> u16 {
        Self::hex_x(off_w, bpr.saturating_sub(1)) + 4 + i as u16
    }

    /// Columns needed to draw rows of `bpr` bytes.
    pub fn width(off_w: u16, bpr: usize) -> u16 {
        Self::ascii_x(off_w, bpr, bpr)
    }

    /// Byte column and area under the relative x coordinate `x`.
    pub fn hit(&self, x: u16) -> Option<(usize, Column)> {
        if self.bpr == 0 {
            return None;
        }
        for i in 0..self.bpr {
            let hx = Self::hex_x(self.off_w, i);
            if x >= hx && x < hx + 3 {
                return Some((i, Column::Hex));
            }
        }
        let a0 = Self::ascii_x(self.off_w, self.bpr, 0);
        if x >= a0 && x < a0 + self.bpr as u16 {
            return Some(((x - a0) as usize, Column::Ascii));
        }
        None
    }

    /// Like `hit`, but snaps to the nearest edge column (used while dragging).
    pub fn hit_clamped(&self, x: u16, fallback: Column) -> (usize, Column) {
        self.hit(x).unwrap_or_else(|| {
            if x < Self::hex_x(self.off_w, 0) {
                (0, fallback)
            } else {
                (self.bpr.saturating_sub(1), fallback)
            }
        })
    }
}

/// Where the overlay field list was drawn, for mouse clicks.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldListGeometry {
    pub area: Rect,
    pub first_index: usize,
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub prompt: Option<Prompt>,
    pub show_help: bool,
    pub help_scroll: u16,
    pub focus: Focus,
    pub field_cursor: usize,
    pub show_panel: bool,
    /// Bytes per row chosen by the user (`None` = fit to the terminal).
    pub manual_bpr: Option<usize>,
    pub endian: Endianness,
    /// While set, cursor movement extends the selection (vim-style visual mode).
    pub visual: bool,
    pub message: String,
    pub truecolor: bool,
    pub hex_geo: HexGeometry,
    pub fields_geo: FieldListGeometry,
    quit_armed: bool,
    should_quit: bool,
    /// Last pointer position of an in-progress mouse drag selection.
    drag: Option<Position>,
    /// Text queued for the terminal clipboard (OSC 52), written after the frame.
    pending_clipboard: Option<String>,
}

pub fn run(options: LaunchOptions) -> io::Result<()> {
    let mut app = App::new(options);
    let mut terminal = ratatui::try_init()?;
    let setup = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    let result = setup.and_then(|()| app.event_loop(&mut terminal));
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
    result
}

impl App {
    pub fn new(options: LaunchOptions) -> Self {
        let mut tabs = Vec::new();
        let mut message = options
            .cli_error
            .clone()
            .unwrap_or_else(|| "Press ? for help, q to quit".into());
        for path in options.paths {
            match Document::open(path.clone()) {
                Ok(mut doc) => {
                    if let Some(offset) = options.initial_offset
                        && let Some(warning) = apply_offset_with_bounds_notice(&mut doc, offset)
                    {
                        message = warning;
                    }
                    tabs.push(Tab::new(doc));
                }
                Err(err) => message = format!("Failed to open {}: {err}", path.display()),
            }
        }
        Self {
            tabs,
            active: 0,
            prompt: None,
            show_help: false,
            help_scroll: 0,
            focus: Focus::Hex,
            field_cursor: 0,
            show_panel: true,
            manual_bpr: None,
            endian: Endianness::Little,
            visual: false,
            message,
            truecolor: detect_truecolor(),
            hex_geo: HexGeometry::default(),
            fields_geo: FieldListGeometry::default(),
            quit_armed: false,
            should_quit: false,
            drag: None,
            pending_clipboard: None,
        }
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> io::Result<()> {
        let mut dirty = true;
        while !self.should_quit {
            if dirty {
                let size = terminal.size()?;
                self.prepare_frame(Rect::new(0, 0, size.width, size.height));
                terminal.draw(|frame| view::render(frame, self))?;
                if let Some(text) = self.pending_clipboard.take() {
                    copy_to_terminal_clipboard(&text);
                }
                dirty = false;
            }

            let busy = self.tabs.iter().any(|t| t.search.running) || self.drag.is_some();
            let timeout = if busy {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(500)
            };
            if event::poll(timeout)? {
                // Handle everything already queued before redrawing (keeps key repeat
                // and mouse drags responsive on slow links).
                loop {
                    dirty |= self.handle_event(event::read()?);
                    if self.should_quit || !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            } else {
                dirty |= self.tick();
            }
            for tab in &mut self.tabs {
                dirty |= tab.poll_search();
            }
        }
        Ok(())
    }

    /// Picks bytes-per-row for the terminal size and records the visible row count so
    /// scrolling math matches what will be drawn.
    pub fn prepare_frame(&mut self, area: Rect) {
        let layout = view::compute_layout(self, area);
        self.hex_geo = layout.geometry;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let doc = &mut tab.doc;
            doc.visible_rows = layout.geometry.area.height.max(1) as usize;
            if doc.settings.bytes_per_row != layout.geometry.bpr {
                doc.settings.bytes_per_row = layout.geometry.bpr;
                doc.rebuild_display_rows();
                if let Some(sel) = doc.selection {
                    doc.ensure_visible(sel.cursor);
                }
            }
            doc.scroll = doc.scroll.min(doc.max_scroll());
        }
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        match self.tabs.get_mut(self.active) {
            Some(tab) => tab.doc.status = msg,
            None => self.message = msg,
        }
    }

    /// Periodic work while idle: continues edge auto-scroll during a mouse drag.
    fn tick(&mut self) -> bool {
        match self.drag {
            Some(pos) if self.pointer_in_scroll_zone(pos) => {
                self.drag_to(pos);
                true
            }
            _ => false,
        }
    }

    fn pointer_in_scroll_zone(&self, pos: Position) -> bool {
        let area = self.hex_geo.area;
        pos.y <= area.y || pos.y + 1 >= area.y + area.height
    }

    /// Returns true when the screen needs to be redrawn.
    pub fn handle_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.handle_key(key);
                true
            }
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::Paste(text) => {
                self.handle_paste(&text);
                true
            }
            Event::Resize(..) => true,
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if !(matches!(key.code, KeyCode::Char('q')) || (ctrl && key.code == KeyCode::Char('q'))) {
            self.quit_armed = false;
        }

        if self.prompt.is_some() {
            self.handle_prompt_key(key);
            return;
        }
        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q') | KeyCode::F(1) => {
                    self.show_help = false;
                    self.help_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = self.help_scroll.saturating_add(1).min(60)
                }
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10).min(60),
                _ => {}
            }
            return;
        }

        // Global shortcuts (work in every mode).
        if ctrl {
            let handled = match key.code {
                KeyCode::Char('q') => {
                    self.request_quit();
                    true
                }
                KeyCode::Char('o') => {
                    self.open_prompt(PromptKind::Open, String::new());
                    true
                }
                KeyCode::Char('s') => {
                    self.save();
                    true
                }
                KeyCode::Char('g') => {
                    self.open_prompt(PromptKind::Jump, String::new());
                    true
                }
                KeyCode::Char('f') => {
                    let q = self
                        .tab()
                        .map(|t| t.search.query.clone())
                        .unwrap_or_default();
                    self.open_prompt(PromptKind::Search, q);
                    true
                }
                KeyCode::Char('c') => {
                    self.copy(CopyFormat::HexCompact, "hex");
                    true
                }
                KeyCode::Char('z') => {
                    if let Some(t) = self.tab_mut() {
                        t.doc.undo();
                    }
                    true
                }
                KeyCode::Char('y' | 'r') => {
                    if let Some(t) = self.tab_mut() {
                        t.doc.redo();
                    }
                    true
                }
                KeyCode::Char('w') => {
                    self.close_tab();
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }
        if key.code == KeyCode::F(1) {
            self.show_help = true;
            return;
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if (ctrl || alt) && matches!(key.code, KeyCode::Left | KeyCode::Right) {
            self.switch_tab(key.code == KeyCode::Right);
            return;
        }

        if self.tabs.is_empty() {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                KeyCode::Char('o') => self.open_prompt(PromptKind::Open, String::new()),
                KeyCode::Char('?') => self.show_help = true,
                _ => {}
            }
            return;
        }

        if self.focus == Focus::Fields {
            self.handle_fields_key(key);
            return;
        }
        self.handle_hex_key(key);
    }

    fn handle_hex_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let visual = self.visual;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let bpr = tab.doc.settings.bytes_per_row.max(1) as isize;
        let page = tab.doc.visible_rows.max(1) as isize * bpr;
        let extend = shift || visual;
        let edit_mode = tab.doc.edit_mode;

        let movement = |doc: &mut Document, delta: isize| {
            doc.commit_pending_edit();
            doc.move_cursor(delta, extend);
        };
        match key.code {
            KeyCode::Left => movement(&mut tab.doc, -1),
            KeyCode::Right => movement(&mut tab.doc, 1),
            KeyCode::Up => movement(&mut tab.doc, -bpr),
            KeyCode::Down => movement(&mut tab.doc, bpr),
            KeyCode::PageUp => movement(&mut tab.doc, -page),
            KeyCode::PageDown => movement(&mut tab.doc, page),
            KeyCode::Home if ctrl => {
                tab.doc.commit_pending_edit();
                tab.doc.select_offset(0, extend);
            }
            KeyCode::End if ctrl => {
                tab.doc.commit_pending_edit();
                tab.doc
                    .select_offset(tab.doc.buffer.len().saturating_sub(1), extend);
            }
            KeyCode::Home => {
                let cur = tab.doc.selection.map_or(0, |s| s.cursor);
                tab.doc.commit_pending_edit();
                tab.doc.select_offset(cur - cur % bpr as usize, extend);
            }
            KeyCode::End => {
                let cur = tab.doc.selection.map_or(0, |s| s.cursor);
                tab.doc.commit_pending_edit();
                tab.doc
                    .select_offset(cur - cur % bpr as usize + bpr as usize - 1, extend);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                tab.doc.commit_pending_edit();
                tab.column = tab.column.toggle();
            }
            KeyCode::Insert => {
                tab.doc.commit_pending_edit();
                if tab.doc.edit_mode {
                    tab.doc.edit_kind = tab.doc.edit_kind.toggle();
                } else {
                    tab.doc.edit_mode = true;
                    tab.doc.edit_kind = EditKind::Insert;
                }
            }
            KeyCode::Esc => {
                if tab.doc.edit_mode {
                    tab.doc.commit_pending_edit();
                    tab.doc.edit_mode = false;
                    tab.doc.status = "View Mode".into();
                } else if self.visual {
                    self.visual = false;
                    tab.doc.status = "Visual selection off".into();
                } else if let Some(sel) = tab.doc.selection {
                    tab.doc.select_offset(sel.cursor, false);
                }
            }
            KeyCode::F(3) => tab.step_match(!shift),
            KeyCode::Backspace | KeyCode::Delete if edit_mode => tab.doc.delete_selected_bytes(),
            // Chords like Alt+q or Ctrl+x are never typed text.
            KeyCode::Char(_) if ctrl || key.modifiers.contains(KeyModifiers::ALT) => {}
            KeyCode::Char(ch) if edit_mode => match tab.column {
                Column::Hex => {
                    if let Some(nibble) = ch.to_digit(16) {
                        tab.doc.edit_nibble(nibble as u8);
                    } else if ch == 'i' {
                        tab.doc.cycle_edit_mode();
                    }
                }
                Column::Ascii => {
                    if ch.is_ascii() && !ch.is_ascii_control() {
                        tab.doc.edit_ascii_char(ch);
                    }
                }
            },
            KeyCode::Char(ch) => self.handle_view_char(ch),
            _ => {}
        }
    }

    /// Single-key commands in View Mode.
    fn handle_view_char(&mut self, ch: char) {
        let visual = self.visual;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let bpr = tab.doc.settings.bytes_per_row.max(1) as isize;
        match ch {
            'h' => tab.doc.move_cursor(-1, visual),
            'l' => tab.doc.move_cursor(1, visual),
            'k' => tab.doc.move_cursor(-bpr, visual),
            'j' => tab.doc.move_cursor(bpr, visual),
            'g' => tab.doc.select_offset(0, visual),
            'G' => tab
                .doc
                .select_offset(tab.doc.buffer.len().saturating_sub(1), visual),
            'i' => tab.doc.cycle_edit_mode(),
            'n' => tab.step_match(true),
            'N' => tab.step_match(false),
            'u' => tab.doc.undo(),
            '[' | ']' => {
                if !tab.doc.fields.is_empty() {
                    let n = tab.doc.fields.len();
                    let cur = tab.doc.selected_field.min(n - 1);
                    let idx = if ch == ']' {
                        (cur + 1) % n
                    } else {
                        cur.checked_sub(1).unwrap_or(n - 1)
                    };
                    tab.select_field(idx);
                    self.field_cursor = idx;
                }
            }
            'v' => {
                self.visual = !self.visual;
                let msg = if self.visual {
                    "Visual selection on: movement extends the selection (v or Esc to stop)"
                } else {
                    "Visual selection off"
                };
                tab.doc.status = msg.into();
            }
            'y' => self.copy(CopyFormat::HexCompact, "hex"),
            'Y' => self.copy(CopyFormat::HexSpaced, "spaced hex"),
            'A' => self.copy(CopyFormat::Ascii, "ASCII"),
            '/' => {
                let q = tab.search.query.clone();
                self.open_prompt(PromptKind::Search, q);
            }
            ':' => self.open_prompt(PromptKind::Jump, String::new()),
            'o' => self.open_prompt(PromptKind::Open, String::new()),
            'S' => {
                let p = tab.doc.path.display().to_string();
                self.open_prompt(PromptKind::SaveAs, p);
            }
            'a' => {
                let name = format!("field_{}", tab.doc.fields.len() + 1);
                self.open_prompt(PromptKind::AddField, name);
            }
            'f' => {
                self.focus = Focus::Fields;
                self.show_panel = true;
                self.field_cursor = tab
                    .doc
                    .selected_field
                    .min(tab.doc.fields.len().saturating_sub(1));
                tab.doc.status =
                    "Fields: ↑/↓ choose · Space show/hide · b/t/c border/text/background color · Enter select · r rename · d delete · Esc back".into();
            }
            'e' => {
                self.endian = match self.endian {
                    Endianness::Little => Endianness::Big,
                    Endianness::Big => Endianness::Little,
                };
            }
            'p' => self.show_panel = !self.show_panel,
            '+' | '=' => self.change_bpr(true),
            '-' => self.change_bpr(false),
            '0' => {
                self.manual_bpr = None;
                self.set_status("Bytes per row: fit to terminal");
            }
            '?' => self.show_help = true,
            'q' => self.request_quit(),
            _ => {}
        }
    }

    fn handle_fields_key(&mut self, key: KeyEvent) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let n = tab.doc.fields.len();
        if n == 0 {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('f' | 'q')) {
                self.focus = Focus::Hex;
            } else if key.code == KeyCode::Char('a') {
                let name = format!("field_{}", n + 1);
                self.focus = Focus::Hex;
                self.open_prompt(PromptKind::AddField, name);
            }
            return;
        }
        let i = self.field_cursor.min(n - 1);
        let mut changed = false;
        match key.code {
            KeyCode::Esc | KeyCode::Char('f' | 'q') => {
                self.focus = Focus::Hex;
                tab.doc.status = "View Mode".into();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.field_cursor = i.checked_sub(1).unwrap_or(n - 1)
            }
            KeyCode::Down | KeyCode::Char('j') => self.field_cursor = (i + 1) % n,
            KeyCode::Home => self.field_cursor = 0,
            KeyCode::End => self.field_cursor = n - 1,
            KeyCode::Enter => {
                tab.select_field(i);
                self.focus = Focus::Hex;
            }
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                let f = &mut tab.doc.fields[i];
                f.visible = !f.visible;
                tab.doc.status = format!(
                    "Field '{}' {}",
                    f.name,
                    if f.visible { "shown" } else { "hidden" }
                );
                changed = true;
            }
            KeyCode::Char(c @ ('b' | 'B' | 't' | 'T' | 'c' | 'C')) => {
                // Lowercase cycles forward, uppercase (Shift) backward.
                let forward = !c.is_ascii_uppercase();
                let f = &mut tab.doc.fields[i];
                let (slot, what) = match c.to_ascii_lowercase() {
                    'b' => (&mut f.border, "Border"),
                    't' => (&mut f.text, "Text"),
                    _ => (&mut f.background, "Background"),
                };
                *slot = cycle_field_background(*slot, forward);
                let shown = slot.map_or("none", FieldColor::name);
                tab.doc.status = format!("{what} color of '{}': {shown}", f.name);
                changed = true;
            }
            KeyCode::Char('r') => {
                let name = tab.doc.fields[i].name.clone();
                self.open_prompt(PromptKind::RenameField(i), name);
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                let name = tab.doc.fields[i].name.clone();
                tab.doc.delete_field(i);
                tab.doc.status = format!("Deleted field '{name}'");
                self.field_cursor = i.min(tab.doc.fields.len().saturating_sub(1));
                changed = true;
            }
            KeyCode::Char('a') => {
                let name = format!("field_{}", n + 1);
                self.focus = Focus::Hex;
                self.open_prompt(PromptKind::AddField, name);
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
        if changed {
            self.persist_overlay();
        }
    }

    fn persist_overlay(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active)
            && let Err(err) = tab.doc.persist_automatic_overlay()
        {
            tab.doc.status = format!("Could not save overlay: {err}");
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, initial: String) {
        self.prompt = Some(Prompt {
            kind,
            input: LineInput::with_text(initial),
        });
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let prompt = self.prompt.take().expect("prompt is open");
                self.submit_prompt(prompt.kind, prompt.input.text.trim().to_string());
            }
            _ => {
                prompt.input.handle_key(key);
            }
        }
    }

    fn submit_prompt(&mut self, kind: PromptKind, text: String) {
        match kind {
            PromptKind::Open => {
                if text.is_empty() {
                    return;
                }
                let path = PathBuf::from(&text);
                match Document::open(path.clone()) {
                    Ok(doc) => {
                        self.tabs.push(Tab::new(doc));
                        self.active = self.tabs.len() - 1;
                        self.focus = Focus::Hex;
                        self.set_status(format!("Opened {}", path.display()));
                    }
                    Err(err) => {
                        self.set_status(format!("Failed to open {}: {err}", path.display()))
                    }
                }
            }
            PromptKind::Jump => {
                if text.is_empty() {
                    return;
                }
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                match Document::parse_offset(&text) {
                    Ok(off) => {
                        apply_offset_with_bounds_notice(&mut tab.doc, off);
                    }
                    Err(err) => tab.doc.status = err,
                }
            }
            PromptKind::Search => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if text.is_empty() {
                        if let Some(w) = tab.search.worker.take() {
                            w.cancel();
                        }
                        tab.search = SearchState::default();
                        tab.doc.status = "Search cleared".into();
                    } else {
                        tab.start_search(text);
                    }
                }
            }
            PromptKind::SaveAs => {
                if text.is_empty() {
                    return;
                }
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    match tab.doc.save_as(PathBuf::from(&text)) {
                        Ok(()) => tab.doc.status = format!("Saved as {text}"),
                        Err(err) => tab.doc.status = format!("Save failed: {err}"),
                    }
                }
            }
            PromptKind::AddField => {
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                let Some(sel) = tab.doc.selection else {
                    tab.doc.status = "Select bytes first, then press a".into();
                    return;
                };
                let n = tab.doc.fields.len();
                let name = if text.is_empty() {
                    format!("field_{}", n + 1)
                } else {
                    text
                };
                tab.doc.add_field(Field {
                    name: name.clone(),
                    start: sel.start(),
                    end: sel.end(),
                    // Only a border by default; rotate colors so adjacent fields differ.
                    border: Some(FieldColor::ALL[n % FieldColor::ALL.len()]),
                    ..Field::default()
                });
                tab.doc.status = format!(
                    "Added field '{name}' (0x{:X}..0x{:X}); press f to style it",
                    sel.start(),
                    sel.end()
                );
                self.field_cursor = tab.doc.selected_field;
                self.persist_overlay();
            }
            PromptKind::RenameField(i) => {
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                if text.is_empty() || i >= tab.doc.fields.len() {
                    return;
                }
                let mut field = tab.doc.fields[i].clone();
                field.name = text;
                tab.doc.update_field(i, field);
                self.field_cursor = tab.doc.selected_field;
                self.persist_overlay();
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        let pos = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.prompt.is_some() || self.show_help {
                    return false;
                }
                if self.fields_geo.area.contains(pos) {
                    self.click_field_list(pos);
                    return true;
                }
                let geo = self.hex_geo;
                if !geo.area.contains(pos) {
                    return false;
                }
                let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
                let Some(offset) = self.offset_at(pos, false) else {
                    return false;
                };
                let hit = geo.hit(pos.x - geo.area.x);
                self.focus = Focus::Hex;
                if let Some(tab) = self.tab_mut() {
                    tab.doc.commit_pending_edit();
                    if let Some((_, column)) = hit {
                        tab.column = column;
                    }
                    tab.doc.select_offset(offset, shift);
                }
                self.drag = Some(pos);
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.drag.is_some() => {
                self.drag = Some(pos);
                self.drag_to(pos);
                true
            }
            MouseEventKind::Up(MouseButton::Left) => self.drag.take().is_some(),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = mouse.kind == MouseEventKind::ScrollDown;
                if self.fields_geo.area.contains(pos) {
                    let n = self.tab().map_or(0, |t| t.doc.fields.len());
                    if n > 0 {
                        self.field_cursor = if down {
                            (self.field_cursor + 1).min(n - 1)
                        } else {
                            self.field_cursor.saturating_sub(1)
                        };
                    }
                    return true;
                }
                if let Some(tab) = self.tab_mut() {
                    let doc = &mut tab.doc;
                    doc.scroll = if down {
                        (doc.scroll + 3).min(doc.max_scroll())
                    } else {
                        doc.scroll.saturating_sub(3)
                    };
                }
                true
            }
            _ => false,
        }
    }

    /// Extends the drag selection to `pos`, scrolling when the pointer is at or beyond
    /// the top/bottom edge of the hex view.
    fn drag_to(&mut self, pos: Position) {
        let area = self.hex_geo.area;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let doc = &mut tab.doc;
            if pos.y <= area.y {
                let step = (area.y - pos.y.min(area.y)) as usize / 2 + 1;
                doc.scroll = doc.scroll.saturating_sub(step);
            } else if pos.y + 1 >= area.y + area.height {
                let step = (pos.y + 1 - (area.y + area.height)) as usize / 2 + 1;
                doc.scroll = (doc.scroll + step).min(doc.max_scroll());
            }
        }
        if let Some(offset) = self.offset_at(pos, true)
            && let Some(tab) = self.tabs.get_mut(self.active)
            && let Some(sel) = &mut tab.doc.selection
        {
            sel.cursor = offset;
        }
    }

    /// Byte offset under a screen position in the hex view. With `clamp`, positions
    /// outside the view snap to the nearest visible row/column.
    fn offset_at(&self, pos: Position, clamp: bool) -> Option<usize> {
        let geo = self.hex_geo;
        let tab = self.tab()?;
        let doc = &tab.doc;
        if doc.buffer.is_empty() || geo.area.height == 0 {
            return None;
        }
        let rel_x = pos.x.saturating_sub(geo.area.x);
        let col = if clamp {
            geo.hit_clamped(rel_x, tab.column).0
        } else {
            geo.hit(rel_x)?.0
        };
        let max_y = geo.area.y + geo.area.height - 1;
        let y = pos.y.clamp(geo.area.y, max_y);
        let row = doc.scroll + (y - geo.area.y) as usize;
        if row >= doc.row_count() {
            return clamp.then(|| doc.buffer.len() - 1);
        }
        let offset = match doc.display_row(row) {
            rex_core::DisplayRow::Bytes { offset } => offset + col,
            rex_core::DisplayRow::Repeated { start, .. } => start,
        };
        Some(offset.min(doc.buffer.len() - 1))
    }

    fn click_field_list(&mut self, pos: Position) {
        let geo = self.fields_geo;
        let index = geo.first_index + (pos.y - geo.area.y) as usize;
        let on_checkbox = pos.x < geo.area.x + 4;
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if index >= tab.doc.fields.len() {
            return;
        }
        self.field_cursor = index;
        self.focus = Focus::Fields;
        if on_checkbox {
            let f = &mut tab.doc.fields[index];
            f.visible = !f.visible;
            self.persist_overlay();
        } else {
            tab.select_field(index);
        }
    }

    fn handle_paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.input.insert_str(text);
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if !tab.doc.edit_mode {
            tab.doc.status = "Press i to enter edit mode before pasting".into();
            return;
        }
        match tab.column {
            Column::Hex => tab.doc.paste_hex_bytes(text),
            Column::Ascii => tab.doc.paste_ascii_bytes(text),
        }
    }

    fn copy(&mut self, format: CopyFormat, label: &str) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        match tab.doc.format_selected_bytes(format) {
            Some(text) => {
                let count = tab.doc.selected_bytes().len();
                tab.doc.status = format!("Copied {count} byte(s) as {label}");
                self.pending_clipboard = Some(text);
            }
            None => tab.doc.status = "No bytes selected to copy".into(),
        }
    }

    fn save(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.doc.commit_pending_edit();
            match tab.doc.save() {
                Ok(()) => tab.doc.status = format!("Saved {}", tab.doc.path.display()),
                Err(err) => tab.doc.status = format!("Save failed: {err}"),
            }
        }
    }

    fn change_bpr(&mut self, wider: bool) {
        const STEPS: [usize; 6] = [4, 8, 16, 24, 32, 48];
        let current = self.manual_bpr.unwrap_or(self.hex_geo.bpr.max(1));
        let next = if wider {
            STEPS
                .iter()
                .copied()
                .find(|&s| s > current)
                .unwrap_or(current)
        } else {
            STEPS
                .iter()
                .rev()
                .copied()
                .find(|&s| s < current)
                .unwrap_or(current)
        };
        self.manual_bpr = Some(next);
        self.set_status(format!("Bytes per row: {next} (0 = fit to terminal)"));
    }

    fn switch_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        if n > 1 {
            if let Some(t) = self.tab_mut() {
                t.doc.commit_pending_edit();
            }
            self.active = if forward {
                (self.active + 1) % n
            } else {
                self.active.checked_sub(1).unwrap_or(n - 1)
            };
            self.focus = Focus::Hex;
        }
    }

    fn close_tab(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if tab.doc.is_dirty() && !self.quit_armed {
            tab.doc.status = "Unsaved changes: press Ctrl+W again to close without saving".into();
            self.quit_armed = true;
            return;
        }
        self.quit_armed = false;
        self.tabs.remove(self.active);
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    fn request_quit(&mut self) {
        let dirty = self.tabs.iter().any(|t| t.doc.is_dirty());
        if dirty && !self.quit_armed {
            self.quit_armed = true;
            self.set_status("Unsaved changes: press q again to quit without saving (Ctrl+S saves)");
        } else {
            self.should_quit = true;
        }
    }
}

/// Writes text to the system clipboard through the terminal (OSC 52). Works over SSH
/// in terminals that support it (most modern ones, tmux with `set-clipboard on`).
fn copy_to_terminal_clipboard(text: &str) {
    use crossterm::clipboard::CopyToClipboard;
    let _ = execute!(io::stdout(), CopyToClipboard::to_clipboard_from(text));
}

/// True when the terminal advertises 24-bit color support.
fn detect_truecolor() -> bool {
    let colorterm = std::env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    colorterm.contains("truecolor")
        || colorterm.contains("24bit")
        || std::env::var_os("WT_SESSION").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(bytes: Vec<u8>) -> App {
        let mut app = App::new(LaunchOptions::default());
        let mut doc = Document::from_bytes("t.bin".into(), bytes);
        doc.select_offset(0, false);
        app.tabs.push(Tab::new(doc));
        app.prepare_frame(Rect::new(0, 0, 120, 30));
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(app: &mut App, s: &str) {
        for ch in s.chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn geometry_hit_tests_hex_and_ascii_columns() {
        let geo = HexGeometry {
            area: Rect::new(0, 0, 100, 10),
            off_w: 8,
            bpr: 16,
        };
        assert_eq!(geo.hit(10), Some((0, Column::Hex)));
        assert_eq!(geo.hit(13), Some((1, Column::Hex)));
        // byte 8 starts after the extra group gap
        assert_eq!(geo.hit(HexGeometry::hex_x(8, 8)), Some((8, Column::Hex)));
        let a0 = HexGeometry::ascii_x(8, 16, 0);
        assert_eq!(geo.hit(a0 + 3), Some((3, Column::Ascii)));
        assert_eq!(geo.hit(0), None);
        assert_eq!(geo.hit_clamped(0, Column::Hex), (0, Column::Hex));
        assert_eq!(geo.hit_clamped(99, Column::Hex), (15, Column::Hex));
    }

    #[test]
    fn edit_mode_overwrites_hex_and_ascii() {
        let mut app = app_with(vec![0; 32]);
        type_str(&mut app, "i");
        assert!(app.tab().unwrap().doc.edit_mode);
        type_str(&mut app, "ab");
        assert_eq!(app.tab().unwrap().doc.buffer.get(0), Some(0xAB));
        app.handle_key(key(KeyCode::Tab));
        type_str(&mut app, "Z");
        assert_eq!(app.tab().unwrap().doc.buffer.get(1), Some(b'Z'));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.tab().unwrap().doc.edit_mode);
        type_str(&mut app, "u");
        assert_eq!(app.tab().unwrap().doc.buffer.get(1), Some(0));
    }

    #[test]
    fn jump_prompt_moves_cursor() {
        let mut app = app_with(vec![0; 256]);
        type_str(&mut app, ":");
        assert!(app.prompt.is_some());
        type_str(&mut app, "0x40");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.prompt.is_none());
        assert_eq!(app.tab().unwrap().doc.selection.unwrap().cursor, 0x40);
    }

    #[test]
    fn fields_can_be_added_hidden_and_restyled() {
        let mut app = app_with(vec![0; 64]);
        type_str(&mut app, "v");
        for _ in 0..3 {
            app.handle_key(key(KeyCode::Right));
        }
        type_str(&mut app, "a");
        app.handle_key(key(KeyCode::Enter));
        let doc = &app.tab().unwrap().doc;
        assert_eq!(doc.fields.len(), 1);
        assert_eq!((doc.fields[0].start, doc.fields[0].end), (0, 3));
        assert!(doc.fields[0].border.is_some());
        assert!(doc.fields[0].text.is_none() && doc.fields[0].background.is_none());

        type_str(&mut app, "f ");
        assert!(!app.tab().unwrap().doc.fields[0].visible);
        type_str(&mut app, "t");
        assert_eq!(
            app.tab().unwrap().doc.fields[0].text,
            Some(FieldColor::Cyan)
        );
        type_str(&mut app, "B");
        let border = app.tab().unwrap().doc.fields[0].border;
        assert_ne!(border, Some(FieldColor::Cyan));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.focus, Focus::Hex);
    }

    #[test]
    fn copy_defaults_to_compact_hex() {
        let mut app = app_with(vec![0xA1, 0xB2, 0xC3, 0]);
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        type_str(&mut app, "y");
        assert_eq!(app.pending_clipboard.as_deref(), Some("A1B2C3"));
        type_str(&mut app, "Y");
        assert_eq!(app.pending_clipboard.as_deref(), Some("A1 B2 C3"));
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Screen position of byte `col` on hex-view row `row`.
    fn byte_at(app: &App, row: u16, col: usize) -> (u16, u16) {
        let geo = app.hex_geo;
        (
            geo.area.x + HexGeometry::hex_x(geo.off_w, col),
            geo.area.y + row,
        )
    }

    #[test]
    fn click_selects_and_drag_extends() {
        let mut app = app_with(vec![0; 1024]);
        let (x, y) = byte_at(&app, 2, 3);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
        assert_eq!(app.tab().unwrap().doc.selection.unwrap().cursor, 0x23);
        let (x2, y2) = byte_at(&app, 4, 1);
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), x2, y2));
        let sel = app.tab().unwrap().doc.selection.unwrap();
        assert_eq!((sel.anchor, sel.cursor), (0x23, 0x41));
        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), x2, y2));
        assert!(app.drag.is_none());

        // Clicking the ASCII column switches the active column.
        let geo = app.hex_geo;
        let ax = geo.area.x + HexGeometry::ascii_x(geo.off_w, geo.bpr, 5);
        app.handle_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            ax,
            geo.area.y,
        ));
        assert_eq!(app.tab().unwrap().column, Column::Ascii);
        assert_eq!(app.tab().unwrap().doc.selection.unwrap().cursor, 5);
    }

    #[test]
    fn holding_a_drag_past_the_bottom_edge_keeps_scrolling() {
        let mut app = app_with(vec![0; 16 * 500]);
        let (x, y) = byte_at(&app, 1, 0);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
        let geo = app.hex_geo;
        let below = geo.area.y + geo.area.height + 1;
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), x, below));
        let first = app.tab().unwrap().doc.scroll;
        assert!(first > 0);
        // No further mouse events: idle ticks continue the auto-scroll.
        for _ in 0..10 {
            assert!(app.tick());
        }
        let doc = &app.tab().unwrap().doc;
        assert!(doc.scroll > first + 5);
        let sel = doc.selection.unwrap();
        assert_eq!(sel.anchor, 0x10);
        assert!(sel.cursor / 16 >= doc.scroll);

        app.handle_event(mouse(MouseEventKind::Up(MouseButton::Left), x, below));
        assert!(!app.tick(), "releasing stops auto-scroll");

        // Wheel scrolling moves the view without moving the cursor.
        let state = |app: &App| {
            let doc = &app.tab().unwrap().doc;
            (doc.selection, doc.scroll)
        };
        let before = state(&app);
        app.handle_event(mouse(MouseEventKind::ScrollUp, x, y));
        let after = state(&app);
        assert_eq!(after.0, before.0);
        assert_eq!(after.1 + 3, before.1);
    }

    #[test]
    fn dragging_past_the_top_edge_scrolls_up() {
        let mut app = app_with(vec![0; 16 * 500]);
        app.tabs[0].doc.select_offset(16 * 300, false);
        app.prepare_frame(Rect::new(0, 0, 120, 30));
        let start_scroll = app.tab().unwrap().doc.scroll;
        let (x, y) = byte_at(&app, 5, 0);
        app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), x, y));
        app.handle_event(mouse(MouseEventKind::Drag(MouseButton::Left), x, 0));
        for _ in 0..10 {
            app.tick();
        }
        assert!(app.tab().unwrap().doc.scroll + 10 < start_scroll);
    }

    #[test]
    fn clicking_a_field_checkbox_toggles_visibility() {
        let mut app = app_with(vec![0; 64]);
        app.tabs[0].doc.add_field(Field {
            name: "magic".into(),
            end: 3,
            ..Field::default()
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(130, 30)).unwrap();
        app.prepare_frame(Rect::new(0, 0, 130, 30));
        terminal.draw(|f| view::render(f, &mut app)).unwrap();
        let list = app.fields_geo.area;
        assert!(list.width > 0, "field list is drawn");

        app.handle_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x + 1,
            list.y,
        ));
        assert!(!app.tab().unwrap().doc.fields[0].visible);
        app.handle_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x + 1,
            list.y,
        ));
        assert!(app.tab().unwrap().doc.fields[0].visible);

        // Clicking the name selects the field's bytes instead.
        app.handle_event(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x + 10,
            list.y,
        ));
        let sel = app.tab().unwrap().doc.selection.unwrap();
        assert_eq!((sel.start(), sel.end()), (0, 3));
        assert_eq!(app.focus, Focus::Fields);
    }

    #[test]
    fn color_cycling_passes_through_no_color() {
        let mut app = app_with(vec![0; 64]);
        app.tabs[0].doc.add_field(Field {
            name: "f".into(),
            end: 1,
            border: Some(FieldColor::ALL[FieldColor::ALL.len() - 1]),
            ..Field::default()
        });
        type_str(&mut app, "f");
        type_str(&mut app, "b");
        assert_eq!(
            app.tab().unwrap().doc.fields[0].border,
            None,
            "after the last color comes none"
        );
        type_str(&mut app, "b");
        assert_eq!(
            app.tab().unwrap().doc.fields[0].border,
            Some(FieldColor::ALL[0])
        );
        type_str(&mut app, "cC");
        assert_eq!(app.tab().unwrap().doc.fields[0].background, None);
    }

    #[test]
    fn paste_requires_edit_mode_and_respects_column() {
        let mut app = app_with(vec![0; 16]);
        app.handle_event(Event::Paste("DE AD".into()));
        assert_eq!(app.tab().unwrap().doc.buffer.get(0), Some(0));
        type_str(&mut app, "i");
        app.handle_event(Event::Paste("DE AD".into()));
        assert_eq!(app.tab().unwrap().doc.buffer.get(0), Some(0xDE));
        assert_eq!(app.tab().unwrap().doc.buffer.get(1), Some(0xAD));
    }

    #[test]
    fn modifier_chords_are_not_typed_into_the_file() {
        let mut app = app_with(vec![0; 16]);
        type_str(&mut app, "i");
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert_eq!(app.tab().unwrap().doc.buffer.get(0), Some(0));
        assert!(!app.tab().unwrap().doc.is_dirty());
    }

    #[test]
    fn search_prompt_finds_and_steps_through_matches() {
        let mut app = app_with(b"..AB..AB..AB....".to_vec());
        type_str(&mut app, "/");
        type_str(&mut app, "text:AB");
        app.handle_key(key(KeyCode::Enter));
        for _ in 0..200 {
            app.tabs[0].poll_search();
            if !app.tabs[0].search.running {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let tab = app.tab().unwrap();
        assert_eq!(tab.search.matches.len(), 3);
        assert_eq!(tab.doc.selection.unwrap().start(), 2);
        type_str(&mut app, "n");
        assert_eq!(app.tab().unwrap().doc.selection.unwrap().start(), 6);
        type_str(&mut app, "N");
        type_str(&mut app, "N");
        assert_eq!(
            app.tab().unwrap().doc.selection.unwrap().start(),
            10,
            "wraps backwards"
        );
    }

    #[test]
    fn line_input_edits_like_a_shell_prompt() {
        let mut input = LineInput::with_text("0x1234".into());
        input.handle_key(key(KeyCode::Backspace));
        assert_eq!(input.text, "0x123");
        input.handle_key(key(KeyCode::Home));
        input.handle_key(key(KeyCode::Delete));
        input.handle_key(key(KeyCode::Right));
        input.handle_key(key(KeyCode::Char('Z')));
        assert_eq!(input.text, "xZ123");
        input.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(input.text, "123");
        input.insert_str("é\n");
        assert_eq!(input.text, "é123", "control characters are dropped");
    }

    #[test]
    fn quitting_with_unsaved_changes_needs_confirmation() {
        let mut app = app_with(vec![0; 8]);
        type_str(&mut app, "i1");
        app.handle_key(key(KeyCode::Esc));
        type_str(&mut app, "q");
        assert!(!app.should_quit);
        type_str(&mut app, "q");
        assert!(app.should_quit);
    }
}
