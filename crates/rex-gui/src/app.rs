use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use egui::{Color32, Context, Key, RichText};
use rex_core::{
    ByteColorMode, CopyFormat, Document, EditKind, SearchMatch, Selection,
    entropy::{self, EntropyMessage, EntropyReport, EntropyWorker},
    plugins::{self, Plugin, PluginWorker},
    python::{self, PythonDocument, PythonSession},
    search::{self, SearchMessage, SearchWorker},
};

use crate::{
    hex_viewport::{EditColumn, render_hex_viewport},
    sidebar::{
        FieldEditorState, InspectorUiState, render_entropy_panel, render_fields_panel,
        render_inspector_panel,
    },
    theme::{configure_workstation_style, hex_byte_str},
};

#[derive(Default)]
pub struct SearchPanelState {
    pub open: bool,
    pub request_focus: bool,
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub current_match: usize,
    pub running: bool,
    pub scanned: usize,
    pub total: usize,
    pub error: Option<String>,
    worker: Option<SearchWorker>,
}

#[derive(Default)]
pub struct EntropyPanelState {
    pub open: bool,
    pub report: Option<EntropyReport>,
    pub running: bool,
    pub scanned: usize,
    pub total: usize,
    worker: Option<EntropyWorker>,
}

pub struct PythonPanelState {
    pub open: bool,
    pub input: String,
    pub repl_lines: Vec<String>,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub history_draft: String,
    pub output_lines: Vec<String>,
    pub pending: usize,
    pub session: Option<PythonSession>,
    pub starting_rx: Option<Receiver<Result<PythonSession, String>>>,
    pub queued_commands: Vec<String>,
}

impl Default for PythonPanelState {
    fn default() -> Self {
        Self {
            open: false,
            input: String::new(),
            repl_lines: Vec::new(),
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
            output_lines: vec![
                "Python 3 Binary Analysis Console".into(),
                "Variables: buffer, selected, selection_start, selection_end, buffers".into(),
                "Type Python expressions or statements (multi-line blocks ending in ':' are supported). Click 'Apply Buffers' (or type :apply) to write changes back.".into(),
            ],
            pending: 0,
            session: None,
            starting_rx: None,
            queued_commands: Vec::new(),
        }
    }
}

fn python_continuation_indentation(line: &str) -> String {
    let leading_spaces = line.chars().take_while(|&c| c == ' ').count();
    if line.trim_end().ends_with(':') {
        " ".repeat(leading_spaces + 4)
    } else {
        " ".repeat(leading_spaces)
    }
}

pub struct DocumentTab {
    pub doc: Document,
    pub active_column: EditColumn,
    pub inspector_state: InspectorUiState,
    pub field_editor: FieldEditorState,
    pub search: SearchPanelState,
    pub entropy: EntropyPanelState,
    pub plugin_results: HashMap<String, String>,
    pub plugin_worker: Option<PluginWorker>,
}

impl DocumentTab {
    pub fn new(doc: Document) -> Self {
        Self {
            doc,
            active_column: EditColumn::Hex,
            inspector_state: InspectorUiState::default(),
            field_editor: FieldEditorState::default(),
            search: SearchPanelState::default(),
            entropy: EntropyPanelState::default(),
            plugin_results: HashMap::new(),
            plugin_worker: None,
        }
    }

    pub fn poll_workers(&mut self) -> bool {
        let mut active = false;

        let search_msgs: Vec<SearchMessage> = if let Some(worker) = &self.search.worker {
            let mut msgs = Vec::new();
            while let Ok(msg) = worker.receiver.try_recv() {
                msgs.push(msg);
            }
            msgs
        } else {
            Vec::new()
        };

        if !search_msgs.is_empty() || self.search.worker.is_some() {
            let mut jump_match = false;
            let mut done = false;
            for msg in search_msgs {
                match msg {
                    SearchMessage::Batch(batch) => {
                        let was_empty = self.search.matches.is_empty();
                        self.search.matches.extend(batch);
                        if was_empty && !self.search.matches.is_empty() {
                            jump_match = true;
                        }
                    }
                    SearchMessage::Progress(scanned) => {
                        self.search.scanned = scanned;
                    }
                    SearchMessage::Done => {
                        self.search.running = false;
                        self.doc.status = format!(
                            "Search finished: {} match(es) for '{}'",
                            self.search.matches.len(),
                            self.search.query
                        );
                        done = true;
                    }
                    SearchMessage::Error(err) => {
                        self.search.error = Some(err.clone());
                        self.search.running = false;
                        self.doc.status = format!("Search error: {err}");
                        done = true;
                    }
                }
            }
            if jump_match {
                self.jump_to_match(0);
            }
            if self.search.worker.is_some() {
                if done {
                    self.search.worker = None;
                } else {
                    active = true;
                }
            }
        }

        if let Some(worker) = &self.entropy.worker {
            let mut done = false;
            while let Ok(msg) = worker.receiver.try_recv() {
                match msg {
                    EntropyMessage::Progress(scanned) => {
                        self.entropy.scanned = scanned;
                    }
                    EntropyMessage::Done(report) => {
                        self.entropy.report = Some(report);
                        self.entropy.running = false;
                        self.doc.status = "Entropy analysis complete".into();
                        done = true;
                    }
                }
            }
            if done {
                self.entropy.worker = None;
            } else {
                active = true;
            }
        }

        if let Some(worker) = &self.plugin_worker {
            loop {
                match worker.receiver.try_recv() {
                    Ok(outcome) => {
                        self.plugin_results.insert(outcome.name, outcome.value);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        active = true;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.plugin_worker = None;
                        self.doc.status = "Inspector plugins complete".into();
                        break;
                    }
                }
            }
        }

        active
    }

    pub fn start_search(&mut self) {
        if let Some(worker) = self.search.worker.take() {
            worker.cancel();
        }
        self.search.matches.clear();
        self.search.current_match = 0;
        self.search.error = None;
        self.search.scanned = 0;
        self.search.total = self.doc.buffer.len();

        match search::spawn(self.doc.buffer.clone(), self.search.query.clone()) {
            Ok(worker) => {
                self.search.running = true;
                self.search.worker = Some(worker);
                self.doc.status = format!("Searching for '{}'...", self.search.query);
            }
            Err(err) => {
                self.search.error = Some(err.clone());
                self.search.running = false;
                self.doc.status = err;
            }
        }
    }

    pub fn jump_to_match(&mut self, index: usize) {
        if let Some(m) = self.search.matches.get(index).copied() {
            self.search.current_match = index;
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

    pub fn start_entropy(&mut self) {
        if self.entropy.running {
            return;
        }
        if let Some(worker) = self.entropy.worker.take() {
            worker.cancel();
        }
        self.entropy.report = None;
        self.entropy.scanned = 0;
        self.entropy.total = self.doc.buffer.len();
        self.entropy.running = true;
        self.entropy.worker = Some(entropy::spawn(self.doc.buffer.clone()));
    }

    pub fn run_plugins(&mut self, plugins: &[Plugin]) {
        if plugins.is_empty() {
            self.doc.status = "No inspector plugins installed".into();
            return;
        }
        if let Some(w) = self.plugin_worker.take() {
            w.cancel();
        }
        self.plugin_results.clear();
        let bytes = self.doc.current_bytes(plugins::MAX_INPUT_BYTES);
        self.plugin_worker = Some(plugins::spawn(plugins.to_vec(), bytes));
        self.doc.status = format!("Running {} inspector plugin(s)...", plugins.len());
    }
}

#[derive(Default, Debug, Clone)]
pub struct LaunchOptions {
    pub paths: Vec<PathBuf>,
    pub initial_offset: Option<usize>,
    pub cli_error: Option<String>,
}

pub fn apply_offset_with_bounds_notice(doc: &mut Document, offset: usize) -> Option<String> {
    let len = doc.buffer.len();
    if len == 0 {
        let msg = format!(
            "Offset 0x{offset:X} ({offset}) does not exist because '{}' is empty (0 bytes).",
            doc.file_name()
        );
        doc.status = msg.clone();
        return Some(msg);
    }
    if offset >= len {
        let last = len - 1;
        doc.select_offset(last, false);
        let msg = format!(
            "Offset 0x{offset:X} ({offset}) does not exist in '{}' (file size is {len} bytes / 0x{len:X}); jumped to end of file (0x{last:X}).",
            doc.file_name()
        );
        doc.status = msg.clone();
        Some(msg)
    } else {
        doc.select_offset(offset, false);
        doc.status = format!("Jumped to offset 0x{offset:X} ({offset})");
        None
    }
}

pub struct RexEditorApp {
    pub tabs: Vec<DocumentTab>,
    pub active_tab: usize,
    pub compare_tab: usize,
    pub side_by_side: bool,
    pub diff_mode: bool,
    pub sync_comparison: bool,
    pub show_inspector: bool,
    pub show_fields: bool,
    pub jump_modal_open: bool,
    pub jump_input: String,
    pub help_modal_open: bool,
    pub notice_modal: Option<String>,
    pub python: PythonPanelState,
    pub discovered_plugins: Vec<Plugin>,
    pub status_banner: String,
    style_initialized: bool,
}

impl RexEditorApp {
    #[allow(dead_code)]
    pub fn new(initial_paths: Vec<PathBuf>) -> Self {
        Self::new_with_options(LaunchOptions {
            paths: initial_paths,
            initial_offset: None,
            cli_error: None,
        })
    }

    pub fn new_with_options(options: LaunchOptions) -> Self {
        python::warm_up();
        let mut tabs = Vec::new();
        let mut status_banner = "Ready — Open a binary via File > Open (Ctrl+O) or drag and drop a file into the window.".to_string();
        let mut notice_modal = options.cli_error.clone();
        if let Some(err) = &options.cli_error {
            status_banner = err.clone();
        }

        for path in options.paths {
            match Document::open(path.clone()) {
                Ok(mut doc) => {
                    status_banner = format!("Opened {}", path.display());
                    if let Some(offset) = options.initial_offset {
                        if let Some(warning) = apply_offset_with_bounds_notice(&mut doc, offset) {
                            status_banner = warning.clone();
                            notice_modal = Some(warning);
                        } else {
                            status_banner = format!(
                                "Opened {} at offset 0x{offset:X} ({offset})",
                                path.display()
                            );
                        }
                    }
                    tabs.push(DocumentTab::new(doc));
                }
                Err(err) => {
                    let msg = format!("Failed to open {}: {err}", path.display());
                    status_banner = msg.clone();
                    notice_modal = Some(msg);
                }
            }
        }

        Self {
            tabs,
            active_tab: 0,
            compare_tab: 1,
            side_by_side: false,
            diff_mode: false,
            sync_comparison: true,
            show_inspector: true,
            show_fields: true,
            jump_modal_open: false,
            jump_input: String::new(),
            help_modal_open: false,
            notice_modal,
            python: PythonPanelState::default(),
            discovered_plugins: plugins::discover(&plugins::default_directory()),
            status_banner,
            style_initialized: false,
        }
    }

    fn effective_compare_tab(&self) -> usize {
        if self.tabs.len() < 2 {
            return 0;
        }
        let candidate = self.compare_tab.min(self.tabs.len() - 1);
        if candidate == self.active_tab {
            (self.active_tab + 1) % self.tabs.len()
        } else {
            candidate
        }
    }

    fn jump_to_diff(&mut self, next: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        let active_idx = self.active_tab;
        let other_idx = self.effective_compare_tab();
        let cur = self.tabs[active_idx]
            .doc
            .selection
            .unwrap_or_else(|| Selection::new(0));
        let found = if next {
            let from = cur.end().saturating_add(1);
            self.tabs[active_idx]
                .doc
                .buffer
                .find_next_diff(&self.tabs[other_idx].doc.buffer, from)
        } else {
            let before = cur.start();
            self.tabs[active_idx]
                .doc
                .buffer
                .find_prev_diff(&self.tabs[other_idx].doc.buffer, before)
        };
        match found {
            Some((start, end)) => {
                let len = end - start + 1;
                {
                    let doc = &mut self.tabs[active_idx].doc;
                    let max_off = doc.buffer.len().saturating_sub(1);
                    let s = start.min(max_off);
                    let e = end.min(max_off);
                    doc.selection = Some(Selection::from_range(s, e));
                    doc.additional_selections.clear();
                    doc.ensure_visible(s);
                    doc.status = format!("Diff at 0x{start:X}..0x{end:X} ({len} byte(s))");
                }
                if self.sync_comparison && !self.tabs[other_idx].doc.buffer.is_empty() {
                    let doc2 = &mut self.tabs[other_idx].doc;
                    let max_off = doc2.buffer.len().saturating_sub(1);
                    let s = start.min(max_off);
                    let e = end.min(max_off);
                    doc2.selection = Some(Selection::from_range(s, e));
                    doc2.additional_selections.clear();
                    doc2.ensure_visible(s);
                }
            }
            None => {
                self.tabs[active_idx].doc.status = "Buffers are identical (no byte differences found)".into();
            }
        }
    }

    fn sync_python_selections(&self) {
        let Some(session) = &self.python.session else {
            return;
        };
        let active_tab = self.active_tab;
        for (idx, tab) in self.tabs.iter().enumerate() {
            let snap_len = tab.doc.buffer.len().min(32 * 1024 * 1024);
            let max_idx = snap_len.saturating_sub(1);
            let sel = tab.doc.selection.unwrap_or_else(|| Selection::new(0));
            let selections = tab
                .doc
                .selected_ranges()
                .into_iter()
                .map(|r| (r.start().min(max_idx), r.end().min(max_idx)))
                .collect();
            let _ = session.update_selection(
                active_tab,
                idx,
                sel.start().min(max_idx),
                sel.end().min(max_idx),
                selections,
            );
        }
    }

    pub fn open_file_dialog(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .set_title("Open Binary File")
            .pick_files()
        {
            for path in paths {
                self.open_path(path);
            }
        }
    }

    pub fn open_path(&mut self, path: PathBuf) {
        match Document::open(path.clone()) {
            Ok(doc) => {
                self.tabs.push(DocumentTab::new(doc));
                self.active_tab = self.tabs.len() - 1;
                self.status_banner = format!("Opened {}", path.display());
                if self.python.open {
                    self.restart_python_session();
                }
            }
            Err(err) => {
                self.status_banner = format!("Could not open {}: {err}", path.display());
            }
        }
    }

    fn save_active(&mut self, save_as: bool) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if save_as || !tab.doc.path.is_file() {
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Save Binary As")
                .set_file_name(tab.doc.file_name())
                .save_file()
            {
                if let Err(err) = tab.doc.save_as(path) {
                    self.status_banner = err;
                }
            }
        } else if let Err(err) = tab.doc.save() {
            self.status_banner = err;
        }
    }

    fn load_overlay_dialog(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Load Overlay JSON")
            .add_filter("JSON Overlay", &["json"])
            .pick_file()
            && let Some(tab) = self.tabs.get_mut(self.active_tab)
        {
            match tab.doc.load_overlay_from(&path) {
                Ok(msg) => tab.doc.status = msg,
                Err(err) => tab.doc.status = err,
            }
        }
    }

    fn save_overlay_dialog(&mut self) {
        let Some(suggested) = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.doc.suggested_overlay_path())
        else {
            return;
        };
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save Overlay JSON As")
            .add_filter("JSON Overlay", &["json"]);
        if let Some(dir) = suggested.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = suggested.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        if let Some(path) = dialog.save_file()
            && let Some(tab) = self.tabs.get_mut(self.active_tab)
        {
            match tab.doc.save_overlay_to(&path) {
                Ok(msg) => tab.doc.status = msg,
                Err(err) => tab.doc.status = err,
            }
        }
    }

    fn toggle_python_console(&mut self) {
        self.python.open = !self.python.open;
        if self.python.open {
            self.ensure_python_session();
        }
    }

    fn toggle_split_view(&mut self) {
        if self.tabs.len() >= 2 {
            self.side_by_side = !self.side_by_side;
            if !self.side_by_side {
                self.diff_mode = false;
            }
        } else if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.doc.status = "Open at least 2 binary files to use Split View (Ctrl+B)".into();
        } else {
            self.status_banner = "Open at least 2 binary files to use Split View (Ctrl+B)".into();
        }
    }

    fn toggle_diff_view(&mut self) {
        if self.tabs.len() >= 2 {
            self.diff_mode = !self.diff_mode;
            self.side_by_side |= self.diff_mode;
        } else if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.doc.status = "Open at least 2 binary files to use Diff View (Ctrl+D)".into();
        } else {
            self.status_banner = "Open at least 2 binary files to use Diff View (Ctrl+D)".into();
        }
    }

    fn copy_selection(&mut self, ctx: &Context, format: CopyFormat, label: &str) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        let Some(out) = tab.doc.format_selected_bytes(format) else {
            tab.doc.status = "No bytes selected to copy".into();
            return;
        };
        let count = tab.doc.selected_bytes().len();
        ctx.copy_text(out.clone());
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(out);
        }
        tab.doc.status = format!("Copied {count} byte(s) as {label}");
    }

    fn copy_selection_hex(&mut self, ctx: &Context) {
        self.copy_selection(ctx, CopyFormat::HexSpaced, "spaced hex");
    }

    fn paste_clipboard_hex(&mut self, pasted_event: Option<String>) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if !tab.doc.edit_mode {
            tab.doc.edit_mode = true;
        }
        let text = pasted_event.or_else(|| {
            arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok())
        });
        if let Some(text) = text {
            tab.doc.paste_hex_bytes(&text);
        }
    }

    fn paste_clipboard_ascii(&mut self, pasted_event: Option<String>) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if !tab.doc.edit_mode {
            tab.doc.edit_mode = true;
        }
        let text = pasted_event.or_else(|| {
            arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok())
        });
        if let Some(text) = text {
            tab.doc.paste_ascii_bytes(&text);
        }
    }

    fn poll_python(&mut self) -> bool {
        if let Some(rx) = &self.python.starting_rx {
            match rx.try_recv() {
                Ok(Ok(session)) => {
                    self.python.starting_rx = None;
                    let queued = std::mem::take(&mut self.python.queued_commands);
                    for cmd in queued {
                        if cmd.trim() == ":apply" {
                            let _ = session.apply();
                        } else {
                            let _ = session.execute(cmd);
                        }
                        self.python.pending += 1;
                    }
                    self.python.session = Some(session);
                }
                Ok(Err(err)) => {
                    self.python.starting_rx = None;
                    self.python.queued_commands.clear();
                    self.python.output_lines.push(err);
                }
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.python.starting_rx = None;
                    self.python.queued_commands.clear();
                }
            }
        }

        let Some(session) = &self.python.session else {
            return false;
        };
        let mut applied_snapshots = Vec::new();
        while let Ok(resp) = session.responses.try_recv() {
            self.python.pending = self.python.pending.saturating_sub(1);
            if !resp.output.is_empty() {
                for line in resp.output.lines() {
                    self.python.output_lines.push(line.to_string());
                }
            }
            if let Some(err) = resp.error {
                self.python.output_lines.push(format!("Error: {err}"));
            }
            if resp.applied {
                applied_snapshots = session.snapshots.clone();
            }
        }
        for snap in applied_snapshots {
            if let Some(tab) = self.tabs.get_mut(snap.index)
                && let Ok(updated) = fs::read(&snap.path)
                && updated.len() == tab.doc.buffer.len()
            {
                tab.doc.overwrite_bytes(0, &updated);
                tab.doc.status = "Applied Python buffer changes".into();
            }
        }
        self.python.pending > 0
    }

    fn restart_python_session(&mut self) {
        self.python.session = None;
        self.python.starting_rx = None;
        self.python.pending = 0;
        self.ensure_python_session();
    }

    fn ensure_python_session(&mut self) {
        if self.python.session.is_some()
            || self.python.starting_rx.is_some()
            || self.tabs.is_empty()
        {
            return;
        }
        let docs: Vec<PythonDocument> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(idx, tab)| {
                let sel = tab.doc.selection.unwrap_or_else(|| Selection::new(0));
                let selections = tab
                    .doc
                    .selected_ranges()
                    .into_iter()
                    .map(|r| (r.start(), r.end()))
                    .collect();
                // Cap Python snapshot size to 32 MiB to keep startup fast
                let snap_len = tab.doc.buffer.len().min(32 * 1024 * 1024);
                PythonDocument {
                    index: idx,
                    bytes: tab.doc.buffer.read_range(0, snap_len),
                    selection_start: sel.start().min(snap_len.saturating_sub(1)),
                    selection_end: sel.end().min(snap_len.saturating_sub(1)),
                    selections,
                }
            })
            .collect();
        let active_tab = self.active_tab;
        let (tx, rx) = mpsc::channel();
        self.python.starting_rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(PythonSession::start(docs, active_tab));
        });
    }

    fn handle_keyboard_shortcuts(&mut self, ctx: &Context) {
        // Handle dropped files
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                self.open_path(path);
            }
        }

        if !self.jump_modal_open {
            let mut open_dialog = false;
            let mut load_overlay = false;
            let mut save_file = false;
            let mut save_as = false;
            let mut save_overlay = false;
            let mut toggle_python = false;
            let mut toggle_split = false;
            let mut toggle_diff = false;
            let mut toggle_search = false;
            let mut toggle_jump = false;
            let mut toggle_help = false;

            ctx.input(|i| {
                let ctrl = i.modifiers.ctrl || i.modifiers.command;
                let shift = i.modifiers.shift;
                let alt = i.modifiers.alt;

                if ctrl && !shift && !alt && i.key_pressed(Key::O) {
                    open_dialog = true;
                }
                if ctrl && shift && !alt && i.key_pressed(Key::O) {
                    load_overlay = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::S) {
                    save_file = true;
                }
                if ctrl && shift && !alt && i.key_pressed(Key::S) {
                    save_as = true;
                }
                if ctrl && !shift && alt && i.key_pressed(Key::S) {
                    save_overlay = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::P) {
                    toggle_python = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::B) {
                    toggle_split = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::D) {
                    toggle_diff = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::F) {
                    toggle_search = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::G) {
                    toggle_jump = true;
                }
                if ctrl && !shift && !alt && i.key_pressed(Key::H) {
                    toggle_help = true;
                }
            });

            if open_dialog {
                self.open_file_dialog();
                return;
            }
            if load_overlay {
                self.load_overlay_dialog();
                return;
            }
            if save_file {
                self.save_active(false);
                return;
            }
            if save_as {
                self.save_active(true);
                return;
            }
            if save_overlay {
                self.save_overlay_dialog();
                return;
            }
            if toggle_python {
                self.toggle_python_console();
                return;
            }
            if toggle_split {
                self.toggle_split_view();
                return;
            }
            if toggle_diff {
                self.toggle_diff_view();
                return;
            }
            if toggle_search {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.search.open = !tab.search.open;
                    if tab.search.open {
                        tab.search.request_focus = true;
                    }
                }
                return;
            }
            if toggle_jump {
                self.jump_modal_open = !self.jump_modal_open;
                return;
            }
            if toggle_help {
                self.help_modal_open = !self.help_modal_open;
                return;
            }
        }

        if ctx.wants_keyboard_input() || self.jump_modal_open || self.help_modal_open {
            return;
        }

        let mut copy_hex = false;
        let mut copy_ascii = false;
        let mut paste_hex: Option<Option<String>> = None;
        let mut paste_ascii: Option<Option<String>> = None;

        ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let shift = i.modifiers.shift;

            let copy_event = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            if (ctrl && !shift && i.key_pressed(Key::C)) || (!shift && copy_event) {
                copy_hex = true;
            }
            if (ctrl && shift && i.key_pressed(Key::C)) || (shift && copy_event) {
                copy_ascii = true;
            }
            let pasted_text = i.events.iter().find_map(|e| match e {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            });
            if let Some(text) = pasted_text {
                if shift {
                    paste_ascii = Some(Some(text));
                } else {
                    paste_hex = Some(Some(text));
                }
            } else {
                if ctrl && !shift && i.key_pressed(Key::V) {
                    paste_hex = Some(None);
                }
                if ctrl && shift && i.key_pressed(Key::V) {
                    paste_ascii = Some(None);
                }
            }
        });

        if copy_hex {
            self.copy_selection_hex(ctx);
            return;
        }
        if copy_ascii {
            self.copy_selection(ctx, CopyFormat::Ascii, "ASCII");
            return;
        }
        if let Some(pasted) = paste_hex {
            self.paste_clipboard_hex(pasted);
            return;
        }
        if let Some(pasted) = paste_ascii {
            self.paste_clipboard_ascii(pasted);
            return;
        }

        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };

        let mut run_plugins = false;
        let mut toggle_entropy = false;
        let mut add_field_for_selection = false;

        ctx.input(|i| {
            let ctrl = i.modifiers.ctrl || i.modifiers.command;
            let shift = i.modifiers.shift;

            if i.key_pressed(Key::F3) && !tab.search.matches.is_empty() {
                if shift {
                    let prev = tab
                        .search
                        .current_match
                        .checked_sub(1)
                        .unwrap_or(tab.search.matches.len() - 1);
                    tab.jump_to_match(prev);
                } else {
                    let next = (tab.search.current_match + 1) % tab.search.matches.len();
                    tab.jump_to_match(next);
                }
            }
            if (ctrl && i.key_pressed(Key::Z)) || (ctrl && i.key_pressed(Key::U)) {
                tab.doc.undo();
            }
            if (ctrl && i.key_pressed(Key::Y)) || (ctrl && i.key_pressed(Key::R)) {
                tab.doc.redo();
            }

            let bpr = tab.doc.settings.bytes_per_row as isize;
            let page = (tab.doc.visible_rows.max(1) as isize) * bpr;

            if i.key_pressed(Key::ArrowLeft) {
                tab.doc.commit_pending_edit();
                tab.doc.move_cursor(-1, shift);
            }
            if i.key_pressed(Key::ArrowRight) {
                tab.doc.commit_pending_edit();
                tab.doc.move_cursor(1, shift);
            }
            if i.key_pressed(Key::ArrowUp) {
                tab.doc.commit_pending_edit();
                tab.doc.move_cursor(-bpr, shift);
            }
            if i.key_pressed(Key::ArrowDown) {
                tab.doc.commit_pending_edit();
                tab.doc.move_cursor(bpr, shift);
            }
            if i.key_pressed(Key::PageUp) {
                tab.doc.commit_pending_edit();
                tab.doc.move_cursor(-page, shift);
            }
            if i.key_pressed(Key::PageDown) {
                tab.doc.commit_pending_edit();
                tab.doc.move_cursor(page, shift);
            }
            if i.key_pressed(Key::Home) {
                tab.doc.commit_pending_edit();
                tab.doc.select_offset(0, shift);
            }
            if i.key_pressed(Key::End) && !tab.doc.buffer.is_empty() {
                tab.doc.commit_pending_edit();
                tab.doc.select_offset(tab.doc.buffer.len() - 1, shift);
            }
            if i.key_pressed(Key::Escape) && tab.doc.edit_mode {
                tab.doc.commit_pending_edit();
                tab.doc.edit_mode = false;
                tab.doc.status = "View Mode".into();
            }
            if i.key_pressed(Key::Insert) {
                tab.doc.commit_pending_edit();
                if !tab.doc.edit_mode {
                    tab.doc.edit_mode = true;
                    tab.doc.edit_kind = EditKind::Insert;
                } else {
                    tab.doc.edit_kind = tab.doc.edit_kind.toggle();
                }
            }
            if i.key_pressed(Key::Tab) {
                tab.doc.commit_pending_edit();
                tab.active_column = match tab.active_column {
                    EditColumn::Hex => EditColumn::Ascii,
                    EditColumn::Ascii => EditColumn::Hex,
                };
            }
            if tab.doc.edit_mode && (i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete)) {
                tab.doc.delete_selected_bytes();
            }

            for event in &i.events {
                if let egui::Event::Text(text) = event {
                    for ch in text.chars() {
                        if !tab.doc.edit_mode {
                            match ch {
                                'i' => tab.doc.cycle_edit_mode(),
                                'e' => toggle_entropy = true,
                                'r' => run_plugins = true,
                                'a' => add_field_for_selection = true,
                                '[' => {
                                    if !tab.doc.fields.is_empty() {
                                        let prev = tab
                                            .doc
                                            .selected_field
                                            .checked_sub(1)
                                            .unwrap_or(tab.doc.fields.len() - 1);
                                        tab.doc.selected_field = prev;
                                        let (s, e) =
                                            (tab.doc.fields[prev].start, tab.doc.fields[prev].end);
                                        tab.doc.select_offset(s, false);
                                        if let Some(sel) = &mut tab.doc.selection {
                                            sel.cursor = e;
                                        }
                                    }
                                }
                                ']' => {
                                    if !tab.doc.fields.is_empty() {
                                        let next =
                                            (tab.doc.selected_field + 1) % tab.doc.fields.len();
                                        tab.doc.selected_field = next;
                                        let (s, e) =
                                            (tab.doc.fields[next].start, tab.doc.fields[next].end);
                                        tab.doc.select_offset(s, false);
                                        if let Some(sel) = &mut tab.doc.selection {
                                            sel.cursor = e;
                                        }
                                    }
                                }
                                'n' => {
                                    if !tab.search.matches.is_empty() {
                                        let next = (tab.search.current_match + 1) % tab.search.matches.len();
                                        tab.jump_to_match(next);
                                    }
                                }
                                'N' => {
                                    if !tab.search.matches.is_empty() {
                                        let prev = tab
                                            .search
                                            .current_match
                                            .checked_sub(1)
                                            .unwrap_or(tab.search.matches.len() - 1);
                                        tab.jump_to_match(prev);
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            match tab.active_column {
                                EditColumn::Hex => {
                                    if ch == 'i' || ch == 'I' {
                                        tab.doc.cycle_edit_mode();
                                    } else if let Some(nibble) = ch.to_digit(16) {
                                        tab.doc.edit_nibble(nibble as u8);
                                    }
                                }
                                EditColumn::Ascii => {
                                    if ch.is_ascii() && !ch.is_ascii_control() {
                                        tab.doc.edit_ascii_char(ch);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        if toggle_entropy {
            tab.entropy.open = !tab.entropy.open;
            if tab.entropy.open && tab.entropy.report.is_none() {
                tab.start_entropy();
            }
        }
        if add_field_for_selection {
            self.show_fields = true;
            tab.field_editor.begin_add_for_selection(&tab.doc);
        }
        if run_plugins {
            let plugins = self.discovered_plugins.clone();
            tab.run_plugins(&plugins);
        }
    }
}

impl eframe::App for RexEditorApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if !self.style_initialized {
            configure_workstation_style(ctx);
            self.style_initialized = true;
        }

        // Disable egui's exponential scroll smoothing decay so scrolling stops immediately
        // when the user stops moving the mouse wheel or trackpad.
        ctx.input_mut(|i| {
            i.smooth_scroll_delta = i.raw_scroll_delta;
        });

        // Poll background workers and only request periodic repaint when a worker is running
        let mut any_worker_active = self.poll_python();
        for tab in &mut self.tabs {
            any_worker_active |= tab.poll_workers();
        }
        if any_worker_active {
            ctx.request_repaint_after(Duration::from_millis(32));
        }

        self.handle_keyboard_shortcuts(ctx);

        // ---- Top Menu & Workstation Toolbar ----
        egui::TopBottomPanel::top("top_menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Binary... (Ctrl+O)").clicked() {
                        ui.close_menu();
                        self.open_file_dialog();
                    }
                    let has_doc = !self.tabs.is_empty();
                    if ui
                        .add_enabled(has_doc, egui::Button::new("Save (Ctrl+S)"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.save_active(false);
                    }
                    if ui
                        .add_enabled(has_doc, egui::Button::new("Save As... (Ctrl+Shift+S)"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.save_active(true);
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            has_doc,
                            egui::Button::new("Load Overlay JSON... (Ctrl+Shift+O)"),
                        )
                        .clicked()
                    {
                        ui.close_menu();
                        self.load_overlay_dialog();
                    }
                    if ui
                        .add_enabled(
                            has_doc,
                            egui::Button::new("Save Overlay JSON As... (Ctrl+Alt+S)"),
                        )
                        .clicked()
                    {
                        ui.close_menu();
                        self.save_overlay_dialog();
                    }
                });

                ui.menu_button("Edit", |ui| {
                    let has_doc = !self.tabs.is_empty();
                    let can_undo = self
                        .tabs
                        .get(self.active_tab)
                        .is_some_and(|t| t.doc.can_undo());
                    let can_redo = self
                        .tabs
                        .get(self.active_tab)
                        .is_some_and(|t| t.doc.can_redo());
                    if ui
                        .add_enabled(can_undo, egui::Button::new("Undo (Ctrl+Z)"))
                        .clicked()
                    {
                        ui.close_menu();
                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                            tab.doc.undo();
                        }
                    }
                    if ui
                        .add_enabled(can_redo, egui::Button::new("Redo (Ctrl+Y)"))
                        .clicked()
                    {
                        ui.close_menu();
                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                            tab.doc.redo();
                        }
                    }
                    ui.separator();
                    for (label, fmt, desc) in [
                        ("Copy Hex (Ctrl+C)", CopyFormat::HexSpaced, "spaced hex"),
                        ("Copy Compact Hex", CopyFormat::HexCompact, "compact hex"),
                        (
                            "Copy ASCII (Ctrl+Shift+C)",
                            CopyFormat::Ascii,
                            "ASCII",
                        ),
                        ("Copy as C Array", CopyFormat::CArray, "C array"),
                        ("Copy as Rust Array", CopyFormat::RustArray, "Rust array"),
                        (
                            "Copy as Python Bytes",
                            CopyFormat::PythonBytes,
                            "Python bytes",
                        ),
                    ] {
                        if ui.add_enabled(has_doc, egui::Button::new(label)).clicked() {
                            ui.close_menu();
                            self.copy_selection(ctx, fmt, desc);
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(has_doc, egui::Button::new("Paste Hex (Ctrl+V)"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.paste_clipboard_hex(None);
                    }
                    if ui
                        .add_enabled(
                            has_doc,
                            egui::Button::new("Paste ASCII (Ctrl+Shift+V)"),
                        )
                        .clicked()
                    {
                        ui.close_menu();
                        self.paste_clipboard_ascii(None);
                    }
                    if ui
                        .add_enabled(
                            has_doc,
                            egui::Button::new("Delete Selected Bytes (Del)"),
                        )
                        .clicked()
                    {
                        ui.close_menu();
                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                            tab.doc.delete_selected_bytes();
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(has_doc, egui::Button::new("Jump to Offset... (Ctrl+G)"))
                        .clicked()
                    {
                        ui.close_menu();
                        self.jump_modal_open = true;
                    }
                    if ui
                        .add_enabled(has_doc, egui::Button::new("Find Bytes / Regex... (Ctrl+F)"))
                        .clicked()
                    {
                        ui.close_menu();
                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                            tab.search.open = !tab.search.open;
                            if tab.search.open {
                                tab.search.request_focus = true;
                            }
                        }
                    }
                });

                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_inspector, "Data Inspector Panel");
                    ui.checkbox(&mut self.show_fields, "Overlay Fields Panel");
                    if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                        ui.checkbox(&mut tab.search.open, "Search Dock (Ctrl+F)");
                        if ui
                            .checkbox(&mut tab.entropy.open, "Entropy & Histogram Dock (e)")
                            .changed()
                            && tab.entropy.open
                            && tab.entropy.report.is_none()
                        {
                            tab.start_entropy();
                        }
                        ui.separator();
                        ui.checkbox(&mut tab.doc.settings.show_offsets, "Show Offset Column");
                        ui.checkbox(&mut tab.doc.settings.show_ascii, "Show ASCII Column");
                        ui.checkbox(&mut tab.doc.settings.uppercase_hex, "Uppercase Hex");
                        ui.checkbox(&mut tab.doc.settings.show_overlays, "Show Field Overlays");
                        if ui
                            .checkbox(
                                &mut tab.doc.settings.compress_repeated_rows,
                                "Compress Repeated Rows",
                            )
                            .changed()
                        {
                            tab.doc.rebuild_display_rows();
                        }
                    }
                    ui.separator();
                    if ui
                        .checkbox(
                            &mut self.python.open,
                            "Python 3 Analysis Console (Ctrl+P)",
                        )
                        .changed()
                        && self.python.open
                    {
                        self.ensure_python_session();
                    }
                    if self.tabs.len() >= 2 {
                        ui.separator();
                        ui.checkbox(&mut self.side_by_side, "Side-by-Side Split View (Ctrl+B)");
                        if ui
                            .checkbox(&mut self.diff_mode, "Byte Diff Highlighting (Ctrl+D)")
                            .changed()
                            && self.diff_mode
                        {
                            self.side_by_side = true;
                        }
                        ui.checkbox(
                            &mut self.sync_comparison,
                            "Synchronize Side-by-Side Scroll & Cursor",
                        );
                    }
                });

                ui.menu_button("Help", |ui| {
                    if ui.button("Help & Keybindings... (Ctrl+H)").clicked() {
                        ui.close_menu();
                        self.help_modal_open = true;
                    }
                });

                ui.separator();
                if ui
                    .button("📂 Open")
                    .on_hover_text("Open Binary File (Ctrl+O)")
                    .clicked()
                {
                    self.open_file_dialog();
                }
                let mut save_clicked = false;
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    if ui
                        .add_enabled(tab.doc.is_dirty(), egui::Button::new("💾 Save"))
                        .on_hover_text("Save Binary File (Ctrl+S)")
                        .clicked()
                    {
                        save_clicked = true;
                    }
                    ui.separator();
                    ui.label("Columns:");
                    for bpr in [8usize, 16, 24, 32] {
                        if ui
                            .selectable_label(
                                tab.doc.settings.bytes_per_row == bpr,
                                format!("{bpr}"),
                            )
                            .clicked()
                        {
                            tab.doc.settings.bytes_per_row = bpr;
                            tab.doc.rebuild_display_rows();
                        }
                    }
                    ui.separator();
                    ui.label("Palette:");
                    egui::ComboBox::from_id_salt("byte_color_mode_combo")
                        .selected_text(tab.doc.theme.byte_mode.name())
                        .show_ui(ui, |ui| {
                            for mode in ByteColorMode::ALL {
                                ui.selectable_value(
                                    &mut tab.doc.theme.byte_mode,
                                    mode,
                                    mode.name(),
                                );
                            }
                        });
                    ui.separator();
                    ui.toggle_value(&mut tab.search.open, "🔍 Search")
                        .on_hover_text("Toggle Search Dock (Ctrl+F)");
                    if ui
                        .toggle_value(&mut tab.entropy.open, "📊 Entropy")
                        .on_hover_text("Toggle Entropy & Histogram Dock (e)")
                        .changed()
                        && tab.entropy.open
                        && tab.entropy.report.is_none()
                    {
                        tab.start_entropy();
                    }
                }
                if save_clicked {
                    self.save_active(false);
                }
                if ui
                    .toggle_value(&mut self.python.open, "🐍 Python")
                    .on_hover_text("Toggle Python 3 Analysis Console (Ctrl+P)")
                    .changed()
                    && self.python.open
                {
                    self.ensure_python_session();
                }
                if self.tabs.len() >= 2 {
                    ui.separator();
                    ui.toggle_value(&mut self.side_by_side, "◫ Split")
                        .on_hover_text("Toggle Side-by-Side Split View (Ctrl+B)");
                    if ui
                        .toggle_value(&mut self.diff_mode, "⚡ Diff")
                        .on_hover_text("Highlight byte differences side-by-side (Ctrl+D)")
                        .changed()
                        && self.diff_mode
                    {
                        self.side_by_side = true;
                    }
                    if self.side_by_side {
                        ui.toggle_value(&mut self.sync_comparison, "🔗 Sync")
                            .on_hover_text("Synchronize scroll and cursor between panes");
                        let other_idx = self.effective_compare_tab();
                        let other_name = self.tabs[other_idx].doc.file_name();
                        egui::ComboBox::from_id_salt("compare_tab_combo")
                            .selected_text(format!("vs {other_name}"))
                            .show_ui(ui, |ui| {
                                for (idx, tab) in self.tabs.iter().enumerate() {
                                    if idx != self.active_tab {
                                        ui.selectable_value(
                                            &mut self.compare_tab,
                                            idx,
                                            tab.doc.file_name(),
                                        );
                                    }
                                }
                            });
                        if ui
                            .small_button("◀ Diff")
                            .on_hover_text("Jump to previous differing byte range")
                            .clicked()
                        {
                            self.jump_to_diff(false);
                        }
                        if ui
                            .small_button("Diff ▶")
                            .on_hover_text("Jump to next differing byte range")
                            .clicked()
                        {
                            self.jump_to_diff(true);
                        }
                    }
                }
            });

            // ---- Document Tab Bar ----
            if !self.tabs.is_empty() {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    let mut close_idx = None;
                    for (idx, tab) in self.tabs.iter().enumerate() {
                        let active = idx == self.active_tab;
                        let dirty = if tab.doc.is_dirty() { " *" } else { "" };
                        let label = format!("{}{}", tab.doc.file_name(), dirty);
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(active, RichText::new(label).strong())
                                .clicked()
                            {
                                self.active_tab = idx;
                            }
                            if ui.small_button("×").on_hover_text("Close tab").clicked() {
                                close_idx = Some(idx);
                            }
                        });
                        ui.separator();
                    }
                    if let Some(idx) = close_idx {
                        let _ = self.tabs[idx].doc.persist_automatic_overlay();
                        self.tabs.remove(idx);
                        if self.active_tab >= self.tabs.len() && !self.tabs.is_empty() {
                            self.active_tab = self.tabs.len() - 1;
                        }
                        if self.tabs.len() < 2 {
                            self.side_by_side = false;
                            self.diff_mode = false;
                        }
                    }
                });
            }
        });

        // ---- Bottom Status Bar ----
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tab) = self.tabs.get(self.active_tab) {
                    ui.monospace(format!(
                        "{} | {} bytes (0x{:X})",
                        tab.doc.path.display(),
                        tab.doc.buffer.len(),
                        tab.doc.buffer.len()
                    ));
                    if let Some(overlay) = tab.doc.open_overlay_name() {
                        ui.separator();
                        ui.colored_label(Color32::from_rgb(100, 210, 230), format!("Overlay: {overlay}"));
                    }
                    ui.separator();
                    ui.label(&tab.doc.status);
                } else {
                    ui.label(&self.status_banner);
                }
            });
        });

        // ---- Optional Bottom Docks: Python (bottom-most), Entropy, Search (top-most above Python) ----
        if self.python.open {
            if self.python.session.is_none()
                && self.python.starting_rx.is_none()
                && !self.tabs.is_empty()
            {
                self.ensure_python_session();
            }
            egui::TopBottomPanel::bottom("python_dock")
                .resizable(true)
                .default_height(215.0)
                .show(ctx, |ui| {
                    let mut sync_clicked = false;
                    let mut clear_clicked = false;
                    ui.horizontal(|ui| {
                        ui.strong("🐍 Python 3 Console");
                        if self.python.starting_rx.is_some() {
                            ui.spinner();
                            ui.weak("Starting Python session...");
                        } else if self.python.pending > 0 {
                            ui.spinner();
                            ui.weak("Running...");
                        }
                        if ui
                            .button("Apply Buffers (:apply)")
                            .on_hover_text("Write modified Python buffer/buffer_N contents back to the Piece Table")
                            .clicked()
                            && let Some(session) = &self.python.session
                        {
                            let _ = session.apply();
                            self.python.pending += 1;
                        }
                        if ui
                            .button("🔄 Sync Buffers")
                            .on_hover_text("Reload open binary buffers into the Python session")
                            .clicked()
                        {
                            sync_clicked = true;
                        }
                        if ui
                            .button("Interrupt")
                            .on_hover_text("Send KeyboardInterrupt to a running Python command")
                            .clicked()
                            && let Some(session) = &self.python.session
                        {
                            let _ = session.interrupt();
                        }
                        if ui
                            .button("Clear")
                            .on_hover_text("Clear console output (Ctrl+L)")
                            .clicked()
                        {
                            clear_clicked = true;
                        }
                    });
                    if sync_clicked {
                        self.restart_python_session();
                    }
                    if clear_clicked {
                        self.python.output_lines.clear();
                    }
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(125.0)
                        .stick_to_bottom(true)
                        .animated(false)
                        .drag_to_scroll(false)
                        .id_salt("python_out")
                        .show(ui, |ui| {
                            for line in &self.python.output_lines {
                                let color = if line.starts_with(">>>") || line.starts_with("...") {
                                    Color32::from_rgb(110, 215, 255)
                                } else if line.starts_with("Error:") || line.contains("Traceback") {
                                    Color32::LIGHT_RED
                                } else if line.starts_with("Applied Python") {
                                    Color32::from_rgb(120, 230, 140)
                                } else {
                                    Color32::from_rgb(225, 230, 238)
                                };
                                ui.colored_label(color, RichText::new(line).monospace());
                            }
                            for (i, pending_line) in self.python.repl_lines.iter().enumerate() {
                                let pfx = if i == 0 { ">>>" } else { "..." };
                                ui.colored_label(
                                    Color32::from_rgb(180, 200, 120),
                                    RichText::new(format!("{pfx} {pending_line}")).monospace(),
                                );
                            }
                        });
                    ui.horizontal(|ui| {
                        let prompt = if self.python.repl_lines.is_empty() {
                            ">>>"
                        } else {
                            "..."
                        };
                        ui.monospace(prompt);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.python.input)
                                .desired_width(ui.available_width() - 120.0)
                                .hint_text("Python expression/statement (Up/Down for history)")
                                .font(egui::TextStyle::Monospace),
                        );

                        if resp.has_focus() {
                            ui.input(|i| {
                                if (i.modifiers.ctrl || i.modifiers.command)
                                    && i.key_pressed(Key::L)
                                {
                                    self.python.output_lines.clear();
                                }
                                if i.key_pressed(Key::Escape) && !self.python.repl_lines.is_empty()
                                {
                                    self.python.repl_lines.clear();
                                    self.python.input.clear();
                                }
                                if i.key_pressed(Key::ArrowUp) && !self.python.history.is_empty() {
                                    let next_idx = match self.python.history_index {
                                        None => {
                                            self.python.history_draft = self.python.input.clone();
                                            self.python.history.len() - 1
                                        }
                                        Some(idx) => idx.saturating_sub(1),
                                    };
                                    self.python.history_index = Some(next_idx);
                                    self.python.input = self.python.history[next_idx].clone();
                                } else if i.key_pressed(Key::ArrowDown)
                                    && let Some(idx) = self.python.history_index
                                {
                                    if idx + 1 < self.python.history.len() {
                                        self.python.history_index = Some(idx + 1);
                                        self.python.input = self.python.history[idx + 1].clone();
                                    } else {
                                        self.python.history_index = None;
                                        self.python.input =
                                            std::mem::take(&mut self.python.history_draft);
                                    }
                                }
                            });
                        }

                        let enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        let run_clicked = ui.button("Run").clicked();
                        if !self.python.repl_lines.is_empty()
                            && ui
                                .small_button("Cancel Block")
                                .on_hover_text("Discard unfinished multi-line block (Esc)")
                                .clicked()
                        {
                            self.python.repl_lines.clear();
                            self.python.input.clear();
                        }
                        if run_clicked || enter {
                            let line = std::mem::take(&mut self.python.input);
                            let mut ready_cmd: Option<String> = None;

                            if !self.python.repl_lines.is_empty() {
                                if line.trim().is_empty() || run_clicked {
                                    if !line.trim().is_empty() {
                                        self.python.repl_lines.push(line);
                                    }
                                    ready_cmd =
                                        Some(std::mem::take(&mut self.python.repl_lines).join("\n"));
                                } else {
                                    let indent = python_continuation_indentation(&line);
                                    self.python.repl_lines.push(line);
                                    self.python.input = indent;
                                }
                            } else if line.trim_end().ends_with(':') && !run_clicked {
                                let indent = python_continuation_indentation(&line);
                                self.python.repl_lines.push(line);
                                self.python.input = indent;
                            } else if !line.trim().is_empty() {
                                ready_cmd = Some(line);
                            }

                            if let Some(cmd) = ready_cmd {
                                for (idx, l) in cmd.lines().enumerate() {
                                    let pfx = if idx == 0 { ">>>" } else { "..." };
                                    self.python.output_lines.push(format!("{pfx} {l}"));
                                }
                                if self.python.history.last() != Some(&cmd) {
                                    self.python.history.push(cmd.clone());
                                }
                                self.python.history_index = None;
                                self.python.history_draft.clear();

                                if self.python.session.is_some() {
                                    self.sync_python_selections();
                                }
                                if let Some(session) = &self.python.session {
                                    if cmd.trim() == ":apply" {
                                        let _ = session.apply();
                                    } else {
                                        let _ = session.execute(cmd);
                                    }
                                    self.python.pending += 1;
                                } else if self.python.starting_rx.is_some() {
                                    self.python.queued_commands.push(cmd);
                                } else {
                                    self.python
                                        .output_lines
                                        .push("Open a binary file first to start Python.".into());
                                }
                            }
                            resp.request_focus();
                        }
                    });
                });
        }

        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            if tab.entropy.open {
                egui::TopBottomPanel::bottom("entropy_dock")
                    .resizable(true)
                    .default_height(210.0)
                    .show(ctx, |ui| {
                        let report = tab.entropy.report.clone();
                        let scanned = tab.entropy.scanned;
                        let total = tab.entropy.total;
                        render_entropy_panel(ui, &mut tab.doc, report.as_ref(), scanned, total);
                    });
            }

            if tab.search.open {
                egui::TopBottomPanel::bottom("search_dock")
                    .resizable(true)
                    .default_height(175.0)
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong("Search:");
                            let hint = "Hex (DE AD / DE ?? BE), text:hello, utf16:str, dec:256, bin:10101010, or re:[A-Z]+";
                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                            let hint_px = ui.fonts(|f| {
                                f.layout_no_wrap(hint.to_owned(), font_id, Color32::WHITE)
                                    .size()
                                    .x
                            }) + 24.0;
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut tab.search.query)
                                    .hint_text(hint)
                                    .desired_width(hint_px),
                            );
                            if tab.search.request_focus {
                                resp.request_focus();
                                tab.search.request_focus = false;
                            }
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            if ui.button("Find All").clicked() || enter {
                                tab.start_search();
                            }
                            if tab.search.running {
                                ui.spinner();
                                let frac = if tab.search.total == 0 {
                                    0.0
                                } else {
                                    tab.search.scanned as f32 / tab.search.total as f32
                                };
                                ui.add(egui::ProgressBar::new(frac).desired_width(100.0));
                            }
                            if !tab.search.matches.is_empty() {
                                ui.label(format!("{} matches", tab.search.matches.len()));
                                if ui.button("◀ Prev").clicked() {
                                    let prev = tab
                                        .search
                                        .current_match
                                        .checked_sub(1)
                                        .unwrap_or(tab.search.matches.len() - 1);
                                    tab.jump_to_match(prev);
                                }
                                if ui.button("Next ▶").clicked() {
                                    let next =
                                        (tab.search.current_match + 1) % tab.search.matches.len();
                                    tab.jump_to_match(next);
                                }
                            }
                            if let Some(err) = &tab.search.error {
                                ui.colored_label(Color32::LIGHT_RED, err);
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Close search (Ctrl+F)")
                                    .clicked()
                                {
                                    tab.search.open = false;
                                }
                            });
                        });
                        ui.separator();
                        let row_h = 18.0;
                        let total_matches = tab.search.matches.len();
                        let mut clicked_match = None;
                        egui::ScrollArea::vertical()
                            .id_salt("search_matches_scroll")
                            .animated(false)
                            .drag_to_scroll(false)
                            .show_rows(ui, row_h, total_matches, |ui, range| {
                                for idx in range {
                                    let m = tab.search.matches[idx];
                                    let preview = tab.doc.buffer.read_range(m.start, m.len().min(16));
                                    let hex_str = preview
                                        .iter()
                                        .map(|b| hex_byte_str(*b, true))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    let ascii_str: String = preview
                                        .iter()
                                        .map(|&b| {
                                            if (0x20..=0x7E).contains(&b) {
                                                char::from(b)
                                            } else {
                                                '.'
                                            }
                                        })
                                        .collect();
                                    let selected = idx == tab.search.current_match;
                                    if ui
                                        .selectable_label(
                                            selected,
                                            RichText::new(format!(
                                                "#{:<5}  0x{:08X}..0x{:08X} ({:<4} B)  {:<47}  |{}|",
                                                idx + 1,
                                                m.start,
                                                m.end,
                                                m.len(),
                                                hex_str,
                                                ascii_str
                                            ))
                                            .monospace(),
                                        )
                                        .clicked()
                                    {
                                        clicked_match = Some(idx);
                                    }
                                }
                            });
                        if let Some(idx) = clicked_match {
                            tab.jump_to_match(idx);
                        }
                    });
            }
        }

        // ---- Right Side Panel: Fields & Data Inspector ----
        if !self.tabs.is_empty() && (self.show_inspector || self.show_fields) {
            let max_inspector_w = (ctx.screen_rect().width() * 0.38).clamp(280.0, 440.0);
            egui::SidePanel::right("right_inspector_sidebar")
                .resizable(true)
                .default_width(340.0)
                .min_width(260.0)
                .max_width(max_inspector_w)
                .show(ctx, |ui| {
                    ui.set_max_width(ui.available_width());
                    let Some(tab) = self.tabs.get_mut(self.active_tab) else {
                        return;
                    };
                    if self.show_fields {
                        ui.heading("Overlay Fields");
                        render_fields_panel(ui, &mut tab.doc, &mut tab.field_editor);
                        ui.separator();
                    }
                    if self.show_inspector {
                        ui.heading("Data Inspector");
                        let mut trigger_plugins = false;
                        let mut reload_plugins = false;
                        let plugins_running = tab.plugin_worker.is_some();
                        render_inspector_panel(
                            ui,
                            &mut tab.doc,
                            &mut tab.inspector_state,
                            &self.discovered_plugins,
                            &tab.plugin_results,
                            plugins_running,
                            &mut trigger_plugins,
                            &mut reload_plugins,
                        );
                        if reload_plugins {
                            self.discovered_plugins =
                                plugins::discover(&plugins::default_directory());
                            tab.doc.status = format!(
                                "Reloaded {} inspector plugin(s) from {}",
                                self.discovered_plugins.len(),
                                plugins::default_directory().display()
                            );
                        }
                        if trigger_plugins {
                            let plugins = self.discovered_plugins.clone();
                            tab.run_plugins(&plugins);
                        }
                    }
                });
        }

        // ---- Central Hex Viewport (or Side-by-Side Comparison) ----
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tabs.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(120.0);
                    ui.heading("RexEditor — Binary Analysis Workstation");
                    ui.add_space(8.0);
                    ui.label("Drop a binary file anywhere in this window or click below to begin.");
                    ui.add_space(16.0);
                    if ui.button("📂 Open Binary File (Ctrl+O)").clicked() {
                        self.open_file_dialog();
                    }
                });
                return;
            }

            let mut request_add_field_active = false;
            if self.side_by_side && self.tabs.len() >= 2 {
                let active_idx = self.active_tab;
                let other_idx = self.effective_compare_tab();
                let active_buf = self.diff_mode.then(|| self.tabs[active_idx].doc.buffer.clone());
                let other_buf = self.diff_mode.then(|| self.tabs[other_idx].doc.buffer.clone());

                let prev_a_scroll = self.tabs[active_idx].doc.scroll;
                let prev_a_sel = self.tabs[active_idx].doc.selection;
                let prev_b_scroll = self.tabs[other_idx].doc.scroll;
                let prev_b_sel = self.tabs[other_idx].doc.selection;

                let total_w = ui.available_width();
                let total_h = ui.available_height();
                let sep_space = 16.0;
                let pane_w = ((total_w - sep_space) * 0.5).max(120.0);

                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::Vec2::new(pane_w, total_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_max_width(pane_w);
                            let clip = ui.max_rect().intersect(ui.clip_rect());
                            ui.set_clip_rect(clip);
                            let tab = &mut self.tabs[active_idx];
                            let matches = tab.search.matches.clone();
                            render_hex_viewport(
                                ui,
                                "hex_viewport_primary",
                                &mut tab.doc,
                                &matches,
                                other_buf.as_ref(),
                                &mut tab.active_column,
                                &mut request_add_field_active,
                            );
                        },
                    );
                    ui.separator();
                    ui.allocate_ui_with_layout(
                        egui::Vec2::new(pane_w, total_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_max_width(pane_w);
                            let clip = ui.max_rect().intersect(ui.clip_rect());
                            ui.set_clip_rect(clip);
                            let mut dummy_add = false;
                            let tab = &mut self.tabs[other_idx];
                            let matches = tab.search.matches.clone();
                            render_hex_viewport(
                                ui,
                                "hex_viewport_secondary",
                                &mut tab.doc,
                                &matches,
                                active_buf.as_ref(),
                                &mut tab.active_column,
                                &mut dummy_add,
                            );
                        },
                    );
                });

                if self.sync_comparison {
                    let new_a_scroll = self.tabs[active_idx].doc.scroll;
                    let new_a_sel = self.tabs[active_idx].doc.selection;
                    let new_b_scroll = self.tabs[other_idx].doc.scroll;
                    let new_b_sel = self.tabs[other_idx].doc.selection;

                    if new_a_scroll != prev_a_scroll {
                        let doc_b = &mut self.tabs[other_idx].doc;
                        let target = new_a_scroll.min(doc_b.max_scroll());
                        if doc_b.scroll != target {
                            doc_b.scroll = target;
                            doc_b.scroll_requested = true;
                        }
                    } else if new_b_scroll != prev_b_scroll {
                        let doc_a = &mut self.tabs[active_idx].doc;
                        let target = new_b_scroll.min(doc_a.max_scroll());
                        if doc_a.scroll != target {
                            doc_a.scroll = target;
                            doc_a.scroll_requested = true;
                        }
                    }

                    if new_a_sel != prev_a_sel
                        && let Some(sel) = new_a_sel
                        && !self.tabs[other_idx].doc.buffer.is_empty()
                    {
                        let doc_b = &mut self.tabs[other_idx].doc;
                        let max_off = doc_b.buffer.len() - 1;
                        doc_b.selection = Some(Selection::from_range(
                            sel.start().min(max_off),
                            sel.end().min(max_off),
                        ));
                    } else if new_b_sel != prev_b_sel
                        && let Some(sel) = new_b_sel
                        && !self.tabs[active_idx].doc.buffer.is_empty()
                    {
                        let doc_a = &mut self.tabs[active_idx].doc;
                        let max_off = doc_a.buffer.len() - 1;
                        doc_a.selection = Some(Selection::from_range(
                            sel.start().min(max_off),
                            sel.end().min(max_off),
                        ));
                    }
                }
            } else if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                let matches = tab.search.matches.clone();
                render_hex_viewport(
                    ui,
                    "hex_viewport_primary",
                    &mut tab.doc,
                    &matches,
                    None,
                    &mut tab.active_column,
                    &mut request_add_field_active,
                );
            }
            if request_add_field_active
                && let Some(tab) = self.tabs.get_mut(self.active_tab)
            {
                self.show_fields = true;
                tab.field_editor.begin_add_for_selection(&tab.doc);
            }
        });

        // ---- Jump to Offset Modal (Ctrl+G) ----
        if self.jump_modal_open {
            egui::Window::new("Jump to Offset")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Enter decimal or 0x-prefixed hex offset:");
                    let resp = ui.text_edit_singleline(&mut self.jump_input);
                    resp.request_focus();
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("Jump").clicked() || enter {
                            if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                match Document::parse_offset(&self.jump_input) {
                                    Ok(off) => {
                                        if let Some(warning) =
                                            apply_offset_with_bounds_notice(&mut tab.doc, off)
                                        {
                                            self.notice_modal = Some(warning);
                                        }
                                        self.jump_modal_open = false;
                                    }
                                    Err(err) => tab.doc.status = err,
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            self.jump_modal_open = false;
                        }
                    });
                });
        }

        // ---- Notice / Warning Modal (e.g. CLI --offset out of bounds) ----
        if let Some(message) = self.notice_modal.clone() {
            let mut dismiss = false;
            egui::Window::new("Notice")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.colored_label(Color32::from_rgb(255, 200, 90), &message);
                    ui.add_space(8.0);
                    if ui.button("OK").clicked()
                        || ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Escape))
                    {
                        dismiss = true;
                    }
                });
            if dismiss {
                self.notice_modal = None;
            }
        }

        // ---- Help & Keybindings Modal (Ctrl+H) ----
        if self.help_modal_open {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.help_modal_open = false;
            } else {
                let mut open = self.help_modal_open;
                egui::Window::new("Help & Keybindings (Ctrl+H)")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_size([720.0, 560.0])
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                render_help_contents(ui);
                            });
                    });
                self.help_modal_open = open;
            }
        }
    }
}

fn render_help_contents(ui: &mut egui::Ui) {
    let accent = Color32::from_rgb(88, 166, 255);
    let key_color = Color32::from_rgb(126, 231, 135);
    let muted = Color32::from_rgb(160, 172, 188);

    ui.label(
        RichText::new("RexEditor — Binary Analysis Workstation Guide")
            .size(16.0)
            .strong()
            .color(accent),
    );
    ui.colored_label(
        muted,
        "Overview of major workstation features and keyboard shortcuts. Press Ctrl+H or Esc at any time to close this window.",
    );
    ui.add_space(8.0);

    let render_section = |ui: &mut egui::Ui,
                          title: &str,
                          description: &str,
                          bindings: &[(&str, &str)]| {
        ui.separator();
        ui.add_space(4.0);
        ui.label(RichText::new(title).size(14.0).strong().color(accent));
        ui.colored_label(muted, description);
        ui.add_space(4.0);
        egui::Grid::new(format!("help_grid_{title}"))
            .num_columns(2)
            .spacing([18.0, 5.0])
            .striped(true)
            .show(ui, |ui| {
                for &(key, action) in bindings {
                    ui.monospace(RichText::new(key).strong().color(key_color));
                    ui.label(action);
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
    };

    render_section(
        ui,
        "1. File & Overlay Management",
        "Open and save binary files of any size (memory-mapped for instant loading) and manage persistent JSON field overlays that annotate structures without modifying the underlying binary.",
        &[
            ("Ctrl + O", "Open a binary file into a new workstation tab"),
            ("Ctrl + S", "Save changes to the active binary file"),
            (
                "Ctrl + Shift + S",
                "Save the active binary under a new filename (automatically forks its overlay to a new overlay file)",
            ),
            (
                "Ctrl + Shift + O",
                "Load an external JSON field overlay into the active document",
            ),
            (
                "Ctrl + Alt + S",
                "Save / export the active document's field overlay JSON to a custom path",
            ),
            ("Ctrl + H", "Toggle this Help & Keybindings popup"),
        ],
    );

    render_section(
        ui,
        "2. Hex & ASCII Viewport Navigation",
        "Navigate across offsets, select byte ranges for inspection or copying, and jump directly to decimal or hexadecimal addresses.",
        &[
            ("Arrow Keys", "Move cursor by 1 byte (Left/Right) or 1 row (Up/Down)"),
            ("Shift + Arrows", "Extend byte selection range"),
            ("PageUp / PageDown", "Move cursor up or down by one visible page"),
            ("Home / End", "Jump to start (0x0) or end of the file"),
            ("Ctrl + G", "Open Jump to Offset modal (accepts hex like 0x1A40 or decimal)"),
            ("Tab", "Switch active editing focus between Hex column and ASCII column"),
        ],
    );

    render_section(
        ui,
        "3. Binary Editing & Clipboard (View / Overwrite / Insert)",
        "Safely inspect bytes in read-only View Mode, overwrite bytes in-place in Overwrite Mode, or splice/insert new bytes in Insert Mode with full multi-step Undo/Redo history.",
        &[
            (
                "i",
                "Cycle editing mode: View Mode -> Edit (Overwrite) -> Edit (Insert) -> View Mode",
            ),
            ("Insert", "Toggle between Overwrite and Insert edit modes"),
            ("Esc", "Return immediately to read-only View Mode"),
            ("0-9, A-F", "Type hex nibbles (when in Edit Mode & Hex column)"),
            ("Printable ASCII", "Type ASCII characters (when in Edit Mode & ASCII column)"),
            ("Delete / Backspace", "Delete selected bytes (in Edit Mode)"),
            ("Ctrl + Z / Ctrl + U", "Undo last byte edit"),
            ("Ctrl + Y / Ctrl + R", "Redo last undone byte edit"),
            ("Ctrl + C", "Copy selected bytes as spaced hexadecimal to clipboard"),
            ("Ctrl + Shift + C", "Copy selected bytes as ASCII text to clipboard"),
            ("Ctrl + V", "Paste hexadecimal bytes from clipboard at cursor"),
            ("Ctrl + Shift + V", "Paste ASCII text from clipboard as raw bytes at cursor"),
        ],
    );

    render_section(
        ui,
        "4. Multi-Pattern Search Dock",
        "Background SIMD-accelerated search engine supporting exact hex, nibble wildcards (DE ?? 4?), ASCII/UTF-8, UTF-16LE, integers (u16/u32/u64 LE/BE), binary bitstrings, and regular expressions.",
        &[
            ("Ctrl + F", "Toggle the Search Dock open or closed (and focus query input)"),
            ("Enter", "Run search across the active binary (while Search input is focused)"),
            ("F3 / n", "Jump to next search match (n works in View Mode)"),
            ("Shift + F3 / N", "Jump to previous search match (N works in View Mode)"),
        ],
    );

    render_section(
        ui,
        "5. Side-by-Side Split & Byte Diff Comparison",
        "Compare two open binaries side-by-side with optional synchronized scrolling/selection and instant byte-level difference highlighting.",
        &[
            (
                "Ctrl + B",
                "Toggle Side-by-Side Split View (requires at least 2 open tabs)",
            ),
            (
                "Ctrl + D",
                "Toggle Byte Diff Highlighting in Side-by-Side Split View (requires 2+ tabs)",
            ),
            (
                "◀ Diff / Diff ▶",
                "Toolbar buttons to jump to the previous or next differing byte range",
            ),
        ],
    );

    render_section(
        ui,
        "6. Python 3 Analysis Console, Entropy & Structure Overlays",
        "Interactive persistent Python 3 bridge with live access to open binary buffers (`buffer`, `selected`, `write()`, `add_field()`), Shannon entropy graphing, and color-coded structure annotations.",
        &[
            ("Ctrl + P", "Toggle the interactive Python 3 Analysis Console"),
            ("e", "Toggle the Entropy & Byte Histogram Dock (in View Mode)"),
            ("a", "Create a new named Overlay Field from the current byte selection (in View Mode)"),
            ("[ / ]", "Jump to previous / next defined Overlay Field (in View Mode)"),
            ("r", "Run discovered external analysis plugins on the active binary (in View Mode)"),
        ],
    );
}
